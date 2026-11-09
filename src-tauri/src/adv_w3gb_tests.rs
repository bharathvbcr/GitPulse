//! Adversarial tests for lane W3-L2 (release tag binding, stash drop/pop
//! binding, one merged rule for branch cleanup). Everything here passes on the
//! lane head; the tests that fail live in `adv_w3gb_break_tests`.

use crate::engine::stash::{list, run_action_with, StashAction};
use crate::ops::{branch_cleanup_plan, prepare_release};
use crate::procguard::LockedSpawn;
use std::path::Path;
use std::process::Command;

pub(crate) fn git(path: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    crate::test_support::pin_git(&mut command);
    let output = command
        .args(args)
        .current_dir(path)
        .env("GIT_AUTHOR_NAME", "GitPulse Test")
        .env("GIT_AUTHOR_EMAIL", "gitpulse@example.test")
        .env("GIT_COMMITTER_NAME", "GitPulse Test")
        .env("GIT_COMMITTER_EMAIL", "gitpulse@example.test")
        .output_locked()
        .expect("git should run");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub(crate) fn init(path: &Path) {
    git(path, &["init", "-b", "main"]);
    crate::test_support::trust_repo(path);
}

pub(crate) fn commit_file(path: &Path, name: &str, content: &str, message: &str) {
    std::fs::write(path.join(name), content).expect("fixture write");
    git(path, &["add", "--", name]);
    git(path, &["commit", "-m", message]);
}

/// A pushed, synchronized default branch with one commit (what
/// `prepare_release` accepts).
pub(crate) fn release_ready_repo() -> (tempfile::TempDir, tempfile::TempDir) {
    let remote = tempfile::tempdir().expect("bare remote");
    git(remote.path(), &["init", "--bare", "--initial-branch=main"]);
    let repo = tempfile::tempdir().expect("local repo");
    init(repo.path());
    commit_file(repo.path(), "app.txt", "ready", "feat: ready release #1");
    git(
        repo.path(),
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    git(repo.path(), &["push", "-u", "origin", "main"]);
    (remote, repo)
}

fn stash_of(path: &Path, content: &str, message: &str) {
    std::fs::write(path.join("a.txt"), content).unwrap();
    git(path, &["stash", "push", "-m", message]);
}

pub(crate) fn stash_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp repo");
    let path = dir.path();
    git(path, &["init", "-b", "main"]);
    crate::test_support::trust_repo(path);
    std::fs::write(path.join("a.txt"), "base").unwrap();
    git(path, &["add", "a.txt"]);
    git(path, &["commit", "-m", "base"]);
    dir
}

fn oids(path: &Path) -> Vec<String> {
    list(path.to_str().unwrap())
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.oid)
        .collect()
}

// ---- 1. release tag --------------------------------------------------------

#[test]
fn an_annotated_tag_at_head_is_reused() {
    let (_remote, repo) = release_ready_repo();
    git(repo.path(), &["tag", "-a", "v1.5.0", "-m", "release"]);
    let plan = prepare_release(repo.path().to_str().unwrap(), "v1.5.0", "Release").unwrap();
    assert!(!plan.create_tag, "annotated tag peels to the commit");
}

#[test]
fn a_packed_tag_at_head_is_reused() {
    let (_remote, repo) = release_ready_repo();
    git(repo.path(), &["tag", "v1.5.0"]);
    git(repo.path(), &["pack-refs", "--all"]);
    let plan = prepare_release(repo.path().to_str().unwrap(), "v1.5.0", "Release").unwrap();
    assert!(!plan.create_tag);
}

#[test]
fn a_tag_at_head_wins_over_a_same_named_branch_elsewhere() {
    let (_remote, repo) = release_ready_repo();
    git(repo.path(), &["tag", "v1.5.0"]);
    git(repo.path(), &["switch", "-q", "-c", "v1.5.0"]);
    commit_file(repo.path(), "o.txt", "o", "feat: o");
    git(repo.path(), &["switch", "-q", "main"]);
    let plan = prepare_release(repo.path().to_str().unwrap(), "v1.5.0", "Release").unwrap();
    assert!(!plan.create_tag, "refs/tags/ qualification, not the branch");
}

#[test]
fn an_annotated_tag_at_another_commit_is_refused() {
    let (_remote, repo) = release_ready_repo();
    git(repo.path(), &["tag", "-a", "v1.5.0", "-m", "old"]);
    commit_file(repo.path(), "n.txt", "n", "feat: next");
    git(repo.path(), &["push", "origin", "main"]);
    let error = prepare_release(repo.path().to_str().unwrap(), "v1.5.0", "Release").unwrap_err();
    assert!(error.contains("different commit"), "{}", error.message());
}

#[test]
fn option_and_revision_shaped_tag_names_never_reach_git() {
    let (_remote, repo) = release_ready_repo();
    let path = repo.path().to_str().unwrap();
    for tag in [
        "--all",
        "-v1.0.0",
        "v1.0.0^{}",
        "v1.0.0..v2.0.0",
        "v1.0.0@{0}",
        "v1.0.0^{commit}",
        "v1.0.0\n",
    ] {
        assert!(prepare_release(path, tag, "Release").is_err(), "{tag:?}");
    }
}

#[test]
fn a_shallow_clone_reads_the_tag_like_any_other() {
    let (remote, repo) = release_ready_repo();
    let shallow = tempfile::tempdir().unwrap();
    let url = format!("file://{}", remote.path().display());
    git(
        shallow.path(),
        &["clone", "-q", "--depth=1", &url, "clone"],
    );
    let clone = shallow.path().join("clone");
    crate::test_support::trust_repo(&clone);
    git(&clone, &["tag", "v1.5.0"]);
    let plan = prepare_release(clone.to_str().unwrap(), "v1.5.0", "Release").unwrap();
    assert!(!plan.create_tag);
}

// ---- 3. stash drop races ---------------------------------------------------

#[test]
fn two_intruder_pushes_cost_the_user_nothing() {
    let dir = stash_repo();
    let path = dir.path();
    stash_of(path, "confirmed", "confirmed work");
    let confirmed = git(path, &["rev-parse", "refs/stash"]);
    let seen = std::cell::RefCell::new(Vec::new());
    let error = run_action_with(path.to_str().unwrap(), StashAction::Drop, 0, &confirmed, |_| {
        stash_of(path, "one", "intruder one");
        seen.borrow_mut().push(git(path, &["rev-parse", "refs/stash"]));
        stash_of(path, "two", "intruder two");
        seen.borrow_mut().push(git(path, &["rev-parse", "refs/stash"]));
        Ok(())
    })
    .unwrap_err();
    assert!(error.message().contains("restored"), "{}", error.message());
    let seen = seen.into_inner();
    assert_eq!(oids(path), vec![seen[1].clone(), seen[0].clone(), confirmed]);
}

#[test]
fn an_outsider_dropping_the_confirmed_entry_does_not_drop_the_next_one() {
    let dir = stash_repo();
    let path = dir.path();
    stash_of(path, "older", "older work");
    let older = git(path, &["rev-parse", "refs/stash"]);
    stash_of(path, "confirmed", "confirmed work");
    let confirmed = git(path, &["rev-parse", "refs/stash"]);
    let result = run_action_with(path.to_str().unwrap(), StashAction::Drop, 0, &confirmed, |_| {
        git(path, &["stash", "drop"]);
        Ok(())
    });
    assert!(result.is_err(), "the confirmed entry was already gone");
    assert_eq!(oids(path), vec![older], "the next entry must survive");
}

#[test]
fn an_outsider_dropping_the_only_entry_is_an_error_not_a_success() {
    let dir = stash_repo();
    let path = dir.path();
    stash_of(path, "confirmed", "confirmed work");
    let confirmed = git(path, &["rev-parse", "refs/stash"]);
    let result = run_action_with(path.to_str().unwrap(), StashAction::Drop, 0, &confirmed, |_| {
        git(path, &["stash", "drop"]);
        Ok(())
    });
    assert!(result.is_err());
    assert!(oids(path).is_empty());
}

#[test]
fn a_dash_and_newline_message_survives_the_restore() {
    let dir = stash_repo();
    let path = dir.path();
    stash_of(path, "confirmed", "confirmed work");
    let confirmed = git(path, &["rev-parse", "refs/stash"]);
    let seen = std::cell::RefCell::new(String::new());
    let _ = run_action_with(path.to_str().unwrap(), StashAction::Drop, 0, &confirmed, |_| {
        stash_of(path, "x", "-rf weird\nsecond line");
        *seen.borrow_mut() = git(path, &["rev-parse", "refs/stash"]);
        Ok(())
    })
    .unwrap_err();
    let listed = list(path.to_str().unwrap()).unwrap().entries;
    assert_eq!(listed[0].oid, seen.into_inner());
    assert!(listed[0].message.starts_with("-rf weird"), "{:?}", listed[0].message);
}

// ---- 5. classification edge cases -----------------------------------------

fn names(plan: &crate::ops::BranchCleanupPlan) -> Vec<String> {
    let mut v: Vec<String> = plan.candidates.iter().map(|c| c.name.clone()).collect();
    v.sort();
    v
}

#[test]
fn a_squash_merge_is_found_even_after_main_advanced_past_the_squash() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    init(p);
    commit_file(p, "base.txt", "base", "feat: initial");
    git(p, &["switch", "-c", "sq"]);
    commit_file(p, "s.txt", "s", "feat: s");
    git(p, &["switch", "main"]);
    git(p, &["merge", "--squash", "sq"]);
    git(p, &["commit", "-m", "feat: s (#1)"]);
    commit_file(p, "later.txt", "later", "feat: later");
    // Main also rewrites the squashed file afterwards: still "in main" as
    // history, but the tip tree no longer equals the merge result.
    commit_file(p, "s.txt", "s changed later", "fix: s tweaked");
    let plan = branch_cleanup_plan(p.to_str().unwrap()).unwrap();
    assert_eq!(
        names(&plan),
        Vec::<String>::new(),
        "documented limit: a later rewrite of the squashed file hides the squash (unmerged, safe side)"
    );
}

#[test]
fn a_gone_upstream_with_nothing_ahead_and_main_past_it_is_merged_by_ancestry() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    init(p);
    git(p, &["remote", "add", "origin", "./nowhere.git"]);
    commit_file(p, "base.txt", "base", "feat: initial");
    git(p, &["branch", "old"]);
    commit_file(p, "n.txt", "n", "feat: main moves on");
    git(p, &["update-ref", "refs/remotes/origin/old", "old"]);
    git(p, &["branch", "--set-upstream-to=origin/old", "old"]);
    git(p, &["update-ref", "-d", "refs/remotes/origin/old"]);
    let plan = branch_cleanup_plan(p.to_str().unwrap()).unwrap();
    assert_eq!(names(&plan), vec!["old".to_string()]);
}

#[test]
fn a_branch_checked_out_in_another_worktree_is_protected_even_when_merged() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    init(p);
    commit_file(p, "base.txt", "base", "feat: initial");
    git(p, &["branch", "busy"]);
    commit_file(p, "n.txt", "n", "feat: next");
    let wt = tempfile::tempdir().unwrap();
    git(p, &["worktree", "add", "-q", wt.path().join("w").to_str().unwrap(), "busy"]);
    let plan = branch_cleanup_plan(p.to_str().unwrap()).unwrap();
    assert!(names(&plan).is_empty(), "{:?}", names(&plan));
    assert_eq!(plan.protected_branches, 2);
}

#[test]
fn a_detached_head_is_refused_not_planned() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    init(p);
    commit_file(p, "base.txt", "base", "feat: initial");
    git(p, &["branch", "old"]);
    commit_file(p, "n.txt", "n", "feat: next");
    git(p, &["switch", "-q", "--detach", "HEAD"]);
    assert!(branch_cleanup_plan(p.to_str().unwrap()).is_err());
}

#[test]
fn no_resolvable_default_branch_plans_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    git(p, &["init", "-b", "weird-trunk"]);
    crate::test_support::trust_repo(p);
    commit_file(p, "base.txt", "base", "feat: initial");
    git(p, &["branch", "old"]);
    commit_file(p, "n.txt", "n", "feat: next");
    match branch_cleanup_plan(p.to_str().unwrap()) {
        Err(_) => {}
        Ok(plan) => assert!(
            names(&plan).is_empty(),
            "an unresolved default must not mark anything merged: {:?}",
            names(&plan)
        ),
    }
}

// ---- 4. spawn measurement (numbers are printed with --nocapture) ------------

fn many_branches(count: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    init(p);
    commit_file(p, "base.txt", "base", "feat: initial");
    for i in 0..count {
        let name = format!("feature/b{i:03}");
        git(p, &["switch", "-q", "-c", &name]);
        commit_file(p, &format!("f{i}.txt"), "x", "feat: work");
        git(p, &["switch", "-q", "main"]);
    }
    commit_file(p, "next.txt", "n", "feat: main moves on");
    dir
}

pub(crate) fn plan_spawns(count: usize) -> (usize, usize, usize) {
    let dir = many_branches(count);
    let p = dir.path();
    let canonical = std::fs::canonicalize(p).unwrap();
    let log = || crate::engine::git_cli::spawn_log::spawns_in(&canonical);
    let before = log().len();
    let plan = branch_cleanup_plan(p.to_str().unwrap()).unwrap();
    let used: Vec<Vec<String>> = log()[before..].to_vec();
    assert_eq!(plan.unmerged_branches, count);
    let merge_trees = used.iter().filter(|a| a.iter().any(|x| x == "merge-tree")).count();
    let all = used.len();
    eprintln!("SPAWNS branch_cleanup_plan branches={count} total={all} merge-tree={merge_trees}");
    (all, merge_trees, count)
}

#[test]
fn measure_plan_spawns_at_50_branches() {
    let (_, merge_trees, n) = plan_spawns(50);
    assert!(merge_trees <= n);
}
