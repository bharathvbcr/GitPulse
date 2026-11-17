//! The commit gate against real repositories: what it blocks, what it lets
//! through, and that a scan which could not finish never reads as clean.

use super::staged::{
    scan_staged, Policy, BATCH_BLOB_BYTES, BATCH_BUDGET_BYTES, STREAM_BUDGET_BYTES,
};
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

/// A binary of `len` zero bytes (one NUL-filled "line"), with `tail` appended.
fn binary_with_tail(len: usize, tail: &str) -> Vec<u8> {
    let mut bytes = vec![0u8; len];
    bytes.extend_from_slice(tail.as_bytes());
    bytes
}

#[test]
fn a_binary_past_the_batch_is_streamed_and_read_to_its_end() {
    let dir = repo_with_commit();
    let len = BATCH_BLOB_BYTES as usize + 1;
    // The key sits after the blob's only newline, past the batch cap, so it
    // is found only if the whole blob was read.
    let tail = format!("\nkey={}\n", aws_key_id());
    std::fs::write(dir.path().join("big.bin"), binary_with_tail(len, &tail)).unwrap();
    git_in(dir.path(), &["add", "big.bin"]);
    let scan = scan_staged(dir.path(), &[]).expect("scan runs");
    assert_eq!(scan.tally.skipped_total, 0, "{:?}", scan.tally.skipped);
    assert_eq!(scan.tally.hits_total, 1, "{:?}", scan.tally.hits);
    assert_eq!(scan.tally.hits[0].path, "big.bin");
    assert_eq!(scan.tally.hits[0].line, 2);
    assert_eq!(scan.tally.bytes_scanned as usize, len + tail.len() - 2);
    let err = GitWriter::commit(&path_of(&dir), "assets", false).expect_err("blocked");
    assert_eq!(err.kind(), IpcErrorKind::SecretScanBlocked);
}

#[test]
fn a_clean_binary_past_the_batch_commits_without_an_override() {
    let dir = repo_with_commit();
    let bytes = binary_with_tail(BATCH_BLOB_BYTES as usize * 2, "\nplain trailer\n");
    std::fs::write(dir.path().join("asset.bin"), bytes).unwrap();
    git_in(dir.path(), &["add", "asset.bin"]);
    let before = head(&dir);
    GitWriter::commit(&path_of(&dir), "asset", false).expect("a clean large binary commits");
    assert_ne!(head(&dir), before);
}

#[test]
fn blobs_past_the_batch_budget_are_streamed_not_skipped() {
    let dir = repo_with_commit();
    // Each blob fits the batch on its own size; together they overflow its
    // budget, and the overflow is streamed. The key is in the last one.
    // Room for the key's tail, so the last blob also fits the batch by size
    // and is streamed only because the budget ran out.
    let each = BATCH_BLOB_BYTES as usize - 64;
    let count = (BATCH_BUDGET_BYTES as usize / each) + 1;
    for n in 0..count {
        let tail = if n + 1 == count {
            format!("\n{}\n", github_pat())
        } else {
            "\n".to_string()
        };
        let name = format!("part{n}.bin");
        std::fs::write(dir.path().join(&name), binary_with_tail(each, &tail)).unwrap();
        git_in(dir.path(), &["add", &name]);
    }
    let scan = scan_staged(dir.path(), &[]).expect("scan runs");
    assert_eq!(scan.tally.skipped_total, 0, "{:?}", scan.tally.skipped);
    assert_eq!(scan.tally.inputs_scanned as usize, count);
    assert_eq!(scan.tally.hits_total, 1, "{:?}", scan.tally.hits);
    assert_eq!(scan.tally.hits[0].path, format!("part{}.bin", count - 1));
}

#[test]
fn a_blob_past_the_runners_buffered_limit_is_streamed_to_its_end() {
    use std::io::{Seek, SeekFrom, Write};
    let dir = repo_with_commit();
    // Sparse zeros past the runner's 64 MiB buffered-output limit, then a key.
    let len = crate::engine::process::MAX_OUTPUT_BYTES as u64 + 1024 * 1024;
    let mut file = std::fs::File::create(dir.path().join("video.bin")).unwrap();
    file.seek(SeekFrom::Start(len)).unwrap();
    file.write_all(format!("\nkey={}\n", aws_key_id()).as_bytes())
        .unwrap();
    drop(file);
    git_in(dir.path(), &["add", "video.bin"]);
    let scan = scan_staged(dir.path(), &[]).expect("scan runs");
    assert_eq!(scan.tally.skipped_total, 0, "{:?}", scan.tally.skipped);
    assert_eq!(scan.tally.hits_total, 1, "{:?}", scan.tally.hits);
    assert_eq!(scan.tally.hits[0].line, 2);
}

#[test]
fn a_binary_past_the_stream_budget_is_a_skip_and_blocks() {
    let dir = repo_with_commit();
    // Sparse, so the fixture costs no disk: git still has to hash it.
    let big = std::fs::File::create(dir.path().join("huge.bin")).unwrap();
    big.set_len(STREAM_BUDGET_BYTES + 1).unwrap();
    drop(big);
    git_in(dir.path(), &["add", "huge.bin"]);
    let scan = scan_staged(dir.path(), &[]).expect("scan runs");
    assert_eq!(scan.tally.skipped_total, 1);
    assert_eq!(scan.tally.skipped[0].reason, SkipReason::Oversized);
    assert!(!scan.tally.is_clean(), "a file nobody read is not clean");
    let err = GitWriter::commit(&path_of(&dir), "assets", false).expect_err("blocked");
    assert!(err.message().contains("huge.bin"), "{}", err.message());
}

#[test]
fn a_streamed_blob_git_could_not_produce_is_a_skip() {
    let dir = repo_with_commit();
    let bytes = binary_with_tail(BATCH_BLOB_BYTES as usize + 1, "\n");
    std::fs::write(dir.path().join("big.bin"), bytes).unwrap();
    git_in(dir.path(), &["add", "big.bin"]);
    // Only the streamed read passes `blob` as an argument.
    let scan = crate::engine::process::with_forced_spawn_failure_of("blob", || {
        scan_staged(dir.path(), &[]).expect("the scan still reaches a verdict")
    });
    assert_eq!(scan.tally.skipped_total, 1, "{:?}", scan.tally);
    assert_eq!(scan.tally.skipped[0].reason, SkipReason::Unreadable);
    assert!(!scan.tally.is_clean());
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

/// Replace `name` in the working tree with a symlink to `target`.
#[cfg(unix)]
fn become_symlink(dir: &Path, name: &str, target: &str) {
    std::fs::remove_file(dir.join(name)).unwrap();
    std::os::unix::fs::symlink(target, dir.join(name)).unwrap();
}

/// Replace `name` in the working tree with a regular file of `bytes`.
#[cfg(unix)]
fn become_file(dir: &Path, name: &str, bytes: &[u8]) {
    std::fs::remove_file(dir.join(name)).unwrap();
    std::fs::write(dir.join(name), bytes).unwrap();
}

/// A repository whose `HEAD` holds a symlink `link` and a regular file
/// `cfg.txt`, so either can change type.
#[cfg(unix)]
fn repo_with_link_and_file() -> TempDir {
    let dir = repo_with_commit();
    std::os::unix::fs::symlink("README.md", dir.path().join("link")).unwrap();
    write(dir.path(), "cfg.txt", "mode=plain\n");
    git_in(dir.path(), &["add", "link", "cfg.txt"]);
    git_in(dir.path(), &["commit", "-q", "-m", "link and file"]);
    dir
}

#[cfg(unix)]
#[test]
fn a_path_that_changes_type_either_way_is_scanned_not_refused() {
    let dir = repo_with_link_and_file();
    become_file(dir.path(), "link", b"now a plain file\n");
    become_symlink(dir.path(), "cfg.txt", "README.md");
    git_in(dir.path(), &["add", "link", "cfg.txt"]);
    // One raw entry and two patch sections per path: the scan must still
    // reach a verdict, and read the added side of each.
    let scan = scan_staged(dir.path(), &[]).expect("a type change is scannable");
    assert!(scan.tally.is_clean(), "{:?}", scan.tally);
    assert_eq!(scan.tally.inputs_scanned, 2, "{:?}", scan.tally);
    assert_eq!(scan.tally.lines_scanned, 2, "{:?}", scan.tally);
    GitWriter::commit(&path_of(&dir), "retype", false).expect("a clean type change commits");
    assert_eq!(out(dir.path(), &["log", "-1", "--format=%s"]), "retype");
}

#[cfg(unix)]
#[test]
fn a_secret_planted_in_the_new_file_of_a_type_change_is_found() {
    let dir = repo_with_link_and_file();
    let token = github_pat();
    become_file(dir.path(), "link", format!("{token}\n").as_bytes());
    git_in(dir.path(), &["add", "link"]);
    let scan = scan_staged(dir.path(), &[]).expect("a type change is scannable");
    assert_eq!(scan.tally.hits_total, 1, "{:?}", scan.tally);
    assert_eq!(scan.tally.hits[0].path, "link");
    assert_eq!(scan.tally.hits[0].line, 1);
    let before = head(&dir);
    let err = GitWriter::commit(&path_of(&dir), "leak", false).expect_err("blocked");
    assert_eq!(err.kind(), IpcErrorKind::SecretScanBlocked);
    assert!(err.message().contains("link:1"), "{}", err.message());
    assert!(
        !err.message().contains("could not be scanned"),
        "{}",
        err.message()
    );
    assert_eq!(head(&dir), before, "nothing was committed");
}

#[cfg(unix)]
#[test]
fn a_type_change_into_or_out_of_a_binary_file_reads_only_the_new_side() {
    let dir = repo_with_link_and_file();
    // symlink -> binary file: the new side is a blob git calls binary, so its
    // bytes come from the index rather than the patch.
    let mut bytes = b"\x00\x01blob\x00\n".to_vec();
    bytes.extend_from_slice(format!("key={}\n", aws_key_id()).as_bytes());
    become_file(dir.path(), "link", &bytes);
    git_in(dir.path(), &["add", "link"]);
    let scan = scan_staged(dir.path(), &[]).expect("a type change is scannable");
    assert_eq!(scan.tally.hits_total, 1, "{:?}", scan.tally);
    assert_eq!(scan.tally.hits[0].path, "link");
    assert_eq!(scan.tally.hits[0].line, 2);
    git_in(dir.path(), &["reset", "-q", "--", "link"]);

    // binary file -> symlink: the removed binary is history and is not read;
    // the new symlink is a text section and is.
    std::fs::write(dir.path().join("cfg.txt"), &bytes).unwrap();
    git_in(dir.path(), &["add", "cfg.txt"]);
    git_in(dir.path(), &["commit", "-q", "-m", "binary", "--no-verify"]);
    become_symlink(dir.path(), "cfg.txt", "README.md");
    git_in(dir.path(), &["add", "cfg.txt"]);
    let scan = scan_staged(dir.path(), &[]).expect("a type change is scannable");
    assert!(scan.tally.is_clean(), "{:?}", scan.tally);
    assert_eq!(scan.tally.inputs_scanned, 1, "{:?}", scan.tally);
}

#[test]
fn a_nested_submodule_diff_in_the_users_configuration_cannot_add_sections() {
    let dir = repo_with_commit();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    git_in(&sub, &["init", "-q", "-b", "main"]);
    git_in(&sub, &["config", "user.name", "Scan Test"]);
    git_in(&sub, &["config", "user.email", "scan@example.com"]);
    git_in(&sub, &["config", "commit.gpgsign", "false"]);
    write(&sub, "a.txt", "one\n");
    git_in(&sub, &["add", "a.txt"]);
    git_in(&sub, &["commit", "-q", "-m", "one"]);
    git_in(dir.path(), &["add", "sub"]);
    git_in(dir.path(), &["commit", "-q", "-m", "add sub"]);
    write(&sub, "a.txt", "one\ntwo\n");
    git_in(&sub, &["commit", "-q", "-am", "two"]);
    git_in(dir.path(), &["add", "sub"]);
    git_in(dir.path(), &["config", "diff.submodule", "diff"]);
    let scan = scan_staged(dir.path(), &[]).expect("scan runs");
    assert!(scan.tally.is_clean(), "{:?}", scan.tally);
}
