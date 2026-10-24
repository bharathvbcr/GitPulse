//! Benchmark: what `gitpulse-hook notify` costs per call at 0 B, 100 KiB and
//! 4 MiB of stdin, before and after it learned to leave stdin alone when it has
//! nothing to say and to read only the fields it uses.
//!
//! Ignored by default; it measures, it does not assert. A is a hook binary
//! built from the tree before the change, B is the one under test:
//!
//! ```text
//! cargo build --release --bin gitpulse-hook
//! cp target/release/gitpulse-hook /tmp/gitpulse-hook-base   # built BEFORE the change
//! # ... apply the change, rebuild ...
//! GITPULSE_BENCH_BASE_HOOK=/tmp/gitpulse-hook-base \
//! GITPULSE_BENCH_HOOK=target/release/gitpulse-hook \
//! cargo test --release --test notify_stdin_bench -- --ignored --nocapture
//! ```
//!
//! `GITPULSE_BENCH_ROUNDS` (default 9) and `GITPULSE_BENCH_CALLS` (default 20,
//! per case per round) size the run. The cases are interleaved within every
//! round and rotated between rounds, so drift in machine load lands on all of
//! them alike. The table reports the minimum of the per-round medians (the run
//! least disturbed by the machine) and their median.
//!
//! Two situations, each at three sizes, each on A and B:
//! - `outside`: a `tool_finished` with no `GITPULSE_SESSION_ID`, the plugin
//!   installed once for every Claude Code session on the machine. Nothing to
//!   report, so the cost is all there is to save.
//! - `inside`: the same report from a GitPulse terminal, delivered to a socket
//!   that acknowledges it. The payload is a `PostToolUse` whose `tool_response`
//!   is padded to the size; at 4 MiB A refuses it (the 4 MiB limit counts the
//!   whole payload) and B delivers it.

#![cfg(unix)]

use gitpulse_lib::procguard::LockedSpawn;
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// A `PostToolUse` payload of about `size` bytes, or nothing at 0.
fn payload(size: usize) -> Vec<u8> {
    if size == 0 {
        return Vec::new();
    }
    let head = r#"{"session_id":"bench","hook_event_name":"PostToolUse","cwd":"/tmp","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"toolu_bench","tool_response":{"stdout":""#;
    let tail = r#""}}"#;
    let pad = size.saturating_sub(head.len() + tail.len());
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(head.as_bytes());
    out.resize(out.len() + pad, b'x');
    out.extend_from_slice(tail.as_bytes());
    out
}

/// A socket that answers every report `{"ok":true}` and counts them.
struct Listener {
    path: PathBuf,
    reports: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Listener {
    fn start(path: PathBuf) -> Self {
        let listener = UnixListener::bind(&path).expect("bind bench socket");
        listener.set_nonblocking(true).unwrap();
        let reports = Arc::new(AtomicUsize::new(0));
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
                        seen.fetch_add(1, Ordering::Relaxed);
                        let _ = stream.write_all(br#"{"ok":true}"#);
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(1)),
                }
            }
        });
        Self {
            path,
            reports,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn call(hook: &Path, logs: &Path, socket: &Path, session: bool, body: &[u8]) -> Duration {
    let started = Instant::now();
    let mut command = Command::new(hook);
    command
        .args(["notify", "tool_finished"])
        .env(gitpulse_lib::logging::LOG_DIR_ENV, logs)
        .env(gitpulse_lib::alerts::bridge::SOCKET_ENV, socket)
        .env_remove(gitpulse_lib::alerts::bridge::SESSION_ENV)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if session {
        command.env(gitpulse_lib::alerts::bridge::SESSION_ENV, "term-bench-1");
    }
    let mut child = command.spawn_locked().expect("spawn gitpulse-hook");
    let mut stdin = child.stdin.take().expect("stdin");
    // A hook that leaves stdin unread closes the pipe: the write then fails,
    // which is the point, so the error is not one.
    let _ = stdin.write_all(body);
    drop(stdin);
    let out = child.wait_with_output().expect("collect");
    let elapsed = started.elapsed();
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty(), "notify wrote to stdout");
    elapsed
}

fn median(samples: &mut [Duration]) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

#[test]
#[ignore = "benchmark: run with --release --ignored"]
fn notify_cost_by_stdin_size_before_and_after() {
    let base = PathBuf::from(
        std::env::var_os("GITPULSE_BENCH_BASE_HOOK")
            .expect("GITPULSE_BENCH_BASE_HOOK names the hook built before the change"),
    );
    let new = std::env::var_os("GITPULSE_BENCH_HOOK")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_gitpulse-hook")));
    let rounds = env_usize("GITPULSE_BENCH_ROUNDS", 9);
    let calls = env_usize("GITPULSE_BENCH_CALLS", 20);

    let scratch = tempfile::Builder::new()
        .prefix("nb")
        .tempdir_in("/tmp")
        .unwrap();
    let logs = scratch.path().join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let listener = Listener::start(scratch.path().join("n.sock"));

    let sizes: [(&str, usize); 3] = [("0 B", 0), ("100 KiB", 100 * 1024), ("4 MiB", 4 << 20)];
    let bodies: Vec<Vec<u8>> = sizes.iter().map(|(_, size)| payload(*size)).collect();

    // (label, hook, session, size index)
    let mut cases: Vec<(String, &Path, bool, usize)> = Vec::new();
    for (situation, session) in [("outside", false), ("inside", true)] {
        for (index, (size, _)) in sizes.iter().enumerate() {
            for (side, hook) in [("A", base.as_path()), ("B", new.as_path())] {
                cases.push((format!("{situation} {size} {side}"), hook, session, index));
            }
        }
    }
    let mut medians: Vec<Vec<Duration>> = vec![Vec::new(); cases.len()];
    for round in 0..rounds {
        for step in 0..cases.len() {
            let case = (step + round) % cases.len();
            let (_, hook, session, size) = &cases[case];
            let mut samples: Vec<Duration> = (0..calls)
                .map(|_| call(hook, &logs, &listener.path, *session, &bodies[*size]))
                .collect();
            medians[case].push(median(&mut samples));
        }
    }

    println!("| case | min median | median median |");
    println!("| --- | --- | --- |");
    for ((label, ..), rounds) in cases.iter().zip(medians.iter_mut()) {
        rounds.sort();
        println!(
            "| {label} | {:.2} ms | {:.2} ms |",
            rounds[0].as_secs_f64() * 1e3,
            rounds[rounds.len() / 2].as_secs_f64() * 1e3
        );
    }
    println!(
        "reports the socket acknowledged: {} (rounds {rounds}, calls {calls})",
        listener.reports.load(Ordering::Relaxed)
    );
}
