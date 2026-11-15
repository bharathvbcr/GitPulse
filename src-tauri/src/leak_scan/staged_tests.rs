//! The commit gate against real repositories: what it blocks, what it lets
//! through, and that a scan which could not finish never reads as clean.

use super::staged::{scan_staged, Policy, MAX_BLOB_BYTES};
use super::tests::{aws_key_id, github_pat};
use super::{scans_completed, SkipReason};
use crate::engine::git_writer::GitWriter;
use crate::ipc_error::IpcErrorKind;
use crate::procguard::LockedSpawn;
use crate::test_support::{git_in, git_repo, write};
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn repo() -> TempDir {
    let dir = git_repo();
    git_in(dir.path(), &["config", "user.name", "Scan Test"]);
    git_in(dir.path(), &["config", "user.email", "scan@example.com"]);
    git_in(dir.path(), &["config", "commit.gpgsign", "false"]);
    dir
}

fn repo_with_commit() -> TempDir {
    let dir = repo();
    write(dir.path(), "README.md", "hello\n");
    git_in(dir.path(), &["add", "README.md"]);
    git_in(dir.path(), &["commit", "-q", "-m", "init"]);
    dir
}

fn path_of(dir: &TempDir) -> String {
    dir.path().to_str().expect("utf-8 temp path").to_string()
}

fn out(dir: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    crate::test_support::pin_git(&mut command);
    let output = command
        .args(args)
        .current_dir(dir)
        .output_locked()
        .expect("git runs");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn head(dir: &TempDir) -> String {
    out(dir.path(), &["rev-parse", "--verify", "-q", "HEAD"])
}

fn staged_names(dir: &TempDir) -> String {
    out(dir.path(), &["diff", "--cached", "--name-only"])
}

#[test]
fn a_clean_change_commits_and_the_scan_is_counted() {
    let dir = repo_with_commit();
    write(
        dir.path(),
        "src/lib.rs",
        "pub fn add(a: i32, b: i32) -> i32 { a + b }\n",
    );
    git_in(dir.path(), &["add", "src/lib.rs"]);
    let before = scans_completed();
    GitWriter::commit(&path_of(&dir), "feat: add", false).expect("a clean change commits");
    assert!(scans_completed() > before, "the in-house scan ran");
    assert_eq!(out(dir.path(), &["log", "-1", "--format=%s"]), "feat: add");
}

#[test]
fn a_staged_token_blocks_the_commit_and_the_refusal_never_carries_it() {
    let dir = repo_with_commit();
    let token = github_pat();
    write(
        dir.path(),
        "ci/deploy.sh",
        &format!("#!/bin/sh\n\nexport GH={token}\n"),
    );
    git_in(dir.path(), &["add", "ci/deploy.sh"]);
    let before = head(&dir);
    let err = GitWriter::commit(&path_of(&dir), "ci: deploy", false).expect_err("blocked");
    assert_eq!(err.kind(), IpcErrorKind::SecretScanBlocked);
    assert!(
        err.message().contains("ci/deploy.sh:3"),
        "{}",
        err.message()
    );
    let ipc = crate::ipc_error::run_command(|| Err::<(), _>(err)).expect_err("refused");
    let wire = serde_json::to_string(&ipc).unwrap();
    assert!(!wire.contains(&token), "{wire}");
    assert!(
        !wire.contains(&token[..10]),
        "a prefix of the token leaked: {wire}"
    );
    let detail = ipc.detail.expect("detail");
    assert_eq!(detail["hits_total"], 1);
    assert_eq!(detail["hits"][0]["path"], "ci/deploy.sh");
    assert_eq!(detail["hits"][0]["line"], 3);
    assert_eq!(head(&dir), before, "nothing was committed");
    assert_eq!(
        staged_names(&dir),
        "ci/deploy.sh",
        "the index is left as it was"
    );
}

#[test]
fn a_person_can_commit_past_the_refusal() {
    let dir = repo_with_commit();
    write(
        dir.path(),
        "fixture.env",
        &format!("AWS_ACCESS_KEY_ID={}\n", aws_key_id()),
    );
    git_in(dir.path(), &["add", "fixture.env"]);
    assert!(GitWriter::commit(&path_of(&dir), "test: fixture", false).is_err());
    GitWriter::commit_with_policy(
        &path_of(&dir),
        "test: fixture",
        false,
        Policy::HumanOverride,
    )
    .expect("the override commits");
    assert_eq!(
        out(dir.path(), &["log", "-1", "--format=%s"]),
        "test: fixture"
    );
}

#[test]
fn quick_commit_scans_what_add_all_staged() {
    let dir = repo_with_commit();
    // Untracked until quick commit's own `add --all`: a scan before the add
    // would see an empty index and pass.
    write(
        dir.path(),
        "notes.txt",
        &format!("token: {}\n", github_pat()),
    );
    let before = head(&dir);
    let err = GitWriter::quick_commit(&path_of(&dir), "chore: everything").expect_err("blocked");
    assert_eq!(err.kind(), IpcErrorKind::SecretScanBlocked);
    assert_eq!(head(&dir), before);
    GitWriter::quick_commit_with_policy(&path_of(&dir), "chore: everything", Policy::HumanOverride)
        .expect("override commits");
}

#[test]
fn a_binary_file_is_read_from_the_index() {
    let dir = repo_with_commit();
    let mut bytes = b"\x00\x01SQLite-ish\x00\n".to_vec();
    bytes.extend_from_slice(format!("key={}\n", aws_key_id()).as_bytes());
    std::fs::write(dir.path().join("cache.db"), &bytes).unwrap();
    git_in(dir.path(), &["add", "cache.db"]);
    let scan = scan_staged(dir.path(), &[]).expect("scan runs");
    assert_eq!(scan.tally.hits_total, 1, "{:?}", scan.tally);
    assert_eq!(scan.tally.hits[0].path, "cache.db");
    assert_eq!(scan.tally.hits[0].line, 2);
    // What git commits is the index, not the working tree: rewriting the
    // file after staging must not change the verdict.
    std::fs::write(dir.path().join("cache.db"), b"\x00clean now").unwrap();
    assert!(GitWriter::commit(&path_of(&dir), "data", false).is_err());
}

#[test]
fn an_oversized_binary_is_a_skip_and_blocks() {
    let dir = repo_with_commit();
    let mut bytes = vec![0u8; MAX_BLOB_BYTES as usize + 1];
    bytes[1] = b'x';
    std::fs::write(dir.path().join("big.bin"), &bytes).unwrap();
    git_in(dir.path(), &["add", "big.bin"]);
    let scan = scan_staged(dir.path(), &[]).expect("scan runs");
    assert_eq!(scan.tally.hits_total, 0);
    assert_eq!(scan.tally.skipped_total, 1);
    assert_eq!(scan.tally.skipped[0].reason, SkipReason::Oversized);
    assert!(!scan.tally.is_clean(), "a file nobody read is not clean");
    let err = GitWriter::commit(&path_of(&dir), "assets", false).expect_err("blocked");
    assert!(err.message().contains("big.bin"), "{}", err.message());
}

#[test]
fn the_first_commit_of_an_unborn_branch_is_scanned() {
    let dir = repo();
    write(dir.path(), "a.txt", &format!("{}\n", github_pat()));
    git_in(dir.path(), &["add", "a.txt"]);
    assert_eq!(head(&dir), "", "unborn");
    let err = GitWriter::commit(&path_of(&dir), "init", false).expect_err("blocked");
    assert_eq!(err.kind(), IpcErrorKind::SecretScanBlocked);
    write(dir.path(), "a.txt", "nothing here\n");
    git_in(dir.path(), &["add", "a.txt"]);
    GitWriter::commit(&path_of(&dir), "init", false).expect("clean first commit");
}

#[test]
fn removing_a_committed_token_is_not_blocked() {
    let dir = repo_with_commit();
    write(
        dir.path(),
        "old.cfg",
        &format!("token={}\nkeep\n", github_pat()),
    );
    git_in(dir.path(), &["add", "old.cfg"]);
    GitWriter::commit_with_policy(&path_of(&dir), "old", false, Policy::HumanOverride).unwrap();
    write(dir.path(), "old.cfg", "keep\n");
    git_in(dir.path(), &["add", "old.cfg"]);
    GitWriter::commit(&path_of(&dir), "remove the token", false)
        .expect("deleting a credential is what should happen next");
    git_in(dir.path(), &["mv", "old.cfg", "renamed.cfg"]);
    GitWriter::commit(&path_of(&dir), "rename", false).expect("a rename adds no lines");
}

#[test]
fn awkward_file_names_keep_their_lines() {
    let dir = repo_with_commit();
    let names = [
        "sp ace.txt",
        "tab\tname.txt",
        "new\nline.txt",
        "star*[x].txt",
        "caf\u{e9}.txt",
    ];
    for (n, name) in names.iter().enumerate() {
        let body = if n == 3 {
            format!("one\ntwo\nk={}\n", github_pat())
        } else {
            "plain\n".to_string()
        };
        std::fs::write(dir.path().join(name), body).unwrap();
        git_in(dir.path(), &["add", "--", name]);
    }
    let scan = scan_staged(dir.path(), &[]).expect("scan runs");
    assert_eq!(scan.tally.hits_total, 1);
    assert_eq!(scan.tally.hits[0].path, "star*[x].txt");
    assert_eq!(scan.tally.hits[0].line, 3);
    assert_eq!(scan.tally.inputs_scanned, names.len() as u64);
}

#[test]
fn a_scan_that_cannot_read_the_index_refuses_rather_than_passes() {
    let dir = repo_with_commit();
    // An index entry naming a blob that does not exist: `git diff` cannot
    // produce the patch, so the scan cannot reach a verdict.
    let missing = "1234567890123456789012345678901234567890";
    git_in(
        dir.path(),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{missing},ghost.txt"),
        ],
    );
    assert!(
        scan_staged(dir.path(), &[]).is_err(),
        "no verdict from an unreadable index"
    );
    let err = GitWriter::commit(&path_of(&dir), "ghost", false).expect_err("fails closed");
    assert_eq!(err.kind(), IpcErrorKind::SecretScanBlocked);
    assert!(
        err.message().contains("could not be scanned"),
        "{}",
        err.message()
    );
    let ipc = crate::ipc_error::run_command(|| Err::<(), _>(err)).expect_err("refused");
    assert!(ipc.detail.expect("detail")["failure"].is_string());
}

#[test]
fn commit_files_scans_only_the_files_it_commits() {
    let dir = repo_with_commit();
    write(dir.path(), "picked.txt", "fine\n");
    // Staged but not selected: `commit --only` leaves it out of the commit,
    // so it must not block one.
    write(dir.path(), "other.txt", &format!("{}\n", github_pat()));
    git_in(dir.path(), &["add", "other.txt"]);
    GitWriter::commit_files(&path_of(&dir), "pick", &["picked.txt".into()])
        .expect("the selected file is clean");
    write(dir.path(), "picked.txt", &format!("{}\n", aws_key_id()));
    let err = GitWriter::commit_files(&path_of(&dir), "pick again", &["picked.txt".into()])
        .expect_err("the selected file is not");
    assert_eq!(err.kind(), IpcErrorKind::SecretScanBlocked);
}

#[test]
fn an_amend_that_adds_a_token_is_blocked() {
    let dir = repo_with_commit();
    write(
        dir.path(),
        "README.md",
        &format!("hello\n{}\n", github_pat()),
    );
    git_in(dir.path(), &["add", "README.md"]);
    let err = GitWriter::commit(&path_of(&dir), "", true).expect_err("blocked");
    assert_eq!(err.kind(), IpcErrorKind::SecretScanBlocked);
    assert_eq!(out(dir.path(), &["log", "-1", "--format=%s"]), "init");
}

#[test]
fn a_diff_configuration_cannot_change_what_is_read() {
    let dir = repo_with_commit();
    // An external diff driver and a textconv that hide everything would turn
    // the patch into nothing; the scan disables both.
    git_in(dir.path(), &["config", "diff.external", "true"]);
    git_in(dir.path(), &["config", "diff.hide.textconv", "true"]);
    write(dir.path(), ".gitattributes", "*.txt diff=hide\n");
    write(dir.path(), "x.txt", &format!("{}\n", github_pat()));
    git_in(dir.path(), &["add", ".gitattributes", "x.txt"]);
    git_in(dir.path(), &["config", "color.diff", "always"]);
    git_in(dir.path(), &["config", "diff.noprefix", "true"]);
    let scan = scan_staged(dir.path(), &[]).expect("scan runs");
    assert_eq!(scan.tally.hits_total, 1, "{:?}", scan.tally);
}
