//! Bounded subprocess runner with deadlines, soft-stop grace, QoS tiering,
//! and captured output bounds.

use super::env::{
    build_capture_command, build_capture_command_in, default_child_path,
    restrict_env_to_allowlist_plus, PathSource,
};
use super::gate::{
    current_admission, is_deferred_under_load, is_slot_wait_timeout, refusal_message_at,
    spawn_gate, strip_code_prefix, with_admission, with_exec_probe, Admission, Refusal, SpawnGate,
    TIMEOUT_MARKER,
};
#[cfg(test)]
use super::gate::{refusal_message, spawn_log};
#[cfg(unix)]
use super::pipe_drain;
#[cfg(all(test, target_os = "macos"))]
use super::qos;
use super::qos::QosTier;
#[cfg(not(unix))]
use super::thread_io;
use crate::ipc_error::CommandError;
use std::cell::Cell;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
#[cfg(not(unix))]
use std::thread;
use std::time::{Duration, Instant};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(90);
/// Ceiling for network-bound operations (`clone`, `fetch`, `push`, remote
/// `ls-remote`) where multi-gigabyte transfers are legitimate. Local plumbing
/// keeps [`DEFAULT_TIMEOUT`].
pub const NETWORK_TIMEOUT: Duration = Duration::from_secs(30 * 60);
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
/// Stream budget a run may declare when its observer drains stdout
/// ([`ProcessObserver::drains_stdout`]): the bytes are dropped as they are
/// seen, so [`MAX_OUTPUT_BYTES`]' reason — what a buffered read costs to
/// hold — does not apply, but the stream is still bounded, and every byte
/// still counts against the declared cap.
pub const MAX_DRAINED_OUTPUT_BYTES: usize = 1024 * 1024 * 1024;

/// Shared grace window for pipe EOF after child exit. Unix closes unfinished
/// descriptors; Windows cancels workers and retains their resource slots until
/// they exit. This cleanup grace is separate from the command deadline.
pub(super) const DRAIN_JOIN_GRACE: Duration = Duration::from_secs(2);

/// Default pipe EOF drain grace after normal child exit.
pub const DEFAULT_DRAIN_GRACE: Duration = DRAIN_JOIN_GRACE;

/// Default soft-stop grace before SIGKILL when reaping a child.
pub const DEFAULT_KILL_GRACE: Duration = crate::procguard::CHILD_GRACE;

/// The text of captured output, decoded lossily: the one owner of that
/// decode for buffers the caller owns. Valid UTF-8, which is nearly every
/// git output, becomes the `String` over the SAME allocation; only a buffer
/// with invalid bytes is copied, once, into its lossy form.
/// `String::from_utf8_lossy(&bytes).into_owned()` copies a buffer it just
/// proved valid, which for a 100k-row graph window or a large diff is one
/// extra memcpy of up to the whole stream.
pub(crate) fn text_from_bytes(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|invalid| String::from_utf8_lossy(invalid.as_bytes()).into_owned())
}

/// Captured stdout/stderr from a bounded external process.
///
/// Unlike `run_command`, a non-zero exit is not an error: tools such as
/// `npm audit` and `npm outdated` use the status code to mean "findings",
/// and the JSON the caller needs is still on stdout.
#[derive(Debug, Clone)]
pub struct CapturedOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub success: bool,
    pub status_code: i32,
}

impl CapturedOutput {
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_string()
    }
}

/// Runs an external command with the same timeout and output caps as `git`.
pub fn run_command(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<Vec<u8>, CommandError> {
    run_command_in(program, args, timeout, None)
}

/// Like [`run_command`], with an optional working directory (used for `gh` in a repo).
pub fn run_command_in(
    program: &str,
    args: &[&str],
    timeout: Duration,
    cwd: Option<&Path>,
) -> Result<Vec<u8>, CommandError> {
    let output = capture_command(program, args, cwd, timeout, &[])?;
    if output.success {
        return Ok(output.stdout);
    }
    let err = output.stderr_text();
    if err.is_empty() {
        return Err(format!("{} failed with status {}", program, output.status_code).into());
    }
    Err(err.into())
}

/// Runs `program` with a hard timeout and bounded pipes.
///
/// `cwd` is optional. When set, it must already be a directory the caller has
/// judged (a validated repo, or a path `sandbox_join` produced). This helper
/// does not re-validate the path. Non-zero exits are returned, not raised.
pub fn capture_command(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
    extra_env: &[(&str, &str)],
) -> Result<CapturedOutput, CommandError> {
    let path_var = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    capture_command_with_env(
        program,
        args,
        cwd,
        timeout,
        extra_env,
        path_var.as_deref(),
        home.as_deref(),
        PathSource::Inherited,
    )
}

/// [`capture_command_with_env`] for a tool that reads repository-controlled
/// config (npm, bun): the child gets an allowlisted environment instead of the
/// process's, so `NPM_TOKEN`, `GITHUB_TOKEN`, `AWS_*` and the like cannot be
/// expanded into a request. Git variables are still pinned as for any capture.
#[allow(clippy::too_many_arguments)]
pub(crate) fn capture_command_scrubbed(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
    extra_env: &[(&str, &str)],
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
    source: PathSource,
    also_allowed: &[String],
) -> Result<CapturedOutput, CommandError> {
    let mut cmd = build_capture_command_in(program, args, cwd, extra_env, path_var, home, source);
    restrict_env_to_allowlist_plus(&mut cmd, std::env::vars_os(), home, also_allowed);
    let out = run_bounded(cmd, program, timeout, None)?.require_complete(program)?;
    Ok(CapturedOutput {
        stdout: out.stdout,
        stderr: out.stderr,
        success: out.success,
        status_code: out.status_code,
    })
}

/// [`capture_command`] with injectable `PATH`/home lookups — the seam its
/// callers fill from the process environment by default. Tests use it to run
/// the exact production spawn path under a simulated GUI-minimal environment
/// without mutating process-global state. `source` says which of those two
/// `path_var` is; see [`PathSource`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn capture_command_with_env(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
    extra_env: &[(&str, &str)],
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
    source: PathSource,
) -> Result<CapturedOutput, CommandError> {
    let cmd = build_capture_command_in(program, args, cwd, extra_env, path_var, home, source);

    let out = run_bounded(cmd, program, timeout, None)?.require_complete(program)?;
    Ok(CapturedOutput {
        stdout: out.stdout,
        stderr: out.stderr,
        success: out.success,
        status_code: out.status_code,
    })
}

/// Tail of one byte stream as display text, cut on a char boundary so a
/// multibyte character split at the cap does not render as replacement
/// garbage. Shared by every surface that shows capped tool output.
pub fn byte_tail(bytes: &[u8], cap: usize) -> String {
    let start = bytes.len().saturating_sub(cap);
    let tail = &bytes[start..];
    let boundary = tail
        .iter()
        .position(|b| (*b & 0xC0) != 0x80)
        .unwrap_or(tail.len());
    String::from_utf8_lossy(&tail[boundary..]).into_owned()
}

/// A finished run (any exit code) with capped per-stream tails.
#[derive(Debug)]
pub struct CapturedRun {
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub success: bool,
    pub status_code: i32,
    /// True when the tails are a prefix, for any of the reasons below.
    pub truncated: bool,
    /// Which reason, in one clause. `None` when nothing was cut. Carried
    /// beside the flag because "we stopped reading at the display budget" and
    /// "we never finished reading" are different facts, and a surface that
    /// renders one sentence for both asserts a cause it cannot know.
    pub truncation_reason: Option<String>,
}

/// Outcome of [`run_captured`]. A timeout is an outcome, not an error: the
/// terminal surfaces "killed at N s" next to normal exits, and collapsing it
/// into a spawn-style error string would make the two indistinguishable.
#[derive(Debug)]
pub enum RunOutcome {
    Finished(CapturedRun),
    TimedOut(Duration),
}

/// Like [`capture_command`], but truncation keeps capped tails instead of
/// erroring away everything, and the timeout is reported as an outcome.
///
/// `tail_cap` bounds each returned tail in bytes; pass [`MAX_OUTPUT_BYTES`]
/// to keep everything up to the drain cap. Built for callers that show raw
/// tool output to a user (the terminal); `capture_command` stays the right
/// shape for JSON-scraping scanners.
pub fn run_captured(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
    extra_env: &[(&str, &str)],
    tail_cap: usize,
) -> Result<RunOutcome, CommandError> {
    let path_var = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let cmd = build_capture_command(
        program,
        args,
        cwd,
        extra_env,
        path_var.as_deref(),
        home.as_deref(),
    );
    // Spawn/wait failures stay errors. The timeout is detected from the error
    // `run_bounded` formats for exactly that case (see TIMEOUT_MARKER); the
    // regression test below pins this contract so a rewording cannot silently
    // turn timeouts into spawn errors again.
    match run_bounded(cmd, program, timeout, None) {
        Ok(out) => {
            // The drain's own reason wins: it describes the whole stream. The
            // display cap only says this VIEW is a tail, which is a weaker and
            // separate claim.
            let cut_for_display = out.stdout.len() > tail_cap || out.stderr.len() > tail_cap;
            let truncation_reason =
                out.incomplete
                    .as_ref()
                    .map(Incomplete::describe)
                    .or_else(|| {
                        cut_for_display.then(|| format!("only the last {tail_cap} bytes are shown"))
                    });
            Ok(RunOutcome::Finished(CapturedRun {
                stdout_tail: byte_tail(&out.stdout, tail_cap),
                stderr_tail: byte_tail(&out.stderr, tail_cap),
                success: out.success,
                status_code: out.status_code,
                truncated: truncation_reason.is_some(),
                truncation_reason,
            }))
        }
        Err(e) if is_own_run_timeout(program, e.message()) => Ok(RunOutcome::TimedOut(timeout)),
        Err(e) => Err(e),
    }
}

/// The phrase a spawn error carries when the OS found nothing to run — the
/// only failure that answers "is this tool installed?" with no. Every other
/// spawn failure (too many open files, an argument list too long, a busy
/// fork, a refused trust check) says the system could not start a program,
/// which is not evidence the program is missing.
///
/// With a working directory set, a missing directory reports the same OS
/// error, so a presence probe passes none.
const SPAWN_NOT_FOUND_MARKER: &str = ": no such program: ";

/// The error text for a failed `spawn`, formatted in one place so
/// [`is_spawn_not_found`] can only match what this wrote.
pub(super) fn spawn_error(label: &str, error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::NotFound {
        format!("Failed to spawn {label}{SPAWN_NOT_FOUND_MARKER}{error}")
    } else {
        format!("Failed to spawn {label}: {error}")
    }
}

/// True when `err` is a spawn the OS refused because nothing exists at that
/// program name.
pub fn is_spawn_not_found(err: &str) -> bool {
    err.starts_with("Failed to spawn ") && err.contains(SPAWN_NOT_FOUND_MARKER)
}

/// What a `tool --version` run says about the tool.
///
/// Four answers, because "no" has to be earned. Only the OS failing to find
/// the program is [`ToolProbe::NotFound`]; a run that started and went wrong
/// is [`ToolProbe::FoundButFailed`]; a run that never started — deferred
/// under load, no process slot in time, a spawn the system refused — is
/// [`ToolProbe::NotRun`], which says nothing about whether the tool exists.
/// Reporting either of the last two as "not installed" names a cause nobody
/// observed: GitPulse told a user with `gh` on PATH to install it, because
/// its probe had been shed while the machine was busy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolProbe {
    /// Exited 0 and printed a non-empty first stdout line.
    Present(String),
    /// The OS found nothing at that name; carries the spawn error text.
    NotFound(String),
    /// Found and started, but no version came back: non-zero exit (with its
    /// stderr), a timeout after start, empty output. Carries the diagnosis.
    FoundButFailed(String),
    /// Never started, for a reason other than absence. Carries the reason.
    NotRun(String),
}

impl ToolProbe {
    pub(crate) fn version(&self) -> Option<&str> {
        match self {
            ToolProbe::Present(version) => Some(version),
            _ => None,
        }
    }
}

/// Classifies one probe run. `result` is what [`capture_command`] (or a
/// seam over it) returned for `program`.
pub(crate) fn classify_tool_probe(
    program: &str,
    result: Result<CapturedOutput, CommandError>,
) -> ToolProbe {
    // A probe's answer is a status the caller shows, not a command's error, so
    // the text is all it keeps.
    match result.map_err(CommandError::into_message) {
        Ok(out) => {
            let stdout = out.stdout_text();
            let line = stdout.lines().next().unwrap_or("").trim();
            if out.success && !line.is_empty() {
                ToolProbe::Present(line.to_string())
            } else {
                ToolProbe::FoundButFailed(found_but_failed_detail(program, &out))
            }
        }
        Err(error) if is_spawn_not_found(&error) => ToolProbe::NotFound(error),
        Err(error)
            if is_deferred_under_load(&error)
                || is_slot_wait_timeout(&error)
                || error.starts_with("Failed to spawn ")
                || error.contains("cancelled before spawn") =>
        {
            ToolProbe::NotRun(error)
        }
        // A timeout after start, an output cap, a failed wait: the program
        // was there and the run went wrong.
        Err(error) => ToolProbe::FoundButFailed(error),
    }
}

/// Diagnosis for a finished run that yielded no usable version line.
///
/// Success is checked before stdout so a non-zero exit can never be promoted
/// to "present" by incidental output; a mute stderr falls back to naming the
/// exit status rather than leaving an unexplained failure.
pub fn found_but_failed_detail(program: &str, out: &CapturedOutput) -> String {
    if out.success {
        return format!("{program} produced no version output");
    }
    let err = out.stderr_text();
    if err.is_empty() {
        format!("{program} exited {} without a diagnosis", out.status_code)
    } else {
        err
    }
}

/// Whether `error` is a cancellation (a read whose callers have gone, or any
/// spawn cancelled while it queued) and so no fault of the process: it is not
/// counted as a process failure, which feeds health reporting.
///
/// Decided by the kind its producer gave it, never by its words: git's stderr
/// becomes error text, and a line of it that matched a marker must not pass for
/// a cancel. The kind survives a layer that wraps the message.
pub(super) fn is_cancellation(error: &CommandError) -> bool {
    super::read_cancel::is_cancelled_error(error)
}

/// First sleep between `try_wait` polls, and the ceiling it grows to.
///
/// A fixed 15 ms sleep quantized EVERY git invocation upward by up to a full
/// interval: a `git rev-parse` that exits in 2 ms was not observed for another
/// 13, and GitPulse spends most of its subprocess budget on exactly these
/// short reads — `git status` measures ~20 ms on a mid-size repository and
/// `for-each-ref` ~10 ms, so the sleep was 75-150% overhead on the common
/// case. It compounds through sequential chains: `list_branches` alone spawns
/// four processes before it returns a row.
///
/// Starting fine and backing off keeps the fast case fast without spinning on
/// a slow one. A `git clone` that runs for a minute reaches the ceiling after
/// ~13 polls and then behaves exactly as before; the polls spent getting there
/// cost well under a millisecond of CPU in total.
pub(super) const POLL_BACKOFF_START: Duration = Duration::from_micros(250);
pub(super) const POLL_BACKOFF_MAX: Duration = Duration::from_millis(15);

/// Next backoff step: double, capped. Pure so the schedule is testable without
/// spawning a process.
pub(super) fn next_poll_backoff(current: Duration) -> Duration {
    current.saturating_mul(2).min(POLL_BACKOFF_MAX)
}

/// True when `err` says a command ended because its own timeout elapsed,
/// whether it ran out of time while running or while waiting for a process
/// slot. A load deferral never carries the marker, so it is not one.
pub(crate) fn is_timed_out(err: &str) -> bool {
    err.contains(TIMEOUT_MARKER)
}

/// True when `err` is exactly the deadline text `run_bounded` writes for the
/// child `run_captured` ran as `program`: `<program> timed out after <n>s`,
/// where `<n>` is the seconds as a plain decimal and nothing follows.
///
/// The label is the program in whatever characters its path holds, so it is
/// matched by prefix, not by shape. Everything else that carries the marker
/// is refused by construction, because none of it ends in digits and `s`: a
/// slot-wait refusal (`... waiting for a process slot`), a deferral, the
/// up-front rejection of a zero deadline (`...0s: deadline must be positive
/// ...`), a cancellation and a spawn failure. A child that never ran is not a
/// run that timed out.
pub(super) fn is_own_run_timeout(program: &str, err: &str) -> bool {
    let Some(seconds) = err
        .strip_prefix(program)
        .and_then(|rest| rest.strip_prefix(TIMEOUT_MARKER))
        .and_then(|rest| rest.strip_suffix('s'))
    else {
        return false;
    };
    seconds.bytes().any(|b| b.is_ascii_digit())
        && seconds.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && seconds.bytes().filter(|&b| b == b'.').count() <= 1
}

/// What a runner failure message says happened, read once and in one place.
///
/// The runner still hands callers the `String` it always did (hooks, MCP and
/// tests match on its text, so the text is unchanged); this is the typed view
/// every classifier outside this module reads instead of matching substrings of
/// its own. Order of precedence is the order of the variants that can overlap:
/// a deferral and a slot-wait refusal both read like a timeout to a bare
/// substring match, and neither one ran the child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunError {
    /// The gate declined to start the child under load; asking again later may
    /// succeed. See [`is_deferred_under_load`].
    Deferred,
    /// The child never got a concurrency slot inside its own deadline. It did
    /// not start, so it did not run out of time. See [`is_slot_wait_timeout`].
    SlotWaitTimeout,
    /// The child started and ran past its own deadline.
    Timeout,
    /// The OS found nothing to run at the program name.
    SpawnNotFound,
    /// The OS refused to start the program for any other reason.
    SpawnFailed,
    /// Anything else: the child ran and failed, or the text is not the
    /// runner's.
    Other,
}

impl RunError {
    /// Classify a runner failure message. Spawn failures are recognised before
    /// timeouts so a program path that spells the timeout phrase cannot turn a
    /// spawn failure into a hang.
    pub(crate) fn classify(err: &str) -> Self {
        let body = strip_code_prefix(err);
        if is_deferred_under_load(err) {
            Self::Deferred
        } else if is_spawn_not_found(body) {
            Self::SpawnNotFound
        } else if body.starts_with("Failed to spawn ") {
            Self::SpawnFailed
        } else if is_slot_wait_timeout(err) {
            Self::SlotWaitTimeout
        } else if is_runner_timeout_shape(body) {
            Self::Timeout
        } else {
            Self::Other
        }
    }
}

/// The runner's own deadline text, `<label> timed out after <n>s`, anchored at
/// the start: a label of plain words (program and subcommand, or a program
/// path), the marker, then the seconds. Text a child printed and a caller
/// relayed (`... Operation timed out after 30001 milliseconds`) has a label
/// full of punctuation and a unit that is not `s`, and is not this.
fn is_runner_timeout_shape(body: &str) -> bool {
    let Some((label, after)) = body.split_once(TIMEOUT_MARKER) else {
        return false;
    };
    let plain_label = !label.is_empty()
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '/'));
    let digits = after
        .bytes()
        .take_while(|b| b.is_ascii_digit() || *b == b'.')
        .count();
    plain_label && digits > 0 && after[digits..].starts_with('s')
}

/// What [`time_exec_probe`] saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecProbeRun {
    /// The child was admitted, ran and exited zero; this long from admission
    /// to reaped.
    Ran(Duration),
    /// The child started and was still running at its deadline. The slowest
    /// outcome there is, so a caller reads it as a sample of the deadline.
    DeadlineHit,
    /// No timing: the gate refused it or no slot came free, it failed to
    /// spawn, or it exited non-zero.
    Failed,
}

/// Starts `program` (a child that does nothing) and times it from the moment
/// the spawn gate admits it to the moment the runner has reaped it, for
/// `sysload::exec_probe`.
///
/// It runs through the same funnel as every other child (`run_admitted`), so
/// it is bounded by `deadline`, counted in the gate report as `exec probe`, and
/// held to the concurrency limit as a `Background` spawn; it is exempt from
/// the rate bucket and the load verdict ([`with_exec_probe`] says why). It runs
/// at standard QoS whatever its class: the signal stands for the reactive reads
/// a person is waiting on, and a background-band child is throttled by design.
/// The wait for a slot is not part of the time.
pub(crate) fn time_exec_probe(program: &Path, deadline: Duration) -> ExecProbeRun {
    let Some(program) = program.to_str() else {
        return ExecProbeRun::Failed;
    };
    // The seam's own constructor: the same scrubbed environment as every other
    // captured child, so the probe times what a git read would pay.
    let mut cmd = build_capture_command(program, &[], None, &[], None, None);
    let mut options = RunOptions::new(deadline);
    options.qos = Some(QosTier::Standard);
    let admitted = Cell::new(None);
    let result = with_admission(Admission::Background, || {
        with_exec_probe(|| {
            run_admitted(
                &mut cmd,
                "exec probe",
                options,
                None,
                1024,
                &mut (),
                spawn_gate(),
                &|| admitted.set(Some(Instant::now())),
            )
        })
    });
    match result {
        Ok(run) if run.success && run.incomplete.is_none() => admitted
            .get()
            .map_or(ExecProbeRun::Failed, |at| ExecProbeRun::Ran(at.elapsed())),
        Ok(_) => ExecProbeRun::Failed,
        Err(error) if is_run_timeout(error.message()) => ExecProbeRun::DeadlineHit,
        Err(_) => ExecProbeRun::Failed,
    }
}

/// True when `err` is a child that started and ran past its deadline: not a
/// deferral, not a refused slot wait, not a spawn failure.
pub(crate) fn is_run_timeout(err: &str) -> bool {
    RunError::classify(err) == RunError::Timeout
}

/// What one pipe drain produced.
///
/// `stop` is the field that keeps a broken read distinguishable from a short
/// one: `bytes` may be a perfectly well-formed prefix either way, and only
/// this says whether the rest is missing because there was no more or because
/// we stopped being able to read it.
#[derive(Default)]
pub(super) struct Drained {
    pub(super) bytes: Vec<u8>,
    /// Bytes already handed to a streaming observer and dropped from `bytes`
    /// (see [`ProcessObserver::drains_stdout`]); they still count against the
    /// cap, so a streamed read is as bounded as a buffered one.
    pub(super) discarded: usize,
    /// Cut off at the byte cap: the child had more to say.
    pub(super) truncated: bool,
    /// Why the read stopped, when it was not end-of-stream.
    pub(super) stop: Option<Stop>,
}

impl Drained {
    pub(super) fn append(&mut self, bytes: &[u8], cap: usize) {
        let held = self.bytes.len().saturating_add(self.discarded);
        let take = bytes.len().min(cap.saturating_sub(held));
        self.bytes.extend_from_slice(&bytes[..take]);
        self.truncated |= take < bytes.len();
    }

    /// Drops what an observer has now seen, keeping its length in the budget.
    pub(super) fn discard_observed(&mut self) {
        self.discarded = self.discarded.saturating_add(self.bytes.len());
        self.bytes.clear();
    }
}

/// The two ways a drain ends without reaching end-of-stream. They are kept
/// apart because they deserve different answers: one is a fault, the other is
/// a known-incomplete read of a child that did exit cleanly.
pub(super) enum Stop {
    /// The read itself failed. What arrived is a fragment of unknown length,
    /// and nothing downstream can tell it from a complete short output.
    Broken(String),
    /// EOF was not observed inside the grace window, or the blocking fallback
    /// reader did not deliver. The child's own status is still trustworthy.
    Undelivered(String),
}

/// Why captured stdout is a prefix of what the child actually wrote.
///
/// This exists because a single `truncated: bool` was asked to mean two
/// unrelated things — "the child said more than the budget allowed" and "we
/// never finished reading it" — and the callers that turn the flag into a
/// sentence could only name one of them. They named the wrong one: a
/// `for-each-ref` whose entire output was 1,482 bytes was reported to the user
/// as `output exceeded 64 MB`, because the drain thread missed its delivery
/// window on a loaded machine. A check that could not run must not report the
/// same way as one that ran and found too much.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incomplete {
    /// stdout reached the caller's byte budget; the rest was dropped. The
    /// amount missing is unknown but the child was read to the end of the cap.
    OverCap(usize),
    /// The read did not finish inside the grace window, so how much is missing
    /// — if anything — is unknown. Captured bytes may still be available.
    Unread(String),
    /// The child was stopped because its runtime deadline elapsed. Captured
    /// bytes are a prefix of what it would have written. `over_cap` is set
    /// when that prefix had already filled the caller's byte budget.
    ///
    /// `message` is the same sentence a timeout error carries
    /// (`"{label} timed out after {n}s"`), so a caller that cannot accept a
    /// prefix reports the deadline and not a slot wait or a deferral.
    Deadline {
        message: String,
        over_cap: Option<usize>,
    },
}

impl Incomplete {
    /// The clause a caller appends after naming its subject — "git blame
    /// output", "This diff", "Log output". Deliberately subject-free so one
    /// renderer serves an error string, an AI warning and two UI banners
    /// without any of them re-deriving the cause and getting it wrong.
    ///
    /// Every arm says what happened; none invents a cause.
    pub fn describe(&self) -> String {
        match self {
            // Whole MiB when the budget is one, exact bytes otherwise: a
            // 512 KiB cap rendered as "exceeded 0 MB" reads as a bug in the
            // message rather than a fact about the output.
            Incomplete::OverCap(cap) => over_cap_phrase(*cap),
            Incomplete::Unread(why) => format!("could not be read to the end ({why})"),
            // The byte budget is the proven bound when both happened. The
            // deadline message stays on the variant for callers that still
            // have to fail a partial stream.
            Incomplete::Deadline {
                over_cap: Some(cap),
                ..
            } => over_cap_phrase(*cap),
            Incomplete::Deadline {
                message,
                over_cap: None,
            } => format!("stopped at its deadline ({message})"),
        }
    }
}

fn over_cap_phrase(cap: usize) -> String {
    if cap >= 1024 * 1024 {
        format!("exceeded {} MB", cap / (1024 * 1024))
    } else {
        format!("exceeded {cap} bytes")
    }
}

/// What [`run_bounded`] observed, before a caller shapes its own errors.
#[derive(Clone, Debug)]
pub(crate) struct BoundedRun {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub success: bool,
    pub status_code: i32,
    /// The signal that ended the child, on Unix, when one did. `status_code`
    /// is -1 for that death and for "no status at all" alike, so a caller that
    /// must tell a crash from a deadline or a cancel reads this.
    pub signal: Option<i32>,
    /// Why `stdout` is a prefix, when it is; `None` when it is the whole
    /// stream. Carried as a reason rather than a flag so no caller has to
    /// guess which of the two causes it is looking at.
    pub incomplete: Option<Incomplete>,
    pub stderr_incomplete: Option<Incomplete>,
    pub cancelled: bool,
}

/// Everything the runner itself knows about how a child's life ended, for an
/// observer that has to explain a death `BoundedRun` flattens to a status of -1.
///
/// Nothing here is inferred: each field is read from the wait status, from a
/// branch the runner took, or from the process-wide shutdown flag.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ExitFacts {
    /// The raw wait status word (`wait(2)`) on Unix, the exit code elsewhere.
    /// `None` when the child was never waited on to a status: a deadline that
    /// ended it.
    pub raw_wait_status: Option<i32>,
    /// The signal that ended the child, when the status says one did.
    pub signal: Option<i32>,
    /// Whether the status says the child dumped core.
    pub core_dumped: bool,
    /// The signal that stopped the child, when the status says it stopped
    /// rather than exited.
    pub stopped_signal: Option<i32>,
    /// Whether the status says the child was continued after a stop.
    pub continued: bool,
    /// The observer's cancel fired while the runner waited.
    pub cancelled: bool,
    /// The runtime deadline ran out while the runner waited.
    pub deadline_expired: bool,
    /// The runner itself asked the child's group to stop and then killed it.
    /// True exactly for a cancel or a deadline, the only two places the runner
    /// signals a child it has not been told to give up on.
    pub runner_signalled_group: bool,
    /// [`crate::procguard::reap_all`] had started when the wait ended, so the
    /// shutdown sweep may have signalled the child.
    pub shutdown_sweep_started: bool,
}

impl ExitFacts {
    fn of(
        status: Option<std::process::ExitStatus>,
        cancelled: bool,
        deadline_expired: bool,
    ) -> Self {
        #[cfg(unix)]
        let (raw_wait_status, signal, core_dumped, stopped_signal, continued) = {
            use std::os::unix::process::ExitStatusExt;
            (
                status.map(ExitStatusExt::into_raw),
                status.and_then(|status| status.signal()),
                status.is_some_and(|status| status.core_dumped()),
                status.and_then(|status| status.stopped_signal()),
                status.is_some_and(|status| status.continued()),
            )
        };
        #[cfg(not(unix))]
        let (raw_wait_status, signal, core_dumped, stopped_signal, continued) = (
            status.and_then(|status| status.code()),
            None,
            false,
            None,
            false,
        );
        Self {
            raw_wait_status,
            signal,
            core_dumped,
            stopped_signal,
            continued,
            cancelled,
            deadline_expired,
            runner_signalled_group: cancelled || deadline_expired,
            shutdown_sweep_started: crate::procguard::shutdown_started(),
        }
    }
}

impl BoundedRun {
    /// Parsers must establish completeness before interpreting a prefix.
    pub(crate) fn require_complete(self, label: &str) -> Result<Self, CommandError> {
        for (stream, reason) in [
            ("output", &self.incomplete),
            ("stderr", &self.stderr_incomplete),
        ] {
            if let Some(reason) = reason {
                return Err(format!("{label} {stream} {}", reason.describe()).into());
            }
        }
        Ok(self)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum OutputStream {
    Stdout,
    Stderr,
}

/// Callbacks run on the waiter, must return promptly, and receive only the
/// bounded captured prefix. They never run on a pipe worker.
pub(crate) trait ProcessObserver {
    fn cancelled(&self) -> bool {
        false
    }
    /// The error a spawn that was cancelled while it queued returns, when this
    /// observer's cancel has a meaning of its own. `None` keeps the runner's
    /// plain "cancelled before spawn", which is what every other observer
    /// (secrets, hooks, tool install, devmap, the commit graph) gets and what
    /// reaches the frontend unclassified.
    fn cancellation(&self) -> Option<CommandError> {
        None
    }
    fn output(&mut self, _stream: OutputStream, _bytes: &[u8]) {}
    /// Told once, on the waiter, how the child's wait ended, just before the
    /// run is returned. Not called when the run fails with an error instead of
    /// producing a [`BoundedRun`].
    fn exited(&mut self, _facts: &ExitFacts) {}
    /// True when the observer keeps what it needs from stdout itself, so the
    /// run need not retain it: each chunk is dropped once `output` has seen it
    /// and the returned [`BoundedRun::stdout`] holds only the unseen tail
    /// (empty in practice). The byte cap still applies to the stream as a
    /// whole. For a reader of a long stream that wants a few bytes per record
    /// out of megabytes, which would otherwise be buffered whole.
    fn drains_stdout(&self) -> bool {
        false
    }
}

impl ProcessObserver for () {}

pub(super) fn observe_output(
    observer: &mut dyn ProcessObserver,
    cursors: &mut [usize; 2],
    stdout: &[u8],
    stderr: &[u8],
) {
    for (index, (stream, bytes)) in [
        (OutputStream::Stdout, stdout),
        (OutputStream::Stderr, stderr),
    ]
    .into_iter()
    .enumerate()
    {
        if bytes.len() > cursors[index] {
            observer.output(stream, &bytes[cursors[index]..]);
            cursors[index] = bytes.len();
        }
    }
}

/// Shared engine behind `git_timeout` and `capture_command`: spawns `cmd`,
/// enforces `timeout`, and bounds stdout/stderr.
///
/// `label` names the process in spawn/timeout/wait errors (callers keep their
/// own wording for truncation). When `stdin_bytes` is set, the child gets a
/// piped stdin pumped without blocking the deadline loop, even while megabytes
/// are still being pushed into the child.
pub(crate) fn run_bounded(
    cmd: Command,
    label: &str,
    timeout: Duration,
    stdin_bytes: Option<&[u8]>,
) -> Result<BoundedRun, CommandError> {
    run_bounded_capped(cmd, label, timeout, stdin_bytes, MAX_OUTPUT_BYTES)
}

/// [`run_bounded`] with an explicit stdout budget.
///
/// [`MAX_OUTPUT_BYTES`] is a backstop against a runaway process, not a sane
/// payload size: 64 MiB of diff text costs ~90 MiB in this process (the bytes
/// plus the lossy `String` copy), ~44 MiB more to serialize for IPC, and
/// ~330 MiB once the webview holds the string and its parsed rows. Callers
/// whose output lands in the UI pass the budget their surface can actually
/// render, and tell the user when they hit it.
pub(crate) fn run_bounded_capped(
    mut cmd: Command,
    label: &str,
    timeout: Duration,
    stdin_bytes: Option<&[u8]>,
    stdout_cap: usize,
) -> Result<BoundedRun, CommandError> {
    run_observed(&mut cmd, label, timeout, stdin_bytes, stdout_cap, &mut ())
}

/// Per-invocation controls for [`run_observed_with`] and [`run_with_gate_options`].
#[derive(Clone, Copy, Debug)]
pub struct RunOptions {
    pub timeout: Duration,
    pub kill_grace: Duration,
    pub drain_grace: Duration,
    pub qos: Option<QosTier>,
    pub keep_deadline_prefix: bool,
}

impl RunOptions {
    pub const fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            kill_grace: DEFAULT_KILL_GRACE,
            drain_grace: DEFAULT_DRAIN_GRACE,
            qos: None,
            keep_deadline_prefix: false,
        }
    }
}

pub(crate) fn run_observed(
    cmd: &mut Command,
    label: &str,
    timeout: Duration,
    stdin_bytes: Option<&[u8]>,
    stdout_cap: usize,
    observer: &mut dyn ProcessObserver,
) -> Result<BoundedRun, CommandError> {
    run_observed_with(
        cmd,
        label,
        RunOptions::new(timeout),
        stdin_bytes,
        stdout_cap,
        observer,
    )
}

pub(crate) fn run_observed_with(
    cmd: &mut Command,
    label: &str,
    options: RunOptions,
    stdin_bytes: Option<&[u8]>,
    stdout_cap: usize,
    observer: &mut dyn ProcessObserver,
) -> Result<BoundedRun, CommandError> {
    run_with_gate_options(
        cmd,
        label,
        options,
        stdin_bytes,
        stdout_cap,
        observer,
        spawn_gate(),
    )
}

pub(crate) fn run_with_gate(
    cmd: &mut Command,
    label: &str,
    timeout: Duration,
    stdin_bytes: Option<&[u8]>,
    stdout_cap: usize,
    observer: &mut dyn ProcessObserver,
    gate: &'static SpawnGate,
) -> Result<BoundedRun, CommandError> {
    run_with_gate_options(
        cmd,
        label,
        RunOptions::new(timeout),
        stdin_bytes,
        stdout_cap,
        observer,
        gate,
    )
}

pub(crate) fn run_with_gate_options(
    cmd: &mut Command,
    label: &str,
    options: RunOptions,
    stdin_bytes: Option<&[u8]>,
    stdout_cap: usize,
    observer: &mut dyn ProcessObserver,
    gate: &'static SpawnGate,
) -> Result<BoundedRun, CommandError> {
    run_admitted(
        cmd,
        label,
        options,
        stdin_bytes,
        stdout_cap,
        observer,
        gate,
        &|| {},
    )
}

thread_local! {
    /// Set only by [`KeepDeadlinePrefix`]. A capped git read holds it so a
    /// deadline returns the bytes already captured. Every other caller leaves
    /// it unset and still receives a timeout error.
    static KEEP_DEADLINE_PREFIX: Cell<bool> = const { Cell::new(false) };
    static PROCESS_FAILURES: Cell<u64> = const { Cell::new(0) };
    static LAST_PROCESS_FAILURE: std::cell::RefCell<Option<CommandError>> =
        const { std::cell::RefCell::new(None) };
    #[cfg(test)]
    static FORCE_SPAWN_FAILURE: Cell<bool> = const { Cell::new(false) };
    /// Every spawn on this thread is refused as the gate sheds background work.
    #[cfg(test)]
    static FORCE_DEFERRAL: Cell<bool> = const { Cell::new(false) };
    /// Runs at every forced failure, so a test can tell when a computation
    /// is in progress, or stretch it, without timing guesses.
    #[cfg(test)]
    static FORCED_FAILURE_HOOK: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
    /// Labels of the children this thread asked the gate to run, newest last,
    /// for [`spawn_labels_during`].
    #[cfg(test)]
    static SPAWN_LABELS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Fails only the spawns on this thread whose arguments include this one.
    #[cfg(test)]
    static FORCED_FAILURE_ARG: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Child processes on this thread that produced no answer at all: refused or
/// deferred by the gate, failed to spawn, or killed at their deadline. A
/// non-zero git exit is an answer and is not counted; neither is a run whose
/// output was cut short, which carries its own `incomplete` reason. A deadline
/// whose prefix a capped read kept is that second case: it is counted only
/// when a caller that required the whole stream rejects it.
///
/// Callers that swallow a git error as "not set" (`config --get`, a
/// `symbolic-ref --quiet`) cannot tell those apart from a real "not set".
/// Comparing this counter across a computation tells them whether it saw
/// the repository or saw the gate, so a cache never keeps an answer that
/// was really a refusal.
pub(crate) fn process_failures() -> u64 {
    PROCESS_FAILURES.get()
}

/// What the newest failure counted by [`process_failures`] said. A caller
/// that compared the counter and found a failure it swallowed can report the
/// cause instead of an empty field — and keep its kind, so the frontend knows
/// a deferred answer is worth asking for again.
pub(crate) fn last_process_failure() -> Option<CommandError> {
    LAST_PROCESS_FAILURE.with(|slot| slot.borrow().clone())
}

pub(crate) fn note_process_failure(error: &CommandError) {
    PROCESS_FAILURES.set(PROCESS_FAILURES.get().wrapping_add(1));
    LAST_PROCESS_FAILURE.with(|slot| *slot.borrow_mut() = Some(error.clone()));
}

/// While alive on this thread, a child killed at its runtime deadline is a
/// captured prefix ([`Incomplete::Deadline`]) rather than an error that drops
/// those bytes. [`git_run_inner`] is the only production holder: pulse and
/// the other capped git reads can show what git already printed, and a
/// command that must see the whole stream still fails.
pub(crate) struct KeepDeadlinePrefix;

impl KeepDeadlinePrefix {
    pub(crate) fn enter() -> Self {
        KEEP_DEADLINE_PREFIX.set(true);
        Self
    }
}

impl Drop for KeepDeadlinePrefix {
    fn drop(&mut self) {
        KEEP_DEADLINE_PREFIX.set(false);
    }
}

pub(super) fn keeping_deadline_prefix() -> bool {
    KEEP_DEADLINE_PREFIX.get()
}

/// Every child spawned on this thread inside `body` fails as a spawn error,
/// without starting. Lets a test reproduce "the gate refused" independent of
/// machine load.
#[cfg(test)]
pub(crate) fn with_forced_spawn_failure<T>(body: impl FnOnce() -> T) -> T {
    let _restore = std::mem::DropGuard::new(FORCE_SPAWN_FAILURE.replace(true), |prev| {
        FORCE_SPAWN_FAILURE.set(prev);
    });
    body()
}

/// Every child spawned on this thread inside `body` is refused exactly as the
/// gate refuses shed background work ([`Refusal::Shed`]): the error a
/// `gitpulse-mcp` read gets when the spawn budget is low, independent of
/// machine load.
#[cfg(test)]
pub(crate) fn with_forced_deferral<T>(body: impl FnOnce() -> T) -> T {
    let _restore = std::mem::DropGuard::new(FORCE_DEFERRAL.replace(true), |prev| {
        FORCE_DEFERRAL.set(prev);
    });
    body()
}

/// [`with_forced_spawn_failure`] for only the spawns whose arguments include
/// `arg` (a subcommand such as `"symbolic-ref"`), so a test can refuse one read
/// of a computation and let the rest answer.
#[cfg(test)]
pub(crate) fn with_forced_spawn_failure_of<T>(arg: &str, body: impl FnOnce() -> T) -> T {
    let previous = FORCED_FAILURE_ARG.with(|slot| slot.borrow_mut().replace(arg.to_string()));
    let _restore = std::mem::DropGuard::new(previous, |previous| {
        FORCED_FAILURE_ARG.with(|slot| *slot.borrow_mut() = previous);
    });
    body()
}

/// True while this thread is inside [`with_forced_spawn_failure`] or
/// [`with_forced_spawn_failure_of`]. Process-wide caches of spawn results
/// consult it: the forced refusal is thread-local, the cache is shared, and
/// another test warming the cache would otherwise let the real tool answer past
/// the refusal.
#[cfg(test)]
pub(crate) fn forced_failure_active() -> bool {
    FORCE_SPAWN_FAILURE.get() || FORCED_FAILURE_ARG.with(|slot| slot.borrow().is_some())
}

#[cfg(test)]
pub(super) fn forced_deferral_active() -> bool {
    FORCE_DEFERRAL.get()
}

#[cfg(test)]
fn forced_failure_matches(cmd: &Command) -> bool {
    FORCED_FAILURE_ARG.with(|slot| {
        slot.borrow()
            .as_deref()
            .is_some_and(|wanted| cmd.get_args().any(|arg| arg == wanted))
    })
}

/// [`with_forced_spawn_failure`], calling `hook` at each forced failure.
#[cfg(test)]
pub(crate) fn with_forced_spawn_failure_then<T>(
    hook: impl Fn() + 'static,
    body: impl FnOnce() -> T,
) -> T {
    FORCED_FAILURE_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    let _clear = std::mem::DropGuard::new((), |()| {
        FORCED_FAILURE_HOOK.with(|slot| slot.borrow_mut().take());
    });
    with_forced_spawn_failure(body)
}

/// Runs `body` and returns the label of every child it asked this thread's
/// gate to spawn — the spawn-count assertion for "one process, not N". Only
/// spawns made on the calling thread are seen, which is where a synchronous
/// reader makes them.
#[cfg(test)]
pub(crate) fn spawn_labels_during<R>(body: impl FnOnce() -> R) -> (R, Vec<String>) {
    SPAWN_LABELS.with(|labels| labels.borrow_mut().clear());
    let result = body();
    let seen = SPAWN_LABELS.with(|labels| std::mem::take(&mut *labels.borrow_mut()));
    (result, seen)
}

/// Asks `child` to stop and spends the grace reading its pipes.
///
/// A child told to stop writes while it cleans up, and one blocked on a full
/// pipe cannot finish, so a sleeping grace gets it SIGKILLed with its
/// `.git/index.lock` still held. The wait is the output poll itself, so the
/// grace both waits and drains, and procguard still ends it at [`CHILD_GRACE`]
/// however much arrives; the bytes land in the same capped buffers. Stdin is
/// closed once the ask has been sent and not fed again: a cleanup that reads
/// stdin to EOF must see it, but never before the ask. And `input.finish()` afterwards still only turns undelivered
/// bytes into an error for a successful, uncancelled run. A panic inside the
/// wait is handled by `Registration::drop`, which force-kills a child no reap
/// path has claimed. Where the platform cannot ask there is no grace, and on
/// non-Unix the pipe workers already read on their own threads.
pub(super) fn ask_and_drain(
    guard: &crate::procguard::Registration,
    child: &mut std::process::Child,
    kill_grace: Duration,
    #[cfg(unix)] output: &mut pipe_drain::OutputDrains,
    #[cfg(not(unix))] _output: &mut thread_io::OutputDrains,
    #[cfg(unix)] input: &mut pipe_drain::InputFeed<'_>,
    #[cfg(not(unix))] _input: &mut thread_io::InputFeed,
) -> std::io::Result<std::process::ExitStatus> {
    #[cfg(unix)]
    return guard.ask_then_stop_and_reap_with_grace(child, kill_grace, &mut |span| {
        // First wait: SIGTERM has already been sent, so a child that reads
        // EOF now was asked to stop first and cannot take truncated input
        // for the whole of it.
        input.close();
        output.drain_ready();
        if output.wait_with_input(span, None).is_err() {
            std::thread::sleep(span);
        }
        output.drain_ready();
    });
    #[cfg(not(unix))]
    guard.ask_then_stop_and_reap_with_grace(child, kill_grace, &mut std::thread::sleep)
}

/// [`run_with_gate`] with `on_admitted` called after the gate admits the
/// child and before it is spawned — the last moment at which nothing has been
/// read yet, which is what [`run_read_shared`] needs to stop new joiners.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_admitted(
    cmd: &mut Command,
    label: &str,
    options: RunOptions,
    stdin_bytes: Option<&[u8]>,
    stdout_cap: usize,
    observer: &mut dyn ProcessObserver,
    gate: &'static SpawnGate,
    on_admitted: &dyn Fn(),
) -> Result<BoundedRun, CommandError> {
    #[cfg(test)]
    SPAWN_LABELS.with(|labels| labels.borrow_mut().push(label.to_string()));
    #[cfg(test)]
    if FORCE_DEFERRAL.get() {
        return Err(refusal_message(label, Refusal::Shed));
    }
    #[cfg(test)]
    if FORCE_SPAWN_FAILURE.get() || forced_failure_matches(cmd) {
        FORCED_FAILURE_HOOK.with(|slot| {
            if let Some(hook) = slot.borrow().as_ref() {
                hook();
            }
        });
        let error = CommandError::from(format!("Failed to spawn {label}: forced by test"));
        note_process_failure(&error);
        return Err(error);
    }
    let result = run_admitted_inner(
        cmd,
        label,
        options,
        stdin_bytes,
        stdout_cap,
        observer,
        gate,
        on_admitted,
    );
    // A capped git read asked to keep the prefix. Everyone else still sees
    // the deadline as an error, with the same sentence as before.
    let result = match result {
        Ok(run)
            if !(options.keep_deadline_prefix || keeping_deadline_prefix())
                && matches!(run.incomplete, Some(Incomplete::Deadline { .. })) =>
        {
            let Some(Incomplete::Deadline { message, .. }) = run.incomplete else {
                unreachable!("matched Deadline above");
            };
            Err(message.into())
        }
        other => other,
    };
    if let Some(error) = result
        .as_ref()
        .err()
        .filter(|error| !is_cancellation(error))
    {
        note_process_failure(error);
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn run_admitted_inner(
    cmd: &mut Command,
    label: &str,
    options: RunOptions,
    stdin_bytes: Option<&[u8]>,
    stdout_cap: usize,
    observer: &mut dyn ProcessObserver,
    gate: &'static SpawnGate,
    on_admitted: &dyn Fn(),
) -> Result<BoundedRun, CommandError> {
    let timeout = options.timeout;
    if timeout > NETWORK_TIMEOUT {
        return Err(format!("{label} deadline exceeds {}s", NETWORK_TIMEOUT.as_secs()).into());
    }
    if timeout.is_zero() {
        return Err(format!(
            "{label}{TIMEOUT_MARKER}{}s: deadline must be positive and at most {}s",
            timeout.as_secs_f64(),
            NETWORK_TIMEOUT.as_secs()
        )
        .into());
    }
    let stdout_limit = if observer.drains_stdout() {
        MAX_DRAINED_OUTPUT_BYTES
    } else {
        MAX_OUTPUT_BYTES
    };
    if stdout_cap > stdout_limit || stdin_bytes.is_some_and(|bytes| bytes.len() > MAX_OUTPUT_BYTES)
    {
        return Err(
            format!("{label} input/output budget exceeds the {stdout_limit} byte limit").into(),
        );
    }
    let queue_deadline = gate.clock.now() + timeout;
    if stdin_bytes.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    // Sited here rather than at each construction site because this is the one
    // funnel every spawn in the process already passes through: `git_timeout`,
    // `capture_command`, `run_bounded`, `run_bounded_capped` and `run_observed`
    // all end up in this function. A per-site fix would have to be remembered
    // by the next spawn someone adds; this one cannot be forgotten.
    default_child_path(cmd);

    // Held until this function returns, covering the child's descriptors, its
    // output buffers and fallback reader threads -- see [`SpawnGate`] for why an
    // unbounded fan-out here exhausted the process descriptor table.
    // A refresh in the repository a user action just changed is admitted
    // like the action itself, within the credit `run_command_scope` granted.
    let credited = current_admission() == Admission::Reactive
        && cmd
            .get_current_dir()
            .is_some_and(|dir| gate.take_credit(dir));
    let class = if credited {
        Admission::Interactive
    } else {
        current_admission()
    };
    let _permit = Arc::new(
        with_admission(class, || {
            gate.acquire_judged(queue_deadline, &|| observer.cancelled())
        })
        .map_err(|(refusal, level)| {
            // Decided here, where the observer that cancelled is known: only a
            // read-cancel observer's cancel is a `ReadCancelled`.
            match observer.cancellation() {
                Some(cancelled) if refusal == Refusal::Cancelled => cancelled,
                _ => refusal_message_at(label, refusal, level),
            }
        })?,
    );
    gate.note_admitted_label(label);
    on_admitted();

    // Slot wait is already bounded by `queue_deadline`. The child then gets
    // the full requested runtime: a 5s git call that spent 4.9s queued must
    // not be reported as "timed out" after 100ms of actual work. Under
    // llvm-cov that is how shebang and stub-devmap tests failed a 5s/30s
    // deadline they would have met once they started.
    let deadline = Instant::now() + timeout;

    // Spawned into a process group of its own and registered, so a SIGTERM to
    // this process — or the deliberate `process::exit` on `gitpulse-mcp`'s
    // shutdown path — takes this child and everything it forked down with it
    // instead of orphaning them. See [`crate::procguard`].
    //
    // A background-band child enters the band itself, before it execs: applied
    // from here after the spawn, the band missed any child that exec'd at once
    // (every `sh -c`, and the `git` shims), and a child that cannot enter it
    // fails the spawn rather than silently running at normal priority. See
    // [`crate::procguard::spawn_background`] for the mechanism and its cost.
    let in_band = QosTier::current(options.qos, class) == QosTier::Background;
    let spawned = if in_band {
        crate::procguard::spawn_background(cmd, label)
    } else {
        crate::procguard::spawn(cmd, label)
    };
    #[cfg(all(test, target_os = "macos"))]
    if in_band {
        qos::count_band_spawn(&spawned);
    }
    let (mut child, guard) = spawned.map_err(|e| {
        let message = spawn_error(label, &e);
        // A trust refusal keeps its kind in other words, which is what a
        // caller that only holds the spawn error asks about.
        match crate::procguard::trust_refusal(&e) {
            Some(refusal) => refusal.clone().wrap(|_| message),
            None => message.into(),
        }
    })?;
    #[cfg(test)]
    spawn_log::record(cmd);

    // On Unix the command waiter owns and drains both nonblocking pipes.
    // EOF no longer depends on two separately scheduled reader threads
    // handing their entire result over inside a two-second window.
    #[cfg(unix)]
    let mut output =
        match pipe_drain::OutputDrains::new(child.stdout.take(), child.stderr.take(), stdout_cap) {
            Ok(output) => output,
            Err(error) => {
                guard.stop_and_reap(&mut child, Duration::ZERO).ok();
                return Err(format!("Failed to prepare {label} output: {error}").into());
            }
        };
    #[cfg(not(unix))]
    let mut output = match thread_io::OutputDrains::new(
        child.stdout.take(),
        child.stderr.take(),
        stdout_cap,
        _permit.clone(),
    ) {
        Ok(output) => output,
        Err(error) => {
            guard.stop_and_reap(&mut child, Duration::ZERO).ok();
            return Err(format!("Failed to prepare {label} output: {error}").into());
        }
    };

    // Input delivery is bounded independently of child exit. A successful
    // exit cannot conceal rejected or undelivered stdin.
    #[cfg(unix)]
    let mut input =
        match pipe_drain::InputFeed::new(child.stdin.take(), stdin_bytes.unwrap_or_default()) {
            Ok(input) => input,
            Err(error) => {
                guard.stop_and_reap(&mut child, Duration::ZERO).ok();
                return Err(format!("Failed to prepare {label} stdin: {error}").into());
            }
        };
    #[cfg(not(unix))]
    let mut input = match thread_io::InputFeed::new(
        child.stdin.take(),
        stdin_bytes.unwrap_or_default(),
        _permit.clone(),
    ) {
        Ok(input) => input,
        Err(error) => {
            guard.stop_and_reap(&mut child, Duration::ZERO).ok();
            return Err(format!("Failed to prepare {label} stdin: {error}").into());
        }
    };

    let mut backoff = POLL_BACKOFF_START;
    let mut cursors = [0; 2];
    enum Halt {
        Exited(std::process::ExitStatus, bool),
        /// Runtime deadline. The child was asked to stop, then killed and
        /// reaped; if the signal guard refused the kill, it is instead left
        /// registered with procguard after a bounded wait (REAP_BOUND) and
        /// is not reaped. `message` is the timeout sentence callers that
        /// cannot keep a prefix still return.
        Deadline(String),
        Failed(String),
    }
    let halt = loop {
        #[cfg(unix)]
        output.drain_ready();
        #[cfg(unix)]
        input.pump();
        output.observe(observer, &mut cursors);
        if observer.cancelled() {
            // Asked first: git removes its own `.git/index.lock` when asked and
            // cannot when killed, so a cancelled commit would block the next.
            break match ask_and_drain(
                &guard,
                &mut child,
                options.kill_grace,
                &mut output,
                &mut input,
            ) {
                Ok(status) => Halt::Exited(status, true),
                Err(e) => Halt::Failed(format!("Failed to reap cancelled {label}: {e}")),
            };
        }
        // Every wait goes through the registration so the registry never
        // holds a pid that has already been reaped — see `procguard` for why
        // signalling a recycled pid is the thing to avoid.
        match guard.poll(|| child.try_wait()) {
            Ok(Some(status)) => break Halt::Exited(status, false),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let message = format!("{label}{TIMEOUT_MARKER}{}s", timeout.as_secs_f64());
                    // Reap before reading the rest of the pipes: a live child
                    // can hold them open. The ask comes first so git can
                    // release `.git/index.lock`; a child that ignores it is
                    // killed after [`CHILD_GRACE`], which a child that exits
                    // sooner does not wait out.
                    ask_and_drain(
                        &guard,
                        &mut child,
                        options.kill_grace,
                        &mut output,
                        &mut input,
                    )
                    .ok();
                    break Halt::Deadline(message);
                }
                #[cfg(unix)]
                if let Err(error) = output.wait_with_input(
                    backoff.min(deadline.saturating_duration_since(Instant::now())),
                    Some(&input),
                ) {
                    guard.stop_and_reap(&mut child, Duration::ZERO).ok();
                    break Halt::Failed(format!("Failed to poll {label} output: {error}"));
                }
                #[cfg(not(unix))]
                thread::sleep(backoff);
                backoff = next_poll_backoff(backoff);
            }
            Err(e) => {
                guard.stop_and_reap(&mut child, Duration::ZERO).ok();
                break Halt::Failed(format!("Failed to wait on {}: {}", label, e));
            }
        }
    };
    let (status, cancelled, deadline_message) = match halt {
        Halt::Failed(error) => return Err(error.into()),
        Halt::Exited(status, cancelled) => (Some(status), cancelled, None),
        Halt::Deadline(message) => (None, false, Some(message)),
    };
    if let Err(error) = input.finish() {
        if deadline_message.is_none() && status.is_some_and(|status| status.success()) && !cancelled
        {
            return Err(format!("Failed to deliver {label} {error}").into());
        }
    }
    // Normal exit: EOF is imminent unless something else is holding a
    // write end; then we take whatever was buffered after the grace
    // window instead of hanging forever. Two things can hold one, and
    // the rarer-sounding one is what actually fired in the field: a
    // grandchild the child daemonized, and — until
    // `procguard::with_inheritance_lock` — any sibling spawned inside
    // this pipe's pre-`FD_CLOEXEC` window, which on macOS `std` cannot
    // close atomically. Do not read this reason as the first cause
    // alone. A deadline spends no grace on the pipes, whether the child
    // was reaped or, after a refused kill, left registered with procguard
    // after its bounded wait (REAP_BOUND): the finish deadline is zero, so
    // a still-live child holding the pipes cannot hang the read, and the
    // bytes in the buffer are the prefix.
    let (mut stdout, mut stderr) = output.finish(
        Instant::now()
            + if cancelled || deadline_message.is_some() {
                Duration::ZERO
            } else {
                options.drain_grace
            },
    );
    observe_output(observer, &mut cursors, &stdout.bytes, &stderr.bytes);
    // A stdout we could not read to the end is not a shorter stdout:
    // every caller past this point parses what it is handed as the
    // whole answer. A broken read is a fault and fails the run; an
    // undelivered one is the documented grandchild case, where the
    // child's status is still good — that reports as a prefix, and
    // carries its reason so no caller has to guess at one.
    // A deadline is itself the reason the read stopped. A pipe that
    // breaks because the child was killed must not discard the prefix.
    let mut unread: Option<String> = None;
    match stdout.stop.take() {
        Some(Stop::Broken(e)) if deadline_message.is_none() => {
            return Err(format!("Failed to read {label} output: {e}").into());
        }
        Some(Stop::Broken(e) | Stop::Undelivered(e)) => {
            stderr
                .bytes
                .extend_from_slice(format!("\n[stdout incomplete: {e}]").as_bytes());
            if deadline_message.is_none() {
                unread = Some(e);
            }
        }
        None => {}
    }
    // stderr is diagnosis, not payload: losing it must not fail a
    // command that worked, but a message built from a partial stderr
    // has to say that is what it is.
    let mut stderr_incomplete = None;
    if let Some(Stop::Broken(e) | Stop::Undelivered(e)) = stderr.stop.take() {
        stderr
            .bytes
            .extend_from_slice(format!("\n[stderr incomplete: {e}]").as_bytes());
        stderr_incomplete = Some(Incomplete::Unread(e));
    }
    if stderr.truncated {
        stderr_incomplete = Some(Incomplete::OverCap(4 * 1024 * 1024));
        stderr
            .bytes
            .extend_from_slice(b"\n[stderr incomplete: exceeded 4 MB]");
    }
    // A capped stream may also miss EOF. The proven cap violation is
    // reported first; the read diagnosis remains in stderr. A deadline
    // keeps the cap on the same variant so the prefix is not thrown
    // away on the way out of this function.
    let incomplete = if let Some(message) = deadline_message.clone() {
        Some(Incomplete::Deadline {
            message,
            over_cap: stdout.truncated.then_some(stdout_cap),
        })
    } else if stdout.truncated {
        Some(Incomplete::OverCap(stdout_cap))
    } else {
        unread.map(Incomplete::Unread)
    };
    observer.exited(&ExitFacts::of(
        status,
        cancelled,
        deadline_message.is_some(),
    ));
    Ok(BoundedRun {
        stdout: stdout.bytes,
        stderr: stderr.bytes,
        success: deadline_message.is_none()
            && status.is_some_and(|status| status.success())
            && !cancelled,
        status_code: status.and_then(|status| status.code()).unwrap_or(-1),
        signal: terminating_signal(status),
        incomplete,
        stderr_incomplete,
        cancelled,
    })
}

/// The signal that ended a child, where the platform reports one.
fn terminating_signal(status: Option<std::process::ExitStatus>) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.and_then(|status| status.signal())
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}
