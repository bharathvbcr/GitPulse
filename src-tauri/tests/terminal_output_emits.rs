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
        gitpulse_lib::terminal::OutputSink::events(app.handle()),
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
    for size in [
        1usize, 511, 512, 700, 4096, 16383, 16384, 16385, 40000, 65535, 65536, 65537, 150000,
    ] {
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

/// Events a view has been sent and not yet repaid: session, reserved length,
/// the bytes.
type Unrepaid = Arc<Mutex<Vec<(String, usize, Vec<u8>)>>>;

/// The view that never acknowledges is sent at most its window, told once, and
/// the process keeps running. Once it repays, delivery resumes.
#[test]
fn a_view_that_never_acknowledges_gets_one_window_then_one_notice_then_resumes() {
    let _serial = serial();
    const WINDOW: usize = 512 * 1024;
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    state.set_output_stall_timeout(Duration::from_millis(20));
    let held: Unrepaid = Arc::default();
    let keep = held.clone();
    // Every byte the view was sent, repaid or not.
    let everything: Arc<Mutex<Vec<u8>>> = Arc::default();
    let log = everything.clone();
    // Off while the view "never acknowledges"; on once it repays, after which it
    // acknowledges as it receives, as a live view does. Without it, whatever the
    // reader still held when the view repaid would refill the window with
    // nobody left to repay it, and the resumed output would be shed again.
    let acknowledging = Arc::new(AtomicBool::new(false));
    let repay = (acknowledging.clone(), state.clone());
    app.listen("terminal-output", move |event| {
        let data: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
        let bytes = STANDARD.decode(data["data_b64"].as_str().unwrap()).unwrap();
        let id = data["id"].as_str().unwrap().to_owned();
        let reserved = data["bytes"].as_u64().unwrap() as usize;
        // Decided under the same lock the drain below holds, so an event is
        // either drained and repaid there or repaid here, never both or neither.
        let mut unrepaid = keep.lock().unwrap();
        log.lock().unwrap().extend_from_slice(&bytes);
        if repay.0.load(Ordering::SeqCst) {
            acknowledge_output(&repay.1, &id, reserved).unwrap();
        } else {
            unrepaid.push((id, reserved, bytes));
        }
    });
    let losses = Arc::new(Mutex::new(0usize));
    let counted = losses.clone();
    app.listen("terminal-loss", move |_| {
        *counted.lock().unwrap() += 1;
    });
    let dir = repo();
    // After the burst the shell says so out of band (a file: the view is being
    // shed, so it cannot be told in-band), waits for a line, then proves it is
    // alive.
    let burst_done = dir.path().join("burst-done");
    let tail = format!(
        "touch '{}'; read line; printf RESUMED-OK; sleep 30",
        burst_done.display()
    );
    let id = spawn(
        &app,
        &state,
        &dir,
        burst_command_then(2 * 1024 * 1024, &tail),
    );
    // The window fills, the stall passes, and the rest is shed.
    assert!(
        wait_for(Duration::from_secs(60), || *losses.lock().unwrap() > 0),
        "a view that never acknowledges was never told output was skipped"
    );
    // The whole burst has been written. With no acknowledgement the window
    // stays shut, so nothing more is delivered or reported from here on.
    assert!(
        wait_for(Duration::from_secs(60), || burst_done.exists()),
        "the burst never finished being written"
    );
    let delivered: usize = held.lock().unwrap().iter().map(|(_, n, _)| n).sum();
    assert!(
        delivered <= WINDOW,
        "{delivered} bytes sent to a view that acknowledged none; the window is {WINDOW}"
    );
    assert!(delivered > 0, "positive control: something was delivered");
    assert_eq!(*losses.lock().unwrap(), 1, "told exactly once");
    // Repay everything: the session was never stopped, and delivery resumes.
    {
        let mut unrepaid = held.lock().unwrap();
        for (session, reserved, bytes) in unrepaid.drain(..) {
            assert_eq!(bytes.len(), reserved);
            acknowledge_output(&state, &session, reserved).unwrap();
        }
        acknowledging.store(true, Ordering::SeqCst);
    }
    write_to_session(&state, &id, "go\n").unwrap();
    let resumed = wait_for(Duration::from_secs(60), || {
        everything
            .lock()
            .unwrap()
            .windows(10)
            .any(|w| w == b"RESUMED-OK")
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
            > 512 * 1024),
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
    gitpulse_lib::terminal::attach_session(
        &state,
        &id,
        24,
        80,
        gitpulse_lib::terminal::OutputSink::events(app.handle()),
    )
    .unwrap();
    assert!(
        wait_for(Duration::from_secs(20), || received(&tallies, &id)
            > settled + 512 * 1024),
        "a reattached session never streamed again"
    );
    gitpulse_lib::terminal::kill_session(&state, &id).unwrap();
}

/// When each `#` a session printed was heard, per session, acknowledging
/// every event as the view does.
type Arrivals = Arc<Mutex<HashMap<String, Vec<Instant>>>>;

fn hearing_marks(app: &tauri::App<tauri::test::MockRuntime>, state: &TerminalSessions) -> Arrivals {
    let arrivals: Arrivals = Arc::default();
    let record = arrivals.clone();
    let ack = state.clone();
    app.listen("terminal-output", move |event| {
        let heard = Instant::now();
        let data: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
        let bytes = STANDARD.decode(data["data_b64"].as_str().unwrap()).unwrap();
        let id = data["id"].as_str().unwrap().to_owned();
        let marks = bytes.iter().filter(|b| **b == b'#').count();
        if marks > 0 {
            record
                .lock()
                .unwrap()
                .entry(id.clone())
                .or_default()
                .extend(std::iter::repeat_n(heard, marks));
        }
        let _ = acknowledge_output(&ack, &id, bytes.len());
    });
    arrivals
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

/// Types `keys` single `#` keystrokes into `id`, each once the last echo came
/// back, and returns how long each echo took to be emitted, sorted.
fn echo_latencies(
    state: &TerminalSessions,
    arrivals: &Arrivals,
    id: &str,
    keys: usize,
) -> Vec<Duration> {
    let mut took = Vec::with_capacity(keys);
    // Echoes already heard from this session belong to an earlier run.
    let before = arrivals.lock().unwrap().get(id).map_or(0, Vec::len);
    for k in before..before + keys {
        let sent = Instant::now();
        write_to_session(state, id, "#").unwrap();
        let echoed = wait_for(Duration::from_secs(10), || {
            arrivals
                .lock()
                .unwrap()
                .get(id)
                .is_some_and(|a| a.len() > k)
        });
        assert!(echoed, "keystroke {k} was never echoed");
        let heard = arrivals.lock().unwrap()[id][k];
        assert!(heard >= sent, "echo {k} was heard before it was typed");
        took.push(heard - sent);
        std::thread::sleep(Duration::from_millis(3));
    }
    took.sort();
    took
}

/// A typed key's echo is a write under the gather threshold, so it is sent at
/// once: a flood on another session does not hold it at all (each session has
/// its own reader), and a flood on the same session holds it at most for the
/// gather bound, never for the flood.
#[test]
fn a_typed_keys_echo_is_not_held_by_a_flood_on_another_session_or_its_own() {
    const KEYS: usize = 60;
    let _serial = serial();
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    let arrivals = hearing_marks(&app, &state);
    let dir = repo();
    // `cat` echoes each key back; the tty itself echoes nothing.
    let echo = || {
        spawn(
            &app,
            &state,
            &dir,
            vec!["-c".into(), "stty raw -echo; exec cat".into()],
        )
    };
    let quiet = echo();
    std::thread::sleep(Duration::from_millis(300));
    let idle = echo_latencies(&state, &arrivals, &quiet, KEYS);

    // Another session prints a mebibyte at a time, over and over.
    let flood = spawn(
        &app,
        &state,
        &dir,
        vec![
            "-c".into(),
            "stty raw -echo; while :; do head -c 1048576 /dev/zero | tr '\\0' 'f'; done".into(),
        ],
    );
    std::thread::sleep(Duration::from_millis(300));
    let beside = echo_latencies(&state, &arrivals, &quiet, KEYS);
    gitpulse_lib::terminal::kill_session(&state, &flood).unwrap();

    // The same session floods while it echoes.
    let own = spawn(
        &app,
        &state,
        &dir,
        vec![
            "-c".into(),
            "stty raw -echo; (while :; do head -c 1048576 /dev/zero | tr '\\0' 'f'; done) & exec cat"
                .into(),
        ],
    );
    std::thread::sleep(Duration::from_millis(300));
    let within = echo_latencies(&state, &arrivals, &own, KEYS);

    for (name, took) in [
        ("idle", &idle),
        ("beside a flood", &beside),
        ("inside a flood", &within),
    ] {
        println!(
            "echo {name}: min {:?} median {:?} p90 {:?} max {:?} over {KEYS} keys",
            took[0],
            percentile(took, 0.5),
            percentile(took, 0.9),
            took[took.len() - 1]
        );
    }
    let bound = |d: Duration| common::env::coverage_relaxed(d);
    // A key beside a flood is as quick as a key on an idle host: its reader is
    // not the flood's. Loose bounds, a loaded host is the point.
    assert!(
        percentile(&beside, 0.5) < bound(Duration::from_millis(30)),
        "the median echo beside a flood took {:?}",
        percentile(&beside, 0.5)
    );
    // Inside a flood the echo shares one reader with it, which sends at most
    // every 8 ms while it gathers, plus a 64 KiB message. Still nowhere near the
    // flood's length.
    assert!(
        percentile(&within, 0.5) < bound(Duration::from_millis(50)),
        "the median echo inside a flood took {:?}",
        percentile(&within, 0.5)
    );
    assert!(
        within[within.len() - 1] < bound(Duration::from_secs(1)),
        "an echo inside a flood took {:?}",
        within[within.len() - 1]
    );
    gitpulse_lib::terminal::kill_session(&state, &own).unwrap();
    gitpulse_lib::terminal::kill_session(&state, &quiet).unwrap();
}

/// Gathering puts event boundaries wherever 64 KiB falls, which is inside a
/// UTF-8 character or an escape sequence as often as not. The bytes must
/// still arrive exactly, each event within its bound, and the boundaries must
/// really have fallen inside characters and sequences for this to mean
/// anything.
#[test]
fn characters_and_escape_sequences_split_across_events_arrive_byte_exact() {
    let _serial = serial();
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    let boundaries: Arc<Mutex<Vec<usize>>> = Arc::default();
    let tallies = counting(&app, &state);
    let mark = boundaries.clone();
    let sizes = tallies.clone();
    app.listen("terminal-output", move |_| {
        // After `counting`'s listener: the content so far ends at a boundary.
        if let Some(t) = sizes.lock().unwrap().values().next() {
            mark.lock().unwrap().push(t.content.len());
        }
    });
    let dir = repo();
    // 2-, 3- and 4-byte characters and CSI/OSC sequences, 2 bytes short of
    // a multiple of 64 KiB per repetition so boundaries walk through them.
    let unit = "é世🚀\u{1b}[38;5;196mX\u{1b}[0m\u{1b}]0;t\u{7}ab";
    let reps = 100_000usize;
    let want: Vec<u8> = unit.repeat(reps).into_bytes();
    let id = spawn(
        &app,
        &state,
        &dir,
        vec![
            "-c".into(),
            format!(
                "stty raw -echo; perl -CS -e 'binmode STDOUT, \":utf8\"; print \"\\x{{e9}}\\x{{4e16}}\\x{{1f680}}\\e[38;5;196mX\\e[0m\\e]0;t\\aab\" x {reps}'; sleep 30"
            ),
        ],
    );
    assert!(
        wait_for(Duration::from_secs(60), || tallies
            .lock()
            .unwrap()
            .get(&id)
            .is_some_and(|t| t.content.ends_with(&want))),
        "the stream never arrived whole: {} of {} bytes",
        tallies
            .lock()
            .unwrap()
            .get(&id)
            .map_or(0, |t| t.content.len()),
        want.len()
    );
    let tally = tallies.lock().unwrap().get(&id).cloned().unwrap();
    assert!(
        tally.content.len() < want.len() + 64,
        "output was duplicated: {} bytes for {}",
        tally.content.len(),
        want.len()
    );
    let start = tally.content.len() - want.len();
    let ends: Vec<usize> = boundaries
        .lock()
        .unwrap()
        .iter()
        .filter(|end| **end > start && **end < tally.content.len())
        .map(|end| end - start)
        .collect();
    let inside_char = ends
        .iter()
        .filter(|end| (want[**end] & 0xC0) == 0x80)
        .count();
    // A boundary at `at` splits the sequence that starts before it and ends
    // after it: its ESC went in one event, the rest in the next.
    let sequences: Vec<(usize, usize)> = ["\u{1b}[38;5;196m", "\u{1b}[0m", "\u{1b}]0;t\u{7}"]
        .iter()
        .map(|seq| (unit.find(seq).unwrap(), seq.len()))
        .collect();
    let escape_at = |offset: usize| {
        let at = offset % unit.len();
        sequences
            .iter()
            .any(|(begin, len)| at > *begin && at < begin + len)
    };
    let inside_escape = ends.iter().filter(|end| escape_at(**end)).count();
    println!(
        "{} events for {} bytes; {} boundaries inside a UTF-8 character, {} inside an escape sequence",
        tally.emits,
        want.len(),
        inside_char,
        inside_escape
    );
    assert!(inside_char > 0, "no event boundary fell inside a character");
    assert!(
        inside_escape > 0,
        "no event boundary fell inside an escape sequence"
    );
    assert!(
        acknowledge_output(&state, &id, 1).is_err(),
        "credit accounting drifted from what was emitted"
    );
    gitpulse_lib::terminal::kill_session(&state, &id).unwrap();
}

/// The view repays late and in batches, as its ack coalescer does: one
/// acknowledgement for many events, sent on a timer. It stalls long enough to
/// be shed, then repays everything it held. Every acknowledgement is accepted,
/// nothing is owed at the end, and delivery resumes.
#[test]
fn coalesced_late_acknowledgements_across_a_shed_keep_credit_exact() {
    let _serial = serial();
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    state.set_output_stall_timeout(Duration::from_millis(50));
    let owed: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
    let tail: Arc<Mutex<Vec<u8>>> = Arc::default();
    let largest = Arc::new(Mutex::new(0usize));
    {
        let (owed, tail, largest) = (owed.clone(), tail.clone(), largest.clone());
        app.listen("terminal-output", move |event| {
            let data: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
            let bytes = STANDARD.decode(data["data_b64"].as_str().unwrap()).unwrap();
            let reserved = data["bytes"].as_u64().unwrap() as usize;
            assert_eq!(reserved, bytes.len());
            let mut big = largest.lock().unwrap();
            *big = (*big).max(reserved);
            *owed
                .lock()
                .unwrap()
                .entry(data["id"].as_str().unwrap().to_owned())
                .or_default() += reserved;
            let mut tail = tail.lock().unwrap();
            tail.extend_from_slice(&bytes);
            let keep = tail.len().saturating_sub(64);
            tail.drain(..keep);
        });
    }
    let losses = Arc::new(Mutex::new(0usize));
    {
        let losses = losses.clone();
        app.listen("terminal-loss", move |_| *losses.lock().unwrap() += 1);
    }
    let dir = repo();
    let id = spawn(
        &app,
        &state,
        &dir,
        burst_command_then(3 * 1024 * 1024, "read line; printf RESUMED-OK; sleep 30"),
    );
    let refused = Arc::new(Mutex::new(Vec::<String>::new()));
    let acker = {
        let (state, owed, refused) = (state.clone(), owed.clone(), refused.clone());
        let losses = losses.clone();
        std::thread::spawn(move || {
            let mut stalled = false;
            let deadline = Instant::now() + Duration::from_secs(60);
            while Instant::now() < deadline {
                // One stall, long enough to be shed, once the stream is going.
                if !stalled && owed.lock().unwrap().values().sum::<usize>() > 64 * 1024 {
                    stalled = true;
                    std::thread::sleep(Duration::from_millis(400));
                }
                let batch: Vec<(String, usize)> = owed.lock().unwrap().drain().collect();
                for (session, bytes) in batch {
                    if let Err(error) = acknowledge_output(&state, &session, bytes) {
                        refused.lock().unwrap().push(error);
                    }
                }
                if stalled && *losses.lock().unwrap() > 0 {
                    return;
                }
                std::thread::sleep(Duration::from_millis(8));
            }
        })
    };
    acker.join().unwrap();
    assert!(
        *losses.lock().unwrap() > 0,
        "the stalled view was never shed"
    );
    // Keep repaying, coalesced, until the shell is asked to show it is alive.
    write_to_session(&state, &id, "go\n").unwrap();
    let resumed = wait_for(Duration::from_secs(30), || {
        let batch: Vec<(String, usize)> = owed.lock().unwrap().drain().collect();
        for (session, bytes) in batch {
            if let Err(error) = acknowledge_output(&state, &session, bytes) {
                refused.lock().unwrap().push(error);
            }
        }
        tail.lock().unwrap().windows(10).any(|w| w == b"RESUMED-OK")
    });
    assert!(resumed, "delivery never resumed after the view repaid");
    std::thread::sleep(Duration::from_millis(100));
    let batch: Vec<(String, usize)> = owed.lock().unwrap().drain().collect();
    for (session, bytes) in batch {
        if let Err(error) = acknowledge_output(&state, &session, bytes) {
            refused.lock().unwrap().push(error);
        }
    }
    let refused = refused.lock().unwrap().clone();
    assert!(refused.is_empty(), "acknowledgements refused: {refused:?}");
    assert!(
        acknowledge_output(&state, &id, 1).is_err(),
        "credit accounting drifted: more was acknowledged than emitted, or the reverse"
    );
    println!(
        "{} loss notice(s); largest event {} bytes",
        *losses.lock().unwrap(),
        *largest.lock().unwrap()
    );
    assert!(*largest.lock().unwrap() <= OUTPUT_CHUNK_BYTES);
    gitpulse_lib::terminal::kill_session(&state, &id).unwrap();
}

// ---------------------------------------------------------------------------
// The production path: a view's channel of raw bytes (`OutputSink::channel`).
//
// Everything above counts events through `OutputSink::events`, which re-emits a
// chunk as a `terminal-output` event for an in-process listener. What a real
// view receives is a `tauri::ipc::Channel`: raw bytes, one message per chunk,
// then one end marker before the exit event. These tests are that view.
// ---------------------------------------------------------------------------

use gitpulse_lib::terminal::{OutputSink, END_OF_OUTPUT};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tauri::ipc::{Channel, InvokeResponseBody};

/// What one channel was sent.
#[derive(Default)]
struct Received {
    bytes: Vec<u8>,
    /// Length of each raw message, in order.
    messages: Vec<usize>,
    /// Index into `messages` at which the end marker arrived.
    ended_after: Option<usize>,
    /// Raw messages that arrived after the end marker.
    after_end: usize,
}

/// A view of one session: its channel, and an acknowledger that repays what
/// the channel was sent as a real view does once it has painted.
struct Viewer {
    received: Arc<Mutex<Received>>,
    owed: Arc<AtomicUsize>,
    id: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    acker: Option<std::thread::JoinHandle<()>>,
}

impl Viewer {
    fn new(state: &TerminalSessions, acking: bool) -> (Self, OutputSink) {
        let received: Arc<Mutex<Received>> = Arc::default();
        let owed = Arc::new(AtomicUsize::new(0));
        let (record, count) = (received.clone(), owed.clone());
        let channel = Channel::new(move |body| {
            let mut seen = record.lock().unwrap();
            match body {
                InvokeResponseBody::Raw(bytes) => {
                    if seen.ended_after.is_some() {
                        seen.after_end += 1;
                    }
                    count.fetch_add(bytes.len(), Ordering::SeqCst);
                    seen.messages.push(bytes.len());
                    seen.bytes.extend_from_slice(&bytes);
                }
                InvokeResponseBody::Json(json) => {
                    assert_eq!(json, END_OF_OUTPUT, "the only JSON a channel carries");
                    assert!(seen.ended_after.is_none(), "the end marker came twice");
                    seen.ended_after = Some(seen.messages.len());
                }
            }
            Ok(())
        });
        let id: Arc<Mutex<Option<String>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let acker = acking.then(|| {
            let (state, owed, id, stop) = (state.clone(), owed.clone(), id.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let session = id.lock().unwrap().clone();
                    if let Some(session) = session {
                        let bytes = owed.swap(0, Ordering::SeqCst);
                        if bytes > 0 {
                            let _ = acknowledge_output(&state, &session, bytes);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            })
        });
        (
            Self {
                received,
                owed,
                id,
                stop,
                acker,
            },
            OutputSink::channel(channel),
        )
    }

    fn bind(&self, session: &str) {
        *self.id.lock().unwrap() = Some(session.to_owned());
    }

    fn bytes(&self) -> usize {
        self.received.lock().unwrap().bytes.len()
    }

    fn messages(&self) -> usize {
        self.received.lock().unwrap().messages.len()
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(acker) = self.acker.take() {
            let _ = acker.join();
        }
        let _ = &self.owed;
    }
}

fn spawn_for(
    app: &tauri::App<tauri::test::MockRuntime>,
    state: &TerminalSessions,
    dir: &tempfile::TempDir,
    script: String,
    sink: OutputSink,
) -> String {
    spawn_session(
        app.handle(),
        state,
        dir.path().to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec!["-c".into(), script]),
        None,
        sink,
    )
    .unwrap()
    .id
}

/// The defect: output reached a view as a global event per read, base64
/// encoded, and the end of a session was an event that could overtake bytes
/// still being fetched. A view's channel gets the raw bytes in order, in
/// messages of up to `OUTPUT_CHUNK_BYTES`, and a marker after the last byte
/// that arrives before the exit event does.
#[test]
fn a_views_channel_gets_raw_bytes_in_order_then_one_end_marker_before_the_exit() {
    let _serial = serial();
    const TOTAL: usize = 2 * 1024 * 1024;
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    let dir = repo();
    let (viewer, sink) = Viewer::new(&state, true);
    // One listener both samples the view and wakes the test, so the sample is
    // always taken before the test proceeds. Two listeners ran in no fixed order,
    // and the test could read the flag before the sampler had set it.
    let (exit_sent, exited) = mpsc::channel();
    {
        let seen = viewer.received.clone();
        app.listen("terminal-exit", move |_| {
            let ended = seen.lock().unwrap().ended_after.is_some();
            let _ = exit_sent.send(ended);
        });
    }
    let script = burst_command_then(TOTAL, "exit 0").pop().unwrap();
    let id = spawn_for(&app, &state, &dir, script, sink);
    viewer.bind(&id);
    let ended_before_exit = exited.recv_timeout(Duration::from_secs(120)).expect("exit");
    let seen = viewer.received.lock().unwrap();
    let want = pattern(TOTAL);
    assert!(
        seen.bytes.ends_with(&want),
        "the burst did not arrive whole and in order: {} bytes",
        seen.bytes.len()
    );
    assert!(seen.bytes.len() < TOTAL + 4096, "output was duplicated");
    assert!(
        seen.messages
            .iter()
            .all(|n| (1..=OUTPUT_CHUNK_BYTES).contains(n)),
        "a message was empty or longer than {OUTPUT_CHUNK_BYTES}: largest {:?}",
        seen.messages.iter().max()
    );
    assert_eq!(
        seen.ended_after,
        Some(seen.messages.len()),
        "the end marker is the last message"
    );
    assert_eq!(seen.after_end, 0);
    assert!(
        ended_before_exit,
        "the exit event overtook the end of the output"
    );
    println!(
        "{} messages for {} bytes: mean {} B, per MiB {:.1}",
        seen.messages.len(),
        seen.bytes.len(),
        seen.bytes.len() / seen.messages.len().max(1),
        seen.messages.len() as f64 / (seen.bytes.len() as f64 / (1024.0 * 1024.0))
    );
    // One message per 64 KiB is 16 per MiB. The reader sends what it has
    // gathered whenever the tty runs dry for a couple of milliseconds, and on a
    // host with no core to spare the writer is descheduled for longer than that
    // all the time (120 to 340 per MiB measured at load 150 to 300 on this
    // machine's cores; the per-read defect makes about 1,024 and a starved
    // writer can approach it), so how many messages a flood makes is a function
    // of scheduling, not of the code. Where the host can keep the writer
    // running the count is pinned tightly. Where it cannot, what still
    // separates coalescing from a message per read is that the reader, when it
    // is the one descheduled, finds the tty full and sends a large message: the
    // per-read defect never makes one near the chunk size.
    let per_mib = seen.messages.len() as f64 / (seen.bytes.len() as f64 / (1024.0 * 1024.0));
    let largest = seen.messages.iter().copied().max().unwrap_or(0);
    let quiet_host =
        common::env::coverage_relaxed(Duration::from_secs(1)) == Duration::from_secs(1);
    if quiet_host {
        assert!(
            per_mib <= 24.0,
            "{per_mib:.1} messages per MiB: a flood is sent in messages of up to 64 KiB"
        );
    } else {
        assert!(
            largest >= OUTPUT_CHUNK_BYTES / 2,
            "largest message {largest} of {} ({per_mib:.1} per MiB): a flood is not being gathered into large messages",
            seen.messages.len()
        );
    }
    println!("largest message {largest}, quiet host {quiet_host}");
    drop(seen);
    // Everything sent was repaid, and the session has left nothing to repay.
    assert!(
        wait_for(Duration::from_secs(5), || viewer
            .owed
            .load(Ordering::SeqCst)
            == 0),
        "credit was left outstanding"
    );
}

/// A reloaded page's replacement brings a channel of its own. Output goes to
/// it from the attach on; the channel of the page that went away is not sent
/// to again (its messages would run in a page that no longer has the callback).
#[test]
fn attaching_a_view_moves_the_output_to_its_channel() {
    let _serial = serial();
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    let dir = repo();
    let (first, first_sink) = Viewer::new(&state, true);
    let id = spawn_for(
        &app,
        &state,
        &dir,
        "stty raw -echo; exec cat".into(),
        first_sink,
    );
    first.bind(&id);
    write_to_session(&state, &id, "ping").unwrap();
    assert!(
        wait_for(Duration::from_secs(10), || first.bytes() >= 4),
        "the first view never saw its echo"
    );
    assert!(gitpulse_lib::terminal::detach_session(&state, &id).unwrap());
    let before = first.bytes();
    let (second, second_sink) = Viewer::new(&state, true);
    gitpulse_lib::terminal::attach_session(&state, &id, 24, 80, second_sink).unwrap();
    second.bind(&id);
    write_to_session(&state, &id, "pong").unwrap();
    assert!(
        wait_for(Duration::from_secs(10), || {
            String::from_utf8_lossy(&second.received.lock().unwrap().bytes).contains("pong")
        }),
        "the new view never saw its echo"
    );
    assert_eq!(
        first.bytes(),
        before,
        "the first view's channel was sent to after the second took over"
    );
}

/// A5. A session on a tab nobody is looking at is sent a few times a second
/// instead of at frame rate, with every byte still delivered and no notice
/// that anything was skipped: its credit is repaid, only later.
#[test]
fn a_hidden_sessions_output_is_batched_at_least_ten_times_harder_and_still_whole() {
    let _serial = serial();
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    let losses = Arc::new(AtomicUsize::new(0));
    {
        let losses = losses.clone();
        app.listen("terminal-loss", move |_| {
            losses.fetch_add(1, Ordering::SeqCst);
        });
    }
    let dir = repo();
    // 150 bursts of 2 KiB, 10 ms apart: a stream that is nowhere near full.
    let script = "stty raw -echo; perl -e '$|=1; select(undef,undef,undef,0.4); for (1..150) { syswrite(STDOUT, \"s\" x 2048); select(undef,undef,undef,0.01) }'; sleep 30".to_string();
    let (seen, seen_sink) = Viewer::new(&state, true);
    let (hidden, hidden_sink) = Viewer::new(&state, true);
    let visible_id = spawn_for(&app, &state, &dir, script.clone(), seen_sink);
    seen.bind(&visible_id);
    let hidden_id = spawn_for(&app, &state, &dir, script, hidden_sink);
    hidden.bind(&hidden_id);
    gitpulse_lib::terminal::set_background_sessions(&state, std::slice::from_ref(&hidden_id));
    let total = 150 * 2048;
    assert!(
        wait_for(Duration::from_secs(30), || seen.bytes() >= total
            && hidden.bytes() >= total),
        "the streams did not both arrive: visible {} hidden {} of {total}",
        seen.bytes(),
        hidden.bytes()
    );
    let (visible_messages, hidden_messages) = (seen.messages(), hidden.messages());
    println!("visible {visible_messages} messages, hidden {hidden_messages}");
    assert!(
        hidden_messages * 10 <= visible_messages,
        "hidden {hidden_messages} messages vs visible {visible_messages}: not 10x fewer"
    );
    assert_eq!(
        hidden.bytes(),
        seen.bytes(),
        "the hidden session lost bytes"
    );
    assert_eq!(losses.load(Ordering::SeqCst), 0, "a loss notice was raised");
}

mod common;
