//! Real OS PTYs through the production spawn/write/resize/kill functions.
//! MockRuntime supplies only the event sink; shells and PTYs are real.
#![cfg(unix)]
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use gitpulse_lib::terminal::{
    acknowledge_output, kill_session, resize_session, spawn_session, write_to_session,
    TerminalSessions,
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
    let dir = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(dir.path())
        .status()
        .unwrap()
        .success());
    dir
}

#[test]
fn immediate_output_and_exit_status_survive_spawn() {
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    let output = Arc::new(Mutex::new(Vec::new()));
    let captured = output.clone();
    let ack = state.clone();
    app.listen("terminal-output", move |event| {
        let data: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
        let bytes = STANDARD.decode(data["data_b64"].as_str().unwrap()).unwrap();
        captured.lock().unwrap().extend(&bytes);
        acknowledge_output(&ack, data["id"].as_str().unwrap(), bytes.len()).unwrap();
    });
    let (send, receive) = mpsc::channel();
    app.listen("terminal-exit", move |event| {
        send.send(event.payload().to_string()).unwrap();
    });
    let dir = repo();
    spawn_session(
        app.handle(),
        &state,
        dir.path().to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec!["-c".into(), "printf '世界 hello'; exit 37".into()]),
        None,
    )
    .unwrap();
    let exit: serde_json::Value =
        serde_json::from_str(&receive.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
    assert_eq!(
        String::from_utf8(output.lock().unwrap().clone()).unwrap(),
        "世界 hello"
    );
    assert_eq!(exit["exit_code"], 37);
    assert!(exit["error"].is_null());
}

#[test]
fn output_flood_is_backpressured_then_drains_without_loss() {
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    let (send, receive) = mpsc::channel();
    app.listen("terminal-output", move |event| {
        send.send(event.payload().to_string()).unwrap();
    });
    let (exit_send, exit_receive) = mpsc::channel();
    app.listen("terminal-exit", move |_| {
        exit_send.send(()).unwrap();
    });
    let dir = repo();
    let spawned = spawn_session(app.handle(), &state, dir.path().to_str().unwrap(), 24, 80,
        Some("/bin/sh".into()), Some(vec!["-c".into(), "i=0; while [ $i -lt 32768 ]; do printf 0123456789abcdef0123456789abcdef; i=$((i+1)); done".into()]), None).unwrap();
    std::thread::sleep(Duration::from_millis(250));
    assert!(
        exit_receive.try_recv().is_err(),
        "producer must wait for render acknowledgements"
    );
    let mut total = 0usize;
    let deadline = Instant::now() + Duration::from_secs(15);
    while total < 1024 * 1024 && Instant::now() < deadline {
        let data: serde_json::Value =
            serde_json::from_str(&receive.recv_timeout(Duration::from_secs(3)).unwrap()).unwrap();
        let bytes = STANDARD.decode(data["data_b64"].as_str().unwrap()).unwrap();
        for (offset, byte) in bytes.iter().enumerate() {
            assert_eq!(*byte, b"0123456789abcdef"[(total + offset) % 16]);
        }
        total += bytes.len();
        acknowledge_output(&state, &spawned.id, bytes.len()).unwrap();
    }
    assert_eq!(total, 1024 * 1024);
    exit_receive.recv_timeout(Duration::from_secs(5)).unwrap();
}

#[test]
fn interactive_round_trips_do_not_accumulate_polling_delays() {
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    let (send, receive) = mpsc::channel();
    app.listen("terminal-output", move |event| {
        let _ = send.send(event.payload().to_string());
    });
    let (exit_send, exit_receive) = mpsc::channel();
    app.listen("terminal-exit", move |event| {
        let _ = exit_send.send(event.payload().to_string());
    });
    let dir = repo();
    let spawned = spawn_session(
        app.handle(), &state, dir.path().to_str().unwrap(), 24, 80,
        Some("/bin/sh".into()),
        Some(vec!["-c".into(), "stty -echo; i=0; while [ $i -lt 512 ]; do printf .; read -r line || exit 91; i=$((i+1)); done".into()]),
        None,
    ).unwrap();
    let started = Instant::now();
    for round in 0..512 {
        let data: serde_json::Value =
            serde_json::from_str(&receive.recv_timeout(Duration::from_secs(3)).unwrap()).unwrap();
        let bytes = STANDARD.decode(data["data_b64"].as_str().unwrap()).unwrap();
        assert_eq!(bytes, b".");
        acknowledge_output(&state, &spawned.id, bytes.len()).unwrap();
        write_to_session(&state, &spawned.id, "\n").unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "round {round}: readiness must wake the reader without a fixed delay"
        );
    }
    let exit: serde_json::Value =
        serde_json::from_str(&exit_receive.recv_timeout(Duration::from_secs(3)).unwrap()).unwrap();
    assert_eq!(exit["exit_code"], 0);
    assert!(exit["error"].is_null());
}

#[test]
fn a_blocked_writer_does_not_block_other_sessions_or_close() {
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    let dir = repo();
    let args = Some(vec![
        "-c".into(),
        "stty -echo -icanon; printf ready; sleep 30".into(),
    ]);
    let (send, receive) = mpsc::channel();
    app.listen("terminal-output", move |_| {
        let _ = send.send(());
    });
    let a = spawn_session(
        app.handle(),
        &state,
        dir.path().to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        args.clone(),
        None,
    )
    .unwrap();
    receive.recv_timeout(Duration::from_secs(5)).unwrap();
    let writing_state = state.clone();
    let id = a.id.clone();
    let (written_send, written_receive) = mpsc::channel();
    let blocked = std::thread::spawn(move || {
        let result = write_to_session(&writing_state, &id, &"x".repeat(65536));
        let _ = written_send.send(result);
    });
    assert!(written_receive
        .recv_timeout(Duration::from_millis(50))
        .is_err());
    let start = Instant::now();
    let b = spawn_session(
        app.handle(),
        &state,
        dir.path().to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        args,
        None,
    )
    .unwrap();
    resize_session(&state, &b.id, 40, 120).unwrap();
    kill_session(&state, &a.id).unwrap();
    kill_session(&state, &b.id).unwrap();
    assert!(start.elapsed() < Duration::from_secs(3));
    assert!(written_receive
        .recv_timeout(Duration::from_secs(3))
        .expect("closing a session must release its blocked input request")
        .is_err());
    blocked.join().unwrap();
}

#[test]
fn a_nonreading_terminal_bounds_input_wait_without_requiring_close() {
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    let dir = repo();
    let (ready_send, ready_receive) = mpsc::channel();
    app.listen("terminal-output", move |_| {
        let _ = ready_send.send(());
    });
    let session = spawn_session(
        app.handle(),
        &state,
        dir.path().to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec![
            "-c".into(),
            "stty -echo -icanon; printf ready; sleep 30".into(),
        ]),
        None,
    )
    .unwrap();
    ready_receive.recv_timeout(Duration::from_secs(5)).unwrap();
    let writing = state.clone();
    let id = session.id.clone();
    let (send, receive) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let _ = send.send(write_to_session(&writing, &id, &"x".repeat(65536)));
    });
    let result = receive
        .recv_timeout(Duration::from_secs(4))
        .expect("input must have its own deadline");
    assert!(result.unwrap_err().contains("Terminal input timed out"));
    thread.join().unwrap();
    kill_session(&state, &session.id).unwrap();
}

#[test]
fn close_terminates_a_hup_ignoring_foreground_process_and_is_idempotent() {
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    let dir = repo();
    let (send, receive) = mpsc::channel();
    app.listen("terminal-output", move |_| {
        let _ = send.send(());
    });
    let (exit_send, exit_receive) = mpsc::channel();
    app.listen("terminal-exit", move |_| {
        let _ = exit_send.send(());
    });
    let session = spawn_session(
        app.handle(),
        &state,
        dir.path().to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec![
            "-c".into(),
            "trap '' HUP TERM; printf ready; while :; do sleep 1; done".into(),
        ]),
        None,
    )
    .unwrap();
    receive.recv_timeout(Duration::from_secs(5)).unwrap();
    kill_session(&state, &session.id).unwrap();
    exit_receive.recv_timeout(Duration::from_secs(5)).unwrap();
    kill_session(&state, &session.id).unwrap();
}

#[test]
fn full_capacity_can_close_and_immediately_replace_without_a_false_limit() {
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    let dir = repo();
    let launch = || {
        spawn_session(
            app.handle(),
            &state,
            dir.path().to_str().unwrap(),
            24,
            80,
            Some("/bin/sh".into()),
            Some(vec!["-c".into(), "sleep 30".into()]),
            None,
        )
    };
    let mut sessions = Vec::new();
    for _ in 0..16 {
        sessions.push(launch().unwrap());
    }
    assert!(launch().is_err());
    for index in 0..16 {
        kill_session(&state, &sessions[index].id).unwrap();
        let replacement = launch();
        if replacement.is_err() {
            for session in &sessions {
                let _ = kill_session(&state, &session.id);
            }
        }
        sessions[index] = replacement.expect("a completed close must release native capacity");
    }
    gitpulse_lib::terminal::shutdown_sessions(&state).unwrap();
}

#[test]
fn closing_after_stdio_eof_still_reaps_the_live_child() {
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    let dir = repo();
    let session = spawn_session(
        app.handle(),
        &state,
        dir.path().to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec!["-c".into(), "exec 0<&- 1>&- 2>&-; sleep 30".into()]),
        None,
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let start = Instant::now();
    kill_session(&state, &session.id).unwrap();
    assert!(start.elapsed() < Duration::from_secs(3));
    gitpulse_lib::terminal::shutdown_sessions(&state).unwrap();
}

#[test]
fn binary_mouse_input_reaches_the_pty_without_utf8_reencoding() {
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    let dir = repo();
    let (send, receive) = mpsc::channel();
    let ack = state.clone();
    app.listen("terminal-output", move |event| {
        let data: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
        let bytes = STANDARD.decode(data["data_b64"].as_str().unwrap()).unwrap();
        acknowledge_output(&ack, data["id"].as_str().unwrap(), bytes.len()).unwrap();
        let _ = send.send(bytes);
    });
    let session = spawn_session(
        app.handle(),
        &state,
        dir.path().to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec![
            "-c".into(),
            "stty raw -echo; printf ready; dd bs=1 count=5 2>/dev/null | od -An -tx1".into(),
        ]),
        None,
    )
    .unwrap();
    let mut output = Vec::new();
    while !String::from_utf8_lossy(&output).contains("ready") {
        output.extend(receive.recv_timeout(Duration::from_secs(3)).unwrap());
    }
    gitpulse_lib::terminal::write_binary_to_session(&state, &session.id, "\u{1b}[M\u{80}\u{ff}")
        .unwrap();
    while !output.contains(&b'\n') {
        match receive.recv_timeout(Duration::from_secs(3)) {
            Ok(bytes) => output.extend(bytes),
            Err(error) => {
                let _ = gitpulse_lib::terminal::shutdown_sessions(&state);
                panic!(
                    "binary PTY output: {:?}; {error}",
                    String::from_utf8_lossy(&output)
                );
            }
        }
    }
    let rendered = String::from_utf8(output).unwrap();
    let bytes: Vec<u8> = rendered
        .strip_prefix("ready")
        .unwrap()
        .split_whitespace()
        .map(|part| u8::from_str_radix(part, 16).unwrap())
        .collect();
    assert_eq!(bytes, [0x1b, 0x5b, 0x4d, 0x80, 0xff]);
    gitpulse_lib::terminal::shutdown_sessions(&state).unwrap();
}

#[test]
fn close_wakes_a_reader_blocked_on_a_full_output_window() {
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let state = TerminalSessions::default();
    let _cleanup = TerminalCleanup(state.clone());
    let dir = repo();
    let total = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let received = total.clone();
    let (send, receive) = mpsc::channel();
    app.listen("terminal-output", move |event| {
        let data: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
        let bytes = STANDARD.decode(data["data_b64"].as_str().unwrap()).unwrap();
        if received.fetch_add(bytes.len(), std::sync::atomic::Ordering::SeqCst) + bytes.len()
            >= 256 * 1024 - 4096
        {
            let _ = send.send(());
        }
    });
    let session = spawn_session(
        app.handle(),
        &state,
        dir.path().to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec![
            "-c".into(),
            "while :; do printf 0123456789abcdef; done".into(),
        ]),
        None,
    )
    .unwrap();
    receive.recv_timeout(Duration::from_secs(5)).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let pending = total.load(std::sync::atomic::Ordering::SeqCst);
    assert!((256 * 1024 - 4096..=256 * 1024).contains(&pending));
    kill_session(&state, &session.id).unwrap();
    gitpulse_lib::terminal::shutdown_sessions(&state).unwrap();
}
