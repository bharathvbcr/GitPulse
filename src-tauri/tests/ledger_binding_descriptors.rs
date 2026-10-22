//! Binding resolution and consolidation must not hold descriptors.
//!
//! A terminal session resolves the worktree's binding at spawn, so a handle kept
//! per call is a leak per session. This runs in its own process (an integration
//! test), where the descriptor count is the measurement and no other test's files
//! move it.
#![cfg(unix)]

mod common;

use gitpulse_lib::ledger::bindings::{bind, repository_address, resolve};
use std::time::Duration;

/// Open file descriptors of this process (macOS and Linux both list them here).
fn open_descriptors() -> usize {
    std::fs::read_dir("/dev/fd")
        .map(|entries| entries.count())
        .unwrap_or(0)
}

/// The lowest reading over a short wait: a descriptor still being closed is not
/// a leak, one that stays is.
fn settled(target: usize) -> usize {
    let mut lowest = usize::MAX;
    for _ in 0..40 {
        lowest = lowest.min(open_descriptors());
        if lowest <= target {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    lowest
}

/// Repeated, concurrent resolve and consolidate calls over a family with a
/// ledger leave the descriptor count where it started. A hundred concurrent
/// terminal sessions make exactly these calls: a read-only connection opened per
/// call used to leave one descriptor behind per concurrent open.
#[test]
fn resolving_and_consolidating_concurrently_leaves_no_descriptor_behind() {
    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
    let dir = tempfile::tempdir().expect("temp repository");
    let main = dir.path().canonicalize().expect("canonical repository");
    common::run_git(&main, &["init", "-q", "-b", "main"]);
    std::fs::write(main.join("seed.txt"), "seed").expect("seed");
    common::run_git(&main, &["add", "seed.txt"]);
    common::run_git(&main, &["commit", "-q", "-m", "seed"]);
    common::trust_repo(&main);
    let parent = tempfile::tempdir().expect("worktree parent");
    let linked = parent.path().join("wt");
    common::run_git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
        ],
    );
    common::trust_repo(&linked);
    let main = main.to_string_lossy().into_owned();
    let linked = linked
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    bind(&main, &linked, "TASK-A").expect("bind creates the anchor");
    // Process-wide, lazily created state is paid for before the baseline.
    resolve(&main, &linked).expect("warm resolve");
    repository_address(&main).expect("warm consolidation");
    let before = settled(0);

    std::thread::scope(|scope| {
        for _ in 0..16 {
            scope.spawn(|| {
                for _ in 0..25 {
                    resolve(&main, &linked).expect("resolve");
                    repository_address(&main).expect("consolidate");
                }
            });
        }
    });

    let after = settled(before);
    assert!(
        after <= before + 1,
        "descriptors {before} -> {after} after 800 resolve/consolidate calls"
    );
}
