//! The `manvi serve` sidecar: one long-lived child process, NDJSON over stdio.
//!
//! Three properties are load-bearing here.
//!
//! * **The user's repository is never touched.** Every MANVI command first
//!   "prepares the repository it is standing in" — it creates a state
//!   directory and appends managed rules to `.gitignore`. A Git client that
//!   silently rewrote the `.gitignore` of every repository the user opened
//!   would be indefensible, so the child is spawned in a scratch directory of
//!   ours with `MANVI_HARNESS_INIT_ENABLED=false` and `MANVI_STATE_DIR`
//!   pointed away from the repository. The repository reaches the harness only
//!   as the `root` parameter of a policy call, which is read, never written.
//! * **A call that could not run is never reported as one that passed.** A
//!   missing binary, a dead child, a timeout and a refusal are four different
//!   outcomes and stay four different outcomes all the way to the UI.
//! * **Dispatch is serial**, because the harness's own dispatch is serial: it
//!   writes exactly one response line per request and handles them in order.
//!   Holding the lock across the round trip is what makes a stray line
//!   attributable to the request that provoked it.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use super::protocol::*;

/// How long a single request may take before the child is considered wedged.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(15);
/// Handshake budget. A cold `manvi serve` has to resolve its configuration
/// before it answers, so this is deliberately looser than a warm call.
const HELLO_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a failed start is remembered before another spawn is attempted.
/// Without it, a machine with no `manvi` installed would fork a process per
/// status poll.
const RESPAWN_BACKOFF: Duration = Duration::from_secs(30);
/// How often [`acquire_slot`] re-tests a slot another call is holding.
/// `std::sync::Mutex` has no timed acquire, so waiting is a bounded spin;
/// this is the granularity of that spin, not a budget.
const SLOT_POLL_INTERVAL: Duration = Duration::from_millis(20);
/// Lines of the child's stderr kept for diagnostics.
const STDERR_TAIL_LINES: usize = 40;
/// Grace period between closing the child's stdin (its documented clean
/// shutdown) and escalating to `kill` in [`Drop for Sidecar`]. A harness that
/// honors EOF exits in milliseconds; this only bounds the dishonorable case.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);
/// Refuse to send a line the harness would refuse anyway (its cap is 8 MiB).
/// Ours is lower on purpose: a request this large is a defect in our prompt
/// budgeting, and finding it here is cheaper than finding it as an E_TOO_LARGE.
const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
/// Upper bound on a single NDJSON frame read from the sidecar. Mirrors the
/// request budget: a legitimate reply never approaches this, so a longer
/// stream means the child is wedged or hostile and its frame is dropped.
const MAX_SIDECAR_FRAME_BYTES: usize = 4 * 1024 * 1024;
/// Upper bound on *everything* the sidecar may stream back during one
/// request cycle — answers, noise and foreign ids alike. A healthy reply is
/// one bounded frame; more than a handful of frames means the child is
/// flooding, and the cycle faults instead of reading forever within its
/// timeout window.
const MAX_RESPONSE_TOTAL_BYTES: usize = 32 * 1024 * 1024;

/// Why the harness is not answering, in terms a user can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessError {
    /// No `manvi` binary was found.
    NotInstalled(String),
    /// The binary exists but the sidecar could not be started or handshaken.
    Unavailable(String),
    /// The call went out and the child did not answer in time.
    Timeout(String),
    /// The harness answered with `ok:false`.
    Refused(WireError),
    /// The harness answered something this client could not decode.
    Protocol(String),
    /// Another gated action already holds the sidecar slot; this request was
    /// not queued behind it. Deliberately distinct from `Unavailable`: the
    /// harness may be perfectly healthy, just busy with one serial dispatch.
    Busy(String),
}

impl HarnessError {
    pub fn message(&self) -> String {
        match self {
            HarnessError::NotInstalled(m) => m.clone(),
            HarnessError::Unavailable(m) => m.clone(),
            HarnessError::Timeout(m) => m.clone(),
            HarnessError::Refused(e) => e.to_string(),
            HarnessError::Protocol(m) => m.clone(),
            HarnessError::Busy(m) => m.clone(),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            HarnessError::NotInstalled(_) => "not_installed",
            HarnessError::Unavailable(_) => "unavailable",
            HarnessError::Timeout(_) => "timeout",
            HarnessError::Refused(_) => "refused",
            HarnessError::Protocol(_) => "protocol",
            HarnessError::Busy(_) => "busy",
        }
    }
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

/// A running sidecar and the handshake it answered with.
struct Sidecar {
    /// Shared with the two pipe-reader threads so an oversized or failed read
    /// can kill the child *immediately* instead of waiting for the next call
    /// to notice the disconnected channel.
    child: std::sync::Arc<Mutex<Child>>,
    /// `None` once stdin has been closed for shutdown. An `Option` is what
    /// lets [`Drop`] hand the pipe back to the OS without moving out of
    /// `&mut self`.
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr: std::sync::Arc<Mutex<VecDeque<String>>>,
    hello: HelloResult,
    binary: String,
    next_id: u64,
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        let mut child = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        shutdown_child(&mut child, self.stdin.take(), SHUTDOWN_GRACE);
    }
}

/// Kills the child from a pipe-reader thread. A wedged or hostile child must
/// not stay alive merely because nobody happens to be mid-call when its
/// overflow is detected; the faulting reader reaps it here, and the
/// disconnected channel turns the next (or in-flight) call into a transport
/// fault that sets the respawn backoff.
fn force_kill(child: &Mutex<Child>) {
    let mut child = child
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ = child.kill();
    let _ = child.wait();
}

/// Clean-shutdown escalation ladder, factored out of [`Drop for Sidecar`]
/// because it is testable against arbitrary children.
///
/// 1. Close stdin. The harness's documented clean shutdown is EOF on its
///    request stream; an honorable child exits on its own.
/// 2. Wait up to `grace` for that exit.
/// 3. Kill whatever remains, so a wedged child cannot leak past its owner.
fn shutdown_child(child: &mut Child, stdin: Option<ChildStdin>, grace: Duration) {
    // Dropping the write end is what sends EOF; there is no explicit close.
    drop(stdin);
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(25));
            }
            // Still running at the deadline, or wait itself failed: either
            // way the escalation below reaps the process or reports why not.
            _ => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

impl Sidecar {
    fn call(
        &mut self,
        op: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, HarnessError> {
        self.next_id += 1;
        let id = self.next_id.to_string();
        let line = serde_json::to_string(&Request {
            id: id.clone(),
            op: op.to_string(),
            params,
        })
        .map_err(|e| HarnessError::Protocol(format!("could not encode {} request: {}", op, e)))?;

        if line.len() > MAX_REQUEST_BYTES {
            return Err(HarnessError::Protocol(format!(
                "{} request is {} bytes, past this client's {} byte cap",
                op,
                line.len(),
                MAX_REQUEST_BYTES
            )));
        }

        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| HarnessError::Unavailable("sidecar stdin closed".into()))?;
        stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.write_all(b"\n"))
            .and_then(|_| stdin.flush())
            .map_err(|e| HarnessError::Unavailable(format!("sidecar stdin closed: {}", e)))?;

        let deadline = Instant::now() + timeout;
        let mut response_bytes = 0usize;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(HarnessError::Timeout(format!(
                    "{} did not answer within {:?}{}",
                    op,
                    timeout,
                    self.stderr_hint()
                )));
            }
            match self.lines.recv_timeout(remaining) {
                Ok(raw) => {
                    // Every line the child emits during this cycle counts,
                    // including ignored noise: a flood of unattributable
                    // lines is just as much a wedged child as one oversized
                    // frame. Protocol faults drop the sidecar and set the
                    // respawn backoff, same as the other transport faults.
                    response_bytes += raw.len() + 1;
                    if response_bytes > MAX_RESPONSE_TOTAL_BYTES {
                        return Err(HarnessError::Protocol(format!(
                            "{} streamed {} bytes past this client's {} byte \
                             response budget{}",
                            op,
                            response_bytes,
                            MAX_RESPONSE_TOTAL_BYTES,
                            self.stderr_hint()
                        )));
                    }
                    match classify_line(&raw, &id) {
                        LineOutcome::Ignore => {
                            // Noise, a log line, or another call's answer: none of
                            // these fault a healthy sidecar, so keep waiting
                            // within the same deadline.
                            continue;
                        }
                        LineOutcome::Answer(response) => {
                            if !response.ok {
                                return Err(HarnessError::Refused(response.error.unwrap_or(
                                    WireError {
                                        code: "E_INTERNAL".into(),
                                        message: format!("{} failed without an error body", op),
                                        retryable: false,
                                    },
                                )));
                            }
                            return Ok(response.result.unwrap_or(Value::Null));
                        }
                        LineOutcome::Fault(message) => {
                            // Addressed to this very call yet undecodable: the
                            // protocol itself is broken and retrying the read
                            // cannot help.
                            return Err(HarnessError::Protocol(message));
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(HarnessError::Unavailable(format!(
                        "sidecar exited during {}{}",
                        op,
                        self.stderr_hint()
                    )))
                }
            }
        }
    }

    fn stderr_hint(&self) -> String {
        let tail = self
            .stderr
            .lock()
            .map(|t| t.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        match tail.last() {
            Some(last) if !last.is_empty() => {
                format!(" (last diagnostic: {})", truncate(last, 200))
            }
            _ => String::new(),
        }
    }
}

/// What one raw sidecar stdout line means for the call waiting on it.
#[derive(Debug)]
enum LineOutcome {
    /// Log chatter, an undecodable line, or a response to some other call:
    /// none of these implicate the protocol, so the caller keeps waiting
    /// within its deadline instead of faulting a healthy sidecar.
    Ignore,
    /// The answer to this call.
    Answer(Response),
    /// A line addressed to this very call yet unparseable: the protocol
    /// itself is broken and the sidecar must be dropped.
    Fault(String),
}

/// Pure decision core of the read loop, so the ignore-vs-fault policy is
/// unit-testable without spawning a child.
fn classify_line(raw: &str, expected_id: &str) -> LineOutcome {
    match serde_json::from_str::<Response>(raw) {
        Ok(response) => {
            if response.id == expected_id {
                LineOutcome::Answer(response)
            } else {
                // Serial dispatch means a foreign id is either a leftover from
                // an earlier call or harness noise; it is not ours to act on.
                LineOutcome::Ignore
            }
        }
        Err(e) => {
            // Only treat the line as a breach when it carries OUR id: that is
            // the harness answering us in a shape we cannot read. Anything
            // else (plain text, partial JSON, other ids) stays noise.
            let addressed_to_us = serde_json::from_str::<Value>(raw)
                .ok()
                .and_then(|v| v.get("id").and_then(Value::as_str).map(String::from))
                .is_some_and(|id| id == expected_id);
            if addressed_to_us {
                LineOutcome::Fault(format!(
                    "sidecar answered {} with an undecodable response ({}): {}",
                    expected_id,
                    e,
                    truncate(raw, 200)
                ))
            } else {
                LineOutcome::Ignore
            }
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;

/// Prepares an isolated, transient state directory for `manvi serve`.
///
/// MANVI creates SQLite and runtime state on disk. A user's home directory
/// is not ours to touch and is visible to any other process running as the
/// local user; this per-boot directory is process-unique, private
/// (`0o700` on Unix), and refuses to traverse a symlink at its own path.
/// Old directories are deliberately not cleaned up: best-effort deletion of
/// unknown-provenance paths is riskier than the few kilobytes they hold.
fn scratch_dir() -> Result<PathBuf, HarnessError> {
    let unique = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
            .hash(&mut hasher);
        format!("{}-{:016x}", std::process::id(), hasher.finish())
    };
    create_scratch_under(&std::env::temp_dir(), &unique)
        .map_err(|e| HarnessError::Unavailable(format!("could not prepare harness state dir: {e}")))
}

/// Creates `<root>/gitpulse-harness-<unique>` fail-closed: refuses an
/// existing symlink at the target path, creates the directory privately on
/// Unix, and never follows someone else's pre-planted entry. Factored with
/// the root as a parameter so the refusal and privacy behavior are testable
/// without touching the real temp directory.
fn create_scratch_under(root: &std::path::Path, unique: &str) -> Result<PathBuf, String> {
    let dir = root.join(format!("gitpulse-harness-{unique}"));

    // symlink_metadata never follows the final component, so a symlink
    // planted at `dir` is detected even though it may point somewhere
    // innocuous. Any other pre-existing entry also fails closed below via
    // create_dir's AlreadyExists error.
    if let Ok(meta) = std::fs::symlink_metadata(&dir) {
        if meta.file_type().is_symlink() {
            return Err(format!(
                "refusing {}: a symlink is planted at the harness state path",
                dir.display()
            ));
        }
    }

    #[cfg(unix)]
    {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| format!("could not create {}: {}", dir.display(), e))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir(&dir)
            .map_err(|e| format!("could not create {}: {}", dir.display(), e))?;
    }
    Ok(dir)
}

/// Resolves the `manvi` binary: explicit override, then the shared
/// PATH + GUI-fallback search (one canonical owner of those semantics, in
/// [`crate::engine::git_cli`]).
pub fn resolve_binary() -> Option<String> {
    if let Ok(explicit) = std::env::var("GITPULSE_MANVI_BIN") {
        let path = PathBuf::from(&explicit);
        return if path.is_file() { Some(explicit) } else { None };
    }
    let bin_name = if cfg!(windows) { "manvi.exe" } else { "manvi" };
    crate::engine::git_cli::find_external_tool(bin_name)
}

/// Reads one newline-terminated frame without ever buffering more than
/// `max` bytes: the buffer is fed chunk-by-chunk from `fill_buf`, so a
/// sidecar streaming gigabytes without a newline cannot balloon memory.
/// Semantics:
/// - `Ok(Some(line))` — a complete in-budget line (trailing newline stripped)
/// - `Ok(None)` — end of stream
/// - `Err` — a frame beyond `max` was seen, or the stream errored. An
///   oversized frame is a protocol fault, not a quiet EOF: the caller kills
///   the child and sets the respawn backoff rather than treating a hostile
///   stream as a clean exit. The remainder is drained only up to its newline.
fn read_bounded_line<R: BufRead>(reader: &mut R, max: usize) -> std::io::Result<Option<String>> {
    let mut out: Vec<u8> = Vec::with_capacity(4096);
    let mut overflowed = false;
    let mut saw_data = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        saw_data = true;
        match available.iter().position(|&b| b == b'\n') {
            Some(pos) => {
                let chunk = &available[..=pos];
                if !overflowed && out.len() + chunk.len() <= max {
                    out.extend_from_slice(chunk);
                } else {
                    overflowed = true;
                }
                reader.consume(pos + 1);
                break;
            }
            None => {
                if !overflowed && out.len() + available.len() > max {
                    overflowed = true;
                    out.clear();
                } else if !overflowed {
                    out.extend_from_slice(available);
                }
                let len = available.len();
                reader.consume(len);
            }
        }
    }
    if !saw_data && out.is_empty() {
        return Ok(None);
    }
    if overflowed {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("sidecar frame exceeds the {max} byte cap"),
        ));
    }
    if out.is_empty() {
        return Ok(None);
    }
    while out.last() == Some(&b'\n') {
        out.pop();
    }
    String::from_utf8(out)
        .map(Some)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "sidecar sent non-UTF8"))
}

fn spawn() -> Result<Sidecar, HarnessError> {
    let binary = resolve_binary().ok_or_else(|| {
        HarnessError::NotInstalled(
            "no `manvi` binary on PATH, in ~/.local/bin, or named by GITPULSE_MANVI_BIN".into(),
        )
    })?;

    let dir = scratch_dir()?;
    let mut child = Command::new(&binary)
        .args(["serve", "--posture", "host"])
        .current_dir(&dir)
        // Both of these keep the harness out of the working tree: the first
        // stops it preparing a repository at all, the second keeps its state
        // out of whatever directory it did start in.
        .env("MANVI_HARNESS_INIT_ENABLED", "false")
        .env("MANVI_STATE_DIR", dir.join("state"))
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            HarnessError::Unavailable(format!("could not start `{} serve`: {}", binary, e))
        })?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| HarnessError::Unavailable("sidecar has no stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| HarnessError::Unavailable("sidecar has no stdout".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| HarnessError::Unavailable("sidecar has no stderr".into()))?;

    let child = std::sync::Arc::new(Mutex::new(child));
    let child_for_stdout = child.clone();

    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            match read_bounded_line(&mut reader, MAX_SIDECAR_FRAME_BYTES) {
                Ok(Some(line)) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    if tx.send(line).is_err() {
                        return;
                    }
                }
                Ok(None) => return,
                Err(_) => {
                    force_kill(&child_for_stdout);
                    return;
                }
            }
        }
    });

    let tail = std::sync::Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));
    let tail_writer = tail.clone();
    let child_for_stderr = child.clone();
    std::thread::spawn(move || {
        // Bounded exactly like stdout: stderr is untrusted bytes from the
        // same child, and `.lines()` would buffer an unlimited newline-less
        // flood just to keep a diagnostic tail. A frame past the cap faults
        // the child instead of feeding it.
        let mut reader = BufReader::new(stderr);
        loop {
            match read_bounded_line(&mut reader, MAX_SIDECAR_FRAME_BYTES) {
                Ok(Some(line)) => {
                    if let Ok(mut buf) = tail_writer.lock() {
                        if buf.len() == STDERR_TAIL_LINES {
                            buf.pop_front();
                        }
                        buf.push_back(line);
                    }
                }
                Ok(None) => return,
                Err(_) => {
                    force_kill(&child_for_stderr);
                    return;
                }
            }
        }
    });

    let mut sidecar = Sidecar {
        child,
        stdin: Some(stdin),
        lines: rx,
        stderr: tail,
        hello: HelloResult::default(),
        binary: binary.clone(),
        next_id: 0,
    };

    let raw = sidecar.call(
        OP_HELLO,
        Some(serde_json::json!({"protocol": PROTOCOL_VERSION, "host": "gitpulse"})),
        HELLO_TIMEOUT,
    )?;
    let hello: HelloResult = serde_json::from_value(raw)
        .map_err(|e| HarnessError::Protocol(format!("could not decode hello: {}", e)))?;

    if hello.protocol != PROTOCOL_VERSION {
        return Err(HarnessError::Protocol(format!(
            "sidecar speaks protocol {} and this build speaks {}; refusing rather than \
             mis-reading a policy decision",
            hello.protocol, PROTOCOL_VERSION
        )));
    }
    sidecar.hello = hello;
    Ok(sidecar)
}

/// The process-wide sidecar, started on first use and restarted after a fault.
struct Slot {
    sidecar: Option<Sidecar>,
    last_error: Option<HarnessError>,
    backoff_until: Option<Instant>,
}

fn slot() -> &'static Mutex<Slot> {
    static SLOT: OnceLock<Mutex<Slot>> = OnceLock::new();
    SLOT.get_or_init(|| {
        Mutex::new(Slot {
            sidecar: None,
            last_error: None,
            backoff_until: None,
        })
    })
}

impl Slot {
    fn ensure(&mut self) -> Result<&mut Sidecar, HarnessError> {
        if self.sidecar.is_none() {
            if let Some(until) = self.backoff_until {
                if Instant::now() < until {
                    return Err(self.last_error.clone().unwrap_or_else(|| {
                        HarnessError::Unavailable("harness unavailable".into())
                    }));
                }
            }
            match spawn() {
                Ok(s) => {
                    self.sidecar = Some(s);
                    self.last_error = None;
                    self.backoff_until = None;
                }
                Err(e) => {
                    self.last_error = Some(e.clone());
                    self.backoff_until = Some(Instant::now() + RESPAWN_BACKOFF);
                    return Err(e);
                }
            }
        }
        Ok(self.sidecar.as_mut().expect("sidecar just ensured"))
    }
}

/// Waits for the process-wide slot until `deadline`, or gives up.
///
/// `std::sync::Mutex` offers no timed acquire, so this is a bounded spin
/// rather than a park. A poisoned slot is *recovered* rather than propagated —
/// the guard protects only this process's bookkeeping, and the data it guards
/// stays valid enough to retry from (a wedged child is dropped on its next
/// fault).
///
/// `None` means the slot was held for the entire budget. That is a real fault
/// worth reporting; merely being second in line is not.
fn acquire_slot(deadline: Instant) -> Option<std::sync::MutexGuard<'static, Slot>> {
    use std::sync::TryLockError;
    loop {
        match slot().try_lock() {
            Ok(guard) => return Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => return Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return None;
                }
                thread::sleep(remaining.min(SLOT_POLL_INTERVAL));
            }
        }
    }
}

/// Issues one operation, starting the sidecar if it is not running.
///
/// A transport fault drops the child so the next call starts a fresh one; a
/// refusal by the harness leaves it running, because the harness is fine and
/// the request was not.
///
/// The slot lock is held across the whole round trip (serial dispatch is
/// what makes a stray line attributable). Acquisition retries until `timeout`
/// then fails with [`HarnessError::Busy`] so overlapping agent actions wait
/// out a short in-flight call instead of failing on the first `try_lock`,
/// without queueing behind a wedged request when the caller used a shorter
/// budget.
pub fn call(op: &str, params: Option<Value>, timeout: Duration) -> Result<Value, HarnessError> {
    let mut guard = acquire_slot(Instant::now() + timeout).ok_or_else(|| {
        HarnessError::Busy(format!(
            "another gated action held the harness for the whole {timeout:?} budget; \
             '{op}' was not sent. Retry once the current commit/push/pull settles."
        ))
    })?;
    let sidecar = guard.ensure()?;
    match sidecar.call(op, params, timeout) {
        Ok(v) => Ok(v),
        Err(e) => {
            if matches!(
                e,
                HarnessError::Timeout(_) | HarnessError::Unavailable(_) | HarnessError::Protocol(_)
            ) {
                guard.sidecar = None;
                guard.last_error = Some(e.clone());
                guard.backoff_until = Some(Instant::now() + RESPAWN_BACKOFF);
            }
            Err(e)
        }
    }
}

/// Types a call's result, so every caller does not repeat the decode.
pub fn call_typed<T: serde::de::DeserializeOwned>(
    op: &str,
    params: impl Serialize,
    timeout: Duration,
) -> Result<T, HarnessError> {
    let params = serde_json::to_value(params)
        .map_err(|e| HarnessError::Protocol(format!("could not encode {} params: {}", op, e)))?;
    let raw = call(op, Some(params), timeout)?;
    serde_json::from_value(raw)
        .map_err(|e| HarnessError::Protocol(format!("could not decode {} result: {}", op, e)))
}

/// What the sidecar answered at handshake, without provoking a spawn beyond
/// the first.
pub fn handshake() -> Result<(String, HelloResult), HarnessError> {
    // Same reasoning as `call`: a single `try_lock` turned a harness that is
    // merely busy into `HarnessStatus { available: false }`, which reads as
    // "no harness installed" everywhere it is consumed — including the guard
    // that decides whether the gate tests run at all.
    let mut guard = acquire_slot(Instant::now() + Duration::from_millis(500)).ok_or_else(|| {
        HarnessError::Busy("harness is busy with an in-flight operation".to_string())
    })?;
    let sidecar = guard.ensure()?;
    Ok((sidecar.binary.clone(), sidecar.hello.clone()))
}

/// Forgets the backoff so the next call retries immediately. The UI's
/// "reconnect" affordance.
///
/// A poisoned slot is recovered deterministically: the fields being cleared
/// are plain bookkeeping, and leaving them stale behind a poisoned lock would
/// keep the 30s respawn backoff alive with no way to reset it.
pub fn reset() {
    let mut guard = slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.sidecar = None;
    guard.last_error = None;
    guard.backoff_until = None;
}

/// True when this build knows how to ask for `op` and the running sidecar
/// serves it.
pub fn serves(hello: &HelloResult, op: &str) -> bool {
    hello.ops.iter().any(|o| o == op)
}

/// Kills any live sidecar child, for application exit.
///
/// The slot lives in a static, and statics never drop — without this, a live
/// `manvi serve` would be orphaned when the UI process quits. Dropping the
/// slot's sidecar runs the full clean-shutdown escalation ladder (stdin EOF,
/// grace wait, kill). Idempotent by construction (second call sees `None`)
/// and safe to fire on both `RunEvent::ExitRequested` and `RunEvent::Exit`,
/// whichever the platform delivers: a later harness call simply respawns.
///
/// Lock safety: the slot lock is held across a whole round trip by an
/// in-flight call, so this retries `try_lock` for ~1.2s and then gives up
/// rather than stalling quit behind a 15s wedged request. Giving up is
/// best-effort by design: once the process exits, the child's stdin closes
/// and its documented EOF shutdown applies.
pub(crate) fn shutdown() {
    let deadline = Instant::now() + Duration::from_millis(1200);
    loop {
        match slot().try_lock() {
            Ok(mut guard) => {
                guard.sidecar = None;
                return;
            }
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                poisoned.into_inner().sidecar = None;
                return;
            }
            // Another caller is mid-round-trip; wait briefly, then let exit
            // proceed. A bounded give-up is deliberate here.
            Err(std::sync::TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    return;
                }
                thread::sleep(Duration::from_millis(25));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: an undecodable stdout line used to return
    /// HarnessError::Protocol, which drops a healthy sidecar and starts the
    /// 30s respawn backoff. Noise must be ignored instead; only a line
    /// addressed to this call that cannot decode is a protocol fault.
    #[test]
    fn undecodable_noise_is_ignored_not_faulted() {
        for noise in [
            "",
            "   ",
            "manvi: starting up",
            "[2026-08-25] notice",
            "{not json at all",
            r#"{"id": "999", "broken"#,
            // A valid response belonging to some other call.
            r#"{"id":"other","ok":true,"result":{"x":1}}"#,
        ] {
            assert!(
                matches!(classify_line(noise, "42"), LineOutcome::Ignore),
                "line {noise:?} must not fault the sidecar"
            );
        }
    }

    #[test]
    fn matching_id_yields_the_answer() {
        let line = r#"{"id":"7","ok":true,"result":{"allowed":true}}"#;
        match classify_line(line, "7") {
            LineOutcome::Answer(r) => {
                assert!(r.ok);
                assert_eq!(
                    r.result.unwrap_or(Value::Null)["allowed"],
                    Value::Bool(true)
                );
            }
            other => panic!("expected Answer, got {other:?}"),
        }
    }

    #[test]
    fn refused_answer_is_delivered_not_ignored() {
        let line = r#"{"id":"7","ok":false,"error":{"code":"E_BLOCKED","message":"no","retryable":false}}"#;
        match classify_line(line, "7") {
            LineOutcome::Answer(r) => {
                assert!(!r.ok);
                assert_eq!(r.error.unwrap().code, "E_BLOCKED");
            }
            other => panic!("expected Answer, got {other:?}"),
        }
    }

    #[test]
    fn malformed_response_with_our_id_is_a_fault() {
        // Carries our id, so it claims to answer us — but it lacks the
        // mandatory `ok` field. Retrying cannot help; this faults.
        let outcome = classify_line(r#"{"id":"9","result":null}"#, "9");
        assert!(
            matches!(outcome, LineOutcome::Fault(_)),
            "expected Fault, got {outcome:?}"
        );
    }

    /// Regression (slot-lock hardening, corrected): while one gated action
    /// holds the slot, a second caller must neither queue forever nor give up
    /// instantly. It waits out its own timeout, then reports `Busy`.
    ///
    /// This test previously asserted a *single* `try_lock` — fail fast,
    /// always. That expectation was wrong, and wrong in the dangerous
    /// direction. The slot is held across a whole round trip, so two
    /// overlapping gated actions — the ordinary case, since the UI polls
    /// harness status while a commit or push is being judged — produced a
    /// `Busy`, and `check_command` renders any error as an *unchecked*
    /// verdict, which does not block. `git push --force` went through the gate
    /// that exists to refuse it, recorded only as "unguarded". Fast-fail
    /// bought UI latency that no caller wanted: every one of them already runs
    /// off the UI thread.
    ///
    /// Both halves live in one test because they share the process-wide slot;
    /// as separate `#[test]`s cargo's threads would race them against
    /// each other.
    #[test]
    fn a_waiter_sits_out_the_holder_but_still_gives_up_bounded() {
        // Held for the caller's whole budget: `Busy` is honest, but only
        // after the wait. Going through `super::call` keeps the real seam —
        // the one that renders an unchecked verdict — under test.
        let holder = slot().lock().expect("acquire slot to simulate a call");
        let started = Instant::now();
        let err = super::call("policy.check", None, Duration::from_millis(200))
            .expect_err("slot is held for the whole budget");
        let waited = started.elapsed();
        drop(holder);
        assert_eq!(err.code(), "busy");
        assert!(matches!(err, HarnessError::Busy(ref m) if m.contains("was not sent")));
        assert!(
            waited >= Duration::from_millis(150),
            "gave up after {waited:?} without waiting out its 200ms budget"
        );
        assert!(
            waited < Duration::from_secs(2),
            "waited {waited:?}; the caller queued instead of giving up"
        );

        // Released within the budget: the waiter gets the slot. This is the
        // case that used to fault instantly and un-gate the action.
        let held_for = Duration::from_millis(300);
        let (tx, rx) = mpsc::channel::<()>();
        let holder = thread::spawn(move || {
            let _guard = slot()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            tx.send(()).expect("signal that the slot is held");
            thread::sleep(held_for);
        });
        rx.recv().expect("holder took the slot");

        let started = Instant::now();
        let acquired = acquire_slot(Instant::now() + Duration::from_secs(5));
        assert!(
            acquired.is_some(),
            "a waiter within its budget must get the slot, not a Busy fault"
        );
        drop(acquired);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the waiter should wake as soon as the holder releases"
        );
        holder.join().expect("holder thread");
    }

    /// A child that honors stdin EOF exits on its own inside the grace
    /// window; no kill is needed and the exit status is its own.
    #[test]
    fn shutdown_closes_stdin_and_lets_an_honoring_child_exit_cleanly() {
        let mut child = Command::new("sh")
            .args(["-c", "cat > /dev/null; echo done >&2"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn honoring child");
        let stdin = child.stdin.take().expect("piped stdin");

        let started = Instant::now();
        shutdown_child(&mut child, Some(stdin), Duration::from_millis(500));
        let status = child.wait().expect("reap");
        assert!(
            started.elapsed() < Duration::from_millis(2_000),
            "clean exit should not wait out the full grace"
        );
        assert!(status.success(), "child exited on EOF, not killed");
    }

    /// A child that ignores EOF is killed at the grace deadline; the total
    /// shutdown stays bounded by roughly the grace period.
    #[test]
    fn shutdown_escalates_to_kill_when_child_ignores_eof() {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn stubborn child");
        let stdin = child.stdin.take().expect("piped stdin");

        let started = Instant::now();
        shutdown_child(&mut child, Some(stdin), Duration::from_millis(200));
        let elapsed = started.elapsed();
        let status = child.wait().expect("reap after kill");
        assert!(
            elapsed >= Duration::from_millis(180),
            "escalation fired early: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(3_000),
            "shutdown exceeded grace materially: {elapsed:?}"
        );
        assert!(!status.success(), "a SIGKILLed sleep cannot report success");
    }
}

#[cfg(test)]
mod bounded_reader_tests {
    use super::read_bounded_line;
    use std::io::{Cursor, Read, Result};

    /// Wraps a cursor to yield one byte per fill_buf so chunk-boundary
    /// handling is exercised on every possible split point.
    struct OneByteAtATime<'a>(&'a [u8]);
    impl<'a> Read for OneByteAtATime<'a> {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
            if self.0.is_empty() || buf.is_empty() {
                return Ok(0);
            }
            buf[0] = self.0[0];
            self.0 = &self.0[1..];
            Ok(1)
        }
    }
    impl<'a> std::io::BufRead for OneByteAtATime<'a> {
        fn fill_buf(&mut self) -> Result<&[u8]> {
            if self.0.is_empty() {
                Ok(&[])
            } else {
                Ok(&self.0[..1])
            }
        }
        fn consume(&mut self, n: usize) {
            self.0 = &self.0[n.min(self.0.len())..];
        }
    }

    fn lines_of(data: &[u8], max: usize) -> Vec<String> {
        let mut cur = Cursor::new(data.to_vec());
        let mut out = Vec::new();
        while let Ok(Some(l)) = read_bounded_line(&mut cur, max) {
            out.push(l);
        }
        out
    }

    #[test]
    fn reads_simple_framed_lines() {
        assert_eq!(lines_of(b"a\nbb\nccc\n", 64), vec!["a", "bb", "ccc"]);
    }

    #[test]
    fn handles_missing_final_newline_and_blank_lines() {
        assert_eq!(lines_of(b"x\n\ny", 64), vec!["x", "", "y"]);
        assert_eq!(lines_of(b"", 64), Vec::<String>::new());
    }

    fn first_error(data: &[u8], max: usize) -> std::io::Error {
        let mut cur = Cursor::new(data.to_vec());
        loop {
            match read_bounded_line(&mut cur, max) {
                Ok(Some(_)) => continue,
                Ok(None) => panic!("expected a fault before end of stream"),
                Err(e) => return e,
            }
        }
    }

    /// Regression (audit F6): an oversized frame used to be swallowed as a
    /// quiet EOF, indistinguishable from a clean child exit. It must be an
    /// explicit fault naming the cap — and the 10 MiB flood must never be
    /// buffered whole against a 1 KiB cap.
    #[test]
    fn oversized_frame_is_a_fault_that_names_the_cap() {
        let mut flood = vec![b'x'; 10 * 1024 * 1024];
        flood.push(b'\n');
        let err = first_error(&flood, 1024);
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        let msg = err.to_string();
        assert!(msg.contains("cap") && msg.contains("1024"), "got: {msg}");
    }

    #[test]
    fn frames_at_exact_cap_pass_one_over_fails() {
        let exact = format!("{}\n", "e".repeat(16));
        assert_eq!(lines_of(exact.as_bytes(), 17), vec!["e".repeat(16)]);
        let over = format!("{}\n", "o".repeat(17));
        let err = first_error(over.as_bytes(), 17);
        assert!(err.to_string().contains("cap"), "got: {err}");
    }

    #[test]
    fn byte_split_boundaries_match_cursor_results() {
        let data: Vec<u8> = b"alpha\nbe\ta\nlonger line here\ntail".to_vec();
        let fast = lines_of(&data, 1024);
        let slow = {
            let mut r = OneByteAtATime(&data);
            let mut out = Vec::new();
            while let Ok(Some(l)) = read_bounded_line(&mut r, 1024) {
                out.push(l);
            }
            out
        };
        assert_eq!(fast, slow);
    }
}

#[cfg(test)]
mod scratch_dir_tests {
    use super::create_scratch_under;
    use std::path::PathBuf;

    /// An isolated root so tests never race the real temp dir or each other.
    fn root(tag: &str) -> PathBuf {
        let r = std::env::temp_dir().join(format!(
            "gitpulse-scratch-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&r);
        std::fs::create_dir_all(&r).expect("test root");
        r
    }

    #[test]
    fn creates_distinct_dirs_with_private_mode() {
        let root = root("unique");
        let a = create_scratch_under(&root, "alpha").expect("first");
        let b = create_scratch_under(&root, "beta").expect("second");
        assert_ne!(a, b, "per-boot names must not collide");
        assert!(a.is_dir());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&a)
                .expect("scratch dir exists")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o700,
                "scratch dir must be 0700, got {mode:o}"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_pre_planted_symlink_at_the_target_path() {
        let root = root("symlink");
        let planted = root.join("gitpulse-harness-planted");
        std::os::unix::fs::symlink("/etc", &planted).expect("plant symlink");
        let err = create_scratch_under(&root, "planted").expect_err("symlink must be refused");
        assert!(
            err.contains("refusing") && err.contains("symlink"),
            "refusal must be explicit, got: {err}"
        );
        // Fail closed also means fail clean: the planted entry is untouched.
        assert!(std::fs::symlink_metadata(&planted)
            .expect("entry survives")
            .file_type()
            .is_symlink());
        let _ = std::fs::remove_file(&planted);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fails_closed_when_target_already_exists() {
        let root = root("exists");
        create_scratch_under(&root, "dup").expect("first creation");
        assert!(
            create_scratch_under(&root, "dup").is_err(),
            "an existing entry must never be adopted"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
