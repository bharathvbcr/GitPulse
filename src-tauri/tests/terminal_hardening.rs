//! Crash and wedge resilience of the terminal subsystem, against real PTYs.
//!
//! Every PTY is a child of this process, so what these pin is that nothing a
//! session does - a flood nobody reads, a hundred opening at once, bytes that
//! are not UTF-8, a storm of resizes, a write to a corpse - can leak, grow
//! without bound, or wedge the process that owns every other session.
//!
//! The tests share one process, whose descriptor, thread and memory counts are
//! the measurement, so they run one at a time behind `SERIAL`.
#![cfg(unix)]
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use gitpulse_lib::procguard::LockedSpawn;
use gitpulse_lib::terminal::{
    acknowledge_output, kill_session, list_sessions, resize_session, spawn_session,
    write_to_session, TerminalSessions,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::Listener;

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct TerminalCleanup(TerminalSessions);
impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        if let Err(error) = gitpulse_lib::terminal::shutdown_sessions(&self.0) {
            eprintln!("PTY test cleanup incomplete: {error}");
        }
    }
}

fn repo() -> tempfile::TempDir {
    // The 100-session test opens sessions faster than the production spawn
    // rate allows, and what it measures is lifetime, not that rate.
    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
    let dir = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(dir.path())
        .status_locked()
        .unwrap()
        .success());
    common::trust_repo(dir.path());
    dir
}

fn wait_until(within: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    done()
}

fn open_descriptors() -> usize {
    std::fs::read_dir("/dev/fd")
        .or_else(|_| std::fs::read_dir("/proc/self/fd"))
        .expect("list descriptors")
        .count()
}

fn threads() -> usize {
    if let Ok(tasks) = std::fs::read_dir("/proc/self/task") {
        return tasks.count();
    }
    // macOS: `ps -M` prints a header and one line per thread.
    let out = std::process::Command::new("ps")
        .args(["-M", "-p", &std::process::id().to_string()])
        .output_locked()
        .expect("ps -M");
    String::from_utf8_lossy(&out.stdout).lines().count() - 1
}

fn rss_bytes() -> u64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output_locked()
        .expect("ps");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .unwrap_or(0)
        * 1024
}

/// Acknowledges every chunk and keeps everything it was sent.
fn collecting(
    app: &tauri::App<tauri::test::MockRuntime>,
    state: &TerminalSessions,
) -> (Arc<Mutex<Vec<u8>>>, mpsc::Receiver<serde_json::Value>) {
    let output = Arc::new(Mutex::new(Vec::new()));
    let captured = output.clone();
    let ack = state.clone();
    app.listen("terminal-output", move |event| {
        let data: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
        let bytes = STANDARD.decode(data["data_b64"].as_str().unwrap()).unwrap();
        captured.lock().unwrap().extend(&bytes);
        let _ = acknowledge_output(&ack, data["id"].as_str().unwrap(), bytes.len());
    });
    let (send, exits) = mpsc::channel();
    app.listen("terminal-exit", move |event| {
        let _ = send.send(serde_json::from_str(event.payload()).unwrap());
    });
    (output, exits)
}

/// A hundred sessions opened and closed at once return every descriptor and
/// thread they took. Each is a PTY pair, a duplicated writer descriptor and a
/// reader thread; a leak per session is invisible in one test and fatal after
/// a week of agents.
#[test]
fn a_hundred_concurrent_sessions_open_and_close_without_leaking_descriptors_or_threads() {
    let _serial = serial();
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = common::terminals();
    let _cleanup = TerminalCleanup(state.clone());
    state.set_session_limit(100);
    let dir = repo();
    let path = dir.path().to_str().unwrap().to_owned();
    let handle = app.handle().clone();
    let launch = |program_args: &str| {
        spawn_session(
            &handle,
            &state,
            &path,
            24,
            80,
            Some("/bin/sh".into()),
            Some(vec!["-c".into(), program_args.into()]),
            None,
            gitpulse_lib::terminal::OutputSink::events(&handle),
        )
    };

    // Lazily created process-wide state (ledger, alert hub, pools) is not a
    // leak, so it is paid for before the baseline is taken.
    let warm = launch("sleep 300").unwrap();
    kill_session(&state, &warm.id).unwrap();
    assert!(wait_until(Duration::from_secs(10), || list_sessions(
        &state
    )
    .unwrap()
    .is_empty()));
    std::thread::sleep(Duration::from_millis(500));
    let (fds_before, threads_before) = (open_descriptors(), threads());

    for round in 0..2 {
        let ids: Vec<String> = std::thread::scope(|scope| {
            let launches: Vec<_> = (0..100)
                .map(|_| scope.spawn(|| launch("sleep 300").unwrap().id))
                .collect();
            launches.into_iter().map(|l| l.join().unwrap()).collect()
        });
        assert_eq!(list_sessions(&state).unwrap().len(), 100, "round {round}");
        std::thread::scope(|scope| {
            for id in &ids {
                let state = &state;
                scope.spawn(move || kill_session(state, id).unwrap());
            }
        });
        assert!(
            wait_until(Duration::from_secs(20), || list_sessions(&state)
                .unwrap()
                .is_empty()),
            "round {round}: sessions left after every close returned"
        );
    }

    // Reader and ledger threads end just after the entry leaves the map.
    let slack = 6;
    assert!(
        wait_until(Duration::from_secs(15), || {
            open_descriptors() <= fds_before + slack && threads() <= threads_before + slack
        }),
        "after 200 sessions: descriptors {fds_before} -> {}, threads {threads_before} -> {}",
        open_descriptors(),
        threads()
    );
}

/// A session printing to a view that never reads it costs this process no
/// memory: the reader drains the PTY, sheds what the window cannot hold, and
/// keeps nothing. This is the window hidden or minimised for a night.
#[test]
fn sixty_four_megabytes_nobody_reads_keep_memory_bounded_and_the_session_alive() {
    unread_flood(64 * 1024 * 1024, Duration::from_secs(50));
}

/// The same at 500 MB, which a debug build drains in minutes (every byte
/// passes the alert scanner). Run with:
/// `cargo test --manifest-path src-tauri/Cargo.toml --test terminal_hardening half_a_gigabyte -- --ignored`
#[test]
#[ignore = "minutes in a debug build; see the doc comment for the command"]
fn half_a_gigabyte_nobody_reads_keeps_memory_bounded_and_the_session_alive() {
    unread_flood(500 * 1024 * 1024, Duration::from_secs(600));
}

fn unread_flood(total: usize, budget: Duration) {
    let _serial = serial();
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = common::terminals();
    let _cleanup = TerminalCleanup(state.clone());
    state.set_output_stall_timeout(Duration::from_millis(10));
    let delivered = Arc::new(AtomicUsize::new(0));
    let counted = delivered.clone();
    // Never acknowledges, and keeps nothing.
    app.listen("terminal-output", move |event| {
        let data: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
        counted.fetch_add(data["bytes"].as_u64().unwrap() as usize, Ordering::SeqCst);
    });
    let (send, exits) = mpsc::channel();
    app.listen("terminal-exit", move |event| {
        let _ = send.send(serde_json::from_str::<serde_json::Value>(event.payload()).unwrap());
    });
    let dir = repo();
    // Warm the allocator's high-water mark before measuring.
    let rss_before = rss_bytes();
    let spawned = spawn_session(
        app.handle(),
        &state,
        dir.path().to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec![
            "-c".into(),
            format!("head -c {total} /dev/zero | tr '\\000' x; echo finished"),
        ]),
        None,
        gitpulse_lib::terminal::OutputSink::events(app.handle()),
    )
    .unwrap();
    let mut peak = rss_before;
    let deadline = Instant::now() + budget;
    let exit = loop {
        peak = peak.max(rss_bytes());
        match exits.recv_timeout(Duration::from_millis(250)) {
            Ok(exit) => break exit,
            Err(_) => assert!(
                Instant::now() < deadline,
                "{total} bytes did not drain in {budget:?}: the unread session is wedged ({} bytes delivered)",
                delivered.load(Ordering::SeqCst)
            ),
        }
    };
    assert_eq!(exit["exit_code"], 0, "the flood ended on its own: {exit}");
    assert!(
        peak.saturating_sub(rss_before) < 96 * 1024 * 1024,
        "resident memory grew {} MiB while {total} bytes went unread",
        peak.saturating_sub(rss_before) / (1024 * 1024)
    );
    assert!(
        delivered.load(Ordering::SeqCst) < 4 * 1024 * 1024,
        "an unacknowledged view was sent {} bytes; the window is 512 KiB",
        delivered.load(Ordering::SeqCst)
    );
    assert!(list_sessions(&state)
        .unwrap()
        .iter()
        .all(|s| s.id != spawned.id));
}

/// Output is bytes, not text. Invalid UTF-8, and multi-byte sequences cut at
/// every alignment against the 4 KiB read size, reach the view exactly as the
/// program wrote them: nothing is dropped, replaced or panicked on.
#[test]
fn invalid_utf8_and_split_multibyte_sequences_arrive_byte_exact_at_every_alignment() {
    let _serial = serial();
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = common::terminals();
    let _cleanup = TerminalCleanup(state.clone());
    let (output, exits) = collecting(&app, &state);
    let dir = repo();
    let unit: &[u8] = "é€😀".as_bytes();
    let hostile: &[u8] = &[
        0xff, 0xfe, 0xc3, 0xe2, 0x82, 0xf0, 0x9f, 0x98, 0xc0, 0x80, 0xed, 0xa0, 0x80,
    ];
    for shift in 0..8usize {
        let mut payload = vec![b'.'; shift];
        while payload.len() < 3 * 4096 + 29 {
            payload.extend_from_slice(unit);
            payload.extend_from_slice(hostile);
        }
        let file = dir.path().join(format!("payload-{shift}"));
        std::fs::write(&file, &payload).unwrap();
        output.lock().unwrap().clear();
        let spawned = spawn_session(
            app.handle(),
            &state,
            dir.path().to_str().unwrap(),
            24,
            80,
            Some("/bin/sh".into()),
            // Raw, so the line discipline does not rewrite the bytes under test.
            Some(vec![
                "-c".into(),
                format!(
                    "stty raw -echo; printf START; cat '{}'; printf END",
                    file.display()
                ),
            ]),
            None,
            gitpulse_lib::terminal::OutputSink::events(app.handle()),
        )
        .unwrap();
        let exit = exits
            .recv_timeout(Duration::from_secs(20))
            .expect("session exit");
        assert!(exit["error"].is_null(), "shift {shift}: {exit}");
        let got = output.lock().unwrap().clone();
        let body = &got[got.windows(5).position(|w| w == b"START").expect("START") + 5..];
        let body = &body[..body.len() - 3];
        assert_eq!(body.len(), payload.len(), "shift {shift} ({})", spawned.id);
        assert!(body == payload.as_slice(), "shift {shift}: bytes differ");
    }
}

/// Ten thousand resizes, among them 0x0 and the largest values a `u16` holds,
/// are all answered without error and leave the session usable at the last
/// size asked for.
#[test]
fn a_resize_storm_with_degenerate_sizes_leaves_the_session_usable() {
    let _serial = serial();
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = common::terminals();
    let _cleanup = TerminalCleanup(state.clone());
    let (output, exits) = collecting(&app, &state);
    let dir = repo();
    let spawned = spawn_session(
        app.handle(),
        &state,
        dir.path().to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec!["-c".into(), "read go; stty size".into()]),
        None,
        gitpulse_lib::terminal::OutputSink::events(app.handle()),
    )
    .unwrap();
    let started = Instant::now();
    let sizes: [(u16, u16); 6] = [
        (0, 0),
        (u16::MAX, u16::MAX),
        (1, 1),
        (0, 80),
        (24, 0),
        (50, 132),
    ];
    for step in 0..10_000usize {
        let (rows, cols) = sizes[step % sizes.len()];
        resize_session(&state, &spawned.id, rows, cols)
            .unwrap_or_else(|e| panic!("resize {step} to {rows}x{cols}: {e}"));
    }
    resize_session(&state, &spawned.id, 33, 77).unwrap();
    assert!(
        started.elapsed() < common::env::coverage_relaxed(Duration::from_secs(30)),
        "10k resizes took {:?}",
        started.elapsed()
    );
    write_to_session(&state, &spawned.id, "go\n").unwrap();
    exits
        .recv_timeout(Duration::from_secs(15))
        .expect("session exit");
    let text = String::from_utf8_lossy(&output.lock().unwrap()).into_owned();
    assert!(text.contains("33 77"), "the last size won: {text:?}");
}

/// Typing into a session whose process is gone is an error returned at once,
/// not a hang and not a panic, however many times it is tried.
#[test]
fn writing_to_a_dead_pty_fails_fast_and_never_wedges() {
    let _serial = serial();
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = common::terminals();
    let _cleanup = TerminalCleanup(state.clone());
    let (_output, exits) = collecting(&app, &state);
    let dir = repo();
    let spawned = spawn_session(
        app.handle(),
        &state,
        dir.path().to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec!["-c".into(), "exit 0".into()]),
        None,
        gitpulse_lib::terminal::OutputSink::events(app.handle()),
    )
    .unwrap();
    exits
        .recv_timeout(Duration::from_secs(10))
        .expect("session exit");
    let started = Instant::now();
    for _ in 0..1_000 {
        assert!(write_to_session(&state, &spawned.id, "x").is_err());
        assert!(resize_session(&state, &spawned.id, 24, 80).is_err());
    }
    assert!(
        started.elapsed() < common::env::coverage_relaxed(Duration::from_secs(5)),
        "{:?}",
        started.elapsed()
    );

    // And while the process is being torn down rather than already gone.
    let live = spawn_session(
        app.handle(),
        &state,
        dir.path().to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec!["-c".into(), "sleep 300".into()]),
        None,
        gitpulse_lib::terminal::OutputSink::events(app.handle()),
    )
    .unwrap();
    kill_session(&state, &live.id).unwrap();
    let started = Instant::now();
    for _ in 0..1_000 {
        assert!(write_to_session(&state, &live.id, "x").is_err());
    }
    assert!(
        started.elapsed() < common::env::coverage_relaxed(Duration::from_secs(5)),
        "{:?}",
        started.elapsed()
    );
}

mod common;
