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
    run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
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

    // Unique tips here are well under the 96 compute budget (every 3rd branch
    // diverges; the rest share main's tip), so one call covers every eligible
    // branch. Duplicate tips are one compute fanned out to every name.
    let first = GitReader::branch_stats(path).expect("branch_stats");
    assert!(!first.capped, "41 unique tips must fit the compute budget");
    assert_eq!(first.compared_to, "main");
    assert_eq!(first.updates.len(), 120);
    assert_eq!(
        first.computed + first.cached,
        120,
        "computed {} + cached {} must cover every update",
        first.computed,
        first.cached
    );
    assert_eq!(first.computed, 120);
    assert_eq!(first.cached, 0);

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
    assert_eq!(second.cached, 120);
    assert_eq!(
        serde_json::to_string(&first.updates).unwrap(),
        serde_json::to_string(&second.updates).unwrap()
    );
}

fn rev_parse(dir: &Path, rev: &str) -> String {
    let out = StdCommand::new("git")
        .args(["rev-parse", rev])
        .current_dir(dir)
        .output()
        .expect("rev-parse");
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn create_refs(dir: &Path, specs: impl IntoIterator<Item = (String, String)>) {
    use std::io::Write;
    let mut child = StdCommand::new("git")
        .args(["update-ref", "--stdin"])
        .current_dir(dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn update-ref");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        for (name, oid) in specs {
            writeln!(stdin, "create refs/heads/{name} {oid}").unwrap();
        }
    }
    let out = child.wait_with_output().expect("wait update-ref");
    assert!(
        out.status.success(),
        "update-ref failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// 500 branches sharing 8 unique tips: one stats call, exact cache-hit ratio
/// on the repeat, no extra git walks for duplicates.
#[test]
fn stress_500_branches_duplicate_tips_exact_cache_ratio() {
    let repo = init_repo();
    let path = repo.path().to_str().unwrap();
    commit_file(repo.path(), "base.txt", "base\n", "base");

    let mut oids = Vec::new();
    for i in 0..8 {
        let name = format!("seed-{i}");
        run_git(repo.path(), &["checkout", "-q", "-b", &name]);
        let body = format!("{}\n", "x\n".repeat(i + 1));
        commit_file(
            repo.path(),
            &format!("f-{i}.txt"),
            &body,
            &format!("seed {i}"),
        );
        oids.push(rev_parse(repo.path(), "HEAD"));
        run_git(repo.path(), &["checkout", "-q", "main"]);
    }

    create_refs(
        repo.path(),
        (0..492).map(|i| (format!("dup-{i:03}"), oids[i % 8].clone())),
    );

    let listed = GitReader::list_branches(path).expect("list");
    assert_eq!(listed.len(), 1 + 8 + 492);

    let first = GitReader::branch_stats(path).expect("stats 1");
    assert!(!first.capped, "8 unique tips must not trip the compute cap");
    assert_eq!(first.updates.len(), 500);
    assert_eq!(first.computed, 500);
    assert_eq!(first.cached, 0);
    let seed0 = first
        .updates
        .iter()
        .find(|u| u.name == "seed-0")
        .expect("seed-0");
    assert!(seed0.additions > 0);
    let matching: Vec<_> = first
        .updates
        .iter()
        .filter(|u| u.tip_commit_id == seed0.tip_commit_id)
        .collect();
    assert!(matching.len() > 50);
    assert!(matching.iter().all(|u| u.additions == seed0.additions));

    let second = GitReader::branch_stats(path).expect("stats 2");
    assert_eq!(second.computed, 0);
    assert_eq!(second.cached, 500);
    assert!(!second.capped);
    assert_eq!(
        serde_json::to_string(&first.updates).unwrap(),
        serde_json::to_string(&second.updates).unwrap()
    );
}

/// More unique tips than the compute budget: first call caps, second drains
/// the remainder while returning already-cached tips alongside.
#[test]
fn unique_tips_above_budget_cap_then_drain() {
    let repo = init_repo();
    let path = repo.path().to_str().unwrap();
    commit_file(repo.path(), "base.txt", "base\n", "base");
    let base_oid = rev_parse(repo.path(), "HEAD");

    let mut specs = Vec::new();
    for i in 0..110 {
        run_git(
            repo.path(),
            &["commit", "-q", "--allow-empty", "-m", &format!("c-{i}")],
        );
        specs.push((format!("uniq-{i:03}"), rev_parse(repo.path(), "HEAD")));
    }
    run_git(repo.path(), &["reset", "-q", "--hard", &base_oid]);
    create_refs(repo.path(), specs);

    let first = GitReader::branch_stats(path).expect("stats 1");
    assert!(first.capped);
    assert_eq!(first.updates.len(), 96);
    assert_eq!(first.computed, 96);
    assert_eq!(first.cached, 0);

    let second = GitReader::branch_stats(path).expect("stats 2");
    assert!(!second.capped);
    assert_eq!(second.updates.len(), 110);
    assert_eq!(second.cached, 96);
    assert_eq!(second.computed, 14);

    let third = GitReader::branch_stats(path).expect("stats 3");
    assert_eq!(third.computed, 0);
    assert_eq!(third.cached, 110);
    assert!(!third.capped);
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
