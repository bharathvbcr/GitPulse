//! The one door `benches/idle_wakeups.rs` uses to run the external-changes
//! loop.
//!
//! The running app starts the loop with an `AppHandle`, which needs a window
//! system a bench does not have, and what that bench measures is the loop's
//! schedule. [`IdleLoop`] runs the same [`drive`] over a real clock, a real
//! [`Watcher`] on a private profile, and a window that is whatever the caller
//! says. It adds no logic of its own; the window's answer, the clock and the
//! announcement are the only things replaced.

use super::{drive, Env, Watcher};
use crate::workbench::retention::Retention;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Idle {
    profile: PathBuf,
    open: Arc<AtomicBool>,
    visible: Arc<AtomicBool>,
    pauses: Arc<AtomicU64>,
    watcher: Watcher,
    retention: Retention,
}

impl Env for Idle {
    fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn retention_tick(&mut self, now: Instant) {
        // Retention's clock runs; its pass needs a host profile and is not
        // what a wakeup count is about.
        self.retention.due_to_check(now);
    }
    fn query_window(&self) -> bool {
        self.visible.load(Ordering::Acquire)
    }
    fn poll(&mut self) -> Result<bool, String> {
        self.watcher.poll(&self.profile)
    }
    fn announce(&self) -> Result<(), String> {
        Ok(())
    }
    fn pause(&self, period: Duration) {
        self.pauses.fetch_add(1, Ordering::Relaxed);
        std::thread::sleep(period);
    }
}

/// A running loop over a private profile, with a window the caller controls.
pub struct IdleLoop {
    open: Arc<AtomicBool>,
    pauses: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl IdleLoop {
    /// Creates a profile database in `dir` and starts the loop on it, with the
    /// window `visible` or not.
    pub fn start(dir: &std::path::Path, visible: bool) -> Result<Self, String> {
        let profile = dir.join("workbench.sqlite");
        let connection = rusqlite::Connection::open(&profile).map_err(|e| e.to_string())?;
        connection
            .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS t(v INTEGER);")
            .map_err(|e| e.to_string())?;
        drop(connection);
        let open = Arc::new(AtomicBool::new(true));
        let pauses = Arc::new(AtomicU64::new(0));
        let mut env = Idle {
            profile,
            open: Arc::clone(&open),
            visible: Arc::new(AtomicBool::new(visible)),
            pauses: Arc::clone(&pauses),
            watcher: Watcher::default(),
            retention: Retention::new(),
        };
        let thread = std::thread::Builder::new()
            .name("bench-external-changes".into())
            .spawn(move || drive(&mut env))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            open,
            pauses,
            thread: Some(thread),
        })
    }

    /// How many times the loop has gone round: one per pause it took.
    pub fn rounds(&self) -> u64 {
        self.pauses.load(Ordering::Relaxed)
    }

    /// Asks the loop to end and waits for it.
    pub fn stop(mut self) {
        self.open.store(false, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
