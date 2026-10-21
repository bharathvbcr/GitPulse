//! The argv the command gate judges equals the argv `push_planned` runs.

use super::*;
use crate::test_support::{git_in, git_repo};

fn repo_with_remote(name: &str) -> (tempfile::TempDir, String) {
    let repo = git_repo();
    git_in(repo.path(), &["commit", "--allow-empty", "-q", "-m", "A"]);
    git_in(
        repo.path(),
        &["remote", "add", name, "https://example.invalid/x.git"],
    );
    let path = repo.path().to_string_lossy().into_owned();
    (repo, path)
}

fn assert_parity(
    path: &str,
    remote: Option<&str>,
    branch: Option<&str>,
    force: bool,
) -> Vec<String> {
    let args = GitWriter::plan_push(path, remote, branch, force).unwrap();
    let judged = judged_push_argv(&args);
    assert_eq!(judged[0], "git");
    assert_eq!(
        &judged[1..],
        &args.iter().map(String::as_str).collect::<Vec<_>>()[..]
    );
    assert_eq!(judged.contains(&"--force-with-lease"), force, "{judged:?}");
    assert_eq!(judged.contains(&"--force-if-includes"), force, "{judged:?}");
    args
}

#[test]
fn adv_l1_judged_equals_run_for_force_with_and_without_u_and_explicit_and_slashed() {
    let (_repo, path) = repo_with_remote("origin");
    // Unpublished branch, nothing named: the planner adds `-u origin main`.
    let unpublished = assert_parity(&path, None, None, true);
    assert!(unpublished.contains(&"-u".to_string()), "{unpublished:?}");
    assert_parity(&path, None, None, false);
    assert_parity(&path, Some("origin"), Some("main"), true);
    assert_parity(&path, Some("origin"), None, true);
    assert_parity(&path, None, Some("feat/x"), true);

    // Published: no `-u`.
    git_in(_repo.path(), &["config", "branch.main.remote", "origin"]);
    git_in(
        _repo.path(),
        &["config", "branch.main.merge", "refs/heads/main"],
    );
    git_in(
        _repo.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    let published = assert_parity(&path, None, None, true);
    assert!(!published.contains(&"-u".to_string()), "{published:?}");

    let (_slashed, slashed_path) = repo_with_remote("team/fork");
    let args = assert_parity(&slashed_path, Some("team/fork"), Some("main"), true);
    assert!(args.contains(&"team/fork".to_string()), "{args:?}");
}
