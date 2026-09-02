use gitpulse_lib::engine::{GitReader, GitWriter};
use std::fs::File;
use std::io::Write;
use std::process::Command;
use tempfile::TempDir;

fn create_temp_git_repo() -> TempDir {
    let dir = TempDir::new().expect("Failed to create tempdir");
    let path = dir.path().to_str().unwrap();

    let init = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(path)
        .output()
        .expect("git init failed");
    assert!(init.status.success());

    let config_email = Command::new("git")
        .args(["config", "user.email", "test@gitpulse.dev"])
        .current_dir(path)
        .output()
        .expect("git config email failed");
    assert!(config_email.status.success());

    let config_name = Command::new("git")
        .args(["config", "user.name", "GitPulse Tester"])
        .current_dir(path)
        .output()
        .expect("git config name failed");
    assert!(config_name.status.success());

    dir
}

#[test]
fn test_git_workflow_lifecycle() {
    let repo = create_temp_git_repo();
    let path = repo.path().to_str().unwrap();

    // 1. Create a file and verify status
    let file_path = repo.path().join("README.md");
    let mut file = File::create(&file_path).unwrap();
    writeln!(file, "# GitPulse Test Repository").unwrap();

    let statuses = GitReader::get_status(path).expect("get_status failed");
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].path, "README.md");
    assert!(!statuses[0].is_staged);

    // 2. Stage file
    GitWriter::stage_file(path, "README.md").expect("stage_file failed");
    let staged_statuses = GitReader::get_status(path).expect("get_status after stage failed");
    assert_eq!(staged_statuses.len(), 1);
    assert!(staged_statuses[0].is_staged);

    // 3. Commit
    let commit_res =
        GitWriter::commit(path, "feat: initial commit for testing", false).expect("commit failed");
    assert!(!commit_res.is_empty());

    // 4. List branches
    let branches = GitReader::list_branches(path).expect("list_branches failed");
    assert_eq!(branches.len(), 1);
    assert_eq!(branches[0].name, "main");
    assert!(branches[0].is_current);

    // 5. Create new branch & checkout
    GitWriter::create_branch(path, "feature/awesome", None).expect("create_branch failed");
    let branches_after_create = GitReader::list_branches(path).expect("list_branches failed");
    assert_eq!(branches_after_create.len(), 2);

    GitWriter::checkout_branch(path, "feature/awesome").expect("checkout failed");
    let branches_after_checkout = GitReader::list_branches(path).expect("list_branches failed");
    let current_b = branches_after_checkout
        .iter()
        .find(|b| b.is_current)
        .unwrap();
    assert_eq!(current_b.name, "feature/awesome");

    // 6. Create tags
    GitWriter::create_tag(path, "v0.1.0", None, Some("Release v0.1.0")).expect("create_tag failed");
    let tags = GitReader::list_tags(path).expect("list_tags failed");
    assert!(!tags.truncated);
    assert_eq!(tags.tags.len(), 1);
    assert_eq!(tags.tags[0].name, "v0.1.0");

    // 7. Verify reflog
    let reflog = GitReader::get_reflog(path, 10).expect("get_reflog failed");
    assert!(!reflog.is_empty());

    // 8. Commit details
    let head_commit = &branches_after_checkout[0].tip_commit_id;
    let details =
        GitReader::get_commit_details(path, head_commit).expect("get_commit_details failed");
    assert_eq!(details.author_name, "GitPulse Tester");
    assert_eq!(details.summary, "feat: initial commit for testing");

    // 9. Language stats
    let lang_stats = GitReader::get_repo_language_stats(path)
        .expect("get_repo_language_stats failed")
        .stats;
    assert!(!lang_stats.is_empty());
    assert_eq!(lang_stats[0].language, "Markdown");
}

/// Regression: porcelain v1 `-z` emits renames as `<new>\0<old>\0` —
/// post-image first. The parser historically swapped them, which made
/// staging, diffing and numstat lookups target the wrong path.
#[test]
fn test_status_rename_reports_new_path_first() {
    let repo = create_temp_git_repo();
    let path = repo.path().to_str().unwrap();

    let file_path = repo.path().join("feature.rs");
    std::fs::write(&file_path, "fn main() {}\nfn helper() {}\nfn third() {}\n").unwrap();
    GitWriter::stage_file(path, "feature.rs").unwrap();
    GitWriter::commit(path, "feat: seed", false).unwrap();

    std::fs::rename(
        repo.path().join("feature.rs"),
        repo.path().join("renamed.rs"),
    )
    .unwrap();
    // Extend the renamed file so numstat reports real +/- counts for it —
    // a content-identical rename legitimately shows 0 additions / 0 deletions.
    std::fs::write(
        repo.path().join("renamed.rs"),
        "fn main() {}\nfn helper() {}\nfn third() {}\nfn fourth() {}\n",
    )
    .unwrap();
    GitWriter::stage_file(path, "renamed.rs").unwrap();
    // Stage the old side's deletion too: a rename becomes one `R` record
    // only when both halves are in the same (index) diff scope.
    GitWriter::stage_file(path, "feature.rs").unwrap();

    let statuses = GitReader::get_status(path).expect("get_status failed");
    assert_eq!(statuses.len(), 1, "a pure rename is one status entry");
    let entry = &statuses[0];
    assert!(
        entry.status_code.contains('R'),
        "expected rename code, got {}",
        entry.status_code
    );
    assert_eq!(entry.path, "renamed.rs", "path must be the post-image");
    assert_eq!(
        entry.old_path.as_deref(),
        Some("feature.rs"),
        "old_path must be the pre-image"
    );

    // The +/- counts come from `--numstat -z` keyed by the new path; a
    // mis-keyed lookup silently zeroes them.
    assert!(
        entry.additions > 0 || entry.deletions > 0,
        "rename counts must not be lost"
    );
}

/// Regression: a tracked file literally named with an arrow must not be
/// mistaken for a rename record, and directory renames plus unicode names
/// must round-trip through the `-z` numstat parser unharmed.
#[test]
fn test_status_exotic_paths_survive_parsing() {
    let repo = create_temp_git_repo();
    let path = repo.path().to_str().unwrap();

    std::fs::create_dir_all(repo.path().join("dir/old")).unwrap();
    std::fs::write(
        repo.path().join("dir/old/mod.txt"),
        "one\ntwo\nthree\nfour\nfive\n",
    )
    .unwrap();
    std::fs::write(repo.path().join("a -> b.txt"), "arrow name\nsecond line\n").unwrap();
    std::fs::write(repo.path().join("ünïcodé.txt"), "unicode\ncontents\n").unwrap();
    GitWriter::stage_file(path, "dir/old/mod.txt").unwrap();
    GitWriter::stage_file(path, "a -> b.txt").unwrap();
    GitWriter::stage_file(path, "ünïcodé.txt").unwrap();
    GitWriter::commit(path, "feat: exotic seeds", false).unwrap();

    std::fs::create_dir_all(repo.path().join("dir/new")).unwrap();
    std::fs::rename(
        repo.path().join("dir/old/mod.txt"),
        repo.path().join("dir/new/mod.txt"),
    )
    .unwrap();
    GitWriter::stage_file(path, "dir/new/mod.txt").unwrap();
    GitWriter::stage_file(path, "dir/old/mod.txt").unwrap();

    std::fs::write(repo.path().join("a -> b.txt"), "arrow name\nCHANGED\n").unwrap();
    std::fs::write(repo.path().join("ünïcodé.txt"), "unicode\nMODIFIED\n").unwrap();

    let statuses = GitReader::get_status(path).expect("get_status failed");
    let by_path: std::collections::HashMap<_, _> =
        statuses.iter().map(|s| (s.path.as_str(), s)).collect();

    let arrow = by_path
        .get("a -> b.txt")
        .expect("arrow-named file must parse as one plain entry");
    assert_eq!(
        arrow.old_path, None,
        "an arrow in a filename is not a rename"
    );
    assert_eq!(
        (arrow.additions, arrow.deletions),
        (1, 1),
        "numstat must key the raw name"
    );

    let uni = by_path
        .get("ünïcodé.txt")
        .expect("unicode name must survive");
    assert_eq!(uni.old_path, None);
    assert_eq!((uni.additions, uni.deletions), (1, 1));

    let moved = by_path
        .get("dir/new/mod.txt")
        .expect("directory rename keyed by new path");
    assert_eq!(moved.old_path.as_deref(), Some("dir/old/mod.txt"));
}

/// Regression: language stats used to read each tracked file fully into
/// memory before checking the size budget. A sparse multi-gigabyte tracked
/// file must be skipped via stat, never read.
#[test]
fn test_language_stats_skip_oversized_files_without_reading() {
    let repo = create_temp_git_repo();
    let path = repo.path().to_str().unwrap();

    std::fs::write(repo.path().join("small.rs"), "fn a() {}\n").unwrap();
    let huge = File::create(repo.path().join("huge.rs")).unwrap();
    huge.set_len(3 * 1024 * 1024 * 1024).unwrap(); // sparse: 3 GiB on disk usage ~0
    drop(huge);
    GitWriter::stage_file(path, "small.rs").unwrap();
    GitWriter::stage_file(path, "huge.rs").unwrap();
    GitWriter::commit(path, "feat: sizes", false).unwrap();

    let started = std::time::Instant::now();
    let stats = GitReader::get_repo_language_stats(path)
        .expect("language stats failed")
        .stats;
    let elapsed = started.elapsed();

    let names: Vec<_> = stats.iter().map(|s| s.language.as_str()).collect();
    assert!(!names.is_empty(), "the small file must still be counted");
    // A sparse 3 GiB file read into memory would take far longer than a
    // second even on fast hardware; the budget skip makes this instant.
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "oversized files must be stat-skipped, took {elapsed:?}"
    );
}
