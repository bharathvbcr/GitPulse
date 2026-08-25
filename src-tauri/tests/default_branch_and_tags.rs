use gitpulse_lib::engine::GitReader;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

struct TestRepo {
    dir: TempDir,
}

impl TestRepo {
    fn init() -> Self {
        let dir = TempDir::new().expect("tempdir");
        run_git(dir.path(), &["init", "-b", "release"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test User"]);
        run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
        Self { dir }
    }

    fn path_str(&self) -> String {
        self.dir.path().to_string_lossy().into_owned()
    }

    fn commit_all(&self, message: &str) {
        run_git(self.dir.path(), &["add", "-A"]);
        run_git(self.dir.path(), &["commit", "--allow-empty", "-m", message]);
    }
}

fn run_git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Adds a remote (config only, no network) and points its HEAD symref at
/// `target` via a hand-built remote-tracking ref.
fn seed_remote_head(repo: &TestRepo, remote: &str, target: &str) {
    run_git(
        repo.dir.path(),
        &[
            "remote",
            "add",
            remote,
            &format!("https://example.com/{remote}.git"),
        ],
    );
    let head = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo.dir.path())
            .output()
            .expect("rev-parse")
            .stdout,
    )
    .unwrap();
    let head = head.trim();
    let tracking = format!("refs/remotes/{remote}/{target}");
    run_git(repo.dir.path(), &["update-ref", &tracking, head]);
    run_git(
        repo.dir.path(),
        &[
            "symbolic-ref",
            &format!("refs/remotes/{remote}/HEAD"),
            &tracking,
        ],
    );
}

fn default_branch<'a>(branches: &'a [gitpulse_lib::engine::BranchInfo]) -> &'a str {
    branches
        .iter()
        .filter(|b| b.is_default)
        .map(|b| b.name.as_str())
        .next()
        .unwrap_or("<none>")
}

#[test]
fn primary_remote_other_than_origin_sets_default_branch() {
    let repo = TestRepo::init();
    repo.commit_all("c1");
    run_git(repo.dir.path(), &["branch", "dev"]);
    seed_remote_head(&repo, "upstream", "release");

    // Neither `release` nor `dev` matches the main/master guess list, so the
    // default must come from upstream/HEAD.
    let branches = GitReader::list_branches(&repo.path_str()).expect("branches");
    assert_eq!(default_branch(&branches), "release");
    assert_eq!(
        branches
            .iter()
            .find(|b| b.name == "release")
            .and_then(|b| b.compared_to.as_deref()),
        Some("release")
    );
}

#[test]
fn checkout_default_remote_config_wins_over_guesses() {
    let repo = TestRepo::init();
    repo.commit_all("c1");
    run_git(repo.dir.path(), &["branch", "dev"]);
    seed_remote_head(&repo, "alpha", "release");
    seed_remote_head(&repo, "beta", "prod");
    run_git(
        repo.dir.path(),
        &["config", "checkout.defaultRemote", "beta"],
    );

    let branches = GitReader::list_branches(&repo.path_str()).expect("branches");
    assert_eq!(default_branch(&branches), "prod");

    let stats = GitReader::branch_stats(&repo.path_str()).expect("stats");
    assert_eq!(stats.compared_to, "prod");
}

#[test]
fn current_branch_upstream_remote_used_when_checkout_config_unset() {
    let repo = TestRepo::init();
    repo.commit_all("c1");
    run_git(repo.dir.path(), &["branch", "dev"]);
    seed_remote_head(&repo, "alpha", "release");
    seed_remote_head(&repo, "beta", "prod");
    run_git(
        repo.dir.path(),
        &["config", "branch.release.remote", "beta"],
    );

    let branches = GitReader::list_branches(&repo.path_str()).expect("branches");
    assert_eq!(default_branch(&branches), "prod");
}

#[test]
fn lone_remote_names_default_head_without_any_config() {
    let repo = TestRepo::init();
    repo.commit_all("c1");
    run_git(repo.dir.path(), &["branch", "dev"]);
    seed_remote_head(&repo, "company", "release");

    let branches = GitReader::list_branches(&repo.path_str()).expect("branches");
    assert_eq!(default_branch(&branches), "release");

    let stats = GitReader::branch_stats(&repo.path_str()).expect("stats");
    assert_eq!(stats.compared_to, "release");
}

#[test]
fn tags_are_newest_first_by_creatordate() {
    let repo = TestRepo::init();
    repo.write("f.txt", "x\n");
    repo.commit_all("c1");

    for (name, date) in [
        ("v1-old", "2026-01-15T12:00:00 +0000"),
        ("v2-mid", "2026-02-15T12:00:00 +0000"),
        ("v3-new", "2026-03-15T12:00:00 +0000"),
    ] {
        let output = Command::new("git")
            .args(["tag", "-a", name, "-m", name])
            .current_dir(repo.dir.path())
            .env("GIT_COMMITTER_DATE", date)
            .output()
            .expect("spawn git tag");
        assert!(
            output.status.success(),
            "git tag {name} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let tags = GitReader::list_tags(&repo.path_str()).expect("tags");
    let names: Vec<&str> = tags.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["v3-new", "v2-mid", "v1-old"]);
}
