//! A child this process spawned must not be running after this process is not.
//!
//! Both cases below were measured before they were fixed, and both look
//! identical from outside: the server or daemon is gone, its exit status is
//! the one you expect, and a `git` it started is still holding the user's
//! repository. The only way to see the difference is to record the child's pid
//! from inside the child and go looking for it afterwards, which is what the
//! `git` shim here is for.
//!
//! Unix only. Windows delivers no SIGTERM, and `src/procguard` says so rather
//! than pretending otherwise — a test that silently passed there would be
//! asserting nothing.
#![cfg(unix)]

use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long to wait for the shim to be spawned and record itself.
const SPAWN_WAIT: Duration = Duration::from_secs(30);

/// How long a child may take to disappear once its parent is gone. Generous:
/// it covers `procguard::CHILD_GRACE` plus the reparenting reap.
const DEATH_WAIT: Duration = Duration::from_secs(15);

/// A repository, and a `git` on `PATH` that never returns.
///
/// **Every** invocation blocks, not just the first. An earlier version of this
/// fixture let one invocation claim the block and answered the rest instantly,
/// which was a race rather than a simplification: both processes under test
/// run several git commands at once, so the one that got the instant empty
/// answer failed its own step and let the process finish and exit **0** while
/// the blocked one was still running. The test then measured its own fixture.
///
/// Blocking everything means neither process can complete a cycle, so the only
/// way either of them exits is the one under test.
struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("fixture dir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("bin")).expect("bin dir");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("repo dir");

        // The real git, resolved before the shim goes on PATH.
        run_git(&repo, &["init", "-q", "."]);
        run_git(&repo, &["config", "user.email", "test@example.com"]);
        run_git(&repo, &["config", "user.name", "Test"]);
        run_git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);

        let shim = root.join("bin/git");
        std::fs::write(
            &shim,
            // One short `printf` appended to a file opened `O_APPEND` is a
            // single write, so concurrent invocations interleave whole lines
            // rather than halves of two.
            //
            // The backgrounded `sleep` is the grandchild: it inherits the
            // pipes, so it is also the case a kill aimed only at the direct
            // child would miss.
            "#!/bin/sh\n\
             sleep 600 &\n\
             printf '%s %s\\n' \"$$\" \"$!\" >> \"$PIDDIR/pids\"\n\
             wait\n",
        )
        .expect("write shim");
        let mut perms = std::fs::metadata(&shim)
            .expect("shim metadata")
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&shim, perms).expect("chmod shim");

        Self { dir }
    }

    fn repo(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    /// Applies the shim and the pid directory to a command.
    fn shim(&self, command: &mut Command) {
        let path = format!(
            "{}:{}",
            self.dir.path().join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        command.env("PATH", path).env("PIDDIR", self.dir.path());
    }

    fn create(&self, name: &str) -> std::fs::File {
        std::fs::File::create(self.dir.path().join(name)).expect("create output file")
    }

    /// Whatever the process under test wrote, for a failure message.
    fn output(&self) -> String {
        ["stdout", "stderr"]
            .iter()
            .map(|name| {
                let text = std::fs::read_to_string(self.dir.path().join(name))
                    .unwrap_or_else(|e| format!("(unreadable: {e})"));
                format!("--- {name} ---\n{}", text.trim())
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every `(child, grandchild)` pair the shim has recorded so far.
    fn recorded(&self) -> Vec<(i32, i32)> {
        let Ok(text) = std::fs::read_to_string(self.dir.path().join("pids")) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
            })
            .collect()
    }

    /// Waits for at least one blocked `git`.
    fn blocked(&self) -> Vec<(i32, i32)> {
        let deadline = Instant::now() + SPAWN_WAIT;
        while Instant::now() < deadline {
            let recorded = self.recorded();
            if !recorded.is_empty() {
                return recorded;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!(
            "the process under test never ran `git`, so there was no child to \
             orphan (waited {SPAWN_WAIT:?})\n{}",
            self.output()
        );
    }
}

fn run_git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn alive(pid: i32) -> bool {
    // Signal 0 checks for existence without sending anything.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn describe(pid: i32) -> String {
    Command::new("ps")
        .args(["-o", "pid,ppid,pgid,stat,command", "-p", &pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|e| format!("(ps failed: {e})"))
}

/// Every pid the shim recorded must be gone, and a failure has to say which
/// one is still there and what it is.
fn assert_all_gone(fixture: &Fixture) {
    let recorded = fixture.recorded();
    assert!(!recorded.is_empty(), "nothing was recorded to check");
    let deadline = Instant::now() + DEATH_WAIT;
    let survivors: Vec<i32> = loop {
        let still_here: Vec<i32> = recorded
            .iter()
            .flat_map(|(child, grandchild)| [*child, *grandchild])
            .filter(|pid| alive(*pid))
            .collect();
        if still_here.is_empty() || Instant::now() >= deadline {
            break still_here;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if survivors.is_empty() {
        return;
    }
    let details: Vec<String> = survivors.iter().map(|pid| describe(*pid)).collect();
    // Leave nothing behind for the next test to trip over.
    for pid in &survivors {
        unsafe { libc::kill(*pid, libc::SIGKILL) };
    }
    panic!(
        "{} of {} recorded processes outlived the process that spawned them:\n{}",
        survivors.len(),
        recorded.len() * 2,
        details.join("\n")
    );
}

fn sigterm(child: &Child) {
    let pid = i32::try_from(child.id()).expect("pid fits");
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0, "SIGTERM");
}

/// The finding: a supervisor stopping GitPulse's daemon left the `git` it was
/// running — and everything that `git` had forked — alive and attached to the
/// user's repository.
///
/// Against the pre-fix binary those processes are still there afterwards, with
/// the daemon's own exit status already reported as 143.
#[test]
fn sigterm_reaps_the_git_child_and_its_grandchild() {
    let fixture = Fixture::new();
    let log_dir = tempfile::tempdir().expect("log dir");
    let mut command = Command::new(env!("CARGO_BIN_EXE_gitpulsed"));
    command
        .arg("--once")
        .arg(fixture.repo())
        .env("GITPULSE_LOG_DIR", log_dir.path())
        .stdin(Stdio::null())
        // To files rather than to `null`: a failure here has to be able to say
        // what the daemon thought it was doing, and pipes would need a drain
        // thread to avoid deadlocking on a full one.
        .stdout(Stdio::from(fixture.create("stdout")))
        .stderr(Stdio::from(fixture.create("stderr")));
    fixture.shim(&mut command);
    let mut daemon = command.spawn().expect("spawn gitpulsed");

    // Without this the test could pass because the fixture never worked: a
    // child that was already gone is not evidence of a child that was reaped.
    for (child, grandchild) in fixture.blocked() {
        assert!(alive(child), "the shim exited before the signal was sent");
        assert!(alive(grandchild), "the grandchild exited before the signal");
    }

    sigterm(&daemon);
    let status = daemon.wait().expect("reap gitpulsed");
    // 128 + SIGTERM exactly, not merely "not success": a daemon killed by the
    // default action reports no code at all, and that difference is how this
    // test can tell the handler ran from the handler never having been armed.
    assert_eq!(
        status.code(),
        Some(128 + libc::SIGTERM),
        "expected the shutdown handler's own exit status, got {status:?}; it said:\n{}",
        fixture.output()
    );

    assert_all_gone(&fixture);
}

/// The same hole on the path `gitpulse-mcp` takes deliberately: stdin closes,
/// the drain gives in-flight workers their budget, and then `process::exit`
/// runs no destructor that would stop their subprocesses.
///
/// The call budget is shortened so the drain gives up while the shim is still
/// blocking, which is the only state in which the bug is observable.
#[test]
fn closing_stdin_reaps_a_git_child_a_worker_left_running() {
    let fixture = Fixture::new();
    let log_dir = tempfile::tempdir().expect("log dir");
    let mut command = Command::new(env!("CARGO_BIN_EXE_gitpulse-mcp"));
    command
        .env("GITPULSE_LOG_DIR", log_dir.path())
        .env("GITPULSE_MCP_CALL_TIMEOUT_MS", "300")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(fixture.create("stderr")));
    fixture.shim(&mut command);
    let mut server = command.spawn().expect("spawn gitpulse-mcp");
    let mut stdin = server.stdin.take().expect("stdin");
    let stdout = server.stdout.take().expect("stdout");
    // Drained on its own thread: a server blocked writing stdout stops reading
    // stdin, and the test would hang rather than fail.
    let drain = std::thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = BufReader::new(stdout).read_to_end(&mut sink);
        sink
    });

    let meta = serde_json::json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {}
    });
    for line in [
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2026-07-28", "capabilities": {},
                        "clientInfo": { "name": "procguard-test", "version": "0" },
                        "_meta": meta }
        }),
        serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "gitpulse_status",
                        "arguments": { "repo_path": fixture.repo() },
                        "_meta": meta }
        }),
    ] {
        writeln!(stdin, "{line}").expect("write request");
    }
    stdin.flush().expect("flush");

    for (child, grandchild) in fixture.blocked() {
        assert!(alive(child), "the shim exited before stdin was closed");
        assert!(
            alive(grandchild),
            "the grandchild exited before stdin closed"
        );
    }

    // The documented shutdown: close stdin and let the server exit by itself.
    drop(stdin);
    let status = server.wait().expect("reap gitpulse-mcp");
    let _ = drain.join();
    assert!(
        status.success(),
        "a clean stdin close is exit 0, got {status:?}; it said:\n{}",
        fixture.output()
    );

    assert_all_gone(&fixture);
}

/// Guards the fixture the two tests above are built on: a `git` that produces
/// no output must still record itself, and every invocation must block. A
/// shim that answered any call would let the process under test finish on its
/// own, and both tests would then be measuring their own setup.
#[test]
fn every_shim_invocation_blocks_and_records_itself() {
    let fixture = Fixture::new();
    let mut children = Vec::new();
    for label in ["one", "two", "three"] {
        let mut command = Command::new("sh");
        command.arg("-c").arg(format!("git {label}"));
        fixture.shim(&mut command);
        command.stdout(Stdio::null()).stderr(Stdio::null());
        children.push(command.spawn().expect("spawn shell"));
    }

    let deadline = Instant::now() + SPAWN_WAIT;
    while fixture.recorded().len() < 3 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    let recorded = fixture.recorded();
    assert_eq!(
        recorded.len(),
        3,
        "an invocation returned instead of blocking, or lost its record"
    );
    for (child, grandchild) in &recorded {
        assert!(alive(*child), "a shim exited instead of blocking");
        assert!(
            alive(*grandchild),
            "a grandchild exited instead of blocking"
        );
    }

    for mut child in children {
        let _ = child.kill();
        let _ = child.wait();
    }
    for (child, grandchild) in recorded {
        unsafe {
            libc::kill(child, libc::SIGKILL);
            libc::kill(grandchild, libc::SIGKILL);
        }
    }
}

/// Keeps the shim honest about being on PATH at all: if the process under test
/// resolved `git` to an absolute path instead, every assertion above would be
/// vacuous.
#[test]
fn the_shim_is_what_path_resolves_git_to() {
    let fixture = Fixture::new();
    let mut command = Command::new("sh");
    command.arg("-c").arg("command -v git");
    fixture.shim(&mut command);
    let out = command.output().expect("which git");
    let resolved = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(
        resolved,
        fixture.dir.path().join("bin/git").display().to_string(),
        "PATH did not resolve git to the shim"
    );
}
