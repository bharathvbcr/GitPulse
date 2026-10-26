//! The signal blast-radius guard, at the real `gitpulse-hook command-gate`
//! boundary.
//!
//! The incident this exists for: an agent ran `pkill -f "sleep 60" -u bharath`.
//! macOS `pkill` stops option parsing at the first operand, so `-u` and the
//! user name became two more patterns OR'd with `sleep 60`, and with `-f` each
//! of them matched against every process's full argument list. Every process
//! with the home directory in its argv — about twenty hosted agent sessions
//! among them — took a SIGTERM.
//!
//! The decision logic is unit-tested in `src/hooks/signal_guard.rs` over
//! process tables written by hand. This file proves the same answer comes out
//! of the executable a host spawns, over the *real* process table, with
//! children this test started and marked itself. Nothing here ever signals
//! anything but those children: the hook only reads the table.

#![cfg(unix)]

// The hermetic test environment: a per-process home, git config and trust
// store, rewritten before `main`. See `src/test_support/env.rs`.
#[path = "../src/test_support/env.rs"]
mod hermetic_env;

#[path = "common/marked_children.rs"]
mod marked_children;

use gitpulse_lib::procguard::LockedSpawn;
use marked_children::Marked;
use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const HOOK: &str = env!("CARGO_BIN_EXE_gitpulse-hook");
const HANG: Duration = Duration::from_secs(60);

/// One `command-gate` answer for a Bash command, as parsed JSON.
fn gate(command: &str) -> (Option<i32>, Option<Value>) {
    let cwd = tempfile::tempdir().expect("cwd");
    let logs = tempfile::tempdir().expect("hook log dir");
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "signal-guard-test",
        "cwd": cwd.path(),
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut child = Command::new(HOOK)
        .arg("command-gate")
        .env(gitpulse_lib::logging::LOG_DIR_ENV, logs.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn_locked()
        .expect("spawn gitpulse-hook");
    let mut stdin = child.stdin.take().expect("stdin");
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(payload.as_bytes());
    });
    let started = Instant::now();
    let output = loop {
        match child.try_wait().expect("wait") {
            Some(_) => break child.wait_with_output().expect("collect"),
            None if started.elapsed() > HANG => {
                child.kill().expect("kill");
                let _ = child.wait();
                panic!("command-gate did not answer within {HANG:?}");
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    let _ = writer.join();
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let json = (!text.is_empty()).then(|| {
        serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {text:?}"))
    });
    (output.status.code(), json)
}

fn decision(answer: &Option<Value>) -> Option<String> {
    answer
        .as_ref()?
        .pointer("/hookSpecificOutput/permissionDecision")?
        .as_str()
        .map(str::to_string)
}

fn reason(answer: &Option<Value>) -> String {
    answer
        .as_ref()
        .and_then(|v| v.pointer("/hookSpecificOutput/permissionDecisionReason"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The incident's shape: the option after the operand turns into a pattern.
/// Thirty marked processes is over the guard's bound whatever else is running,
/// so this holds on any machine.
#[test]
fn a_pkill_whose_late_option_becomes_a_pattern_is_denied_with_the_fix() {
    let marked = Marked::spawn(30);
    let command = format!("pkill {} -f", marked.marker);
    let (code, answer) = gate(&command);
    assert_eq!(code, Some(0), "a hook always exits 0");
    assert_eq!(
        decision(&answer).as_deref(),
        Some("deny"),
        "`{command}` would signal at least {} processes; answer: {answer:?}",
        marked.pids().len()
    );
    let reason = reason(&answer);
    assert!(
        reason.contains(&marked.marker) && reason.contains("\"-f\""),
        "the reason names the patterns pkill parsed: {reason}"
    );
    assert!(
        reason.contains(&format!("pkill -f {}", marked.marker)),
        "the reason carries the corrected command: {reason}"
    );
}

/// More than the bound is refused even when every pattern was meant.
#[test]
fn a_pattern_matching_more_processes_than_the_bound_is_denied() {
    let marked = Marked::spawn(30);
    let (_, answer) = gate(&format!("pkill -f {}", marked.marker));
    assert_eq!(decision(&answer).as_deref(), Some("deny"), "{answer:?}");
    assert!(reason(&answer).contains("30"), "{}", reason(&answer));
}

/// A narrow pattern is the ordinary case: no decision, so the user's own
/// permission rules run.
#[test]
fn a_pattern_matching_a_few_marked_processes_leaves_the_decision_to_the_user() {
    let marked = Marked::spawn(3);
    let (code, answer) = gate(&format!("pkill -f {}", marked.marker));
    assert_eq!(code, Some(0));
    assert_eq!(decision(&answer), None, "{answer:?}");
}

/// `kill -- -1` and `kill 0` have no pattern to evaluate and a scope that
/// includes every process the shell can reach.
#[test]
fn a_kill_of_every_process_is_denied() {
    for command in ["kill -- -1", "kill -TERM 0", "kill -9 -1"] {
        let (_, answer) = gate(command);
        assert_eq!(
            decision(&answer).as_deref(),
            Some("deny"),
            "`{command}`: {answer:?}"
        );
    }
}

/// The same call judged by two hook processes at once, as when a plugin and an
/// injected settings entry both register the hook: the guard is stateless, so
/// both reach the same refusal and neither can suppress the other's.
#[test]
fn two_hook_processes_on_the_same_call_both_deny() {
    let marked = Marked::spawn(30);
    let command = format!("pkill -f {}", marked.marker);
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let command = command.clone();
            std::thread::spawn(move || gate(&command))
        })
        .collect();
    for thread in threads {
        let (code, answer) = thread.join().expect("hook thread");
        assert_eq!(code, Some(0));
        assert_eq!(decision(&answer).as_deref(), Some("deny"), "{answer:?}");
    }
}

/// Killing one explicit pid is none of this guard's business.
#[test]
fn a_kill_of_one_explicit_pid_is_not_a_decision() {
    let marked = Marked::spawn(1);
    let (_, answer) = gate(&format!("kill {}", marked.pids()[0]));
    assert_eq!(decision(&answer), None, "{answer:?}");
}
