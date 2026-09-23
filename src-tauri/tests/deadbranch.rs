use gitpulse_lib::engine::deadbranch::{
    age_severity, clean_branches, create_backup, is_branch_merged_by_tree, list_backups,
    matches_glob, restore_backup, scan_stale_branches, DeadbranchConfig, StaleBranchInfo,
};
use gitpulse_lib::engine::git_cli::git_text;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

mod common;
use common::run_git;

struct TestRepo {
    dir: TempDir,
}

impl TestRepo {
    fn init() -> Self {
        let dir = TempDir::new().expect("tempdir");
        run_git(dir.path(), &["init", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test User"]);
        run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
        run_git(dir.path(), &["config", "core.autocrlf", "false"]);
        Self { dir }
    }

    fn path_str(&self) -> String {
        self.dir.path().to_string_lossy().into_owned()
    }

    fn write(&self, rel: &str, content: &str) {
        let dest = self.dir.path().join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(dest, content).unwrap();
    }

    fn commit_all(&self, message: &str) {
        run_git(self.dir.path(), &["add", "-A"]);
        run_git(self.dir.path(), &["commit", "-m", message]);
    }
}

#[test]
fn glob_matching_supports_prefixes_suffixes_and_wildcards() {
    assert!(matches_glob("wip/*", "wip/auth"));
    assert!(matches_glob("wip/*", "wip/sub/feature"));
    assert!(!matches_glob("wip/*", "feature/auth"));

    assert!(matches_glob("*/draft", "feature/draft"));
    assert!(matches_glob("feature/*/temp", "feature/user/temp"));
    assert!(!matches_glob("feature/*/temp", "feature/user/final"));

    assert!(matches_glob("main", "main"));
    assert!(!matches_glob("main", "maintainer"));
}

#[test]
fn age_severity_maps_correctly_to_tiers() {
    assert_eq!(age_severity(0), "fresh");
    assert_eq!(age_severity(30), "fresh");
    assert_eq!(age_severity(31), "moderate");
    assert_eq!(age_severity(90), "moderate");
    assert_eq!(age_severity(91), "stale");
    assert_eq!(age_severity(365), "stale");
}

#[test]
fn detect_squash_and_rebase_merges_via_merge_tree() {
    let repo = TestRepo::init();
    repo.write("base.txt", "Initial base commit\n");
    repo.commit_all("Initial commit on main");

    // Create a feature branch
    run_git(repo.dir.path(), &["checkout", "-b", "feature/login"]);
    repo.write("login.txt", "Login implementation\n");
    repo.commit_all("Add login form");

    // Checkout main and squash-merge the feature branch
    run_git(repo.dir.path(), &["checkout", "main"]);
    run_git(repo.dir.path(), &["merge", "--squash", "feature/login"]);
    run_git(
        repo.dir.path(),
        &["commit", "-m", "Squash: add login feature (#42)"],
    );

    // Standard git branch --merged does NOT report feature/login because commit ancestry was not linked
    let std_merged = git_text(repo.dir.path(), &["branch", "--merged", "main"]).unwrap();
    assert!(
        !std_merged.contains("feature/login"),
        "Standard git ancestry check incorrectly claims squash branch is merged: {std_merged}"
    );

    // Deep merge-tree comparison must detect that feature/login is fully incorporated
    let default_tree = git_text(repo.dir.path(), &["rev-parse", "main^{tree}"]).unwrap();
    let tree_merged = is_branch_merged_by_tree(
        repo.dir.path(),
        default_tree.trim(),
        "main",
        "feature/login",
    );
    assert_eq!(
        tree_merged,
        Some(true),
        "is_branch_merged_by_tree failed to detect squash-merged branch"
    );

    // scan_stale_branches must classify feature/login as is_merged: true and merged_by_tree: true
    let scan = scan_stale_branches(
        &repo.path_str(),
        &DeadbranchConfig {
            merged_only: false,
            days_threshold: 0,
            check_squash: true,
            ..Default::default()
        },
    )
    .expect("scan_stale_branches succeeded");

    let branch = scan
        .branches
        .iter()
        .find(|b| b.short_name == "feature/login")
        .expect("feature/login found in scan");

    assert!(branch.is_merged, "Branch should be marked is_merged");
    assert!(
        branch.merged_by_tree,
        "Branch should be marked merged_by_tree"
    );
}

#[test]
fn backup_creation_and_restoration_roundtrip() {
    let repo = TestRepo::init();
    repo.write("seed.txt", "Initial commit\n");
    repo.commit_all("Seed");

    // Create branch to backup and delete
    run_git(repo.dir.path(), &["checkout", "-b", "feature/legacy"]);
    repo.write("legacy.txt", "legacy code\n");
    repo.commit_all("Legacy commit");
    let tip_sha = git_text(repo.dir.path(), &["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    run_git(repo.dir.path(), &["checkout", "main"]);

    // Set custom backup directory for the test
    let temp_backups = TempDir::new().unwrap();
    std::env::set_var(
        "GITPULSE_BACKUPS_DIR",
        temp_backups.path().to_str().unwrap(),
    );

    let branch_info = StaleBranchInfo {
        name: "feature/legacy".to_string(),
        short_name: "feature/legacy".to_string(),
        age_days: 100,
        severity: "stale".to_string(),
        is_merged: true,
        merged_by_tree: false,
        is_remote: false,
        is_protected: false,
        is_wip: false,
        is_current_or_worktree: false,
        last_commit_sha: tip_sha.clone(),
        last_commit_timestamp: 1000000,
        last_author: "Dev".to_string(),
        last_summary: "Legacy commit".to_string(),
    };

    // Create backup file
    let backup_path = create_backup(&repo.path_str(), &[branch_info]).unwrap();
    assert!(Path::new(&backup_path).is_file());

    let content = fs::read_to_string(&backup_path).unwrap();
    assert!(content.contains("git branch feature/legacy "));
    assert!(content.contains(&tip_sha));

    // Delete the branch using clean_branches
    let clean = clean_branches(
        &repo.path_str(),
        &["feature/legacy".to_string()],
        true,
        false,
    )
    .unwrap();
    assert_eq!(clean.deleted, vec!["feature/legacy"]);

    // Verify branch is gone
    let list = git_text(repo.dir.path(), &["branch", "--list", "feature/legacy"]).unwrap();
    assert!(!list.contains("feature/legacy"));

    // List backups
    let backups = list_backups(&repo.path_str()).unwrap();
    assert!(!backups.is_empty());
    assert_eq!(backups[0].branch_count, 1);

    // Restore branch
    let restored = restore_backup(&repo.path_str(), &backup_path, None).unwrap();
    assert_eq!(restored.restored, vec!["feature/legacy"]);

    // Verify branch is restored and points to exact tip SHA
    let restored_sha = git_text(repo.dir.path(), &["rev-parse", "refs/heads/feature/legacy"])
        .unwrap()
        .trim()
        .to_string();
    assert_eq!(restored_sha, tip_sha);

    std::env::remove_var("GITPULSE_BACKUPS_DIR");
}

#[test]
fn safety_guards_protect_default_branch_and_wip() {
    let repo = TestRepo::init();
    repo.write("seed.txt", "Initial\n");
    repo.commit_all("Seed");

    run_git(repo.dir.path(), &["checkout", "-b", "wip/experiment"]);
    repo.write("exp.txt", "experiment\n");
    repo.commit_all("Exp");

    run_git(repo.dir.path(), &["checkout", "main"]);

    // Scan should detect wip/experiment as is_wip: true and main as is_protected: true
    let scan = scan_stale_branches(
        &repo.path_str(),
        &DeadbranchConfig {
            merged_only: false,
            days_threshold: 0,
            ..Default::default()
        },
    )
    .unwrap();

    let wip = scan
        .branches
        .iter()
        .find(|b| b.short_name == "wip/experiment")
        .unwrap();
    assert!(wip.is_wip);

    // Attempting to clean the default branch "main" must be rejected
    let err = clean_branches(&repo.path_str(), &["main".to_string()], true, false);
    assert!(err.is_err());
    assert!(err.unwrap_err().contains("default branch"));
}
