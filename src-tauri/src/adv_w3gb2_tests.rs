//! Round-2 adversarial tests for lane W3-L2 (pop by oid, tree-check budget). Test-only.

use crate::engine::deadbranch::{
    clean_branches_with_gate, tree_merge_checks, TreeMerge, MAX_TREE_CHECKS,
};
use crate::engine::git_cli::git_text;
use crate::engine::stash::{list, run_action_with, StashAction};
use crate::test_support::env::{bind_env_owned, env_serial};
use crate::test_support::{git_in, git_repo};
use std::path::Path;
use std::time::{Duration, Instant};

fn g(path: &Path, args: &[&str]) -> String {
    git_text(path, args).expect("git").trim().to_string()
}

fn base_repo() -> tempfile::TempDir {
    let dir = git_repo();
    std::fs::write(dir.path().join("a.txt"), "base").unwrap();
    git_in(dir.path(), &["add", "a.txt"]);
    git_in(dir.path(), &["commit", "-q", "-m", "base"]);
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

fn pop(path: &Path, oid: &str) -> Result<(), crate::ipc_error::CommandError> {
    run_action_with(path.to_str().unwrap(), StashAction::Pop, 0, oid, |_| Ok(())).map(|_| ())
}

#[test]
fn adv_pop_restores_untracked_files_stashed_with_u() {
    let dir = base_repo();
    let p = dir.path();
    std::fs::write(p.join("new.txt"), "untracked").unwrap();
    std::fs::write(p.join("a.txt"), "edit").unwrap();
    git_in(p, &["stash", "push", "-u", "-m", "u"]);
    let oid = g(p, &["rev-parse", "refs/stash"]);
    pop(p, &oid).expect("pop");
    assert_eq!(std::fs::read_to_string(p.join("new.txt")).unwrap(), "untracked");
    assert_eq!(std::fs::read_to_string(p.join("a.txt")).unwrap(), "edit");
    assert!(oids(p).is_empty());
}

#[test]
fn adv_pop_leaves_index_changes_unstaged_like_plain_pop() {
    let dir = base_repo();
    let p = dir.path();
    std::fs::write(p.join("a.txt"), "staged").unwrap();
    git_in(p, &["add", "a.txt"]);
    git_in(p, &["stash", "push", "-m", "s"]);
    let oid = g(p, &["rev-parse", "refs/stash"]);
    pop(p, &oid).expect("pop");
    assert_eq!(g(p, &["status", "--porcelain"]), "M a.txt");
}

#[test]
fn adv_pop_with_untracked_collision_reports_error_and_keeps_entry() {
    let dir = base_repo();
    let p = dir.path();
    std::fs::write(p.join("new.txt"), "stashed").unwrap();
    std::fs::write(p.join("a.txt"), "edit").unwrap();
    git_in(p, &["stash", "push", "-u", "-m", "u"]);
    let oid = g(p, &["rev-parse", "refs/stash"]);
    std::fs::write(p.join("new.txt"), "local").unwrap();
    let r = pop(p, &oid);
    assert!(r.is_err(), "colliding untracked file must fail the pop");
    assert_eq!(oids(p), vec![oid], "entry must survive");
    assert_eq!(std::fs::read_to_string(p.join("new.txt")).unwrap(), "local");
    // Plain `git stash pop` leaves the same partial state (tracked edit applied,
    // entry kept); parity, verified against real git by hand.
    let status = g(p, &["status", "--porcelain"]);
    assert!(status.contains("M a.txt"), "{status:?}");
}

#[test]
fn adv_pop_mismatch_error_is_typed_stale_stash() {
    let dir = base_repo();
    let p = dir.path();
    std::fs::write(p.join("a.txt"), "c").unwrap();
    git_in(p, &["stash", "push", "-m", "c"]);
    let oid = g(p, &["rev-parse", "refs/stash"]);
    let err = run_action_with(p.to_str().unwrap(), StashAction::Pop, 0, &oid, |_| {
        std::fs::write(p.join("a.txt"), "x").unwrap();
        git_in(p, &["stash", "push", "-m", "x"]);
        Ok(())
    })
    .unwrap_err();
    assert_eq!(err.kind(), crate::ipc_error::IpcErrorKind::StaleStash);
    assert_eq!(g(p, &["status", "--porcelain"]), "");
}

/// A `git` that hangs on `merge-tree` and is otherwise the real one.
fn hanging_merge_tree_path(dir: &Path) -> std::ffi::OsString {
    use std::os::unix::fs::PermissionsExt;
    let orig = std::env::var_os("PATH").unwrap_or_default();
    let real = std::process::Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let real = String::from_utf8(real.stdout).unwrap().trim().to_string();
    let script = format!(
        "#!/bin/sh\nfor a in \"$@\"; do if [ \"$a\" = merge-tree ]; then exec sleep 120; fi; done\nexec {real} \"$@\"\n"
    );
    let tool = dir.join("git");
    std::fs::write(&tool, script).unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::join_paths(
        std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&orig)),
    )
    .unwrap()
}

fn feature_branches(p: &Path, names: &[String]) -> Vec<(String, String)> {
    names
        .iter()
        .map(|n| {
            git_in(p, &["checkout", "-q", "-b", n, "main"]);
            std::fs::write(p.join(format!("{n}.txt")), n).unwrap();
            git_in(p, &["add", "."]);
            git_in(p, &["commit", "-q", "-m", n]);
            let tip = g(p, &["rev-parse", "HEAD"]);
            git_in(p, &["checkout", "-q", "main"]);
            (n.clone(), tip)
        })
        .collect()
}

#[test]
fn adv_hung_merge_tree_returns_within_deadline_and_counts_unchecked() {
    let dir = base_repo();
    let p = dir.path();
    let names: Vec<String> = (0..12).map(|i| format!("f{i:02}")).collect();
    let tips = feature_branches(p, &names);
    let base = g(p, &["rev-parse", "main"]);
    let wrap = tempfile::tempdir().unwrap();
    let path = hanging_merge_tree_path(wrap.path());
    let _env = bind_env_owned(env_serial()).set("PATH", path);
    let refs: Vec<(&str, &str)> = tips.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let started = Instant::now();
    let checks = tree_merge_checks(p, &base, &refs);
    let took = started.elapsed();
    assert!(took < Duration::from_secs(25), "deadline not enforced: {took:?}");
    assert_eq!(checks.unchecked, 12, "{:?}", checks.reason);
    assert!(checks.reason.is_some());
    assert!(checks.verdicts.iter().all(|v| matches!(v, TreeMerge::Unknown(_))));
}

#[test]
fn adv_clean_refuses_to_treat_a_hung_check_as_merged() {
    let dir = base_repo();
    let p = dir.path();
    let tips = feature_branches(p, &["feat".to_string()]);
    // The same change lands on main as a different commit: a squash merge.
    std::fs::write(p.join("feat.txt"), "feat").unwrap();
    git_in(p, &["add", "."]);
    git_in(p, &["commit", "-q", "-m", "squash"]);
    let _ = tips;
    let wrap = tempfile::tempdir().unwrap();
    let path = hanging_merge_tree_path(wrap.path());
    let _env = bind_env_owned(env_serial()).set("PATH", path);
    let result = clean_branches_with_gate(
        p.to_str().unwrap(),
        &["feat".to_string()],
        false,
        false,
        |_| Ok(()),
    );
    let deleted = result.map(|r| r.deleted).unwrap_or_default();
    assert!(deleted.is_empty(), "deleted on an unchecked verdict: {deleted:?}");
    drop(_env);
    assert!(!g(p, &["branch", "--list", "feat"]).is_empty(), "branch is gone");
}

#[test]
fn adv_cap_is_spent_only_on_branches_that_need_a_tree_check() {
    let _env = bind_env_owned(env_serial());
    let dir = base_repo();
    let p = dir.path();
    // Many branches at the default tip: nothing ahead, no tree check needed.
    std::fs::write(p.join("a.txt"), "base2").unwrap();
    git_in(p, &["commit", "-q", "-am", "move main"]);
    for i in 0..(MAX_TREE_CHECKS + 10) {
        git_in(p, &["branch", &format!("same{i:03}")]);
    }
    let names: Vec<String> = (0..3).map(|i| format!("zz{i}")).collect();
    feature_branches(p, &names);
    git_in(p, &["checkout", "-q", "main"]);
    let plan = crate::ops::branch_cleanup_plan(p.to_str().unwrap()).unwrap();
    assert_eq!(plan.unchecked_branches, 0, "{:?}", plan.unchecked_reason);
}
