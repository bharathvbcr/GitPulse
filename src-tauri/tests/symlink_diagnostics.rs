//! Regressions from the diagnostics report: Git entries and filesystem targets
//! have different semantics. Real Git fixtures prove the distinction.
mod common;

use common::run_git;
use gitpulse_lib::engine::git_reader::GitReader;

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    run_git(dir.path(), &["init"]);
    run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
    run_git(dir.path(), &["config", "core.autocrlf", "false"]);
    dir
}

#[cfg(unix)]
#[test]
fn diff_links_as_git_objects_without_reading_their_targets() {
    use gitpulse_lib::engine::git_cli::{git_text, git_with_stdin};
    use gitpulse_lib::graph::ref_scope::RefScope;
    use std::os::unix::fs::symlink;
    let dir = repo();
    let root = dir.path();
    let path = root.to_str().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "PRIVATE CONTENT\n").unwrap();
    for (name, target) in [
        ("node_modules", outside.path().to_path_buf()),
        ("external.txt", outside.path().join("secret.txt")),
        ("dangling.txt", outside.path().join("missing")),
        ("cycle.txt", root.join("cycle.txt")),
        ("literal[*].txt", outside.path().join("secret.txt")),
    ] {
        symlink(&target, root.join(name)).unwrap();
        let diff = GitReader::get_file_diff(path, name, false, false).unwrap();
        assert!(
            diff.text.contains("new file mode 120000"),
            "{name}: {}",
            diff.text
        );
        assert!(diff.text.contains(target.to_str().unwrap()));
        assert!(!diff.text.contains("PRIVATE CONTENT"));
        git_with_stdin(
            root,
            &["apply", "--cached", "--check", "-"],
            diff.text.as_bytes(),
        )
        .unwrap();
        match GitReader::get_file_content(path, name, None) {
            Ok(content) => {
                assert!(
                    !content.contains("PRIVATE CONTENT"),
                    "{name}: followed the target: {content}"
                );
                assert!(
                    content.contains(target.to_str().unwrap()),
                    "{name}: link text should be the target path: {content}"
                );
            }
            Err(err) => {
                assert!(
                    !err.contains("PRIVATE CONTENT"),
                    "{name}: error followed the target: {err}"
                );
            }
        }
    }
    run_git(root, &["add", "--all"]);
    for name in [
        "node_modules",
        "external.txt",
        "dangling.txt",
        "cycle.txt",
        "literal[*].txt",
    ] {
        let diff = GitReader::get_file_diff(path, name, true, false).unwrap();
        assert!(diff.text.contains("120000"));
        assert!(!diff.text.contains("PRIVATE CONTENT"));
    }
    run_git(root, &["commit", "-m", "links"]);
    let oid = git_text(root, &["rev-parse", "HEAD"]).unwrap();
    for name in [
        "node_modules",
        "external.txt",
        "dangling.txt",
        "cycle.txt",
        "literal[*].txt",
    ] {
        assert!(GitReader::get_commit_file_diff(path, oid.trim(), name)
            .unwrap()
            .text
            .contains("120000"));
        let history =
            GitReader::read_commit_history_paged(path, 0, 10, None, Some(name), RefScope::Named)
                .unwrap();
        assert_eq!(history.len(), 1);
        let blob = GitReader::get_file_content(path, name, Some(oid.trim())).unwrap();
        assert!(!blob.contains("PRIVATE CONTENT"));
    }
    // Descendants of a directory link must not become outside disk reads.
    assert!(GitReader::get_file_diff(path, "node_modules/secret.txt", false, false).is_err());
    assert_eq!(
        std::fs::read_to_string(outside.path().join("secret.txt")).unwrap(),
        "PRIVATE CONTENT\n"
    );
}

#[cfg(unix)]
#[test]
fn synthetic_link_diffs_roundtrip_hostile_filenames_through_git_apply() {
    use gitpulse_lib::engine::git_cli::git_with_stdin;
    let dir = repo();
    for name in [
        "space name",
        "tab\tname",
        "line\nname",
        "quote\"name",
        "back\\slash",
        "é.rs",
    ] {
        std::os::unix::fs::symlink("target with\na newline", dir.path().join(name)).unwrap();
        let diff =
            GitReader::get_file_diff(dir.path().to_str().unwrap(), name, false, false).unwrap();
        git_with_stdin(
            dir.path(),
            &["apply", "--cached", "--check", "-"],
            diff.text.as_bytes(),
        )
        .unwrap_or_else(|e| panic!("invalid diff for {name:?}: {e}"));
    }
}

#[cfg(unix)]
#[test]
fn synthetic_diffs_refuse_lossy_text_instead_of_offering_a_corrupted_patch() {
    use std::os::unix::ffi::OsStrExt;
    let dir = repo();
    let path = dir.path().to_str().unwrap();
    std::os::unix::fs::symlink(
        std::ffi::OsStr::from_bytes(b"target-\xff"),
        dir.path().join("link"),
    )
    .unwrap();
    std::fs::write(dir.path().join("text.txt"), b"text-\xff").unwrap();
    for name in ["link", "text.txt"] {
        let error = GitReader::get_file_diff(path, name, false, false)
            .expect_err("lossy patch must be refused");
        assert!(error.contains("UTF-8"), "{error}");
    }
}

#[cfg(unix)]
#[test]
fn historical_reads_ignore_replaced_working_tree_directories() {
    use gitpulse_lib::engine::git_cli::git_text;
    use gitpulse_lib::graph::RefScope;
    let dir = repo();
    let root = dir.path();
    let path = root.to_str().unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/file.rs"), "fn original() {}\n").unwrap();
    run_git(root, &["add", "--all"]);
    run_git(root, &["commit", "-m", "original"]);
    let oid = git_text(root, &["rev-parse", "HEAD"]).unwrap();
    std::fs::remove_file(root.join("src/file.rs")).unwrap();
    std::fs::remove_dir(root.join("src")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("file.rs"), "external content").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("src")).unwrap();
    assert_eq!(
        GitReader::get_file_content(path, "src/file.rs", Some(oid.trim())).unwrap(),
        "fn original() {}\n"
    );
    assert_eq!(
        GitReader::read_commit_history_paged(
            path,
            0,
            10,
            None,
            Some("src/file.rs"),
            RefScope::Named
        )
        .unwrap()
        .len(),
        1
    );
    assert!(
        GitReader::get_commit_file_diff(path, oid.trim(), "src/file.rs")
            .unwrap()
            .text
            .contains("original")
    );
    assert!(GitReader::get_file_content(path, "src/file.rs", None).is_err());
}

#[test]
fn language_file_cap_reports_partial_and_counts_exact_candidates() {
    let dir = repo();
    for i in 0..10_001 {
        std::fs::write(dir.path().join(format!("file{i}.rs")), "fn main() {}\n").unwrap();
    }
    let report =
        GitReader::get_repo_language_stats_bounded(dir.path().to_str().unwrap(), None).unwrap();
    assert_eq!(report.candidate_files, 10_001);
    assert_eq!(report.scanned_files, 10_000);
    assert!(
        report.truncated,
        "the 10k cap must never masquerade as full coverage"
    );
}

#[cfg(unix)]
#[test]
fn language_scan_refuses_fifo_without_waiting_for_a_writer() {
    use std::ffi::CString;
    use std::sync::mpsc;
    use std::time::Duration;
    let dir = repo();
    std::fs::write(dir.path().join("pipe.rs"), "fn main() {}\n").unwrap();
    run_git(dir.path(), &["add", "--all"]);
    std::fs::remove_file(dir.path().join("pipe.rs")).unwrap();
    let fifo = CString::new(dir.path().join("pipe.rs").as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: fifo is a live, NUL-terminated pathname owned by this fixture.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = GitReader::get_repo_language_stats_bounded(dir.path().to_str().unwrap(), None);
        let _ = tx.send(result);
    });
    let report = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("FIFO must never block the scan")
        .unwrap();
    assert_eq!(report.candidate_files, 1);
    assert_eq!(report.scanned_files, 0);
    assert!(
        report.truncated,
        "unreadable candidates are not a complete count"
    );
}

#[test]
fn language_candidates_preserve_literal_whitespace_and_deduplicate_conflict_stages() {
    let dir = repo();
    let root = dir.path();
    std::fs::write(root.join(" source.rs"), "fn first() {}\nfn second() {}\n").unwrap();
    run_git(root, &["add", "--all"]);
    let report = GitReader::get_repo_language_stats_bounded(root.to_str().unwrap(), None).unwrap();
    assert_eq!(report.scanned_files, 1);
    assert_eq!(report.stats.iter().map(|s| s.code_lines).sum::<usize>(), 2);
    // One path in three unmerged index stages is still one filesystem file.
    use gitpulse_lib::engine::git_cli::{git_text, git_with_stdin};
    let oid = git_text(root, &["rev-parse", ": source.rs"]).unwrap();
    let records = format!("0 {}\t source.rs\n100644 {} 1\t source.rs\n100644 {} 2\t source.rs\n100644 {} 3\t source.rs\n", "0".repeat(oid.trim().len()), oid.trim(), oid.trim(), oid.trim());
    git_with_stdin(root, &["update-index", "--index-info"], records.as_bytes()).unwrap();
    let report = GitReader::get_repo_language_stats_bounded(root.to_str().unwrap(), None).unwrap();
    assert_eq!(report.candidate_files, 1);
    assert_eq!(report.scanned_files, 1);
}
