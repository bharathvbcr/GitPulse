//! How many `terminal-output` events a session's output costs, against real PTYs.
//!
//! Every event is one script evaluated in the webview on the process main
//! thread (`Emitter::emit` -> `Listeners::emit_js_filter` -> one
//! `Webview::eval` per webview holding a listener; see the framework's
//! `event/listener.rs` and `webview/mod.rs`). The main thread is also where
//! the webview's IPC is serviced, so the number of events a TUI costs is a
//! cost every other command pays. These tests pin the count, not the bytes: a
//! session must not emit once per `write(2)` its child makes.
//!
//! MockRuntime supplies only the event sink; shells, PTYs and children are
//! real. The listener runs on the emitting (reader) thread, so each call is
//! exactly one emit.
#![cfg(unix)]
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use gitpulse_lib::procguard::LockedSpawn;
use gitpulse_lib::terminal::{
    acknowledge_output, spawn_session, write_to_session, TerminalSessions, OUTPUT_CHUNK_BYTES,
};
use std::collections::HashMap;
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

/// What one session's events came to.
#[derive(Default, Clone)]
struct Tally {
    emits: usize,
    bytes: usize,
    /// Largest single event, in raw bytes.
    largest: usize,
    /// The bytes themselves, in arrival order.
    content: Vec<u8>,
    /// `bytes` field of every event, which is the credit it reserved.
    credit: Vec<usize>,
}

type Tallies = Arc<Mutex<HashMap<String, Tally>>>;

/// Records and acknowledges every event, so flow control never intervenes.
fn counting(app: &tauri::App<tauri::test::MockRuntime>, state: &TerminalSessions) -> Tallies {
    counting_with(app, state, true)
}

/// `strict` makes a refused acknowledgement fail the test; a test that detaches
/// a view mid-stream legitimately acknowledges credit the new view never held.
fn counting_with(
    app: &tauri::App<tauri::test::MockRuntime>,
    state: &TerminalSessions,
    strict: bool,
) -> Tallies {
    let tallies: Tallies = Arc::default();
    let record = tallies.clone();
    let ack = state.clone();
    app.listen("terminal-output", move |event| {
        let data: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
        let bytes = STANDARD.decode(data["data_b64"].as_str().unwrap()).unwrap();
        let reserved = data["bytes"].as_u64().expect("reserved length") as usize;
        assert_eq!(reserved, bytes.len(), "credit must equal the emitted bytes");
        assert!(
            (1..=OUTPUT_CHUNK_BYTES).contains(&reserved),
            "event of {reserved} bytes is outside 1..={OUTPUT_CHUNK_BYTES}"
        );
        let id = data["id"].as_str().unwrap().to_owned();
        {
            let mut all = record.lock().unwrap();
            let tally = all.entry(id.clone()).or_default();
            tally.emits += 1;
            tally.bytes += bytes.len();
            tally.largest = tally.largest.max(bytes.len());
            tally.content.extend_from_slice(&bytes);
            tally.credit.push(reserved);
        }
        let acked = acknowledge_output(&ack, &id, reserved);
        if strict {
            acked.expect("acknowledging an emitted event");
        }
    });
    tallies
}

fn exits(app: &tauri::App<tauri::test::MockRuntime>) -> mpsc::Receiver<serde_json::Value> {
    let (send, receive) = mpsc::channel();
    app.listen("terminal-exit", move |event| {
        let _ = send.send(serde_json::from_str(event.payload()).unwrap());
    });
    receive
}

/// A TUI repaint: `writes_per_frame` separate `write(2)`s of about 46 bytes
/// (cursor move, erase line, text), `frames` times at 60 Hz. Ink, ncurses and
/// every agent CLI redraw this way: a frame is many small writes, not one.
fn tui_command(frames: usize, writes_per_frame: usize) -> Vec<String> {
    let script = format!(
        r#"use Time::HiRes qw(sleep time); my $next = time;
for my $f (1..{frames}) {{
  for my $i (1..{writes_per_frame}) {{ syswrite(STDOUT, "\e[$i;1H\e[2K" . ("x" x 40)); }}
  $next += 1/60; my $d = $next - time; sleep($d) if $d > 0;
}}"#
    );
    vec!["-c".into(), format!("perl -e '{script}'")]
}

fn spawn(
    app: &tauri::App<tauri::test::MockRuntime>,
    state: &TerminalSessions,
    dir: &tempfile::TempDir,
    args: Vec<String>,
) -> String {
    spawn_session(
        app.handle(),
        state,
        dir.path().to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        Some(args),
        None,
    )
    .unwrap()
    .id
}

fn run_tui(sessions: usize, frames: usize, writes: usize) -> (Vec<Tally>, Duration) {
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    let tallies = counting(&app, &state);
    let exited = exits(&app);
    let dir = repo();
    let started = Instant::now();
    let ids: Vec<String> = (0..sessions)
        .map(|_| spawn(&app, &state, &dir, tui_command(frames, writes)))
        .collect();
    for _ in &ids {
        let exit = exited
            .recv_timeout(Duration::from_secs(60))
            .expect("every TUI session ends");
        assert!(exit["error"].is_null(), "{exit}");
        assert_eq!(exit["exit_code"], 0, "{exit}");
    }
    let elapsed = started.elapsed();
    let all = tallies.lock().unwrap();
    let per_session = ids
        .iter()
        .map(|id| all.get(id).cloned().unwrap_or_default())
        .collect();
    (per_session, elapsed)
}

const FRAMES: usize = 120;
const WRITES: usize = 8;
/// Bytes one TUI session writes: per line `ESC [ row ; 1 H` (5 bytes plus the
/// row's digits), `ESC [ 2 K` (4) and 40 columns.
fn expected_tui_bytes(frames: usize, writes: usize) -> usize {
    frames
        * (1..=writes)
            .map(|row| 5 + row.to_string().len() + 4 + 40)
            .sum::<usize>()
}

/// The defect: the reader emitted once per `read(2)`, and a 60 Hz repaint made
/// of eight small writes arrives as eight reads. Every one of those is a script
/// evaluated on the main thread.
#[test]
fn a_60hz_repaint_costs_about_one_event_per_frame_not_one_per_write() {
    let _serial = serial();
    let (tallies, elapsed) = run_tui(1, FRAMES, WRITES);
    let tally = &tallies[0];
    let writes = FRAMES * WRITES;
    eprintln!(
        "TUI 1 session: {} emits for {writes} writes ({} frames) in {:.2}s = {:.0} emits/s, {} bytes = {:.0} bytes/s, largest event {}",
        tally.emits,
        FRAMES,
        elapsed.as_secs_f64(),
        tally.emits as f64 / elapsed.as_secs_f64(),
        tally.bytes,
        tally.bytes as f64 / elapsed.as_secs_f64(),
        tally.largest
    );
    assert_eq!(
        tally.bytes,
        expected_tui_bytes(FRAMES, WRITES),
        "no byte lost"
    );
    assert!(
        tally.emits < writes / 2,
        "{} events for {writes} writes: output is not being coalesced",
        tally.emits
    );
}

/// Sixteen agent sessions repainting at once: the aggregate is what the main
/// thread sees.
#[test]
fn sixteen_repainting_sessions_stay_far_below_one_event_per_write() {
    let _serial = serial();
    let (tallies, elapsed) = run_tui(16, FRAMES, WRITES);
    let emits: usize = tallies.iter().map(|t| t.emits).sum();
    let bytes: usize = tallies.iter().map(|t| t.bytes).sum();
    let writes = 16 * FRAMES * WRITES;
    eprintln!(
        "TUI 16 sessions: {emits} emits for {writes} writes in {:.2}s = {:.0} emits/s total ({:.0}/session), {bytes} bytes = {:.0} bytes/s",
        elapsed.as_secs_f64(),
        emits as f64 / elapsed.as_secs_f64(),
        emits as f64 / elapsed.as_secs_f64() / 16.0,
        bytes as f64 / elapsed.as_secs_f64()
    );
    for tally in &tallies {
        assert_eq!(tally.bytes, expected_tui_bytes(FRAMES, WRITES));
    }
    assert!(
        emits < writes / 2,
        "{emits} events for {writes} writes across 16 sessions"
    );
}

/// The bytes a 1 MiB burst is made of.
fn pattern(total: usize) -> Vec<u8> {
    (0..total).map(|i| b'A' + (i % 26) as u8).collect()
}

fn burst_command(total: usize) -> Vec<String> {
    burst_command_then(total, "sleep 30")
}

/// The burst, then `tail` run by the same shell.
fn burst_command_then(total: usize, tail: &str) -> Vec<String> {
    vec![
        "-c".into(),
        format!(
            "stty raw -echo; perl -e 'binmode STDOUT; my $s = join(\"\", map {{ chr(65 + $_ % 26) }} 0..{}); print $s'; {tail}",
            total - 1
        ),
    ]
}

/// A 1 MiB burst arrives byte-exact in a fraction of the 256 events that 4 KiB
/// reads would make, and every event's credit is repaid exactly once.
#[test]
fn a_one_megabyte_burst_is_byte_exact_in_few_events_with_exact_credit() {
    let _serial = serial();
    const TOTAL: usize = 1024 * 1024;
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    let tallies = counting(&app, &state);
    let dir = repo();
    let started = Instant::now();
    let id = spawn(&app, &state, &dir, burst_command(TOTAL));
    // The shell stays alive after printing, so the session is still there to
    // be asked whether it owes anything.
    let want = pattern(TOTAL);
    let arrived = |tally: &Tally| tally.content.ends_with(&want);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if tallies.lock().unwrap().get(&id).is_some_and(arrived) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the burst never finished arriving"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let elapsed = started.elapsed();
    let tally = tallies.lock().unwrap().get(&id).cloned().unwrap();
    eprintln!(
        "burst 1 MiB: {} emits in {:.2}s, mean {} B/event, largest {}",
        tally.emits,
        elapsed.as_secs_f64(),
        tally.bytes / tally.emits.max(1),
        tally.largest
    );
    // `stty raw` runs first, so whatever the line discipline echoed before it
    // took effect precedes the burst; the burst is the tail, byte for byte.
    assert!(
        tally.content.len() < TOTAL + 4096,
        "{} bytes arrived for a {TOTAL}-byte burst: output was duplicated",
        tally.content.len()
    );
    assert!(
        tally.emits <= TOTAL / 8192,
        "{} events for 1 MiB: mean event under 8 KiB",
        tally.emits
    );
    // Every event's credit was repaid by the listener; nothing is owed, so one
    // more byte of acknowledgement is refused.
    assert!(
        acknowledge_output(&state, &id, 1).is_err(),
        "credit accounting drifted from what was emitted"
    );
    gitpulse_lib::terminal::kill_session(&state, &id).unwrap();
}

fn wait_for(within: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    done()
}

fn received(tallies: &Tallies, id: &str) -> usize {
    tallies.lock().unwrap().get(id).map_or(0, |t| t.bytes)
}

/// Output that looks like the front of a stream is held briefly for more. When
/// no more comes it must still be sent, long before the next byte could push
/// it out, and a prompt-sized write must not be held at all.
#[test]
fn output_that_stops_after_looking_like_a_stream_is_still_delivered() {
    let _serial = serial();
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    let tallies = counting(&app, &state);
    let dir = repo();
    // 700 bytes (over the hold threshold) in one write, then silence for good.
    let id = spawn(
        &app,
        &state,
        &dir,
        vec![
            "-c".into(),
            "stty raw -echo; perl -e 'syswrite(STDOUT, \"y\" x 700)'; sleep 30".into(),
        ],
    );
    assert!(
        wait_for(Duration::from_secs(10), || received(&tallies, &id) >= 700),
        "700 bytes followed by silence never arrived: {} did",
        received(&tallies, &id)
    );
    gitpulse_lib::terminal::kill_session(&state, &id).unwrap();

    // A prompt-sized write is not held for company either.
    let id = spawn(
        &app,
        &state,
        &dir,
        vec![
            "-c".into(),
            "stty raw -echo; perl -e 'syswrite(STDOUT, \"$ \")'; sleep 30".into(),
        ],
    );
    assert!(
        wait_for(Duration::from_secs(10), || received(&tallies, &id) >= 2),
        "a two-byte prompt never arrived"
    );
    gitpulse_lib::terminal::kill_session(&state, &id).unwrap();
}

/// What a program prints just before it exits is output too: it reaches the
/// view before the exit does, not after and not never.
#[test]
fn output_gathered_when_the_process_exits_is_delivered_before_the_exit() {
    let _serial = serial();
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    let tallies = counting(&app, &state);
    let exited = exits(&app);
    let dir = repo();
    for size in [1usize, 511, 512, 700, 4096, 16383, 16384, 16385, 40000] {
        let id = spawn(
            &app,
            &state,
            &dir,
            vec![
                "-c".into(),
                format!("stty raw -echo; perl -e 'syswrite(STDOUT, \"z\" x {size})'"),
            ],
        );
        let exit = exited.recv_timeout(Duration::from_secs(30)).unwrap();
        assert!(exit["error"].is_null(), "{size}: {exit}");
        // The exit event is emitted after the reader's last event, on the same
        // thread, so everything is already counted when it arrives.
        let got = tallies
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .unwrap_or_default();
        let zeros = got.content.iter().filter(|b| **b == b'z').count();
        assert_eq!(zeros, size, "{size}: output lost or duplicated around exit");
        assert!(
            got.content
                .iter()
                .all(|b| *b == b'z' || *b == b'\r' || *b == b'\n'),
            "{size}: unexpected bytes"
        );
    }
}

/// The view that never acknowledges is sent at most its window, told once, and
/// the process keeps running. Once it repays, delivery resumes.
#[test]
fn a_view_that_never_acknowledges_gets_one_window_then_one_notice_then_resumes() {
    let _serial = serial();
    const WINDOW: usize = 256 * 1024;
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    state.set_output_stall_timeout(Duration::from_millis(20));
    let held: Arc<Mutex<Vec<(String, usize, Vec<u8>)>>> = Arc::default();
    let keep = held.clone();
    app.listen("terminal-output", move |event| {
        let data: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
        let bytes = STANDARD.decode(data["data_b64"].as_str().unwrap()).unwrap();
        keep.lock().unwrap().push((
            data["id"].as_str().unwrap().to_owned(),
            data["bytes"].as_u64().unwrap() as usize,
            bytes,
        ));
    });
    let losses = Arc::new(Mutex::new(0usize));
    let counted = losses.clone();
    app.listen("terminal-loss", move |_| {
        *counted.lock().unwrap() += 1;
    });
    let dir = repo();
    // After the burst the shell waits for a line, then proves it is alive.
    let id = spawn(
        &app,
        &state,
        &dir,
        burst_command_then(2 * 1024 * 1024, "read line; printf RESUMED-OK; sleep 30"),
    );
    // The window fills, the stall passes, and the rest is shed.
    assert!(
        wait_for(Duration::from_secs(30), || *losses.lock().unwrap() > 0),
        "a view that never acknowledges was never told output was skipped"
    );
    std::thread::sleep(Duration::from_millis(300));
    let delivered: usize = held.lock().unwrap().iter().map(|(_, n, _)| n).sum();
    assert!(
        delivered <= WINDOW,
        "{delivered} bytes sent to a view that acknowledged none; the window is {WINDOW}"
    );
    assert!(delivered > 0, "positive control: something was delivered");
    assert_eq!(*losses.lock().unwrap(), 1, "told exactly once");
    // Repay everything: the session was never stopped, and delivery resumes.
    for (session, reserved, bytes) in held.lock().unwrap().drain(..) {
        assert_eq!(bytes.len(), reserved);
        acknowledge_output(&state, &session, reserved).unwrap();
    }
    write_to_session(&state, &id, "go\n").unwrap();
    let resumed = wait_for(Duration::from_secs(20), || {
        held.lock()
            .unwrap()
            .iter()
            .any(|(_, _, bytes)| bytes.windows(10).any(|w| w == b"RESUMED-OK"))
    });
    assert!(resumed, "delivery never resumed after the view repaid");
    gitpulse_lib::terminal::kill_session(&state, &id).unwrap();
}

/// A view that goes away mid-stream stops being sent to; the process keeps
/// running; a view that takes it over is sent to again, with credit it can
/// repay.
#[test]
fn detaching_and_reattaching_mid_stream_stops_and_restarts_delivery() {
    let _serial = serial();
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    let tallies = counting_with(&app, &state, false);
    let dir = repo();
    let id = spawn(
        &app,
        &state,
        &dir,
        vec!["-c".into(), "yes abcdefghijklmnopqrstuvwxyz".into()],
    );
    assert!(
        wait_for(Duration::from_secs(20), || received(&tallies, &id)
            > 256 * 1024),
        "the stream never got going"
    );
    assert!(gitpulse_lib::terminal::detach_session(&state, &id).unwrap());
    // Whatever was already in flight lands; then it is quiet.
    std::thread::sleep(Duration::from_millis(300));
    let settled = received(&tallies, &id);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        received(&tallies, &id),
        settled,
        "a detached session was still being sent output"
    );
    assert!(
        gitpulse_lib::terminal::list_sessions(&state)
            .iter()
            .any(|s| s.id == id && s.detached),
        "the process must still be running, detached"
    );
    gitpulse_lib::terminal::attach_session(&state, &id, 24, 80).unwrap();
    assert!(
        wait_for(Duration::from_secs(20), || received(&tallies, &id)
            > settled + 256 * 1024),
        "a reattached session never streamed again"
    );
    gitpulse_lib::terminal::kill_session(&state, &id).unwrap();
}

mod common;
