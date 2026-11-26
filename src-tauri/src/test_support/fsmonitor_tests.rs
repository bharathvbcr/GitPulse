//! `FsmonitorDaemon` must leave no daemon behind, even for a git command that
//! runs after it dropped.
//!
//! The built-in daemon is a macOS and Windows feature; elsewhere
//! `core.fsmonitor=true` is a no-op, so there is nothing to leak or to guard.

use super::fsmonitor::FsmonitorDaemon;
use crate::procguard::LockedSpawn;
use std::path::Path;
use std::process::Command;

fn git(root: &Path, args: &[&str]) -> bool {
    let mut command = Command::new("git");
    super::pin_git(&mut command);
    command
        .args(args)
        .current_dir(root)
        .output_locked()
        .expect("spawn git")
        .status
        .success()
}

/// A daemon answers on this repository's socket.
fn daemon_running(root: &Path) -> bool {
    git(root, &["fsmonitor--daemon", "status"])
}

/// A repository with the built-in daemon switched on and running.
fn monitored_repo() -> tempfile::TempDir {
    let repo = super::git_repo();
    assert!(git(repo.path(), &["config", "core.fsmonitor", "true"]));
    assert!(git(repo.path(), &["status", "--porcelain"]));
    assert!(
        daemon_running(repo.path()),
        "this git did not start a built-in fsmonitor daemon"
    );
    repo
}

#[cfg(target_os = "macos")]
#[test]
fn dropping_the_guard_stops_the_daemon() {
    let repo = monitored_repo();
    drop(FsmonitorDaemon::new(repo.path()));
    assert!(
        !daemon_running(repo.path()),
        "the daemon outlived its guard"
    );
}

/// The leak: a reader still running when the guard drops (a watch loop whose
/// test failed while priming) issues one more `status`. Stopping the daemon
/// alone lets that read start a new one, which nothing stops before the
/// tempdir goes. The guard has to keep it from starting again.
#[cfg(target_os = "macos")]
#[test]
fn a_read_after_the_guard_drops_does_not_start_a_new_daemon() {
    let repo = monitored_repo();
    drop(FsmonitorDaemon::new(repo.path()));
    assert!(git(repo.path(), &["status", "--porcelain"]));
    assert!(
        !daemon_running(repo.path()),
        "a status after the guard dropped started a daemon nobody will stop"
    );
}
