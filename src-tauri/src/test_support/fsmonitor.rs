//! Owner of git's built-in fsmonitor daemon for a test repository.
//!
//! `core.fsmonitor=true` makes every git command that reads the repository
//! start a detached `git fsmonitor--daemon` the first time one runs, and that
//! daemon outlives the test process: reparented to launchd, holding the
//! repository's `fsmonitor--daemon.ipc` socket in a tempdir that is deleted
//! underneath it, and holding every descriptor it inherited (one held the
//! cargo build-slot lock and blocked builds machine-wide for hours).
//!
//! Stopping the daemon once is not enough. A test that starts a watch loop or
//! any product reader keeps issuing `git status` against the repository, and a
//! `status` that runs after the stop starts a new daemon, which nothing then
//! stops. This is what happened when a watcher test failed while priming: its
//! loop thread was still alive when the guard stopped the daemon and the
//! tempdir was removed. So the guard first turns the setting off in the
//! repository, which no later read can undo, and then stops what is running.
//!
//! Every test that sets `core.fsmonitor` to a truthy value owns one of these;
//! `tests/git_config_posture.rs` holds the sources to that.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// One bounded git call.
const GIT_BUDGET: Duration = Duration::from_secs(60);

/// Rounds of stop-then-look. A read that began before the setting was turned
/// off can still start a daemon a moment later; each round catches that one.
const ROUNDS: usize = 4;

/// Pause between rounds, for a read that is mid-start.
const SETTLE: Duration = Duration::from_millis(250);

/// Stops the repository's fsmonitor daemon when dropped, and keeps it stopped.
pub(crate) struct FsmonitorDaemon {
    root: PathBuf,
}

impl FsmonitorDaemon {
    /// Guards `root`, a repository the test has already trusted.
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }
}

/// Whether a daemon answers on `root`'s socket. A call that could not run
/// counts as running: "could not look" must not read as "stopped".
fn running(root: &Path) -> bool {
    git(root, &["fsmonitor--daemon", "status"]).is_none_or(|ok| ok)
}

/// `git args` in `root` under a deadline: `Some(success)`, or `None` when it
/// could not run or did not finish.
fn git(root: &Path, args: &[&str]) -> Option<bool> {
    let mut command = Command::new("git");
    // Pinned so an inherited GIT_DIR or GIT_WORK_TREE cannot aim the stop at
    // another repository than `root`.
    crate::test_support::pin_git(&mut command);
    command.args(args).current_dir(root);
    let label = format!("git {}", args.join(" "));
    crate::engine::process::run_bounded(
        command,
        &label,
        crate::test_support::coverage_relaxed(GIT_BUDGET)
            .min(crate::engine::process::NETWORK_TIMEOUT),
        None,
    )
    .ok()
    .map(|run| run.success)
}

impl Drop for FsmonitorDaemon {
    /// Never panics: this also runs while a failing test unwinds.
    fn drop(&mut self) {
        // Off first, so a reader still running cannot start another daemon.
        let _ = git(&self.root, &["config", "core.fsmonitor", "false"]);
        for round in 0..ROUNDS {
            let _ = git(&self.root, &["fsmonitor--daemon", "stop"]);
            if !running(&self.root) {
                return;
            }
            if round + 1 < ROUNDS {
                std::thread::sleep(SETTLE);
            }
        }
        eprintln!(
            "fsmonitor daemon for {} still answers after {ROUNDS} stops",
            self.root.display()
        );
    }
}
