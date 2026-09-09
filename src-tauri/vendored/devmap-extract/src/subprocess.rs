//! One bounded subprocess, for every place the kernel shells out.
//!
//! The kernel asks `git` three questions — `HEAD` for the store's freshness
//! sentinel, `HEAD` and `ls-files` for the artifact digests, `log` for churn —
//! and until this module existed each asker ran its own child process its own
//! way. `devmap-store` drained both pipes on threads and killed at five
//! seconds; `devmap-query`'s digests called `Command::output()` with no bound
//! at all, on the path every artifact write takes — the hang the store's
//! deadline exists to prevent, reintroduced one crate up; and the churn reader
//! had its own drain thread and its own cap, closing the pipe at the cap and
//! letting `SIGPIPE` end git. Correct, and a third copy of the discipline.
//!
//! Three runners is how three disciplines drift. This is the one, and the
//! properties every caller gets by going through it:
//!
//! * **A wall-clock deadline.** The child is killed and reaped when it expires,
//!   whether it is still writing, has closed its pipes and hung, or is waiting
//!   on a terminal that is not there.
//! * **An output cap that keeps the child honest.** Bytes past the cap are
//!   read and discarded rather than the pipe being closed under the child, so
//!   it finishes on its own terms and its exit status still means what it
//!   says; the caller learns that the answer is incomplete — never a truncated
//!   answer presented as whole. A pipe that stops answering before EOF is
//!   reported the same way, because the caller is holding the same thing: a
//!   prefix.
//! * **Both pipes drained concurrently**, because reading them after exit
//!   deadlocks the moment either buffer fills.
//! * **No terminal, no stdin.** `stdin` is `/dev/null`; the `git` constructor
//!   below also refuses interactive prompts and optional locks.
//!
//! The deadline reaches the child's *descendants*, not only the child. On unix
//! the child leads its own process group and the expiry signals the group, so a
//! hook, a credential helper, a pager or an `sh -c` fan-out dies with the
//! process that started it. Killing one pid instead used to leave two problems
//! behind, and they are the same problem: the descendant ran on past a deadline
//! the caller had been told was enforced, and — still holding the inherited
//! write ends — it left both drain threads blocked in `read` on a process the
//! runner never started. The runner returned `Deadline` without them: two
//! threads and two descriptors per expiry, freed only when the descendant chose
//! to exit, in a daemon that runs `git` on a schedule. After the group kill the
//! threads are given a bounded chance to see EOF and come home, so the common
//! case hands nothing back. A descendant that called `setsid` has left the group
//! and can still hold a pipe past that window; that is the residual, and the
//! runner still returns on time when it happens.
//!
//! **The trade-off, stated plainly.** A child in its own process group no longer
//! receives the terminal's `SIGINT`: interrupting a foreground `devmap` leaves an
//! in-flight child to finish on its own, or — in the hung case — to linger with
//! nobody left to enforce the deadline, because the enforcer was the process the
//! user just interrupted. It is bought deliberately. Every caller today is a
//! `git` read with no stdin and `GIT_TERMINAL_PROMPT=0`, which finishes in
//! milliseconds or is the pathological case this bound exists for; `devmap serve`
//! handles `SIGTERM`/`SIGINT` itself and lets in-flight runs complete. And an
//! interrupted parent takes the read ends of both pipes with it, so a child that
//! is still *writing* dies of `SIGPIPE` on its next write — leaving only the
//! child that has gone quiet, which is the one the deadline could not have
//! helped either. The alternative — a signal handler — is process-global state,
//! and a library that installs one takes that decision away from every binary
//! that links it.
//!
//! What it deliberately does not decide is what a failure *means*: the store
//! treats an unavailable `HEAD` as "unavailable", the digests fall back to the
//! two fingerprints, churn reports `computed: false` with the reason. Each
//! caller keeps its contract and gains the bounds.

use std::ffi::OsStr;
use std::fmt;
use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Wall clock allowed for `git rev-parse HEAD`, wherever the kernel asks it.
///
/// `git` can stall on pathological repositories, network mounts or hook
/// misconfigurations; unbounded, it hung every drain batch and CLI status
/// behind it. On expiry the child is killed and the caller gets an error —
/// every asker already treats an unavailable head as "unavailable", so a
/// stalled git degrades honestly instead of wedging the daemon. One constant
/// for the store's sentinel and the artifacts' digest, so the two cannot
/// disagree about how long a head is worth waiting for.
pub const GIT_HEAD_DEADLINE: Duration = Duration::from_secs(5);

/// How long the runner waits, after killing the child's process group, for the
/// two drain threads to see EOF and finish.
///
/// Bounded on purpose and spent only on an expiry — the path that has already
/// cost a full deadline — so it buys the common case back its threads and its
/// descriptors without letting a descendant that escaped the group (`setsid`)
/// hold the runner past its promise. Exceeding it is the old behaviour, not a
/// new failure: the threads are handed back to nobody, exactly as before.
const DRAIN_HANDBACK: Duration = Duration::from_millis(500);

/// How much of a child the caller is prepared to wait for and to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    /// Wall clock from spawn to exit. On expiry the child is killed.
    pub deadline: Duration,
    /// Bytes of stdout kept. Anything past it is drained and dropped, and
    /// [`Captured::stdout_truncated`] says so.
    pub stdout_cap: usize,
    /// Bytes of stderr kept, the same way.
    pub stderr_cap: usize,
}

/// What a child that exited within its bounds left behind.
#[derive(Debug)]
pub struct Captured {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    /// The bytes here are a *prefix* of what the child had to say: it wrote
    /// more than the cap, or the pipe stopped answering before EOF. One flag
    /// for both, because every caller asks the same question of it — "are these
    /// bytes the whole answer" — and there is no reading of that question where
    /// a pipe that failed mid-stream counts as yes.
    pub stdout_truncated: bool,
    pub stderr: Vec<u8>,
    pub stderr_truncated: bool,
    /// Spawn to reap.
    pub elapsed: Duration,
}

impl Captured {
    /// stdout as text, lossily. Truncation is still the caller's to check.
    pub fn stdout_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// stderr as trimmed text, for error messages.
    pub fn stderr_trimmed(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_string()
    }
}

/// Why no [`Captured`] came back. A non-zero exit is *not* a failure here —
/// the child ran and answered — so it arrives as a [`Captured`] with its
/// status, and each caller decides what a refusal means for it.
#[derive(Debug)]
pub enum Failure {
    /// The program could not be started at all.
    Spawn {
        program: String,
        error: std::io::Error,
    },
    /// The child was still running at the deadline and was killed.
    Deadline { program: String, deadline: Duration },
    /// The child could not be waited on.
    Wait {
        program: String,
        error: std::io::Error,
    },
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Spawn { program, error } => write!(f, "cannot spawn {program}: {error}"),
            Failure::Deadline { program, deadline } => {
                write!(f, "{program} exceeded {deadline:?} and was killed")
            }
            Failure::Wait { program, error } => write!(f, "cannot wait for {program}: {error}"),
        }
    }
}

impl std::error::Error for Failure {}

/// A `git` invocation against `root`, with the hygiene every kernel call
/// wants: `-C root` rather than `current_dir` so a missing root is git's error
/// and not a spawn failure, no terminal prompt, and no optional index locks
/// (a read must never leave `index.lock` behind in someone's checkout).
///
/// Subcommand and flags are the caller's to add.
pub fn git(root: &Path) -> Command {
    git_with_program(OsStr::new("git"), root)
}

/// [`git`] with the program named, so a test can stand a script in for it.
pub fn git_with_program(program: &OsStr, root: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .arg("-C")
        .arg(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0");
    command
}

/// Linux can briefly retain a writer inherited by another concurrent fork
/// until that process execs. Retry only this pre-exec refusal: no child has
/// started, and a successful spawn must never be repeated. Persistent writers
/// stop after eight attempts; the original request deadline also bounds retries.
fn spawn_with_retry(
    mut spawn: impl FnMut() -> std::io::Result<std::process::Child>,
    program: &str,
    started: Instant,
    deadline: Duration,
) -> Result<std::process::Child, Failure> {
    const MAX_ATTEMPTS: usize = 8;
    const RETRY_DELAY: Duration = Duration::from_millis(10);
    for attempt in 1..=MAX_ATTEMPTS {
        if started.elapsed() >= deadline {
            return Err(Failure::Deadline {
                program: program.to_owned(),
                deadline,
            });
        }
        match spawn() {
            Ok(child) => return Ok(child),
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && attempt < MAX_ATTEMPTS =>
            {
                thread::sleep(RETRY_DELAY.min(deadline.saturating_sub(started.elapsed())));
            }
            Err(error) => {
                return Err(Failure::Spawn {
                    program: program.to_owned(),
                    error,
                })
            }
        }
    }
    unreachable!("the final attempt always returns")
}

/// Run `command` to completion within `bounds`.
///
/// `stdin`, `stdout` and `stderr` are set here; anything the caller configured
/// for them is replaced, because a child holding a terminal or an undrained
/// pipe is exactly what the bounds exist to rule out.
pub fn run_bounded(command: &mut Command, bounds: Bounds) -> Result<Captured, Failure> {
    let program = command.get_program().to_string_lossy().into_owned();
    let started = Instant::now();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The child leads a process group of its own, so the expiry below can name
    // the whole tree it started rather than the one pid `Child` knows about.
    // `process_group` is `std`'s own `setpgid` in the child, between fork and
    // exec — no `pre_exec`, nothing unsafe here.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = spawn_with_retry(|| command.spawn(), &program, started, bounds.deadline)?;

    // Both pipes are taken before anything waits: a reader that starts after
    // the child has filled its buffer starts too late.
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let (stdout_done, stdout_rx) = mpsc::channel();
    let (stderr_done, stderr_rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = stdout_done.send(drain(stdout_pipe, bounds.stdout_cap));
    });
    thread::spawn(move || {
        let _ = stderr_done.send(drain(stderr_pipe, bounds.stderr_cap));
    });

    let deadline = started + bounds.deadline;
    // EOF on both pipes is the cheap signal that the child is finishing; the
    // reap below is what confirms it. Waiting on the readers first means the
    // common case costs no polling at all.
    let stdout = match stdout_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(drained) => drained,
        Err(_) => {
            kill_and_reap(&mut child, &stdout_rx, &stderr_rx);
            return Err(Failure::Deadline {
                program,
                deadline: bounds.deadline,
            });
        }
    };
    let stderr = match stderr_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(drained) => drained,
        Err(_) => {
            kill_and_reap(&mut child, &stdout_rx, &stderr_rx);
            return Err(Failure::Deadline {
                program,
                deadline: bounds.deadline,
            });
        }
    };

    // A child can close its pipes and then hang — a hook that daemonised, a
    // process waiting on something that is not coming. The reap is bounded by
    // the same deadline for that reason.
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                kill_and_reap(&mut child, &stdout_rx, &stderr_rx);
                return Err(Failure::Deadline {
                    program,
                    deadline: bounds.deadline,
                });
            }
            Ok(None) => thread::sleep(Duration::from_millis(1)),
            Err(error) => {
                // The same clean-up: a child we can no longer wait on is a child
                // still running, and its descendants are still holding the pipes.
                kill_and_reap(&mut child, &stdout_rx, &stderr_rx);
                return Err(Failure::Wait { program, error });
            }
        }
    };

    Ok(Captured {
        status,
        stdout: stdout.bytes,
        stdout_truncated: stdout.truncated,
        stderr: stderr.bytes,
        stderr_truncated: stderr.truncated,
        elapsed: started.elapsed(),
    })
}

/// End the child and everything it started, then let the drain threads come
/// home.
///
/// The order is the whole point. The kill goes out **before** the reap, because
/// an unreaped child keeps its process group alive even when it has already
/// exited — which is exactly the daemonising-hook shape, the parent gone and
/// the descendants still holding the pipes. Reaping first would dissolve the
/// group and leave nothing to signal.
///
/// The wait on the readers afterwards is bounded by [`DRAIN_HANDBACK`] and is
/// not load-bearing: the pipes reach EOF once the last holder is dead, and the
/// only reason to wait at all is so this function does not return two threads
/// and two descriptors to nobody. A descendant that escaped the group can still
/// outlast the window, and then the runner does what it always did — returns on
/// time and leaves the readers to end when the pipe does.
fn kill_and_reap(
    child: &mut std::process::Child,
    stdout_rx: &mpsc::Receiver<Drained>,
    stderr_rx: &mpsc::Receiver<Drained>,
) {
    kill_descendants(child);
    let _ = child.wait();
    // One budget across both, not one each: the bound the caller was promised is
    // a wall clock, and two waits in series would spend twice what it says.
    let handback = Instant::now() + DRAIN_HANDBACK;
    let _ = stdout_rx.recv_timeout(handback.saturating_duration_since(Instant::now()));
    let _ = stderr_rx.recv_timeout(handback.saturating_duration_since(Instant::now()));
}

/// `SIGKILL` to the child's process group — the child and every descendant that
/// stayed in it.
///
/// The child was spawned with `process_group(0)`, so it leads the group and its
/// pid is the group id; a negated pid names the group to `kill(2)`. If the
/// group cannot be signalled at all the direct child is killed the old way,
/// because "the group call failed" must never come out as "nothing was killed".
#[cfg(unix)]
fn kill_descendants(child: &mut std::process::Child) {
    let Ok(leader) = i32::try_from(child.id()) else {
        let _ = child.kill();
        return;
    };
    // SAFETY: `kill` names a process group led by a child we started and
    // delivers a signal to it; it reads and writes none of our memory.
    let signalled = unsafe { libc::kill(-leader, libc::SIGKILL) } == 0;
    if !signalled {
        let _ = child.kill();
    }
}

/// Elsewhere the deadline reaches the direct child only.
///
/// Windows would need a Job Object — a different lifetime model, with its own
/// handle to own and inherit — and this runner does not have one. Naming the
/// gap here rather than leaving `process_group` silently absent: on these
/// targets a descendant that inherited the pipes survives the deadline, exactly
/// as it did everywhere before.
#[cfg(not(unix))]
fn kill_descendants(child: &mut std::process::Child) {
    let _ = child.kill();
}

struct Drained {
    bytes: Vec<u8>,
    /// The bytes kept are a prefix of what the child had to say — because the
    /// cap cut it, or because the pipe stopped answering before EOF.
    truncated: bool,
}

/// Read a pipe to EOF, keeping at most `cap` bytes.
///
/// Reading continues past the cap on purpose. A reader that stops leaves the
/// child blocked on a full pipe, and then only the deadline ends it — which
/// turns "this answer was long" into "this answer took ten seconds", and
/// charges every caller the full deadline for the privilege of a partial
/// result.
///
/// Only `Ok(0)` is the end. `read` on a pipe may return `Interrupted` before it
/// has transferred anything, and `std` retries that only inside `read_to_end` /
/// `read_exact` — never on a bare `read` — so this loop has to. Any other error
/// leaves a *prefix*, and a prefix is reported the same way the cap's is: the
/// one property every caller of this module reads is "are the bytes I have the
/// whole answer", and there is no version of that question where a pipe that
/// stopped answering counts as yes.
fn drain(pipe: Option<impl Read>, cap: usize) -> Drained {
    let mut drained = Drained {
        bytes: Vec::new(),
        truncated: false,
    };
    let Some(mut pipe) = pipe else {
        return drained;
    };
    let mut chunk = [0u8; 64 * 1024];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                drained.truncated = true;
                break;
            }
            Ok(read) => {
                let room = cap.saturating_sub(drained.bytes.len());
                if read > room {
                    drained.bytes.extend_from_slice(&chunk[..room]);
                    drained.truncated = true;
                } else {
                    drained.bytes.extend_from_slice(&chunk[..read]);
                }
            }
        }
    }
    drained
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;

    #[test]
    fn a_transient_executable_writer_retries_without_duplicating_a_child() {
        let mut attempts = 0;
        let mut child = spawn_with_retry(
            || {
                attempts += 1;
                if attempts < 3 {
                    Err(std::io::Error::from(ErrorKind::ExecutableFileBusy))
                } else {
                    Command::new(std::env::current_exe().unwrap())
                        .arg("--list")
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                }
            },
            "fixture",
            Instant::now(),
            Duration::from_secs(5),
        )
        .expect("transient writer must clear");
        assert!(child.wait().unwrap().success());
        assert_eq!(attempts, 3, "success must end retries immediately");
    }

    #[test]
    fn a_persistent_executable_writer_has_a_finite_retry_budget() {
        let mut attempts = 0;
        let failure = spawn_with_retry(
            || {
                attempts += 1;
                Err(std::io::Error::from(ErrorKind::ExecutableFileBusy))
            },
            "fixture",
            Instant::now(),
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(matches!(failure, Failure::Spawn { .. }));
        assert_eq!(attempts, 8);
    }

    #[test]
    fn executable_retries_do_not_reset_the_overall_deadline() {
        let started = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        let mut attempts = 0;
        let failure = spawn_with_retry(
            || {
                attempts += 1;
                Err(std::io::Error::from(ErrorKind::ExecutableFileBusy))
            },
            "fixture",
            started,
            Duration::from_millis(20),
        )
        .unwrap_err();
        assert!(matches!(failure, Failure::Deadline { .. }));
        assert_eq!(attempts, 0, "an expired request must not execute");
    }

    #[test]
    fn an_ordinary_spawn_error_is_never_retried() {
        let mut attempts = 0;
        let failure = spawn_with_retry(
            || {
                attempts += 1;
                Err(std::io::Error::from(ErrorKind::PermissionDenied))
            },
            "fixture",
            Instant::now(),
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(
            matches!(failure, Failure::Spawn { error, .. } if error.kind() == ErrorKind::PermissionDenied)
        );
        assert_eq!(attempts, 1);
    }

    /// A reader that returns a scripted sequence, so `drain`'s error handling
    /// can be exercised without arranging a signal.
    struct Scripted(std::collections::VecDeque<std::io::Result<&'static [u8]>>);

    impl Read for Scripted {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.pop_front() {
                None => Ok(0),
                Some(Err(error)) => Err(error),
                Some(Ok(bytes)) => {
                    buf[..bytes.len()].copy_from_slice(bytes);
                    Ok(bytes.len())
                }
            }
        }
    }

    fn scripted(steps: Vec<std::io::Result<&'static [u8]>>) -> Option<Scripted> {
        Some(Scripted(steps.into_iter().collect()))
    }

    /// `read` is allowed to return `Interrupted` before it has transferred
    /// anything, and `std` retries that only inside `read_to_end` /
    /// `read_exact` — never on a bare `read`. Reading it as EOF drops the rest
    /// of the child's answer.
    #[test]
    fn an_interrupted_read_is_resumed_not_read_as_the_end() {
        let drained = drain(
            scripted(vec![
                Ok(b"first "),
                Err(std::io::Error::from(ErrorKind::Interrupted)),
                Ok(b"second"),
            ]),
            1 << 20,
        );
        assert_eq!(drained.bytes, b"first second");
        assert!(
            !drained.truncated,
            "nothing was lost, so nothing may be reported as lost"
        );
    }

    /// A pipe that stops answering before EOF leaves a *prefix*. The whole
    /// point of the cap is that a prefix is never presented as the whole
    /// answer, and a read error produces exactly the same prefix.
    #[test]
    fn a_pipe_that_fails_before_eof_leaves_a_prefix_that_says_so() {
        let drained = drain(
            scripted(vec![
                Ok(b"as far as this"),
                Err(std::io::Error::from(ErrorKind::BrokenPipe)),
                Ok(b"never seen"),
            ]),
            1 << 20,
        );
        assert_eq!(drained.bytes, b"as far as this");
        assert!(
            drained.truncated,
            "the read stopped short of EOF; the caller is holding a prefix and \
             has just been told it is the whole answer"
        );
    }

    /// The cap's own case, unchanged: bytes past it are dropped and reported.
    #[test]
    fn the_cap_still_reports_its_own_truncation() {
        let drained = drain(scripted(vec![Ok(b"0123456789")]), 4);
        assert_eq!(drained.bytes, b"0123");
        assert!(drained.truncated);
    }

    /// …and a clean read to EOF reports nothing.
    #[test]
    fn a_clean_read_reports_no_truncation() {
        let drained = drain(scripted(vec![Ok(b"whole")]), 1 << 20);
        assert_eq!(drained.bytes, b"whole");
        assert!(!drained.truncated);
    }
}
