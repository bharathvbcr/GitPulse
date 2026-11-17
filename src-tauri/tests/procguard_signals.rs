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

// The hermetic test environment: a per-process home, git config and trust
// store, rewritten before `main`. See `src/test_support/env.rs`.
#[path = "../src/test_support/env.rs"]
mod hermetic_env;

use gitpulse_lib::procguard::LockedSpawn;
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

        // Persist the explicit fixture approval in a child with its own HOME.
        // Neither the developer's grant store nor parent-process environment is changed.
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        process_trust::approve(&repo, &home);

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
        command
            .env("PATH", path)
            .env("PIDDIR", self.dir.path())
            .env("HOME", self.dir.path().join("home"))
            .env("XDG_CONFIG_HOME", self.dir.path().join("home/.config"));
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
                let child: i32 = parts.next()?.parse().ok()?;
                let grandchild: i32 = parts.next()?.parse().ok()?;
                // 0, 1 and negatives address more than one process.
                (child > 1 && grandchild > 1).then_some((child, grandchild))
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
        .output_locked()
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
        .output_locked()
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
    // The daemon takes a per-profile lock; a scratch profile keeps this test
    // off the developer's real one (and off a daemon running there).
    let profile = tempfile::tempdir().expect("profile dir");
    let mut command = Command::new(env!("CARGO_BIN_EXE_gitpulsed"));
    command
        .arg("--once")
        .arg(fixture.repo())
        .env("GITPULSE_LOG_DIR", log_dir.path())
        .env("HOME", profile.path())
        .env("XDG_CONFIG_HOME", profile.path())
        .env("APPDATA", profile.path())
        .stdin(Stdio::null())
        // To files rather than to `null`: a failure here has to be able to say
        // what the daemon thought it was doing, and pipes would need a drain
        // thread to avoid deadlocking on a full one.
        .stdout(Stdio::from(fixture.create("stdout")))
        .stderr(Stdio::from(fixture.create("stderr")));
    fixture.shim(&mut command);
    let mut daemon = command.spawn_locked().expect("spawn gitpulsed");

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
    let mut server = command.spawn_locked().expect("spawn gitpulse-mcp");
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
        children.push(command.spawn_locked().expect("spawn shell"));
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
    let out = command.output_locked().expect("which git");
    let resolved = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(
        resolved,
        fixture.dir.path().join("bin/git").display().to_string(),
        "PATH did not resolve git to the shim"
    );
}

/// Proves that the signal blast-radius guard prevents signals from targeting
/// PID 0, PID 1, PID -1, or any part of the host process tree (current process,
/// parent process, process group, or session), under both spawned and session
/// bases.
#[test]
fn adversarial_signal_blast_radius_guard_blocks_host_tree() {
    use gitpulse_lib::procguard::signal::{self, Basis, Me};

    let me = Me::current();
    assert!(me.pid > 1, "me.pid={}", me.pid);
    assert!(me.ppid > 1, "me.ppid={}", me.ppid);
    assert!(me.pgrp > 1, "me.pgrp={}", me.pgrp);
    // A job started by a service manager can share launchd's session: the macOS
    // GitHub runner reports `me.sid=1`. The session id is then also a boundary id,
    // which the loops below still refuse, so the guard stays exercised; what that
    // host cannot show is a refusal by session membership alone, which step 5b
    // proves with ids no boundary check could explain.
    assert!(me.sid >= 1, "me.sid={}", me.sid);

    // Host tree IDs: me.pid, me.ppid, me.pgrp, me.sid
    let host_ids = [me.pid, me.ppid, me.pgrp, me.sid];

    // Boundary and wrap IDs: 0, 1, -1, -2, i32::MIN, u32::MAX as i32
    let boundary_ids = [0, 1, -1, -2, i32::MIN, u32::MAX as i32];

    // 1. Verify signal_group rejects all host and boundary IDs under Basis::Spawned
    for id in host_ids.iter().chain(boundary_ids.iter()) {
        let res = signal::signal_group(*id, libc::SIGKILL, Basis::Spawned, "adversarial-test");
        assert!(res.is_err(), "signal_group should reject id {id}");
        let err = res.unwrap_err();
        assert!(
            signal::is_refusal(&err),
            "error for id {id} should be refusal: {err}"
        );
    }

    // 2. Verify signal_group rejects host IDs under Basis::Session(me.sid)
    for id in host_ids.iter() {
        let res = signal::signal_group(
            *id,
            libc::SIGKILL,
            Basis::Session(me.sid),
            "adversarial-test",
        );
        assert!(
            res.is_err(),
            "signal_group in host session should reject id {id}"
        );
        let err = res.unwrap_err();
        assert!(
            signal::is_refusal(&err),
            "error for id {id} in host session should be refusal: {err}"
        );
    }

    // 3. Verify signal_process rejects all host and boundary IDs
    for id in host_ids.iter().chain(boundary_ids.iter()) {
        let res = signal::signal_process(*id, 1, &|_| Some(1), libc::SIGKILL, "adversarial-test");
        assert!(res.is_err(), "signal_process should reject id {id}");
        let err = res.unwrap_err();
        assert!(
            signal::is_refusal(&err),
            "error for id {id} should be refusal: {err}"
        );
    }

    // 4. Verify vet_group specifically refuses host session under Basis::Spawned
    // even when OS reports group == me.sid and sid == me.sid (the shell hanging up hazard)
    let mock_pgid = |pid: i32| (pid == me.sid).then_some(me.sid);
    let mock_sid = |pid: i32| (pid == me.sid).then_some(me.sid);
    let vet_res = signal::vet_group(me.sid, Basis::Spawned, me, &mock_pgid, &mock_sid);
    assert!(
        vet_res.is_err(),
        "vet_group must refuse host session under Basis::Spawned"
    );
    assert!(signal::is_refusal(&vet_res.unwrap_err()));

    // 5. Verify vet_group specifically refuses parent PID (me.ppid)
    let mock_pgid_ppid = |pid: i32| (pid == me.ppid).then_some(me.ppid);
    let mock_sid_ppid = |pid: i32| (pid == me.ppid).then_some(me.sid);
    let vet_ppid = signal::vet_group(me.ppid, Basis::Spawned, me, &mock_pgid_ppid, &mock_sid_ppid);
    assert!(vet_ppid.is_err(), "vet_group must refuse parent PID");
    assert!(signal::is_refusal(&vet_ppid.unwrap_err()));

    // 5b. The same refusals for a host whose ids are all above the boundary
    // (`plain_id` rejects pgid <= 1 first), so each is refused as a member of
    // the host tree, whatever session this process happens to run in.
    let host = Me {
        pid: 40_001,
        ppid: 40_000,
        pgrp: 40_002,
        sid: 40_003,
    };
    for member in [host.pid, host.ppid, host.pgrp, host.sid] {
        let pgid_of = |pid: i32| (pid == member).then_some(member);
        let sid_of = |pid: i32| (pid == member).then_some(host.sid);
        for basis in [Basis::Spawned, Basis::Session(host.sid)] {
            let refused = signal::vet_group(member, basis, host, &pgid_of, &sid_of)
                .expect_err("a host tree member must be refused");
            assert!(
                signal::is_refusal(&refused),
                "member {member} of the host tree must be refused as a host tree group: {refused}"
            );
            assert!(
                refused.to_string().contains("host process tree"),
                "member {member} was refused for another reason: {refused}"
            );
        }
    }

    // 6. Verify that our host process and parent remain completely intact
    assert!(alive(me.pid), "this process must remain alive");
    assert!(alive(me.ppid), "parent process must remain alive");
}

#[path = "common/process_trust.rs"]
mod process_trust;
