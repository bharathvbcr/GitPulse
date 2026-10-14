//! Teardown of a worktree holding 100k ignored files: how long the repository
//! mutation lock is held, and what a second mutation waits meanwhile.
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench worktree_teardown`.
//!
//! Merge-and-teardown and the Worktrees panel's delete both remove the worktree
//! directory while holding `repo_mutation_lock`, which every other mutation of
//! the repository takes too (`lock_worktree`, `add_worktree`, the git writer).
//! The recursive delete is the one step whose cost scales with the number of
//! entries, so this bench builds the worst realistic lane (a `build/` tree) and
//! times it. Listing never takes the lock, so only mutations can wait.
//!
//! * the fixture, rebuilt for every sample outside the timed span: a repository
//!   whose committed `.gitignore` names `build/`; a linked worktree with one
//!   commit and 100 000 ignored files (1000 directories x 100 files x 512 B).
//!   The files are ignored rather than untracked because the merge refuses a
//!   worktree with untracked files. They are 100 hard links of one 512 B file per directory, so
//!   the fixture does not write 400 MB per sample (see `make_lane`);
//! * `teardown remove 100k (lock-held)`: the public `remove_worktree`, which
//!   takes the lock and runs the removal under it. The row's time is how long
//!   it holds the lock;
//! * `merge_and_teardown 100k (total)` and `... 0 entries (control)`: the
//!   whole merge and removal, with and without the entries, so the entries'
//!   share is the difference;
//! * `contender_wait_ns` (a metric, never gated): once teardown has started,
//!   a second thread calls `lock_worktree` on another worktree and records how
//!   long it waited for the lock.

mod common;

use common::fixture::{build_repo, count_git_processes, git, Repo, Spec};
use common::{interleave, Case, Report, Sample};
use gitpulse_lib::engine::worktree::{
    add_worktree, lock_worktree, merge_and_teardown_worktree, remove_worktree, unlock_worktree,
    MergeTeardownResult,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SPEC: Spec = Spec {
    files: 20,
    dirs: 4,
    extra_commits: 2,
    touch: 2,
    linked_worktrees: 0,
};

const DIRS: usize = 1000;
const PER_DIR: usize = 100;
const ENTRIES: usize = DIRS * PER_DIR;

fn path_text(path: &Path) -> &str {
    path.to_str().expect("fixture path is UTF-8")
}

/// A fresh worktree on its own branch with one commit and `dirs` x `PER_DIR`
/// ignored files under `build/`.
fn make_lane(repo: &Repo, root: &Path, name: &str, dirs: usize) -> PathBuf {
    let lane = root.join(name);
    add_worktree(
        repo.path(),
        path_text(&lane),
        Some(&format!("bench/{name}")),
        None,
        false,
    )
    .unwrap_or_else(|error| panic!("add_worktree {name} failed: {error}"));
    std::fs::write(lane.join(format!("{name}.txt")), name).expect("write lane file");
    git(&lane, &["add", "-A"]);
    git(&lane, &["commit", "-q", "-m", &format!("feat: {name}")]);
    // Every entry is a directory entry and an inode of its own, 512 B long,
    // but they share one data block: written afresh, 100k files would be about
    // 400 MB per sample, which trips the test harness's disk-write budget (a
    // runaway-writer guard) and is not what the delete's cost is made of.
    let build = lane.join("build");
    std::fs::create_dir_all(&build).expect("mkdir build");
    let block = vec![b'x'; 512];
    for d in 0..dirs {
        let dir = build.join(format!("d{d:04}"));
        std::fs::create_dir_all(&dir).expect("mkdir build dir");
        // One file per directory, linked PER_DIR - 1 times: a link count stays
        // far under the 65 000 a Linux ext4 inode allows.
        let first = dir.join("f000");
        std::fs::write(&first, &block).expect("write ignored file");
        for f in 1..PER_DIR {
            std::fs::hard_link(&first, dir.join(format!("f{f:03}"))).expect("link ignored file");
        }
    }
    lane
}

fn merge(repo: &Repo, lane: &Path) -> MergeTeardownResult {
    let result =
        merge_and_teardown_worktree(repo.path(), path_text(lane), None, false, None, &mut |_| {
            Ok(())
        })
        .unwrap_or_else(|error| panic!("merge_and_teardown failed: {error:?}"));
    assert!(
        result.merged && result.worktree_removed && result.branch_deleted,
        "teardown degraded ({:?} / {:?}), so this run would time a partial teardown",
        result.removal_error,
        result.branch_error
    );
    assert!(!lane.exists(), "the lane directory is still there");
    result
}

fn main() {
    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
    let mut report = Report::new("worktree_teardown");
    let temp = tempfile::tempdir().expect("temp dir");
    let root = temp.path().join("fixture");
    let repo = build_repo(&root, &SPEC);
    std::fs::write(repo.main.join(".gitignore"), "build/\n").expect("write .gitignore");
    git(&repo.main, &["add", ".gitignore"]);
    git(&repo.main, &["commit", "-q", "-m", "chore: ignore build"]);
    report.note(format!(
        "fixture: {ENTRIES} ignored files ({DIRS} dirs x {PER_DIR} x 512 B) in the removed worktree, hermetic HOME, production spawn rate lifted"
    ));

    // A second worktree the contender locks and unlocks.
    let other = root.join("other");
    add_worktree(
        repo.path(),
        path_text(&other),
        Some("bench/other"),
        None,
        false,
    )
    .expect("add the contender's worktree");

    let serial = std::cell::Cell::new(0usize);
    let next = |prefix: &str| {
        serial.set(serial.get() + 1);
        format!("{prefix}{}", serial.get())
    };

    // Git-process counters, each from an untimed pass over a small lane.
    let (_, remove_procs) = count_git_processes(|| {
        let lane = make_lane(&repo, &root, &next("count-rm"), 1);
        remove_worktree(repo.path(), path_text(&lane), true).expect("remove");
    });
    let (_, merge_procs) = count_git_processes(|| {
        let lane = make_lane(&repo, &root, &next("count-mg"), 1);
        merge(&repo, &lane);
    });

    let remove_100k = || {
        let lane = make_lane(&repo, &root, &next("rm"), DIRS);
        let started_flag = Arc::new(AtomicBool::new(false));
        let contender = {
            let flag = Arc::clone(&started_flag);
            let repo_path = repo.path().to_string();
            let other = path_text(&other).to_string();
            std::thread::spawn(move || {
                while !flag.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_micros(200));
                }
                // Let the teardown get past its own validation and into the delete.
                std::thread::sleep(Duration::from_millis(2));
                let asked = Instant::now();
                lock_worktree(&repo_path, &other, Some("bench")).expect("contender lock");
                let waited = asked.elapsed();
                unlock_worktree(&repo_path, &other).expect("contender unlock");
                waited
            })
        };
        started_flag.store(true, Ordering::Release);
        let started = Instant::now();
        remove_worktree(repo.path(), path_text(&lane), true).expect("remove_worktree");
        let elapsed = started.elapsed();
        let waited = contender.join().expect("contender thread");
        assert!(!lane.exists(), "the lane directory is still there");
        Sample::default()
            .counter("entries", ENTRIES as u64)
            .counter("git_processes", remove_procs)
            .counter("worktree_removed", 1)
            .elapsed(elapsed)
            .metric("contender_wait_ns", waited.as_nanos() as f64)
    };

    let merge_row = |dirs: usize| {
        let lane = make_lane(&repo, &root, &next("mg"), dirs);
        let started = Instant::now();
        let result = merge(&repo, &lane);
        let elapsed = started.elapsed();
        Sample::default()
            .counter("entries", (dirs * PER_DIR) as u64)
            .counter("git_processes", merge_procs)
            .counter("worktree_removed", u64::from(result.worktree_removed))
            .counter("branch_deleted", u64::from(result.branch_deleted))
            .elapsed(elapsed)
    };

    let cases = vec![
        Case::new("teardown remove 100k (lock-held)", remove_100k),
        Case::new("merge_and_teardown 100k (total)", || merge_row(DIRS)),
        Case::new("merge_and_teardown 0 entries (control)", || merge_row(0)),
    ];
    let rows = interleave(cases, report.opts.samples(5), report.opts.warmup(1));
    report.extend(rows);
    report.finish();
}
