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
    /// The thread is gone: its policy is not to restart it, or it panicked
    /// too often too fast to keep restarting. Nothing is retrying it.
    pub stopped: bool,
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

pub(crate) fn payload_text(payload: &(dyn std::any::Any + Send)) -> String {
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
    record_with(subsystem, payload, false);
}

fn record_with(subsystem: &str, payload: &(dyn std::any::Any + Send), stopped: bool) {
    let reason = payload_text(payload);
    if stopped {
        log::error!(target: "supervisor", "{subsystem} panicked and has stopped: {reason}");
    } else {
        log::error!(target: "supervisor", "{subsystem} panicked and will be retried: {reason}");
    }
    let mut all = records();
    if let Some(existing) = all.iter_mut().find(|d| d.subsystem == subsystem) {
        existing.reason = reason;
        existing.panics += 1;
        existing.stopped = stopped;
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
        stopped,
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
    thread::Builder::new()
        .name(name.into())
        .spawn(move || loop {
            match catch_unwind(AssertUnwindSafe(&mut tick)) {
                Ok(()) => recover(name),
                Err(payload) => record(name, payload.as_ref()),
            }
            thread::sleep(interval);
        })
}

/// What a supervised thread does after its body panics.
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    /// First wait before running the body again; doubles per consecutive fast
    /// failure up to `cap`.
    pub base: Duration,
    pub cap: Duration,
    /// A body that ran at least this long before panicking counts as having
    /// been healthy: its failure streak and wait start over.
    pub healthy_after: Duration,
    /// Consecutive fast failures after which the thread stops for good, and
    /// says so (`Degraded::stopped`). Zero means never restart.
    pub max_fast_failures: u32,
}

impl Policy {
    /// Restart with 1 s doubling to 60 s; give up after 8 failures in a row
    /// that each came within a minute of the last start.
    pub const RESTART: Policy = Policy {
        base: Duration::from_secs(1),
        cap: Duration::from_secs(60),
        healthy_after: Duration::from_secs(60),
        max_fast_failures: 8,
    };
    /// A panic ends the thread, visibly. For bodies that own what they read
    /// from (a receiver, a listener) and cannot be run twice.
    pub const NEVER: Policy = Policy {
        base: Duration::ZERO,
        cap: Duration::ZERO,
        healthy_after: Duration::ZERO,
        max_fast_failures: 0,
    };
}

/// Runs `body` to completion under `policy`. A normal return ends the thread
/// and clears any record; a panic is recorded, and the body is run again after
/// the policy's wait or, when the policy says so, never.
pub fn run_supervised(subsystem: &str, policy: Policy, mut body: impl FnMut()) {
    let mut streak = 0u32;
    let mut wait = policy.base;
    loop {
        let started = std::time::Instant::now();
        match catch_unwind(AssertUnwindSafe(&mut body)) {
            Ok(()) => {
                recover(subsystem);
                return;
            }
            Err(payload) => {
                if started.elapsed() >= policy.healthy_after {
                    streak = 0;
                    wait = policy.base;
                }
                streak += 1;
                if streak > policy.max_fast_failures {
                    record_with(subsystem, payload.as_ref(), true);
                    return;
                }
                record_with(subsystem, payload.as_ref(), false);
                thread::sleep(wait);
                wait = (wait * 2).min(policy.cap);
            }
        }
    }
}

/// Records a subsystem as degraded for a failure that is not a panic (an
/// upgrade that could not be backed up, a profile that cannot be read), so
/// Diagnostics lists it beside the panicked threads. Cleared by
/// [`note_recovered`].
pub fn note_degraded(subsystem: &str, reason: &str) {
    record_with(subsystem, &reason.to_owned(), false);
}

/// Clears a record made by [`note_degraded`] once the subsystem works again.
pub fn note_recovered(subsystem: &str) {
    recover(subsystem);
}

/// Records that a supervised-in-spirit thread caught its own panic and ended,
/// for a body that has its own cleanup and so handles the unwind itself.
pub fn note_stopped(subsystem: &str, payload: &(dyn std::any::Any + Send)) {
    record_with(subsystem, payload, true);
}

/// Starts a long-lived thread that restarts after a panic with backoff
/// ([`Policy::RESTART`]). `body` must be safe to run again: it rebuilds what
/// it needs from what it captured by clone.
pub fn spawn_service(
    name: &'static str,
    body: impl FnMut() + Send + 'static,
) -> io::Result<JoinHandle<()>> {
    spawn_service_with(name, Policy::RESTART, body)
}

pub fn spawn_service_with(
    name: &'static str,
    policy: Policy,
    body: impl FnMut() + Send + 'static,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name(name.into())
        .spawn(move || run_supervised(name, policy, body))
}

/// Starts a thread that runs `body` once. A panic is recorded under
/// `subsystem` as stopped, never silently lost; `name` is the thread's own
/// (it may carry a session id, which `subsystem` must not).
pub fn spawn_once(
    name: impl Into<String>,
    subsystem: &'static str,
    body: impl FnOnce() + Send + 'static,
) -> io::Result<JoinHandle<()>> {
    spawn_once_recovering(name, subsystem, true, body)
}

/// [`spawn_once`] for a subsystem that has several instances alive at once
/// (the output writers, one per stream). A record is by subsystem
/// name, not by thread, so a clean exit of one instance would clear the
/// `stopped` record another instance's panic had just made: the failure was
/// erased from Diagnostics by an unrelated thread finishing. This variant
/// records a panic and never clears on a clean exit.
pub fn spawn_one_of_many(
    name: impl Into<String>,
    subsystem: &'static str,
    body: impl FnOnce() + Send + 'static,
) -> io::Result<JoinHandle<()>> {
    spawn_once_recovering(name, subsystem, false, body)
}

fn spawn_once_recovering(
    name: impl Into<String>,
    subsystem: &'static str,
    clean_exit_recovers: bool,
    body: impl FnOnce() + Send + 'static,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new().name(name.into()).spawn(move || {
        match catch_unwind(AssertUnwindSafe(body)) {
            Ok(()) if clean_exit_recovers => recover(subsystem),
            Ok(()) => {}
            Err(payload) => record_with(subsystem, payload.as_ref(), true),
        }
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
        // The listing is cleared by the next successful tick. Two panics and
        // then a recovery is a window of about ten milliseconds; on a loaded
        // runner the test thread misses it and the assertion fails even though
        // the panic was recorded. Hold the panic until this test has seen the
        // listing, then let a tick complete.
        let panicking = Arc::new(AtomicBool::new(true));
        let flag = panicking.clone();
        let ticks = Arc::new(AtomicUsize::new(0));
        let seen = ticks.clone();
        spawn_loop(NAME, Duration::from_millis(5), move || {
            if flag.load(Ordering::SeqCst) {
                panic!("injected panic");
            }
            seen.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        let listed = || degraded().into_iter().find(|d| d.subsystem == NAME);
        assert!(wait(Duration::from_secs(5), || listed().is_some()));
        let first = listed().unwrap();
        assert!(first.reason.contains("injected panic"), "{first:?}");
        panicking.store(false, Ordering::SeqCst);
        assert!(
            wait(Duration::from_secs(5), || {
                ticks.load(Ordering::SeqCst) >= 1 && listed().is_none()
            }),
            "the loop did not survive the panic and recover"
        );
    }

    fn fast(max: u32) -> Policy {
        Policy {
            base: Duration::from_millis(2),
            cap: Duration::from_millis(8),
            healthy_after: Duration::from_secs(60),
            max_fast_failures: max,
        }
    }

    #[test]
    fn a_service_that_panics_is_restarted_with_backoff_then_recovers() {
        const NAME: &str = "supervisor-test-service";
        let runs = Arc::new(AtomicUsize::new(0));
        let seen = runs.clone();
        let handle = spawn_service_with(NAME, fast(8), move || {
            if seen.fetch_add(1, Ordering::SeqCst) < 2 {
                panic!("injected service panic");
            }
        })
        .unwrap();
        handle.join().unwrap();
        assert_eq!(
            runs.load(Ordering::SeqCst),
            3,
            "restarted twice, then returned"
        );
        assert!(
            degraded().iter().all(|d| d.subsystem != NAME),
            "a finished service stays listed"
        );
    }

    #[test]
    fn a_service_that_keeps_panicking_stops_visibly() {
        const NAME: &str = "supervisor-test-flapping";
        let handle = spawn_service_with(NAME, fast(3), || panic!("always")).unwrap();
        handle.join().unwrap();
        let entry = degraded()
            .into_iter()
            .find(|d| d.subsystem == NAME)
            .unwrap();
        assert!(entry.stopped && entry.panics == 4, "{entry:?}");
        recover(NAME);
    }

    #[test]
    fn spawn_once_records_a_panic_as_stopped_and_never_reruns() {
        const NAME: &str = "supervisor-test-once";
        let runs = Arc::new(AtomicUsize::new(0));
        let seen = runs.clone();
        spawn_once("supervisor-once-7", NAME, move || {
            seen.fetch_add(1, Ordering::SeqCst);
            panic!("injected once panic");
        })
        .unwrap()
        .join()
        .unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        let entry = degraded()
            .into_iter()
            .find(|d| d.subsystem == NAME)
            .unwrap();
        assert!(entry.stopped && entry.reason.contains("injected once panic"));
        // A later clean run of the same subsystem clears it.
        spawn_once("supervisor-once-8", NAME, || {})
            .unwrap()
            .join()
            .unwrap();
        assert!(degraded().iter().all(|d| d.subsystem != NAME));
    }

    #[test]
    fn a_clean_exit_of_one_of_many_instances_keeps_anothers_stopped_record() {
        const NAME: &str = "supervisor-test-one-of-many";
        spawn_one_of_many("supervisor-many-1", NAME, || panic!("instance one"))
            .unwrap()
            .join()
            .unwrap();
        spawn_one_of_many("supervisor-many-2", NAME, || {})
            .unwrap()
            .join()
            .unwrap();
        let entry = degraded()
            .into_iter()
            .find(|d| d.subsystem == NAME)
            .expect("a sibling's clean exit cleared the stopped record");
        assert!(entry.stopped && entry.reason.contains("instance one"));
        recover(NAME);
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
