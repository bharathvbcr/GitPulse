//! Terminal execution module: PTY session management and bounded command execution.

use crate::engine::git_cli::{run_captured, sandbox_join_canonical, validate_repo, RunOutcome};
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

fn normalized_program(program: &str) -> String {
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
        ManviActionKind::Coverage | ManviActionKind::CoverageGenerator => match subcommand.as_str()
        {
            "test" => true,
            "run" => npm_run_is_verification(args),
            // npm exec may otherwise download a missing package. `--no` makes
            // this a local-dependency-only execution on supported npm builds.
            "exec" => local_js_runner(args, 2),
            _ => false,
        },
    }
}

fn python_module_allowed(args: &[String], kind: ManviActionKind) -> bool {
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
        ManviActionKind::Coverage | ManviActionKind::CoverageGenerator => match module.as_deref() {
            Some("pytest") => true,
            Some("coverage") => command_is_one_of(
                &args[2..],
                &["run", "report", "html", "xml", "json", "lcov"],
            ),
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
fn validate_manvi_action(args: &[String], kind: ManviActionKind) -> Result<(), String> {
    if args.is_empty() {
        return Err("MANVI action has no command".into());
    }
    validate_argv_bounds(args).map_err(|e| format!("MANVI action refused: {e}"))?;

    let program_raw = args[0].trim();
    let repo_wrapper = matches!(
        program_raw,
        "./gradlew" | ".\\gradlew.bat" | "./mvnw" | ".\\mvnw.cmd"
    );
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

    let program = normalized_program(program_raw);
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
        "python" | "python3" | "py" => python_module_allowed(args, kind),
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
        "composer" => {
            kind == ManviActionKind::Health
                && command_is_one_of(
                    args,
                    &["audit", "update", "require", "install", "show", "test"],
                )
        }
        "bundle" | "bundler" => match args.get(1).map(String::as_str) {
            Some("audit" | "update" | "install" | "check") => kind == ManviActionKind::Health,
            Some("exec") => {
                kind == ManviActionKind::Health
                    && args.get(2).is_some_and(|tool| {
                        matches!(tool.as_str(), "rake" | "rspec" | "bundler-audit")
                    })
            }
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
fn validate_manvi_paths(
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
    if matches!(
        program_raw,
        "./gradlew" | ".\\gradlew.bat" | "./mvnw" | ".\\mvnw.cmd"
    ) {
        let normalized = program_raw.replace('\\', "/");
        let relative = normalized.strip_prefix("./").unwrap_or(&normalized);
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
