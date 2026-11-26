//! The hook process's exit leaves no harness state directory behind.
//!
//! `gitpulse-hook` ends through `procguard::exit`, which runs no destructor, so
//! the `gitpulse-harness-*` directory of the `manvi serve` it started would stay
//! in the temp root for ever. This runs the real binary several times against a
//! fake `manvi`, with a short `TMPDIR` of its own (a long one breaks the
//! Unix-socket tests), and counts what is left.
#![cfg(unix)]

// The hermetic test environment: a per-process home, git config and trust
// store, rewritten before `main`. See `src/test_support/env.rs`.
#[path = "../src/test_support/env.rs"]
mod hermetic_env;

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use waylode_lib::procguard::LockedSpawn;

const ROUNDS: usize = 5;

/// Answers every request, and records that it was started.
fn fake_manvi(path: &std::path::Path, started: &std::path::Path) {
    let script = format!(
        r#"#!/bin/sh
echo started >> '{}'
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/^{{"id":"\([0-9]*\)".*/\1/p')
  case "$line" in
    *'"op":"hello"'*) printf '{{"id":"%s","ok":true,"result":{{"protocol":1,"ops":["hello"],"posture":"host"}}}}\n' "$id" ;;
    *) printf '{{"id":"%s","ok":true,"result":{{}}}}\n' "$id" ;;
  esac
done
"#,
        started.display()
    );
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn harness_dirs(root: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("gitpulse-harness-"))
        .collect()
}

#[test]
fn a_hook_that_judged_a_command_leaves_no_state_directory() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let logs = work.path().join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let cwd = work.path().join("bare");
    std::fs::create_dir_all(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let started = work.path().join("started");
    let manvi = work.path().join("manvi");
    fake_manvi(&manvi, &started);
    // Short and its own, so the count is this test's alone.
    let tmp = tempfile::Builder::new()
        .prefix("gp-hk")
        .tempdir_in("/tmp")
        .unwrap();

    for round in 0..ROUNDS {
        let mut child = Command::new(env!("CARGO_BIN_EXE_waylode-hook"))
            .arg("command-gate")
            .env(waylode_lib::logging::LOG_DIR_ENV, &logs)
            .env("HOME", home.path())
            .env("TMPDIR", tmp.path())
            .env("GITPULSE_MANVI_BIN", &manvi)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn_locked()
            .expect("spawn gitpulse-hook");
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "cwd": cwd.to_string_lossy(),
            "tool_name": "Bash",
            "tool_input": {"command": "git status"},
        })
        .to_string();
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(payload.as_bytes())
            .expect("payload");
        child.wait_with_output().expect("collect");
        assert_eq!(
            harness_dirs(tmp.path()),
            Vec::<String>::new(),
            "round {round}: the hook left its state directory behind"
        );
    }
    let spawned = std::fs::read_to_string(&started).unwrap_or_default();
    assert_eq!(
        spawned.lines().count(),
        ROUNDS,
        "the hook did not start the fake manvi each round, so the check above proved nothing"
    );
}
