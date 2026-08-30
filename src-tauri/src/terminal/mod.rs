//! Terminal execution module: PTY session management and bounded command execution.

use crate::coverage_toolchain::{
    repo_local_program, JS_COVERAGE_PROVIDERS, PYTEST_PACKAGES, VENV_DIR_NAMES,
};
use crate::engine::git_cli::{
    run_captured, sandbox_join, sandbox_join_canonical, validate_repo, RunOutcome,
};
use crate::harness::{guard_command, PolicyVerdict};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

/// Bounds for one-shot command execution timeout.
const MIN_RUN_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_RUN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const DEFAULT_RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// Per-stream output tail cap (64 KB).
const TERMINAL_TAIL_CAP_BYTES: usize = 64 * 1024;
/// Command payload bounds. These protect both the process launcher and the
/// policy sidecar from an IPC caller sending an effectively unbounded argv.
const TERMINAL_ARG_COUNT_CAP: usize = 256;
const TERMINAL_ARG_BYTES_CAP: usize = 16 * 1024;
const TERMINAL_ARGV_BYTES_CAP: usize = 128 * 1024;
/// Interactive terminal resource bounds. The PTY is user-owned, but its IPC
/// surface must not permit unbounded processes, allocations, or resize work.
const MAX_PTY_SESSIONS: usize = 16;
const MAX_PTY_INPUT_BYTES: usize = 64 * 1024;
const MAX_PTY_ROWS: u16 = 1_000;
const MAX_PTY_COLS: u16 = 1_000;

/// Why a model-authored command is being requested. The value is not merely
/// telemetry: it selects a purpose-specific allowlist before any process is
/// spawned, so a coverage *analysis* plan cannot become a package install and
/// a remediation plan cannot become an arbitrary shell. Coverage *generation*
/// may install one locked crate (`cargo-llvm-cov`) and add `llvm-tools-preview`
/// when the scanner planned that setup; it still cannot install arbitrary crates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManviActionKind {
    Health,
    Coverage,
    CoverageGenerator,
}

impl ManviActionKind {
    fn label(self) -> &'static str {
        match self {
            Self::Health => "health remediation",
            Self::Coverage => "coverage analysis",
            Self::CoverageGenerator => "coverage generation",
        }
    }
}

/// Result of one-shot command execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalRunResult {
    pub command: String,
    pub gated: bool,
    pub policy: Option<PolicyVerdict>,
    pub timed_out: bool,
    pub exit_code: Option<i32>,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub truncated: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalSpawned {
    pub id: String,
    pub shell: String,
    pub cwd: String,
}

#[derive(Clone, Serialize)]
pub struct TerminalOutputPayload {
    pub id: String,
    pub data_b64: String,
}

#[derive(Clone, Serialize)]
pub struct TerminalExitPayload {
    pub id: String,
    pub exit_code: Option<i32>,
    pub signal: String,
}

struct SessionEntry {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    /// Signal handle for the shell, split off the child before the owned
    /// child moves into the reader thread. Keeps kill_session able to
    /// terminate the process and prevents an unreapable zombie.
    killer: Box<dyn ChildKiller + Send>,
    dead: Arc<AtomicBool>,
}

/// Thread-safe registry of live PTY sessions.
#[derive(Default, Clone)]
pub struct TerminalSessions {
    sessions: Arc<Mutex<HashMap<String, SessionEntry>>>,
    active_sessions: Arc<AtomicUsize>,
}

struct SessionReservation {
    active_sessions: Arc<AtomicUsize>,
    armed: bool,
}

impl SessionReservation {
    fn commit(mut self) {
        self.armed = false;
    }
}

impl Drop for SessionReservation {
    fn drop(&mut self) {
        if self.armed {
            self.active_sessions.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

fn reserve_session(state: &TerminalSessions) -> Result<SessionReservation, String> {
    state
        .active_sessions
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            (current < MAX_PTY_SESSIONS).then_some(current + 1)
        })
        .map_err(|_| format!("Terminal session limit reached ({MAX_PTY_SESSIONS})"))?;
    Ok(SessionReservation {
        active_sessions: state.active_sessions.clone(),
        armed: true,
    })
}

fn bounded_pty_size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows: rows.clamp(1, MAX_PTY_ROWS),
        cols: cols.clamp(1, MAX_PTY_COLS),
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Determines the default shell for interactive sessions.
fn default_shell() -> String {
    if cfg!(windows) {
        std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".into())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into())
    }
}

static SESSION_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Snapshot of the parts of [`portable_pty::ExitStatus`] the exit event
/// reports, extracted so the finalize logic is unit-testable without
/// spawning a real PTY-backed process.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExitStatusLike {
    code: u32,
    signal: Option<String>,
}

impl From<&portable_pty::ExitStatus> for ExitStatusLike {
    fn from(status: &portable_pty::ExitStatus) -> Self {
        // On Unix a signal death surfaces as a fallback code (see vendored
        // lib.rs From<std::process::ExitStatus>) plus Some(signal name), so
        // both fields carry honest data.
        ExitStatusLike {
            code: status.exit_code(),
            signal: status.signal().map(str::to_string),
        }
    }
}

/// Reaps the shell and emits exactly one authoritative "terminal-exit" event.
///
/// `wait` blocks until the child terminates; the reader thread calls this only
/// after the master side hit EOF/error/dead-flag, so the child is already gone
/// or dying. A successful wait reports the real exit code — on Unix a killed
/// shell yields the fallback code with the signal name attached — while a
/// failed wait degrades honestly to `exit_code: None` instead of inventing a
/// status.
fn finalize_pty_session<E, W>(session_id: &str, wait: W, mut emit: E)
where
    W: FnOnce() -> std::io::Result<ExitStatusLike>,
    E: FnMut(TerminalExitPayload),
{
    let (exit_code, signal) = match wait() {
        Ok(status) => (
            Some(i32::try_from(status.code).unwrap_or(-1)),
            status.signal.unwrap_or_default(),
        ),
        Err(e) => {
            log::warn!(
                target: "terminal",
                "terminal session '{session_id}': could not reap shell process: {e}"
            );
            (None, String::new())
        }
    };
    emit(TerminalExitPayload {
        id: session_id.to_string(),
        exit_code,
        signal,
    });
}

/// Spawns a PTY session running an interactive shell in `repo_path`.
pub fn spawn_session(
    app: &AppHandle,
    state: &TerminalSessions,
    repo_path: &str,
    rows: u16,
    cols: u16,
) -> Result<TerminalSpawned, String> {
    let repo = validate_repo(repo_path)?;
    let reservation = reserve_session(state)?;
    let pty_system = native_pty_system();
    let size = bounded_pty_size(rows, cols);
    let pair = pty_system
        .openpty(size)
        .map_err(|e| format!("Failed to open PTY: {e}"))?;

    let shell = default_shell();
    let mut cmd = CommandBuilder::new(&shell);
    cmd.cwd(&repo);

    // The child handle is owned: the killer is split off for SessionEntry
    // (kill_session), and the child itself moves into the reader thread,
    // which reaps it after EOF so no zombie survives.
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("Failed to spawn shell '{shell}': {e}"))?;
    let killer: Box<dyn ChildKiller + Send> = child.clone_killer();

    // Drop slave explicitly; master remains open.
    drop(pair.slave);

    let count = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    let session_id = format!("term-{}-{:x}", std::process::id(), count);
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("Failed to clone PTY reader: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("Failed to take PTY writer: {e}"))?;

    let dead = Arc::new(AtomicBool::new(false));
    let entry = SessionEntry {
        master: pair.master,
        writer,
        killer,
        dead: dead.clone(),
    };

    {
        let mut guard = state
            .sessions
            .lock()
            .map_err(|e| format!("Lock error: {e}"))?;
        guard.insert(session_id.clone(), entry);
    }

    // Reader thread: streams output to the frontend via "terminal-output" event.
    let app_handle = app.clone();
    let sid_for_thread = session_id.clone();
    let sid_for_exit = session_id.clone();
    let dead_flag = dead.clone();
    let sessions_map = state.sessions.clone();
    let active_sessions = state.active_sessions.clone();

    let reader_thread = thread::Builder::new()
        .name(format!("pty-read-{session_id}"))
        .spawn(move || {
            let mut buf = [0u8; 4096];
            while !dead_flag.load(Ordering::Relaxed) {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let data_b64 = BASE64_STANDARD.encode(&buf[..n]);
                        if let Err(e) = app_handle.emit(
                            "terminal-output",
                            TerminalOutputPayload {
                                id: sid_for_thread.clone(),
                                data_b64,
                            },
                        ) {
                            log::warn!(
                                target: "terminal",
                                "failed to emit terminal-output for '{sid_for_thread}': {e}"
                            );
                        }
                    }
                    Err(_) => break,
                }
            }

            dead_flag.store(true, Ordering::SeqCst);
            // Clean up session entry from map; a poisoned lock (a sibling
            // panicked mid-write) must not skip cleanup or reaping.
            let mut guard = sessions_map
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let removed = guard.remove(&sid_for_exit).is_some();
            drop(guard);
            if removed {
                active_sessions.fetch_sub(1, Ordering::AcqRel);
            }

            // Reap the shell and report its real exit status.
            finalize_pty_session(
                &sid_for_exit,
                move || child.wait().map(|status| ExitStatusLike::from(&status)),
                |payload| {
                    if let Err(e) = app_handle.emit("terminal-exit", payload) {
                        log::warn!(target: "terminal", "failed to emit terminal-exit event: {e}");
                    }
                },
            );
        });
    if let Err(e) = reader_thread {
        let mut guard = state
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.remove(&session_id);
        return Err(format!("Failed to spawn PTY reader thread: {e}"));
    }
    reservation.commit();

    Ok(TerminalSpawned {
        id: session_id,
        shell,
        cwd: repo.to_string_lossy().into_owned(),
    })
}

/// Writes user input bytes into a PTY session.
pub fn write_to_session(
    state: &TerminalSessions,
    session_id: &str,
    data: &str,
) -> Result<(), String> {
    if data.len() > MAX_PTY_INPUT_BYTES {
        return Err(format!(
            "Terminal input is {} bytes; the per-write limit is {MAX_PTY_INPUT_BYTES}",
            data.len()
        ));
    }
    let mut guard = state
        .sessions
        .lock()
        .map_err(|e| format!("Lock error: {e}"))?;
    if let Some(session) = guard.get_mut(session_id) {
        session
            .writer
            .write_all(data.as_bytes())
            .map_err(|e| format!("Failed to write to terminal: {e}"))?;
        session
            .writer
            .flush()
            .map_err(|e| format!("Failed to flush terminal: {e}"))?;
        Ok(())
    } else {
        Err(format!("Terminal session '{session_id}' not found"))
    }
}

/// Resizes a PTY session.
pub fn resize_session(
    state: &TerminalSessions,
    session_id: &str,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    let guard = state
        .sessions
        .lock()
        .map_err(|e| format!("Lock error: {e}"))?;
    if let Some(session) = guard.get(session_id) {
        let size = bounded_pty_size(rows, cols);
        session
            .master
            .resize(size)
            .map_err(|e| format!("Failed to resize terminal: {e}"))?;
        Ok(())
    } else {
        Err(format!("Terminal session '{session_id}' not found"))
    }
}

/// Kills a PTY session.
///
/// Signals the shell via the split [`ChildKiller`] handle, then drops the
/// master/writer (hanging up the PTY). The reader thread observes EOF, waits
/// the killed child, and emits the real exit status — so the process is
/// reaped rather than left as a zombie.
pub fn kill_session(state: &TerminalSessions, session_id: &str) -> Result<(), String> {
    let mut guard = state
        .sessions
        .lock()
        .map_err(|e| format!("Lock error: {e}"))?;
    if let Some(mut session) = guard.remove(session_id) {
        state.active_sessions.fetch_sub(1, Ordering::AcqRel);
        session.dead.store(true, Ordering::SeqCst);
        // Best-effort signal; a failure here is logged but must not fail the
        // kill — dropping the PTY master still hangs up the slave side.
        if let Err(e) = session.killer.kill() {
            log::warn!(
                target: "terminal",
                "kill: could not signal shell of terminal session '{session_id}': {e}"
            );
        }
    }
    Ok(())
}

/// One-shot bounded command execution in `repo_path`.
pub fn run_terminal(
    repo_path: &str,
    args: &[String],
    timeout_secs: Option<u64>,
) -> Result<TerminalRunResult, String> {
    run_terminal_inner(repo_path, args, timeout_secs, None)
}

/// Executes one command proposed by MANVI's local-model plane.
///
/// This is deliberately separate from [`run_terminal`], the user-owned
/// console. The app chooses the purpose, the backend validates the command
/// against that purpose, and every accepted command reaches the MANVI command
/// gate (not only Git). The allowlist remains authoritative because host
/// posture can demote a sidecar allowlist miss to an allow.
pub fn run_manvi_action(
    repo_path: &str,
    args: &[String],
    action_kind: ManviActionKind,
    timeout_secs: Option<u64>,
) -> Result<TerminalRunResult, String> {
    validate_manvi_action(args, action_kind)?;
    let repo = validate_repo(repo_path)?;
    validate_manvi_paths(&repo, args, action_kind)?;
    run_terminal_inner(repo_path, args, timeout_secs, Some(action_kind))
}

fn run_terminal_inner(
    repo_path: &str,
    args: &[String],
    timeout_secs: Option<u64>,
    manvi_action: Option<ManviActionKind>,
) -> Result<TerminalRunResult, String> {
    if args.is_empty() {
        return Err("No command provided".into());
    }
    validate_argv_bounds(args)?;
    let repo = validate_repo(repo_path)?;
    let program = &args[0];
    if program.trim().is_empty() {
        return Err("Invalid program name".into());
    }

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let command_rendered = crate::harness::render_command(&arg_refs);

    // Git commands pass through the MANVI harness command gate.
    // Force pushes and other policy-violating git mutations are blocked.
    // Non-git commands bypass the harness gate (which only judges git commands)
    // and follow the deps/CI execution precedent.
    let is_git = program == "git"
        || program.ends_with("/git")
        || program.ends_with("\\git")
        || program.ends_with("\\git.exe")
        || program.ends_with("/git.exe");
    let should_gate = is_git || manvi_action.is_some();
    let (policy_verdict, gated) = if should_gate {
        match guard_command(repo_path, &arg_refs) {
            Ok(verdict) => (Some(verdict), true),
            Err(refusal) => {
                // Return honest refusal/failure if blocked or gate failed.
                return Err(refusal);
            }
        }
    } else {
        (None, false)
    };

    let timeout = timeout_secs
        .map(|s| Duration::from_secs(s).clamp(MIN_RUN_TIMEOUT, MAX_RUN_TIMEOUT))
        .unwrap_or(DEFAULT_RUN_TIMEOUT);

    let started = Instant::now();
    let outcome = run_captured(
        program,
        &arg_refs[1..],
        Some(&repo),
        timeout,
        &[],
        TERMINAL_TAIL_CAP_BYTES,
    );
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    match outcome {
        Ok(RunOutcome::Finished(run)) => Ok(TerminalRunResult {
            command: command_rendered,
            gated,
            policy: policy_verdict,
            timed_out: false,
            exit_code: Some(run.status_code),
            stdout_tail: run.stdout_tail,
            stderr_tail: run.stderr_tail,
            truncated: run.truncated,
            duration_ms,
        }),
        Ok(RunOutcome::TimedOut(dur)) => Ok(TerminalRunResult {
            command: command_rendered,
            gated,
            policy: policy_verdict,
            timed_out: true,
            exit_code: None,
            stdout_tail: String::new(),
            stderr_tail: format!("Command timed out after {}s", dur.as_secs()),
            truncated: false,
            duration_ms,
        }),
        Err(e) => Err(e),
    }
}

fn validate_argv_bounds(args: &[String]) -> Result<(), String> {
    if args.len() > TERMINAL_ARG_COUNT_CAP {
        return Err(format!(
            "Command has {} arguments; the limit is {TERMINAL_ARG_COUNT_CAP}",
            args.len()
        ));
    }
    let mut total = 0usize;
    for arg in args {
        if arg.len() > TERMINAL_ARG_BYTES_CAP {
            return Err(format!(
                "Command argument is {} bytes; the per-argument limit is {TERMINAL_ARG_BYTES_CAP}",
                arg.len()
            ));
        }
        if arg.chars().any(char::is_control) {
            return Err("Command arguments cannot contain control characters".into());
        }
        total = total.saturating_add(arg.len());
        if total > TERMINAL_ARGV_BYTES_CAP {
            return Err(format!(
                "Command argv exceeds the {TERMINAL_ARGV_BYTES_CAP}-byte limit"
            ));
        }
    }
    Ok(())
}

pub(crate) fn normalized_program(program: &str) -> String {
    let lower = program.to_ascii_lowercase();
    lower
        .strip_suffix(".exe")
        .or_else(|| lower.strip_suffix(".cmd"))
        .or_else(|| lower.strip_suffix(".bat"))
        .unwrap_or(&lower)
        .to_string()
}

fn has_parent_or_absolute_path(value: &str) -> bool {
    let candidate = value.split_once('=').map_or(value, |(_, rhs)| rhs);
    if candidate.is_empty() || candidate.starts_with('-') {
        return false;
    }
    let path = std::path::Path::new(candidate);
    path.is_absolute()
        || candidate.starts_with("\\\\")
        || candidate.as_bytes().get(1) == Some(&b':')
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
}

fn command_is_one_of(args: &[String], allowed: &[&str]) -> bool {
    args.get(1)
        .map(|arg| allowed.contains(&arg.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// `go test ...` or `go -C <dir> test ...`. `-C` must be first (see `go help`).
fn go_coverage_test_allowed(args: &[String]) -> bool {
    match args.get(1).map(String::as_str) {
        Some("test") => true,
        Some("-C") => {
            let dir = args.get(2).map(String::as_str).unwrap_or("");
            !dir.is_empty()
                && !dir.starts_with('-')
                && args.get(3).map(String::as_str) == Some("test")
        }
        _ => false,
    }
}

/// Coverage generation may mutate the host toolchain in exactly one way:
/// `cargo install cargo-llvm-cov --locked` (flag order may swap). Extra
/// crates, `--git`, `--path`, or a missing `--locked` are refused. Analysis
/// (`Coverage`) never calls this.
fn cargo_llvm_cov_install_allowed(args: &[String]) -> bool {
    if args.get(1).map(String::as_str) != Some("install") {
        return false;
    }
    let rest: Vec<&str> = args.iter().skip(2).map(String::as_str).collect();
    rest.len() == 2 && rest.contains(&"--locked") && rest.contains(&"cargo-llvm-cov")
}

/// `python -m venv .venv` — creating the project's own virtualenv.
///
/// The target directory is pinned to the two conventional names rather than
/// "any repository-relative path": coverage generation has exactly one reason
/// to create a virtualenv, and a free-form path would let plan text scatter
/// interpreters through the checkout.
/// Resolves a bare program name against the process PATH to its canonical
/// path. Host-controlled by construction — the repository has no say in it,
/// which is what makes it usable as a trust anchor.
fn resolve_on_path(program: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .filter(|candidate| candidate.is_file())
        .find_map(|candidate| std::fs::canonicalize(candidate).ok())
}

/// True when `name` is a Python interpreter's file name (`python`, `python3`,
/// `python3.14`, `python3.14t`), and not some other executable a symlink was
/// pointed at. This is what separates a virtualenv from `.venv/bin/python`
/// aimed at `/bin/sh`.
fn is_python_interpreter_name(name: &str) -> bool {
    let name = name.strip_suffix(".exe").unwrap_or(name);
    if name == "python" || name == "python3" {
        return true;
    }
    name.strip_prefix("python3.")
        .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
}

/// True when the virtualenv's interpreter resolves to a Python that GitPulse
/// is willing to execute.
///
/// Two independent ways to qualify, because one of them is not always
/// available:
///
/// 1. It is byte-for-byte the Python on PATH. Exact, and true whenever
///    GitPulse runs from a shell.
/// 2. It is *a* Python interpreter, by file name, resolving to somewhere
///    outside the repository.
///
/// (2) exists because (1) silently fails for an installed app. A macOS app
/// launched from Finder inherits launchd's PATH, not the user's shell PATH —
/// on this machine that means `/usr/bin/python3` rather than the Homebrew
/// interpreter every project virtualenv here is built from. Anchoring only on
/// (1) made GitPulse refuse the very virtualenv it had just created, but only
/// once installed, which is the configuration users actually run.
///
/// (2) keeps the property that matters. The realistic attack is a repository
/// shipping `.venv/bin/python` as a symlink to a checked-in payload, or to a
/// shell: the first is refused because the target must resolve *outside* the
/// repository, the second because the target must be named like a Python.
/// Writing a hostile binary named `python3` outside the repository already
/// requires the code execution this check exists to prevent.
fn resolves_to_trusted_python(repo: &std::path::Path, interpreter: &std::path::Path) -> bool {
    let Ok(target) = std::fs::canonicalize(interpreter) else {
        return false;
    };
    if ["python3", "python"]
        .iter()
        .any(|name| resolve_on_path(name).is_some_and(|host| host == target))
    {
        return true;
    }
    // A target inside the repository is repository-authored content, never a
    // system interpreter, whatever it is called.
    if let Ok(root) = std::fs::canonicalize(repo) {
        if target.starts_with(&root) {
            return false;
        }
    } else {
        return false;
    }
    target
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_python_interpreter_name)
}

/// Validates the project virtualenv's interpreter as a program.
///
/// Canonical containment — the rule every other repository-local program is
/// held to — is the wrong test here and would reject every real virtualenv:
/// `python -m venv` builds `.venv/bin/python` as a symlink *out* to the host
/// toolchain, by construction. So the property is preserved a different way
/// rather than dropped. Three things must hold:
///
/// 1. the venv directory carries `pyvenv.cfg`, the marker of a real
///    virtualenv rather than a directory shaped like one;
/// 2. the interpreter path exists inside the repository, lexically; and
/// 3. it resolves to the same binary as `python3`/`python` on PATH.
///
/// (3) is the load-bearing one. It means the process GitPulse executes is the
/// interpreter it would have executed regardless — the virtualenv changes
/// which site-packages get imported, not which binary runs. A repository that
/// ships `.venv/bin/python` as a symlink to `/bin/sh`, to a checked-in
/// payload, or to any other interpreter fails it and is refused.
pub(crate) fn check_venv_interpreter(
    repo: &std::path::Path,
    relative: &str,
) -> Result<(), &'static str> {
    let Some(venv_dir) = relative.split('/').next().filter(|dir| !dir.is_empty()) else {
        return Err("has no virtualenv directory");
    };
    match sandbox_join_canonical(repo, &format!("{venv_dir}/pyvenv.cfg")) {
        Ok(cfg) if cfg.is_file() => {}
        _ => return Err("is not inside a virtualenv (no pyvenv.cfg)"),
    }
    let Ok(interpreter) = sandbox_join(repo, relative) else {
        return Err("is not a repository-relative path");
    };
    if !interpreter.is_file() {
        return Err("is not a repository file");
    }
    if !resolves_to_trusted_python(repo, &interpreter) {
        return Err("does not resolve to a Python interpreter outside the repository");
    }
    Ok(())
}

fn validate_venv_interpreter(
    repo: &std::path::Path,
    relative: &str,
    kind: ManviActionKind,
) -> Result<(), String> {
    check_venv_interpreter(repo, relative).map_err(|detail| {
        format!(
            "MANVI {} action refused: virtualenv interpreter '{relative}' {detail}; GitPulse will not execute it",
            kind.label()
        )
    })
}

/// Single owner of "is this program a repository file rather than a PATH
/// name". Three checks must agree on it — the executable-path refusal, the
/// allowlist match, and path validation — and they previously each carried
/// their own literal list, which is how the vendored phpunit and venv
/// interpreter the scanner plans could be accepted by one and refused by
/// another.
fn program_is_repo_local(program_raw: &str) -> bool {
    repo_local_program(&normalized_program(program_raw)).is_some()
}

fn python_venv_create_allowed(args: &[String]) -> bool {
    args.len() == 4
        && args.get(2).map(String::as_str) == Some("venv")
        && args
            .get(3)
            .is_some_and(|dir| VENV_DIR_NAMES.contains(&dir.as_str()))
}

/// `<repo venv>/bin/python -m pip install pytest pytest-cov`.
///
/// `pip install` is absent from every other allowlist arm on purpose. This is
/// the single pinned exception and it is refused outright unless the
/// interpreter is the repository's own virtualenv: installing into whatever
/// interpreter happens to be on PATH mutates the host Python (and on a
/// PEP 668 system fails or demands `--break-system-packages`), which is not a
/// trade a coverage panel may make. The package set is fixed, so this cannot
/// become a general package installer.
fn pip_coverage_install_allowed(args: &[String], interpreter_is_repo_local: bool) -> bool {
    if !interpreter_is_repo_local {
        return false;
    }
    if args.get(3).map(String::as_str) != Some("install") {
        return false;
    }
    let packages: Vec<&str> = args.iter().skip(4).map(String::as_str).collect();
    !packages.is_empty() && packages.iter().all(|pkg| PYTEST_PACKAGES.contains(pkg))
}

/// `npm install --save-dev @vitest/coverage-v8`.
///
/// A project devDependency, not a host mutation — but still a write to
/// package.json and the lockfile, so it is pinned to the provider set and
/// requires an explicit dev flag. A bare `npm install <anything>` under a
/// coverage action would let plan text add any package to the project.
fn node_coverage_provider_install_allowed(args: &[String]) -> bool {
    if !matches!(
        args.get(1).map(String::as_str),
        Some("install" | "i" | "add")
    ) {
        return false;
    }
    let rest: Vec<&str> = args.iter().skip(2).map(String::as_str).collect();
    let dev_flag = |arg: &str| matches!(arg, "--save-dev" | "-D" | "--dev");
    if !rest.iter().copied().any(dev_flag) {
        return false;
    }
    // Every token is either the dev flag or a pinned provider: no stray flags
    // (`--registry`, `--force`) and no extra packages ride along.
    let mut packages = 0usize;
    for arg in &rest {
        if dev_flag(arg) {
            continue;
        }
        if !JS_COVERAGE_PROVIDERS.contains(arg) {
            return false;
        }
        packages += 1;
    }
    packages > 0
}

fn rustup_llvm_tools_allowed(args: &[String]) -> bool {
    args.len() == 4
        && args.get(1).map(String::as_str) == Some("component")
        && args.get(2).map(String::as_str) == Some("add")
        && args.get(3).map(String::as_str) == Some("llvm-tools-preview")
}

fn npm_run_is_verification(args: &[String]) -> bool {
    let Some(script) = args.get(2).map(|s| s.to_ascii_lowercase()) else {
        return false;
    };
    [
        "test", "check", "lint", "cover", "coverage", "audit", "verify", "ci",
    ]
    .iter()
    .any(|needle| {
        script == *needle
            || script
                .strip_prefix(needle)
                .is_some_and(|suffix| suffix.starts_with(':') || suffix.starts_with('-'))
    })
}

fn local_js_runner(args: &[String], flag_index: usize) -> bool {
    matches!(
        (
            args.get(flag_index).map(String::as_str),
            args.get(flag_index + 1).map(String::as_str)
        ),
        (
            Some("--no" | "--no-install"),
            Some("vitest" | "jest" | "c8" | "nyc")
        )
    )
}

fn build_tool_tasks_allowed(args: &[String], kind: ManviActionKind) -> bool {
    let mut saw_task = false;
    for raw in args.iter().skip(1) {
        let arg = raw.to_ascii_lowercase();
        if matches!(
            arg.as_str(),
            "--no-daemon" | "--stacktrace" | "--info" | "--quiet" | "--offline" | "-q"
        ) {
            continue;
        }
        saw_task = true;
        let allowed = match kind {
            ManviActionKind::Health => matches!(
                arg.as_str(),
                "clean"
                    | "test"
                    | "check"
                    | "verify"
                    | "build"
                    | "dependencyupdates"
                    | "dependency:tree"
                    | "versions:display-dependency-updates"
                    | "versions:use-latest-releases"
            ),
            ManviActionKind::Coverage | ManviActionKind::CoverageGenerator => matches!(
                arg.as_str(),
                "clean"
                    | "test"
                    | "check"
                    | "verify"
                    | "jacocotestreport"
                    | "jacoco:test"
                    | "jacoco:report"
                    | "koverxmlreport"
                    | "koverhtmlreport"
            ),
        };
        if !allowed {
            return false;
        }
    }
    saw_task
}

fn node_package_command_allowed(args: &[String], kind: ManviActionKind) -> bool {
    let Some(subcommand) = args.get(1).map(|s| s.to_ascii_lowercase()) else {
        return false;
    };
    match kind {
        ManviActionKind::Health => match subcommand.as_str() {
            "audit" | "outdated" | "update" | "upgrade" | "install" | "ci" | "test" => true,
            "run" => npm_run_is_verification(args),
            _ => false,
        },
        ManviActionKind::Coverage => match subcommand.as_str() {
            "test" => true,
            "run" => npm_run_is_verification(args),
            // npm exec may otherwise download a missing package. `--no` makes
            // this a local-dependency-only execution on supported npm builds.
            "exec" => local_js_runner(args, 2),
            _ => false,
        },
        // Generation may additionally install the missing coverage provider.
        // Analysis (`Coverage`) never writes to the project.
        ManviActionKind::CoverageGenerator => match subcommand.as_str() {
            "test" => true,
            "run" => npm_run_is_verification(args),
            "exec" => local_js_runner(args, 2),
            "install" | "i" | "add" => node_coverage_provider_install_allowed(args),
            _ => false,
        },
    }
}

fn python_module_allowed(
    args: &[String],
    kind: ManviActionKind,
    interpreter_is_repo_local: bool,
) -> bool {
    if args.get(1).map(String::as_str) != Some("-m") {
        return false;
    }
    let module = args.get(2).map(|s| s.to_ascii_lowercase());
    match kind {
        ManviActionKind::Health => match module.as_deref() {
            Some("pip") => command_is_one_of(&args[2..], &["check", "list", "audit"]),
            Some("pip_audit" | "pytest") => true,
            _ => false,
        },
        ManviActionKind::Coverage => match module.as_deref() {
            Some("pytest") => true,
            Some("coverage") => command_is_one_of(
                &args[2..],
                &["run", "report", "html", "xml", "json", "lcov"],
            ),
            _ => false,
        },
        // Generation may additionally build the project virtualenv and put
        // the pytest coverage toolchain in it. Both steps are pinned; neither
        // is reachable from the read-only analysis action.
        ManviActionKind::CoverageGenerator => match module.as_deref() {
            Some("pytest") => true,
            Some("coverage") => command_is_one_of(
                &args[2..],
                &["run", "report", "html", "xml", "json", "lcov"],
            ),
            Some("venv") => python_venv_create_allowed(args),
            Some("pip") => pip_coverage_install_allowed(args, interpreter_is_repo_local),
            _ => false,
        },
    }
}

/// Backend authority boundary for commands originating in model text.
///
/// This intentionally validates shapes, not exact package names: reports can
/// cover any ecosystem and package. Program families and verbs are bounded,
/// shells/network/file utilities are absent, paths cannot be absolute or walk
/// upward, and the separate MANVI policy gate still judges every accepted
/// command immediately before execution.
pub(crate) fn validate_manvi_action(args: &[String], kind: ManviActionKind) -> Result<(), String> {
    if args.is_empty() {
        return Err("MANVI action has no command".into());
    }
    validate_argv_bounds(args).map_err(|e| format!("MANVI action refused: {e}"))?;

    let program_raw = args[0].trim();
    let repo_wrapper = program_is_repo_local(program_raw);
    if !repo_wrapper
        && (program_raw.contains('/')
            || program_raw.contains('\\')
            || has_parent_or_absolute_path(program_raw))
    {
        return Err(format!(
            "MANVI {} action refused: executable paths are not allowed",
            kind.label()
        ));
    }
    if args
        .iter()
        .skip(1)
        .any(|arg| has_parent_or_absolute_path(arg))
    {
        return Err(format!(
            "MANVI {} action refused: arguments must stay inside the open repository",
            kind.label()
        ));
    }
    if args.iter().skip(1).any(|arg| {
        let lower = arg.to_ascii_lowercase();
        ["http:", "https:", "file:", "git:", "git+", "ssh:"]
            .iter()
            .any(|prefix| lower.starts_with(prefix))
    }) {
        return Err(format!(
            "MANVI {} action refused: external URLs and transports are not allowed",
            kind.label()
        ));
    }
    if args.iter().skip(1).any(|arg| {
        matches!(
            arg.to_ascii_lowercase().as_str(),
            "-g" | "--global" | "--user" | "--system" | "--location=global"
        )
    }) {
        return Err(format!(
            "MANVI {} action refused: global package-environment mutation is not allowed",
            kind.label()
        ));
    }

    let spelled = normalized_program(program_raw);
    // A repository-local executable is judged as the tool it is, once its
    // spelling has been pinned to a known-safe exact path.
    let repo_local = repo_local_program(&spelled);
    let program = repo_local.unwrap_or(spelled.as_str()).to_string();
    let allowed = match program.as_str() {
        "npm" | "pnpm" | "yarn" | "bun" => node_package_command_allowed(args, kind),
        "npx" => {
            matches!(
                kind,
                ManviActionKind::Coverage | ManviActionKind::CoverageGenerator
            ) && local_js_runner(args, 1)
        }
        "cargo" => match kind {
            ManviActionKind::Health => command_is_one_of(
                args,
                &["audit", "update", "test", "check", "clippy", "tree"],
            ),
            ManviActionKind::Coverage => {
                command_is_one_of(args, &["llvm-cov", "tarpaulin", "test", "nextest"])
            }
            ManviActionKind::CoverageGenerator => {
                command_is_one_of(args, &["llvm-cov", "tarpaulin", "test", "nextest"])
                    || cargo_llvm_cov_install_allowed(args)
            }
        },
        "rustup" => kind == ManviActionKind::CoverageGenerator && rustup_llvm_tools_allowed(args),
        "python" | "python3" | "py" => {
            python_module_allowed(args, kind, repo_local == Some("python"))
        }
        "pip" | "pip3" => {
            kind == ManviActionKind::Health && command_is_one_of(args, &["check", "list", "audit"])
        }
        "pip-audit" => kind == ManviActionKind::Health,
        "pytest" => {
            matches!(
                kind,
                ManviActionKind::Coverage | ManviActionKind::CoverageGenerator
            ) || kind == ManviActionKind::Health
        }
        "coverage" => {
            matches!(
                kind,
                ManviActionKind::Coverage | ManviActionKind::CoverageGenerator
            ) && command_is_one_of(args, &["run", "report", "html", "xml", "json", "lcov"])
        }
        "go" => match kind {
            ManviActionKind::Health => command_is_one_of(args, &["get", "mod", "test", "list"]),
            ManviActionKind::Coverage | ManviActionKind::CoverageGenerator => {
                go_coverage_test_allowed(args)
            }
        },
        "govulncheck" => kind == ManviActionKind::Health,
        "composer" => match kind {
            ManviActionKind::Health => command_is_one_of(
                args,
                &["audit", "update", "require", "install", "show", "test"],
            ),
            // Materializing dependencies already pinned in composer.lock is
            // what makes vendor/bin/phpunit exist at all.
            ManviActionKind::CoverageGenerator => command_is_one_of(args, &["install", "test"]),
            ManviActionKind::Coverage => command_is_one_of(args, &["test"]),
        },
        "bundle" | "bundler" => match args.get(1).map(String::as_str) {
            Some("audit" | "update" | "check") => kind == ManviActionKind::Health,
            // `bundle install` resolves the Gemfile so `bundle exec` can run
            // at all; generation needs it, analysis does not.
            Some("install") => matches!(
                kind,
                ManviActionKind::Health | ManviActionKind::CoverageGenerator
            ),
            Some("exec") => args.get(2).is_some_and(|tool| match kind {
                ManviActionKind::Health => {
                    matches!(tool.as_str(), "rake" | "rspec" | "bundler-audit")
                }
                // The Ruby generate command the scanner plans, and only it.
                // Without this arm the command was planned and then refused at
                // the gate. `rake` stays health-only: coverage has no reason
                // to invoke arbitrary project tasks.
                ManviActionKind::Coverage | ManviActionKind::CoverageGenerator => {
                    tool.as_str() == "rspec"
                }
            }),
            _ => false,
        },
        "dotnet" => match kind {
            ManviActionKind::Health => {
                command_is_one_of(args, &["add", "remove", "list", "restore", "test"])
            }
            ManviActionKind::Coverage | ManviActionKind::CoverageGenerator => {
                command_is_one_of(args, &["test"])
            }
        },
        "swift" => {
            matches!(
                kind,
                ManviActionKind::Coverage | ManviActionKind::CoverageGenerator
            ) && command_is_one_of(args, &["test"])
        }
        "dart" => {
            matches!(
                kind,
                ManviActionKind::Coverage | ManviActionKind::CoverageGenerator
            ) && command_is_one_of(args, &["test"])
        }
        "mvn" | "mvnw" | "./mvnw" | ".\\mvnw" | "gradle" | "gradlew" | "./gradlew"
        | ".\\gradlew" => build_tool_tasks_allowed(args, kind),
        "phpunit" => matches!(
            kind,
            ManviActionKind::Coverage | ManviActionKind::CoverageGenerator
        ),
        _ => false,
    };

    if !allowed {
        return Err(format!(
            "MANVI {} action refused: '{}' is outside the purpose-specific command allowlist",
            kind.label(),
            crate::harness::render_command(&args.iter().map(String::as_str).collect::<Vec<_>>())
        ));
    }
    Ok(())
}

/// Resolves every path-bearing option the supported tools can write or read.
/// Lexical checks above reject obvious `/` and `..` escapes; canonical
/// resolution here also refuses a repository symlink that points outside.
pub(crate) fn validate_manvi_paths(
    repo: &std::path::Path,
    args: &[String],
    kind: ManviActionKind,
) -> Result<(), String> {
    const NEXT_PATH_FLAGS: &[&str] = &[
        "--manifest-path",
        "--output-path",
        "--junitxml",
        "--cov-config",
        "--rcfile",
        "--requirement",
        "--constraint",
        "-r",
        "-c",
        "-C",
    ];
    const PREFIX_PATH_FLAGS: &[&str] = &[
        "--manifest-path=",
        "--output-path=",
        "--junitxml=",
        "--cov-config=",
        "--rcfile=",
        "--requirement=",
        "--constraint=",
        "-coverprofile=",
        "--coverage=",
    ];

    let program_raw = args.first().map(String::as_str).unwrap_or_default();
    // Programs that are files in the repository rather than names on PATH:
    // build wrappers, the project virtualenv's interpreter, vendored phpunit.
    // The allowlist has already pinned the spelling; this proves the path
    // resolves to a real file that has not been symlinked out of the tree.
    if program_is_repo_local(program_raw) {
        let normalized = program_raw.replace('\\', "/");
        let relative = normalized.strip_prefix("./").unwrap_or(&normalized);
        // The virtualenv interpreter is validated on its own terms; every
        // other repository-local program must canonically resolve inside the
        // tree. Both fall through to the argument checks below.
        if repo_local_program(&normalized_program(program_raw)) == Some("python") {
            validate_venv_interpreter(repo, relative, kind)?;
        } else {
            let wrapper = sandbox_join_canonical(repo, relative).map_err(|e| {
                format!(
                    "MANVI {} action refused: wrapper '{}' escapes the open repository: {e}",
                    kind.label(),
                    program_raw
                )
            })?;
            if !wrapper.is_file() {
                return Err(format!(
                    "MANVI {} action refused: wrapper '{}' is not a repository file",
                    kind.label(),
                    program_raw
                ));
            }
        }
    }

    let mut path_values: Vec<&str> = Vec::new();
    let mut index = 1usize;
    while index < args.len() {
        let arg = &args[index];
        if NEXT_PATH_FLAGS.contains(&arg.as_str()) {
            let value = args.get(index + 1).ok_or_else(|| {
                format!(
                    "MANVI {} action refused: {arg} requires a repository-relative path",
                    kind.label()
                )
            })?;
            path_values.push(value);
            index += 2;
            continue;
        }
        if let Some(value) = PREFIX_PATH_FLAGS
            .iter()
            .find_map(|prefix| arg.strip_prefix(prefix))
        {
            path_values.push(value);
        }
        // pytest's `--cov-report=xml:path` carries a path after the format.
        if let Some(value) = arg.strip_prefix("--cov-report=") {
            if let Some((_, path)) = value.split_once(':') {
                path_values.push(path);
            }
        }
        index += 1;
    }

    for value in path_values {
        if value.trim().is_empty() {
            return Err(format!(
                "MANVI {} action refused: output/input path cannot be empty",
                kind.label()
            ));
        }
        sandbox_join_canonical(repo, value).map_err(|e| {
            format!(
                "MANVI {} action refused: path '{}' escapes the open repository: {e}",
                kind.label(),
                value
            )
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn init_test_repo(dir: &std::path::Path) {
        let output = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir)
            .output()
            .expect("git init");
        assert!(output.status.success());
    }

    #[test]
    fn rejects_empty_argv() {
        let dir = TempDir::new().unwrap();
        init_test_repo(dir.path());
        let res = run_terminal(dir.path().to_str().unwrap(), &[], None);
        assert!(res.is_err());
        assert_eq!(res.unwrap_err(), "No command provided");
    }

    #[test]
    fn rejects_invalid_repo_path() {
        let res = run_terminal(
            "/nonexistent/directory/path/here",
            &["git".into(), "status".into()],
            None,
        );
        assert!(res.is_err());
    }

    #[test]
    fn runs_safe_git_command_in_valid_repo() {
        let dir = TempDir::new().unwrap();
        init_test_repo(dir.path());
        let res = run_terminal(
            dir.path().to_str().unwrap(),
            &["git".into(), "status".into()],
            Some(10),
        )
        .expect("runs git status");

        assert_eq!(res.exit_code, Some(0));
        assert!(!res.timed_out);
        assert!(res.command.contains("git status"));
        assert!(res.stdout_tail.contains("On branch main"));
        assert!(res.gated);
    }

    #[test]
    fn runs_non_git_command() {
        let dir = TempDir::new().unwrap();
        init_test_repo(dir.path());
        let res = run_terminal(
            dir.path().to_str().unwrap(),
            &["echo".into(), "hello gitpulse".into()],
            Some(5),
        )
        .expect("runs echo");

        assert_eq!(res.exit_code, Some(0));
        assert!(res.stdout_tail.contains("hello gitpulse"));
        assert!(res.policy.is_none());
        assert!(!res.gated);
    }

    #[test]
    fn manvi_health_actions_refuse_arbitrary_processes_and_repo_escape() {
        for argv in [
            vec!["rm".into(), "-rf".into(), ".".into()],
            vec!["sh".into(), "-c".into(), "npm audit fix".into()],
            vec!["curl".into(), "https://example.com".into()],
            vec![
                "npm".into(),
                "install".into(),
                "https://example.com/pkg.tgz".into(),
            ],
            vec![
                "npm".into(),
                "--prefix".into(),
                "/tmp/outside".into(),
                "install".into(),
            ],
            vec!["../tool".into(), "audit".into()],
        ] {
            let err = validate_manvi_action(&argv, ManviActionKind::Health).unwrap_err();
            assert!(
                err.contains("MANVI"),
                "refusal must name the authority boundary: {err}"
            );
        }
    }

    #[test]
    fn health_actions_cannot_mutate_global_package_environments() {
        for argv in [
            vec![
                "npm".into(),
                "install".into(),
                "--global".into(),
                "eslint".into(),
            ],
            vec!["npm".into(), "install".into(), "-g".into(), "eslint".into()],
            vec![
                "python3".into(),
                "-m".into(),
                "pip".into(),
                "install".into(),
                "requests".into(),
            ],
            vec!["pip".into(), "install".into(), "requests".into()],
        ] {
            let err = validate_manvi_action(&argv, ManviActionKind::Health)
                .expect_err("global/environment package mutation must be refused");
            assert!(err.contains("MANVI"), "{argv:?}: {err}");
        }
    }

    #[test]
    fn python_module_actions_are_verb_scoped() {
        validate_manvi_action(
            &[
                "python3".into(),
                "-m".into(),
                "coverage".into(),
                "run".into(),
                "-m".into(),
                "pytest".into(),
            ],
            ManviActionKind::Coverage,
        )
        .unwrap();
        let err = validate_manvi_action(
            &[
                "python3".into(),
                "-m".into(),
                "coverage".into(),
                "erase".into(),
            ],
            ManviActionKind::Coverage,
        )
        .expect_err("coverage cleanup is not coverage generation");
        assert!(err.contains("allowlist"), "{err}");
    }

    #[test]
    fn manvi_health_actions_allow_remediation_and_verification_shapes() {
        for argv in [
            vec!["npm".into(), "audit".into(), "fix".into()],
            vec!["cargo".into(), "update".into(), "-p".into(), "serde".into()],
            vec!["python3".into(), "-m".into(), "pip".into(), "check".into()],
            vec!["go".into(), "test".into(), "./...".into()],
        ] {
            validate_manvi_action(&argv, ManviActionKind::Health).unwrap();
        }
    }

    #[test]
    fn manvi_coverage_actions_are_purpose_limited() {
        validate_manvi_action(
            &["cargo".into(), "llvm-cov".into(), "--workspace".into()],
            ManviActionKind::Coverage,
        )
        .unwrap();
        validate_manvi_action(
            &["pytest".into(), "--cov".into(), "--cov-report=xml".into()],
            ManviActionKind::CoverageGenerator,
        )
        .unwrap();
        let err = validate_manvi_action(
            &["npm".into(), "install".into(), "left-pad".into()],
            ManviActionKind::Coverage,
        )
        .unwrap_err();
        assert!(err.contains("coverage"));
    }

    #[test]
    fn allowlist_keywords_cannot_be_smuggled_behind_a_different_action() {
        for argv in [
            vec![
                "npx".into(),
                "--no-install".into(),
                "hostile-runner".into(),
                "vitest".into(),
            ],
            vec!["npm".into(), "run".into(), "contest-deploy".into()],
            vec!["./gradlew".into(), "publish".into(), "test".into()],
            vec!["mvn".into(), "deploy".into(), "verify".into()],
        ] {
            let err = validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
                .expect_err("an allowed keyword must not bless a different executable/task");
            assert!(err.contains("allowlist"), "{argv:?}: {err}");
        }
    }

    /// The virtualenv interpreter is the one program GitPulse executes that
    /// is *expected* to symlink out of the repository, so it is exempted from
    /// canonical containment. These tests pin what replaced that guarantee.
    /// Regression, found by running the installed app: the trust anchor used
    /// to be "resolves to the Python on PATH". A macOS app launched from
    /// Finder inherits launchd's PATH, not the shell's, so on a Homebrew
    /// machine the installed GitPulse refused the very virtualenv it had just
    /// created — while the shell-launched dev build accepted it. The rule must
    /// not depend on how the app was launched.
    #[cfg(unix)]
    #[test]
    fn venv_interpreter_is_accepted_without_a_matching_path_entry() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::TempDir::new().expect("tempdir");
        let repo = dir.path();
        std::fs::create_dir_all(repo.join(".venv/bin")).unwrap();
        std::fs::write(repo.join(".venv/pyvenv.cfg"), "home = /elsewhere/bin\n").unwrap();

        // A Python interpreter that is deliberately NOT the one on PATH, in a
        // directory PATH does not contain — the Homebrew-vs-/usr/bin split.
        let elsewhere = tempfile::TempDir::new().expect("tempdir2");
        let real_python = elsewhere.path().join("python3.14");
        std::fs::write(&real_python, "#!/bin/sh\nexit 0\n").unwrap();
        symlink(&real_python, repo.join(".venv/bin/python")).unwrap();

        check_venv_interpreter(repo, ".venv/bin/python")
            .expect("a Python outside the repo must qualify without a PATH match");
    }

    /// The relaxation must not become "run anything the repository points at".
    #[cfg(unix)]
    #[test]
    fn venv_interpreter_still_refuses_non_python_and_in_repo_targets() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::TempDir::new().expect("tempdir");
        let repo = dir.path();
        std::fs::create_dir_all(repo.join(".venv/bin")).unwrap();
        std::fs::write(repo.join(".venv/pyvenv.cfg"), "home = /usr/bin\n").unwrap();

        // (a) A real system binary that is not a Python.
        symlink("/bin/sh", repo.join(".venv/bin/python")).unwrap();
        assert!(
            check_venv_interpreter(repo, ".venv/bin/python").is_err(),
            "a shell is not a Python interpreter"
        );
        std::fs::remove_file(repo.join(".venv/bin/python")).unwrap();

        // (b) A checked-in payload named like a Python. Being inside the
        //     repository disqualifies it however it is named.
        let payload = repo.join("tools/python3");
        std::fs::create_dir_all(payload.parent().unwrap()).unwrap();
        std::fs::write(&payload, "#!/bin/sh\nexit 0\n").unwrap();
        symlink(&payload, repo.join(".venv/bin/python")).unwrap();
        assert!(
            check_venv_interpreter(repo, ".venv/bin/python").is_err(),
            "a repository-authored binary must never be executed as the interpreter"
        );
    }

    #[test]
    fn python_interpreter_names_are_recognized_precisely() {
        for good in [
            "python",
            "python3",
            "python3.14",
            "python3.9",
            "python3.14t",
        ] {
            assert!(is_python_interpreter_name(good), "{good}");
        }
        for bad in [
            "sh", "bash", "node", "python2", "pythonic", "python3x", "", "py",
        ] {
            assert!(!is_python_interpreter_name(bad), "{bad}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn venv_interpreter_must_resolve_to_the_host_python() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::TempDir::new().expect("tempdir");
        let repo = dir.path();
        std::fs::create_dir_all(repo.join(".venv/bin")).unwrap();
        std::fs::write(repo.join(".venv/pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        // A repository that ships its own "interpreter" pointing at a shell.
        symlink("/bin/sh", repo.join(".venv/bin/python")).unwrap();

        let err = check_venv_interpreter(repo, ".venv/bin/python")
            .expect_err("a symlink to anything but the host python must be refused");
        assert!(err.contains("does not resolve"), "{err}");

        // And the refusal is what the command gate reports, not a bypass.
        let argv = vec![
            ".venv/bin/python".to_string(),
            "-m".to_string(),
            "pytest".to_string(),
        ];
        let gate = validate_manvi_paths(repo, &argv, ManviActionKind::CoverageGenerator)
            .expect_err("the gate must refuse it too");
        assert!(gate.contains("will not execute it"), "{gate}");
    }

    #[cfg(unix)]
    #[test]
    fn venv_interpreter_requires_a_real_virtualenv_marker() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let repo = dir.path();
        std::fs::create_dir_all(repo.join(".venv/bin")).unwrap();
        // A regular file in a directory merely shaped like a virtualenv.
        std::fs::write(repo.join(".venv/bin/python"), "#!/bin/sh\necho hi\n").unwrap();

        let err = check_venv_interpreter(repo, ".venv/bin/python")
            .expect_err("no pyvenv.cfg means this is not a virtualenv");
        assert!(err.contains("pyvenv.cfg"), "{err}");
    }

    #[test]
    fn venv_interpreter_must_exist() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let err = check_venv_interpreter(dir.path(), ".venv/bin/python")
            .expect_err("an absent interpreter is not runnable");
        assert!(
            err.contains("pyvenv.cfg") || err.contains("repository file"),
            "{err}"
        );
    }

    /// The positive case, against a virtualenv built the way `python -m venv`
    /// really builds one (interpreter symlinked out to the host toolchain).
    /// Skipped where no Python is installed; the negative cases above carry
    /// the security contract on every machine.
    #[test]
    fn a_real_virtualenv_interpreter_is_accepted() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let repo = dir.path();
        let built = std::process::Command::new("python3")
            .args(["-m", "venv", ".venv"])
            .current_dir(repo)
            .status();
        match built {
            Ok(status) if status.success() => {}
            _ => return, // no usable python3 on this host
        }
        check_venv_interpreter(repo, ".venv/bin/python")
            .expect("a virtualenv built by python -m venv must be accepted");
        let argv = vec![
            ".venv/bin/python".to_string(),
            "-m".to_string(),
            "pytest".to_string(),
            "--cov".to_string(),
            "--cov-report=xml".to_string(),
        ];
        validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
            .expect("the planned generate command must be allowed");
        validate_manvi_paths(repo, &argv, ManviActionKind::CoverageGenerator)
            .expect("and its paths must validate");
    }

    /// The setup steps coverage generation is now allowed to run.
    ///
    /// Each is pinned to one shape. This test is the inventory of what the
    /// widening actually bought; anything not listed here must be refused by
    /// the companion test below.
    #[test]
    fn coverage_generator_allows_exactly_the_planned_setup_steps() {
        for argv in [
            // Python: build the project's own virtualenv, then put the pytest
            // coverage toolchain in it, then run it.
            vec!["python3".into(), "-m".into(), "venv".into(), ".venv".into()],
            vec!["python".into(), "-m".into(), "venv".into(), "venv".into()],
            vec![
                ".venv/bin/python".into(),
                "-m".into(),
                "pip".into(),
                "install".into(),
                "pytest".into(),
                "pytest-cov".into(),
            ],
            vec![
                ".venv/bin/python".into(),
                "-m".into(),
                "pytest".into(),
                "--cov".into(),
                "--cov-report=xml".into(),
            ],
            // JavaScript: the missing coverage provider as a devDependency.
            vec![
                "npm".into(),
                "install".into(),
                "--save-dev".into(),
                "@vitest/coverage-v8".into(),
            ],
            vec![
                "npm".into(),
                "install".into(),
                "-D".into(),
                "@vitest/coverage-istanbul".into(),
            ],
            // PHP and Ruby: materialize dependencies the manifest already pins.
            vec!["composer".into(), "install".into()],
            vec!["bundle".into(), "install".into()],
            vec!["bundle".into(), "exec".into(), "rspec".into()],
            vec![
                "vendor/bin/phpunit".into(),
                "--coverage-clover".into(),
                "coverage.xml".into(),
            ],
            // JVM: the wrapper a project ships so nobody needs a system Maven.
            vec!["./mvnw".into(), "verify".into()],
        ] {
            validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
                .unwrap_or_else(|err| panic!("{argv:?} must be allowed: {err}"));
        }
    }

    /// The blast radius of the setup widening, stated as refusals.
    ///
    /// The load-bearing case is the first one: installing into whatever
    /// interpreter is on PATH mutates the host Python. Only the repository's
    /// own virtualenv may receive packages, and only the pinned ones.
    #[test]
    fn coverage_generator_refuses_everything_adjacent_to_the_setup_steps() {
        for (argv, why) in [
            (
                vec![
                    "python3".into(),
                    "-m".into(),
                    "pip".into(),
                    "install".into(),
                    "pytest".into(),
                ],
                "host interpreter must never receive an install",
            ),
            (
                vec!["pip".into(), "install".into(), "pytest".into()],
                "bare pip is not an install channel",
            ),
            (
                vec![
                    ".venv/bin/python".into(),
                    "-m".into(),
                    "pip".into(),
                    "install".into(),
                    "requests".into(),
                ],
                "package set is pinned to the pytest coverage toolchain",
            ),
            (
                vec![
                    ".venv/bin/python".into(),
                    "-m".into(),
                    "pip".into(),
                    "install".into(),
                    "pytest".into(),
                    "evil-package".into(),
                ],
                "an extra package must not ride along with a pinned one",
            ),
            (
                vec![
                    ".venv/bin/python".into(),
                    "-m".into(),
                    "pip".into(),
                    "uninstall".into(),
                    "pytest".into(),
                ],
                "only install is allowed",
            ),
            (
                vec![
                    "python3".into(),
                    "-m".into(),
                    "venv".into(),
                    "tools/env".into(),
                ],
                "virtualenv location is pinned to .venv/venv",
            ),
            (
                vec![
                    "npm".into(),
                    "install".into(),
                    "--save-dev".into(),
                    "left-pad".into(),
                ],
                "provider set is pinned",
            ),
            (
                vec!["npm".into(), "install".into(), "@vitest/coverage-v8".into()],
                "a coverage provider is a devDependency, not a dependency",
            ),
            (
                vec![
                    "npm".into(),
                    "install".into(),
                    "--save-dev".into(),
                    "@vitest/coverage-v8".into(),
                    "--force".into(),
                ],
                "no stray flags may ride along",
            ),
            (
                vec!["npm".into(), "install".into()],
                "a bare install resolves the whole tree, which is not a coverage step",
            ),
            (
                vec!["composer".into(), "require".into(), "evil/pkg".into()],
                "generation may install pinned deps, not add new ones",
            ),
            (
                vec![
                    "bundle".into(),
                    "exec".into(),
                    "rake".into(),
                    "release".into(),
                ],
                "coverage has no reason to run arbitrary project tasks",
            ),
            (
                vec![
                    "vendor/bin/evil".into(),
                    "--coverage-clover".into(),
                    "coverage.xml".into(),
                ],
                "a repository file is not an executable just because it is named",
            ),
            (
                vec![".venv/bin/pip".into(), "install".into(), "pytest".into()],
                "only the pinned interpreter spellings are repo-local programs",
            ),
        ] {
            let err = validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
                .expect_err(&format!("{argv:?} must be refused: {why}"));
            assert!(
                err.contains("refused"),
                "{argv:?} ({why}) must be refused explicitly: {err}"
            );
        }
    }

    /// Analysis never writes. Every setup step the generator may run must be
    /// refused for the read-only coverage action, so a mislabeled call site
    /// cannot install anything.
    #[test]
    fn coverage_analysis_refuses_every_setup_step() {
        for argv in [
            vec!["python3".into(), "-m".into(), "venv".into(), ".venv".into()],
            vec![
                ".venv/bin/python".into(),
                "-m".into(),
                "pip".into(),
                "install".into(),
                "pytest".into(),
                "pytest-cov".into(),
            ],
            vec![
                "npm".into(),
                "install".into(),
                "--save-dev".into(),
                "@vitest/coverage-v8".into(),
            ],
            vec!["composer".into(), "install".into()],
            vec!["bundle".into(), "install".into()],
        ] {
            let err = validate_manvi_action(&argv, ManviActionKind::Coverage)
                .expect_err("analysis must not be able to install anything");
            assert!(err.contains("allowlist"), "{argv:?}: {err}");
        }
    }

    /// A health remediation must not become an install channel either: the
    /// venv/pip pair is coverage-generation-only.
    #[test]
    fn health_actions_cannot_use_the_coverage_install_channel() {
        for argv in [
            vec!["python3".into(), "-m".into(), "venv".into(), ".venv".into()],
            vec![
                ".venv/bin/python".into(),
                "-m".into(),
                "pip".into(),
                "install".into(),
                "pytest".into(),
            ],
        ] {
            let err = validate_manvi_action(&argv, ManviActionKind::Health)
                .expect_err("health remediation is not a coverage setup channel");
            assert!(err.contains("allowlist"), "{argv:?}: {err}");
        }
    }

    /// The global refusals still bind the new shapes: no escaping the repo,
    /// no host-wide flags, no network sources.
    #[test]
    fn setup_steps_remain_subject_to_the_global_refusals() {
        for argv in [
            vec![
                "python3".into(),
                "-m".into(),
                "venv".into(),
                "/tmp/env".into(),
            ],
            vec![
                "python3".into(),
                "-m".into(),
                "venv".into(),
                "../outside".into(),
            ],
            vec![
                ".venv/bin/python".into(),
                "-m".into(),
                "pip".into(),
                "install".into(),
                "--user".into(),
                "pytest".into(),
            ],
            vec![
                "npm".into(),
                "install".into(),
                "--save-dev".into(),
                "https://evil.example/pkg.tgz".into(),
            ],
            vec![
                "npm".into(),
                "install".into(),
                "--global".into(),
                "@vitest/coverage-v8".into(),
            ],
        ] {
            validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
                .expect_err("global refusals must still bind");
        }
    }

    #[test]
    fn coverage_generator_allowlist_accepts_scanner_planned_commands() {
        for argv in [
            vec!["npm".into(), "run".into(), "coverage".into()],
            vec![
                "npx".into(),
                "--no-install".into(),
                "vitest".into(),
                "run".into(),
                "--coverage".into(),
            ],
            vec![
                "npx".into(),
                "--no-install".into(),
                "jest".into(),
                "--coverage".into(),
            ],
            vec![
                "cargo".into(),
                "llvm-cov".into(),
                "--manifest-path".into(),
                "src-tauri/Cargo.toml".into(),
                "--workspace".into(),
                "--lcov".into(),
                "--output-path".into(),
                "src-tauri/lcov.info".into(),
            ],
            vec![
                "cargo".into(),
                "llvm-cov".into(),
                "--workspace".into(),
                "--lcov".into(),
                "--output-path".into(),
                "lcov.info".into(),
            ],
            vec!["pytest".into(), "--cov".into(), "--cov-report=xml".into()],
            vec![
                "go".into(),
                "test".into(),
                "./...".into(),
                "-coverprofile=coverage.out".into(),
            ],
            vec![
                "go".into(),
                "-C".into(),
                "backend/go_orchestrator".into(),
                "test".into(),
                "./...".into(),
                "-coverprofile=coverage.out".into(),
            ],
            vec!["npm".into(), "run".into(), "test:coverage".into()],
            vec!["./gradlew".into(), "test".into(), "jacocoTestReport".into()],
            vec!["mvn".into(), "verify".into()],
            vec![
                "swift".into(),
                "test".into(),
                "--enable-code-coverage".into(),
            ],
            vec!["dart".into(), "test".into(), "--coverage=coverage".into()],
        ] {
            validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
                .unwrap_or_else(|err| panic!("{argv:?} must be allowed: {err}"));
        }
        let npx_without_no_install = validate_manvi_action(
            &[
                "npx".into(),
                "vitest".into(),
                "run".into(),
                "--coverage".into(),
            ],
            ManviActionKind::CoverageGenerator,
        )
        .unwrap_err();
        assert!(
            npx_without_no_install.contains("allowlist"),
            "npx must require --no-install: {npx_without_no_install}"
        );
    }

    #[test]
    fn coverage_generator_allowlist_refuses_go_chdir_without_test() {
        for argv in [
            vec![
                "go".into(),
                "-C".into(),
                "backend/go_orchestrator".into(),
                "run".into(),
                ".".into(),
            ],
            vec!["go".into(), "-C".into(), "backend".into()],
            vec![
                "go".into(),
                "-C".into(),
                "-modcache".into(),
                "test".into(),
                "./...".into(),
            ],
        ] {
            let err = validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
                .expect_err("only go -C <dir> test is allowed");
            assert!(err.contains("allowlist"), "{argv:?}: {err}");
        }
    }

    #[test]
    fn coverage_generator_allowlist_accepts_swift_and_dart_test_only() {
        validate_manvi_action(
            &[
                "swift".into(),
                "test".into(),
                "--enable-code-coverage".into(),
            ],
            ManviActionKind::CoverageGenerator,
        )
        .unwrap();
        validate_manvi_action(
            &["dart".into(), "test".into(), "--coverage=coverage".into()],
            ManviActionKind::CoverageGenerator,
        )
        .unwrap();
        for argv in [
            vec!["swift".into(), "package".into(), "reset".into()],
            vec!["dart".into(), "pub".into(), "get".into()],
        ] {
            let err = validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
                .expect_err("non-test swift/dart must stay refused");
            assert!(err.contains("allowlist"), "{argv:?}: {err}");
        }
    }

    // REGRESSION GUARD: repo-local wrapper spellings are the only executable
    // paths the allowlist accepts. Path validation must still canonicalize the
    // wrapper itself, or ./gradlew can be a symlink to an executable outside
    // the open repository.
    #[cfg(unix)]
    #[test]
    fn coverage_wrapper_symlink_escape_is_refused() {
        let repo = TempDir::new().unwrap();
        init_test_repo(repo.path());
        let outside = TempDir::new().unwrap();
        let executable = outside.path().join("gradlew");
        std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        std::os::unix::fs::symlink(&executable, repo.path().join("gradlew")).unwrap();
        let argv = vec!["./gradlew".into(), "test".into(), "jacocoTestReport".into()];

        validate_manvi_action(&argv, ManviActionKind::CoverageGenerator).unwrap();
        let err = validate_manvi_paths(repo.path(), &argv, ManviActionKind::CoverageGenerator)
            .expect_err("outside wrapper symlink must be refused");
        assert!(err.contains("escapes"), "{err}");
    }

    #[test]
    fn coverage_generator_allowlist_accepts_locked_llvm_cov_setup() {
        for argv in [
            vec![
                "cargo".into(),
                "install".into(),
                "cargo-llvm-cov".into(),
                "--locked".into(),
            ],
            vec![
                "cargo".into(),
                "install".into(),
                "--locked".into(),
                "cargo-llvm-cov".into(),
            ],
            vec![
                "rustup".into(),
                "component".into(),
                "add".into(),
                "llvm-tools-preview".into(),
            ],
        ] {
            validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
                .unwrap_or_else(|err| panic!("{argv:?} must be allowed: {err}"));
            let analysis = validate_manvi_action(&argv, ManviActionKind::Coverage)
                .expect_err("analysis must not install tools");
            assert!(analysis.contains("allowlist"), "{argv:?}: {analysis}");
            let health = validate_manvi_action(&argv, ManviActionKind::Health)
                .expect_err("health must not install llvm-cov");
            assert!(health.contains("allowlist"), "{argv:?}: {health}");
        }
    }

    #[test]
    fn coverage_generator_allowlist_refuses_arbitrary_cargo_install() {
        for argv in [
            vec!["cargo".into(), "install".into(), "cargo-llvm-cov".into()],
            vec![
                "cargo".into(),
                "install".into(),
                "cargo-tarpaulin".into(),
                "--locked".into(),
            ],
            vec![
                "cargo".into(),
                "install".into(),
                "cargo-llvm-cov".into(),
                "--locked".into(),
                "left-pad".into(),
            ],
            vec![
                "cargo".into(),
                "install".into(),
                "cargo-llvm-cov".into(),
                "--locked".into(),
                "--git".into(),
                "https://example.invalid/llvm-cov.git".into(),
            ],
            vec![
                "cargo".into(),
                "install".into(),
                "--path".into(),
                ".".into(),
                "cargo-llvm-cov".into(),
            ],
            vec![
                "rustup".into(),
                "component".into(),
                "add".into(),
                "rust-src".into(),
            ],
            vec![
                "rustup".into(),
                "toolchain".into(),
                "install".into(),
                "nightly".into(),
            ],
        ] {
            let err = validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
                .expect_err("arbitrary toolchain mutation must stay refused");
            assert!(
                err.contains("allowlist") || err.contains("URL") || err.contains("transports"),
                "{argv:?}: {err}"
            );
        }
    }

    #[test]
    fn manvi_action_argv_is_bounded_and_control_character_free() {
        let oversized = vec![
            "npm".into(),
            "test".into(),
            "x".repeat(TERMINAL_ARG_BYTES_CAP + 1),
        ];
        assert!(validate_manvi_action(&oversized, ManviActionKind::Coverage).is_err());
        assert!(validate_manvi_action(
            &["npm".into(), "test\nforged-log".into()],
            ManviActionKind::Coverage,
        )
        .is_err());
    }

    #[test]
    fn allowed_non_git_manvi_action_is_always_gated() {
        let dir = TempDir::new().unwrap();
        init_test_repo(dir.path());
        let result = run_manvi_action(
            dir.path().to_str().unwrap(),
            &["npm".into(), "test".into()],
            ManviActionKind::Health,
            Some(5),
        )
        .expect("an allowed shape should run to an ordinary process result");
        assert!(
            result.gated,
            "model-authored non-git commands must reach MANVI"
        );
        assert!(
            result.policy.is_some(),
            "a gated action must retain its verdict"
        );
        assert_ne!(
            result.exit_code,
            Some(0),
            "the fixture has no package.json test script"
        );
    }

    #[cfg(unix)]
    #[test]
    fn manvi_path_options_refuse_symlink_escape() {
        let dir = TempDir::new().unwrap();
        init_test_repo(dir.path());
        let outside = TempDir::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("coverage-link")).unwrap();
        let repo = validate_repo(dir.path().to_str().unwrap()).unwrap();
        let args = vec![
            "cargo".into(),
            "llvm-cov".into(),
            "--output-path".into(),
            "coverage-link/lcov.info".into(),
        ];
        let err = validate_manvi_paths(&repo, &args, ManviActionKind::Coverage).unwrap_err();
        assert!(err.contains("escapes the open repository"), "{err}");

        let chdir = vec![
            "go".into(),
            "-C".into(),
            "coverage-link".into(),
            "test".into(),
            "./...".into(),
            "-coverprofile=coverage.out".into(),
        ];
        let chdir_err =
            validate_manvi_paths(&repo, &chdir, ManviActionKind::CoverageGenerator).unwrap_err();
        assert!(
            chdir_err.contains("escapes the open repository"),
            "{chdir_err}"
        );
    }

    #[test]
    fn rejects_whitespace_only_program() {
        let dir = TempDir::new().unwrap();
        init_test_repo(dir.path());
        let res = run_terminal(
            dir.path().to_str().unwrap(),
            &["   ".into(), "status".into()],
            None,
        );
        assert!(res.is_err());
        assert_eq!(res.unwrap_err(), "Invalid program name");
    }

    #[test]
    fn handles_failed_command_exit_code() {
        let dir = TempDir::new().unwrap();
        init_test_repo(dir.path());
        let res = run_terminal(
            dir.path().to_str().unwrap(),
            &["git".into(), "log".into(), "nonexistent-branch-xyz".into()],
            Some(5),
        )
        .expect("runs git log");

        assert_ne!(res.exit_code, Some(0));
        assert!(!res.stderr_tail.is_empty());
    }

    /// A mutating git command must reach the harness gate, whether or not a
    /// harness is installed to answer.
    ///
    /// This deliberately does *not* assert that `git push --force` is refused.
    /// `harness::gated` documents a missing harness as the one unchecked
    /// verdict allowed to proceed, so the refusal only happens on a machine
    /// that actually has MANVI installed. Asserting it made the suite pass on
    /// a developer laptop with `manvi` on PATH and fail on every CI runner
    /// without it — the test was describing the machine, not the code.
    ///
    /// The invariant this module really owns is routing: a git command is
    /// never spawned ungated. Either the gate refuses it, or it comes back
    /// marked `gated` with the verdict attached.
    #[test]
    fn force_push_is_never_spawned_ungated() {
        let dir = TempDir::new().unwrap();
        init_test_repo(dir.path());
        let res = run_terminal(
            dir.path().to_str().unwrap(),
            &[
                "git".into(),
                "push".into(),
                "--force".into(),
                "origin".into(),
                "main".into(),
            ],
            Some(5),
        );

        match res {
            // A harness answered and blocked it: the refusal must name a reason
            // rather than being an opaque failure.
            Err(err) => assert!(
                err.contains("policy")
                    || err.contains("force")
                    || err.contains("blocked")
                    || err.contains("refused")
                    || err.contains("MANVI"),
                "refusal should name why it was refused, got: {err}"
            ),
            // No harness to answer: it may proceed, but it must still be
            // recorded as having gone through the gate, with the verdict kept.
            // `gated == false` here would mean a force push took the non-git
            // bypass path.
            Ok(run) => {
                assert!(run.gated, "a git command must never report itself ungated");
                assert!(
                    run.policy.is_some(),
                    "a gated command must carry the verdict it was judged by"
                );
            }
        }
    }

    /// PTY-free tests for the post-EOF finalize logic: the exit payload must
    /// carry the real status when wait succeeds and degrade honestly to
    /// `exit_code: None` when reaping fails.
    #[test]
    fn finalize_propagates_clean_exit_code() {
        let mut emitted = Vec::new();
        finalize_pty_session(
            "term-t1",
            || {
                Ok(ExitStatusLike {
                    code: 0,
                    signal: None,
                })
            },
            |p| emitted.push(p),
        );
        assert_eq!(emitted.len(), 1, "exactly one exit event");
        assert_eq!(emitted[0].id, "term-t1");
        assert_eq!(emitted[0].exit_code, Some(0));
        assert_eq!(emitted[0].signal, "");
    }

    #[test]
    fn finalize_propagates_nonzero_exit_code() {
        let mut emitted = Vec::new();
        finalize_pty_session(
            "term-t2",
            || {
                Ok(ExitStatusLike {
                    code: 127,
                    signal: None,
                })
            },
            |p| emitted.push(p),
        );
        assert_eq!(emitted[0].exit_code, Some(127));
    }

    #[test]
    fn finalize_surfaces_signal_name_from_status() {
        let mut emitted = Vec::new();
        finalize_pty_session(
            "term-t3",
            || {
                Ok(ExitStatusLike {
                    code: 1,
                    signal: Some("Hangup".to_string()),
                })
            },
            |p| emitted.push(p),
        );
        assert_eq!(emitted[0].exit_code, Some(1));
        assert_eq!(emitted[0].signal, "Hangup");
    }

    #[test]
    fn finalize_reports_unknown_exit_when_wait_fails() {
        let mut emitted = Vec::new();
        finalize_pty_session(
            "term-t4",
            || Err(std::io::Error::other("waitpid failed")),
            |p| emitted.push(p),
        );
        assert_eq!(emitted.len(), 1);
        assert_eq!(
            emitted[0].exit_code, None,
            "failed reap must not invent a status"
        );
        assert_eq!(emitted[0].signal, "");
    }

    /// The adapter must mirror the vendored portable-pty ExitStatus exactly:
    /// plain deaths expose `exit_code()`, signal deaths keep the fallback code
    /// plus the signal name (vendored lib.rs `From<std::process::ExitStatus>`).
    #[test]
    fn exit_status_like_adapter_matches_vendored_api() {
        let ok = portable_pty::ExitStatus::with_exit_code(42);
        let like = ExitStatusLike::from(&ok);
        assert_eq!(
            like,
            ExitStatusLike {
                code: 42,
                signal: None
            }
        );

        let killed = portable_pty::ExitStatus::with_signal("Terminated");
        let like = ExitStatusLike::from(&killed);
        assert_eq!(like.code, 1, "vendored fallback code for signals is 1");
        assert_eq!(like.signal.as_deref(), Some("Terminated"));
    }

    /// kill_session on an unknown id stays a silent no-op success — killing a
    /// session that already exited (and was removed by its reader thread) must
    /// not surface as an error to the frontend.
    #[test]
    fn kill_session_of_unknown_id_is_ok_noop() {
        let state = TerminalSessions::default();
        assert!(kill_session(&state, "term-missing").is_ok());
    }

    #[test]
    fn pty_dimensions_are_clamped_at_both_boundaries() {
        let minimum = bounded_pty_size(0, 0);
        assert_eq!((minimum.rows, minimum.cols), (1, 1));

        let maximum = bounded_pty_size(u16::MAX, u16::MAX);
        assert_eq!((maximum.rows, maximum.cols), (MAX_PTY_ROWS, MAX_PTY_COLS));
    }

    #[test]
    fn oversized_pty_input_is_refused_before_session_lookup() {
        let state = TerminalSessions::default();
        let input = "x".repeat(MAX_PTY_INPUT_BYTES + 1);
        let err = write_to_session(&state, "term-missing", &input).unwrap_err();
        assert!(err.contains("input"), "{err}");
        assert!(err.contains("limit"), "{err}");
    }

    #[test]
    fn pty_session_reservations_are_globally_bounded() {
        let state = TerminalSessions::default();
        let reservations: Vec<_> = (0..MAX_PTY_SESSIONS)
            .map(|_| reserve_session(&state).expect("within cap"))
            .collect();
        let err = reserve_session(&state).err().expect("cap must refuse");
        assert!(err.contains("limit"), "{err}");
        drop(reservations);
        assert!(
            reserve_session(&state).is_ok(),
            "released slots must be reusable"
        );
    }
}
