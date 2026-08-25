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
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].name, "v0.1.0");

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
    let lang_stats =
        GitReader::get_repo_language_stats(path).expect("get_repo_language_stats failed");
    assert!(!lang_stats.is_empty());
    assert_eq!(lang_stats[0].language, "Markdown");
}
