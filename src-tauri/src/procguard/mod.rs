//! Children this process spawned must not outlive it.
//!
//! # The hole this fills
//!
//! [`std::process::Child`] has no killing `Drop`, and [`std::process::exit`]
//! runs no destructors at all. So a `git` we started survives us in two very
//! ordinary situations:
//!
//! * a supervisor (launchd, systemd, an agent host, a shell) sends SIGTERM,
//!   whose default action terminates us immediately, and
//! * `gitpulse-mcp` reaches its documented "close stdin and exit" shutdown and
//!   calls `process::exit` deliberately without waiting on in-flight workers.
//!
//! In both, the orphan keeps running: it holds `.git/index.lock`, it keeps
//! writing to a repository nobody is watching any more, and it is attributed
//! to a process that is gone.
//!
//! # How it is closed
//!
//! One seam, [`spawn`], does three things that only make sense together:
//!
//! 1. `setpgid(0, 0)` in the post-fork child, making it a process-group leader
//!    whose group id equals its pid. Killing that group reaches the child *and
//!    everything it forked* — which is also what retires the grandchild leak
//!    documented on `engine::git_cli::collect_drained`.
//! 2. registration in a process-wide table of live children, and
//! 3. a [`Registration`] that keeps the table honest as the child is reaped.
//!
//! [`install_signal_handlers`] then turns SIGHUP/SIGINT/SIGTERM into a run
//! through [`reap_all`] instead of an immediate death, and [`reap_all`] is
//! callable directly for the `process::exit` case.
//!
//! # Signalling a pid is only safe while we still own it
//!
//! After `wait` succeeds the kernel may hand that pid to anybody, so a late
//! `killpg` could take down an unrelated process group. Every reap therefore
//! goes through [`Registration::poll`] or [`Registration::reap`], which clear
//! the recorded pid *while holding that entry's lock*. The sweep takes the
//! same lock before it signals, so "reaped" and "signalled" cannot interleave:
//! the sweep either signals a pid we still own or sees `None`.
//!
//! # Only for children with no inherited terminal
//!
//! A process group of its own is a *background* group with respect to the
//! controlling terminal: a child in one that reads the tty gets SIGTTIN and
//! stops. Every child spawned through this module is given piped or null stdio
//! by its caller (`engine::git_cli::run_bounded_capped`,
//! `harness::sidecar::spawn`), so none of them has a tty to touch. A future
//! caller that wants `Stdio::inherit()` on a terminal must not use [`spawn`].
//!
//! # Windows
//!
//! There is no `setpgid`, and the OS delivers no SIGTERM — a console control
//! handler or a Job Object is the equivalent, and both need a Windows-specific
//! dependency this crate does not have. So on Windows the registry and
//! [`reap_all`] still work, backed by the `taskkill /T /F` tree kill the
//! timeout path already used, and [`install_signal_handlers`] reports plainly
//! that it armed nothing rather than returning the same value as a successful
//! install.

use std::collections::BTreeMap;
use std::io;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, TryLockError};
use std::time::{Duration, Instant};

/// How long a child gets between "please stop" and "stop".
///
/// This is the window in which `git` runs its own cleanup and removes
/// `.git/index.lock`. Skipping it and going straight to SIGKILL leaves that
/// file behind, and the user's next command fails with "Unable to create
/// index.lock: File exists" — a shutdown that breaks the repository it was
/// protecting. Half a second is far more than git's cleanup needs and far less
/// than any supervisor's own patience before it escalates on us.
pub const CHILD_GRACE: Duration = Duration::from_millis(500);

/// How long the sweep waits for one entry's lock before moving on.
///
/// A locked entry means its owner is inside a reap right now, which is the
/// outcome the sweep wants anyway. The budget exists so a wedged owner cannot
/// stall the whole shutdown, and anything skipped is reported rather than
/// counted as handled.
const ENTRY_LOCK_BUDGET: Duration = Duration::from_millis(100);

/// Poll interval while waiting out [`CHILD_GRACE`].
const LIVENESS_POLL: Duration = Duration::from_millis(10);

/// How long [`exit`] waits for the shutdown watcher to finish its sweep.
///
/// Comfortably more than [`CHILD_GRACE`] plus the syscalls around it, so the
/// bound only ever fires if the watcher is wedged — in which case exiting with
/// the signal's code is still a better answer than reporting success.
const HANDOVER: Duration = Duration::from_secs(2);

/// A registered child.
///
/// `label` lives outside the lock on purpose: it never changes after
/// registration, and the one moment the sweep most needs to name a child is
/// the moment it could not take that child's lock.
struct Entry {
    label: String,
    /// The child's pid, which on Unix is also its process-group id because
    /// [`spawn`] made it a group leader.
    ///
    /// `None` once its owner has reaped it. The distinction is the whole
    /// safety argument: a pid we have waited on is no longer ours to signal.
    pid: Mutex<Option<u32>>,
}

type Slot = Arc<Entry>;

fn registry() -> &'static Mutex<BTreeMap<u64, Slot>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<u64, Slot>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn next_key() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Set by [`reap_all`], never cleared.
///
/// A sweep is a snapshot, and a snapshot is not a guarantee on its own: a
/// worker whose child had just been killed goes on to run its *next* git
/// command, which is registered after the sweep has already passed it by and
/// is then orphaned by the `process::exit` that follows. That is not
/// hypothetical — it is what `closing_stdin_reaps_a_git_child_a_worker_left_
/// running` caught, with two of four recorded processes surviving.
///
/// So the seam closes before the sweep starts. Anything already running is in
/// the snapshot; anything that tries to start after it is refused.
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

fn shutting_down() -> bool {
    SHUTTING_DOWN.load(Ordering::SeqCst)
}

fn refused(label: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Interrupted,
        format!("refusing to start {label}: this process is shutting down"),
    )
}

/// A live child's place in the registry. Dropping it removes the entry, and
/// kills the child's group first if nothing ever reaped it.
pub struct Registration {
    key: u64,
    slot: Slot,
}

impl Registration {
    /// Runs a *non-blocking* wait under this entry's lock, forgetting the pid
    /// if the child was reaped.
    ///
    /// `f` must be [`Child::try_wait`] or an equivalent: `Ok(Some(_))` has to
    /// mean "this pid has been waited on and is no longer ours".
    pub fn poll<T>(&self, f: impl FnOnce() -> io::Result<Option<T>>) -> io::Result<Option<T>> {
        let mut pid = lock(&self.slot.pid);
        let outcome = f();
        if matches!(outcome, Ok(Some(_))) {
            *pid = None;
        }
        outcome
    }

    /// Runs a reap that is expected to consume the child — [`Child::wait`]
    /// after a kill — under this entry's lock, and forgets the pid
    /// unconditionally.
    ///
    /// Unconditional because a `wait` that fails has still lost track of the
    /// child: continuing to hold its pid would leave the sweep signalling a
    /// number we can no longer reason about.
    pub fn reap<R>(&self, f: impl FnOnce() -> R) -> R {
        let mut pid = lock(&self.slot.pid);
        let out = f();
        *pid = None;
        out
    }

    /// Kills the child *and every process it forked*, without reaping it.
    ///
    /// Unix takes the whole process group down at once, which is the part a
    /// bare [`Child::kill`] cannot do. `child` is still needed: the direct
    /// kill stays as a backstop for a group the leader has already left, and
    /// on Windows the tree kill is all there is.
    pub fn kill_tree(&self, child: &mut Child) {
        let pid = lock(&self.slot.pid);
        if let Some(pid) = *pid {
            let _ = sys::force_kill(pid);
        }
        let _ = child.kill();
    }

    /// The child's pid while it is still ours to signal.
    pub fn pid(&self) -> Option<u32> {
        *lock(&self.slot.pid)
    }

    // Reached only from the unix-gated tests, so `#[cfg(test)]` alone leaves it
    // dead on a Windows test build.
    #[cfg(all(test, unix))]
    fn slot(&self) -> Slot {
        Arc::clone(&self.slot)
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        // Still holding a pid here means no reap path ran — the wait itself
        // failed, or an early return skipped it. Leaving the child running
        // would be exactly the orphan this module exists to prevent, and the
        // pid is provably un-recycled because nothing has waited on it.
        {
            let mut pid = lock(&self.slot.pid);
            if let Some(pid) = pid.take() {
                let _ = sys::force_kill(pid);
            }
        }
        lock(registry()).remove(&self.key);
    }
}

/// Spawns `cmd` as a killable process group and registers the result.
///
/// This is the only supported way to start a child that must not outlive us;
/// preparing the command and registering it are one decision, and splitting
/// them into two calls is how half of them get forgotten.
///
/// Fails with [`io::ErrorKind::Interrupted`] once [`reap_all`] has run. The
/// check is made twice on purpose: once before the fork, which handles the
/// ordinary case cheaply, and once after registering, which is the only way to
/// catch a caller that passed the first check while the sweep was taking its
/// snapshot. A child caught by the second check is killed and reaped here
/// rather than handed back, because its caller is about to be told the spawn
/// did not happen.
pub fn spawn(cmd: &mut Command, label: &str) -> io::Result<(Child, Registration)> {
    if shutting_down() {
        return Err(refused(label));
    }
    sys::prepare(cmd);
    let mut child = cmd.spawn()?;
    let slot: Slot = Arc::new(Entry {
        label: label.to_string(),
        pid: Mutex::new(Some(child.id())),
    });
    let key = next_key();
    lock(registry()).insert(key, Arc::clone(&slot));
    let registration = Registration { key, slot };
    if shutting_down() {
        registration.kill_tree(&mut child);
        let _ = registration.reap(|| child.wait());
        return Err(refused(label));
    }
    Ok((child, registration))
}

/// What one [`reap_all`] did, in terms that separate what it verified from
/// what it could not.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Sweep {
    /// Children that exited within [`CHILD_GRACE`] of being asked to.
    pub stopped_when_asked: usize,
    /// Children still alive after the grace window, killed outright.
    pub force_killed: usize,
    /// Registered children that had already exited when the sweep reached them.
    pub already_gone: usize,
    /// Entries whose lock could not be taken inside [`ENTRY_LOCK_BUDGET`].
    /// **Not swept** — listed so an unswept child is never counted as a swept
    /// one.
    pub busy: Vec<String>,
    /// Entries whose kill syscall failed for a reason other than "no such
    /// process". Also not swept.
    pub failed: Vec<String>,
}

impl Sweep {
    /// True when every registered child was accounted for.
    pub fn is_complete(&self) -> bool {
        self.busy.is_empty() && self.failed.is_empty()
    }

    pub fn describe(&self) -> String {
        let mut parts = vec![format!(
            "children: {} stopped when asked, {} force-killed, {} already exited",
            self.stopped_when_asked, self.force_killed, self.already_gone
        )];
        if !self.busy.is_empty() {
            parts.push(format!("NOT swept (owner busy): {}", self.busy.join(", ")));
        }
        if !self.failed.is_empty() {
            parts.push(format!(
                "NOT swept (kill failed): {}",
                self.failed.join(", ")
            ));
        }
        parts.join("; ")
    }
}

/// Takes down every registered child, and everything those children forked.
///
/// Ask-then-kill where the platform can ask (Unix: SIGTERM, then SIGKILL after
/// `grace`), a single tree kill where it cannot (Windows). Returns immediately
/// when nothing is registered, so calling it on a clean exit path costs
/// nothing.
///
/// **Terminal.** It closes [`spawn`] permanently before it looks at anything,
/// so every later spawn fails. Call it only from a path that is about to end
/// the process; there is deliberately no way to reopen the seam, because a
/// shutdown that can be cancelled is a shutdown that can be half-done.
pub fn reap_all(grace: Duration) -> Sweep {
    SHUTTING_DOWN.store(true, Ordering::SeqCst);
    let slots: Vec<Slot> = lock(registry()).values().cloned().collect();
    sweep(&slots, grace)
}

/// One slot's outcome from the ask pass, carried into the kill pass so nothing
/// is counted twice.
enum Ask {
    /// Signal delivered; this pid still has to be checked.
    Pending(u32),
    /// Owner had already reaped it, or its group was gone.
    Gone,
    /// Could not take the lock, so nothing was asked of it.
    Busy,
    /// The platform has no graceful step; the kill pass decides everything.
    NotSupported,
}

fn sweep(slots: &[Slot], grace: Duration) -> Sweep {
    let mut sweep = Sweep::default();
    if slots.is_empty() {
        return sweep;
    }

    let mut asked: Vec<Ask> = Vec::with_capacity(slots.len());
    if sys::CAN_ASK_TO_STOP {
        for slot in slots {
            asked.push(match try_lock_for(&slot.pid, ENTRY_LOCK_BUDGET) {
                None => Ask::Busy,
                Some(pid) => match *pid {
                    None => Ask::Gone,
                    Some(pid) => match sys::request_stop(pid) {
                        Ok(true) => Ask::Pending(pid),
                        Ok(false) => Ask::Gone,
                        Err(e) => {
                            sweep.failed.push(format!("{} ({pid}): {e}", slot.label));
                            Ask::Gone
                        }
                    },
                },
            });
        }
        let pending: Vec<(&Slot, u32)> = slots
            .iter()
            .zip(&asked)
            .filter_map(|(slot, a)| match a {
                Ask::Pending(pid) => Some((slot, *pid)),
                _ => None,
            })
            .collect();
        wait_out_grace(&pending, grace);
    } else {
        asked.resize_with(slots.len(), || Ask::NotSupported);
    }

    for (slot, asked) in slots.iter().zip(asked) {
        // An entry the ask pass could not reach is reported once, from here,
        // and only if the kill pass cannot reach it either.
        let Some(mut pid_slot) = try_lock_for(&slot.pid, ENTRY_LOCK_BUDGET) else {
            sweep.busy.push(slot.label.clone());
            continue;
        };
        let Some(pid) = *pid_slot else {
            match asked {
                // Reaped by its owner inside the grace window: it stopped
                // because we asked.
                Ask::Pending(_) => sweep.stopped_when_asked += 1,
                _ => sweep.already_gone += 1,
            }
            continue;
        };
        match sys::force_kill(pid) {
            Ok(true) => match asked {
                // Still there after being asked and given the grace window.
                Ask::Pending(_) => sweep.force_killed += 1,
                Ask::NotSupported => sweep.force_killed += 1,
                // The ask pass could not lock it, so this is its first signal.
                Ask::Busy => sweep.force_killed += 1,
                Ask::Gone => sweep.already_gone += 1,
            },
            Ok(false) => match asked {
                Ask::Pending(_) => sweep.stopped_when_asked += 1,
                _ => sweep.already_gone += 1,
            },
            Err(e) => sweep.failed.push(format!("{} ({pid}): {e}", slot.label)),
        }
        // The group is gone either way; stop treating the pid as ours so a
        // later sweep cannot signal a recycled one.
        *pid_slot = None;
    }

    sweep
}

/// Waits up to `grace` for everything we asked to stop to actually stop.
///
/// Settled means one of two things, and the owner's answer is the better one:
///
/// * its owner reaped it, which clears the recorded pid, or
/// * the OS says nothing in its group can be signalled any more.
///
/// The OS answer alone is not enough, because the two platforms disagree about
/// a child that has exited but not been reaped: macOS reports its group as
/// unsignalable, Linux happily reports it as still there. Watching the
/// registry instead makes the common case — the owning thread reaping a `git`
/// that stopped when asked — end the wait promptly on both.
fn wait_out_grace(pending: &[(&Slot, u32)], grace: Duration) {
    if pending.is_empty() {
        return;
    }
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        let settled = pending.iter().all(|(slot, pid)| match slot.pid.try_lock() {
            Ok(recorded) => recorded.is_none() || !sys::is_alive(*pid),
            // Its owner is mid-reap: not settled yet, and not ours to guess at.
            Err(TryLockError::WouldBlock) => false,
            Err(TryLockError::Poisoned(p)) => p.into_inner().is_none() || !sys::is_alive(*pid),
        });
        if settled {
            return;
        }
        std::thread::sleep(LIVENESS_POLL);
    }
}

fn try_lock_for(pid: &Mutex<Option<u32>>, budget: Duration) -> Option<MutexGuard<'_, Option<u32>>> {
    let deadline = Instant::now() + budget;
    loop {
        match pid.try_lock() {
            Ok(guard) => return Some(guard),
            Err(TryLockError::Poisoned(p)) => return Some(p.into_inner()),
            Err(TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

/// What [`install_signal_handlers`] actually armed.
///
/// Modelled on `limits::raise_open_file_limit`: the caller logs
/// [`Self::describe`] either way, so a shutdown guarantee that could not be
/// installed is visible in the log instead of assumed.
#[derive(Debug)]
pub struct SignalGuard {
    installed: Vec<&'static str>,
    skipped: Vec<String>,
}

impl SignalGuard {
    /// True when at least one termination signal now runs the sweep.
    pub fn is_armed(&self) -> bool {
        !self.installed.is_empty()
    }

    pub fn describe(&self) -> String {
        let armed = if self.installed.is_empty() {
            "no termination signal is handled".to_string()
        } else {
            format!("spawned children reaped on {}", self.installed.join(", "))
        };
        if self.skipped.is_empty() {
            armed
        } else {
            format!("{armed}; not handled: {}", self.skipped.join(", "))
        }
    }
}

/// Ends this process, deferring to a shutdown that is already running.
///
/// A plain `std::process::exit` on a normal return path races the shutdown
/// watcher, and loses in both directions: it can end the process mid-sweep,
/// leaving alive exactly the children the sweep was killing, and it reports
/// the caller's code — success — for a run that was terminated. Both were
/// observed: SIGTERM to `gitpulsed` freed its blocked `git`, whose worker then
/// finished the cycle and returned from `main` in 107 ms, while the sweep was
/// still inside its grace window.
///
/// So every `main` ends here. With no signal caught this is
/// `std::process::exit`; with one, it hands over to the watcher and falls back
/// to the signal's own code if the watcher does not finish inside
/// [`HANDOVER`].
pub fn exit(normal: i32) -> ! {
    let Some(signo) = sys::caught_signal() else {
        std::process::exit(normal);
    };
    let deadline = Instant::now() + HANDOVER;
    while Instant::now() < deadline {
        std::thread::sleep(LIVENESS_POLL);
    }
    std::process::exit(128 + signo);
}

/// Turns termination signals into a [`reap_all`] followed by an exit.
///
/// Idempotent: a second call reports the first call's result rather than
/// installing a second handler. Call it once, early, from `main` — signal
/// disposition is process-global, so a library path that installed it as a
/// side effect of running `git` would be changing its embedder's behaviour
/// without being asked.
pub fn install_signal_handlers() -> SignalGuard {
    sys::install_signal_handlers()
}

#[cfg(unix)]
mod sys {
    use super::{reap_all, SignalGuard, CHILD_GRACE};
    use std::io;
    use std::os::unix::io::FromRawFd;
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

    /// Unix can ask a child to stop before insisting.
    pub(super) const CAN_ASK_TO_STOP: bool = true;

    /// Signals whose default action ends the process and which a supervisor, a
    /// shell or a terminal actually sends.
    ///
    /// SIGQUIT is deliberately absent: its default action writes a core dump,
    /// and swallowing that would take away a debugging tool to gain nothing —
    /// a SIGQUIT'd process is one somebody is inspecting, not one shutting
    /// down cleanly.
    const HANDLED: &[(libc::c_int, &str)] = &[
        (libc::SIGHUP, "SIGHUP"),
        (libc::SIGINT, "SIGINT"),
        (libc::SIGTERM, "SIGTERM"),
    ];

    static INSTALLED: AtomicBool = AtomicBool::new(false);
    static WAKE_WRITE_FD: AtomicI32 = AtomicI32::new(-1);
    /// The signal that started the shutdown, 0 while none has. Read by
    /// [`super::exit`] so a normal return cannot overtake the watcher.
    static CAUGHT: AtomicI32 = AtomicI32::new(0);

    pub(super) fn caught_signal() -> Option<i32> {
        match CAUGHT.load(Ordering::SeqCst) {
            0 => None,
            signo => Some(signo),
        }
    }

    /// Makes the child a process-group leader.
    ///
    /// `process_group(0)` is `setpgid(0, 0)` asked for declaratively, and the
    /// distinction is not cosmetic: writing it as a `pre_exec` closure instead
    /// disqualifies the command from the standard library's `posix_spawn` fast
    /// path and forces a `fork` + `exec` of a process holding the parent's
    /// whole address space. `benches/process_spawn.rs` measures both spellings
    /// of the identical syscall: `process_group(0)` lands within noise of a
    /// plain spawn, and `pre_exec` cost between 35% and 95% more per spawn
    /// across runs — on the seam every `git` GitPulse runs goes through. It
    /// also needs no `unsafe`.
    pub(super) fn prepare(cmd: &mut Command) {
        cmd.process_group(0);
    }

    /// `Ok(true)` when the signal reached a live member of the group,
    /// `Ok(false)` when there was nothing left in it to signal.
    ///
    /// Two errnos mean the second thing, and conflating them with a real
    /// failure is what made the first version of this report every ordinary
    /// shutdown as "kill failed":
    ///
    /// * `ESRCH` — no such process group.
    /// * `EPERM` — the group exists but nothing in it can be signalled. On
    ///   macOS that is exactly what a group whose only remaining member is an
    ///   unreaped zombie returns, which is the completely normal state between
    ///   a child exiting and its owner calling `wait`. (Linux answers 0 for
    ///   the same case, so this only ever *adds* accuracy there.)
    ///
    /// Reading `EPERM` as "someone else's group" would require the pid to have
    /// been recycled, and the registry's rule that a reaped pid is forgotten
    /// under the entry lock is what rules that out.
    fn signal_group(pid: u32, signo: libc::c_int) -> io::Result<bool> {
        let pgid = i32::try_from(pid).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("pid {pid} does not fit a pid_t"),
            )
        })?;
        if unsafe { libc::killpg(pgid, signo) } == 0 {
            return Ok(true);
        }
        let e = io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::ESRCH) | Some(libc::EPERM) => Ok(false),
            _ => Err(e),
        }
    }

    pub(super) fn request_stop(pid: u32) -> io::Result<bool> {
        signal_group(pid, libc::SIGTERM)
    }

    pub(super) fn force_kill(pid: u32) -> io::Result<bool> {
        signal_group(pid, libc::SIGKILL)
    }

    /// Signal 0 checks for a signalable member without sending anything.
    ///
    /// An answer we could not obtain is reported as *alive*: the only thing
    /// that costs is waiting out the rest of the grace window before the kill
    /// pass, whereas guessing "gone" would end the sweep early on a child that
    /// is still running.
    pub(super) fn is_alive(pid: u32) -> bool {
        signal_group(pid, 0).unwrap_or(true)
    }

    /// Runs in signal context: everything it touches must be
    /// async-signal-safe. An atomic load and `write(2)` are; taking the
    /// registry lock here would deadlock against whichever thread was already
    /// holding it, which is why the real work happens on the watcher thread
    /// this wakes.
    extern "C" fn on_signal(signo: libc::c_int) {
        // An atomic store is async-signal-safe, and this is what stops a
        // normal `main` return from exiting 0 out from under the sweep.
        CAUGHT.store(signo, Ordering::SeqCst);
        // Every signal in HANDLED is well under 128, so this byte round-trips
        // back to the same number on the other end of the pipe.
        let byte = [signo as u8];
        let fd = WAKE_WRITE_FD.load(Ordering::SeqCst);
        if fd >= 0 {
            unsafe { libc::write(fd, byte.as_ptr().cast(), 1) };
        }
    }

    /// # Safety
    /// `handler` must be `SIG_DFL`, `SIG_IGN`, or a function pointer that is
    /// safe to run in signal context.
    unsafe fn set_disposition(signo: libc::c_int, handler: libc::sighandler_t) -> bool {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = handler;
        libc::sigemptyset(&mut action.sa_mask);
        // SA_RESTART so an interrupted read or write in a worker resumes
        // instead of surfacing EINTR to code with no reason to expect it.
        action.sa_flags = libc::SA_RESTART;
        libc::sigaction(signo, &action, std::ptr::null_mut()) == 0
    }

    fn restore_defaults() {
        for (signo, _) in HANDLED {
            // SAFETY: SIG_DFL is always a valid disposition.
            unsafe { set_disposition(*signo, libc::SIG_DFL) };
        }
    }

    pub(super) fn install_signal_handlers() -> SignalGuard {
        if INSTALLED.swap(true, Ordering::SeqCst) {
            return SignalGuard {
                installed: HANDLED.iter().map(|(_, name)| *name).collect(),
                skipped: vec!["(handlers were already installed)".to_string()],
            };
        }

        let mut fds = [0 as libc::c_int; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            let why = io::Error::last_os_error();
            INSTALLED.store(false, Ordering::SeqCst);
            return SignalGuard {
                installed: Vec::new(),
                skipped: vec![format!("wake pipe could not be created: {why}")],
            };
        }
        // Children must not inherit the wake pipe: a long-lived one holding the
        // write end would keep it open past our own exit.
        for fd in fds {
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }

        // SAFETY: `fds[0]` is a fresh pipe end this function owns and never
        // touches again.
        let reader = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        if let Err(e) = std::thread::Builder::new()
            .name("procguard-shutdown".to_string())
            .spawn(move || watch(reader))
        {
            // Without the watcher a caught signal would be swallowed and the
            // process would become unkillable by SIGTERM. Never install the
            // handlers at all, and say why.
            unsafe { libc::close(fds[1]) };
            INSTALLED.store(false, Ordering::SeqCst);
            return SignalGuard {
                installed: Vec::new(),
                skipped: vec![format!("shutdown watcher could not start: {e}")],
            };
        }
        WAKE_WRITE_FD.store(fds[1], Ordering::SeqCst);

        let handler: extern "C" fn(libc::c_int) = on_signal;
        let mut installed = Vec::new();
        let mut skipped = Vec::new();
        for (signo, name) in HANDLED {
            // SAFETY: `on_signal` only stores to an atomic and calls `write`.
            if unsafe { set_disposition(*signo, handler as libc::sighandler_t) } {
                installed.push(*name);
            } else {
                skipped.push(format!("{name}: {}", io::Error::last_os_error()));
            }
        }
        SignalGuard { installed, skipped }
    }

    /// Blocks until [`on_signal`] writes, then sweeps and exits.
    ///
    /// This runs on an ordinary thread, so it may allocate, lock and log — the
    /// whole point of waking it through a pipe rather than doing the work in
    /// signal context.
    fn watch(mut reader: std::fs::File) {
        use std::io::Read;
        let mut byte = [0u8; 1];
        loop {
            match reader.read(&mut byte) {
                Ok(1) => break,
                // The write end is held by this process for its whole life, so
                // EOF is unreachable; treat it like any other broken wake.
                Ok(_) => return give_up("shutdown watcher saw an empty wake"),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return give_up(&format!("shutdown watcher read failed: {e}")),
            }
        }
        let signo = libc::c_int::from(byte[0]);
        let name = HANDLED
            .iter()
            .find(|(s, _)| *s == signo)
            .map(|(_, n)| *n)
            .unwrap_or("signal");

        let sweep = reap_all(CHILD_GRACE);
        if sweep.is_complete() {
            log::info!(target: "shutdown", "{name}: {}", sweep.describe());
        } else {
            log::warn!(target: "shutdown", "{name}: {}", sweep.describe());
        }
        // 128 + signo is what a shell reports for a signalled process, and what
        // a supervisor watching our exit status expects to see.
        std::process::exit(128 + signo);
    }

    /// Hands the signals back to the kernel so the process stays killable.
    fn give_up(why: &str) {
        log::error!(target: "shutdown", "{why}; default signal handling restored");
        restore_defaults();
        WAKE_WRITE_FD.store(-1, Ordering::SeqCst);
        CAUGHT.store(0, Ordering::SeqCst);
        INSTALLED.store(false, Ordering::SeqCst);
    }
}

#[cfg(windows)]
mod sys {
    use super::SignalGuard;
    use std::io;
    use std::process::{Command, Stdio};

    /// `taskkill` without `/F` sends WM_CLOSE, which a console `git` never
    /// sees, so there is no graceful step to take.
    pub(super) const CAN_ASK_TO_STOP: bool = false;

    /// No `setpgid` here; the tree is walked at kill time by pid instead.
    pub(super) fn prepare(_cmd: &mut Command) {}

    pub(super) fn request_stop(_pid: u32) -> io::Result<bool> {
        Ok(false)
    }

    pub(super) fn force_kill(pid: u32) -> io::Result<bool> {
        // `/T` walks the pid tree, `/F` terminates. Spawned directly by argv,
        // never through a shell. A non-zero status is "no such process" as
        // often as it is a real failure, and the two are not distinguishable
        // from the exit code alone, so this reports "already gone" rather than
        // inventing an error.
        let status = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        Ok(status.success())
    }

    /// Never called: [`CAN_ASK_TO_STOP`] is false, so there is no grace window
    /// to wait out.
    pub(super) fn is_alive(_pid: u32) -> bool {
        false
    }

    /// Nothing is ever caught here, so [`super::exit`] is a plain exit.
    pub(super) fn caught_signal() -> Option<i32> {
        None
    }

    pub(super) fn install_signal_handlers() -> SignalGuard {
        SignalGuard {
            installed: Vec::new(),
            skipped: vec![
                "Windows delivers no SIGTERM; a console control handler or a \
                 Job Object needs a Windows-specific dependency this crate \
                 does not have"
                    .to_string(),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the `#[cfg(unix)]` cases below spawn and signal real children, so on
    // Windows these are unused imports and clippy's `-D warnings` rejects them.
    #[cfg(unix)]
    use std::process::{Child, Stdio};
    #[cfg(unix)]
    use std::sync::mpsc;

    /// Process-level liveness, which is not the same question as
    /// [`sys::is_alive`]: that one asks about a process *group*, and a
    /// grandchild is not a group leader, so asking it about one always
    /// answers "gone".
    #[cfg(unix)]
    fn process_alive(pid: u32) -> bool {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    /// Waits for a pid to leave the process table, so an assertion never
    /// depends on how quickly `init` reaps a reparented orphan.
    #[cfg(unix)]
    fn wait_gone(pid: u32, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if !process_alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// Reaps `child`, refusing to block forever if the kill did not land.
    #[cfg(unix)]
    fn reap_within(child: &mut Child, budget: Duration) -> std::process::ExitStatus {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            match child.try_wait().expect("try_wait") {
                Some(status) => return status,
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        panic!("the child was still running {budget:?} after it should have been killed");
    }

    /// A shell that ignores SIGTERM and then execs, so the group holds exactly
    /// one process and no zombie churn.
    ///
    /// `echo ready` is the synchronisation that matters: `trap` is installed
    /// by the shell at runtime, so a signal sent before the shell has read its
    /// script kills it by default and the test would be measuring its own race
    /// rather than the sweep.
    #[cfg(unix)]
    fn stubborn() -> Command {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("trap '' TERM; echo ready; exec sleep 30")
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        cmd
    }

    #[cfg(unix)]
    fn wait_for_ready(child: &mut Child) {
        use std::io::BufRead;
        let stdout = child.stdout.take().expect("piped stdout");
        let mut line = String::new();
        std::io::BufReader::new(stdout)
            .read_line(&mut line)
            .expect("read readiness");
        assert_eq!(line.trim(), "ready", "the fixture never armed its trap");
    }

    /// The property everything else rests on: killing the child's pid as a
    /// *group* only reaches its descendants if the child leads a group of its
    /// own.
    #[cfg(unix)]
    #[test]
    fn a_spawned_child_leads_a_process_group_of_its_own() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30").stdin(Stdio::null()).stdout(Stdio::null());
        let (mut child, guard) = spawn(&mut cmd, "sleep").expect("spawn");
        let pid = child.id();
        let pgid = unsafe { libc::getpgid(pid as i32) };
        assert_eq!(
            pgid, pid as i32,
            "the child is still in our process group, so killpg would signal us"
        );
        assert_ne!(
            pgid,
            unsafe { libc::getpgid(0) },
            "the child must not share this process's group"
        );
        guard.kill_tree(&mut child);
        let _ = guard.reap(|| child.wait());
    }

    /// The capability a bare `Child::kill` does not have, and the reason the
    /// grandchild leak documented on `git_cli::collect_drained` shrank.
    #[cfg(unix)]
    #[test]
    fn kill_tree_takes_a_grandchild_with_it() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("sleep 30 & echo $!; sleep 30")
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        let (mut child, guard) = spawn(&mut cmd, "sh").expect("spawn");
        let stdout = child.stdout.take().expect("piped stdout");
        let grandchild: u32 = {
            use std::io::BufRead;
            let mut line = String::new();
            std::io::BufReader::new(stdout)
                .read_line(&mut line)
                .expect("read grandchild pid");
            line.trim().parse().expect("pid")
        };
        assert!(process_alive(grandchild), "fixture never started");

        guard.kill_tree(&mut child);
        let _ = guard.reap(|| child.wait());

        assert!(
            wait_gone(grandchild, Duration::from_secs(5)),
            "the grandchild survived a kill aimed at its parent"
        );
    }

    /// A pid that has been waited on is no longer ours: continuing to hold it
    /// is what would let a later sweep signal a recycled one.
    #[cfg(unix)]
    #[test]
    fn a_reaped_child_is_forgotten() {
        let mut cmd = Command::new("true");
        cmd.stdin(Stdio::null()).stdout(Stdio::null());
        let (mut child, guard) = spawn(&mut cmd, "true").expect("spawn");
        assert!(guard.pid().is_some(), "registered while running");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if guard.poll(|| child.try_wait()).expect("try_wait").is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            guard.pid(),
            None,
            "a reaped pid stayed in the registry, where a sweep could signal it"
        );
    }

    /// The path where nothing reaps: a `wait` that failed, or an early return
    /// past the timeout arm. Before this the child simply kept running.
    #[cfg(unix)]
    #[test]
    fn dropping_a_registration_kills_a_child_nothing_reaped() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30").stdin(Stdio::null()).stdout(Stdio::null());
        let (mut child, guard) = spawn(&mut cmd, "sleep").expect("spawn");
        drop(guard);
        let status = reap_within(&mut child, Duration::from_secs(5));
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&status),
            Some(libc::SIGKILL),
            "an unreaped child outlived its registration"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dropping_a_registration_removes_its_registry_entry() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30").stdin(Stdio::null()).stdout(Stdio::null());
        let (mut child, guard) = spawn(&mut cmd, "sleep").expect("spawn");
        // By key, not by count: other tests spawn concurrently, and a count
        // would be asserting on their timing rather than on this entry.
        let key = guard.key;
        assert!(
            lock(registry()).contains_key(&key),
            "spawning did not register the child"
        );
        drop(guard);
        assert!(
            !lock(registry()).contains_key(&key),
            "the entry outlived its registration"
        );
        let _ = child.wait();
    }

    /// SIGTERM first, so `git` gets to remove its own `.git/index.lock`
    /// instead of leaving it for the user's next command to trip over.
    ///
    /// A poller stands in for the owner thread that `run_bounded_capped` runs
    /// in production; without one the exited child would linger as a zombie
    /// and the sweep would report it as force-killed, which is the honest
    /// answer for a corpse nobody has claimed.
    #[cfg(unix)]
    #[test]
    fn the_sweep_asks_before_it_insists() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30").stdin(Stdio::null()).stdout(Stdio::null());
        let (mut child, guard) = spawn(&mut cmd, "polite").expect("spawn");
        let guard = Arc::new(guard);
        let slot = guard.slot();

        let poller = {
            let guard = Arc::clone(&guard);
            std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(10);
                while Instant::now() < deadline {
                    if matches!(guard.poll(|| child.try_wait()), Ok(Some(_))) {
                        return true;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                false
            })
        };

        let result = sweep(&[slot], CHILD_GRACE);
        assert!(poller.join().expect("poller"), "the child never exited");
        assert_eq!(result.stopped_when_asked, 1, "{}", result.describe());
        assert_eq!(result.force_killed, 0, "{}", result.describe());
        assert!(result.is_complete(), "{}", result.describe());
    }

    /// A child that ignores the ask is killed at the deadline, and reported as
    /// force-killed rather than as one that cooperated.
    #[cfg(unix)]
    #[test]
    fn the_sweep_force_kills_a_child_that_ignores_the_ask() {
        let mut cmd = stubborn();
        let (mut child, guard) = spawn(&mut cmd, "stubborn").expect("spawn");
        wait_for_ready(&mut child);

        let started = Instant::now();
        let result = sweep(&[guard.slot()], CHILD_GRACE);
        let elapsed = started.elapsed();

        assert_eq!(result.force_killed, 1, "{}", result.describe());
        assert_eq!(result.stopped_when_asked, 0, "{}", result.describe());
        assert!(result.is_complete(), "{}", result.describe());
        assert!(
            elapsed >= CHILD_GRACE,
            "the grace window was skipped: {elapsed:?}"
        );
        let status = reap_within(&mut child, Duration::from_secs(5));
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&status),
            Some(libc::SIGKILL),
            "a child that ignored SIGTERM was not force-killed"
        );
    }

    /// The honesty invariant: an entry the sweep could not reach must never be
    /// counted alongside the ones it did.
    #[cfg(unix)]
    #[test]
    fn an_entry_the_sweep_cannot_lock_is_reported_not_counted() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30").stdin(Stdio::null()).stdout(Stdio::null());
        let (mut child, guard) = spawn(&mut cmd, "held").expect("spawn");
        let slot = guard.slot();

        let (ready, held) = mpsc::channel();
        let (release, wait_release) = mpsc::channel::<()>();
        let holder = {
            let slot = Arc::clone(&slot);
            std::thread::spawn(move || {
                let _guard = lock(&slot.pid);
                ready.send(()).expect("signal ready");
                let _ = wait_release.recv();
            })
        };
        held.recv().expect("holder took the lock");

        let result = sweep(&[Arc::clone(&slot)], Duration::from_millis(10));
        assert_eq!(
            result.busy,
            vec!["held".to_string()],
            "{}",
            result.describe()
        );
        assert_eq!(
            result.stopped_when_asked + result.force_killed + result.already_gone,
            0,
            "an unswept child was counted as swept: {}",
            result.describe()
        );
        assert!(
            !result.is_complete(),
            "a sweep that skipped an entry reported itself complete"
        );
        assert!(
            result.describe().contains("NOT swept"),
            "the summary hid the entry it could not reach: {}",
            result.describe()
        );

        drop(release);
        holder.join().expect("holder");
        guard.kill_tree(&mut child);
        let _ = guard.reap(|| child.wait());
    }

    /// The mapping the first version of this module got wrong, and the reason
    /// every ordinary shutdown reported "kill failed".
    ///
    /// Between a child exiting and its owner calling `wait` its process group
    /// holds nothing signalable. macOS answers `EPERM` for that group and
    /// Linux answers success, so the two platforms cannot be made to agree on
    /// *what* they report — but neither may report a **failure**, because a
    /// corpse nobody has claimed yet is the most ordinary state there is.
    #[cfg(unix)]
    #[test]
    fn a_kill_aimed_at_an_unreaped_corpse_is_not_a_failure() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30").stdin(Stdio::null()).stdout(Stdio::null());
        let (mut child, guard) = spawn(&mut cmd, "corpse").expect("spawn");
        let pid = guard.pid().expect("registered");

        assert!(sys::force_kill(pid).expect("first kill"));
        // Deliberately not reaped: this is the window under test.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen = 0usize;
        while Instant::now() < deadline {
            sys::force_kill(pid).expect("a kill aimed at an unreaped corpse reported a failure");
            seen += 1;
            std::thread::sleep(Duration::from_millis(10));
            if seen > 10 {
                break;
            }
        }
        assert!(seen > 10, "the loop never ran");
        let status = reap_within(&mut child, Duration::from_secs(5));
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&status),
            Some(libc::SIGKILL)
        );
    }

    #[test]
    fn an_empty_sweep_reports_nothing_and_does_nothing() {
        let result = sweep(&[], CHILD_GRACE);
        assert_eq!(result, Sweep::default());
        assert!(result.is_complete());
    }

    #[test]
    fn a_failed_install_does_not_describe_itself_like_a_successful_one() {
        let armed = SignalGuard {
            installed: vec!["SIGTERM"],
            skipped: Vec::new(),
        };
        let unarmed = SignalGuard {
            installed: Vec::new(),
            skipped: vec!["no pipe".to_string()],
        };
        assert!(armed.is_armed());
        assert!(!unarmed.is_armed());
        assert_ne!(armed.describe(), unarmed.describe());
        assert!(
            unarmed
                .describe()
                .contains("no termination signal is handled"),
            "{}",
            unarmed.describe()
        );
        assert!(
            unarmed.describe().contains("no pipe"),
            "the reason was dropped: {}",
            unarmed.describe()
        );
    }

    /// Windows is the case where nothing can be armed. It must say so, not
    /// return the value a working install returns.
    #[cfg(windows)]
    #[test]
    fn windows_reports_that_it_armed_nothing() {
        let guard = install_signal_handlers();
        assert!(!guard.is_armed());
        assert!(guard
            .describe()
            .contains("no termination signal is handled"));
    }
}
