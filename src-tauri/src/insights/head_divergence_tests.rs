//! The snapshot's per-HEAD divergence read is cached by the immutable
//! `(head, base)` pair: a steady-state snapshot spawns no `rev-list`, one moved
//! HEAD costs exactly one, and a cold cache past the per-pass cap is reported
//! as unmeasured, never as `ahead: 0, behind: 0`.

use super::*;
use crate::test_support::{git_in, repo_with_worktrees, write};
use std::path::PathBuf;

/// The per-HEAD fallback read: `rev-list --left-right --count <oid>...<oid>`.
fn is_head_read(argv: &[String]) -> bool {
    argv.iter().any(|arg| arg == "rev-list")
        && argv.iter().any(|arg| arg == "--left-right")
        && argv.iter().any(|arg| arg == "--count")
}

/// The batched branch divergence read: `for-each-ref --format=...ahead-behind:...`.
fn is_batched_head_read(argv: &[String]) -> bool {
    argv.iter().any(|arg| arg == "for-each-ref")
        && argv.iter().any(|arg| arg.contains("ahead-behind:"))
}

/// How many per-HEAD `rev-list` reads have been spawned from `repo` so far.
fn head_reads(repo: &std::path::Path) -> usize {
    crate::engine::process::spawn_log::spawns_in(repo)
        .iter()
        .filter(|argv| is_head_read(argv))
        .count()
}

/// How many batched `for-each-ref` divergence reads have been spawned from `repo`.
fn batched_reads(repo: &std::path::Path) -> usize {
    crate::engine::process::spawn_log::spawns_in(repo)
        .iter()
        .filter(|argv| is_batched_head_read(argv))
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
    let after_cold_rl = head_reads(&root);
    let after_cold_batch = batched_reads(&root);
    // Main is on the base commit (0 reads); the two branch lanes resolve
    // together in one batched `for-each-ref` and zero `rev-list` spawns.
    assert_eq!(
        after_cold_batch, 1,
        "a cold snapshot resolves all branch HEADs in one batched for-each-ref"
    );
    assert_eq!(
        after_cold_rl, 0,
        "branch HEADs should not spawn per-HEAD rev-list"
    );
    assert_eq!(row(&cold, &linked[0]).ahead, Some(1));
    assert_eq!(row(&cold, &linked[1]).ahead, Some(1));
    assert_eq!(row(&cold, &root).ahead, Some(0));

    let warm = snapshot(repo);

    assert_eq!(
        head_reads(&root),
        after_cold_rl,
        "a steady-state snapshot ran rev-list"
    );
    assert_eq!(
        batched_reads(&root),
        after_cold_batch,
        "a steady-state snapshot ran for-each-ref"
    );
    assert_eq!(row(&warm, &linked[0]).ahead, Some(1), "{warm:?}");
    assert_eq!(row(&warm, &linked[1]).behind, Some(0), "{warm:?}");
}

#[test]
fn many_branch_worktrees_measure_divergence_in_one_batched_spawn() {
    let (_main, _parent, root, linked) = lanes_with_work(12);
    let repo = root.to_str().unwrap();

    let cold = snapshot(repo);
    assert_eq!(
        batched_reads(&root),
        1,
        "one batched for-each-ref for all 12 branches"
    );
    assert_eq!(
        head_reads(&root),
        0,
        "no per-HEAD rev-list for branch worktrees"
    );
    for lane in &linked {
        let item = row(&cold, lane);
        assert_eq!(item.ahead, Some(1), "{item:?}");
        assert_eq!(item.behind, Some(0), "{item:?}");
        assert_eq!(item.divergence_note, None, "{item:?}");
    }
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
    let (_main, _parent, root, linked) = lanes_with_work(4);
    // Detach each lane onto its own commit and drop its branch so none of the
    // four distinct detached HEADs match `refs/heads` or the base commit.
    let mut detached_heads = Vec::new();
    for (n, lane) in linked.iter().enumerate() {
        git_in(lane, &["checkout", "-q", "--detach", "HEAD"]);
        git_in(&root, &["branch", "-D", &format!("agent/{n:02}")]);
        let oid = crate::engine::git_cli::git_text(lane, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();
        detached_heads.push(oid);
    }
    let heads: Vec<&str> = detached_heads.iter().map(String::as_str).collect();
    assert_eq!(heads.len(), 4);
    let clock = worktree::ScanDeadline::after(Instant::now(), Duration::from_secs(120));

    let measured = worktree::measure_head_divergence_capped(&root, &heads, &clock, 2);

    assert_eq!(head_reads(&root), 2, "the cap is on rev-list spawns");
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
fn a_shallow_clones_counts_say_they_are_bounded_by_the_depth() {
    let (main, _parent, _linked) = repo_with_worktrees(1);
    let src = main.path().canonicalize().unwrap();
    commit_in(&src, "m1.txt");
    commit_in(&src, "m2.txt");
    let holder = tempfile::tempdir().unwrap();
    let dst = holder.path().join("shallow");
    let url = format!("file://{}", src.display());
    git_in(
        holder.path(),
        &["clone", "-q", "--depth", "1", &url, dst.to_str().unwrap()],
    );
    crate::test_support::trust_repo(&dst);
    let dst = dst.canonicalize().unwrap();
    let mut rows: Vec<WorktreeSummary> = worktree::list_worktrees_lite(dst.to_str().unwrap())
        .unwrap()
        .iter()
        .map(|info| summarise_worktree(info, String::new(), false))
        .collect();
    let clock = worktree::ScanDeadline::after(Instant::now(), Duration::from_secs(60));

    fill_head_divergence(dst.to_str().unwrap(), &mut rows, &clock);

    assert!(rows[0].ahead.is_some(), "{:?}", rows[0]);
    assert_eq!(
        rows[0].divergence_note.as_deref(),
        Some("shallow: counts are bounded by the clone depth")
    );
    // A whole-history repository carries no note.
    let whole = snapshot(src.to_str().unwrap());
    assert!(whole
        .worktrees
        .items
        .iter()
        .all(|w| w.divergence_note.is_none()));
}

#[test]
fn a_moved_base_remeasures_every_head() {
    let (_main, _parent, root, linked) = lanes_with_work(2);
    let repo = root.to_str().unwrap();
    snapshot(repo);
    let before_batch = batched_reads(&root);
    let before_rl = head_reads(&root);

    // The default base advances: every cached pair names the old base.
    commit_in(&root, "base.txt");
    let moved = snapshot(repo);

    assert!(
        (batched_reads(&root) - before_batch) + (head_reads(&root) - before_rl) >= 1,
        "the cache served a stale base"
    );
    assert_eq!(row(&moved, &linked[0]).behind, Some(1), "{moved:?}");
    assert_eq!(row(&moved, &linked[1]).behind, Some(1), "{moved:?}");
}
