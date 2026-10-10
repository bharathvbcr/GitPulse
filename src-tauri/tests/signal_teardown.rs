//! A SIGTERM runs the registered shutdown hook before the procguard sweep, and
//! the sweep takes the registered children with it.
//!
//! The signal watcher ends in `process::exit`, so this re-executes itself as
//! the process under test. The child registers a hook that leaves a marker
//! file naming whether the registered `sleep` was still alive when the hook
//! ran (the hook must come first: it stops what procguard does not own), then
//! parks. The parent signals it and checks the marker, the exit status and
//! that the sleep is gone.

#![cfg(unix)]

use gitpulse_lib::procguard::{self, LockedSpawn};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const ROLE: &str = "GITPULSE_SIGNAL_TEARDOWN_DIR";

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only probes.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn wait_for(within: Duration, done: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + within;
    while !done() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    done()
}

fn child_role(dir: PathBuf) -> ! {
    let guard = procguard::install_signal_handlers();
    assert!(guard.is_armed(), "{}", guard.describe());
    let (mut sleeper, registration) = procguard::spawn(
        Command::new("sleep").arg("300").stdin(Stdio::null()),
        "sleeper",
    )
    .expect("spawn");
    let pid = sleeper.id();
    std::fs::write(dir.join("sleeper"), pid.to_string()).expect("pid file");
    let hook_dir = dir.clone();
    procguard::set_shutdown_hook(move || {
        // The sweep has not run yet: the sleeper is still there.
        let seen = alive(i32::try_from(pid).expect("pid"));
        std::fs::write(
            hook_dir.join("hook"),
            if seen {
                "sleeper-alive"
            } else {
                "sleeper-gone"
            },
        )
        .expect("marker");
    });
    std::fs::write(dir.join("ready"), "").expect("ready");
    // Parked until the watcher exits the process.
    let _keep = (&mut sleeper, registration);
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[test]
fn sigterm_runs_the_hook_then_sweeps_registered_children() {
    if let Ok(dir) = std::env::var(ROLE) {
        child_role(PathBuf::from(dir));
    }
    let dir = tempfile::tempdir().expect("dir");
    let mut child = Command::new(std::env::current_exe().expect("exe"))
        .args([
            "sigterm_runs_the_hook_then_sweeps_registered_children",
            "--exact",
            "--nocapture",
        ])
        .env(ROLE, dir.path())
        .stdin(Stdio::null())
        .spawn_locked()
        .expect("re-exec");
    assert!(wait_for(Duration::from_secs(20), || dir
        .path()
        .join("ready")
        .exists()));
    let sleeper: i32 = std::fs::read_to_string(dir.path().join("sleeper"))
        .expect("pid")
        .parse()
        .expect("number");
    assert!(alive(sleeper));
    // SAFETY: signals the child this test spawned.
    unsafe { libc::kill(i32::try_from(child.id()).expect("pid"), libc::SIGTERM) };
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the process ignored SIGTERM"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(
        status.code(),
        Some(128 + libc::SIGTERM),
        "{status:?} {:?}",
        status.signal()
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("hook")).expect("the hook did not run"),
        "sleeper-alive",
        "the hook must run before the sweep"
    );
    assert!(
        wait_for(Duration::from_secs(3), || !alive(sleeper)),
        "the sweep left the sleeper"
    );
}
