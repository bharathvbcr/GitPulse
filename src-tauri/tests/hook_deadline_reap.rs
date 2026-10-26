//! The real `gitpulse-hook`, driven against a `git` that stalls: the hook has
//! one deadline under the host's timeout and leaves no child behind.
//!
//! The unit tests prove each wait is clamped; this proves the process. Two
//! faults reproduced against the unmodified hook (2026-10-09 audit): it exited
//! after its 5 s budget with the stalled `git` still alive (never reaped before
//! `process::exit`), and a stdin that arrived late left the later waits their
//! full `BUDGET`, so the total reached the host's 10 s timeout.

#![cfg(unix)]

mod common;
#[path = "common/process_trust.rs"]
mod process_trust;

use gitpulse_lib::hooks::{HOST_TIMEOUT_MARGIN, TOTAL_BUDGET};
use gitpulse_lib::procguard::LockedSpawn;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const HOOK: &str = env!("CARGO_BIN_EXE_gitpulse-hook");

/// How late the host delivers the payload. Past the point where two full
/// `BUDGET` waits (5 s each) would end at the host's 10 s timeout.
const STDIN_DELAY: Duration = Duration::from_millis(4800);

/// The pid a fixture recorded, as a number `kill` can only aim at one process:
/// 0, 1 and negatives address groups, init or everything, and an unparsable
/// line names nothing.
fn recorded_pid(text: &str) -> Option<i32> {
    text.trim().parse::<i32>().ok().filter(|pid| *pid > 1)
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only probes for existence and permission.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Whether `pid` is still the fixture's `sleep 60`. The pid was recorded
/// seconds ago by a process that has since exited, so a live process under that
/// number may be a stranger's; only the fixture's own command line is ours.
fn is_the_fixture_sleep(pid: i32) -> bool {
    Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output_locked()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "sleep 60")
}

#[test]
fn a_stalled_child_is_reaped_and_the_total_stays_under_the_host_timeout() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let status = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo.path())
        .status_locked()
        .unwrap();
    assert!(status.success());
    process_trust::approve(repo.path(), home.path());

    // A git that records its pid and then stalls past every budget.
    let bin = tempfile::tempdir().unwrap();
    let pids = bin.path().join("pids");
    let fake = bin.path().join("git");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\necho $$ >> '{}'\nexec sleep 60\n",
            pids.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").unwrap()
    );

    let logs = tempfile::tempdir().unwrap();
    let mut child = Command::new(HOOK)
        .arg("session-brief")
        .env("PATH", &path)
        .envs(process_trust::profile_env(home.path()))
        .env(gitpulse_lib::logging::LOG_DIR_ENV, logs.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn_locked()
        .unwrap();
    let started = Instant::now();
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "deadline",
        "cwd": repo.path(),
        "source": "startup",
    })
    .to_string();
    std::thread::sleep(STDIN_DELAY);
    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(payload.as_bytes()).unwrap();
    }
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            started.elapsed() < common::env::coverage_relaxed(Duration::from_secs(30)),
            "hook hung"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let hook_secs = started.elapsed();
    std::thread::sleep(Duration::from_millis(500));

    let recorded = std::fs::read_to_string(&pids).unwrap_or_default();
    let pid_list: Vec<i32> = recorded.lines().filter_map(recorded_pid).collect();
    let survivors: Vec<i32> = pid_list.iter().copied().filter(|pid| alive(*pid)).collect();
    for pid in survivors.iter().filter(|pid| is_the_fixture_sleep(**pid)) {
        // SAFETY: a pid above 1 whose command line is the fixture's own.
        unsafe { libc::kill(*pid, libc::SIGKILL) };
    }
    assert!(
        !pid_list.is_empty(),
        "precondition: the hook never ran the fake git (exited after {hook_secs:?})"
    );
    assert!(
        survivors.is_empty(),
        "hook exited after {hook_secs:?} leaving git children alive: {survivors:?} of {pid_list:?}"
    );
    // One deadline from process start: the late stdin and the stalled scan share
    // TOTAL_BUDGET, with the reap and print inside the margin before the host's
    // 10 s timeout.
    let ceiling = TOTAL_BUDGET + HOST_TIMEOUT_MARGIN - Duration::from_millis(800);
    assert!(
        hook_secs < ceiling,
        "hook took {hook_secs:?}; the one deadline is {TOTAL_BUDGET:?} plus a reap and a print, under {ceiling:?}"
    );
}
