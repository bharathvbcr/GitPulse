//! Observational counters shared by discovery, cached extraction and the CLI.
//! No callbacks, output, per-file allocation or unbounded event queue.

use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct FileProgress {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    started: Option<Instant>,
    total: Option<usize>,
    completed: usize,
    cache_hits: usize,
    failed: usize,
    invalid: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct FileSnapshot {
    pub total: Option<usize>,
    pub completed: usize,
    pub cache_hits: usize,
    pub failed: usize,
    pub valid: bool,
    pub seconds: f64,
    pub percent: Option<f64>,
    pub files_per_second: Option<f64>,
    pub eta_seconds: Option<f64>,
}

impl FileProgress {
    /// A counter belongs to one phase. Reuse is disclosed as invalid telemetry;
    /// it must never change the work being observed.
    pub fn start(&self, total: usize) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.started.is_some() {
            state.invalid = true;
        } else {
            state.total = Some(total);
            state.started = Some(Instant::now());
        }
    }

    /// Count completed attempts, including failures. A refused cache identity
    /// is a miss; only a payload actually returned to its caller is a hit.
    pub fn finish_file(&self, cached: bool, failed: bool) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.completed = state.completed.saturating_add(1);
        state.cache_hits = state.cache_hits.saturating_add(usize::from(cached));
        state.failed = state.failed.saturating_add(usize::from(failed));
        state.invalid |=
            state.total.is_none_or(|total| state.completed > total) || (cached && failed);
    }

    pub fn snapshot(&self) -> FileSnapshot {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.snapshot(
            state
                .started
                .map_or(Duration::ZERO, |start| start.elapsed()),
        )
    }
}

impl State {
    fn snapshot(&self, elapsed: Duration) -> FileSnapshot {
        let seconds = elapsed.as_secs_f64();
        let valid = !self.invalid;
        let known = valid && self.total.is_some_and(|total| total > 0);
        // A brief burst is not a useful estimate. Never estimate an unknown
        // phase or report an ETA for work that is already complete.
        let rate = (known && self.completed >= 10 && seconds >= 1.0)
            .then(|| self.completed as f64 / seconds);
        FileSnapshot {
            total: self.total,
            completed: self.completed,
            cache_hits: self.cache_hits,
            failed: self.failed,
            valid,
            seconds,
            percent: known.then(|| self.completed as f64 / self.total.unwrap_or(1) as f64 * 100.0),
            files_per_second: rate,
            eta_seconds: rate.and_then(|rate| {
                self.total
                    .filter(|&total| total > self.completed)
                    .map(|total| (total - self.completed) as f64 / rate)
            }),
        }
    }
}

#[derive(Default, Debug, serde::Serialize)]
pub struct FileDelta {
    pub added: usize,
    pub changed: usize,
    pub removed: usize,
    pub unchanged: usize,
}

impl FileDelta {
    pub fn is_unchanged(&self) -> bool {
        self.added == 0 && self.changed == 0 && self.removed == 0
    }
}

#[cfg(test)]
mod tests {
    use super::{FileProgress, State};
    use std::time::{Duration, Instant};

    #[test]
    fn concurrent_counts_are_exact_and_invalid_reuse_cannot_invent_estimates() {
        let progress = FileProgress::default();
        assert!(progress.snapshot().percent.is_none());
        progress.start(100_000);
        std::thread::scope(|scope| {
            for _ in 0..10 {
                scope.spawn(|| {
                    for _ in 0..10_000 {
                        progress.finish_file(true, false);
                    }
                });
            }
        });
        let snapshot = progress.snapshot();
        assert_eq!(snapshot.completed, 100_000);
        assert_eq!(snapshot.cache_hits, 100_000);
        assert_eq!(snapshot.percent, Some(100.0));
        assert!(snapshot.eta_seconds.is_none());
        progress.start(1);
        assert!(!progress.snapshot().valid);
        assert!(progress.snapshot().percent.is_none());
        let empty = FileProgress::default();
        empty.start(0);
        assert!(empty.snapshot().percent.is_none());
        empty.finish_file(false, true);
        assert!(!empty.snapshot().valid);
    }

    #[test]
    fn estimates_require_enough_measured_work_and_never_survive_completion() {
        let mut state = State {
            started: Some(Instant::now()),
            total: Some(100),
            completed: 10,
            ..State::default()
        };
        assert!(state
            .snapshot(Duration::from_millis(999))
            .eta_seconds
            .is_none());
        let snapshot = state.snapshot(Duration::from_secs(2));
        assert_eq!(snapshot.files_per_second, Some(5.0));
        assert_eq!(snapshot.eta_seconds, Some(18.0));
        state.completed = 100;
        assert!(state
            .snapshot(Duration::from_secs(20))
            .eta_seconds
            .is_none());
        state.total = None;
        assert!(state.snapshot(Duration::MAX).percent.is_none());
    }
}
