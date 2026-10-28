//! What `gitpulse-hook notify` costs per call at 0 B, 100 KiB and 4 MiB of
//! stdin: it leaves stdin alone when it has nothing to say and reads only the
//! fields it uses.
//!
//! Run with
//! `cargo bench --manifest-path src-tauri/Cargo.toml --bench notify_stdin_bench`.
//! It started as an ignored integration test because `Cargo.toml` was held when
//! it was written; it is a registered bench on the shared harness now, so
//! `benches.yml` runs it and a work counter that changes fails the compare.
//!
//! Two situations, each at three sizes:
//! - `outside`: a `tool_finished` with no `GITPULSE_SESSION_ID`, the plugin
//!   installed once for every Claude Code session on the machine. Nothing to
//!   report, so the cost is all there is to save.
//! - `inside`: the same report from a GitPulse terminal, delivered to a socket
//!   that acknowledges it. The payload is a `PostToolUse` whose `tool_response`
//!   is padded to the size; the hook delivers it whole (a hook built before the
//!   change refused anything over 4 MiB, the whole payload counted).
//!
//! A sample is the median of [`CALLS`] calls of the hook built from this tree
//! (`CARGO_BIN_EXE_gitpulse-hook`, or `GITPULSE_BENCH_HOOK`), so a row's `min`
//! is the minimum of per-round medians: the round least disturbed by the
//! machine. The cases are interleaved and rotated between rounds, so drift in
//! machine load lands on all of them alike.
//!
//! A before/after comparison is two runs of this bench from two builds, compared
//! by `scripts/bench-compare.mjs` like every other bench. When a hook built
//! before a change is at hand, `GITPULSE_BENCH_BASE_HOOK=<path>` adds a
//! `(base)` row beside each one, measured in the same process and the same
//! rotation, which is the closest A/B there is:
//!
//! ```text
//! cargo build --release --bin gitpulse-hook
//! cp target/release/gitpulse-hook /tmp/gitpulse-hook-base   # built BEFORE the change
//! # ... apply the change, then:
//! GITPULSE_BENCH_BASE_HOOK=/tmp/gitpulse-hook-base \
//!     cargo bench --manifest-path src-tauri/Cargo.toml --bench notify_stdin_bench
//! ```
//!
//! The `(base)` rows are printed and never written to the JSON, so no baseline
//! can be made from a run that had them.
//!
//! What the gate catches here. Every row's time is `ungated`: it is dominated by
//! starting a process, which is the host's more than the code's, and a 1.5x
//! limit on a 7 ms row would fail on runner noise. The counters are gated:
//! `calls` and `stdin_bytes` are inputs, and `reports_acked` is measured, the
//! number of reports the socket acknowledged (the hook waits for the reply, so
//! the figure is exact once the hook has exited). A hook that stops delivering
//! (or delivers twice) changes it and fails the compare. **A hook that gets
//! slower but still delivers is not caught**: a 100 ms sleep added to the hook
//! (7.5 ms to 119 ms at min) compared `OK`. This bench records that cost; it
//! does not gate it.

#[cfg(unix)]
mod common;

#[cfg(unix)]
mod imp {
    use super::common::{interleave, Case, Report, Sample};
    use gitpulse_lib::procguard::LockedSpawn;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// Calls per sample; the sample is their median.
    const CALLS: usize = 20;
    /// Timed samples per case in a full run (`--smoke` cuts it).
    const ROUNDS: usize = 9;
    /// What a row measured on the base hook is named after.
    const BASE_SUFFIX: &str = " (base)";

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

    pub fn main() {
        let new = std::env::var_os("GITPULSE_BENCH_HOOK")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_gitpulse-hook")));
        // An empty value is unset: a wrapper that exports the variable empty
        // must not make the bench try to run a hook named "".
        let base = std::env::var_os("GITPULSE_BENCH_BASE_HOOK")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);
        let mut report = Report::new("notify_stdin_bench");
        report.note(format!(
            "hook {}{}; {CALLS} calls per sample, the sample is their median",
            new.display(),
            base.as_ref().map_or_else(String::new, |base| format!(
                "; base hook {} adds the (base) rows",
                base.display()
            )),
        ));

        let scratch = tempfile::Builder::new()
            .prefix("nb")
            .tempdir_in("/tmp")
            .expect("scratch directory (a unix socket path must stay short)");
        let logs = scratch.path().join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        let listener = Listener::start(scratch.path().join("n.sock"));

        let sizes: [(&str, usize); 3] = [("0 B", 0), ("100 KiB", 100 * 1024), ("4 MiB", 4 << 20)];
        let bodies: Vec<Vec<u8>> = sizes.iter().map(|(_, size)| payload(*size)).collect();

        let mut cases: Vec<Case<'_>> = Vec::new();
        for (situation, session) in [("outside", false), ("inside", true)] {
            for (index, (size, _)) in sizes.iter().enumerate() {
                let mut sides = vec![(format!("{situation} {size}"), new.clone())];
                if let Some(base) = &base {
                    sides.push((format!("{situation} {size}{BASE_SUFFIX}"), base.clone()));
                }
                for (name, hook) in sides {
                    let (logs, socket, body) = (&logs, &listener.path, &bodies[index]);
                    let (reports, stdin_bytes) = (&listener.reports, body.len() as u64);
                    cases.push(
                        Case::new(name, move || {
                            let acked = reports.load(Ordering::Relaxed);
                            let mut samples: Vec<Duration> = (0..CALLS)
                                .map(|_| call(&hook, logs, socket, session, body))
                                .collect();
                            let acked = (reports.load(Ordering::Relaxed) - acked) as u64;
                            Sample::default()
                                .counter("calls", CALLS as u64)
                                .counter("stdin_bytes", stdin_bytes)
                                .counter("reports_acked", acked)
                                .elapsed(median(&mut samples))
                        })
                        .ungated(),
                    );
                }
            }
        }
        let rows = interleave(cases, report.opts.samples(ROUNDS), report.opts.warmup(1));
        // The `(base)` rows are measured in the same rotation, so the A/B is
        // fair, but they are printed and not written to the JSON: a baseline
        // made from a run that had them would fail every run that does not.
        let (base_rows, rows): (Vec<_>, Vec<_>) = rows
            .into_iter()
            .partition(|row| row.name.ends_with(BASE_SUFFIX));
        report.extend(rows);
        report.extend_unrecorded(base_rows);
        report.finish();
    }
}

#[cfg(unix)]
fn main() {
    imp::main();
}

#[cfg(not(unix))]
fn main() {
    println!("the notify bench drives the hook over a unix socket; nothing to measure here");
}
