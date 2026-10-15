//! Four real PTY sessions killed from outside within 100 ms are one incident,
//! and the app side is cleaned up afterwards.
//!
//! Real shells and real PTYs through the production spawn path; MockRuntime
//! supplies only the event sink. Only the explicit pids this test started are
//! signalled.
#![cfg(unix)]
use gitpulse_lib::procguard::LockedSpawn;
use gitpulse_lib::terminal::{kill_session, spawn_session, TerminalSessions};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::Listener;

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

/// Open descriptors a session could leak: PTYs, pipes and sockets. Regular
/// files are left out: the ledger's SQLite connection (database, -wal, -shm)
/// is opened once per process on the spawn-record thread, which can land
/// after the baseline under load and read as three leaked descriptors.
fn open_fds() -> usize {
    std::fs::read_dir("/dev/fd").map_or(0, |entries| {
        entries
            .flatten()
            .filter(|entry| !std::fs::metadata(entry.path()).is_ok_and(|meta| meta.is_file()))
            .count()
    })
}

fn read_pid(path: &std::path::Path) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(pid) = std::fs::read_to_string(path)
            .unwrap_or_default()
            .trim()
            .parse::<i32>()
        {
            return pid;
        }
        assert!(Instant::now() < deadline, "the session never wrote its pid");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn four_sessions_sigtermed_from_outside_are_one_incident_and_leave_nothing_behind() {
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let incidents = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let seen = incidents.clone();
    app.listen("terminal-incident", move |event| {
        seen.lock()
            .unwrap()
            .push(serde_json::from_str(event.payload()).unwrap());
    });
    let (exit_tx, exits) = mpsc::channel();
    app.listen("terminal-exit", move |event| {
        let _ = exit_tx.send(serde_json::from_str::<serde_json::Value>(event.payload()).unwrap());
    });

    let dir = repo();
    // One full open-and-close first, so descriptors the process opens once
    // and keeps (event plumbing, the spawn gate) are in the baseline.
    let warm = spawn_session(
        app.handle(),
        &state,
        dir.path().to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec!["-c".into(), "exec sleep 600".into()]),
        None,
        gitpulse_lib::terminal::OutputSink::events(app.handle()),
    )
    .unwrap();
    kill_session(&state, &warm.id).unwrap();
    let closed = exits
        .recv_timeout(Duration::from_secs(10))
        .expect("the warm-up session reports its end");
    // GitPulse's own Close says so, and a view offers no Resume for it.
    assert_eq!(closed["ended_by_gitpulse"], true, "{closed}");
    let fds_before = open_fds();
    let mut ids = Vec::new();
    let mut pids = Vec::new();
    for i in 0..4 {
        let pidfile = dir.path().join(format!("job{i}.pid"));
        let script = format!("echo $$ > '{}'; exec sleep 600", pidfile.display());
        let spawned = spawn_session(
            app.handle(),
            &state,
            dir.path().to_str().unwrap(),
            24,
            80,
            Some("/bin/sh".into()),
            Some(vec!["-c".into(), script]),
            None,
            gitpulse_lib::terminal::OutputSink::events(app.handle()),
        )
        .unwrap();
        ids.push(spawned.id);
        pids.push(read_pid(&pidfile));
    }
    for pid in &pids {
        // SAFETY: an explicit pid this test spawned, signalled once.
        assert_eq!(unsafe { libc::kill(*pid, libc::SIGTERM) }, 0);
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut exited = Vec::new();
    for _ in 0..4 {
        let exit = exits
            .recv_timeout(Duration::from_secs(10))
            .expect("every session reports its end");
        assert!(!exit["signal"].as_str().unwrap_or("").is_empty(), "{exit}");
        // A signal from outside is not GitPulse's end: Resume is offered.
        assert_eq!(exit["ended_by_gitpulse"], false, "{exit}");
        exited.push(exit["id"].as_str().unwrap().to_string());
    }

    let deadline = Instant::now() + Duration::from_secs(15);
    while incidents.lock().unwrap().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    // Let a wrongly split second incident show itself before judging.
    std::thread::sleep(Duration::from_millis(2500));
    let got = incidents.lock().unwrap().clone();
    assert_eq!(got.len(), 1, "exactly one incident: {got:?}");
    let mut members: Vec<String> = got[0]["session_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    members.sort();
    exited.sort();
    ids.sort();
    assert_eq!(members, ids, "all four, by terminal session id");
    assert_eq!(exited, ids);
    assert!(
        got[0]["signal"].as_str().unwrap().contains("15"),
        "{}",
        got[0]
    );
    assert!(got[0]["at_ms"].as_u64().unwrap() > 0);
    assert!(got[0]["sender_note"].as_str().is_some());
    assert!(got[0]["sender"].is_null() || got[0]["sender"]["pid"].is_u64());
    assert_eq!(
        state.incidents().len(),
        1,
        "readable through the command's source"
    );

    // App-side cleanup: sessions gone, slots free, nothing left to close, no
    // descriptor left over.
    for id in &ids {
        kill_session(&state, id).expect("closing a finished session is a no-op");
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while open_fds() > fds_before + 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let after = open_fds();
    assert!(
        after <= fds_before + 2,
        "descriptors leaked: {fds_before} before, {after} after"
    );
    gitpulse_lib::terminal::shutdown_sessions(&state).expect("no session left to stop");
}

/// Closing sessions through GitPulse is not an external kill: a whole dock
/// closed at quit must not be reported as an attack.
#[test]
fn sessions_closed_through_gitpulse_are_never_an_incident() {
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let dir = repo();
    let mut ids = Vec::new();
    for _ in 0..4 {
        let spawned = spawn_session(
            app.handle(),
            &state,
            dir.path().to_str().unwrap(),
            24,
            80,
            Some("/bin/sh".into()),
            Some(vec!["-c".into(), "exec sleep 600".into()]),
            None,
            gitpulse_lib::terminal::OutputSink::events(app.handle()),
        )
        .unwrap();
        ids.push(spawned.id);
    }
    for id in &ids {
        kill_session(&state, id).unwrap();
    }
    std::thread::sleep(Duration::from_millis(2600));
    assert!(state.incidents().is_empty(), "{:?}", state.incidents());
}

mod common;
