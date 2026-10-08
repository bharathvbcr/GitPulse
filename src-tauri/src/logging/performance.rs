//! Bounded timing observations for the existing native command seam.
//! Labels come from compiler type names, never captured command arguments.

use std::collections::HashMap;
use std::sync::{Condvar, Mutex, Once, OnceLock, PoisonError};
use std::time::{Duration, Instant};

const SLOW: Duration = Duration::from_secs(1);
const REPORT_INTERVAL: Duration = Duration::from_secs(30);
const MAX_OPERATIONS: usize = 256;
const OVERFLOW: &str = "additional operations (grouped)";

#[derive(Default)]
struct Window {
    last_report: Option<Instant>,
    calls: u64,
    max_queue: Duration,
    max_work: Duration,
}

#[derive(Default)]
struct SlowCommands {
    operations: HashMap<&'static str, Window>,
    overflow: Window,
}

struct Report {
    operation: &'static str,
    calls: u64,
    max_queue: Duration,
    max_work: Duration,
}

impl SlowCommands {
    fn observe(
        &mut self,
        operation: &'static str,
        now: Instant,
        queue: Duration,
        work: Duration,
    ) -> Option<Report> {
        if queue.saturating_add(work) < SLOW {
            return None;
        }
        let (operation, window) =
            if self.operations.contains_key(operation) || self.operations.len() < MAX_OPERATIONS {
                (operation, self.operations.entry(operation).or_default())
            } else {
                (OVERFLOW, &mut self.overflow)
            };
        window.calls = window.calls.saturating_add(1);
        window.max_queue = window.max_queue.max(queue);
        window.max_work = window.max_work.max(work);
        if window
            .last_report
            .is_some_and(|last| now.duration_since(last) < REPORT_INTERVAL)
        {
            return None;
        }
        let report = Report {
            operation,
            calls: window.calls,
            max_queue: window.max_queue,
            max_work: window.max_work,
        };
        *window = Window {
            last_report: Some(now),
            ..Window::default()
        };
        Some(report)
    }
}

impl SlowCommands {
    /// Every operation with slow calls the 30-second pacing has not reported
    /// yet. Without this, a session's last aggregates vanished at exit.
    fn drain(&mut self) -> Vec<Report> {
        let mut reports = Vec::new();
        let windows = self
            .operations
            .iter_mut()
            .chain(std::iter::once((&OVERFLOW, &mut self.overflow)));
        for (operation, window) in windows {
            if window.calls > 0 {
                reports.push(Report {
                    operation,
                    calls: window.calls,
                    max_queue: window.max_queue,
                    max_work: window.max_work,
                });
                *window = Window {
                    last_report: window.last_report,
                    ..Window::default()
                };
            }
        }
        reports
    }
}

fn slow_commands() -> &'static Mutex<SlowCommands> {
    static COMMANDS: OnceLock<Mutex<SlowCommands>> = OnceLock::new();
    COMMANDS.get_or_init(|| Mutex::new(SlowCommands::default()))
}

/// A completion record cannot describe a call that never completes, so calls
/// are also registered while they wait and run. Past this age a call is
/// reported as still running, then again each time its age doubles.
const RUNNING_BUDGET: Duration = Duration::from_secs(5);
const WATCH_TICK: Duration = Duration::from_secs(1);
/// Above Tauri's blocking-pool ceiling; a call beyond it is counted, not lost.
const MAX_IN_FLIGHT: usize = 1024;
/// Lines per watchdog pass; the remainder is one summary line.
const MAX_RUNNING_LINES: usize = 8;

struct Running {
    operation: &'static str,
    queued: Instant,
    started: Option<Instant>,
    /// Age at which this call is next reported.
    next_report: Duration,
}

impl Running {
    fn reported(&self) -> bool {
        self.next_report > RUNNING_BUDGET
    }
}

#[derive(Debug, PartialEq, Eq)]
struct RunningReport {
    operation: &'static str,
    /// Calls of this operation past the budget, waiting or working.
    running: usize,
    /// Of those, calls still waiting for a blocking worker.
    queued: usize,
    oldest: Duration,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Sweep {
    running: Vec<RunningReport>,
    /// Operations past the line cap, reported only as a count.
    more_operations: usize,
    /// Previously reported calls that have since ended, per operation.
    finished: Vec<(&'static str, u64)>,
    /// Calls not registered because the table was full, since the last sweep.
    untracked: u64,
}

#[derive(Default)]
struct InFlight {
    next_id: u64,
    calls: HashMap<u64, Running>,
    finished_after_report: HashMap<&'static str, u64>,
    untracked: u64,
}

impl InFlight {
    fn insert(&mut self, operation: &'static str, queued: Instant) -> Option<u64> {
        if self.calls.len() >= MAX_IN_FLIGHT {
            self.untracked = self.untracked.saturating_add(1);
            return None;
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.calls.insert(
            id,
            Running {
                operation,
                queued,
                started: None,
                next_report: RUNNING_BUDGET,
            },
        );
        Some(id)
    }

    fn start(&mut self, id: u64, now: Instant) {
        if let Some(call) = self.calls.get_mut(&id) {
            call.started = Some(now);
        }
    }

    fn remove(&mut self, id: u64) {
        if let Some(call) = self.calls.remove(&id) {
            if call.reported() && self.finished_after_report.len() < MAX_IN_FLIGHT {
                *self
                    .finished_after_report
                    .entry(call.operation)
                    .or_default() += 1;
            }
        }
    }

    /// Calls that crossed their next report age, grouped by operation, and
    /// reported calls that have ended since the previous sweep. Every call of
    /// an operation that is due is counted, so `running` is the whole group,
    /// not only the call that tripped the line.
    fn sweep(&mut self, now: Instant) -> Sweep {
        let mut due: Vec<&'static str> = Vec::new();
        for call in self.calls.values_mut() {
            let age = now.saturating_duration_since(call.queued);
            if age >= call.next_report {
                while call.next_report <= age {
                    call.next_report = call.next_report.saturating_mul(2);
                }
                if !due.contains(&call.operation) {
                    due.push(call.operation);
                }
            }
        }
        let mut running: Vec<RunningReport> = due
            .into_iter()
            .map(|operation| {
                let mut report = RunningReport {
                    operation,
                    running: 0,
                    queued: 0,
                    oldest: Duration::ZERO,
                };
                for call in self.calls.values() {
                    let age = now.saturating_duration_since(call.queued);
                    if call.operation == operation && age >= RUNNING_BUDGET {
                        report.running += 1;
                        report.queued += usize::from(call.started.is_none());
                        report.oldest = report.oldest.max(age);
                    }
                }
                report
            })
            .collect();
        running.sort_by(|a, b| b.oldest.cmp(&a.oldest).then(a.operation.cmp(b.operation)));
        let more_operations = running.len().saturating_sub(MAX_RUNNING_LINES);
        running.truncate(MAX_RUNNING_LINES);
        let mut finished: Vec<_> = self.finished_after_report.drain().collect();
        finished.sort_unstable();
        Sweep {
            running,
            more_operations,
            finished,
            untracked: std::mem::take(&mut self.untracked),
        }
    }

    /// Everything still registered, regardless of age, for the exit record.
    fn remaining(&self, now: Instant) -> Vec<RunningReport> {
        let mut by_operation: HashMap<&'static str, RunningReport> = HashMap::new();
        for call in self.calls.values() {
            let report = by_operation.entry(call.operation).or_insert(RunningReport {
                operation: call.operation,
                running: 0,
                queued: 0,
                oldest: Duration::ZERO,
            });
            report.running += 1;
            report.queued += usize::from(call.started.is_none());
            report.oldest = report
                .oldest
                .max(now.saturating_duration_since(call.queued));
        }
        let mut reports: Vec<_> = by_operation.into_values().collect();
        reports.sort_by(|a, b| b.oldest.cmp(&a.oldest).then(a.operation.cmp(b.operation)));
        reports
    }
}

struct Watch {
    state: Mutex<InFlight>,
    wake: Condvar,
}

fn watch() -> &'static Watch {
    static WATCH: OnceLock<Watch> = OnceLock::new();
    static WATCHDOG: Once = Once::new();
    let watch = WATCH.get_or_init(|| Watch {
        state: Mutex::new(InFlight::default()),
        wake: Condvar::new(),
    });
    WATCHDOG.call_once(|| {
        // One thread per process, parked while nothing is in flight. A spawn
        // failure leaves completion records working and says so once.
        if let Err(error) = std::thread::Builder::new()
            .name("gitpulse-perf-watch".into())
            .spawn(move || run_watchdog(watch))
        {
            log::warn!(target: "performance", "still-running watchdog unavailable: {error}");
        }
    });
    watch
}

fn run_watchdog(watch: &Watch) {
    let mut state = watch.state.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        while state.calls.is_empty() && state.finished_after_report.is_empty() {
            state = watch
                .wake
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        state = watch
            .wake
            .wait_timeout(state, WATCH_TICK)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
        let sweep = state.sweep(Instant::now());
        // Log outside the lock: the durable sink does file I/O.
        drop(state);
        log_sweep(&sweep);
        state = watch.state.lock().unwrap_or_else(PoisonError::into_inner);
    }
}

fn log_running(prefix: &str, report: &RunningReport) {
    log::warn!(
        target: "performance",
        "{prefix} operation={} calls={} waiting_for_worker={} oldest_ms={}",
        report.operation, report.running, report.queued, report.oldest.as_millis(),
    );
}

fn log_sweep(sweep: &Sweep) {
    for report in &sweep.running {
        log_running("operation still running", report);
    }
    if sweep.more_operations > 0 {
        log::warn!(
            target: "performance",
            "operation still running: {} more operations past the {} ms budget (not listed)",
            sweep.more_operations, RUNNING_BUDGET.as_millis(),
        );
    }
    for (operation, calls) in &sweep.finished {
        log::warn!(
            target: "performance",
            "operation no longer running operation={operation} reported_calls_ended={calls}",
        );
    }
    if sweep.untracked > 0 {
        log::warn!(
            target: "performance",
            "in-flight table full: {} calls were not tracked for still-running reports",
            sweep.untracked,
        );
    }
}

/// Writes what the paced records have not: slow-call aggregates still inside
/// their 30-second window, and every call that has not finished. Called once
/// at application exit; the durable sink writes synchronously.
pub(crate) fn flush() {
    let pending = slow_commands()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .drain();
    for report in pending {
        log::warn!(
            target: "performance",
            "slow command summary at exit operation={} slow_calls_since_report={} max_queue_ms={} max_work_ms={}",
            report.operation, report.calls,
            report.max_queue.as_millis(), report.max_work.as_millis(),
        );
    }
    let remaining = watch()
        .state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remaining(Instant::now());
    for report in &remaining {
        log_running("operation unfinished at exit", report);
    }
}

pub(crate) struct CommandTiming {
    operation: &'static str,
    id: Option<u64>,
    queued: Instant,
    started: Instant,
    outcome: &'static str,
}

impl CommandTiming {
    /// Registers a call before it waits for a blocking worker, so a saturated
    /// pool is visible as calls waiting, not only as late completions.
    pub(crate) fn queue(operation: &'static str) -> Self {
        let queued = Instant::now();
        let watch = watch();
        let id = watch
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(operation, queued);
        watch.wake.notify_one();
        Self {
            operation,
            id,
            queued,
            started: queued,
            outcome: "ok",
        }
    }

    pub(crate) fn start(&mut self) {
        self.started = Instant::now();
        if let Some(id) = self.id {
            watch()
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .start(id, self.started);
        }
    }

    pub(crate) fn finish<T>(&mut self, result: &Result<T, String>) {
        if result.is_err() {
            self.outcome = "error";
        }
    }
}

impl Drop for CommandTiming {
    fn drop(&mut self) {
        if let Some(id) = self.id {
            watch()
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(id);
        }
        let now = Instant::now();
        // Fast commands pay three uncontended lock acquisitions for the
        // in-flight table, and no allocation or lock for the slow-call record.
        if now.duration_since(self.queued) < SLOW {
            return;
        }
        let queue = self.started.duration_since(self.queued);
        let work = now.duration_since(self.started);
        let report = slow_commands()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .observe(self.operation, now, queue, work);
        if let Some(report) = report {
            let outcome = if std::thread::panicking() {
                "panic"
            } else {
                self.outcome
            };
            log::warn!(
                target: "performance",
                "slow command operation={} outcome={} queue_ms={} work_ms={} slow_calls_since_report={} max_queue_ms={} max_work_ms={}",
                report.operation, outcome, queue.as_millis(), work.as_millis(),
                report.calls, report.max_queue.as_millis(), report.max_work.as_millis(),
            );
        }
    }
}

/// Unit tests deliberately leave the process log facade unregistered. Enable
/// only this target for the command integration probe, so unrelated library
/// fixtures do not fill the diagnostics ring with their expected failures.
///
/// Idempotent: several command probes in one test binary share the facade.
#[cfg(test)]
pub(crate) fn enable_test_facade() {
    struct PerformanceLogger;
    impl log::Log for PerformanceLogger {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.target() == "performance"
        }
        fn log(&self, record: &log::Record) {
            if self.enabled(record.metadata()) {
                if let Some(logger) = super::LOGGER.get() {
                    log::Log::log(logger, record);
                }
            }
        }
        fn flush(&self) {}
    }
    static LOGGER: PerformanceLogger = PerformanceLogger;
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        log::set_logger(&LOGGER).expect("performance test owns the facade");
        log::set_max_level(log::LevelFilter::Warn);
    });
}

#[cfg(test)]
mod tests {
    use super::{
        InFlight, RunningReport, SlowCommands, MAX_IN_FLIGHT, MAX_OPERATIONS, MAX_RUNNING_LINES,
        OVERFLOW, REPORT_INTERVAL, RUNNING_BUDGET, SLOW,
    };
    use std::time::{Duration, Instant};

    #[test]
    fn drain_returns_only_unreported_aggregates_including_overflow() {
        let mut tracker = SlowCommands::default();
        let now = Instant::now();
        // Reported immediately: nothing pending for it.
        assert!(tracker
            .observe("reported", now, SLOW, Duration::ZERO)
            .is_some());
        // First call reports, the next two are held by the 30-second pacing.
        assert!(tracker
            .observe("paced", now, Duration::ZERO, SLOW)
            .is_some());
        assert!(tracker
            .observe("paced", now + SLOW, SLOW, SLOW * 2)
            .is_none());
        assert!(tracker
            .observe("paced", now + SLOW * 2, Duration::ZERO, SLOW * 5)
            .is_none());
        // "reported" and "paced" already hold two of the bounded slots.
        for i in 0..MAX_OPERATIONS - 2 {
            let name = Box::leak(format!("fill-{i}").into_boxed_str());
            tracker.observe(name, now, SLOW, Duration::ZERO);
        }
        assert!(tracker
            .observe("spill", now, SLOW, Duration::ZERO)
            .is_some());
        assert!(tracker
            .observe("spill-2", now, SLOW * 3, Duration::ZERO)
            .is_none());

        let mut drained = tracker.drain();
        drained.sort_by_key(|report| report.operation);
        let summary: Vec<_> = drained
            .iter()
            .map(|r| (r.operation, r.calls, r.max_queue, r.max_work))
            .collect();
        assert_eq!(
            summary,
            vec![
                (OVERFLOW, 1, SLOW * 3, Duration::ZERO),
                ("paced", 2, SLOW, SLOW * 5),
            ]
        );
        assert!(
            tracker.drain().is_empty(),
            "a drained aggregate is not repeated"
        );
        // Draining keeps pacing: the next slow call is still inside the window.
        assert!(tracker
            .observe("paced", now + SLOW * 3, SLOW, Duration::ZERO)
            .is_none());
    }

    #[test]
    fn running_calls_are_reported_at_the_budget_then_each_doubling() {
        let mut table = InFlight::default();
        let t0 = Instant::now();
        let stuck = table.insert("stuck", t0).unwrap();
        table.start(stuck, t0);
        let waiting = table.insert("stuck", t0 + Duration::from_secs(1)).unwrap();
        let fast = table.insert("fast", t0).unwrap();
        table.remove(fast);

        assert!(table
            .sweep(t0 + RUNNING_BUDGET - Duration::from_millis(1))
            .running
            .is_empty());
        let first = table.sweep(t0 + RUNNING_BUDGET);
        assert_eq!(
            first.running,
            vec![RunningReport {
                operation: "stuck",
                running: 1,
                queued: 0,
                oldest: RUNNING_BUDGET,
            }]
        );
        // The second call crosses its own budget a second later; the group
        // line counts both, and the one still waiting for a worker as such.
        let second = table.sweep(t0 + RUNNING_BUDGET + Duration::from_secs(1));
        assert_eq!(second.running.len(), 1);
        assert_eq!(
            (second.running[0].running, second.running[0].queued),
            (2, 1)
        );
        // Nothing new until a call's age doubles; each call keeps its own
        // schedule, so the later one crosses 10 s a second after the first.
        assert!(table.sweep(t0 + Duration::from_secs(9)).running.is_empty());
        assert_eq!(table.sweep(t0 + Duration::from_secs(10)).running.len(), 1);
        assert_eq!(table.sweep(t0 + Duration::from_secs(11)).running.len(), 1);
        assert!(table.sweep(t0 + Duration::from_secs(19)).running.is_empty());

        table.remove(stuck);
        table.remove(waiting);
        let ended = table.sweep(t0 + Duration::from_secs(20));
        assert!(ended.running.is_empty());
        assert_eq!(ended.finished, vec![("stuck", 2)]);
        assert!(table
            .sweep(t0 + Duration::from_secs(21))
            .finished
            .is_empty());
        assert!(table.calls.is_empty());
    }

    #[test]
    fn a_call_that_ends_before_its_budget_is_never_reported_as_finished() {
        let mut table = InFlight::default();
        let t0 = Instant::now();
        let id = table.insert("quick", t0).unwrap();
        table.remove(id);
        assert_eq!(table.sweep(t0 + RUNNING_BUDGET * 4), Default::default());
    }

    #[test]
    fn in_flight_table_and_report_lines_are_bounded_and_overflow_is_counted() {
        let mut table = InFlight::default();
        let t0 = Instant::now();
        for i in 0..MAX_IN_FLIGHT {
            let name = Box::leak(format!("op-{}", i % (MAX_RUNNING_LINES + 3)).into_boxed_str());
            assert!(table.insert(name, t0).is_some());
        }
        assert!(table.insert("dropped", t0).is_none());
        assert!(table.insert("dropped", t0).is_none());
        let sweep = table.sweep(t0 + RUNNING_BUDGET);
        assert_eq!(sweep.running.len(), MAX_RUNNING_LINES);
        assert_eq!(sweep.more_operations, 3);
        assert_eq!(sweep.untracked, 2);
        let total: usize = sweep.running.iter().map(|r| r.running).sum();
        assert!(
            total < MAX_IN_FLIGHT,
            "capped lines are not presented as every call"
        );
        assert_eq!(table.sweep(t0 + RUNNING_BUDGET).untracked, 0);
    }

    #[test]
    fn remaining_lists_every_unfinished_call_regardless_of_age() {
        let mut table = InFlight::default();
        let t0 = Instant::now();
        let a = table.insert("young", t0).unwrap();
        table.start(a, t0);
        table.insert("old", t0).unwrap();
        let remaining = table.remaining(t0 + Duration::from_millis(10));
        let names: Vec<_> = remaining.iter().map(|r| (r.operation, r.queued)).collect();
        assert_eq!(names, vec![("old", 1), ("young", 0)]);
    }

    #[test]
    fn threshold_includes_queue_wait_and_fast_calls_allocate_no_state() {
        let mut tracker = SlowCommands::default();
        let now = Instant::now();
        assert!(tracker
            .observe("fast", now, Duration::ZERO, SLOW - Duration::from_nanos(1))
            .is_none());
        assert!(tracker.operations.is_empty());
        let report = tracker
            .observe("queued", now, SLOW, Duration::ZERO)
            .unwrap();
        assert_eq!(report.max_queue, SLOW);
        assert_eq!(report.max_work, Duration::ZERO);
        assert_eq!(report.calls, 1);
    }

    #[test]
    fn repeated_slow_calls_are_counted_with_maxima_and_paced_per_operation() {
        let mut tracker = SlowCommands::default();
        let now = Instant::now();
        assert!(tracker.observe("scan", now, Duration::ZERO, SLOW).is_some());
        for seconds in 1..30 {
            assert!(tracker
                .observe("scan", now + Duration::from_secs(seconds), SLOW, SLOW * 3)
                .is_none());
        }
        assert!(tracker
            .observe("different", now, SLOW, Duration::ZERO)
            .is_some());
        let report = tracker
            .observe("scan", now + REPORT_INTERVAL, Duration::ZERO, SLOW)
            .unwrap();
        assert_eq!(report.calls, 30);
        assert_eq!(report.max_queue, SLOW);
        assert_eq!(report.max_work, SLOW * 3);
    }

    #[test]
    fn operation_cardinality_is_bounded_and_overflow_is_labelled() {
        let mut tracker = SlowCommands::default();
        let now = Instant::now();
        for i in 0..MAX_OPERATIONS {
            let name = Box::leak(format!("operation-{i}").into_boxed_str());
            assert!(tracker.observe(name, now, SLOW, Duration::ZERO).is_some());
        }
        let report = tracker
            .observe("overflow", now, SLOW, Duration::ZERO)
            .unwrap();
        assert_eq!(report.operation, OVERFLOW);
        assert_eq!(tracker.operations.len(), MAX_OPERATIONS);
        assert!(tracker
            .observe("another overflow", now, SLOW, Duration::ZERO)
            .is_none());
        assert_eq!(tracker.operations.len(), MAX_OPERATIONS);
    }
}
