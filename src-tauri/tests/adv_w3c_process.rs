//! W3c adversary pass: the durable record of missed notify replies, driven
//! through the real `gitpulse-hook` process (the `dispatch` path with its
//! `PROCESS_DROPS` sink) against a listener that accepts and never answers.
//!
//! What the lib tests cannot say: that the process resolves the socket the way
//! the app would, that the record lands beside it, that an unwritable record
//! changes neither the exit code nor stdout, and that the record's mode is
//! private when the process (not a test) creates it.

#![cfg(unix)]

mod common;

use gitpulse_lib::alerts::bridge::{SESSION_ENV, SOCKET_ENV};
use gitpulse_lib::procguard::LockedSpawn;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Accepts connections and holds them open without a word.
struct SilentSocket {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SilentSocket {
    fn bind(path: &Path) -> Self {
        let listener = UnixListener::bind(path).expect("bind");
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut held = Vec::new();
            while !done.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => held.push(stream),
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for SilentSocket {
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
    stderr: String,
    code: Option<i32>,
    elapsed: Duration,
}

/// `notify session_ended` as a session in a GitPulse terminal runs it.
/// `socket_env` None leaves the variable unset so the hook resolves the
/// default from `home`.
fn session_ended(socket_env: Option<&Path>, home: &Path) -> Ran {
    let logs = tempfile::Builder::new()
        .prefix("al")
        .tempdir_in("/tmp")
        .unwrap();
    let mut command = Command::new(hook_path());
    command
        .args(["notify", "session_ended"])
        .env(gitpulse_lib::logging::LOG_DIR_ENV, logs.path())
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env(SESSION_ENV, "term-4-1")
        .env_remove(SOCKET_ENV)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = socket_env {
        command.env(SOCKET_ENV, path);
    }
    let started = Instant::now();
    let mut child = command.spawn_locked().expect("spawn gitpulse-hook");
    let mut pipe = child.stdin.take().unwrap();
    let _ = pipe.write_all(br#"{"session_id":"abc","hook_event_name":"SessionEnd","cwd":"/work"}"#);
    drop(pipe);
    let out = child.wait_with_output().expect("collect");
    Ran {
        stdout: out.stdout,
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code(),
        elapsed: started.elapsed(),
    }
}

fn short_dir(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("/tmp")
        .unwrap()
}

#[test]
fn adv_w3c_the_hook_process_records_a_missed_reply_beside_the_socket_it_was_told() {
    let dir = short_dir("w3a");
    let socket = dir.path().join("n.sock");
    let _listener = SilentSocket::bind(&socket);
    let ran = session_ended(Some(&socket), dir.path());
    assert!(ran.stdout.is_empty(), "{:?}", ran.stdout);
    assert!(ran.stderr.contains("got no reply"), "{}", ran.stderr);
    assert!(!ran.stderr.contains("not recorded"), "{}", ran.stderr);
    let record = PathBuf::from(format!("{}.drops", socket.display()));
    let text = std::fs::read_to_string(&record).expect("the record exists");
    assert_eq!(text.lines().count(), 1, "{text:?}");
    let line: serde_json::Value = serde_json::from_str(text.trim_end()).expect("parsable");
    assert_eq!(line["v"], 1);
    assert_eq!(line["event"], "session_ended");
    assert_eq!(line["session"], "term-4-1");
    let at = line["at"].as_u64().expect("a time");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    assert!(at <= now && now - at < 10_000, "at {at} now {now}");
    assert_eq!(
        std::fs::metadata(&record).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn adv_w3c_an_unwritable_record_changes_neither_exit_code_stdout_nor_the_stderr_head() {
    let written_dir = short_dir("w3b");
    let written_socket = written_dir.path().join("n.sock");
    let blocked_dir = short_dir("w3c");
    let blocked_socket = blocked_dir.path().join("n.sock");
    // At the 64 KiB cap, so the hook must refuse to append.
    std::fs::write(
        format!("{}.drops", blocked_socket.display()),
        vec![b'x'; 64 * 1024],
    )
    .unwrap();
    let _a = SilentSocket::bind(&written_socket);
    let _b = SilentSocket::bind(&blocked_socket);
    let recorded = session_ended(Some(&written_socket), written_dir.path());
    let refused = session_ended(Some(&blocked_socket), blocked_dir.path());
    assert_eq!(recorded.code, refused.code, "exit code moved");
    assert_eq!(recorded.stdout, refused.stdout, "stdout moved");
    assert!(
        refused.stderr.contains("not recorded"),
        "{}",
        refused.stderr
    );
    // The log line leads with a one-second timestamp, and the two runs are
    // ~300 ms apart, so about a third of the time they straddle a second
    // boundary. The head is compared without it.
    let head = |text: &str| {
        let line = text.split("scope this_process").next().unwrap_or("");
        line.split_once(' ')
            .map_or(line, |(_, rest)| rest)
            .to_owned()
    };
    assert_eq!(head(&recorded.stderr), head(&refused.stderr));
    // Inside the hook's own budget: 300 ms for the wait plus process start.
    assert!(
        refused.elapsed < common::env::coverage_relaxed(Duration::from_secs(3)),
        "{:?}",
        refused.elapsed
    );
}

/// With `GITPULSE_NOTIFY_SOCKET` unset the hook resolves the default socket in
/// the configuration directory the same way the app does, and the record lands
/// beside it: the file the app's unbound `status()` reads.
#[test]
fn adv_w3c_the_default_socket_gets_its_record_beside_it_in_the_config_directory() {
    let home = short_dir("w3d");
    let config = if cfg!(target_os = "macos") {
        home.path()
            .join("Library")
            .join("Application Support")
            .join("GitPulse")
    } else {
        home.path().join(".config").join("gitpulse")
    };
    std::fs::create_dir_all(&config).unwrap();
    let socket = config.join("agent-notify.sock");
    let _listener = SilentSocket::bind(&socket);
    let ran = session_ended(None, home.path());
    assert!(ran.stderr.contains("got no reply"), "{}", ran.stderr);
    let record = config.join("agent-notify.sock.drops");
    let text = std::fs::read_to_string(&record).expect("the record is beside the default socket");
    assert_eq!(text.lines().count(), 1, "{text:?}");
}
