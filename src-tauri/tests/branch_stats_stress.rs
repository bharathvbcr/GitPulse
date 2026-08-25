use gitpulse_lib::engine::{BranchInfo, GitReader};
use std::path::Path;
use std::process::Command as StdCommand;
use tempfile::TempDir;

fn run_git(dir: &Path, args: &[&str]) {
    let out = StdCommand::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    run_git(dir.path(), &["init", "-q", "-b", "main"]);
    run_git(dir.path(), &["config", "user.email", "stress@gitpulse.dev"]);
    run_git(dir.path(), &["config", "user.name", "Stress Tester"]);
    dir
}

fn commit_file(dir: &Path, name: &str, contents: &str, message: &str) {
    std::fs::write(dir.join(name), contents).expect("write file");
    run_git(dir, &["add", "--", name]);
    run_git(dir, &["commit", "-q", "-m", message]);
}

fn find<'a>(branches: &'a [BranchInfo], name: &str) -> &'a BranchInfo {
    branches
        .iter()
        .find(|b| b.name == name)
        .unwrap_or_else(|| panic!("branch {name} missing"))
}

/// ~120 local branches with divergent commits: list_branches must return
/// fully-shaped data with correct ahead/behind vs main in one for-each-ref,
/// and the stats command must cap, then serve repeats from the cache.
#[test]
fn stress_list_and_stats_over_120_divergent_branches() {
    let repo = init_repo();
    let path = repo.path().to_str().unwrap();
    commit_file(repo.path(), "base.txt", "one\ntwo\nthree\n", "base");

    for i in 0..120 {
        let name = format!("feat-{i:03}");
        run_git(repo.path(), &["checkout", "-q", "-b", &name]);
        if i % 3 == 0 {
            let body = format!("one\ntwo\nthree\nextra-{i}\n");
            commit_file(repo.path(), "base.txt", &body, &format!("diverge {i}"));
        }
        run_git(repo.path(), &["checkout", "-q", "main"]);
    }

    let branches = GitReader::list_branches(path).expect("list_branches");
    assert_eq!(branches.len(), 121);
    let main = find(&branches, "main");
    assert!(main.is_default);
    assert_eq!(
        (main.commits_ahead_of_base, main.commits_behind_base),
        (0, 0)
    );
    for i in 0..120 {
        let b = find(&branches, &format!("feat-{i:03}"));
        assert_eq!(
            b.compared_to.as_deref(),
            Some("main"),
            "compared_to feat-{i:03}"
        );
        let expected_ahead = if i % 3 == 0 { 1 } else { 0 };
        assert_eq!(b.commits_ahead_of_base, expected_ahead, "ahead feat-{i:03}");
        assert_eq!(b.commits_behind_base, 0, "behind feat-{i:03}");
        assert_eq!(b.tip_commit_id.len(), 40, "tip feat-{i:03}");
        assert!(!b.last_author.is_empty(), "author feat-{i:03}");
        assert!(!b.last_summary.is_empty(), "summary feat-{i:03}");
    }

    // Stats pass 1: cap engaged, every eligible branch accounted for. Branches
    // sharing a tip oid may be computed concurrently on the cold pass, so the
    // computed/cached split is not deterministic — the total is.
    let first = GitReader::branch_stats(path).expect("branch_stats");
    assert!(first.capped, ">96 eligible branches must trip the cap");
    assert_eq!(first.compared_to, "main");
    assert_eq!(first.updates.len(), 96);
    assert_eq!(
        first.computed + first.cached,
        96,
        "computed {} + cached {} must cover every update",
        first.computed,
        first.cached
    );

    // Divergent branches carry real churn; pointer-only ones stay zero.
    let divergent = first
        .updates
        .iter()
        .find(|u| u.name == "feat-000")
        .expect("feat-000 update");
    assert_eq!(divergent.files_changed, 1);
    assert_eq!(divergent.commits_ahead_of_base, 1);
    assert!(divergent.additions > 0);
    let plain = first
        .updates
        .iter()
        .find(|u| u.name == "feat-001")
        .expect("feat-001 update");
    assert_eq!(
        (
            plain.additions,
            plain.deletions,
            plain.commits_ahead_of_base
        ),
        (0, 0, 0)
    );

    // Stats pass 2: fully served from the oid-keyed cache, identical payload.
    let second = GitReader::branch_stats(path).expect("branch_stats 2");
    assert_eq!(second.computed, 0);
    assert_eq!(second.cached, 96);
    assert_eq!(
        serde_json::to_string(&first.updates).unwrap(),
        serde_json::to_string(&second.updates).unwrap()
    );
}

/// Unicode and slash-heavy ref names survive both commands intact.
#[test]
fn unicode_and_slashed_branch_names_round_trip() {
    let repo = init_repo();
    let path = repo.path().to_str().unwrap();
    commit_file(repo.path(), "readme.md", "# hi\n", "base");
    for name in ["feature/ünïcode/naïve", "release/v1.0"] {
        run_git(repo.path(), &["checkout", "-q", "-b", name]);
        commit_file(
            repo.path(),
            "readme.md",
            &format!("# hi\n{name}\n"),
            &format!("work on {name}"),
        );
        run_git(repo.path(), &["checkout", "-q", "main"]);
    }

    let branches = GitReader::list_branches(path).expect("list_branches");
    assert_eq!(branches.len(), 3);
    for name in ["feature/ünïcode/naïve", "release/v1.0"] {
        let b = find(&branches, name);
        assert_eq!(b.compared_to.as_deref(), Some("main"), "compared_to {name}");
        assert_eq!(b.commits_ahead_of_base, 1, "ahead {name}");
        assert_eq!(b.files_changed, 0, "no churn on the fast path");
    }

    let report = GitReader::branch_stats(path).expect("branch_stats");
    assert!(!report.capped);
    assert_eq!(report.updates.len(), 2);
    for name in ["feature/ünïcode/naïve", "release/v1.0"] {
        let update = report
            .updates
            .iter()
            .find(|u| u.name == name)
            .unwrap_or_else(|| panic!("update for {name} missing"));
        assert_eq!(update.files_changed, 1, "{name}");
        assert_eq!(update.commits_ahead_of_base, 1, "{name}");
        assert!(update.additions > 0, "{name}");
    }
}

/// `git init` only: no refs anywhere, nothing to compute, nothing panics.
#[test]
fn empty_repo_yields_empty_results_without_panicking() {
    let repo = init_repo();
    let path = repo.path().to_str().unwrap();

    let branches = GitReader::list_branches(path).expect("list_branches");
    assert!(branches.is_empty());

    let report = GitReader::branch_stats(path).expect("branch_stats");
    assert!(report.updates.is_empty());
    assert_eq!(
        (report.computed, report.cached, report.capped),
        (0, 0, false)
    );
}

/// Detached HEAD: no ref is current, listing and stats still work.
#[test]
fn detached_head_repo_still_lists_and_computes() {
    let repo = init_repo();
    let path = repo.path().to_str().unwrap();
    commit_file(repo.path(), "a.txt", "x\n", "first");
    run_git(repo.path(), &["checkout", "-q", "--detach"]);

    let branches = GitReader::list_branches(path).expect("list_branches");
    assert_eq!(branches.len(), 1);
    assert!(branches.iter().all(|b| !b.is_current));
    assert_eq!(branches[0].compared_to.as_deref(), Some("main"));

    // Only the default branch exists, so nothing is eligible for churn.
    let report = GitReader::branch_stats(path).expect("branch_stats");
    assert!(report.updates.is_empty());
    assert_eq!(report.computed, 0);
}

/// A force-moved branch gets a new tip oid, which misses the cache and
/// recomputes different churn; the pre-move result stays untouched.
#[test]
fn force_moved_branch_recomputes_under_new_tip() {
    let repo = init_repo();
    let path = repo.path().to_str().unwrap();
    commit_file(repo.path(), "main.txt", "main\n", "base");
    run_git(repo.path(), &["checkout", "-q", "-b", "topic"]);
    commit_file(repo.path(), "feature.txt", "l1\nl2\nl3\n", "topic work");

    let first = GitReader::branch_stats(path).expect("stats 1");
    let before = first
        .updates
        .iter()
        .find(|u| u.name == "topic")
        .expect("topic in pass 1");
    assert_eq!(before.additions, 3);
    let old_tip = before.tip_commit_id.clone();

    run_git(repo.path(), &["checkout", "-q", "main"]);
    commit_file(repo.path(), "main.txt", "main\nmore\n", "advance main");
    run_git(repo.path(), &["checkout", "-q", "-B", "topic"]);

    let second = GitReader::branch_stats(path).expect("stats 2");
    assert!(
        second.computed >= 1,
        "moved tip must miss the content-addressed cache"
    );
    let after = second
        .updates
        .iter()
        .find(|u| u.name == "topic")
        .expect("topic in pass 2");
    assert_ne!(after.tip_commit_id, old_tip);
    // topic now points at the new main tip: zero churn vs base.
    assert_eq!(
        (
            after.additions,
            after.deletions,
            after.commits_ahead_of_base,
            after.commits_behind_base
        ),
        (0, 0, 0, 0)
    );

    let third = GitReader::branch_stats(path).expect("stats 3");
    assert_eq!(third.computed, 0);
    assert_eq!(
        serde_json::to_string(&second.updates).unwrap(),
        serde_json::to_string(&third.updates).unwrap()
    );
}

/// Remote-only checkout (remote-tracking refs, no local heads): listing works
/// through the origin/HEAD fallback and stats compare against the remote tip.
#[test]
fn remote_only_repo_lists_and_computes_against_remote_tip() {
    let upstream = init_repo();
    commit_file(upstream.path(), "src.txt", "a\n", "seed");

    let clone = TempDir::new().expect("clone tempdir");
    run_git(
        upstream.path(),
        &[
            "clone",
            "-q",
            upstream.path().to_str().unwrap(),
            clone.path().to_str().unwrap(),
        ],
    );
    // Strip the local head so only refs/remotes/* remain.
    run_git(clone.path(), &["checkout", "-q", "--detach", "origin/main"]);
    run_git(clone.path(), &["branch", "-qD", "main"]);

    let path = clone.path().to_str().unwrap();
    let branches = GitReader::list_branches(path).expect("list_branches");
    assert_eq!(branches.len(), 1);
    let origin_main = &branches[0];
    assert!(origin_main.is_remote);
    assert_eq!(origin_main.remote_name.as_deref(), Some("origin"));
    assert_eq!(origin_main.compared_to.as_deref(), Some("main"));

    let report = GitReader::branch_stats(path).expect("branch_stats");
    assert_eq!(report.compared_to, "main");
    assert_eq!(report.updates.len(), 1);
    let update = &report.updates[0];
    assert_eq!(update.name, "origin/main");
    assert!(update.is_remote);
    assert_eq!(
        (
            update.additions,
            update.deletions,
            update.commits_ahead_of_base,
            update.commits_behind_base
        ),
        (0, 0, 0, 0)
    );
}
