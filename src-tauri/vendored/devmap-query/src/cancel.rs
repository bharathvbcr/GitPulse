//! Cooperative cancellation for queries whose caller has stopped waiting.
//!
//! The IPC layer bounds a query with `tokio::time::timeout` around a
//! `spawn_blocking` task. That bound frees the *connection*: the client gets a
//! `query_timeout` envelope and the connection slot is released. It does not
//! free the work. A blocking task cannot be aborted — dropping its `JoinHandle`
//! detaches it — so the traversal or the corpus scan that ran past the deadline
//! kept running on a blocking-pool thread with nobody left to read its answer.
//! Under repeated timeouts (the case where a repository is large enough to time
//! out at all) the abandoned tasks accumulate until the pool is full of them.
//!
//! A flag the caller can set and the loops consult is the only thing that
//! actually stops that work. It is checked every
//! [`Cancel::CHECK_INTERVAL`] iterations, which is often enough to abandon
//! promptly and rare enough not to show up in a query's cost.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Query loops abandoned in this process because their caller gave up.
///
/// Counted rather than logged because "did the abandoned work actually stop?"
/// is otherwise unobservable from outside: a task that keeps running looks
/// exactly like one that stopped, right up until the blocking pool is full.
static ABANDONED: AtomicU64 = AtomicU64::new(0);

/// How many query loops this process has abandoned mid-flight.
///
/// Monotonic for the life of the process. A caller watching it can tell that a
/// timed-out query really stopped rather than merely stopped being awaited.
pub fn cancelled_queries() -> u64 {
    ABANDONED.load(Ordering::Relaxed)
}

/// A query stopped because its caller stopped waiting for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryCancelled;

impl std::fmt::Display for QueryCancelled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "query was cancelled: its caller stopped waiting for the answer"
        )
    }
}

impl std::error::Error for QueryCancelled {}

/// A shared "stop what you are doing" flag.
///
/// Cloning shares the flag, so the side that hands work to a worker keeps a
/// handle it can trip. The default is a flag nobody will ever set, which is
/// what every non-IPC caller (the CLI, tests) wants: `Cancel::default()` costs
/// one allocation and one relaxed load per check.
#[derive(Clone, Default)]
pub struct Cancel(
    Arc<AtomicBool>,
    #[cfg(test)] Option<Arc<dyn Fn() + Send + Sync>>,
);

impl Cancel {
    #[cfg(test)]
    pub(crate) fn with_check_probe(mut self, probe: impl Fn() + Send + Sync + 'static) -> Self {
        self.1 = Some(Arc::new(probe));
        self
    }
    /// Iterations between consultations of the flag inside a loop.
    ///
    /// A relaxed atomic load is cheap but not free, and the loops this guards
    /// run tens of thousands of iterations. At 512 an abandoned traversal stops
    /// within a few microseconds of the flag being set.
    pub const CHECK_INTERVAL: usize = 512;

    /// A fresh, uncancelled flag.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask whatever is holding a clone of this to stop.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Stop here if the caller has given up.
    ///
    /// Records the abandonment before returning, so [`cancelled_queries`]
    /// counts work that actually stopped rather than requests that were
    /// answered with a timeout.
    pub fn check(&self) -> Result<(), QueryCancelled> {
        #[cfg(test)]
        if let Some(probe) = &self.1 {
            probe();
        }
        if self.is_cancelled() {
            ABANDONED.fetch_add(1, Ordering::Relaxed);
            return Err(QueryCancelled);
        }
        Ok(())
    }

    /// [`Self::check`], but only on every [`Self::CHECK_INTERVAL`]-th
    /// iteration. `iteration` is the loop's own counter.
    pub fn check_every(&self, iteration: usize) -> Result<(), QueryCancelled> {
        if iteration.is_multiple_of(Self::CHECK_INTERVAL) {
            return self.check();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_uncancelled_flag_never_stops_a_loop() {
        let cancel = Cancel::new();
        assert!(!cancel.is_cancelled());
        for iteration in 0..(Cancel::CHECK_INTERVAL * 3) {
            cancel
                .check_every(iteration)
                .expect("an uncancelled flag must never stop a loop");
        }
    }

    /// Cancelling one clone stops the other, and the abandonment is counted.
    #[test]
    fn cancelling_a_clone_stops_the_holder_and_is_counted() {
        let before = cancelled_queries();
        let cancel = Cancel::new();
        let worker = cancel.clone();
        cancel.cancel();
        assert!(worker.is_cancelled());
        assert_eq!(worker.check(), Err(QueryCancelled));
        assert!(
            cancelled_queries() > before,
            "an abandoned loop must be counted so a caller can see it stopped"
        );
    }

    /// The interval must not be so coarse that a loop misses its first chance
    /// to stop: iteration zero always checks.
    #[test]
    fn the_first_iteration_always_checks() {
        let cancel = Cancel::new();
        cancel.cancel();
        assert_eq!(cancel.check_every(0), Err(QueryCancelled));
        assert_eq!(cancel.check_every(1), Ok(()));
    }
}
