//! Integration coverage for the desktop shell's repository resolution.
//!
//! Most of `desktop/` needs a Tauri runtime, but `cmd_resolve_git_root` does
//! not: it is the entry point for "open this folder", reached from the native
//! menu, the recent list, and macOS file-open events. Resolving the wrong root
//! silently points the whole app at another repository, so it is exercised
//! here against real on-disk layouts rather than mocked paths.

use gitpulse_lib::desktop::cmd_resolve_git_root;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git must be on PATH");
    assert!(status.success(), "git {args:?} failed");
}

fn init_repo(dir: &Path) {
    run_git(dir, &["init", "-b", "main"]);
    run_git(dir, &["config", "user.email", "test@example.com"]);
    run_git(dir, &["config", "user.name", "Test User"]);
    run_git(dir, &["config", "commit.gpgsign", "false"]);
}

/// Compare through canonicalized paths: on macOS a TempDir lives under a
/// symlinked /var, so a string comparison fails for the right directory.
fn assert_same_dir(resolved: &str, expected: &Path) {
    let left = fs::canonicalize(resolved).expect("resolved path exists");
    let right = fs::canonicalize(expected).expect("expected path exists");
    assert_eq!(left, right);
}

#[test]
fn resolves_the_repository_root_from_the_root_itself() {
    let dir = TempDir::new().expect("tempdir");
    init_repo(dir.path());
    let resolved = cmd_resolve_git_root(dir.path().to_string_lossy().into_owned()).expect("a root");
    assert_same_dir(&resolved, dir.path());
}

#[test]
fn resolves_upward_from_a_nested_directory() {
    let dir = TempDir::new().expect("tempdir");
    init_repo(dir.path());
    let nested = dir.path().join("a/b/c");
    fs::create_dir_all(&nested).expect("nested dirs");
    let resolved = cmd_resolve_git_root(nested.to_string_lossy().into_owned()).expect("a root");
    assert_same_dir(&resolved, dir.path());
}

#[test]
fn stops_at_the_innermost_repository_rather_than_the_outer_one() {
    // A repo checked out inside another repo must resolve to the inner one;
    // walking past it would open the wrong project.
    let outer = TempDir::new().expect("tempdir");
    init_repo(outer.path());
    let inner = outer.path().join("vendor/library");
    fs::create_dir_all(&inner).expect("inner dirs");
    init_repo(&inner);

    let resolved = cmd_resolve_git_root(inner.to_string_lossy().into_owned()).expect("a root");
    assert_same_dir(&resolved, &inner);
}

#[test]
fn a_directory_outside_any_repository_is_an_error_not_a_guess() {
    let dir = TempDir::new().expect("tempdir");
    let error = cmd_resolve_git_root(dir.path().to_string_lossy().into_owned())
        .expect_err("a plain directory is not a repository");
    assert!(
        error.contains("Not a Git repository"),
        "unexpected error: {error}"
    );
}

#[test]
fn a_nonexistent_path_is_an_error() {
    let dir = TempDir::new().expect("tempdir");
    let missing = dir.path().join("no/such/place");
    assert!(cmd_resolve_git_root(missing.to_string_lossy().into_owned()).is_err());
    assert!(cmd_resolve_git_root(String::new()).is_err());
}

#[test]
fn resolves_from_a_file_path_not_only_a_directory() {
    // macOS "open with" hands the app a file, not its directory.
    let dir = TempDir::new().expect("tempdir");
    init_repo(dir.path());
    let file = dir.path().join("README.md");
    fs::write(&file, "hello").expect("write");
    let resolved = cmd_resolve_git_root(file.to_string_lossy().into_owned()).expect("a root");
    assert_same_dir(&resolved, dir.path());
}
