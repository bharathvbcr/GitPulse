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

use gitpulse_lib::procguard::LockedSpawn;
use std::path::Path;

/// The test environment owner: the hermetic home every test process runs in,
/// the one environment lock, and the scoped `bind_env` guard. Declaring `common`
/// is what makes a crate hermetic, so a new suite gets it with the helpers it
/// already needs; `tests/hermetic_contract.rs` fails a suite that has neither.
#[path = "../../src/test_support/env.rs"]
pub mod env;
use std::process::Command;

/// Lifts the spawn rate cap for this test process, so a fixture that reaches
/// the library's git gate is not deferred by it.
///
/// A `tests/*.rs` crate links the library without `cfg(test)`, so it runs under
/// the production spawn rate: a 192-token burst a loaded parallel run drains,
/// which defers a `GitReader` call mid-test ("local bucket: 0 token(s)") for a
/// reason that has nothing to do with the code under test. Call it first in
/// every fixture entry point of a suite that calls the library in-process.
/// Order-independent and idempotent. Not for the suites that measure the gate
/// itself (`tests/process_admission.rs` lists them).
pub fn unmetered_spawns() {
    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
}

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
        .output_locked()
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
        trust_repo(cwd);
    }
}

/// The test owns this fixture and explicitly admits it through the real trust
/// boundary. This is a session grant, never a bypass or a user's saved grant —
/// unless the terminal suites run against the terminal host: a session grant
/// never reaches another process, so there it is saved, in the hermetic home
/// both processes read.
pub fn trust_repo(repo: &Path) {
    let path = repo.to_str().expect("fixture path");
    let preview = gitpulse_lib::repository_trust::inspect(path).expect("inspect fixture");
    gitpulse_lib::repository_trust::grant(path, &preview.identity, terminal_host_selected())
        .expect("approve fixture");
}

/// Which backend the terminal suites run against:
/// `GITPULSE_TEST_TERMINAL_BACKEND=host` runs each of them against a real
/// `gitpulse-ptyd`; anything else, in-process.
pub fn terminal_host_selected() -> bool {
    std::env::var("GITPULSE_TEST_TERMINAL_BACKEND").as_deref() == Ok("host")
}

/// A session registry on the selected backend. A host registry gets a host of
/// its own, in a directory of its own, so suites running side by side never
/// share sessions.
pub fn terminals() -> gitpulse_lib::terminal::TerminalSessions {
    #[cfg(unix)]
    if terminal_host_selected() {
        let base = std::env::temp_dir().canonicalize().expect("temp dir");
        // Kept for the life of the test process: the host exits on its own
        // once its registry is gone and it holds no session.
        let dir = tempfile::Builder::new()
            .prefix("ph")
            .tempdir_in(base)
            .expect("host dir")
            .keep();
        return gitpulse_lib::terminal::TerminalSessions::with_host(
            gitpulse_lib::terminal::host::client::HostConfig {
                binary: Some(env!("CARGO_BIN_EXE_gitpulse-ptyd").into()),
                idle_exit: Some(std::time::Duration::from_secs(3)),
                // GITPULSE_LOG_DIR for the host: never the user's real log.
                log_dir: Some(dir.join("logs")),
                dir,
            },
        );
    }
    gitpulse_lib::terminal::TerminalSessions::default()
}

/// Every shipped binary's entry file, relative to the crate, derived two
/// independent ways that must agree: the `[[bin]]` paths `Cargo.toml` builds,
/// and the files a binary can live in (`src/main.rs` plus `src/bin/*.rs`). A
/// `[[bin]]` with a custom path, or a new file in `src/bin/` that Cargo picks
/// up on its own, fails here instead of escaping the guards below.
pub fn shipped_binaries() -> std::collections::BTreeSet<String> {
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    let mut declared = std::collections::BTreeSet::new();
    let mut in_bin = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_bin = line == "[[bin]]";
        } else if in_bin {
            if let Some(path) = line
                .strip_prefix("path")
                .map(str::trim_start)
                .and_then(|rest| rest.strip_prefix('='))
            {
                declared.insert(path.trim().trim_matches('"').to_string());
            }
        }
    }
    let mut on_disk = std::collections::BTreeSet::from(["src/main.rs".to_string()]);
    for entry in std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bin")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            let name = path.file_name().unwrap().to_string_lossy();
            on_disk.insert(format!("src/bin/{name}"));
        }
    }
    assert_eq!(
        declared, on_disk,
        "Cargo.toml's [[bin]] paths and src/main.rs + src/bin/*.rs disagree"
    );
    assert!(
        declared.len() >= 3,
        "derived only {declared:?}: the walk or the parse is broken"
    );
    declared
}
