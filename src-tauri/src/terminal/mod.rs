//! Terminal execution module: PTY session management and bounded command execution.

use crate::engine::git_cli::{run_captured, validate_repo, RunOutcome};
use crate::harness::{guard_command, PolicyVerdict};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
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
    let pty_system = native_pty_system();
    let size = PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    };
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

    thread::Builder::new()
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
            guard.remove(&sid_for_exit);

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
        })
        .map_err(|e| format!("Failed to spawn PTY reader thread: {e}"))?;

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
        let size = PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        };
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
    if args.is_empty() {
        return Err("No command provided".into());
    }
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
    let (policy_verdict, gated) = if is_git {
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
}
