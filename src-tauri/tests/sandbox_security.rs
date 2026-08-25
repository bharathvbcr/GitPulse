//! Integration tests for sandbox hardening: symlink escapes through
//! `sandbox_write` and working-tree reads, plus the bounded `gh` CLI probe.

use gitpulse_lib::engine::git_cli::sandbox_write;
use gitpulse_lib::engine::git_reader::GitReader;
use std::time::{Duration, Instant};

/// Creates a plain (no commits needed) git repo so `validate_repo` accepts it.
fn init_repo(dir: &std::path::Path) {
    let output = std::process::Command::new("git")
        .arg("init")
        .current_dir(dir)
        .output()
        .expect("spawn git init");
    assert!(
        output.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn sandbox_write_refuses_symlink_escape() {
    let outside = tempfile::TempDir::new().unwrap();
    std::fs::write(outside.path().join("target.txt"), "original").unwrap();

    let repo_dir = tempfile::TempDir::new().unwrap();
    init_repo(repo_dir.path());
    let repo_str = repo_dir.path().to_string_lossy().into_owned();
    let repo = repo_dir.path().canonicalize().unwrap();

    // Case 1: symlink directly to an existing file outside the repo. Pre-fix,
    // fs::write followed the link and overwrote the external file.
    std::os::unix::fs::symlink(outside.path().join("target.txt"), repo.join("file-leak")).unwrap();
    let err = sandbox_write(&repo_str, "file-leak", "pwned").expect_err("file symlink");
    assert!(err.contains("escapes the repository"), "got: {err}");
    assert_eq!(
        std::fs::read_to_string(outside.path().join("target.txt")).unwrap(),
        "original",
        "outside file must be untouched"
    );

    // Case 2: symlink to a directory outside the repo; the write lands in a
    // NEW nested path through the link.
    std::os::unix::fs::symlink(outside.path(), repo.join("dir-leak")).unwrap();
    let err =
        sandbox_write(&repo_str, "dir-leak/payload.txt", "pwned").expect_err("directory symlink");
    assert!(err.contains("escapes the repository"), "got: {err}");
    assert!(
        !outside.path().join("payload.txt").exists(),
        "nothing may be written through the directory link"
    );

    // Case 3: dangling symlink pointing at a non-existent outside target.
    // The link itself exists, so it must still be refused.
    std::os::unix::fs::symlink(outside.path().join("missing.txt"), repo.join("dangling")).unwrap();
    assert!(sandbox_write(&repo_str, "dangling", "pwned").is_err());
}

#[cfg(unix)]
#[test]
fn sandbox_write_still_creates_nested_paths_inside_repo() {
    let repo_dir = tempfile::TempDir::new().unwrap();
    init_repo(repo_dir.path());
    let repo_str = repo_dir.path().to_string_lossy().into_owned();

    sandbox_write(&repo_str, "src/deep/nested/new.txt", "hello").expect("nested write");
    assert_eq!(
        std::fs::read_to_string(repo_dir.path().join("src/deep/nested/new.txt")).unwrap(),
        "hello"
    );

    // Reading back through the reader works on the canonicalized path too.
    let content = GitReader::get_file_content(&repo_str, "src/deep/nested/new.txt", None)
        .expect("read back written file");
    assert_eq!(content, "hello");

    // An internal symlink (target inside the repo) remains readable — the fix
    // must not over-reject legitimate repos that use relative internal links.
    #[cfg(unix)]
    {
        let repo = repo_dir.path().canonicalize().unwrap();
        std::os::unix::fs::symlink(repo.join("src/deep/nested/new.txt"), repo.join("alias.txt"))
            .unwrap();
        let blob = GitReader::get_file_blob(&repo_str, "alias.txt", None).expect("internal link");
        assert_eq!(blob.text.as_deref(), Some("hello"));
    }
}

#[cfg(unix)]
#[test]
fn working_tree_reads_refuse_symlink_escape() {
    let outside = tempfile::TempDir::new().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "top secret").unwrap();

    let repo_dir = tempfile::TempDir::new().unwrap();
    init_repo(repo_dir.path());
    let repo_str = repo_dir.path().to_string_lossy().into_owned();
    let repo = repo_dir.path().canonicalize().unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.txt"), repo.join("leak.txt")).unwrap();

    // Pre-fix both calls happily fs::read the outside file.
    assert!(GitReader::get_file_content(&repo_str, "leak.txt", None).is_err());
    assert!(GitReader::get_file_blob(&repo_str, "leak.txt", None).is_err());

    // Language stats: the untracked link is listed by `ls-files --others`.
    // Pre-fix the loop read through it and accrued LOC for Python from bytes
    // living outside the repo; post-fix the read is refused and the file
    // records zero code lines.
    let outside_py = tempfile::TempDir::new().unwrap();
    std::fs::write(outside_py.path().join("payload.py"), "print('pwned')\n").unwrap();
    std::os::unix::fs::symlink(outside_py.path().join("payload.py"), repo.join("evil.py")).unwrap();
    let stats = GitReader::get_repo_language_stats(&repo_str).expect("language stats");
    assert!(
        stats
            .iter()
            .filter(|s| s.language == "Python")
            .all(|s| s.code_lines == 0),
        "no language may accrue lines from an escaped read: {stats:?}"
    );
}

/// A hung `gh` earlier on PATH must not block the presence probe forever:
/// pre-fix this test hung on `Command::output()`; post-fix it returns within
/// the probe timeout and reports absence.
#[cfg(unix)]
#[test]
fn hung_gh_probe_is_bounded() {
    let fake_bin = tempfile::TempDir::new().unwrap();
    let fake_gh = fake_bin.path().join("gh");
    std::fs::write(&fake_gh, "#!/bin/sh\nsleep 300\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake_gh, std::fs::Permissions::from_mode(0o755)).unwrap();

    let old_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var(
        "PATH",
        format!("{}:{}", fake_bin.path().display(), old_path),
    );
    let start = Instant::now();
    let present = gitpulse_lib::github::gh_cli_present();
    std::env::set_var("PATH", old_path);

    assert!(!present, "a hanging gh must not count as present");
    assert!(
        start.elapsed() < Duration::from_secs(30),
        "probe took {:?}; it must be bounded well below the fake gh's 300s sleep",
        start.elapsed()
    );
}
