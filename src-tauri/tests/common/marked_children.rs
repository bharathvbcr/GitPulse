//! Child processes a test starts and marks, so a signal guard can be measured
//! against the real process table without ever signalling anything else.
//!
//! Each child is `/bin/sleep 600` with a unique marker as its `argv[0]`, so the
//! marker is both its name and the head of its full argument list. They are
//! killed, by the pids this test holds, when the group is dropped.

use gitpulse_lib::procguard::LockedSpawn;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct Marked {
    pub marker: String,
    children: Vec<Child>,
}

impl Marked {
    pub fn spawn(count: usize) -> Self {
        static SEQUENCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let marker = format!(
            "gpsg{}x{}x{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos()),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let children = (0..count)
            .map(|_| {
                Command::new("/bin/sleep")
                    .arg0(&marker)
                    .arg("600")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn_locked()
                    .expect("spawn a marked sleeper")
            })
            .collect::<Vec<_>>();
        // `arg0` is applied at exec; wait until every child shows it.
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(20) {
            if pgrep(&["-f", &marker]).len() == count {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Self { marker, children }
    }

    pub fn pids(&self) -> Vec<i32> {
        let mut pids: Vec<i32> = self
            .children
            .iter()
            .filter_map(|c| i32::try_from(c.id()).ok())
            .collect();
        pids.sort_unstable();
        pids
    }
}

impl Drop for Marked {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The real `pgrep`, read-only, with exactly the arguments given.
pub fn pgrep(args: &[&str]) -> Vec<i32> {
    let output = Command::new("/usr/bin/pgrep")
        .args(args)
        .output_locked()
        .expect("run pgrep");
    let mut pids: Vec<i32> = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .filter_map(|p| p.parse().ok())
        .collect();
    pids.sort_unstable();
    pids
}
