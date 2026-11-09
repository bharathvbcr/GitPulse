//! Break tests for lane W3-L2. Each FAILS on the lane head; see the message of
//! each for the scenario and the expected behaviour.

use super::adv_w3gb_tests::{commit_file, git, init, plan_spawns, release_ready_repo, stash_repo};
use crate::engine::stash::{run_action_with, StashAction};
use crate::ops::{branch_cleanup_plan, prepare_release};

/// A tag at a tree (or blob) cannot be peeled to a commit. `rev-parse --verify
/// --quiet <tag>^{commit}` exits 1 with empty stdout for that too, so the
/// classification reads "missing" and `create_tag=true` is reported for a tag
/// that exists (the later `git tag` then fails). The doc comment promises every
/// outcome other than "missing" is an Err.
#[test]
fn a_tag_that_exists_but_is_no_commit_is_an_error_not_missing() {
    let (_remote, repo) = release_ready_repo();
    git(repo.path(), &["tag", "v1.5.0", "HEAD^{tree}"]);
    let result = prepare_release(repo.path().to_str().unwrap(), "v1.5.0", "Release");
    match result {
        Err(_) => {}
        Ok(plan) => panic!(
            "tag v1.5.0 exists (a tree); plan says create_tag={} instead of refusing",
            plan.create_tag
        ),
    }
}

#[test]
fn a_tag_at_a_blob_is_an_error_not_missing() {
    let (_remote, repo) = release_ready_repo();
    let blob = git(repo.path(), &["rev-parse", "HEAD:app.txt"]);
    git(repo.path(), &["tag", "v1.5.0", &blob]);
    let result = prepare_release(repo.path().to_str().unwrap(), "v1.5.0", "Release");
    assert!(result.is_err(), "blob tag treated as missing: {result:?}");
}

/// `git stash pop stash@{0}` applies whatever sits at index 0 BEFORE the oid is
/// compared. After a raced pop the lane restores the entry to the stack, but the
/// intruder's changes are already in the working tree. The call fails, so the
/// working tree must be as it was.
#[test]
fn a_raced_pop_leaves_the_working_tree_untouched() {
    let dir = stash_repo();
    let path = dir.path();
    std::fs::write(path.join("a.txt"), "confirmed").unwrap();
    git(path, &["stash", "push", "-m", "confirmed work"]);
    let confirmed = git(path, &["rev-parse", "refs/stash"]);
    let error = run_action_with(
        path.to_str().unwrap(),
        StashAction::Pop,
        0,
        &confirmed,
        |_| {
            std::fs::write(path.join("a.txt"), "intruder").unwrap();
            git(path, &["stash", "push", "-m", "outside work"]);
            Ok(())
        },
    )
    .unwrap_err();
    assert!(error.message().contains("applied"), "{}", error.message());
    let on_disk = std::fs::read_to_string(path.join("a.txt")).unwrap();
    let status = git(path, &["status", "--porcelain"]);
    assert_eq!(
        (on_disk.as_str(), status.as_str()),
        ("base", ""),
        "the failed pop left the foreign entry's changes in the working tree"
    );
}

/// 200 unmerged branches: the cleanup plan (and so `storage::branch_summary`,
/// which calls it) used to cost one `git branch --merged`. It now runs one
/// quarantined `merge-tree` per unmerged branch with no cap.
#[test]
fn the_cleanup_plan_does_not_fan_out_one_merge_tree_per_branch() {
    let (total, merge_trees, n) = plan_spawns(200);
    assert!(
        merge_trees <= 50,
        "{merge_trees} merge-tree spawns ({total} spawns total) for {n} branches: unbounded fan-out"
    );
}

/// A tree comparison that cannot run leaves the branch unmerged with no trace:
/// the plan type has no field that says "N branches were not checked".
#[test]
fn a_skipped_tree_check_is_visible_in_the_plan() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    init(p);
    commit_file(p, "base.txt", "base", "feat: initial");
    git(p, &["switch", "-q", "-c", "sq"]);
    commit_file(p, "s.txt", "s", "feat: s");
    git(p, &["switch", "-q", "main"]);
    git(p, &["merge", "--squash", "sq"]);
    git(p, &["commit", "-m", "feat: s (#1)"]);
    // Make this a partial clone so every tree comparison is refused.
    git(p, &["config", "extensions.partialClone", "origin"]);
    git(p, &["config", "remote.origin.promisor", "true"]);
    git(p, &["config", "remote.origin.url", "./nowhere.git"]);
    let plan = branch_cleanup_plan(p.to_str().unwrap()).unwrap();
    let json = serde_json::to_string(&plan).unwrap().to_lowercase();
    assert!(
        plan.candidates.is_empty()
            && (json.contains("unknown") || json.contains("warn") || json.contains("unchecked")),
        "squash-merged `sq` is silently reported as plain unmerged: {json}"
    );
}
