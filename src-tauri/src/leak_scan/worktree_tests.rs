//! The working-tree scan against real repositories: what it reads, what it
//! leaves to the ignore rules, and that every input it could not read is a
//! reported skip rather than a clean result.

use super::tests::github_pat;
use super::worktree::{scan_worktree, WorktreeLimits};
use super::{scans_completed, SkipReason};
use crate::test_support::{git_in, git_repo, write};
use std::time::Duration;
use tempfile::TempDir;

fn repo() -> TempDir {
    let dir = git_repo();
    git_in(dir.path(), &["config", "commit.gpgsign", "false"]);
    dir
}

fn limits() -> WorktreeLimits {
    WorktreeLimits::DEFAULT
}

#[test]
fn a_planted_credential_is_found_in_a_tracked_and_an_untracked_file() {
    let dir = repo();
    write(
        dir.path(),
        "src/app.rs",
        &format!("let t = \"{}\";\n", github_pat()),
    );
    git_in(dir.path(), &["add", "src/app.rs"]);
    write(
        dir.path(),
        "notes/local.txt",
        &format!("\n{}\n", github_pat()),
    );
    let before = scans_completed();
    let scan = scan_worktree(dir.path(), limits()).expect("scan runs");
    assert_eq!(scan.tally.hits_total, 2, "{:?}", scan.tally);
    let hit = |p: &str| scan.tally.hits.iter().find(|h| h.path == p).expect(p);
    assert_eq!(hit("src/app.rs").line, 1);
    assert_eq!(hit("notes/local.txt").line, 2);
    assert!(!scan.untracked.contains("src/app.rs"));
    assert!(scan.untracked.contains("notes/local.txt"));
    assert!(
        scans_completed() > before,
        "the completion counter did not move"
    );
    assert!(!scan.tally.is_clean());
}

#[test]
fn a_gitignored_file_is_not_read() {
    let dir = repo();
    write(dir.path(), ".gitignore", "secret.env\n");
    write(dir.path(), "secret.env", &format!("{}\n", github_pat()));
    write(dir.path(), "ok.txt", "fine\n");
    let scan = scan_worktree(dir.path(), limits()).expect("scan runs");
    assert_eq!(scan.tally.hits_total, 0, "{:?}", scan.tally);
    assert!(scan.tally.is_clean(), "{:?}", scan.tally);
}

#[test]
fn an_oversized_file_is_a_skip_and_not_clean() {
    let dir = repo();
    write(dir.path(), "big.txt", &"x".repeat(2_000));
    write(dir.path(), "small.txt", "fine\n");
    let scan = scan_worktree(
        dir.path(),
        WorktreeLimits {
            file_bytes: 1_000,
            ..limits()
        },
    )
    .expect("scan runs");
    assert_eq!(scan.tally.skipped_total, 1, "{:?}", scan.tally);
    assert_eq!(scan.tally.skipped[0].path, "big.txt");
    assert_eq!(scan.tally.skipped[0].reason, SkipReason::Oversized);
    assert_eq!(scan.tally.skipped[0].bytes, Some(2_000));
    assert!(!scan.tally.is_clean());
}

#[test]
fn a_file_past_the_byte_budget_is_a_skip_and_not_clean() {
    let dir = repo();
    write(dir.path(), "a.txt", &"a".repeat(600));
    write(dir.path(), "b.txt", &"b".repeat(600));
    let scan = scan_worktree(
        dir.path(),
        WorktreeLimits {
            total_bytes: 1_000,
            ..limits()
        },
    )
    .expect("scan runs");
    assert_eq!(scan.tally.skipped_total, 1, "{:?}", scan.tally);
    assert_eq!(scan.tally.skipped[0].reason, SkipReason::BudgetExhausted);
    assert_eq!(scan.tally.inputs_scanned, 1);
    assert!(!scan.tally.is_clean());
}

#[test]
fn a_binary_file_is_read_through_the_same_scan_not_skipped() {
    // The commit gate reads the whole blob of every binary file, so a key
    // embedded in one cannot pass the working-tree scan while failing the
    // commit gate.
    let dir = repo();
    let mut bytes = vec![0u8, 1, 2, 0, 255];
    bytes.extend_from_slice(format!("\nkey={}\n", github_pat()).as_bytes());
    std::fs::write(dir.path().join("blob.bin"), bytes).unwrap();
    let scan = scan_worktree(dir.path(), limits()).expect("scan runs");
    assert_eq!(scan.tally.hits_total, 1, "{:?}", scan.tally);
    assert_eq!(scan.tally.skipped_total, 0);
}

#[test]
fn the_time_limit_is_a_counted_skip_of_what_was_not_reached() {
    let dir = repo();
    for name in ["a.txt", "b.txt", "c.txt"] {
        write(dir.path(), name, "fine\n");
    }
    let scan = scan_worktree(
        dir.path(),
        WorktreeLimits {
            time: Duration::ZERO,
            ..limits()
        },
    )
    .expect("scan runs");
    assert_eq!(scan.tally.skipped_total, 3, "{:?}", scan.tally);
    assert_eq!(scan.tally.skipped[0].reason, SkipReason::TimeLimit);
    assert_eq!(scan.tally.inputs_scanned, 0);
    assert!(!scan.tally.is_clean());
}

#[test]
fn a_tracked_file_deleted_from_disk_is_not_a_hole() {
    let dir = repo();
    write(dir.path(), "gone.txt", "fine\n");
    write(dir.path(), "kept.txt", "fine\n");
    git_in(dir.path(), &["add", "."]);
    std::fs::remove_file(dir.path().join("gone.txt")).unwrap();
    let scan = scan_worktree(dir.path(), limits()).expect("scan runs");
    assert!(scan.tally.is_clean(), "{:?}", scan.tally);
    assert_eq!(scan.tally.inputs_scanned, 1);
}

#[cfg(unix)]
#[test]
fn an_unreadable_file_is_reported_as_unreadable() {
    use std::os::unix::fs::PermissionsExt;
    let dir = repo();
    write(dir.path(), "locked.txt", "fine\n");
    let path = dir.path().join("locked.txt");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Root reads anything; the premise of the test is a file that cannot be.
    if std::fs::File::open(&path).is_ok() {
        return;
    }
    let scan = scan_worktree(dir.path(), limits()).expect("scan runs");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(scan.tally.skipped_total, 1, "{:?}", scan.tally);
    assert_eq!(scan.tally.skipped[0].reason, SkipReason::Unreadable);
}

#[test]
fn a_nested_repository_is_named_and_left_out() {
    let dir = repo();
    write(dir.path(), "top.txt", "fine\n");
    let inner = dir.path().join("vendor/inner");
    std::fs::create_dir_all(&inner).unwrap();
    git_in(&inner, &["init", "-q", "-b", "main"]);
    write(&inner, "k.txt", &format!("{}\n", github_pat()));
    let scan = scan_worktree(dir.path(), limits()).expect("scan runs");
    assert_eq!(scan.nested_repos, vec!["vendor/inner/".to_string()]);
    assert_eq!(scan.tally.hits_total, 0);
}

#[test]
fn the_report_never_carries_the_secret() {
    let dir = repo();
    let token = github_pat();
    write(dir.path(), "a.txt", &format!("{token}\n"));
    let scan = scan_worktree(dir.path(), limits()).expect("scan runs");
    let wire = serde_json::to_string(&scan.tally).unwrap();
    assert!(
        !wire.contains(&token) && !wire.contains(&token[..12]),
        "{wire}"
    );
}
