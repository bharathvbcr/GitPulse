//! Repositories that are legal but unusual: a tag named like the branch, a
//! remote whose name contains a slash, a branch called `release/HEAD`, a
//! shallow clone. Each case runs the real engine over a real repository and
//! checks what the parsers concluded.

mod common;
use common::{run_git, trust_repo};
use gitpulse_lib::engine::{remotes, GitReader};
use gitpulse_lib::graph::{list_ref_decorations, LaneSolver, RefKind, RefScope};
use std::path::Path;

fn repo() -> tempfile::TempDir {
    common::unmetered_spawns();
    let dir = tempfile::tempdir().expect("tempdir");
    run_git(dir.path(), &["init", "-q", "-b", "main"]);
    run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
    run_git(dir.path(), &["commit", "-q", "--allow-empty", "-m", "init"]);
    trust_repo(dir.path());
    dir
}

fn path(dir: &Path) -> &str {
    dir.to_str().expect("utf8 path")
}

/// With branch `main` and tag `main`, `symbolic-ref --short HEAD` prints
/// `heads/main`, which matched no branch: the graph lost its HEAD chip.
#[test]
fn a_tag_named_like_the_branch_keeps_the_head_chip() {
    let dir = repo();
    run_git(dir.path(), &["tag", "main"]);

    let listing = list_ref_decorations(path(dir.path()), RefScope::Named).expect("decorations");
    let head: Vec<_> = listing
        .decorations
        .iter()
        .filter(|d| d.kind == RefKind::Local && d.is_head)
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(head, ["main"], "{:?}", listing.decorations);
    assert_eq!(
        GitReader::current_branch(path(dir.path())).expect("current branch"),
        Some("main".to_string())
    );
}

/// A remote named `team/fork` owns `refs/remotes/team/fork/main`; reading the
/// first path segment attributed it to a remote called `team`, which does not
/// exist.
#[test]
fn a_remote_with_a_slash_in_its_name_owns_its_branches() {
    let dir = repo();
    run_git(
        dir.path(),
        &["remote", "add", "team/fork", "/nonexistent/fork.git"],
    );
    run_git(
        dir.path(),
        &["update-ref", "refs/remotes/team/fork/main", "HEAD"],
    );

    let branches = GitReader::list_branches(path(dir.path())).expect("branches");
    let fork = branches
        .iter()
        .find(|b| b.is_remote && b.name == "team/fork/main")
        .unwrap_or_else(|| panic!("remote branch missing: {branches:?}"));
    assert_eq!(fork.remote_name.as_deref(), Some("team/fork"));

    let listing = list_ref_decorations(path(dir.path()), RefScope::Named).expect("decorations");
    let decoration = listing
        .decorations
        .iter()
        .find(|d| d.kind == RefKind::Remote && d.name == "team/fork/main")
        .expect("remote decoration");
    assert_eq!(decoration.remote.as_deref(), Some("team/fork"));

    let listed = remotes::list(path(dir.path())).expect("remotes");
    let count = |name: &str| {
        listed
            .remotes
            .iter()
            .find(|r| r.name == name)
            .map(|r| r.tracking_branches)
    };
    assert_eq!(count("team/fork"), Some(1));
    assert_eq!(count("team"), None, "a remote called `team` was invented");
}

/// Only a remote's own `HEAD` pointer is hidden. A branch called
/// `release/HEAD` ended in "/HEAD" and disappeared, locally and on a remote.
#[test]
fn a_branch_named_release_head_is_listed_and_the_remote_pointer_is_not() {
    let dir = repo();
    run_git(
        dir.path(),
        &["remote", "add", "origin", "/nonexistent/origin.git"],
    );
    run_git(
        dir.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    run_git(
        dir.path(),
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    run_git(
        dir.path(),
        &["update-ref", "refs/remotes/origin/release/HEAD", "HEAD"],
    );
    run_git(dir.path(), &["branch", "release/HEAD"]);

    let branches = GitReader::list_branches(path(dir.path())).expect("branches");
    let names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
    assert!(
        names.contains(&"release/HEAD"),
        "local branch hidden: {names:?}"
    );
    assert!(
        names.contains(&"origin/release/HEAD"),
        "remote branch hidden: {names:?}"
    );
    assert!(
        !names.contains(&"origin/HEAD"),
        "the remote pointer is not a branch: {names:?}"
    );

    let listing = list_ref_decorations(path(dir.path()), RefScope::Named).expect("decorations");
    let decorated: Vec<&str> = listing
        .decorations
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    assert!(decorated.contains(&"release/HEAD"), "{decorated:?}");
    assert!(decorated.contains(&"origin/release/HEAD"), "{decorated:?}");
    assert!(!decorated.contains(&"origin/HEAD"), "{decorated:?}");
}

/// A branch the writers would refuse to name in a command is still the branch
/// HEAD is on: the graph shows it.
#[test]
fn a_branch_with_parentheses_still_carries_the_head_chip() {
    let dir = repo();
    run_git(dir.path(), &["checkout", "-q", "-b", "fix(ui)"]);

    let listing = list_ref_decorations(path(dir.path()), RefScope::Named).expect("decorations");
    assert!(
        listing
            .decorations
            .iter()
            .any(|d| d.is_head && d.name == "fix(ui)"),
        "{:?}",
        listing.decorations
    );
}

/// A `--depth 1` clone's only commit has no parents in `%P`, which the lane
/// solver read as the root of the history. It is the edge of what was cloned:
/// drawn as a stub, never as a root.
#[test]
fn a_shallow_boundary_is_dangling_not_root() {
    let source = repo();
    run_git(
        source.path(),
        &["commit", "-q", "--allow-empty", "-m", "two"],
    );
    run_git(
        source.path(),
        &["commit", "-q", "--allow-empty", "-m", "three"],
    );
    let clone = tempfile::tempdir().expect("tempdir");
    let url = format!("file://{}", source.path().display());
    run_git(
        clone.path(),
        &["clone", "-q", "--depth", "1", &url, "shallow"],
    );
    let shallow = clone.path().join("shallow");
    trust_repo(&shallow);

    let commits =
        GitReader::read_commit_history_paged(path(&shallow), 0, 50, None, None, RefScope::Named)
            .expect("history");
    assert_eq!(commits.len(), 1, "a depth-1 clone holds one commit");
    let rows = LaneSolver::new(12).solve(&commits);
    assert!(!rows[0].is_root, "the shallow boundary was drawn as a root");
    assert!(
        rows[0].connections.iter().any(|c| c.is_dangling),
        "the cut-off parent must be a stub: {:?}",
        rows[0].connections
    );

    // A complete repository's real root is still a root.
    let full = GitReader::read_commit_history_paged(
        path(source.path()),
        0,
        50,
        None,
        None,
        RefScope::Named,
    )
    .expect("full history");
    let rows = LaneSolver::new(12).solve(&full);
    assert!(rows.last().unwrap().is_root);
}
