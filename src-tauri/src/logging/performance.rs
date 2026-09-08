//! Bounded timing observations for the existing native command seam.
//! Labels come from compiler type names, never captured command arguments.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};
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

pub(crate) struct CommandTiming {
    operation: &'static str,
    queued: Instant,
    started: Instant,
    outcome: &'static str,
}

impl CommandTiming {
    pub(crate) fn start(operation: &'static str, queued: Instant) -> Self {
        Self {
            operation,
            queued,
            started: Instant::now(),
            outcome: "ok",
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
        let now = Instant::now();
        // Fast commands pay two clock reads, with no allocation or lock.
        if now.duration_since(self.queued) < SLOW {
            return;
        }
        let queue = self.started.duration_since(self.queued);
        let work = now.duration_since(self.started);
        static COMMANDS: OnceLock<Mutex<SlowCommands>> = OnceLock::new();
        let report = COMMANDS
            .get_or_init(|| Mutex::new(SlowCommands::default()))
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
    log::set_logger(&LOGGER).expect("performance test owns the facade");
    log::set_max_level(log::LevelFilter::Warn);
}

#[cfg(test)]
mod tests {
    use super::{SlowCommands, MAX_OPERATIONS, OVERFLOW, REPORT_INTERVAL, SLOW};
    use std::time::{Duration, Instant};

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
