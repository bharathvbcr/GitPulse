//! Adversarial tests for card ft-b2073db6: rebase plan must be a permutation of
//! `onto..HEAD`, and branch cleanup names targets by full ref. Test-only.

use crate::engine::deadbranch::{clean_branches_with_gate, DeadbranchCleanResult};
use crate::engine::git_cli::git_text;
use crate::engine::git_writer::{GitWriter, RebaseActionKind, RebaseStep};
use crate::ipc_error::CommandError;
use crate::test_support::env::{bind_env_owned, env_serial};
use crate::test_support::{git_in, git_repo};
use std::path::{Path, PathBuf};

pub(crate) fn g(p: &Path, args: &[&str]) -> String {
    git_text(p, args).expect("git").trim().to_string()
}

pub(crate) fn try_g(p: &Path, args: &[&str]) -> Option<String> {
    git_text(p, args).ok().map(|s| s.trim().to_string())
}

fn commit_file(p: &Path, name: &str) -> String {
    std::fs::write(p.join(name), format!("{name}\n")).unwrap();
    git_in(p, &["add", "--", name]);
    git_in(p, &["commit", "-q", "-m", &format!("add {name}")]);
    g(p, &["rev-parse", "HEAD"])
}

fn pick(id: &str) -> RebaseStep {
    RebaseStep {
        commit_id: id.to_string(),
        action: RebaseActionKind::Pick,
    }
}

fn picks(ids: &[String]) -> Vec<RebaseStep> {
    ids.iter().map(|c| pick(c)).collect()
}

/// `main` with a base commit, `feature` checked out with `n` commits on top.
fn feature(n: usize) -> (tempfile::TempDir, PathBuf, String, Vec<String>) {
    let dir = git_repo();
    let p = dir.path().canonicalize().unwrap();
    let base = commit_file(&p, "base.txt");
    git_in(&p, &["checkout", "-q", "-b", "feature"]);
    let commits = (0..n)
        .map(|i| commit_file(&p, &format!("f{i:03}.txt")))
        .collect();
    (dir, p, base, commits)
}

fn rebase(p: &Path, onto: &str, steps: &[RebaseStep]) -> Result<(), CommandError> {
    GitWriter::execute_rebase_sequence(p.to_str().unwrap(), onto, steps)
}

/// The tree `feature` holds: sorted tracked file names.
fn files(p: &Path, rev: &str) -> Vec<String> {
    g(p, &["ls-tree", "-r", "--name-only", rev])
        .lines()
        .map(str::to_string)
        .collect()
}

/// The branch either kept its tip (refusal) or carries every file it had
/// (a successful rebase): never a silent loss.
fn assert_no_loss(p: &Path, tip: &str, before_files: &[String], result: &Result<(), CommandError>) {
    let now = g(p, &["rev-parse", "refs/heads/feature"]);
    if result.is_err() {
        assert_eq!(now, tip, "refused rebase moved feature: {result:?}");
    } else {
        let after = files(p, "refs/heads/feature");
        for f in before_files {
            assert!(after.contains(f), "{f} lost after Ok rebase; has {after:?}");
        }
    }
}

// ---- rebase: forms of `onto` ---------------------------------------------

fn onto_form_holds(onto_for: impl Fn(&Path, &str) -> String) {
    let _env = bind_env_owned(env_serial());
    let (_d, p, base, commits) = feature(3);
    let tip = g(&p, &["rev-parse", "HEAD"]);
    let before = files(&p, "HEAD");
    let onto = onto_for(&p, &base);
    let r = rebase(&p, &onto, &picks(&commits));
    r.as_ref().expect("a full plan over onto..HEAD is accepted");
    assert_no_loss(&p, &tip, &before, &r);
    assert_eq!(
        g(&p, &["rev-list", "--count", &format!("{base}..feature")]),
        "3"
    );
}

#[test]
fn adv_onto_annotated_tag() {
    onto_form_holds(|p, base| {
        git_in(p, &["tag", "-a", "-m", "t", "v1", base]);
        "v1".to_string()
    });
}

#[test]
fn adv_onto_remote_ref() {
    onto_form_holds(|p, base| {
        git_in(p, &["update-ref", "refs/remotes/origin/main", base]);
        "origin/main".to_string()
    });
}

#[test]
fn adv_onto_head_tilde() {
    onto_form_holds(|_, _| "HEAD~3".to_string());
}

#[test]
fn adv_onto_abbreviated_oid() {
    onto_form_holds(|_, base| base[..10].to_string());
}

#[test]
fn adv_onto_not_an_ancestor_of_head_drops_nothing() {
    let _env = bind_env_owned(env_serial());
    let (_d, p, base, commits) = feature(3);
    git_in(&p, &["checkout", "-q", "-b", "other", &base]);
    commit_file(&p, "other.txt");
    git_in(&p, &["checkout", "-q", "feature"]);
    let tip = g(&p, &["rev-parse", "HEAD"]);
    let before = files(&p, "HEAD");
    let r = rebase(&p, "other", &picks(&commits));
    assert_no_loss(&p, &tip, &before, &r);
    r.expect("range other..HEAD is exactly the 3 feature commits");
    assert!(files(&p, "feature").contains(&"other.txt".to_string()));
}

#[test]
fn adv_onto_equal_to_head_refused() {
    let _env = bind_env_owned(env_serial());
    let (_d, p, _base, commits) = feature(2);
    let tip = g(&p, &["rev-parse", "HEAD"]);
    let r = rebase(&p, "HEAD", &picks(&commits));
    assert!(r.is_err(), "{r:?}");
    assert_eq!(g(&p, &["rev-parse", "HEAD"]), tip);
}

// ---- rebase: forms of step ids -------------------------------------------

#[test]
fn adv_step_ids_in_odd_shapes_never_lose_or_misapply() {
    let _env = bind_env_owned(env_serial());
    for id in [
        "--all",
        "-n",
        "HEAD",
        "HEAD~1",
        "HEAD^",
        "feature",
        "refs/heads/feature",
        "\u{1F600}",
        "\u{00e9}\u{00e9}",
        &"a".repeat(5000),
        "",
    ] {
        let (_d, p, base, commits) = feature(2);
        let tip = g(&p, &["rev-parse", "HEAD"]);
        let before = files(&p, "HEAD");
        let mut steps = picks(&commits);
        steps[0].commit_id = id.to_string();
        let r = rebase(&p, &base, &steps);
        assert_no_loss(&p, &tip, &before, &r);
        if r.is_ok() {
            // Accepted: the rebased branch must be the original content in order.
            let subjects = g(
                &p,
                &[
                    "log",
                    "--reverse",
                    "--format=%s",
                    &format!("{base}..feature"),
                ],
            );
            assert_eq!(
                subjects, "add f000.txt\nadd f001.txt",
                "step id {id:?} was accepted but a different commit was applied"
            );
        }
        let branch = g(&p, &["symbolic-ref", "--short", "HEAD"]);
        assert_eq!(branch, "feature", "HEAD left off the branch for id {id:?}");
    }
}

#[test]
fn adv_step_head_relative_second_step() {
    // `HEAD~1` resolves against the pre-detach HEAD in the plan check but the
    // replay hands the raw string to cherry-pick after `checkout --detach onto`.
    let _env = bind_env_owned(env_serial());
    let (_d, p, base, commits) = feature(2);
    let tip = g(&p, &["rev-parse", "HEAD"]);
    let before = files(&p, "HEAD");
    let steps = vec![pick("HEAD~1"), pick(&commits[1])];
    let r = rebase(&p, &base, &steps);
    assert_no_loss(&p, &tip, &before, &r);
    if r.is_ok() {
        let subjects = g(
            &p,
            &[
                "log",
                "--reverse",
                "--format=%s",
                &format!("{base}..feature"),
            ],
        );
        assert_eq!(subjects, "add f000.txt\nadd f001.txt");
    }
}

#[test]
fn adv_step_annotated_tag_on_range_commit() {
    let _env = bind_env_owned(env_serial());
    let (_d, p, base, commits) = feature(2);
    git_in(&p, &["tag", "-a", "-m", "t", "rel", &commits[0]]);
    let mut steps = picks(&commits);
    steps[0].commit_id = "rel".to_string();
    rebase(&p, &base, &steps).expect("an annotated tag naming a range commit is a commit");
    assert_eq!(
        g(&p, &["rev-list", "--count", &format!("{base}..feature")]),
        "2"
    );
}

#[test]
fn adv_same_commit_full_and_short_is_a_duplicate() {
    let _env = bind_env_owned(env_serial());
    let (_d, p, base, commits) = feature(2);
    let tip = g(&p, &["rev-parse", "HEAD"]);
    let steps = vec![pick(&commits[0]), pick(&commits[0][..12])];
    assert!(rebase(&p, &base, &steps).is_err());
    assert_eq!(g(&p, &["rev-parse", "HEAD"]), tip);
}

#[test]
fn adv_reordered_permutation_is_accepted() {
    let _env = bind_env_owned(env_serial());
    let (_d, p, base, commits) = feature(3);
    let steps = vec![pick(&commits[2]), pick(&commits[0]), pick(&commits[1])];
    rebase(&p, &base, &steps).expect("independent files reorder cleanly");
    assert_eq!(
        g(&p, &["rev-list", "--count", &format!("{base}..feature")]),
        "3"
    );
}

// ---- rebase: size limits and history shapes ------------------------------

#[test]
fn adv_range_at_the_cap_and_one_over() {
    let _env = bind_env_owned(env_serial());
    let dir = git_repo();
    let p = dir.path().canonicalize().unwrap();
    let base = commit_file(&p, "base.txt");
    git_in(&p, &["checkout", "-q", "-b", "feature"]);
    let mut ids = Vec::new();
    for i in 0..200 {
        git_in(
            &p,
            &["commit", "-q", "--allow-empty", "-m", &format!("e{i}")],
        );
        ids.push(g(&p, &["rev-parse", "HEAD"]));
    }
    let last = commit_file(&p, "last.txt");
    // 201 commits in the range: refused.
    ids.push(last.clone());
    let tip = g(&p, &["rev-parse", "HEAD"]);
    let steps: Vec<RebaseStep> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| RebaseStep {
            commit_id: id.clone(),
            action: if i == ids.len() - 1 {
                RebaseActionKind::Pick
            } else {
                RebaseActionKind::Drop
            },
        })
        .collect();
    let r = rebase(&p, &base, &steps);
    assert!(r.is_err(), "201-commit range accepted");
    assert_eq!(g(&p, &["rev-parse", "HEAD"]), tip);
    // Exactly 200: a plan over the newest 200 against an onto one commit up.
    let onto = ids[0].clone();
    let r = rebase(&p, &onto, &steps[1..]);
    r.expect("exactly MAX_REPLAY_COMMITS commits is accepted");
    assert_eq!(
        g(&p, &["rev-list", "--count", &format!("{onto}..feature")]),
        "1"
    );
}

#[test]
fn adv_orphan_onto_unrelated_history_loses_nothing() {
    let _env = bind_env_owned(env_serial());
    let (_d, p, _base, commits) = feature(2);
    git_in(&p, &["checkout", "-q", "--orphan", "island"]);
    git_in(&p, &["rm", "-rfq", "."]);
    std::fs::write(p.join("island.txt"), "i\n").unwrap();
    git_in(&p, &["add", "island.txt"]);
    git_in(&p, &["commit", "-q", "-m", "island root"]);
    git_in(&p, &["checkout", "-q", "feature"]);
    let tip = g(&p, &["rev-parse", "HEAD"]);
    let before = files(&p, "HEAD");
    let range: Vec<String> = g(&p, &["rev-list", "--reverse", "island..HEAD"])
        .lines()
        .map(str::to_string)
        .collect();
    assert!(
        range.len() > commits.len(),
        "range includes the feature's root"
    );
    let r = rebase(&p, "island", &picks(&range));
    // A root commit cannot be cherry-picked cleanly onto unrelated history; the
    // contract is only that nothing is lost and HEAD stays on the branch.
    assert_no_loss(&p, &tip, &before, &r);
    assert_eq!(g(&p, &["symbolic-ref", "--short", "HEAD"]), "feature");
}

#[test]
fn adv_detached_head_at_start() {
    let _env = bind_env_owned(env_serial());
    let (_d, p, base, commits) = feature(2);
    let tip = g(&p, &["rev-parse", "HEAD"]);
    git_in(&p, &["checkout", "-q", "--detach"]);
    let r = rebase(&p, &base, &picks(&commits));
    assert_eq!(
        g(&p, &["rev-parse", "refs/heads/feature"]),
        tip,
        "branch untouched"
    );
    if r.is_ok() {
        assert!(
            try_g(&p, &["symbolic-ref", "-q", "HEAD"]).is_none(),
            "stays detached"
        );
        assert_eq!(
            g(&p, &["rev-list", "--count", &format!("{base}..HEAD")]),
            "2"
        );
    }
}

// ---- branch cleanup -------------------------------------------------------

pub(crate) struct Fixture {
    pub(crate) _dirs: Vec<tempfile::TempDir>,
    pub(crate) repo: PathBuf,
    pub(crate) bares: Vec<(String, PathBuf)>,
}

/// A repo on `work` (main is not checked out) with `main`, and each remote
/// holding `branches` at main's commit.
pub(crate) fn cleanup_fixture(remotes: &[(&str, &[&str])]) -> Fixture {
    let dir = git_repo();
    let repo = dir.path().canonicalize().unwrap();
    git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let mut dirs = vec![dir];
    let mut bares = Vec::new();
    for (remote, branches) in remotes {
        let bare = tempfile::tempdir().unwrap();
        let bare_path = bare.path().canonicalize().unwrap();
        git_in(&bare_path, &["init", "-q", "--bare"]);
        // `git remote add origin/eu` is refused beside `origin`; config is not.
        git_in(
            &repo,
            &[
                "config",
                &format!("remote.{remote}.url"),
                bare_path.to_str().unwrap(),
            ],
        );
        git_in(
            &repo,
            &[
                "config",
                &format!("remote.{remote}.fetch"),
                &format!("+refs/heads/*:refs/remotes/{remote}/*"),
            ],
        );
        for b in *branches {
            git_in(
                &repo,
                &["push", "-q", remote, &format!("HEAD:refs/heads/{b}")],
            );
        }
        git_in(&repo, &["fetch", "-q", remote]);
        bares.push((remote.to_string(), bare_path));
        dirs.push(bare);
    }
    git_in(&repo, &["checkout", "-q", "-b", "work"]);
    Fixture {
        _dirs: dirs,
        repo,
        bares,
    }
}

pub(crate) fn clean(
    repo: &Path,
    names: &[&str],
    force: bool,
) -> (Vec<String>, Result<DeadbranchCleanResult, CommandError>) {
    let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    let mut judged = Vec::new();
    let r = clean_branches_with_gate(repo.to_str().unwrap(), &names, force, false, |argv| {
        judged.push(argv.join(" "));
        Ok(())
    });
    (judged, r)
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[test]
fn adv_local_branch_literally_named_like_a_remote_branch() {
    let _env = bind_env_owned(env_serial());
    let f = cleanup_fixture(&[("origin", &["feature"])]);
    git_in(&f.repo, &["branch", "origin/feature"]);
    let (judged, r) = clean(&f.repo, &["refs/heads/origin/feature"], false);
    assert_eq!(judged, vec!["git branch -d origin/feature"], "{r:?}");
    assert!(try_g(
        &f.repo,
        &["rev-parse", "--verify", "-q", "refs/heads/origin/feature"]
    )
    .is_none());
    assert!(try_g(
        &f.repo,
        &["rev-parse", "--verify", "-q", "refs/remotes/origin/feature"]
    )
    .is_some());
    assert!(try_g(
        &f.bares[0].1,
        &["rev-parse", "--verify", "-q", "refs/heads/feature"]
    )
    .is_some());
}

#[test]
fn adv_bare_name_that_looks_like_a_remote_ref_reaches_no_remote() {
    let _env = bind_env_owned(env_serial());
    let f = cleanup_fixture(&[("origin", &["feature"])]);
    git_in(&f.repo, &["branch", "origin/feature"]);
    // Legacy short-name caller sending the local branch's name.
    let (judged, _r) = clean(&f.repo, &["origin/feature"], false);
    assert_eq!(judged, vec!["git branch -d origin/feature"]);
    assert!(try_g(
        &f.bares[0].1,
        &["rev-parse", "--verify", "-q", "refs/heads/feature"]
    )
    .is_some());
}

#[test]
fn adv_local_branch_named_with_a_refs_heads_prefix() {
    let _env = bind_env_owned(env_serial());
    let f = cleanup_fixture(&[]);
    git_in(&f.repo, &["branch", "x"]);
    // Does a branch literally named `refs/heads/x` exist beside `x`?
    let made = git_text(&f.repo, &["branch", "refs/heads/x"]).is_ok()
        && try_g(
            &f.repo,
            &["rev-parse", "--verify", "-q", "refs/heads/refs/heads/x"],
        )
        .is_some();
    if !made {
        return; // git cannot hold both here; nothing to attack
    }
    let (judged, r) = clean(&f.repo, &["refs/heads/refs/heads/x"], false);
    assert_eq!(judged.len(), 1, "{judged:?} {r:?}");
    assert!(
        try_g(&f.repo, &["rev-parse", "--verify", "-q", "refs/heads/x"]).is_some(),
        "the unrelated branch x was deleted"
    );
}

#[test]
fn adv_local_and_remote_of_same_name_yield_exactly_two_argv() {
    let _env = bind_env_owned(env_serial());
    let f = cleanup_fixture(&[("origin", &["feature"])]);
    git_in(&f.repo, &["branch", "feature"]);
    let (judged, r) = clean(
        &f.repo,
        &["refs/heads/feature", "refs/remotes/origin/feature"],
        false,
    );
    assert_eq!(
        sorted(judged),
        vec!["git branch -d feature", "git push origin --delete feature"],
        "{r:?}"
    );
    let res = r.unwrap();
    assert_eq!(res.deleted.len(), 2, "{res:?}");
}

#[test]
fn adv_nested_remote_names_origin_and_origin_eu() {
    let _env = bind_env_owned(env_serial());
    // `origin/eu` holds x; `origin` holds a branch eu/y. Both fetch into
    // refs/remotes/origin/eu/*, so neither ref has a provable owner.
    let f = cleanup_fixture(&[("origin", &["eu/y"]), ("origin/eu", &["x"])]);
    for target in ["refs/remotes/origin/eu/x", "refs/remotes/origin/eu/y"] {
        let (judged, r) = clean(&f.repo, &[target], false);
        assert!(judged.is_empty(), "{target}: {judged:?} {r:?}");
        let message = r.expect_err("an ambiguous owner is refused").into_message();
        assert!(
            message.contains("origin")
                && message.contains("origin/eu")
                && message.contains("ambiguous"),
            "{message}"
        );
    }
    for (bare, branch) in [(&f.bares[0].1, "eu/y"), (&f.bares[1].1, "x")] {
        assert!(try_g(
            bare,
            &[
                "rev-parse",
                "--verify",
                "-q",
                &format!("refs/heads/{branch}")
            ]
        )
        .is_some());
    }
}

/// A remote removed from config after its tracking refs were fetched has no
/// owner; nothing reaches the gate.
#[test]
fn adv_remote_removed_from_config_after_scan_fails_closed() {
    let _env = bind_env_owned(env_serial());
    let f = cleanup_fixture(&[("origin", &["feature"])]);
    git_in(&f.repo, &["config", "--remove-section", "remote.origin"]);
    let (judged, r) = clean(&f.repo, &["refs/remotes/origin/feature"], false);
    assert!(
        judged.is_empty(),
        "pushed to an unconfigured remote: {judged:?} {r:?}"
    );
    assert!(try_g(
        &f.bares[0].1,
        &["rev-parse", "--verify", "-q", "refs/heads/feature"]
    )
    .is_some());
}

/// A single nested-name remote with no overlapping remote still deletes.
#[test]
fn a_lone_nested_remote_still_deletes_its_branch() {
    let _env = bind_env_owned(env_serial());
    let f = cleanup_fixture(&[("team/fork", &["x"])]);
    let (judged, r) = clean(&f.repo, &["refs/remotes/team/fork/x"], false);
    assert_eq!(judged, vec!["git push team/fork --delete x"], "{r:?}");
    assert!(try_g(
        &f.bares[0].1,
        &["rev-parse", "--verify", "-q", "refs/heads/x"]
    )
    .is_none());
}

/// An ordinary remote and a slashed branch still delete.
#[test]
fn an_ordinary_remote_with_a_slashed_branch_still_deletes() {
    let _env = bind_env_owned(env_serial());
    let f = cleanup_fixture(&[("origin", &["feature/login"])]);
    let (judged, r) = clean(&f.repo, &["refs/remotes/origin/feature/login"], false);
    assert_eq!(
        judged,
        vec!["git push origin --delete feature/login"],
        "{r:?}"
    );
    assert!(try_g(
        &f.bares[0].1,
        &["rev-parse", "--verify", "-q", "refs/heads/feature/login"]
    )
    .is_none());
}

#[test]
fn adv_unicode_remote_and_branch() {
    let _env = bind_env_owned(env_serial());
    let f = cleanup_fixture(&[("r\u{00e9}seau", &["\u{00df}-\u{00fc}n\u{00ef}"])]);
    git_in(&f.repo, &["branch", "\u{00df}-\u{00fc}n\u{00ef}"]);
    let (judged, r) = clean(
        &f.repo,
        &["refs/remotes/r\u{00e9}seau/\u{00df}-\u{00fc}n\u{00ef}"],
        false,
    );
    assert_eq!(
        judged,
        vec!["git push r\u{00e9}seau --delete \u{00df}-\u{00fc}n\u{00ef}"],
        "{r:?}"
    );
    let (judged, r) = clean(&f.repo, &["refs/heads/\u{00df}-\u{00fc}n\u{00ef}"], false);
    assert_eq!(
        judged,
        vec!["git branch -d \u{00df}-\u{00fc}n\u{00ef}"],
        "{r:?}"
    );
}

#[test]
fn adv_force_and_merged_by_tree_pick_the_right_flag() {
    let _env = bind_env_owned(env_serial());
    let f = cleanup_fixture(&[]);
    let p = &f.repo;
    git_in(p, &["checkout", "-q", "main"]);
    git_in(p, &["checkout", "-q", "-b", "sq"]);
    commit_file(p, "s.txt");
    git_in(p, &["checkout", "-q", "main"]);
    // Same content landed as a different commit: squash-merged by tree.
    std::fs::write(p.join("s.txt"), "s.txt\n").unwrap();
    git_in(p, &["add", "s.txt"]);
    git_in(p, &["commit", "-q", "-m", "squash of sq"]);
    git_in(p, &["checkout", "-q", "-b", "ahead"]);
    commit_file(p, "a.txt");
    git_in(p, &["checkout", "-q", "work"]);
    git_in(p, &["merge", "-q", "--ff-only", "main"]);
    let (judged, r) = clean(p, &["refs/heads/sq"], false);
    assert_eq!(judged, vec!["git branch -D sq"], "{r:?}");
    let (judged, r) = clean(p, &["refs/heads/ahead"], false);
    assert_eq!(judged, vec!["git branch -d ahead"], "{r:?}");
    let (judged, _r) = clean(p, &["refs/heads/ahead"], true);
    assert_eq!(judged, vec!["git branch -D ahead"]);
}

#[test]
fn adv_protected_and_checked_out_refs_stay_refused_by_full_ref() {
    let _env = bind_env_owned(env_serial());
    let f = cleanup_fixture(&[("origin", &["feature"])]);
    git_in(&f.repo, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
    git_in(&f.repo, &["fetch", "-q", "origin"]);
    for (name, why) in [
        ("refs/heads/main", "default branch, local"),
        ("refs/remotes/origin/main", "default branch, remote"),
        ("refs/heads/work", "checked out"),
    ] {
        let (judged, r) = clean(&f.repo, &[name], true);
        assert!(
            r.is_err() || judged.is_empty(),
            "{why} ({name}) was deleted: {judged:?} {r:?}"
        );
        assert!(judged.is_empty(), "{why}: judged {judged:?}");
    }
    assert!(try_g(
        &f.bares[0].1,
        &["rev-parse", "--verify", "-q", "refs/heads/main"]
    )
    .is_some());
    assert!(try_g(&f.repo, &["rev-parse", "--verify", "-q", "refs/heads/work"]).is_some());
}

#[test]
fn adv_foreign_ref_namespaces_select_nothing() {
    let _env = bind_env_owned(env_serial());
    let f = cleanup_fixture(&[("origin", &["feature"])]);
    git_in(&f.repo, &["tag", "v1"]);
    let (judged, _r) = clean(
        &f.repo,
        &[
            "refs/tags/v1",
            "refs/remotes/origin/../../heads/work",
            "refs/remotes/origin/feature/../feature",
            "",
        ],
        true,
    );
    assert!(judged.is_empty(), "{judged:?}");
    assert!(try_g(
        &f.bares[0].1,
        &["rev-parse", "--verify", "-q", "refs/heads/feature"]
    )
    .is_some());
}

// ---- branch cleanup: refspec / owner re-attack (f00d6e50) ------------------

use crate::engine::remotes::FetchClaims;

fn present(p: &Path, r: &str) -> bool {
    try_g(p, &["rev-parse", "--verify", "-q", r]).is_some()
}

fn cfg(p: &Path, args: &[&str]) {
    let mut v = vec!["config"];
    v.extend_from_slice(args);
    git_in(p, &v);
}

/// Replace every fetch refspec of `remote` with `specs` and fetch.
fn respec(p: &Path, remote: &str, specs: &[&str]) {
    let key = format!("remote.{remote}.fetch");
    git_in(p, &["config", "--unset-all", &key]);
    for s in specs {
        git_in(p, &["config", "--add", &key, s]);
    }
    git_in(p, &["fetch", "-q", remote]);
}

fn assert_refused_untouched(
    f: &Fixture,
    target: &str,
    judged: &[String],
    r: &Result<DeadbranchCleanResult, CommandError>,
) {
    assert!(
        judged.is_empty(),
        "{target}: reached the gate: {judged:?} {r:?}"
    );
    for (_, bare) in &f.bares {
        assert!(
            present(bare, "refs/heads/x"),
            "{target}: a remote branch was deleted"
        );
    }
}

#[test]
fn adv2_nondefault_refspec_into_unconfigured_namespace_is_refused() {
    let _env = bind_env_owned(env_serial());
    let f = cleanup_fixture(&[("origin", &["x"])]);
    respec(
        &f.repo,
        "origin",
        &["+refs/heads/*:refs/remotes/upstream/*"],
    );
    assert!(present(&f.repo, "refs/remotes/upstream/x"));
    let (judged, r) = clean(&f.repo, &["refs/remotes/upstream/x"], true);
    assert_refused_untouched(&f, "upstream/x", &judged, &r);
}

#[test]
fn adv2_nondefault_refspec_into_a_configured_remotes_namespace_is_refused() {
    let _env = bind_env_owned(env_serial());
    let f = cleanup_fixture(&[("origin", &["x"]), ("upstream", &["x"])]);
    respec(
        &f.repo,
        "origin",
        &["+refs/heads/*:refs/remotes/upstream/*"],
    );
    let (judged, r) = clean(&f.repo, &["refs/remotes/upstream/x"], true);
    assert_refused_untouched(&f, "upstream/x", &judged, &r);
    assert!(r.unwrap_err().into_message().contains("ambiguous"));
}

#[test]
fn adv2_refspec_shapes_claim_exactly_their_destinations() {
    let _env = bind_env_owned(env_serial());
    let f = cleanup_fixture(&[("o", &["a-dev"])]);
    let p = &f.repo;
    git_in(p, &["config", "--unset-all", "remote.o.fetch"]);
    for s in [
        "^refs/heads/secret",
        "refs/heads/*-dev:refs/remotes/o/*-dev",
        "refs/heads/only:refs/remotes/o/only",
    ] {
        git_in(p, &["config", "--add", "remote.o.fetch", s]);
    }
    let claims = FetchClaims::read(p).unwrap();
    assert_eq!(claims.claimants("o/a-dev"), vec!["o"]);
    assert_eq!(claims.claimants("o/only"), vec!["o"]);
    assert!(claims.claimants("o/a").is_empty());
    assert!(claims.claimants("o/secret").is_empty());
    // A lone negative refspec fetches nothing: no claim, and no default claim.
    git_in(p, &["config", "--unset-all", "remote.o.fetch"]);
    git_in(
        p,
        &["config", "--add", "remote.o.fetch", "^refs/heads/secret"],
    );
    let claims = FetchClaims::read(p).unwrap();
    assert!(claims.claimants("o/a-dev").is_empty());
}

#[test]
fn adv2_negative_refspec_beside_a_positive_one_still_deletes() {
    let _env = bind_env_owned(env_serial());
    let f = cleanup_fixture(&[("origin", &["x", "secret"])]);
    cfg(
        &f.repo,
        &["--add", "remote.origin.fetch", "^refs/heads/secret"],
    );
    let (judged, r) = clean(&f.repo, &["refs/remotes/origin/x"], true);
    assert_eq!(judged, vec!["git push origin --delete x"], "{r:?}");
}

#[test]
fn adv2_dotted_and_prefix_named_remotes() {
    let _env = bind_env_owned(env_serial());
    let f = cleanup_fixture(&[("origin", &["x"]), ("orig", &["y"]), ("my.fork", &["z"])]);
    let (judged, r) = clean(&f.repo, &["refs/remotes/origin/x"], true);
    assert_eq!(judged, vec!["git push origin --delete x"], "{r:?}");
    let (judged, r) = clean(&f.repo, &["refs/remotes/orig/y"], true);
    assert_eq!(judged, vec!["git push orig --delete y"], "{r:?}");
    let (judged, r) = clean(&f.repo, &["refs/remotes/my.fork/z"], true);
    assert_eq!(judged, vec!["git push my.fork --delete z"], "{r:?}");
}

#[test]
fn adv2_metacharacter_remote_name_claims_nothing_spurious() {
    let _env = bind_env_owned(env_serial());
    let f = cleanup_fixture(&[("origin", &["x"])]);
    // git refuses `*` in ref names, so such a remote has no tracking refs, but
    // its config must not make origin's refs ambiguous or unreadable.
    cfg(&f.repo, &["remote.a*b.url", "/nonexistent"]);
    cfg(&f.repo, &["remote.a.b.url", "/nonexistent2"]);
    let claims = FetchClaims::read(&f.repo).unwrap();
    assert_eq!(claims.claimants("origin/x"), vec!["origin"]);
    let (judged, r) = clean(&f.repo, &["refs/remotes/origin/x"], true);
    assert_eq!(judged, vec!["git push origin --delete x"], "{r:?}");
}

#[test]
fn adv2_unreadable_config_refuses_every_remote_delete() {
    let _env = bind_env_owned(env_serial());
    let f = cleanup_fixture(&[("origin", &["x"])]);
    assert!(FetchClaims::read(&f.repo).is_ok());
    let cfg_path = f.repo.join(".git/config");
    let good = std::fs::read_to_string(&cfg_path).unwrap();
    std::fs::write(
        &cfg_path,
        format!("{good}\n[remote \"origin\"\n\tgarbage ===\n"),
    )
    .unwrap();
    assert!(
        FetchClaims::read(&f.repo).is_err(),
        "malformed config read as claims"
    );
    let (judged, r) = clean(&f.repo, &["refs/remotes/origin/x"], true);
    assert!(judged.is_empty(), "{judged:?} {r:?}");
    assert!(r.is_err());
    assert!(present(&f.bares[0].1, "refs/heads/x"));
}

#[test]
fn adv2_unreadable_include_refuses() {
    let _env = bind_env_owned(env_serial());
    let f = cleanup_fixture(&[("origin", &["x"])]);
    let dir = f.repo.join("incdir");
    std::fs::create_dir(&dir).unwrap();
    cfg(&f.repo, &["include.path", dir.to_str().unwrap()]);
    let readable = git_text(&f.repo, &["config", "--get-regexp", "-z", "^remote\\..*"]).is_ok();
    let (judged, r) = clean(&f.repo, &["refs/remotes/origin/x"], true);
    if !readable {
        assert!(judged.is_empty(), "{judged:?} {r:?}");
        assert!(present(&f.bares[0].1, "refs/heads/x"));
    }
}

#[test]
fn adv2_mixed_selection_with_an_ambiguous_ref_deletes_nothing() {
    let _env = bind_env_owned(env_serial());
    let f = cleanup_fixture(&[("origin", &["eu/y", "ok"]), ("origin/eu", &["x"])]);
    git_in(&f.repo, &["branch", "gone"]);
    let (judged, r) = clean(
        &f.repo,
        &[
            "refs/heads/gone",
            "refs/remotes/origin/ok",
            "refs/remotes/origin/eu/x",
        ],
        true,
    );
    assert!(judged.is_empty(), "reached authorize: {judged:?} {r:?}");
    let msg = r.expect_err("whole clean refused").into_message();
    assert!(
        msg.contains("refs/remotes/origin/eu/x"),
        "culprit not named: {msg}"
    );
    assert!(present(&f.repo, "refs/heads/gone"), "local branch deleted");
    assert!(present(&f.bares[0].1, "refs/heads/ok"));
    assert!(present(&f.bares[1].1, "refs/heads/x"));
}

#[test]
fn adv2_local_only_clean_ignores_broken_remote_config() {
    let _env = bind_env_owned(env_serial());
    let f = cleanup_fixture(&[("origin", &["eu/y"]), ("origin/eu", &["x"])]);
    git_in(&f.repo, &["branch", "gone"]);
    // Valid-but-odd remote config (git itself rejects an invalid refspec for
    // every branch operation, which no remote-aware check could change).
    cfg(
        &f.repo,
        &[
            "--add",
            "remote.nourl.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    cfg(&f.repo, &["--add", "remote.neg.fetch", "^refs/heads/x"]);
    let (judged, r) = clean(&f.repo, &["refs/heads/gone"], true);
    assert_eq!(judged, vec!["git branch -D gone"], "{r:?}");
    assert!(!present(&f.repo, "refs/heads/gone"));
}
