//! Terminal output through a real PTY: throughput, IPC messages per MB, and
//! keystroke echo latency with and without a flood next to it.
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench terminal_pty`.
//!
//! The subject is the production path, `terminal::spawn_session` ->
//! reader thread -> `terminal-output` event -> `acknowledge_output`, with a
//! real PTY and a real `/bin/sh`. Tauri's `MockRuntime` is the only stand-in:
//! it supplies the event sink the webview would be on the other end of. So the
//! numbers are the Rust side's cost of moving bytes, with a listener that does
//! what the frontend must do per chunk (read the reserved length, repay it) and
//! nothing else. They are the baseline for the output-channel and PTY-reader
//! changes; they are not a claim about xterm.js rendering, which is
//! installed-app territory (card ft-9167572a).
//!
//! # Rows
//!
//! * `flood 16 MiB`: a shell pipeline writes 16 MiB of 'x' (no newlines, so
//!   line-discipline output processing does not inflate it) through the PTY.
//!   `mb_per_s` is bytes received / wall time; `ipc_messages_per_mb` is the
//!   number of `terminal-output` events per MiB (a chunk is at most
//!   `OUTPUT_CHUNK_BYTES`, so ~256 is the floor); `bytes_received` must equal
//!   16 MiB exactly or the run aborts, because a lossy flood is not a flood.
//! * `echo idle` / `echo during flood`: one keystroke written to a `cat`
//!   session, timed until the echo arrives as an event. The flood row repeats it
//!   with a second session writing 64 MiB beside it, so the echo competes for
//!   the same reader/IPC machinery. Each sample is 12 keystrokes; its time is
//!   the sample's median, and `echo_max_ms` its worst.
//!
//! All three are `time_gated: false`: they are properties of the OS scheduler
//! and the load of the host as much as of the code, so the baseline gate holds
//! their counters (bytes, keystrokes) and the comparison of times is by eye
//! from the JSON, with the host-load snapshot beside it.

#[cfg(unix)]
mod common;

#[cfg(unix)]
mod imp {
    use super::common::fixture::{git, trust_repo};
    use super::common::{interleave, Case, Report, Sample};
    use gitpulse_lib::terminal::{
        acknowledge_output, kill_session, shutdown_sessions, spawn_session, write_to_session,
        TerminalSessions,
    };
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use tauri::Listener;

    const FLOOD_BYTES: u64 = 16 * 1024 * 1024;
    const KEYSTROKES: usize = 12;
    /// A producer that outlives any echo sample: killed when the sample ends.
    const LONG_FLOOD: &str = "dd if=/dev/zero bs=65536 count=1024 2>/dev/null | tr '\\0' x";
    const SHORT_FLOOD: &str = "dd if=/dev/zero bs=65536 count=256 2>/dev/null | tr '\\0' x";
    const DEADLINE: Duration = Duration::from_secs(120);

    /// What the listener has seen, by session id.
    #[derive(Default)]
    struct Seen {
        bytes: Mutex<HashMap<String, u64>>,
        messages: Mutex<HashMap<String, u64>>,
        echo: Mutex<Option<(String, Sender<Instant>)>>,
        flowing: AtomicU64,
    }

    impl Seen {
        fn bytes_of(&self, id: &str) -> u64 {
            self.bytes.lock().unwrap().get(id).copied().unwrap_or(0)
        }
        fn messages_of(&self, id: &str) -> u64 {
            self.messages.lock().unwrap().get(id).copied().unwrap_or(0)
        }
    }

    pub fn main() {
        gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
        let mut report = Report::new("terminal_pty");
        let app = tauri::test::mock_builder()
            .build(gitpulse_lib::context())
            .expect("mock app");
        let state = TerminalSessions::default();
        let seen = Arc::new(Seen::default());
        let (exit_tx, exit_rx): (Sender<String>, Receiver<String>) = mpsc::channel();
        let exit_rx = Mutex::new(exit_rx);

        {
            let seen = seen.clone();
            let state = state.clone();
            app.listen("terminal-output", move |event| {
                let payload: serde_json::Value =
                    serde_json::from_str(event.payload()).expect("output payload is JSON");
                let id = payload["id"].as_str().expect("id").to_owned();
                let reserved = payload["bytes"].as_u64().expect("reserved length");
                *seen.bytes.lock().unwrap().entry(id.clone()).or_insert(0) += reserved;
                *seen.messages.lock().unwrap().entry(id.clone()).or_insert(0) += 1;
                seen.flowing.fetch_add(1, Ordering::Relaxed);
                if let Some((echo_id, tx)) = seen.echo.lock().unwrap().as_ref() {
                    if *echo_id == id {
                        let _ = tx.send(Instant::now());
                    }
                }
                // A session killed mid-flood has no credit to repay.
                let _ = acknowledge_output(&state, &id, reserved as usize);
            });
        }
        app.listen("terminal-exit", move |event| {
            let payload: serde_json::Value =
                serde_json::from_str(event.payload()).unwrap_or_default();
            let _ = exit_tx.send(payload["id"].as_str().unwrap_or_default().to_owned());
        });

        let temp = tempfile::tempdir().expect("temp dir");
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo dir");
        git(&repo, &["init", "-q", "-b", "main"]);
        trust_repo(&repo);
        let repo_path = repo.to_str().expect("utf-8").to_owned();

        let spawn = |program: &str, args: &[&str]| {
            spawn_session(
                app.handle(),
                &state,
                &repo_path,
                24,
                80,
                Some(program.to_owned()),
                Some(args.iter().map(|a| (*a).to_owned()).collect()),
                None,
            )
            .unwrap_or_else(|error| panic!("spawn {program}: {error}"))
            .id
        };

        let flood_to_exit = || -> (u64, u64, Duration) {
            let started = Instant::now();
            let id = spawn("/bin/sh", &["-c", SHORT_FLOOD]);
            // Wait for this session's exit event, then for its last output to
            // have been delivered (exit follows the final read).
            loop {
                let remaining = DEADLINE.saturating_sub(started.elapsed());
                assert!(
                    !remaining.is_zero(),
                    "the 16 MiB flood did not finish in {DEADLINE:?}"
                );
                let got = exit_rx.lock().unwrap().recv_timeout(remaining);
                match got {
                    Ok(done) if done == id => break,
                    Ok(_) => continue,
                    Err(_) => panic!("the 16 MiB flood did not finish in {DEADLINE:?}"),
                }
            }
            let elapsed = started.elapsed();
            (seen.bytes_of(&id), seen.messages_of(&id), elapsed)
        };

        let echo_sample = |flooded: bool| -> Sample {
            let flood_id = flooded.then(|| spawn("/bin/sh", &["-c", LONG_FLOOD]));
            let echo_id = spawn("/bin/cat", &[]);
            let (tx, rx) = mpsc::channel();
            *seen.echo.lock().unwrap() = Some((echo_id.clone(), tx));
            // Let the flood reach steady state, and drain cat's startup.
            std::thread::sleep(Duration::from_millis(150));
            while rx.try_recv().is_ok() {}
            let flood_before = flood_id.as_deref().map(|id| seen.bytes_of(id));
            let mut latencies = Vec::with_capacity(KEYSTROKES);
            for _ in 0..KEYSTROKES {
                let started = Instant::now();
                write_to_session(&state, &echo_id, "k")
                    .unwrap_or_else(|error| panic!("write keystroke: {error}"));
                let echoed = rx
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap_or_else(|_| panic!("no echo within 10 s (flooded: {flooded})"));
                latencies.push(echoed.duration_since(started));
                std::thread::sleep(Duration::from_millis(5));
            }
            *seen.echo.lock().unwrap() = None;
            // The flood row is only a flood if bytes were flowing while the
            // keystrokes were timed.
            let flood_during = flood_id
                .as_deref()
                .zip(flood_before)
                .map(|(id, before)| seen.bytes_of(id) - before);
            if let Some(during) = flood_during {
                assert!(
                    during > 0,
                    "the flood delivered nothing while the keystrokes were timed: this is an idle row"
                );
            }
            let _ = kill_session(&state, &echo_id);
            if let Some(flood_id) = flood_id {
                let _ = kill_session(&state, &flood_id);
            }
            latencies.sort();
            let median = latencies[latencies.len() / 2];
            let mut sample = Sample::default()
                .counter("keystrokes", KEYSTROKES as u64)
                .metric("echo_max_ms", latencies.last().unwrap().as_secs_f64() * 1e3);
            if let Some(during) = flood_during {
                sample = sample.metric("flood_kib_during_echo", during as f64 / 1024.0);
            }
            sample.elapsed(median)
        };

        report.note(format!(
            "real PTY + /bin/sh, MockRuntime event sink; the listener reads the reserved length and repays \
             it per chunk. flood = {} MiB per sample; echo = {KEYSTROKES} keystrokes per sample to a `cat` session",
            FLOOD_BYTES >> 20
        ));
        let cases = vec![
            Case::new("flood 16 MiB", || {
                let (bytes, messages, elapsed) = flood_to_exit();
                assert_eq!(
                    bytes, FLOOD_BYTES,
                    "the flood delivered {bytes} of {FLOOD_BYTES} bytes: a lossy run measures nothing"
                );
                let mib = bytes as f64 / (1024.0 * 1024.0);
                Sample::default()
                    .counter("bytes_received", bytes)
                    .metric("mb_per_s", mib / elapsed.as_secs_f64())
                    .metric("ipc_messages", messages as f64)
                    .metric("ipc_messages_per_mb", messages as f64 / mib)
                    .elapsed(elapsed)
            })
            .ungated(),
            Case::new("echo idle", || echo_sample(false)).ungated(),
            Case::new("echo during flood", || echo_sample(true)).ungated(),
        ];
        let rows = interleave(cases, report.opts.samples(9), report.opts.warmup(1));
        report.extend(rows);
        let _ = shutdown_sessions(&state);
        report.finish();
    }
}

#[cfg(unix)]
fn main() {
    imp::main();
}

#[cfg(not(unix))]
fn main() {
    println!("the PTY bench needs a Unix PTY; nothing to measure on this platform");
}
