//! Exercise OS stream failures outside libtest's stderr capture and panic hook.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const CHILD_MODE: &str = "GITPULSE_LOGGING_PROBE";

extern "C" fn native_log_callback() {
    log::warn!(target: "logging-probe", "native callback survived");
}

#[test]
fn logging_child() {
    let Ok(mode) = std::env::var(CHILD_MODE) else {
        return;
    };
    // The parent closes the pipe reader before releasing this barrier.
    std::io::stdin().read_exact(&mut [0]).expect("start signal");
    #[cfg(unix)]
    if let Ok(pipe_mode) = std::env::var("GITPULSE_PROBE_FULL_STDERR") {
        // Only the child changes its descriptor. Retaining the parent's pipe
        // reader distinguishes backpressure (EAGAIN) from a broken pipe.
        unsafe {
            let flags = libc::fcntl(libc::STDERR_FILENO, libc::F_GETFL);
            assert!(flags >= 0);
            assert_eq!(
                libc::fcntl(libc::STDERR_FILENO, libc::F_SETFL, flags | libc::O_NONBLOCK),
                0
            );
        }
        let mut full = false;
        for _ in 0..256 {
            match std::io::stderr().write(&[b'x'; 4096]) {
                Ok(_) => (),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    full = true;
                    break;
                }
                Err(error) => panic!("fill stderr: {error}"),
            }
        }
        assert!(full, "test pipe did not fill within 1 MiB");
        if pipe_mode == "blocking" {
            unsafe {
                let flags = libc::fcntl(libc::STDERR_FILENO, libc::F_GETFL);
                assert!(flags >= 0);
                assert_eq!(
                    libc::fcntl(
                        libc::STDERR_FILENO,
                        libc::F_SETFL,
                        flags & !libc::O_NONBLOCK
                    ),
                    0
                );
            }
        }
    }
    gitpulse_lib::logging::init();
    gitpulse_lib::logging::install_panic_hook();
    if mode == "panic" {
        let caught = std::panic::catch_unwind(|| {
            panic!("logging-panic-probe access_token=synthetic-panic-secret-123456789")
        });
        assert!(caught.is_err(), "original panic must still unwind");
    } else {
        native_log_callback();
        std::thread::scope(|scope| {
            for worker in 0..4 {
                scope.spawn(move || {
                    for entry in 0..25 {
                        log::info!(target: "logging-probe", "worker {worker} entry {entry}");
                    }
                });
            }
        });
    }
    log::info!(target: "logging-probe", "logging continued");
    let tail = gitpulse_lib::logging::diagnostic_tail(500);
    assert!(tail.iter().any(|line| line.contains("logging continued")));
    if mode == "panic" {
        assert!(tail.iter().any(|line| line.contains("logging-panic-probe")));
        assert!(tail.iter().any(|line| line.contains("backtrace (")));
    } else {
        assert_eq!(
            tail.iter().filter(|line| line.contains("worker ")).count(),
            100
        );
    }
    let persisted = gitpulse_lib::logging::persisted_log(500);
    let dir = std::env::var_os(gitpulse_lib::logging::LOG_DIR_ENV).unwrap();
    fs::write(
        std::path::PathBuf::from(dir).join(
            std::env::var("GITPULSE_PROBE_STATUS_FILE")
                .unwrap_or_else(|_| "durable-status.json".into()),
        ),
        serde_json::to_vec(&persisted).unwrap(),
    )
    .unwrap();
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum StderrKind {
    Healthy,
    BrokenPipe,
    #[cfg(unix)]
    FullPipe,
    #[cfg(unix)]
    BlockingFullPipe,
    #[cfg(unix)]
    ReadOnly,
}

fn run_probe(mode: &str, stderr_kind: StderrKind) {
    let dir = tempfile::tempdir().expect("probe directory");
    let output_path = dir.path().join("stdout.txt");
    let stderr = match stderr_kind {
        StderrKind::Healthy => Stdio::null(),
        StderrKind::BrokenPipe => Stdio::piped(),
        #[cfg(unix)]
        StderrKind::FullPipe => Stdio::piped(),
        #[cfg(unix)]
        StderrKind::BlockingFullPipe => Stdio::piped(),
        #[cfg(unix)]
        StderrKind::ReadOnly => {
            let path = dir.path().join("read-only-stderr");
            fs::write(&path, b"").expect("stderr file");
            Stdio::from(File::open(path).expect("read-only descriptor"))
        }
    };
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    #[cfg(unix)]
    if stderr_kind == StderrKind::FullPipe {
        command.env("GITPULSE_PROBE_FULL_STDERR", "1");
    }
    #[cfg(unix)]
    if stderr_kind == StderrKind::BlockingFullPipe {
        command.env("GITPULSE_PROBE_FULL_STDERR", "blocking");
    }
    let mut child = command
        .args(["--exact", "logging_child", "--nocapture"])
        .env(CHILD_MODE, mode)
        .env(gitpulse_lib::logging::LOG_DIR_ENV, dir.path())
        .stdin(Stdio::piped())
        .stdout(File::create(&output_path).expect("probe output"))
        .stderr(stderr)
        .spawn()
        .expect("spawn probe");
    if stderr_kind == StderrKind::BrokenPipe {
        drop(child.stderr.take());
    }
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"x")
        .expect("release probe");
    let status = wait_for_probe(&mut child);
    assert!(
        status.success(),
        "{mode}, stderr={stderr_kind:?}: {status}; {}",
        fs::read_to_string(output_path).expect("probe output")
    );
    let log_path = fs::read_dir(dir.path())
        .expect("logs")
        .map(|entry| entry.expect("log entry").path())
        .find(|path| path.extension().is_some_and(|ext| ext == "log"))
        .expect("durable log");
    let saved = fs::read_to_string(log_path).expect("durable content");
    assert!(saved.contains("logging continued"));
    // Rust's Stderr deliberately treats EBADF as a successful discarded write.
    // Broken/full pipes return errors; only those can produce a failure notice.
    let reports_failure = match stderr_kind {
        StderrKind::BrokenPipe => true,
        #[cfg(unix)]
        StderrKind::FullPipe => true,
        #[cfg(unix)]
        StderrKind::BlockingFullPipe => true,
        _ => false,
    };
    assert_eq!(
        saved.matches("stderr mirror disabled").count(),
        usize::from(reports_failure),
        "{saved}"
    );
    if mode == "panic" {
        assert!(saved.contains("logging-panic-probe"));
        assert!(saved.contains("backtrace ("));
    }
}

fn wait_for_probe(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().expect("probe status") {
            return status;
        }
        if Instant::now() >= deadline {
            child.kill().expect("kill timed out probe");
            child.wait().expect("reap probe");
            panic!("probe timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn mcp_keeps_serving_after_stderr_disconnects_and_exits_cleanly() {
    use serde_json::{json, Value};

    let dir = tempfile::tempdir().expect("probe directory");
    let output = dir.path().join("wire.jsonl");
    let mut child = Command::new(env!("CARGO_BIN_EXE_gitpulse-mcp"))
        .env(gitpulse_lib::logging::LOG_DIR_ENV, dir.path())
        .stdin(Stdio::piped())
        .stdout(File::create(&output).expect("wire output"))
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn MCP");
    drop(child.stderr.take());
    let mut input = child.stdin.take().expect("MCP stdin");
    // A malformed notification logs a warning before tools/list executes.
    // This exercises both dispatch and shutdown with a disconnected log sink.
    let input_result = writeln!(
        input,
        "{}\n{}",
        json!({"jsonrpc":"2.0","method":42}),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{
            "_meta": {"io.modelcontextprotocol/protocolVersion":"2026-07-28",
                "io.modelcontextprotocol/clientCapabilities":{}}
        }})
    );
    drop(input);
    let status = wait_for_probe(&mut child);
    assert!(
        status.success(),
        "MCP exited {status}; input: {input_result:?}"
    );
    input_result.expect("write MCP requests");
    let messages: Vec<Value> = fs::read_to_string(output)
        .expect("wire")
        .lines()
        .map(|line| serde_json::from_str(line).expect("only JSON on stdout"))
        .collect();
    assert!(
        messages.iter().any(|reply| reply["id"] == 1
            && reply["result"]["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty())),
        "{messages:?}"
    );
    let saved = fs::read_to_string(dir.path().join("gitpulse-mcp.log")).expect("MCP log");
    assert!(saved.contains("stdin closed; exiting"));
    assert!(!saved.contains("[panic]"));
}

#[test]
fn healthy_stderr_preserves_logs_and_panic_unwinding() {
    run_probe("log", StderrKind::Healthy);
    run_probe("panic", StderrKind::Healthy);
}

#[test]
fn simultaneous_processes_preserve_complete_shared_log_records() {
    let dir = tempfile::tempdir().unwrap();
    let mut children = Vec::new();
    for index in 0..8 {
        children.push(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "logging_child", "--nocapture"])
                .env(CHILD_MODE, "log")
                .env("GITPULSE_PROBE_STATUS_FILE", format!("status-{index}.json"))
                .env(gitpulse_lib::logging::LOG_DIR_ENV, dir.path())
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
    }
    for child in &mut children {
        child.stdin.take().unwrap().write_all(b"x").unwrap();
    }
    for child in &mut children {
        assert!(wait_for_probe(child).success());
    }
    let path = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|extension| extension == "log"))
        .unwrap();
    let saved = fs::read_to_string(path).unwrap();
    let states: Vec<serde_json::Value> = (0..8)
        .map(|index| {
            let value: serde_json::Value = serde_json::from_slice(
                &fs::read(dir.path().join(format!("status-{index}.json"))).unwrap(),
            )
            .unwrap();
            value["degraded"].clone()
        })
        .collect();
    assert_eq!(
        saved
            .lines()
            .filter(|line| line.contains("worker "))
            .count(),
        800,
        "disk states: {states:?}"
    );
    assert_eq!(
        saved
            .lines()
            .filter(|line| line.contains("logging continued"))
            .count(),
        8
    );
    assert!(saved
        .lines()
        .all(|line| line.starts_with("--- ") || line.contains(" [logging-probe] ")));
}

#[test]
fn disconnected_stderr_does_not_abort_native_callbacks_or_concurrent_logging() {
    run_probe("log", StderrKind::BrokenPipe);
}

#[test]
fn disconnected_stderr_does_not_abort_the_panic_hook() {
    run_probe("panic", StderrKind::BrokenPipe);
}

#[cfg(unix)]
#[test]
fn nonblocking_full_stderr_does_not_hang_or_abort() {
    run_probe("log", StderrKind::FullPipe);
    run_probe("panic", StderrKind::FullPipe);
}

#[cfg(unix)]
#[test]
fn unwritable_stderr_does_not_abort() {
    run_probe("log", StderrKind::ReadOnly);
    run_probe("panic", StderrKind::ReadOnly);
}

#[cfg(unix)]
#[test]
fn blocking_full_stderr_cannot_stall_logging_or_panic_recovery() {
    run_probe("log", StderrKind::BlockingFullPipe);
    run_probe("panic", StderrKind::BlockingFullPipe);
}

#[test]
fn panic_diagnostics_never_bypass_redaction_on_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stderr.txt");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "logging_child", "--nocapture"])
        .env(CHILD_MODE, "panic")
        .env(gitpulse_lib::logging::LOG_DIR_ENV, dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(File::create(&path).unwrap())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"x").unwrap();
    assert!(wait_for_probe(&mut child).success());
    let stderr = fs::read_to_string(path).unwrap();
    assert!(
        stderr.contains("logging-panic-probe"),
        "panic must remain observable"
    );
    assert!(
        !stderr.contains("synthetic-panic-secret-123456789"),
        "raw panic payload bypassed the redaction boundary"
    );
}

#[cfg(unix)]
fn unsafe_log_path_probe(kind: &str) {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs");
    fs::create_dir(&logs).unwrap();
    let exe = std::env::current_exe().unwrap();
    let stem = exe.file_stem().unwrap().to_str().unwrap();
    let target = dir.path().join("must-not-change");
    fs::write(&target, "private sentinel").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
    let path = logs.join(format!(
        "{stem}.log{}",
        if kind == "read-fifo" { ".1" } else { "" }
    ));
    match kind {
        "symlink" => symlink(&target, &path).unwrap(),
        "hardlink" => fs::hard_link(&target, &path).unwrap(),
        "write-fifo" | "read-fifo" => {
            use std::os::unix::ffi::OsStrExt;
            let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        }
        _ => unreachable!(),
    }
    let mut child = Command::new(&exe)
        .args(["--exact", "logging_child", "--nocapture"])
        .env(CHILD_MODE, "log")
        .env(gitpulse_lib::logging::LOG_DIR_ENV, &logs)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"x").unwrap();
    assert!(wait_for_probe(&mut child).success(), "{kind}");
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "private sentinel",
        "{kind} modified a foreign file"
    );
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o640,
        "{kind} changed foreign permissions"
    );
    let status: serde_json::Value =
        serde_json::from_slice(&fs::read(logs.join("durable-status.json")).unwrap()).unwrap();
    assert!(
        status["degraded"].is_string(),
        "unsafe {kind} must report unavailable/incomplete diagnostics"
    );
    assert!(
        !status["lines"].to_string().contains("private sentinel"),
        "{kind} read a foreign file"
    );
}

#[cfg(unix)]
#[test]
fn log_files_refuse_symlinks_without_changing_the_target() {
    unsafe_log_path_probe("symlink");
}

#[cfg(unix)]
#[test]
fn log_files_refuse_hardlinks_without_changing_the_target() {
    unsafe_log_path_probe("hardlink");
}

#[cfg(unix)]
#[test]
fn log_open_cannot_block_on_a_fifo() {
    unsafe_log_path_probe("write-fifo");
}

#[cfg(unix)]
#[test]
fn persisted_log_cannot_block_on_a_fifo() {
    unsafe_log_path_probe("read-fifo");
}
