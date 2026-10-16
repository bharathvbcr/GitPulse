//! Adversarial checks on the branch-checkout writer: the judged argv versus the
//! argv that runs, option and ref injection, and remote-name mapping.

use super::*;
use crate::test_support::{git_in, git_repo, trust_repo};

/// A clone with `main` checked out and `origin/{main,feature}` fetched.
fn clone_with_remote_feature() -> (tempfile::TempDir, tempfile::TempDir, std::path::PathBuf) {
    let origin = git_repo();
    git_in(
        origin.path(),
        &["commit", "--allow-empty", "-q", "-m", "base"],
    );
    git_in(origin.path(), &["switch", "-q", "-c", "feature"]);
    git_in(
        origin.path(),
        &["commit", "--allow-empty", "-q", "-m", "on feature"],
    );
    git_in(origin.path(), &["switch", "-q", "main"]);
    let parent = tempfile::TempDir::new().unwrap();
    git_in(
        parent.path(),
        &["clone", "-q", origin.path().to_str().unwrap(), "work"],
    );
    let work = parent.path().join("work").canonicalize().unwrap();
    trust_repo(&work);
    (origin, parent, work)
}

fn attached(repo: &std::path::Path) -> Option<String> {
    git_text(repo, &["symbolic-ref", "-q", "HEAD"])
        .ok()
        .and_then(|name| name.trim().strip_prefix("refs/heads/").map(String::from))
}

fn head_oid(repo: &std::path::Path) -> String {
    git_text(repo, &["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string()
}

fn local_branches(repo: &std::path::Path) -> Vec<String> {
    git_text(repo, &["for-each-ref", "--format=%(refname)", "refs/heads"])
        .unwrap()
        .lines()
        .map(String::from)
        .collect()
}

/// The argv judged by the gate is the argv that runs: a ref that appears
/// between the judging call and the run must make the run refuse, not run a
/// different command than the one that was judged.
#[test]
fn a_ref_created_after_judging_cannot_change_the_argv_that_runs() {
    let (_origin, _parent, work) = clone_with_remote_feature();
    let path = work.to_str().unwrap();
    let judged = GitWriter::checkout_branch_argv(path, "origin/feature").unwrap();
    assert_eq!(
        judged[1..],
        ["switch", "--track", "-c", "feature", "origin/feature"]
    );
    // Between the gate and the run, an unrelated local `feature` appears.
    git_in(&work, &["branch", "feature", "main"]);
    let result = GitWriter::checkout_branch_judged(path, "origin/feature", &judged);
    assert!(
        result.is_err(),
        "a run whose argv would differ from the judged one must refuse"
    );
    assert_eq!(attached(&work).as_deref(), Some("main"));
}

#[test]
fn a_ref_deleted_after_judging_cannot_change_the_argv_that_runs() {
    let (_origin, _parent, work) = clone_with_remote_feature();
    let path = work.to_str().unwrap();
    git_in(&work, &["branch", "feature", "origin/feature"]);
    let judged = GitWriter::checkout_branch_argv(path, "origin/feature").unwrap();
    assert_eq!(judged[1..], ["switch", "feature"]);
    git_in(&work, &["branch", "-D", "feature"]);
    let result = GitWriter::checkout_branch_judged(path, "origin/feature", &judged);
    assert!(result.is_err());
    assert!(!local_branches(&work)
        .iter()
        .any(|b| b == "refs/heads/feature"));
}

#[test]
fn a_judged_argv_that_still_matches_runs() {
    let (_origin, _parent, work) = clone_with_remote_feature();
    let path = work.to_str().unwrap();
    let judged = GitWriter::checkout_branch_argv(path, "origin/feature").unwrap();
    let outcome = GitWriter::checkout_branch_judged(path, "origin/feature", &judged).unwrap();
    assert!(!outcome.detached);
    assert_eq!(attached(&work).as_deref(), Some("feature"));
}

/// Hostile and odd names must never detach HEAD, leave a stray branch named
/// like a revision, or be read as an option.
#[test]
fn hostile_branch_names_never_detach_or_create_stray_branches() {
    let (_origin, _parent, work) = clone_with_remote_feature();
    let path = work.to_str().unwrap();
    let long = "a".repeat(300);
    let names = [
        "-x",
        "--orphan=x",
        "--force",
        "-c",
        "--detach",
        "@",
        "@{-1}",
        "HEAD",
        "..",
        "a..b",
        "refs/heads/main",
        "refs/heads/feature",
        "refs/remotes/origin/feature",
        "heads/main",
        "origin/HEAD",
        "origin",
        "main ",
        "x.lock",
        "origin/feature.lock",
        "\u{fc}ni\u{e7}\u{f6}de",
        "HEAD~1",
        "main^",
        "main~0",
        long.as_str(),
    ];
    let before = local_branches(&work);
    let main_oid = head_oid(&work);
    for name in names {
        let result = GitWriter::checkout_branch(path, name);
        let now = attached(&work);
        assert!(
            now.is_some(),
            "checkout_branch({name:?}) detached HEAD (result {result:?})"
        );
        let after = local_branches(&work);
        let stray: Vec<_> = after.iter().filter(|b| !before.contains(b)).collect();
        assert!(
            stray.is_empty() || stray == ["refs/heads/feature"],
            "checkout_branch({name:?}) created stray branches {stray:?}"
        );
        // Put the repository back for the next probe.
        git_in(&work, &["switch", "-q", "main"]);
        let _ = git_text(&work, &["branch", "-D", "feature"]);
        let _ = git_text(&work, &["branch", "-D", "HEAD"]);
        assert_eq!(head_oid(&work), main_oid);
    }
}

#[test]
fn a_revision_starting_with_a_dash_never_reaches_argv() {
    for rev in ["-x", "--orphan=x", "--force", "-", "--", "-c"] {
        assert!(GitWriter::checkout_detached_argv(rev).is_err(), "{rev}");
    }
}

#[test]
fn checkout_detached_refuses_ranges_and_reflog_grammar() {
    let (_origin, _parent, work) = clone_with_remote_feature();
    let path = work.to_str().unwrap();
    for rev in [
        "a..b",
        "..",
        "HEAD@{1}",
        "main:path",
        "@{-1}",
        "main^{tree}",
    ] {
        assert!(GitWriter::checkout_detached(path, rev).is_err(), "{rev}");
        assert_eq!(attached(&work).as_deref(), Some("main"));
    }
}

/// A remote whose name contains a slash, a remote that prefixes another, and a
/// branch that is named like a remote ref.
#[test]
fn remote_mapping_handles_slashed_and_prefix_remotes() {
    let (origin, _parent, work) = clone_with_remote_feature();
    let path = work.to_str().unwrap();
    let url = origin.path().to_str().unwrap();
    git_in(&work, &["remote", "add", "a/b", url]);
    git_in(&work, &["fetch", "-q", "a/b"]);
    git_in(&work, &["remote", "add", "origin2", url]);
    git_in(&work, &["fetch", "-q", "origin2"]);

    GitWriter::checkout_branch(path, "a/b/feature").unwrap();
    assert_eq!(attached(&work).as_deref(), Some("feature"));
    assert_eq!(
        git_text(&work, &["rev-parse", "--abbrev-ref", "feature@{upstream}"])
            .unwrap()
            .trim(),
        "a/b/feature"
    );
    git_in(&work, &["switch", "-q", "main"]);
    git_in(&work, &["branch", "-D", "feature"]);

    GitWriter::checkout_branch(path, "origin2/feature").unwrap();
    assert_eq!(attached(&work).as_deref(), Some("feature"));
    assert_eq!(
        git_text(&work, &["rev-parse", "--abbrev-ref", "feature@{upstream}"])
            .unwrap()
            .trim(),
        "origin2/feature"
    );
}

/// A remote-tracking ref whose branch part starts with a dash must not become
/// an option once the remote prefix is stripped (`origin/-x` -> `-x`).
#[test]
fn a_dashed_branch_part_of_a_tracking_name_never_becomes_an_option() {
    let (_origin, _parent, work) = clone_with_remote_feature();
    let path = work.to_str().unwrap();
    git_in(
        &work,
        &["update-ref", "refs/remotes/origin/-x", "origin/feature"],
    );
    for name in ["origin/-x", "origin/--orphan=x", "origin/HEAD"] {
        if name != "origin/-x" {
            git_in(
                &work,
                &[
                    "update-ref",
                    &format!("refs/remotes/{name}"),
                    "origin/feature",
                ],
            );
        }
        let argv = GitWriter::checkout_branch_argv(path, name);
        if let Ok(argv) = argv {
            assert!(
                argv[1..]
                    .iter()
                    .skip(1)
                    .all(|a| !a.starts_with('-')
                        || ["--track", "-c", "--guess"].contains(&a.as_str())),
                "{name}: option-shaped argument in {argv:?}"
            );
        }
        let _ = GitWriter::checkout_branch(path, name);
        assert_eq!(attached(&work).as_deref(), Some("main"), "{name}");
        assert_eq!(local_branches(&work), ["refs/heads/main"], "{name}");
    }
}

/// Remotes `a` and `a/b` both own `refs/remotes/a/b/x` by name; the writer
/// cannot tell which branch that is, so it refuses rather than guess.
#[test]
fn a_tracking_name_two_configured_remotes_could_own_is_refused() {
    let (origin, _parent, work) = clone_with_remote_feature();
    let path = work.to_str().unwrap();
    let url = origin.path().to_str().unwrap();
    // `git remote add` refuses overlapping names; hand-edited config does not.
    git_in(&work, &["config", "remote.a.url", url]);
    git_in(&work, &["config", "remote.a/b.url", url]);
    git_in(
        &work,
        &["update-ref", "refs/remotes/a/b/x", "origin/feature"],
    );
    let result = GitWriter::checkout_branch(path, "a/b/x");
    assert!(
        result.is_err(),
        "ambiguous owner must be refused: {result:?}"
    );
    assert_eq!(attached(&work).as_deref(), Some("main"));
    assert!(!local_branches(&work).iter().any(|b| b.ends_with("/x")));
}

/// The remote list must be read fresh for a decision that creates a branch:
/// a remote added after a cached read is still a remote.
#[test]
fn a_remote_added_after_a_cached_config_read_is_still_a_remote() {
    let (origin, _parent, work) = clone_with_remote_feature();
    let path = work.to_str().unwrap();
    let _warm = crate::engine::git_reader::configured_remote_names(&work);
    git_in(
        &work,
        &["remote", "add", "a/b", origin.path().to_str().unwrap()],
    );
    git_in(&work, &["fetch", "-q", "a/b"]);
    GitWriter::checkout_branch(path, "a/b/feature").unwrap();
    assert_eq!(attached(&work).as_deref(), Some("feature"));
}

/// A remote that is not configured but still has refs under `refs/remotes/`
/// (a deleted remote, a hand-made ref). The first-slash fallback must not
/// switch to an unrelated local branch that happens to carry the tail's name.
#[test]
fn an_unconfigured_remote_namespace_never_switches_to_a_lookalike_local_branch() {
    let (_origin, _parent, work) = clone_with_remote_feature();
    let path = work.to_str().unwrap();
    // refs/remotes/gone/team/x -- no remote "gone" is configured.
    git_in(
        &work,
        &["update-ref", "refs/remotes/gone/team/x", "origin/feature"],
    );
    // A local branch whose name equals the tail after the first slash.
    git_in(&work, &["branch", "team/x", "main"]);
    let result = GitWriter::checkout_branch(path, "gone/team/x");
    // Whatever it does, it must not silently land on the unrelated `team/x`
    // (which does not track gone/team/x) and call that the remote's branch.
    assert!(
        attached(&work).as_deref() != Some("team/x") || result.is_err(),
        "silently switched to the unrelated local branch team/x: {result:?}"
    );
}

/// A local branch of the same name that tracks a different upstream is not the
/// local twin of the remote row that was clicked.
#[test]
fn a_local_branch_tracking_a_different_remote_is_not_silently_taken_for_the_twin() {
    let (origin, _parent, work) = clone_with_remote_feature();
    let path = work.to_str().unwrap();
    git_in(
        &work,
        &["remote", "add", "upstream", origin.path().to_str().unwrap()],
    );
    git_in(&work, &["fetch", "-q", "upstream"]);
    git_in(&work, &["branch", "--track", "feature", "upstream/feature"]);
    let result = GitWriter::checkout_branch(path, "origin/feature");
    assert!(
        result.is_err() || attached(&work).as_deref() != Some("feature"),
        "origin/feature was answered by a `feature` that tracks upstream/feature"
    );
}
