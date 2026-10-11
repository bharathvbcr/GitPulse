//! Benchmark: what the command gate costs per call, with and without the app.
//!
//! Ignored by default; it measures, it does not assert. Run it against a
//! release hook and the real `manvi`, on a machine you are not using:
//!
//! ```text
//! cargo build --release --bin gitpulse-hook
//! GITPULSE_BENCH_HOOK=target/release/gitpulse-hook \
//! GITPULSE_BENCH_MANVI=$(command -v manvi) \
//! cargo test --release --test hook_gate_bench -- --ignored --nocapture
//! ```
//!
//! Optional: `GITPULSE_BENCH_LEDGER_REPO=/path/to/repo-with-a-ledger` adds the
//! live-ledger rows; `GITPULSE_BENCH_ROUNDS` (default 7) and
//! `GITPULSE_BENCH_CALLS` (default 30, per case per round) size the run.
//!
//! The cases are interleaved within every round and rotated between rounds, so
//! drift in machine load lands on all of them alike. Each round reports a
//! median; the table reports the minimum of those medians (the run least
//! disturbed by the machine) and their median.
//!
//! Components, from the cheapest up:
//! - `start`: the hook process alone (an empty command is answered before any
//!   lookup or harness);
//! - `bare/own`, `bare/app`: a directory with no repository, so no binding
//!   lookup, judged by the hook's own `manvi serve` and by the app;
//! - `repo/own`, `repo/app`: a repository with no ledger, which adds the
//!   binding lookup;
//! - `ledger/own`, `ledger/app`: a repository with a live ledger, if named.

#![cfg(target_os = "macos")]

#[path = "../src/test_support/env.rs"]
mod hermetic_env;

use gitpulse_lib::desktop::instance::{claim, Claim};
use gitpulse_lib::procguard::LockedSpawn;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn call(hook: &Path, home: &Path, manvi: &Path, logs: &Path, payload: &str) -> Duration {
    let started = Instant::now();
    let mut child = Command::new(hook)
        .arg("command-gate")
        .env(gitpulse_lib::logging::LOG_DIR_ENV, logs)
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
    let elapsed = started.elapsed();
    assert_eq!(out.status.code(), Some(0));
    elapsed
}

fn payload(cwd: &Path, command: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Bash",
        "tool_input": {"command": command},
    })
    .to_string()
}

fn median(samples: &mut [Duration]) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

#[test]
#[ignore = "a measurement, not a check; see the module docs"]
fn command_gate_cost_with_and_without_the_app() {
    let hook = std::env::var_os("GITPULSE_BENCH_HOOK")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_gitpulse-hook")));
    let manvi = PathBuf::from(
        std::env::var_os("GITPULSE_BENCH_MANVI").expect("GITPULSE_BENCH_MANVI names the manvi"),
    );
    // SAFETY: the only test in this binary; nothing else touches the environment.
    unsafe { std::env::set_var("GITPULSE_MANVI_BIN", &manvi) };
    let rounds = env_usize("GITPULSE_BENCH_ROUNDS", 7);
    let calls = env_usize("GITPULSE_BENCH_CALLS", 30);

    let own_home = tempfile::Builder::new()
        .prefix("gb")
        .tempdir_in("/tmp")
        .unwrap();
    let app_home = tempfile::Builder::new()
        .prefix("gb")
        .tempdir_in("/tmp")
        .unwrap();
    let own_home = own_home.path().canonicalize().unwrap();
    let app_home_path = app_home.path().canonicalize().unwrap();
    let profile = app_home_path.join("Library/Application Support/GitPulse");
    let owner = match claim(&profile, Duration::from_millis(300)) {
        Claim::Primary(owner) => owner,
        _ => panic!("expected to own the profile"),
    };
    owner.attach_gate(Arc::new(gitpulse_lib::hooks::serve_gate));

    let scratch = tempfile::tempdir().unwrap();
    let logs = scratch.path().join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let bare = scratch.path().join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    let repo = scratch.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(&repo)
        .status_locked()
        .unwrap()
        .success());
    let (bare, repo) = (bare.canonicalize().unwrap(), repo.canonicalize().unwrap());

    // (label, home, cwd, command)
    let mut cases: Vec<(&str, PathBuf, PathBuf, &str)> = vec![
        ("start", own_home.clone(), bare.clone(), ""),
        ("bare/own", own_home.clone(), bare.clone(), "git status"),
        ("bare/app", app_home_path.clone(), bare, "git status"),
        ("repo/own", own_home.clone(), repo.clone(), "git status"),
        ("repo/app", app_home_path.clone(), repo, "git status"),
    ];
    if let Some(ledger) = std::env::var_os("GITPULSE_BENCH_LEDGER_REPO").map(PathBuf::from) {
        cases.push(("ledger/own", own_home.clone(), ledger.clone(), "git status"));
        cases.push(("ledger/app", app_home_path.clone(), ledger, "git status"));
    }

    // Warm: the app's hook-lane child starts on its first verdict, and that
    // start is not what a steady stream of hooks pays.
    for (_, home, cwd, command) in &cases {
        call(&hook, home, &manvi, &logs, &payload(cwd, command));
    }

    let mut per_round: Vec<Vec<Duration>> = vec![Vec::new(); cases.len()];
    for round in 0..rounds {
        let mut samples: Vec<Vec<Duration>> = vec![Vec::new(); cases.len()];
        for _ in 0..calls {
            for offset in 0..cases.len() {
                let i = (offset + round) % cases.len();
                let (_, home, cwd, command) = &cases[i];
                samples[i].push(call(&hook, home, &manvi, &logs, &payload(cwd, command)));
            }
        }
        for (i, mut s) in samples.into_iter().enumerate() {
            per_round[i].push(median(&mut s));
        }
    }

    println!("\nhook: {}\nmanvi: {}", hook.display(), manvi.display());
    println!("rounds={rounds} calls/case/round={calls}");
    println!(
        "{:<12} {:>12} {:>14}",
        "case", "min median", "median median"
    );
    for (i, (label, ..)) in cases.iter().enumerate() {
        let mut medians = per_round[i].clone();
        let min = *medians.iter().min().unwrap();
        println!(
            "{label:<12} {:>9.2} ms {:>11.2} ms",
            min.as_secs_f64() * 1e3,
            median(&mut medians).as_secs_f64() * 1e3
        );
    }
}
