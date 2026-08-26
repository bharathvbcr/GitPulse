//! Terminal execution module: PTY session management and bounded command execution.

use crate::engine::git_cli::{run_captured, validate_repo, RunOutcome};
use crate::harness::{guard_command, PolicyVerdict};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
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

/// Upper bound on simultaneously live PTY sessions. Every session is a real
/// shell process plus a reader thread, so an unbounded registry lets a
/// frontend bug or runaway loop exhaust processes/fds. The watcher module
/// applies the same shape of bound (`MAX_WATCHES`).
const MAX_SESSIONS: usize = 16;

/// Upper bound on one `cmd_terminal_write` payload. Input arrives from the
/// webview unvalidated; without a cap a compromised renderer could hand us an
/// arbitrarily large string to copy into kernel buffers. Sized generously
/// above any real paste (the sidecar frame cap is 4 MiB).
const MAX_WRITE_BYTES: usize = 1024 * 1024;

/// How long `write_to_session` may wait for a concurrent writer on the same
/// session to finish before reporting the session as busy. Writes hold no
/// shared lock while blocked on the PTY, so this bounds backpressure without
/// reintroducing cross-session stalls. Generous against normal interleaved
/// keystrokes; far below any human-noticeable threshold.
const WRITE_BUSY_RETRY_WINDOW: Duration = Duration::from_millis(50);
const WRITE_BUSY_RETRY_INTERVAL: Duration = Duration::from_millis(1);

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
    /// Taken out of the entry while a write is in flight so the blocking
    /// write never happens under the sessions-map lock (a wedged shell must
    /// not stall spawns/resizes/kills of *other* sessions, nor its own kill).
    writer: Option<Box<dyn Write + Send>>,
    dead: Arc<AtomicBool>,
    /// Retained so the shell process can be reaped on session teardown;
    /// dropping the handle leaves one zombie per exited session.
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
}

/// Waits the child to completion on a detached thread. Called on every
/// teardown path; `wait` unblocks once the master closes and the shell gets
/// SIGHUP, so this never accumulates.
fn reap_child(child: Option<Box<dyn portable_pty::Child + Send + Sync>>) {
    if let Some(mut child) = child {
        thread::Builder::new()
            .name("pty-reap".into())
            .spawn(move || {
                let _ = child.wait();
            })
            .ok();
    }
}

/// Thread-safe registry of live PTY sessions.
#[derive(Default, Clone)]
pub struct TerminalSessions {
    sessions: Arc<Mutex<HashMap<String, SessionEntry>>>,
}

impl TerminalSessions {
    /// Inserts a session, refusing to exceed [`MAX_SESSIONS`].
    fn insert_capped(
        &self,
        session_id: String,
        entry: SessionEntry,
    ) -> Result<(), String> {
        let mut guard = self
            .sessions
            .lock()
            .map_err(|e| format!("Lock error: {e}"))?;
        if guard.len() >= MAX_SESSIONS {
            return Err(format!(
                "Terminal session limit reached ({MAX_SESSIONS}); close a shell before starting another"
            ));
        }
        guard.insert(session_id, entry);
        Ok(())
    }

    /// Removes the writer for an exclusive in-flight write. A missing entry
    /// is `Err("not found")`; a writer already checked out is
    /// `Err("busy")`.
    fn take_writer(&self, session_id: &str) -> Result<Box<dyn Write + Send>, String> {
        let mut guard = self
            .sessions
            .lock()
            .map_err(|e| format!("Lock error: {e}"))?;
        match guard.get_mut(session_id) {
            None => Err(format!("Terminal session '{session_id}' not found")),
            Some(entry) => entry.writer.take().ok_or_else(|| {
                format!("Terminal session '{session_id}' is busy; retry shortly")
            }),
        }
    }

    /// Returns a checked-out writer. If the session was killed while the
    /// write was in flight the writer is dropped instead — killing wins.
    fn return_writer(&self, session_id: &str, writer: Box<dyn Write + Send>) {
        if let Ok(mut guard) = self.sessions.lock() {
            if let Some(entry) = guard.get_mut(session_id) {
                entry.writer = Some(writer);
            }
        }
    }

    /// Number of registered sessions. A poisoned lock is an error, not a
    /// zero — callers must not mistake "cannot observe" for "nothing there".
    pub fn live_session_count(&self) -> Result<usize, String> {
        let guard = self
            .sessions
            .lock()
            .map_err(|e| format!("Lock error: {e}"))?;
        Ok(guard.len())
    }
}

/// Teardown for a sessions map held behind its own Arc: removes `session_id`
/// and reaps the shell child. Used by both [`TerminalSessions::remove_for_teardown`]
/// and the detached reader thread, which owns only the inner map Arc.
fn remove_for_teardown_on(
    sessions: &Arc<Mutex<HashMap<String, SessionEntry>>>,
    session_id: &str,
) -> bool {
    let removed = match sessions.lock() {
        Ok(mut guard) => guard.remove(session_id),
        Err(_) => None,
    };
    match removed {
        Some(entry) => {
            reap_child(entry.child);
            true
        }
        None => false,
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

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("Failed to spawn shell '{shell}': {e}"))?;

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
        writer: Some(writer),
        dead: dead.clone(),
        child: Some(child),
    };

    state.insert_capped(session_id.clone(), entry)?;

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
                        let _ = app_handle.emit(
                            "terminal-output",
                            TerminalOutputPayload {
                                id: sid_for_thread.clone(),
                                data_b64,
                            },
                        );
                    }
                    Err(_) => break,
                }
            }

            dead_flag.store(true, Ordering::SeqCst);
            // Clean up session entry from map and reap the shell child.
            remove_for_teardown_on(&sessions_map, &sid_for_exit);

            // Emit exit event.
            let _ = app_handle.emit(
                "terminal-exit",
                TerminalExitPayload {
                    id: sid_for_exit,
                    exit_code: None,
                    signal: String::new(),
                },
            );
        })
        .map_err(|e| {
            // The live entry (PTY master + shell) must not outlive the failed
            // spawn under an id nobody received — tear it down and reap.
            remove_for_teardown_on(&state.sessions, &session_id);
            format!("Failed to spawn PTY reader thread: {e}")
        })?;

    Ok(TerminalSpawned {
        id: session_id,
        shell,
        cwd: repo.to_string_lossy().into_owned(),
    })
}

/// Writes user input bytes into a PTY session.
///
/// The write happens with the sessions-map lock *released*: a shell that
/// stops consuming input (flow control, SIGSTOP) blocks only this caller's
/// worker — spawns, resizes, kills of other sessions, and this session's own
/// kill all proceed. Concurrent writers on the same session are serialized
/// by writer checkout, retrying briefly before an honest busy error.
pub fn write_to_session(
    state: &TerminalSessions,
    session_id: &str,
    data: &str,
) -> Result<(), String> {
    if data.len() > MAX_WRITE_BYTES {
        return Err(format!(
            "Terminal input exceeds cap ({MAX_WRITE_BYTES} bytes; got {})",
            data.len()
        ));
    }

    let deadline = Instant::now() + WRITE_BUSY_RETRY_WINDOW;
    let mut writer = match state.take_writer(session_id) {
        Ok(writer) => writer,
        Err(err) if err.ends_with("retry shortly") => loop {
            if Instant::now() >= deadline {
                return Err(err);
            }
            thread::sleep(WRITE_BUSY_RETRY_INTERVAL);
            match state.take_writer(session_id) {
                Ok(writer) => break writer,
                Err(e) if e.ends_with("retry shortly") => continue,
                Err(e) => return Err(e),
            }
        },
        Err(err) => return Err(err),
    };

    let result = writer
        .write_all(data.as_bytes())
        .and_then(|()| writer.flush())
        .map_err(|e| format!("Failed to write to terminal: {e}"));

    // Give the writer back unless the session was killed while we wrote.
    state.return_writer(session_id, writer);
    result
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
pub fn kill_session(state: &TerminalSessions, session_id: &str) -> Result<(), String> {
    let mut guard = state
        .sessions
        .lock()
        .map_err(|e| format!("Lock error: {e}"))?;
    if let Some(mut session) = guard.remove(session_id) {
        session.dead.store(true, Ordering::SeqCst);
        // Reap outside the lock; `wait` unblocks once the dropped master
        // SIGHUPs the shell.
        reap_child(session.child.take());
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
    use std::sync::mpsc;
    use tempfile::TempDir;

    fn init_test_repo(dir: &std::path::Path) {
        let output = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir)
            .output()
            .expect("git init");
        assert!(output.status.success());
    }

    /// Builds a real PTY pair running `program` plus a registered entry,
    /// without needing an AppHandle. Returns the sessions registry, the
    /// session id, and a reader on the master side.
    fn spawn_raw_session(
        state: &TerminalSessions,
        id: &str,
        program: &str,
    ) -> (Box<dyn Read + Send>, Arc<AtomicBool>) {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let child = pair
            .slave
            .spawn_command(CommandBuilder::new(program))
            .expect("spawn test program");
        drop(pair.slave);
        let reader = pair.master.try_clone_reader().expect("clone reader");
        let writer = pair.master.take_writer().expect("take writer");
        let dead = Arc::new(AtomicBool::new(false));
        state
            .insert_capped(
                id.to_string(),
                SessionEntry {
                    master: pair.master,
                    writer: Some(writer),
                    dead: dead.clone(),
                    child: Some(child),
                },
            )
            .expect("insert under cap");
        (reader, dead)
    }

    /// Reads `n` bytes from `reader` on a helper thread; fails the test if
    /// the bytes do not arrive within 5s instead of hanging the suite.
    fn read_n(mut reader: Box<dyn Read + Send>, n: usize) -> Vec<u8> {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut got = vec![0u8; n];
            let mut filled = 0;
            while filled < n {
                match reader.read(&mut got[filled..]) {
                    Ok(0) => break,
                    Ok(k) => filled += k,
                    Err(_) => break,
                }
            }
            got.truncate(filled);
            let _ = tx.send(got);
        });
        rx.recv_timeout(Duration::from_secs(5))
            .expect("echo within 5s")
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

    // ------------------------------------------------------------------
    // Session registry behavior (real PTYs, no AppHandle required).
    // ------------------------------------------------------------------

    #[test]
    fn write_roundtrips_through_a_real_pty() {
        let state = TerminalSessions::default();
        let (reader, _dead) = spawn_raw_session(&state, "t-echo", "cat");
        write_to_session(&state, "t-echo", "hello").expect("write succeeds");
        assert_eq!(read_n(reader, 5), b"hello");
    }

    #[test]
    fn session_cap_is_enforced_with_an_honest_error() {
        let state = TerminalSessions::default();
        for i in 0..MAX_SESSIONS {
            let (reader, _dead) = spawn_raw_session(&state, &format!("t-cap-{i}"), "cat");
            let _ = reader; // kept open on purpose: sessions stay live
        }
        assert_eq!(state.live_session_count().unwrap(), MAX_SESSIONS);
        let err = write_to_session(&state, "t-cap-over", "x")
            .expect_err("an over-cap registry has no such session anyway");
        assert!(err.contains("not found"));

        // The cap itself is what insert_capped enforces; verify by direct
        // insert attempt using a fresh pty.
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 2,
                cols: 2,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        drop(pair.slave);
        let writer = pair.master.take_writer().unwrap();
        let entry = SessionEntry {
            master: pair.master,
            writer: Some(writer),
            dead: Arc::new(AtomicBool::new(false)),
            child: None,
        };
        let err = state
            .insert_capped("t-cap-over".into(), entry)
            .expect_err("cap reached");
        assert!(
            err.contains(&MAX_SESSIONS.to_string()) && err.contains("limit"),
            "cap error should name the limit, got: {err}"
        );
    }

    #[test]
    fn oversized_write_is_rejected_before_touching_the_session() {
        let state = TerminalSessions::default();
        let (reader, _dead) = spawn_raw_session(&state, "t-big", "cat");
        let oversized = vec![b'x'; MAX_WRITE_BYTES + 1];
        let err = write_to_session(&state, "t-big", std::str::from_utf8(&oversized).unwrap())
            .expect_err("payload over cap must be refused");
        assert!(err.contains("exceeds cap"), "got: {err}");
        // The session stays usable for honest-sized traffic.
        write_to_session(&state, "t-big", "ok").expect("small write after refusal");
        assert_eq!(read_n(reader, 2), b"ok");
    }

    #[test]
    fn write_to_unknown_session_names_it() {
        let state = TerminalSessions::default();
        let err = write_to_session(&state, "nope", "x").expect_err("must fail");
        assert!(err.contains("not found") && err.contains("nope"));
    }

    #[test]
    fn concurrent_writers_serialize_or_report_busy_without_deadlock() {
        let state = TerminalSessions::default();
        let (_reader, _dead) = spawn_raw_session(&state, "t-busy", "cat");

        // Hold the writer so a competing write hits the busy path.
        let held = state.take_writer("t-busy").expect("checkout writer");

        let (tx, rx) = mpsc::channel();
        {
            let state = state.clone();
            thread::spawn(move || {
                let started = Instant::now();
                let res = write_to_session(&state, "t-busy", "contended");
                let _ = tx.send((res, started.elapsed()));
            });
        }
        // Contended write must resolve within the retry window plus slack —
        // not block forever.
        let (res, elapsed) = rx.recv_timeout(Duration::from_secs(5)).expect("no deadlock");
        match res {
            Err(err) => {
                assert!(err.contains("busy"), "expected honest busy error, got: {err}");
                assert!(
                    elapsed >= WRITE_BUSY_RETRY_WINDOW,
                    "busy error should come only after the retry window"
                );
            }
            Ok(()) => panic!("write succeeded while the writer was held out"),
        }

        // Returning the writer unblocks subsequent writes.
        state.return_writer("t-busy", held);
        write_to_session(&state, "t-busy", "after").expect("write works again");
    }

    #[test]
    fn kill_removes_the_session_and_later_writes_report_it() {
        let state = TerminalSessions::default();
        let (_reader, dead) = spawn_raw_session(&state, "t-kill", "cat");
        kill_session(&state, "t-kill").expect("kill is idempotent-ok");
        assert_eq!(state.live_session_count().unwrap(), 0);
        assert!(dead.load(Ordering::Relaxed), "kill marks the session dead");
        let err = write_to_session(&state, "t-kill", "x").expect_err("session gone");
        assert!(err.contains("not found"));
        // Killing again stays Ok (frontend teardown races are normal).
        kill_session(&state, "t-kill").expect("second kill ok");
    }

    #[test]
    fn registry_recovers_to_empty_after_a_kill_storm() {
        let state = TerminalSessions::default();
        for i in 0..MAX_SESSIONS {
            spawn_raw_session(&state, &format!("t-storm-{i}"), "cat");
        }
        assert_eq!(state.live_session_count().unwrap(), MAX_SESSIONS);
        // Concurrent killers: every session dies exactly once.
        let mut handles = Vec::new();
        for i in 0..MAX_SESSIONS {
            let state = state.clone();
            handles.push(thread::spawn(move || {
                kill_session(&state, &format!("t-storm-{i}"))
            }));
        }
        for h in handles {
            h.join().expect("killer thread ok").expect("kill ok");
        }
        assert_eq!(state.live_session_count().unwrap(), 0);
    }

    /// Mixed-load stress: concurrent writers, resizers, and killers across
    /// several sessions must converge to an empty registry without
    /// deadlocking or poisoning the shared map.
    #[test]
    fn stress_writers_resizers_killers_converge_to_empty() {
        let state = TerminalSessions::default();
        let sessions: Vec<String> = (0..4).map(|i| format!("t-mix-{i}")).collect();
        let mut readers = Vec::new();
        for id in &sessions {
            let (reader, _dead) = spawn_raw_session(&state, id, "cat");
            readers.push(reader);
        }

        let (tx, rx) = mpsc::channel();
        // Writers: 8 threads x 25 writes spread across the 4 sessions.
        // Every thread reports completion exactly once so the receiver can
        // account for all of them.
        for w in 0..8 {
            let state = state.clone();
            let sessions = sessions.clone();
            let tx = tx.clone();
            thread::spawn(move || {
                for i in 0..25 {
                    let id = &sessions[(w + i) % sessions.len()];
                    let payload = format!("w{w}i{i}\n");
                    // Busy errors are legal under contention; anything else is not.
                    if let Err(err) = write_to_session(&state, id, &payload) {
                        if !err.contains("busy") && !err.contains("not found") {
                            let _ = tx.send(Err(format!("unexpected write error: {err}")));
                        }
                    }
                }
                let _ = tx.send(Ok(()));
            });
        }
        // Resizers hammering concurrently.
        for r in 0..2 {
            let state = state.clone();
            let sessions = sessions.clone();
            thread::spawn(move || {
                for i in 0..40u16 {
                    let id = &sessions[(r + i as usize) % sessions.len()];
                    let _ = resize_session(&state, id, 10 + (i % 30), 60 + (i % 40));
                }
            });
        }
        // Killers joining mid-flight.
        let killer_state = state.clone();
        let killer_ids = sessions.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            for id in killer_ids {
                let _ = kill_session(&killer_state, &id);
            }
        });

        // Everything above must settle quickly — a hang here IS the bug.
        let deadline = Duration::from_secs(15);
        let mut failures = Vec::new();
        for _ in 0..8 {
            match rx.recv_timeout(deadline) {
                Ok(Ok::<(), String>(())) => {}
                Ok(Err(msg)) => failures.push(msg),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("stress did not converge within {deadline:?}")
                }
                Err(_) => break,
            }
        }
        assert!(failures.is_empty(), "unexpected failures: {failures:?}");

        // Writers finish before the killer fires at 20ms? No guarantee — so
        // poll briefly for the registry to drain (killer wins every race by
        // construction once it has run).
        let drained = {
            let mut ok = false;
            for _ in 0..100 {
                if state.live_session_count().unwrap() == 0 {
                    ok = true;
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            ok
        };
        assert!(drained, "registry must drain to zero after the kill sweep");
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
}
