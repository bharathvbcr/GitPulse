//! `gitpulse-hook notify` as a process: what it does with stdin.
//!
//! These run the real binary against a socket that records what it is sent.
//! `GITPULSE_TEST_HOOK` names a different binary to run them against (how the
//! change was shown to fail on the tree before it).

#![cfg(unix)]

use gitpulse_lib::alerts::bridge::{SESSION_ENV, SOCKET_ENV};
use gitpulse_lib::procguard::LockedSpawn;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Socket {
    _dir: tempfile::TempDir,
    path: PathBuf,
    reports: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Socket {
    fn start() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("ns")
            .tempdir_in("/tmp")
            .unwrap();
        let path = dir.path().join("n.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let reports = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (seen, done) = (reports.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let mut body = Vec::new();
                        let _ = (&mut stream).take(64 * 1024).read_to_end(&mut body);
                        if let Ok(value) = serde_json::from_slice(&body) {
                            seen.lock().unwrap().push(value);
                        }
                        let _ = stream.write_all(br#"{"ok":true}"#);
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        });
        Self {
            _dir: dir,
            path,
            reports,
            stop,
            thread: Some(thread),
        }
    }

    fn reports(&self) -> Vec<Value> {
        self.reports.lock().unwrap().clone()
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Ran {
    stdout: Vec<u8>,
    code: Option<i32>,
    /// Whether the whole of stdin was written before the hook let go of it.
    wrote_all: bool,
}

fn run(socket: &Socket, event: &str, session: Option<&str>, stdin: Vec<u8>) -> Ran {
    let hook = std::env::var_os("GITPULSE_TEST_HOOK")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_gitpulse-hook")));
    let logs = tempfile::Builder::new()
        .prefix("nl")
        .tempdir_in("/tmp")
        .unwrap();
    let mut command = Command::new(hook);
    command
        .args(["notify", event])
        .env(gitpulse_lib::logging::LOG_DIR_ENV, logs.path())
        .env(SOCKET_ENV, &socket.path)
        .env_remove(SESSION_ENV)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(session) = session {
        command.env(SESSION_ENV, session);
    }
    let mut child = command.spawn_locked().expect("spawn gitpulse-hook");
    let mut pipe = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || pipe.write_all(&stdin).is_ok());
    let out = child.wait_with_output().expect("collect");
    Ran {
        stdout: out.stdout,
        code: out.status.code(),
        wrote_all: writer.join().unwrap(),
    }
}

fn post_tool_use(response_bytes: usize) -> Vec<u8> {
    json!({
        "session_id": "abc",
        "hook_event_name": "PostToolUse",
        "cwd": "/work/repo",
        "tool_name": "Bash",
        "tool_input": {"command": "cargo test"},
        "tool_use_id": "toolu_1",
        "tool_response": {"stdout": "x".repeat(response_bytes)},
    })
    .to_string()
    .into_bytes()
}

/// Outside a GitPulse terminal the plugin runs `notify` for every tool call of
/// every Claude Code session on the machine, with nothing to tell. It does not
/// read what the host sends.
#[test]
fn no_session_and_four_mib_of_stdin_exits_zero_without_reading_it() {
    let socket = Socket::start();
    let ran = run(
        &socket,
        "tool_finished",
        None,
        post_tool_use(4 * 1024 * 1024),
    );
    assert_eq!(ran.code, Some(0));
    assert!(ran.stdout.is_empty());
    assert!(
        !ran.wrote_all,
        "the hook read all of stdin before deciding it had nothing to say"
    );
    assert!(socket.reports().is_empty());
}

/// The same, with stdin that is not even JSON: not parsed, so not refused.
#[test]
fn no_session_and_unparseable_stdin_is_not_parsed() {
    let socket = Socket::start();
    let ran = run(&socket, "turn_finished", None, vec![b'{'; 4 * 1024 * 1024]);
    assert_eq!(ran.code, Some(0));
    assert!(ran.stdout.is_empty());
    assert!(!ran.wrote_all);
}

/// A 5 MiB `tool_response` used to cost the report: over the 4 MiB limit the
/// whole payload was refused, and the question the tool answered stood.
#[test]
fn a_five_mib_tool_response_still_delivers_tool_finished() {
    let socket = Socket::start();
    let ran = run(
        &socket,
        "tool_finished",
        Some("term-1-a"),
        post_tool_use(5 * 1024 * 1024),
    );
    assert_eq!(ran.code, Some(0));
    assert!(ran.stdout.is_empty());
    let reports = socket.reports();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(reports[0]["event"], "tool_finished");
    assert_eq!(reports[0]["session"], "term-1-a");
    assert!(reports[0]["subject"].is_string(), "{:?}", reports[0]);
    assert!(reports[0].get("message").is_none(), "{:?}", reports[0]);
}

/// Past what is read at all, the report still goes, without its fields.
#[test]
fn a_payload_past_the_read_limit_delivers_an_empty_message_report() {
    let socket = Socket::start();
    let ran = run(
        &socket,
        "tool_finished",
        Some("term-1-a"),
        post_tool_use(17 * 1024 * 1024),
    );
    assert_eq!(ran.code, Some(0));
    assert!(ran.stdout.is_empty());
    let reports = socket.reports();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(reports[0]["event"], "tool_finished");
    assert_eq!(reports[0]["session"], "term-1-a");
    assert!(reports[0].get("message").is_none());
}

/// ft-6060bbd7: stdout belongs to the host's protocol. Malformed input reports
/// nothing and says nothing there.
#[test]
fn malformed_stdin_reports_nothing_and_writes_nothing_to_stdout() {
    let socket = Socket::start();
    for body in [&b"not json"[..], b"{\"tool_name\":", b"[1,2]", b""] {
        let ran = run(&socket, "tool_finished", Some("term-1-a"), body.to_vec());
        assert_eq!(ran.code, Some(0));
        assert!(ran.stdout.is_empty(), "stdout for {body:?}");
    }
    assert!(socket.reports().is_empty());
}

/// A banner needs the place the payload names, so outside a GitPulse terminal
/// it still reads its stdin.
#[test]
fn a_banner_outside_a_terminal_still_reads_its_payload() {
    let socket = Socket::start();
    let body = json!({"cwd": "/work/repo", "message": "Run it?"})
        .to_string()
        .into_bytes();
    let ran = run(&socket, "permission_prompt", None, body);
    assert_eq!(ran.code, Some(0));
    assert!(ran.stdout.is_empty());
    let reports = socket.reports();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(reports[0]["event"], "permission_prompt");
    assert_eq!(reports[0]["cwd"], "/work/repo");
    assert_eq!(reports[0]["message"], "Run it?");
}
