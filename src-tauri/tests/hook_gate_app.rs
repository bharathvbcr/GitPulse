//! The command gate over the running app's mailbox, at the process boundary.
//!
//! One test, run sequentially, because the app half is this process and it
//! finds its `manvi` through `GITPULSE_MANVI_BIN`, which is process-global.
//!
//! What it pins, with real `gitpulse-hook` processes and a fake `manvi` that
//! records every start:
//!
//! 1. While the app owns the mailbox, N command-gate hooks start no `manvi`
//!    of their own. The only starts are the app's one long-lived child.
//! 2. With the app gone the hooks start their own, as they always did.
//! 3. The answer is the same either way, and a force push is denied either way.
//! 4. 64 concurrent hooks, past the app's concurrency cap, all still deny.

#![cfg(target_os = "macos")]

#[path = "../src/test_support/env.rs"]
mod hermetic_env;

use gitpulse_lib::desktop::instance::{claim, Claim};
use gitpulse_lib::procguard::LockedSpawn;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

const HOOK: &str = env!("CARGO_BIN_EXE_gitpulse-hook");

/// Logs the parent pid of every start, then answers as Manvi does: a force
/// push is denied, anything else is a demoted allow.
const FAKE_MANVI: &str = r#"#!/bin/sh
echo "$PPID" >> "@STARTS@"
reply() {
  id=$(printf '%s' "$1" | sed -n 's/^{"id":"\([0-9]*\)".*/\1/p')
  printf '{"id":"%s","ok":true,"result":%s}\n' "$id" "$2"
}
IFS= read -r line || exit 1
reply "$line" '{"protocol":1,"ops":["hello","policy.check.command","policy.check.file"],"posture":"host"}'
while IFS= read -r line; do
  case "$line" in
    *'--force'*) reply "$line" '{"action":"deny","rule":"command.force_push","severity":"hard","reason":"Force pushes are not allowed.","target":"git push --force","task_id":"host-scope","demoted":"","degraded":[]}' ;;
    *) reply "$line" '{"action":"allow","rule":"command.not_allowed","severity":"soft","reason":"not listed","target":"x","task_id":"host-scope","demoted":"serve.posture=host: allowlist not enforced","degraded":[]}' ;;
  esac
done
"#;

fn payload(cwd: &Path, command: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Bash",
        "tool_input": {"command": command},
    })
    .to_string()
}

fn hook(home: &Path, manvi: &Path, payload: &str) -> serde_json::Value {
    let logs = tempfile::tempdir().expect("hook log dir");
    let mut child = Command::new(HOOK)
        .arg("command-gate")
        .env(gitpulse_lib::logging::LOG_DIR_ENV, logs.path())
        .env("HOME", home)
        .env("GITPULSE_MANVI_BIN", manvi)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn_locked()
        .expect("spawn gitpulse-hook");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("payload");
    let out = child.wait_with_output().expect("collect");
    assert_eq!(out.status.code(), Some(0));
    serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null)
}

fn decision(answer: &serde_json::Value) -> Option<&str> {
    answer["hookSpecificOutput"]["permissionDecision"].as_str()
}

fn starts(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn hooks_ask_the_app_and_fall_back_to_their_own_manvi() {
    use std::os::unix::fs::PermissionsExt;
    // A short path: the mailbox socket must fit in `sun_path`.
    let home = tempfile::Builder::new()
        .prefix("gh")
        .tempdir_in("/tmp")
        .expect("home");
    let home_path = home.path().canonicalize().unwrap();
    let scratch = tempfile::tempdir().expect("scratch");
    let log = scratch.path().join("starts.log");
    let manvi = scratch.path().join("manvi");
    std::fs::write(
        &manvi,
        FAKE_MANVI.replace("@STARTS@", &log.display().to_string()),
    )
    .unwrap();
    std::fs::set_permissions(&manvi, std::fs::Permissions::from_mode(0o755)).unwrap();
    // SAFETY: the only test in this binary, so nothing else reads or writes
    // the environment concurrently.
    unsafe { std::env::set_var("GITPULSE_MANVI_BIN", &manvi) };

    let repo = scratch.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    let force = payload(&repo, "git push --force origin main");
    let benign = payload(&repo, "ls");

    // Without the app: every hook starts a manvi of its own.
    let without = hook(&home_path, &manvi, &force);
    assert_eq!(decision(&without), Some("deny"), "{without}");
    let alone = starts(&log).len();
    assert!(alone >= 1, "the fallback never started a manvi");
    let without_benign = hook(&home_path, &manvi, &benign);

    // With the app: none do.
    let profile = home_path.join("Library/Application Support/GitPulse");
    let owner = match claim(&profile, Duration::from_millis(300)) {
        Claim::Primary(owner) => owner,
        _ => panic!("expected to own the profile"),
    };
    owner.attach_gate(Arc::new(gitpulse_lib::hooks::serve_gate));
    std::fs::write(&log, "").unwrap();
    for _ in 0..12 {
        let answer = hook(&home_path, &manvi, &force);
        assert_eq!(decision(&answer), Some("deny"), "{answer}");
    }
    let with_benign = hook(&home_path, &manvi, &benign);
    assert_eq!(
        with_benign, without_benign,
        "the app changed an allow's answer"
    );
    let parents = starts(&log);
    let me = std::process::id().to_string();
    assert_eq!(
        parents,
        vec![me],
        "while the app owns the mailbox the only manvi start is the app's own long-lived child"
    );

    // Past the app's concurrency cap: the excess is judged by the hooks
    // themselves, and every one still denies.
    let workers: Vec<_> = (0..64)
        .map(|_| {
            let (home, manvi, force) = (home_path.clone(), manvi.clone(), force.clone());
            std::thread::spawn(move || hook(&home, &manvi, &force))
        })
        .collect();
    for worker in workers {
        let answer = worker.join().unwrap();
        assert_eq!(decision(&answer), Some("deny"), "{answer}");
    }

    // The app goes away: back to a manvi per hook.
    drop(owner);
    std::fs::write(&log, "").unwrap();
    let after = hook(&home_path, &manvi, &force);
    assert_eq!(decision(&after), Some("deny"), "{after}");
    assert!(!starts(&log).is_empty(), "no fallback after the app left");
}
