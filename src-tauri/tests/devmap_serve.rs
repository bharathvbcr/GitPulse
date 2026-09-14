//! The `devmap serve` client, proved against a real daemon.
//!
//! Everything else about this protocol is pinned by unit tests over captured
//! envelopes, which is the right way to fix the parsing but cannot show that
//! the shapes match the daemon. The two facts that matter to the live gate
//! came from measuring a running kernel rather than reading its source:
//!
//! * a `status` reply carries `is_fresh`, `degraded_reason`, `pending_count`
//!   and the generation counts — but **not** `schema_outdated`,
//!   `rebuild_required` or `schema_relation`. That absence is why the gate
//!   still spawns `devmap status --json` for schema decisions instead of
//!   trusting the socket for everything;
//! * a socket file outliving its daemon answers nothing, so liveness has to be
//!   a round trip and cannot be `Path::exists`.
//!
//! Skipped loudly, never silently: without an installed `devmap` this prints
//! why and returns rather than passing on an absence.

// The whole file measures a unix-socket daemon: its single test was already
// `#[cfg(unix)]`, but the fixtures and imports feeding it were not, so on
// Windows they compiled with no caller and `-D warnings` read them — correctly
// — as dead code. Gating the module is the same shape the other unix-only
// integration tests here use, and keeps the two from drifting apart again.
#![cfg(unix)]

mod common;

use gitpulse_lib::devmap;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

fn scratch_repo() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let root = dir.path();
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "t@example.invalid"]);
    git(root, &["config", "user.name", "Test"]);
    std::fs::write(
        root.join("main.rs"),
        "fn main() { helper(); }\nfn helper() -> u32 { 1 }\n",
    )
    .expect("write");
    git(root, &["add", "-A"]);
    git(root, &["commit", "-m", "init"]);
    common::trust_repo(root);
    dir
}

/// Kills the daemon however the test ends, so a failing assertion cannot leave
/// a watcher running against a temp directory that is about to be deleted.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_live_daemon_answers_and_a_dead_one_is_reported_rather_than_assumed() {
    let Ok(binary) = devmap::resolve_binary() else {
        eprintln!("SKIPPED: no `devmap` binary resolved; nothing to measure against");
        return;
    };
    let repo = scratch_repo();
    let root = repo.path();
    let path = root.to_string_lossy().into_owned();

    // A store to serve. Without this the daemon still starts, but the test
    // would be measuring an empty repository rather than a working index.
    devmap::initialize_repository(&path, std::slice::from_ref(&path)).expect("initialize");
    assert!(devmap::build(&path).expect("build").ok);

    let socket = devmap::serve::socket_path(root).expect("socket path");
    assert!(
        !socket.is_empty(),
        "the binary computes the endpoint; we must never re-derive it"
    );

    // Nothing is serving this scratch repository yet, and that has to be said
    // rather than inferred.
    let before = devmap::serve::probe(&path).expect("probe");
    assert!(!before.serving, "unexpected daemon before one was started");
    assert!(
        before.reason.is_some(),
        "an absent daemon must carry its reason"
    );

    let daemon = Daemon(
        Command::new(&binary.path)
            .args(["serve", "--root", &path])
            .current_dir(root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn devmap serve"),
    );

    // Bounded wait: the daemon binds its endpoint after opening the store.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut live = devmap::serve::probe(&path).expect("probe");
    while !live.serving && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
        live = devmap::serve::probe(&path).expect("probe");
    }

    assert!(
        live.serving,
        "no answer from the daemon within the deadline: {:?}",
        live.reason
    );
    assert_eq!(live.reason, None, "a serving daemon carries no reason");
    assert_eq!(live.socket.as_deref(), Some(socket.as_str()));
    assert!(
        live.pending.is_some(),
        "the status reply must carry pending_count; the gate reads it to tell \
         'behind but being worked on' from 'behind and nobody is on it'"
    );

    drop(daemon);

    // The socket file routinely outlives the process that bound it, which is
    // exactly why liveness is a round trip. Poll: the kernel may take a moment
    // to release the endpoint.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut after = devmap::serve::probe(&path).expect("probe");
    while after.serving && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
        after = devmap::serve::probe(&path).expect("probe");
    }
    assert!(!after.serving, "a killed daemon still reported as serving");
    assert!(
        after.reason.is_some(),
        "a dead daemon must say why it is not serving"
    );
}
