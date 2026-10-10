//! Long-lived background threads that report their own panics.
//!
//! The panic hook only logs, so a panic in a thread that nothing joins ended
//! its work silently: the cleaner's clock thread dying meant scheduled cleanup
//! stopped until the app restarted, with nothing in the UI to say so. A thread
//! started here runs its tick under `catch_unwind`, records a panic as a
//! *degraded subsystem* (what the diagnostics view lists), keeps ticking, and
//! clears the record at the next tick that completes.

use serde::Serialize;
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Most subsystems recorded at once, and the longest reason kept: a panic
/// payload is arbitrary text and is never trusted to be short.
const MAX_RECORDS: usize = 64;
const MAX_REASON_CHARS: usize = 300;

/// A subsystem whose last tick panicked and has not since completed one.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Degraded {
    pub subsystem: String,
    pub reason: String,
    pub since_unix: u64,
    /// Ticks that have panicked since the record was made, this one included.
    pub panics: u64,
}

static DEGRADED: Mutex<Vec<Degraded>> = Mutex::new(Vec::new());

fn records() -> std::sync::MutexGuard<'static, Vec<Degraded>> {
    DEGRADED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Locks `mutex`, taking the data as it was left if a holder panicked.
///
/// The one poison policy for state whose writers insert or replace whole
/// values (handles, caches, registries): a panic under the lock cannot leave
/// it half-updated, and refusing it afterwards would disable the subsystem
/// until restart. State with a multi-step invariant keeps its own check, and a
/// security decision (repository trust) fails closed instead of using this.
pub fn lock_or_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Every subsystem currently degraded, oldest first.
pub fn degraded() -> Vec<Degraded> {
    records().clone()
}

fn payload_text(payload: &(dyn std::any::Any + Send)) -> String {
    let text = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic with a non-text payload".into());
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_REASON_CHARS)
        .collect()
}

fn record(subsystem: &str, payload: &(dyn std::any::Any + Send)) {
    let reason = payload_text(payload);
    log::error!(target: "supervisor", "{subsystem} panicked and will be retried: {reason}");
    let mut all = records();
    if let Some(existing) = all.iter_mut().find(|d| d.subsystem == subsystem) {
        existing.reason = reason;
        existing.panics += 1;
        return;
    }
    if all.len() >= MAX_RECORDS {
        all.remove(0);
    }
    all.push(Degraded {
        subsystem: subsystem.to_owned(),
        reason,
        since_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        panics: 1,
    });
}

fn recover(subsystem: &str) {
    let mut all = records();
    if let Some(at) = all.iter().position(|d| d.subsystem == subsystem) {
        all.remove(at);
        log::info!(target: "supervisor", "{subsystem} recovered");
    }
}

/// Runs `tick` forever on a named thread, `interval` apart. A panicking tick
/// is recorded as degraded and the loop carries on; the next tick that returns
/// clears the record.
pub fn spawn_loop(
    name: &'static str,
    interval: Duration,
    mut tick: impl FnMut() + Send + 'static,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new().name(name.into()).spawn(move || loop {
        match catch_unwind(AssertUnwindSafe(&mut tick)) {
            Ok(()) => recover(name),
            Err(payload) => record(name, payload.as_ref()),
        }
        thread::sleep(interval);
    })
}

/// The Tauri command the diagnostics view reads.
#[tauri::command]
pub fn cmd_degraded_subsystems() -> Vec<Degraded> {
    degraded()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    fn wait(within: Duration, done: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + within;
        while !done() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        done()
    }

    /// The defect: a panic in a clock thread ended scheduled work with nothing
    /// to say so. A tick that panics is listed, the thread keeps ticking, and
    /// the listing clears when a tick completes again.
    #[test]
    fn a_panicking_tick_is_listed_then_recovers_and_the_loop_survives() {
        const NAME: &str = "supervisor-test-clock";
        let ticks = Arc::new(AtomicUsize::new(0));
        let seen = ticks.clone();
        spawn_loop(NAME, Duration::from_millis(5), move || {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            if (1..=2).contains(&n) {
                panic!("injected panic in tick {n}");
            }
        })
        .unwrap();
        let listed = || degraded().into_iter().find(|d| d.subsystem == NAME);
        assert!(wait(Duration::from_secs(5), || listed().is_some()));
        let first = listed().unwrap();
        assert!(first.reason.contains("injected panic"), "{first:?}");
        assert!(wait(Duration::from_secs(5), || {
            ticks.load(Ordering::SeqCst) >= 5 && listed().is_none()
        }), "the loop did not survive the panic and recover");
    }

    #[test]
    fn a_panic_reason_is_one_bounded_line() {
        let payload: Box<dyn std::any::Any + Send> =
            Box::new(format!("bad\nnews\u{1b}[31m{}", "x".repeat(10_000)));
        record("supervisor-test-bounded", payload.as_ref());
        let entry = degraded()
            .into_iter()
            .find(|d| d.subsystem == "supervisor-test-bounded")
            .unwrap();
        assert!(entry.reason.chars().count() <= MAX_REASON_CHARS);
        assert!(!entry.reason.chars().any(char::is_control));
        recover("supervisor-test-bounded");
    }
}
