//! Fixtures shared by the crate's unit tests.
//!
//! `git_in` existed seven times over — in `engine::git_cli`, `engine::worktree`,
//! `harness`, `ingest`, `insights`, `ledger::bindings` and `watcher` — as
//! byte-identical bodies that differed only in whether they spelled `Path` and
//! `Command` in full. Seven copies of a fixture is seven places to update when
//! the identity pins or the failure message need to change, and nothing fails
//! when only six of them are.

use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

/// Runs `git` in `dir` with the test identity pinned, and asserts it succeeded.
///
/// The `-c` flags are what make the fixture hermetic: a developer's global
/// `user.name`, `user.email` or `commit.gpgsign` must not decide whether the
/// suite passes, and signing in particular would block on a passphrase prompt.
///
/// The `cfg(test)` is redundant with this module's gated declaration in
/// `lib.rs`, and is kept because `tests/spawn_seam.rs` classifies source by
/// the attributes it can see in the file: without it, this fixture reads as
/// production code spawning outside the gated seam.
#[cfg(test)]
pub(crate) fn git_in(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=GitPulse",
            "-c",
            "user.email=gitpulse@test.local",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A fresh repository on `main`, for suites that only need somewhere to scan.
///
/// Identical bodies stood in `analyzer::coverage` and `analyzer::deps`; the
/// default-branch name is the kind of fact that gets corrected in one copy.
#[cfg(test)]
pub(crate) fn git_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let status = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .status()
        .expect("git init");
    assert!(status.success());
    dir
}

/// Writes `content` to `dir/rel`, creating parent directories as needed.
#[cfg(test)]
pub(crate) fn write(dir: &Path, rel: &str, content: &str) {
    let dest = dir.join(rel);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(dest, content).unwrap();
}
