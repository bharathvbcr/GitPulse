//! Fixtures shared by the integration-test crates.
//!
//! Each file under `tests/` is its own crate, so a helper used by several of
//! them has to live in a module they all declare — otherwise it gets pasted
//! into each, which is what happened to `run_git`: five byte-identical copies
//! across `conflict_eol_integration`, `default_branch_and_tags`, `git_ops`,
//! `language_stack` and `status_numstat_regressions`.
//!
//! Only genuinely identical fixtures belong here. The `TestRepo` types in
//! those same files look alike but have diverged — each carries methods its
//! own suite needs — so they are deliberately left alone rather than forced
//! behind one shape.

// Every test crate that declares `mod common;` compiles all of it, so a helper
// used by four of the five reads as dead in the fifth.
#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

/// Runs `git` in `cwd` and asserts it succeeded.
///
/// Identity is supplied to setup commands and persisted after initialization
/// so application Git calls also work on machines without a global identity.
/// Each caller additionally pins `commit.gpgsign=false` on the repository.
pub fn run_git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    if args.first() == Some(&"init") {
        run_git(cwd, &["config", "--local", "user.name", "Test User"]);
        run_git(
            cwd,
            &["config", "--local", "user.email", "test@example.com"],
        );
    }
}
