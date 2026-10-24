//! W7-L3 adversarial review: `gitpulse-hook notify` as a process, attacked on
//! quiet exit, session-id shapes, slow and oversized writers, and per-call
//! cost. `GITPULSE_TEST_HOOK` names a different binary to run against.

#![cfg(unix)]

mod common;

use gitpulse_lib::alerts::bridge::{SESSION_ENV, SOCKET_ENV};
use gitpulse_lib::procguard::LockedSpawn;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
            .prefix("an")
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
                    Err(_) => std::thread::sleep(Duration::from_millis(1)),
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

fn hook_path() -> PathBuf {
    std::env::var_os("GITPULSE_TEST_HOOK")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_gitpulse-hook")))
}

struct Ran {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    code: Option<i32>,
    writer_finished: bool,
    elapsed: Duration,
}

/// Runs `notify <event>` with `session` (None removes the variable) feeding
/// `feed` into stdin from a thread. `feed` returns when it is done or when the
/// pipe breaks.
fn run(
    socket: &Socket,
    event: &str,
    session: Option<&str>,
    feed: impl FnOnce(std::process::ChildStdin) + Send + 'static,
) -> Ran {
    let logs = tempfile::Builder::new()
        .prefix("al")
        .tempdir_in("/tmp")
        .unwrap();
    let mut command = Command::new(hook_path());
    command
        .args(["notify", event])
        .env(gitpulse_lib::logging::LOG_DIR_ENV, logs.path())
        .env(SOCKET_ENV, &socket.path)
        .env_remove(SESSION_ENV)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(session) = session {
        command.env(SESSION_ENV, session);
    }
    let started = Instant::now();
    let mut child = command.spawn_locked().expect("spawn gitpulse-hook");
    let pipe = child.stdin.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        feed(pipe);
        let _ = tx.send(());
    });
    let out = child.wait_with_output().expect("collect");
    let elapsed = started.elapsed();
    Ran {
        stdout: out.stdout,
        stderr: out.stderr,
        code: out.status.code(),
        writer_finished: rx.recv_timeout(Duration::from_secs(5)).is_ok(),
        elapsed,
    }
}

fn small_post_tool_use() -> Vec<u8> {
    json!({
        "session_id": "abc", "hook_event_name": "PostToolUse", "cwd": "/work/repo",
        "tool_name": "Bash", "tool_input": {"command": "cargo test"},
        "tool_use_id": "toolu_1", "tool_response": {"stdout": "ok"},
    })
    .to_string()
    .into_bytes()
}

fn write_all(body: Vec<u8>) -> impl FnOnce(std::process::ChildStdin) + Send + 'static {
    move |mut pipe| {
        let _ = pipe.write_all(&body);
    }
}

/// An empty or whitespace GITPULSE_SESSION_ID must never produce stdout, a
/// non-zero exit or stderr the host would show.
#[test]
fn empty_and_odd_session_ids_exit_quietly() {
    let socket = Socket::start();
    for session in ["", " ", "\t", "a b", "../../x", "\u{202e}", &"s".repeat(10_000)] {
        for event in ["tool_finished", "session_ended", "turn_finished"] {
            let ran = run(
                &socket,
                event,
                Some(session),
                write_all(small_post_tool_use()),
            );
            assert_eq!(ran.code, Some(0), "{event} session {session:?}");
            assert!(ran.stdout.is_empty(), "{event} session {session:?}");
            assert!(
                ran.stderr.is_empty(),
                "{event} session {session:?}: stderr {:?}",
                String::from_utf8_lossy(&ran.stderr)
            );
            assert!(ran.writer_finished);
        }
    }
}

/// With an EMPTY session id the hook reads nothing, as with none: a large
/// payload must not block the writer past the hook's exit.
#[test]
fn an_empty_session_id_reads_no_stdin_and_does_not_block_the_writer() {
    let socket = Socket::start();
    let body = vec![b'x'; 8 * 1024 * 1024];
    let ran = run(&socket, "tool_finished", Some(""), write_all(body));
    assert_eq!(ran.code, Some(0));
    assert!(ran.writer_finished, "the writer was left blocked");
    assert!(ran.stdout.is_empty());
    assert!(ran.stderr.is_empty(), "{:?}", String::from_utf8_lossy(&ran.stderr));
    assert!(socket.reports().is_empty());
}

/// A banner with no session still reads stdin: bounded at 16 MiB, then exits
/// 0 with the writer released (a 20 MiB writer must not hang).
#[test]
fn a_banner_without_a_session_reads_a_bounded_amount_and_releases_the_writer() {
    let socket = Socket::start();
    let mut body = br#"{"cwd":"/c","message":"m","pad":""#.to_vec();
    body.extend(std::iter::repeat(b'p').take(20 * 1024 * 1024));
    body.extend_from_slice(b"\"}");
    let ran = run(&socket, "permission_prompt", None, write_all(body));
    assert_eq!(ran.code, Some(0));
    assert!(ran.stdout.is_empty());
    assert!(ran.writer_finished, "the writer was left blocked");
    assert!(ran.elapsed < Duration::from_secs(4), "{:?}", ran.elapsed);
}

/// A banner whose host dribbles stdin forever: the hook gives up inside its
/// own 5 s budget with exit 0, quiet. (The plugin's timeout for these sync
/// entries is also 5 s, so the host may cut it first.)
#[test]
fn a_dribbling_writer_ends_in_a_quiet_exit_zero_inside_the_hook_timeout() {
    let socket = Socket::start();
    let ran = run(&socket, "permission_prompt", None, |mut pipe| {
        let deadline = Instant::now() + Duration::from_secs(9);
        while Instant::now() < deadline {
            if pipe.write_all(b" ").is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
    assert_eq!(ran.code, Some(0));
    assert!(ran.stdout.is_empty());
    assert!(
        ran.elapsed < Duration::from_millis(5_000),
        "the hook ran {:?}, past the 5 s the manifest allows it",
        ran.elapsed
    );
}

/// Per-call cost of the injected notify for a typical small PostToolUse:
/// min of N wall times of the whole process (spawn to exit), with a session,
/// against a socket that acks. Ignored: it measures.
#[test]
#[ignore = "benchmark: run with --release --ignored"]
fn per_call_cost_of_the_injected_notify_hook() {
    let socket = Socket::start();
    let n = 40;
    let mut inside = Vec::new();
    let mut outside = Vec::new();
    for _ in 0..n {
        let ran = run(
            &socket,
            "tool_finished",
            Some("term-1-a"),
            write_all(small_post_tool_use()),
        );
        assert_eq!(ran.code, Some(0));
        inside.push(ran.elapsed);
        let ran = run(&socket, "tool_finished", None, write_all(small_post_tool_use()));
        outside.push(ran.elapsed);
    }
    inside.sort();
    outside.sort();
    eprintln!(
        "notify tool_finished, small payload, N={n}: with session min={:?} median={:?}; no session min={:?} median={:?}",
        inside[0],
        inside[n / 2],
        outside[0],
        outside[n / 2]
    );
}
