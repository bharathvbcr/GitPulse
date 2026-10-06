//! A session lives exactly as long as its own process — not as long as the
//! last descendant that still holds the terminal open — and it starts where
//! it was asked to, or not at all.
//!
//! Real shells and real PTYs through the production functions; MockRuntime
//! supplies only the event sink.
#![cfg(unix)]
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use gitpulse_lib::terminal::{
    acknowledge_output, kill_session, session_context, spawn_session, spawn_session_in,
    write_to_session, TerminalSessions,
};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::Listener;

struct TerminalCleanup(TerminalSessions);
impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        if let Err(error) = gitpulse_lib::terminal::shutdown_sessions(&self.0) {
            eprintln!("PTY test cleanup incomplete: {error}");
        }
    }
}

fn repo() -> tempfile::TempDir {
    // These tests are about session lifetime, not the production spawn rate.
    // Every test starts here, and the race below opens 128 sessions whose
    // repository validation would otherwise queue behind the rate in one gate.
    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
    let dir = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(dir.path())
        .status()
        .unwrap()
        .success());
    common::trust_repo(dir.path());
    dir
}

/// Acknowledges every chunk, collects all output, and forwards exits.
struct Harness {
    app: tauri::App<tauri::test::MockRuntime>,
    state: TerminalSessions,
    output: Arc<Mutex<Vec<u8>>>,
    exits: mpsc::Receiver<serde_json::Value>,
}

fn harness() -> Harness {
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
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
    Harness {
        app,
        state,
        output,
        exits,
    }
}

fn wait_for_output(output: &Arc<Mutex<Vec<u8>>>, needle: &str, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if String::from_utf8_lossy(&output.lock().unwrap()).contains(needle) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
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
        assert!(Instant::now() < deadline, "the job never wrote its pid");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `sleep &` inherits the terminal as its stdout. On Linux the master then
/// never reaches EOF while it runs, so a session that waited for EOF instead of
/// for its own process stayed "running" with a dead shell, held its slot, and
/// refused Close. macOS revokes the controlling terminal when the session
/// leader exits, which delivers EOF anyway — so on a macOS host this test
/// passes with or without the reader's exit check, and only a Linux run can
/// fail it. Verified 2026-10-01 by disabling the check on macOS: still green.
#[test]
fn a_shell_that_exits_is_over_even_while_a_background_job_holds_the_terminal() {
    let h = harness();
    let _cleanup = TerminalCleanup(h.state.clone());
    let dir = repo();
    let pidfile = dir.path().join("job.pid");
    let script = format!(
        "sleep 20 & echo $! > '{}'; printf 'shell-done'; exit 3",
        pidfile.display()
    );
    let started = Instant::now();
    let session = spawn_session(
        h.app.handle(),
        &h.state,
        dir.path().to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec!["-c".into(), script]),
        None,
    )
    .unwrap();
    let job = read_pid(&pidfile, Duration::from_secs(5));
    let exit = h
        .exits
        .recv_timeout(Duration::from_secs(5))
        .expect("the shell exited, so the session must end without waiting for its job");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(exit["id"], session.id.as_str());
    assert_eq!(exit["exit_code"], 3);
    assert!(
        String::from_utf8_lossy(&h.output.lock().unwrap()).contains("shell-done"),
        "output printed before the exit is still delivered"
    );
    // The slot is free again: Close on an ended session is a no-op, not a retry.
    kill_session(&h.state, &session.id).unwrap();
    // SAFETY: the job is this test's own grandchild.
    unsafe { libc::kill(job, libc::SIGKILL) };
}

/// Closing an idle interactive shell must hang it up, not SIGKILL it: only a
/// shell that receives SIGHUP passes it on to its jobs. Killing it outright
/// orphans every job it started, and the orphan then holds the terminal.
#[test]
fn closing_an_idle_shell_hangs_up_its_jobs_and_finishes_promptly() {
    let h = harness();
    let _cleanup = TerminalCleanup(h.state.clone());
    let dir = repo();
    let pidfile = dir.path().join("job.pid");
    let session = spawn_session(
        h.app.handle(),
        &h.state,
        dir.path().to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec!["-i".into()]),
        None,
    )
    .unwrap();
    write_to_session(
        &h.state,
        &session.id,
        &format!("sleep 30 & echo $! > '{}'; echo armed\n", pidfile.display()),
    )
    .unwrap();
    assert!(wait_for_output(&h.output, "armed", Duration::from_secs(10)));
    let job = read_pid(&pidfile, Duration::from_secs(5));
    assert!(alive(job));
    let started = Instant::now();
    kill_session(&h.state, &session.id).expect("Close must finish, not ask for a retry");
    assert!(started.elapsed() < Duration::from_secs(3));
    let deadline = Instant::now() + Duration::from_secs(3);
    while alive(job) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let survived = alive(job);
    // SAFETY: the job is this test's own grandchild; reap nothing, just stop it.
    unsafe { libc::kill(job, libc::SIGKILL) };
    assert!(
        !survived,
        "the shell was killed before it could hang up its job"
    );
}

/// A program running in the foreground is still stopped by Close, including
/// one that ignores the hangup.
#[test]
fn closing_stops_a_foreground_program_that_ignores_hangup() {
    let h = harness();
    let _cleanup = TerminalCleanup(h.state.clone());
    let dir = repo();
    let session = spawn_session(
        h.app.handle(),
        &h.state,
        dir.path().to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec![
            "-c".into(),
            "trap '' HUP; printf ready; while :; do sleep 1; done".into(),
        ]),
        None,
    )
    .unwrap();
    assert!(wait_for_output(&h.output, "ready", Duration::from_secs(10)));
    let started = Instant::now();
    kill_session(&h.state, &session.id).unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
    h.exits
        .recv_timeout(Duration::from_secs(3))
        .expect("a closed session reports its exit");
}

/// Quitting closes every session at once. A shell that ignores the hangup
/// costs its full kill grace; one after another, a dock of them made quitting
/// wait for the sum.
#[test]
fn quitting_closes_every_session_concurrently() {
    const SESSIONS: usize = 12;
    let h = harness();
    let _cleanup = TerminalCleanup(h.state.clone());
    let dir = repo();
    for index in 0..SESSIONS {
        spawn_session(
            h.app.handle(),
            &h.state,
            dir.path().to_str().unwrap(),
            24,
            80,
            Some("/bin/sh".into()),
            Some(vec![
                "-c".into(),
                format!("trap '' HUP; printf 'ready-{index};'; while :; do sleep 1; done"),
            ]),
            None,
        )
        .unwrap();
    }
    for index in 0..SESSIONS {
        assert!(wait_for_output(
            &h.output,
            &format!("ready-{index};"),
            Duration::from_secs(10)
        ));
    }
    let started = Instant::now();
    gitpulse_lib::terminal::shutdown_sessions(&h.state).unwrap();
    let took = started.elapsed();
    // Each close waits out a 200 ms hangup grace: 2.4 s in sequence.
    assert!(took < Duration::from_millis(1500), "shutdown took {took:?}");
    for _ in 0..SESSIONS {
        h.exits
            .recv_timeout(Duration::from_secs(2))
            .expect("every session reports its exit");
    }
}

#[test]
fn a_session_starts_in_the_requested_directory_inside_the_repository() {
    let h = harness();
    let _cleanup = TerminalCleanup(h.state.clone());
    let dir = repo();
    std::fs::create_dir_all(dir.path().join("crates/core")).unwrap();
    let spawned = spawn_session_in(
        h.app.handle(),
        &h.state,
        dir.path().to_str().unwrap(),
        Some("crates/core"),
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec!["-c".into(), "pwd -P; exit 0".into()]),
        None,
    )
    .unwrap();
    h.exits.recv_timeout(Duration::from_secs(5)).unwrap();
    let expected = dir.path().join("crates/core").canonicalize().unwrap();
    assert_eq!(std::path::Path::new(&spawned.cwd), expected);
    let printed = String::from_utf8_lossy(&h.output.lock().unwrap()).to_string();
    assert!(
        printed.contains(expected.to_str().unwrap()),
        "shell printed {printed:?}"
    );
}

#[test]
fn a_start_directory_outside_the_repository_is_refused_not_replaced() {
    let h = harness();
    let _cleanup = TerminalCleanup(h.state.clone());
    let dir = repo();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("file.txt"), "x").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
    let root = dir.path().to_str().unwrap();
    for refused in [
        "../",
        "..",
        "/etc",
        "escape",
        "escape/",
        "file.txt",
        "missing/dir",
        "a\nb",
        "a\0b",
    ] {
        let result = spawn_session_in(
            h.app.handle(),
            &h.state,
            root,
            Some(refused),
            24,
            80,
            Some("/bin/sh".into()),
            Some(vec!["-c".into(), "exit 0".into()]),
            None,
        );
        assert!(
            result.is_err(),
            "{refused:?} must be refused, got {result:?}"
        );
    }
    let huge = "a/".repeat(4096);
    assert!(spawn_session_in(
        h.app.handle(),
        &h.state,
        root,
        Some(&huge),
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec!["-c".into(), "exit 0".into()]),
        None,
    )
    .is_err());
    // A refusal consumed no capacity and started no process.
    assert!(h.exits.recv_timeout(Duration::from_millis(300)).is_err());
    for root_spelling in [None, Some(""), Some(".")] {
        let spawned = spawn_session_in(
            h.app.handle(),
            &h.state,
            root,
            root_spelling,
            24,
            80,
            Some("/bin/sh".into()),
            Some(vec!["-c".into(), "exit 0".into()]),
            None,
        )
        .unwrap();
        assert_eq!(
            std::path::Path::new(&spawned.cwd),
            dir.path().canonicalize().unwrap()
        );
        h.exits.recv_timeout(Duration::from_secs(5)).unwrap();
    }
}

#[test]
fn context_names_the_foreground_program_and_follows_cd() {
    let h = harness();
    let _cleanup = TerminalCleanup(h.state.clone());
    let dir = repo();
    std::fs::create_dir_all(dir.path().join("docs/guide")).unwrap();
    let session = spawn_session(
        h.app.handle(),
        &h.state,
        dir.path().to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec!["-i".into()]),
        None,
    )
    .unwrap();
    write_to_session(&h.state, &session.id, "cd docs/guide && echo moved\n").unwrap();
    assert!(wait_for_output(&h.output, "moved", Duration::from_secs(10)));
    let idle = session_context(&h.state, &session.id).unwrap();
    assert_eq!(idle.busy, Some(false), "{idle:?}");
    assert_eq!(idle.repo_dir.as_deref(), Some("docs/guide"), "{idle:?}");
    assert!(idle
        .cwd
        .as_deref()
        .is_some_and(|cwd| cwd.ends_with("docs/guide")));

    write_to_session(&h.state, &session.id, "sleep 30\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let busy = loop {
        let now = session_context(&h.state, &session.id).unwrap();
        if now.busy == Some(true) || Instant::now() > deadline {
            break now;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(busy.busy, Some(true), "{busy:?}");
    assert_eq!(busy.process.as_deref(), Some("sleep"), "{busy:?}");
    assert_eq!(busy.repo_dir.as_deref(), Some("docs/guide"));

    write_to_session(&h.state, &session.id, "\x03cd / && echo outside\n").unwrap();
    assert!(wait_for_output(
        &h.output,
        "outside",
        Duration::from_secs(10)
    ));
    let deadline = Instant::now() + Duration::from_secs(5);
    let outside = loop {
        let now = session_context(&h.state, &session.id).unwrap();
        if now.cwd.as_deref() == Some("/") || Instant::now() > deadline {
            break now;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(outside.cwd.as_deref(), Some("/"));
    assert_eq!(
        outside.repo_dir, None,
        "outside the repository is not a place to start"
    );
    assert!(session_context(&h.state, "term-missing").is_err());
}

/// Spawn-in-a-directory, context reads and Close, raced against each other
/// from many threads. Every read is an answer or "not found" — never a panic,
/// a hang, or a directory the session was not in — and every slot comes back.
#[test]
fn context_reads_race_spawns_and_closes_without_leaking_a_slot() {
    const THREADS: usize = 8;
    const ROUNDS: usize = 12;
    let h = harness();
    let _cleanup = TerminalCleanup(h.state.clone());
    let dir = repo();
    let dirs = ["", "a", "a/b", "c"];
    for sub in &dirs[1..] {
        std::fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    let root = dir.path().to_str().unwrap().to_owned();
    let handle = h.app.handle().clone();
    std::thread::scope(|scope| {
        for worker in 0..THREADS {
            let state = h.state.clone();
            let handle = handle.clone();
            let root = root.clone();
            scope.spawn(move || {
                for round in 0..ROUNDS {
                    let sub = dirs[(worker + round) % dirs.len()];
                    let session = spawn_session_in(
                        &handle,
                        &state,
                        &root,
                        Some(sub),
                        24,
                        80,
                        Some("/bin/sh".into()),
                        Some(vec!["-i".into()]),
                        None,
                    )
                    .expect("capacity is never exhausted: THREADS is below the session cap");
                    let reader_state = state.clone();
                    let id = session.id.clone();
                    let reads = std::thread::spawn(move || {
                        let mut answered = 0usize;
                        loop {
                            match session_context(&reader_state, &id) {
                                Ok(context) => {
                                    answered += 1;
                                    if let Some(dir) = context.repo_dir.as_deref() {
                                        assert!(
                                            dir == sub || dir.is_empty(),
                                            "a session started in {sub:?} reported {dir:?}"
                                        );
                                    }
                                }
                                Err(error) => {
                                    assert!(error.contains("not found"), "{error}");
                                    return answered;
                                }
                            }
                        }
                    });
                    std::thread::sleep(Duration::from_millis(5 * (round as u64 % 4)));
                    kill_session(&state, &session.id).expect("close finishes under contention");
                    let answered = reads.join().expect("the reader never panics");
                    assert!(answered < 1_000_000);
                }
            });
        }
    });
    gitpulse_lib::terminal::shutdown_sessions(&h.state).unwrap();
    // Every slot came back: the cap can be filled again from empty.
    let mut held = Vec::new();
    for _ in 0..h.state.session_limit() {
        held.push(
            spawn_session(
                h.app.handle(),
                &h.state,
                &root,
                24,
                80,
                Some("/bin/sh".into()),
                Some(vec!["-c".into(), "sleep 30".into()]),
                None,
            )
            .expect("a slot leaked during the race"),
        );
    }
    gitpulse_lib::terminal::shutdown_sessions(&h.state).unwrap();
}

mod common;
