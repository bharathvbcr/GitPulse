//! Claude Code's `WorktreeCreate` / `WorktreeRemove`, end to end: the plugin's
//! launcher (`plugins/gitpulse/hooks/gitpulse-worktree.sh`) running the real
//! `gitpulse-hook` binary, as the shipped manifest does.
//!
//! What it pins, at the process boundary the unit tests cannot reach:
//!
//! 1. In a trusted repository the worktree is made by GitPulse (its build
//!    caches are copied) and stdout is the path and nothing else: no log
//!    line, no JSON, no trailing noise Claude Code would read as the path.
//! 2. The add went through the command gate and is in the repository's ledger.
//! 3. In a repository GitPulse does not trust, the binary does nothing (exit
//!    75) and the launcher's plain git still gives Claude Code its worktree.
//! 4. Removal goes through GitPulse and takes the hook's branch with it.

#![cfg(unix)]

mod common;
#[path = "common/process_trust.rs"]
mod process_trust;

use gitpulse_lib::procguard::LockedSpawn;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const HOOK: &str = env!("CARGO_BIN_EXE_gitpulse-hook");

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
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
        .stdin(Stdio::null())
        .output_locked()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repository on `main` with an ignored `node_modules/` holding one file.
fn repository(parent: &Path) -> PathBuf {
    let repo = parent.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "node_modules/\n").unwrap();
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "initial"]);
    std::fs::create_dir_all(repo.join("node_modules/pkg")).unwrap();
    std::fs::write(repo.join("node_modules/pkg/index.js"), "x").unwrap();
    repo
}

fn launch(home: &Path, event: &str, payload: serde_json::Value, cwd: &Path) -> Output {
    let launcher = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../plugins/gitpulse/hooks/gitpulse-worktree.sh");
    let logs = tempfile::tempdir().unwrap();
    let mut child = Command::new("sh")
        .arg(launcher)
        .args([HOOK, event])
        .current_dir(cwd)
        .envs(process_trust::profile_env(home))
        .env(gitpulse_lib::logging::LOG_DIR_ENV, logs.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn_locked()
        .expect("sh runs");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn a_trusted_repository_gets_a_gitpulse_worktree_and_an_untrusted_one_a_plain_one() {
    common::unmetered_spawns();
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("home");

    // Trusted: GitPulse makes it.
    let repo = repository(&fixture.path().join("trusted"));
    process_trust::approve(&repo, &home);
    let before = gitpulse_lib::ledger::latest_cursor(repo.to_str().unwrap()).unwrap_or(0);
    let out = launch(
        &home,
        "worktree-create",
        serde_json::json!({"hook_event_name": "WorktreeCreate", "cwd": repo, "name": "bold-oak-a3f2"}),
        &repo,
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    let path = repo.join(".claude/worktrees/bold-oak-a3f2");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{}\n", path.display()),
        "{stderr}"
    );
    assert!(stderr.contains("through GitPulse"), "{stderr}");
    assert!(
        path.join("node_modules/pkg/index.js").is_file(),
        "the caches were not copied"
    );
    assert_eq!(
        git(&path, &["symbolic-ref", "--short", "HEAD"]),
        "worktree-bold-oak-a3f2"
    );

    let events = gitpulse_lib::ledger::tail(repo.to_str().unwrap(), before, 50).expect("ledger");
    assert!(
        events
            .iter()
            .any(|e| e.argv_json.as_deref().is_some_and(|argv| argv
                .contains("\"worktree\",\"add\"")
                && argv.contains("bold-oak-a3f2"))),
        "the add is not in the ledger: {events:?}"
    );

    // Registered twice (plugin plus a settings copy), both copies run at once
    // for one worktree: both print the same path, and one of them made it.
    let payload =
        serde_json::json!({"hook_event_name": "WorktreeCreate", "cwd": repo, "name": "twin"});
    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| launch(&home, "worktree-create", payload.clone(), &repo));
        let b = scope.spawn(|| launch(&home, "worktree-create", payload.clone(), &repo));
        (a.join().unwrap(), b.join().unwrap())
    });
    let twin = format!("{}\n", repo.join(".claude/worktrees/twin").display());
    for out in [&a, &b] {
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{stderr}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), twin, "{stderr}");
    }
    let handed_over = [&a, &b]
        .iter()
        .filter(|out| String::from_utf8_lossy(&out.stderr).contains("already made"))
        .count();
    assert_eq!(handed_over, 1, "exactly one run made it");

    // And GitPulse takes it away again, with its branch.
    let out = launch(
        &home,
        "worktree-remove",
        serde_json::json!({"hook_event_name": "WorktreeRemove", "cwd": repo, "worktree_path": path}),
        &repo,
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(out.stdout.is_empty());
    assert!(!path.exists());
    assert!(
        stderr.contains("deleted branch worktree-bold-oak-a3f2"),
        "{stderr}"
    );

    // Untrusted: GitPulse does nothing, plain git does the job.
    let repo = repository(&fixture.path().join("untrusted"));
    let out = launch(
        &home,
        "worktree-create",
        serde_json::json!({"hook_event_name": "WorktreeCreate", "cwd": repo, "name": "plain-elm"}),
        &repo,
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    let path = repo.join(".claude/worktrees/plain-elm");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{}\n", path.display()),
        "{stderr}"
    );
    assert!(stderr.contains("is not trusted"), "{stderr}");
    assert!(stderr.contains("plain git will"), "{stderr}");
    assert!(
        !path.join("node_modules").exists(),
        "plain git copies nothing"
    );
}
