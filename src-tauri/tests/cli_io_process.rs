//! Exercise inherited OS streams at the real executable boundary.
use std::fs::{self, File};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

fn wait(child: &mut Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("CLI exceeded its I/O deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn hook_refuses_oversized_json_before_parsing_it() {
    let input = format!(r#"{{"cwd":"{}"}}"#, "a".repeat(4 * 1024 * 1024));
    assert!(gitpulse_lib::hooks::parse_input(&input).is_err());
    assert!(gitpulse_lib::hooks::parse_input("{\n  \"cwd\": \"\"\n}\n").is_ok());
}

#[test]
fn hook_bounds_an_open_input_pipe_without_emitting_a_decision() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("stdout");
    let mut child = Command::new(env!("CARGO_BIN_EXE_gitpulse-hook"))
        .arg("collision-guard")
        .env(gitpulse_lib::logging::LOG_DIR_ENV, dir.path())
        .stdin(Stdio::piped())
        .stdout(File::create(&output).unwrap())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Keep stdin open without EOF: a host disconnect deadline cannot depend
    // on a newline, valid JSON, or the host closing its descriptor.
    let held_input = child.stdin.take().unwrap();
    assert_eq!(wait(&mut child, Duration::from_secs(8)).code(), Some(0));
    drop(held_input);
    assert!(fs::read(output).unwrap().is_empty());
    assert!(fs::read_to_string(dir.path().join("gitpulse-hook.log"))
        .unwrap()
        .contains("stdin"));
}

#[cfg(unix)]
fn pipe_output(full: bool) -> (Stdio, Option<File>) {
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd};
    let mut descriptors = [-1; 2];
    // SAFETY: pipe initializes two owned descriptors on success.
    assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
    let reader = unsafe { File::from_raw_fd(descriptors[0]) };
    let mut writer = unsafe { File::from_raw_fd(descriptors[1]) };
    // posix_spawn needs a live read end to attach the write end as stdout.
    // The child must not inherit that reader: it would keep the pipe readable
    // and `--help` would exit 0 instead of reporting a broken stdout.
    let reader_flags = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFD) };
    assert!(reader_flags >= 0);
    assert_eq!(
        unsafe {
            libc::fcntl(
                reader.as_raw_fd(),
                libc::F_SETFD,
                reader_flags | libc::FD_CLOEXEC,
            )
        },
        0
    );
    if !full {
        return (Stdio::from(writer), Some(reader));
    }
    let fd = writer.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let mut saturated = false;
    for _ in 0..1024 {
        match writer.write(&[0; 4096]) {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                saturated = true;
                break;
            }
            Err(error) => panic!("fill pipe: {error}"),
        }
    }
    assert!(saturated);
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFL, flags) }, 0);
    (Stdio::from(writer), Some(reader))
}

#[cfg(unix)]
#[test]
fn daemon_output_failure_has_a_controlled_exit_instead_of_panicking_or_hanging() {
    for full in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_gitpulsed"));
        command
            .arg("--help")
            .env(gitpulse_lib::logging::LOG_DIR_ENV, dir.path())
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        // Broken pipe: std's piped stdout keeps a reader alive through
        // posix_spawn, then this drops it before `--help` writes. Closing the
        // reader before spawn made macOS inherit the test harness stdout, so
        // help exited 0. Holding a custom reader until after spawn let USAGE
        // land in the pipe buffer and also exit 0.
        let (mut child, held_reader) = if full {
            let (stdout, reader) = pipe_output(true);
            (command.stdout(stdout).spawn().unwrap(), reader)
        } else {
            let mut child = command.stdout(Stdio::piped()).spawn().unwrap();
            drop(child.stdout.take());
            (child, None)
        };
        assert_eq!(
            wait(&mut child, Duration::from_secs(5)).code(),
            Some(1),
            "full={full}"
        );
        drop(held_reader);
        let log = fs::read_to_string(dir.path().join("gitpulsed.log")).unwrap();
        assert!(log.contains("stdout"));
        assert!(!log.contains("[panic]"));
    }
}

#[cfg(unix)]
#[test]
fn argument_errors_keep_their_exit_contract_when_stderr_is_closed() {
    for (binary, expected) in [
        (env!("CARGO_BIN_EXE_gitpulse-hook"), 0),
        (env!("CARGO_BIN_EXE_gitpulsed"), 2),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (stderr, reader) = pipe_output(false);
        let mut child = Command::new(binary)
            .env(gitpulse_lib::logging::LOG_DIR_ENV, dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .unwrap();
        drop(reader);
        assert_eq!(
            wait(&mut child, Duration::from_secs(5)).code(),
            Some(expected)
        );
    }
}

#[cfg(unix)]
#[test]
fn mcp_stops_when_the_host_keeps_stdout_open_but_stops_reading() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let (stdout, held_reader) = pipe_output(true);
    let mut child = Command::new(env!("CARGO_BIN_EXE_gitpulse-mcp"))
        .env(gitpulse_lib::logging::LOG_DIR_ENV, dir.path())
        .stdin(Stdio::piped())
        .stdout(stdout)
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{invalid json}\n")
        .unwrap();
    assert_eq!(wait(&mut child, Duration::from_secs(5)).code(), Some(0));
    drop(held_reader);
    let saved = fs::read_to_string(dir.path().join("gitpulse-mcp.log")).unwrap();
    assert!(saved.contains("stdout"));
    assert!(!saved.contains("[panic]"));
}

#[cfg(unix)]
#[test]
fn mcp_output_failure_interrupts_an_idle_input_reader() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let (stdout, held_reader) = pipe_output(true);
    let mut child = Command::new(env!("CARGO_BIN_EXE_gitpulse-mcp"))
        .env(gitpulse_lib::logging::LOG_DIR_ENV, dir.path())
        .stdin(Stdio::piped())
        .stdout(stdout)
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let request = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": gitpulse_lib::mcp::PROTOCOL_VERSION,
                "io.modelcontextprotocol/clientInfo": { "name": "idle-input-probe", "version": "0" },
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        }
    });
    assert!(
        matches!(
            gitpulse_lib::mcp::accept(
                serde_json::from_value(request.clone()).unwrap(),
                &mut gitpulse_lib::mcp::Era::Unknown
            ),
            gitpulse_lib::mcp::Accepted::Ready(_)
        ),
        "this regression must exercise an asynchronous request"
    );
    writeln!(input, "{request}").unwrap();
    assert_eq!(wait(&mut child, Duration::from_secs(5)).code(), Some(0));
    drop(input);
    drop(held_reader);
}
