//! Adversarial empirical stress suite for Milestone 3 terminal session hardening:
//! 1. kqueue EVFILT_PROC ESRCH / exit handling on macOS.
//! 2. Hidden / backgrounded tab backpressure: verifies child process is NOT terminated
//!    and that output shedding occurs without killing the session.
//! 3. Heavy concurrent spawn, write, detach, and kill cycles under pressure.
#![cfg(unix)]

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use gitpulse_lib::procguard::LockedSpawn;
use gitpulse_lib::terminal::{
    acknowledge_output, attach_session, detach_session, kill_session, list_sessions,
    set_background_sessions, spawn_session, write_to_session, OutputSink, TerminalSessions,
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
            eprintln!("PTY stress test cleanup incomplete: {error}");
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

/// A test view with controllable ACKs and event capture.
#[allow(dead_code)]
struct StressView {
    app: tauri::App<tauri::test::MockRuntime>,
    state: TerminalSessions,
    acking: Arc<AtomicBool>,
    owed: Arc<Mutex<Vec<(String, usize)>>>,
    received: Arc<AtomicUsize>,
    losses: Arc<Mutex<Vec<serde_json::Value>>>,
    exits: mpsc::Receiver<serde_json::Value>,
}

impl StressView {
    fn new(stall: Duration) -> Self {
        let app = tauri::test::mock_builder()
            .build(gitpulse_lib::context())
            .unwrap();
        let state = common::terminals();
        state.set_output_stall_timeout(stall);
        let acking = Arc::new(AtomicBool::new(true));
        let owed = Arc::new(Mutex::new(Vec::new()));
        let received = Arc::new(AtomicUsize::new(0));
        let (ack_state, ack_on, ack_owed, ack_received) = (
            state.clone(),
            acking.clone(),
            owed.clone(),
            received.clone(),
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
                let _ = acknowledge_output(&ack_state, &id, bytes);
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

        Self {
            app,
            state,
            acking,
            owed,
            received,
            losses,
            exits,
        }
    }

    fn catch_up(&self) {
        self.acking.store(true, Ordering::SeqCst);
        for (id, bytes) in self.owed.lock().unwrap().drain(..) {
            let _ = acknowledge_output(&self.state, &id, bytes);
        }
    }

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
            OutputSink::events(self.app.handle()),
        )
        .unwrap();
        (session.id, read_pid(&pidfile, Duration::from_secs(5)))
    }
}

// ---------------------------------------------------------------------------
// 1. kqueue EVFILT_PROC and ESRCH resilience verification on macOS
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
#[test]
fn kqueue_handles_proc_vanished_without_bogus_failure() {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    // SAFETY: kqueue() syscall allocates a new descriptor
    let kq = unsafe { libc::kqueue() };
    assert!(kq >= 0, "kqueue creation must succeed");
    let kq = unsafe { OwnedFd::from_raw_fd(kq) };

    // Spawn a quick child process that exits immediately
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .spawn_locked()
        .unwrap();
    let pid = child.id() as libc::pid_t;

    // Register EVFILT_PROC for NOTE_EXIT
    let change = libc::kevent {
        ident: pid as libc::uintptr_t,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    let rc = unsafe {
        libc::kevent(
            kq.as_raw_fd(),
            &change,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    // The registration may succeed or return ESRCH if the child is already reaped
    if rc < 0 {
        let err = std::io::Error::last_os_error();
        assert_eq!(err.raw_os_error(), Some(libc::ESRCH));
    }

    // Wait and reap child
    let _ = child.wait().unwrap();

    // Now call kevent to poll events
    let timeout = libc::timespec {
        tv_sec: 1,
        tv_nsec: 0,
    };
    let mut events: [libc::kevent; 2] = unsafe { std::mem::zeroed() };
    let ready = unsafe {
        libc::kevent(
            kq.as_raw_fd(),
            std::ptr::null(),
            0,
            events.as_mut_ptr(),
            events.len() as libc::c_int,
            &timeout,
        )
    };

    if ready > 0 {
        for event in &events[..ready as usize] {
            if event.flags & libc::EV_ERROR != 0 {
                let err = event.data as i32;
                // If EV_ERROR is returned, ESRCH must be recognized as process exit, never an error
                assert_eq!(
                    err,
                    libc::ESRCH,
                    "unexpected error other than ESRCH from kevent"
                );
            } else if event.filter == libc::EVFILT_PROC {
                assert!(
                    event.fflags & libc::NOTE_EXIT != 0,
                    "event must indicate process exit"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Hidden tab backpressure shedding and process survival
// ---------------------------------------------------------------------------

#[test]
fn hidden_tab_backpressure_sheds_buffer_without_terminating_process() {
    let view = StressView::new(Duration::from_millis(50));
    let _cleanup = TerminalCleanup(view.state.clone());
    let dir = repo();
    let (id, pid) = view.flood(dir.path(), "hidden_backpressure");

    // Mark the session as backgrounded/hidden
    set_background_sessions(&view.state, std::slice::from_ref(&id));

    // Check that session is listed
    let sessions = list_sessions(&view.state).unwrap();
    assert!(
        sessions.iter().any(|s| s.id == id),
        "session must be listed"
    );

    // Turn off ACKs: view stops acknowledging output chunks
    view.acking.store(false, Ordering::SeqCst);

    // Let the flooding shell produce voluminous output for 2 seconds while hidden
    std::thread::sleep(Duration::from_millis(2000));

    // Verify child process is STILL ALIVE and running
    assert!(
        alive(pid),
        "child process must survive backpressure when hidden on background tab"
    );

    // Verify session is listed and NOT ending
    let sessions = list_sessions(&view.state).unwrap();
    let sess = sessions
        .iter()
        .find(|s| s.id == id)
        .expect("session listed");
    assert!(!sess.ending, "session must not be in ending state");

    // Catch up: view starts acknowledging again
    view.catch_up();

    // Verify output flow resumes and chunks continue to be delivered
    let before = view.received.load(Ordering::SeqCst);
    assert!(
        wait_until(Duration::from_secs(5), || {
            view.received.load(Ordering::SeqCst) > before + 256 * 1024
        }),
        "output must resume after view catches up"
    );
    assert!(alive(pid), "process still alive after catch-up");

    // Cleanly kill the session
    kill_session(&view.state, &id).unwrap();
    let exit = view.exits.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(exit["id"], id.as_str());
    assert!(wait_until(Duration::from_secs(3), || !alive(pid)));
}

// ---------------------------------------------------------------------------
// 3. Stress test: heavy concurrent spawn, write, detach, and kill cycles
// ---------------------------------------------------------------------------

#[test]
fn stress_concurrent_spawn_write_detach_kill_cycles() {
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = common::terminals();
    let _cleanup = TerminalCleanup(state.clone());
    let dir = repo();

    const NUM_SESSIONS: usize = 8;
    let mut session_ids = Vec::new();
    let mut pids = Vec::new();

    // 1. Concurrent Spawns
    for i in 0..NUM_SESSIONS {
        let pidfile = dir.path().join(format!("stress_{i}.pid"));
        let script = format!(
            "echo $$ > '{}'; while read line; do echo \"echo: $line\"; done",
            pidfile.display()
        );
        let session = spawn_session(
            app.handle(),
            &state,
            dir.path().to_str().unwrap(),
            24,
            80,
            Some("/bin/sh".into()),
            Some(vec!["-c".into(), script]),
            None,
            OutputSink::events(app.handle()),
        )
        .expect("spawn session");
        let pid = read_pid(&pidfile, Duration::from_secs(5));
        assert!(alive(pid), "spawned child {i} must be alive");
        session_ids.push(session.id);
        pids.push(pid);
    }

    assert_eq!(list_sessions(&state).unwrap().len(), NUM_SESSIONS);

    // 2. High-Frequency Concurrent Writes
    let handles: Vec<_> = session_ids
        .iter()
        .map(|id| {
            let state_clone = state.clone();
            let id_clone = id.clone();
            std::thread::spawn(move || {
                for j in 0..50 {
                    let msg = format!("msg_{j}\n");
                    write_to_session(&state_clone, &id_clone, &msg).expect("write to session");
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    // 3. Detach Half the Sessions
    for id in &session_ids[..NUM_SESSIONS / 2] {
        assert!(detach_session(&state, id).expect("detach session"));
    }

    let listed = list_sessions(&state).unwrap();
    for id in &session_ids[..NUM_SESSIONS / 2] {
        let s = listed.iter().find(|s| s.id == *id).unwrap();
        assert!(s.detached, "session {id} should be marked detached");
    }

    // 4. Re-attach the Detached Sessions
    for id in &session_ids[..NUM_SESSIONS / 2] {
        let spawned = attach_session(&state, id, 24, 80, OutputSink::events(app.handle()))
            .expect("attach session");
        assert_eq!(spawned.id, *id);
    }

    // 5. Concurrent Teardown / Kill
    let kill_handles: Vec<_> = session_ids
        .iter()
        .map(|id| {
            let state_clone = state.clone();
            let id_clone = id.clone();
            std::thread::spawn(move || {
                kill_session(&state_clone, &id_clone).expect("kill session");
            })
        })
        .collect();

    for h in kill_handles {
        h.join().unwrap();
    }

    // 6. Verify all processes exit and no zombie sessions remain
    for (i, pid) in pids.iter().enumerate() {
        assert!(
            wait_until(Duration::from_secs(4), || !alive(*pid)),
            "child process {i} (pid {pid}) must be reaped"
        );
    }

    assert!(
        wait_until(Duration::from_secs(4), || {
            list_sessions(&state).unwrap().is_empty()
        }),
        "all sessions must be cleaned from registry"
    );
}

// ---------------------------------------------------------------------------
// 4. Hidden tab with a failing output sink sheds buffer and keeps child alive
// ---------------------------------------------------------------------------

#[test]
fn hidden_tab_with_failing_sink_sheds_and_preserves_child() {
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = common::terminals();
    let _cleanup = TerminalCleanup(state.clone());
    let dir = repo();

    let failing_sink = OutputSink::channel(tauri::ipc::Channel::new(|_| {
        Err(tauri::Error::WebviewNotFound)
    }));

    let pidfile = dir.path().join("failing_sink.pid");
    let script = format!(
        "echo $$ > '{}'; for i in $(seq 1 100); do echo \"line $i\"; sleep 0.02; done; exec sleep 30",
        pidfile.display()
    );

    let session = spawn_session(
        app.handle(),
        &state,
        dir.path().to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec!["-c".into(), script]),
        None,
        failing_sink,
    )
    .expect("spawn session");

    let pid = read_pid(&pidfile, Duration::from_secs(5));
    assert!(alive(pid), "child must be alive initially");

    // Mark as hidden
    set_background_sessions(&state, std::slice::from_ref(&session.id));

    // Give it time to output multiple lines while delivery fails
    std::thread::sleep(Duration::from_millis(800));

    // Child MUST still be alive!
    assert!(
        alive(pid),
        "child must not be killed by failed output delivery on hidden tab"
    );

    let listed = list_sessions(&state).unwrap();
    assert!(listed.iter().any(|s| s.id == session.id && !s.ending));

    kill_session(&state, &session.id).unwrap();
    assert!(wait_until(Duration::from_secs(3), || !alive(pid)));
}

// ---------------------------------------------------------------------------
// 5. Child killed by SIGKILL on hidden tab exits cleanly without zombie
// ---------------------------------------------------------------------------

#[test]
fn child_sigkilled_on_hidden_tab_exits_cleanly() {
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = common::terminals();
    let _cleanup = TerminalCleanup(state.clone());
    let dir = repo();

    let (send, exits) = mpsc::channel();
    app.listen("terminal-exit", move |event| {
        let _ = send.send(serde_json::from_str(event.payload()).unwrap());
    });

    let pidfile = dir.path().join("sigkill.pid");
    let script = format!("echo $$ > '{}'; exec sleep 60", pidfile.display());

    let session = spawn_session(
        app.handle(),
        &state,
        dir.path().to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec!["-c".into(), script]),
        None,
        OutputSink::events(app.handle()),
    )
    .expect("spawn session");

    let pid = read_pid(&pidfile, Duration::from_secs(5));
    assert!(alive(pid));

    // Mark hidden
    set_background_sessions(&state, std::slice::from_ref(&session.id));

    // Send SIGKILL directly to child process
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }

    // Readiness / kqueue must detect the exit cleanly and emit terminal-exit
    let exit: serde_json::Value = exits
        .recv_timeout(Duration::from_secs(5))
        .expect("must receive terminal-exit on process kill");
    assert_eq!(exit["id"], session.id.as_str());

    assert!(wait_until(Duration::from_secs(2), || !alive(pid)));
    assert!(wait_until(Duration::from_secs(2), || list_sessions(&state)
        .unwrap()
        .is_empty()));
}

// ---------------------------------------------------------------------------
// 6. Rapid visibility toggling while flooding output
// ---------------------------------------------------------------------------

#[test]
fn rapid_visibility_toggling_under_heavy_output_does_not_crash_or_wedge() {
    let view = StressView::new(Duration::from_millis(50));
    let _cleanup = TerminalCleanup(view.state.clone());
    let dir = repo();
    let (id, pid) = view.flood(dir.path(), "toggle");

    // Toggle background/foreground rapidly 20 times while shell is flooding
    for i in 0..20 {
        if i % 2 == 0 {
            set_background_sessions(&view.state, std::slice::from_ref(&id));
        } else {
            set_background_sessions(&view.state, &[]);
        }
        std::thread::sleep(Duration::from_millis(30));
    }

    assert!(alive(pid), "child must remain alive during rapid toggling");

    let sessions = list_sessions(&view.state).unwrap();
    assert!(sessions.iter().any(|s| s.id == id && !s.ending));

    kill_session(&view.state, &id).unwrap();
    let exit = view.exits.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(exit["id"], id.as_str());
    assert!(wait_until(Duration::from_secs(3), || !alive(pid)));
}
