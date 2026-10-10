//! Two `gitpulsed` processes never run against one profile.
//!
//! Before the daemon took a lock, a second copy started silently and wrote the
//! same ledger; an old daemon survived an upgrade. The real binary is started
//! twice against one scratch profile: the second must refuse with exit 3 and
//! name the first, and a start after the first is gone must be admitted.

#![cfg(unix)]

use gitpulse_lib::procguard::LockedSpawn;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn daemon(profile: &std::path::Path, repo: &std::path::Path, logs: &std::path::Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_gitpulsed"))
        .args(["--interval", "3600"])
        .arg(repo)
        .env("GITPULSE_LOG_DIR", logs)
        .env("HOME", profile)
        .env("XDG_CONFIG_HOME", profile)
        .env("APPDATA", profile)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn_locked()
        .expect("spawn gitpulsed")
}

fn wait_exit(child: &mut Child, within: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("wait") {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

#[test]
fn a_second_daemon_for_one_profile_is_refused_and_a_later_one_is_admitted() {
    let profile = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let dir = profile.path().to_path_buf();

    let mut first = daemon(&dir, repo.path(), logs.path());
    // The first is admitted once the lock names it.
    // `default_config_dir`, as the daemon computes it from this scratch HOME.
    let config = if cfg!(target_os = "macos") {
        dir.join("Library/Application Support/GitPulse/gitpulsed")
    } else {
        dir.join("gitpulse/gitpulsed")
    };
    let status = || gitpulse_lib::daemon_lock::running(&config);
    let deadline = Instant::now() + Duration::from_secs(20);
    let owner = loop {
        if let Ok(Some(owner)) = status() {
            if owner.pid == Some(first.id()) {
                break owner;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the first daemon never took the lock"
        );
        assert!(
            first.try_wait().unwrap().is_none(),
            "the first daemon exited early"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(owner.version.as_deref(), Some(env!("CARGO_PKG_VERSION")));

    let mut second = daemon(&dir, repo.path(), logs.path());
    let refused =
        wait_exit(&mut second, Duration::from_secs(20)).expect("the second daemon kept running");
    assert_eq!(refused.code(), Some(3), "{refused:?}");

    // SAFETY: signals the child this test spawned.
    unsafe { libc::kill(i32::try_from(first.id()).unwrap(), libc::SIGTERM) };
    wait_exit(&mut first, Duration::from_secs(20)).expect("the first daemon ignored SIGTERM");

    let mut third = daemon(&dir, repo.path(), logs.path());
    assert!(
        wait_exit(&mut third, Duration::from_millis(1500)).is_none(),
        "a daemon was refused after the first one ended"
    );
    // SAFETY: as above.
    unsafe { libc::kill(i32::try_from(third.id()).unwrap(), libc::SIGTERM) };
    wait_exit(&mut third, Duration::from_secs(20)).expect("the third daemon ignored SIGTERM");
}
