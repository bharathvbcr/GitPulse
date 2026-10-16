//! The one door `benches/idle_wakeups.rs` uses to run the external-changes
//! loop.
//!
//! The running app starts the loop with an `AppHandle`, which needs a window
//! system a bench does not have, and what that bench measures is the loop's
//! schedule. [`IdleLoop`] runs the same [`drive`] over a real clock, a real
//! [`Watcher`] on a private profile and a [`Beacon`] of its own, with a window
//! that is whatever the caller says. It adds no logic of its own; the window's
//! answer and the announcement are the only things replaced. The unit tests use
//! it too, for the properties that need a real thread and a real wait.

use super::{drive, Beacon, Env, Watcher};
use crate::workbench::retention::Retention;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Default)]
struct Counts {
    waits: AtomicU64,
    polls: AtomicU64,
}

struct Idle {
    profile: PathBuf,
    open: Arc<AtomicBool>,
    visible: Arc<AtomicBool>,
    counts: Arc<Counts>,
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
    fn retention_next_in(&self, now: Instant) -> Duration {
        self.retention.next_check_in(now)
    }
    fn query_window(&self) -> bool {
        self.visible.load(Ordering::Acquire)
    }
    fn poll(&mut self) -> Result<bool, String> {
        self.counts.polls.fetch_add(1, Ordering::Relaxed);
        self.watcher.poll(&self.profile)
    }
    fn announce(&self) -> Result<(), String> {
        Ok(())
    }
    fn wait(&mut self, beacon: &Beacon, seen: u64, timeout: Duration) {
        self.counts.waits.fetch_add(1, Ordering::Relaxed);
        beacon.wait(seen, timeout);
    }
}

/// A running loop over a private profile, with a window the caller controls.
pub struct IdleLoop {
    open: Arc<AtomicBool>,
    visible: Arc<AtomicBool>,
    beacon: Arc<Beacon>,
    counts: Arc<Counts>,
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
        let visible = Arc::new(AtomicBool::new(visible));
        let beacon = Arc::new(Beacon::new(visible.load(Ordering::Acquire)));
        let counts = Arc::new(Counts::default());
        let mut env = Idle {
            profile,
            open: Arc::clone(&open),
            visible: Arc::clone(&visible),
            counts: Arc::clone(&counts),
            watcher: Watcher::default(),
            retention: Retention::new(),
        };
        let thread = crate::supervisor::spawn_once(
            "bench-external-changes",
            "workbench external changes (bench)",
            {
                let beacon = Arc::clone(&beacon);
                move || drive(&mut env, &beacon)
            },
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            open,
            visible,
            beacon,
            counts,
            thread: Some(thread),
        })
    }

    /// The window was shown or hidden, as an event says it.
    pub fn set_visible(&self, visible: bool) {
        self.visible.store(visible, Ordering::Release);
        self.beacon.set_on_screen(visible);
    }

    /// How many times the loop has gone to wait: one per round.
    pub fn rounds(&self) -> u64 {
        self.counts.waits.load(Ordering::Relaxed)
    }

    /// How many watcher passes the loop has made.
    pub fn passes(&self) -> u64 {
        self.counts.polls.load(Ordering::Relaxed)
    }

    /// Closes the host and waits for the loop to notice.
    pub fn stop(mut self) {
        self.open.store(false, Ordering::Release);
        self.beacon.wake();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
