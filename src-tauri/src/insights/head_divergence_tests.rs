//! The snapshot's per-HEAD divergence read is cached by the immutable
//! `(head, base)` pair: a steady-state snapshot spawns no `rev-list`, one moved
//! HEAD costs exactly one, and a cold cache past the per-pass cap is reported
//! as unmeasured, never as `ahead: 0, behind: 0`.

use super::*;
use crate::test_support::{git_in, repo_with_worktrees, write};
use std::path::PathBuf;

/// The per-HEAD read: `rev-list --left-right --count <oid>...<oid>`.
fn is_head_read(argv: &[String]) -> bool {
    argv.iter().any(|arg| arg == "rev-list")
        && argv.iter().any(|arg| arg == "--left-right")
        && argv.iter().any(|arg| arg == "--count")
}

/// How many head reads have been spawned from `repo` so far (cumulative).
fn head_reads(repo: &std::path::Path) -> usize {
    crate::engine::git_cli::spawn_log::spawns_in(repo)
        .iter()
        .filter(|argv| is_head_read(argv))
        .count()
}

fn row<'a>(snap: &'a InsightsSnapshot, dir: &std::path::Path) -> &'a WorktreeSummary {
    snap.worktrees
        .items
        .iter()
        .find(|w| same_path(&w.path, dir.to_str().unwrap()))
        .expect("the worktree is listed")
}

fn commit_in(dir: &std::path::Path, file: &str) {
    write(dir, file, "work\n");
    git_in(dir, &["add", "-A"]);
    git_in(dir, &["commit", "-q", "-m", file]);
}

fn lanes_with_work(count: usize) -> (tempfile::TempDir, tempfile::TempDir, PathBuf, Vec<PathBuf>) {
    let (main, parent, linked) = repo_with_worktrees(count);
    let root = main.path().canonicalize().unwrap();
    for (n, lane) in linked.iter().enumerate() {
        commit_in(lane, &format!("lane{n}.txt"));
    }
    (main, parent, root, linked)
}

#[test]
fn a_snapshot_with_no_head_and_no_base_moved_spawns_no_rev_list() {
    let (_main, _parent, root, linked) = lanes_with_work(2);
    let repo = root.to_str().unwrap();

    let cold = snapshot(repo);
    let after_cold = head_reads(&root);
    // The two lanes are on two commits and the main checkout on a third.
    assert_eq!(
        after_cold, 3,
        "a cold snapshot reads each distinct HEAD once"
    );
    assert_eq!(row(&cold, &linked[0]).ahead, Some(1));

    let warm = snapshot(repo);

    assert_eq!(
        head_reads(&root),
        after_cold,
        "a steady-state snapshot ran rev-list"
    );
    assert_eq!(row(&warm, &linked[0]).ahead, Some(1), "{warm:?}");
    assert_eq!(row(&warm, &linked[1]).behind, Some(0), "{warm:?}");
}

#[test]
fn one_moved_head_costs_exactly_one_rev_list() {
    let (_main, _parent, root, linked) = lanes_with_work(2);
    let repo = root.to_str().unwrap();
    snapshot(repo);
    let before = head_reads(&root);

    commit_in(&linked[0], "more.txt");
    let moved = snapshot(repo);

    assert_eq!(head_reads(&root) - before, 1, "one HEAD moved");
    assert_eq!(row(&moved, &linked[0]).ahead, Some(2), "{moved:?}");
    assert_eq!(row(&moved, &linked[1]).ahead, Some(1));
}

#[test]
fn a_cold_cache_past_the_per_pass_cap_is_unmeasured_never_zero() {
    let (_main, _parent, root, linked) = lanes_with_work(3);
    // Four distinct HEADs (three lanes and the main checkout), a cap of two.
    let listing = worktree::list_worktrees_lite(root.to_str().unwrap()).unwrap();
    let heads: Vec<&str> = listing.iter().map(|info| info.head.as_str()).collect();
    assert_eq!(heads.len(), 4);
    let clock = worktree::ScanDeadline::after(Instant::now(), Duration::from_secs(120));

    let measured = worktree::measure_head_divergence_capped(&root, &heads, &clock, 2);

    assert_eq!(head_reads(&root), 2, "the cap is on spawns");
    assert_eq!(measured.by_head.len(), 2, "{measured:?}");
    let reason = measured.unread.as_deref().expect("a reason");
    assert!(reason.starts_with("unmeasured:"), "{reason}");
    assert!(reason.contains("2 of 4"), "{reason}");
    // The two it did not reach have no entry at all: nothing to read as 0/0.
    let missing: Vec<&&str> = heads
        .iter()
        .filter(|head| !measured.by_head.contains_key(**head))
        .collect();
    assert_eq!(missing.len(), 2);

    // A second pass with the same cap reads two more: the cache served the
    // first two, so the cap went to HEADs that had none.
    let again = worktree::measure_head_divergence_capped(&root, &heads, &clock, 2);
    assert_eq!(head_reads(&root), 4);
    assert_eq!(again.by_head.len(), 4, "{again:?}");
    assert_eq!(again.unread, None);
    let _ = linked;
}

#[test]
fn a_miss_after_the_deadline_is_unmeasured_and_a_hit_still_answers() {
    let (_main, _parent, root, _linked) = lanes_with_work(2);
    let repo = root.to_str().unwrap();
    snapshot(repo);
    let listing = worktree::list_worktrees_lite(repo).unwrap();
    let heads: Vec<&str> = listing.iter().map(|info| info.head.as_str()).collect();
    let before = head_reads(&root);
    let passed = worktree::ScanDeadline::after(Instant::now(), Duration::ZERO);

    let measured = worktree::measure_head_divergence_capped(&root, &heads, &passed, 8);

    assert_eq!(head_reads(&root), before, "an expired deadline spawned");
    assert_eq!(measured.by_head.len(), heads.len(), "hits cost nothing");
}

#[test]
fn a_moved_base_remeasures_every_head() {
    let (_main, _parent, root, linked) = lanes_with_work(2);
    let repo = root.to_str().unwrap();
    snapshot(repo);
    let before = head_reads(&root);

    // The default base advances: every cached pair names the old base.
    commit_in(&root, "base.txt");
    let moved = snapshot(repo);

    assert!(
        head_reads(&root) - before >= 2,
        "the cache served a stale base"
    );
    assert_eq!(row(&moved, &linked[0]).behind, Some(1), "{moved:?}");
}
