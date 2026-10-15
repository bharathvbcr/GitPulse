//! A session ends when its process does, or when someone closes it — never
//! because the view showing it fell behind or went away.
//!
//! The defect these pin: the reader killed the agent when its view did not
//! acknowledge 256 KiB of output within the stall timeout (a hidden or
//! minimized window the OS throttles, a busy page), and a tab that unmounted
//! for any reason other than Close took its process with it.
//!
//! Real shells and real PTYs through the production functions; MockRuntime
//! supplies only the event sink.
#![cfg(unix)]
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use gitpulse_lib::procguard::LockedSpawn;
use gitpulse_lib::terminal::{
    acknowledge_output, attach_session, detach_session, kill_session, list_sessions, spawn_session,
    TerminalSessions,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::Listener;

mod common;

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

/// A view that can stop acknowledging, as a throttled window does, and
/// later catch up by repaying everything it was sent.
struct View {
    app: tauri::App<tauri::test::MockRuntime>,
    state: TerminalSessions,
    /// While false, chunks are received but not acknowledged.
    acking: Arc<AtomicBool>,
    /// Bytes received and not yet acknowledged, per session.
    owed: Arc<Mutex<Vec<(String, usize)>>>,
    received: Arc<AtomicUsize>,
    /// Acknowledgements refused as stale (only ever after a detach).
    stale: Arc<AtomicUsize>,
    losses: Arc<Mutex<Vec<serde_json::Value>>>,
    exits: mpsc::Receiver<serde_json::Value>,
}

impl View {
    fn new(stall: Duration) -> Self {
        let app = tauri::test::mock_builder()
            .build(gitpulse_lib::context())
            .unwrap();
        let state = TerminalSessions::default();
        state.set_output_stall_timeout(stall);
        let acking = Arc::new(AtomicBool::new(true));
        let owed = Arc::new(Mutex::new(Vec::new()));
        let received = Arc::new(AtomicUsize::new(0));
        let stale = Arc::new(AtomicUsize::new(0));
        let (ack_state, ack_on, ack_owed, ack_received, ack_stale) = (
            state.clone(),
            acking.clone(),
            owed.clone(),
            received.clone(),
            stale.clone(),
        );
        app.listen("terminal-output", move |event| {
            let data: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
            let id = data["id"].as_str().unwrap().to_string();
            let bytes = STANDARD
                .decode(data["data_b64"].as_str().unwrap())
                .unwrap()
                .len();
            assert_eq!(data["bytes"].as_u64(), Some(bytes as u64));
            ack_received.fetch_add(bytes, Ordering::SeqCst);
            if ack_on.load(Ordering::SeqCst) {
                // A chunk in flight when its session was detached is acked
                // after the detach forgave the window, and refused — as the
                // real view's late ack is. Anything else must be accepted.
                if let Err(error) = acknowledge_output(&ack_state, &id, bytes) {
                    assert!(
                        error.contains("Invalid terminal output acknowledgement"),
                        "{error}"
                    );
                    ack_stale.fetch_add(1, Ordering::SeqCst);
                }
            } else {
                ack_owed.lock().unwrap().push((id, bytes));
            }
        });
        let losses = Arc::new(Mutex::new(Vec::new()));
        let seen = losses.clone();
        app.listen("terminal-loss", move |event| {
            seen.lock()
                .unwrap()
                .push(serde_json::from_str(event.payload()).unwrap());
        });
        let (send, exits) = mpsc::channel();
        app.listen("terminal-exit", move |event| {
            let _ = send.send(serde_json::from_str(event.payload()).unwrap());
        });
        View {
            app,
            state,
            acking,
            owed,
            received,
            stale,
            losses,
            exits,
        }
    }

    /// The window comes back: repay everything held, then keep up.
    fn catch_up(&self) {
        self.acking.store(true, Ordering::SeqCst);
        for (id, bytes) in self.owed.lock().unwrap().drain(..) {
            // A session detached meanwhile forgave its credit; refusing the
            // stale repayment is correct, not a failure.
            let _ = acknowledge_output(&self.state, &id, bytes);
        }
    }

    /// A shell that floods output forever and records its pid.
    fn flood(&self, dir: &std::path::Path, name: &str) -> (String, i32) {
        let pidfile = dir.join(format!("{name}.pid"));
        let script = format!(
            "echo $$ > '{}'; while :; do printf '%04096d\\n' 0; done",
            pidfile.display()
        );
        let session = spawn_session(
            self.app.handle(),
            &self.state,
            dir.to_str().unwrap(),
            24,
            80,
            Some("/bin/sh".into()),
            Some(vec!["-c".into(), script]),
            None,
            gitpulse_lib::terminal::OutputSink::events(self.app.handle()),
        )
        .unwrap();
        (session.id, read_pid(&pidfile, Duration::from_secs(5)))
    }
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only probes for existence and permission.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn read_pid(path: &std::path::Path, within: Duration) -> i32 {
    let deadline = Instant::now() + within;
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(pid) = text.trim().parse::<i32>() {
                return pid;
            }
        }
        assert!(Instant::now() < deadline, "the shell never wrote its pid");
        std::thread::sleep(Duration::from_millis(20));
    }
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

/// The counter's value once it has not moved for `hold`, or `None` if it was
/// still moving when `within` ran out.
fn settled_count(counter: &AtomicUsize, hold: Duration, within: Duration) -> Option<usize> {
    let deadline = Instant::now() + within;
    let mut last = counter.load(Ordering::SeqCst);
    let mut since = Instant::now();
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        let now = counter.load(Ordering::SeqCst);
        if now != last {
            last = now;
            since = Instant::now();
        } else if since.elapsed() >= hold {
            return Some(last);
        }
    }
    None
}

/// The reported defect. Before the fix the stall killed the shell with
/// "renderer did not acknowledge output" and this exit arrived.
#[test]
fn a_view_that_stops_acknowledging_does_not_end_the_session() {
    let view = View::new(Duration::from_millis(200));
    let _cleanup = TerminalCleanup(view.state.clone());
    let dir = repo();
    let (id, pid) = view.flood(dir.path(), "flood");

    view.acking.store(false, Ordering::SeqCst);
    // Many stall timeouts' worth of an unpainted, flooding session.
    std::thread::sleep(Duration::from_secs(3));
    if let Ok(exit) = view.exits.try_recv() {
        panic!("a slow view ended the session: {exit}");
    }
    assert!(
        alive(pid),
        "the process must survive a view that fell behind"
    );
    let losses = view.losses.lock().unwrap().clone();
    assert_eq!(
        losses.len(),
        1,
        "one notice per stall, not one per chunk: {losses:?}"
    );
    assert_eq!(losses[0]["id"], id.as_str());

    // The window comes back; delivery resumes without anyone restarting it.
    view.catch_up();
    let before = view.received.load(Ordering::SeqCst);
    assert!(
        wait_until(Duration::from_secs(5), || view
            .received
            .load(Ordering::SeqCst)
            > before + 512 * 1024),
        "output resumes once the view repays its window"
    );
    assert!(alive(pid));
    assert_eq!(
        view.stale.load(Ordering::SeqCst),
        0,
        "with no detach, every acknowledgement is exact"
    );
    kill_session(&view.state, &id).unwrap();
    let exit = view.exits.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(exit["id"], id.as_str());
    assert!(wait_until(Duration::from_secs(2), || !alive(pid)));
}

/// A view that goes away without Close leaves the process running and
/// listed as detached; a new view takes it over and is delivered to again.
#[test]
fn detaching_leaves_the_process_running_and_attach_takes_it_back() {
    let view = View::new(Duration::from_millis(200));
    let _cleanup = TerminalCleanup(view.state.clone());
    let dir = repo();
    let (id, pid) = view.flood(dir.path(), "detach");

    assert!(detach_session(&view.state, &id).unwrap());
    let listed = list_sessions(&view.state).unwrap();
    assert!(
        listed.iter().any(|s| s.id == id && s.detached),
        "{listed:?}"
    );
    // Nothing is delivered and nothing waits: the flood keeps running.
    // Delivery that was already in flight lands, then stops: wait for the
    // count to hold still across consecutive polls rather than for a fixed
    // time. A session still delivered after detach either never settles
    // (and fails here) or moves inside the window below; it cannot pass by
    // luck of timing.
    let quiet = settled_count(
        &view.received,
        Duration::from_millis(200),
        Duration::from_secs(5),
    )
    .expect("a detached session kept delivering output");
    // Then hold the line over a bounded window, stopping at the first byte.
    let window = Instant::now() + Duration::from_secs(1);
    while Instant::now() < window {
        assert_eq!(
            view.received.load(Ordering::SeqCst),
            quiet,
            "a detached session is not delivered"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(alive(pid));
    assert!(view.exits.try_recv().is_err());

    attach_session(
        &view.state,
        &id,
        30,
        100,
        gitpulse_lib::terminal::OutputSink::events(view.app.handle()),
    )
    .unwrap();
    assert!(wait_until(Duration::from_secs(5), || view
        .received
        .load(Ordering::SeqCst)
        > quiet + 64 * 1024));
    // Detaching twice, or an id that has ended, is not an error.
    assert!(detach_session(&view.state, &id).unwrap());
    assert!(detach_session(&view.state, &id).unwrap());
    assert!(!detach_session(&view.state, "term-missing").unwrap());
    kill_session(&view.state, &id).unwrap();
    assert!(wait_until(Duration::from_secs(2), || !alive(pid)));
    assert!(!detach_session(&view.state, &id).unwrap());
}

/// Stress: many flooding sessions, a view that stalls and recovers
/// repeatedly, detaches and attaches racing the reader. Nothing ends that
/// was not closed, and closing everything frees every slot.
#[test]
fn stalls_detaches_and_attaches_under_load_never_end_a_session() {
    const SESSIONS: usize = 12;
    let view = View::new(Duration::from_millis(50));
    let _cleanup = TerminalCleanup(view.state.clone());
    let dir = repo();
    let sessions: Vec<(String, i32)> = (0..SESSIONS)
        .map(|n| view.flood(dir.path(), &format!("s{n}")))
        .collect();

    for round in 0..6 {
        view.acking.store(false, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(250));
        for (n, (id, _)) in sessions.iter().enumerate() {
            if (n + round) % 3 == 0 {
                assert!(detach_session(&view.state, id).unwrap());
            }
        }
        view.catch_up();
        for (n, (id, _)) in sessions.iter().enumerate() {
            if (n + round) % 3 == 0 {
                attach_session(
                    &view.state,
                    id,
                    24,
                    80 + round as u16,
                    gitpulse_lib::terminal::OutputSink::events(view.app.handle()),
                )
                .unwrap();
            }
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    if let Ok(exit) = view.exits.try_recv() {
        panic!("a session ended that nobody closed: {exit}");
    }
    for (_, pid) in &sessions {
        assert!(alive(*pid));
    }
    gitpulse_lib::terminal::shutdown_sessions(&view.state).unwrap();
    for (_, pid) in &sessions {
        assert!(wait_until(Duration::from_secs(2), || !alive(*pid)));
    }
    assert!(list_sessions(&view.state).unwrap().is_empty());
}
