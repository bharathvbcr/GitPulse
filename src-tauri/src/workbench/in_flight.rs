//! Launches that are exclusive per attempt, never per host.
//!
//! One attempt must not be launched twice at once: the store's claim is
//! single-use, so the second launch either failed with "already claimed"
//! while the first was still forking, or — for a managed run — raced the
//! first's provider startup. But launches of *different* attempts must never
//! wait on each other; a host-wide lock made every task queue behind the one
//! whose provider was slowest to start.
//!
//! The terminal lane waits for the launch in flight (bounded), because the
//! second caller's right answer is the session the first one creates. The
//! managed lane refuses instead, because its launch can block for the
//! provider's whole startup and the caller can simply retry.

use super::WorkbenchError;
use std::collections::HashSet;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct Attempts {
    held: Mutex<HashSet<String>>,
    freed: Condvar,
    #[cfg(test)]
    waiting: std::sync::atomic::AtomicUsize,
}

/// One attempt's place, given back on drop — including by unwinding.
pub(super) struct Held<'a> {
    attempts: &'a Attempts,
    id: String,
}

impl Attempts {
    /// Takes `id`'s place now, or refuses with `busy`.
    pub(super) fn try_enter(&self, id: &str) -> Result<Held<'_>, WorkbenchError> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        if !held.insert(id.to_owned()) {
            return Err(WorkbenchError::new(
                "busy",
                "This attempt is already being launched. Retry shortly.",
            ));
        }
        Ok(Held {
            attempts: self,
            id: id.to_owned(),
        })
    }

    /// Takes `id`'s place once the launch holding it ends, waiting at most
    /// `limit`. A launch that outlasts the limit is reported as such, never
    /// waited on forever.
    pub(super) fn enter_within(
        &self,
        id: &str,
        limit: Duration,
    ) -> Result<Held<'_>, WorkbenchError> {
        let deadline = Instant::now() + limit;
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        #[cfg(test)]
        let _counted = Counted::new(&self.waiting);
        while held.contains(id) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(WorkbenchError::new(
                    "busy",
                    format!(
                        "This attempt's terminal is still being started after {}s. Open it again shortly.",
                        limit.as_secs()
                    ),
                ));
            }
            held = self
                .freed
                .wait_timeout(held, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        held.insert(id.to_owned());
        Ok(Held {
            attempts: self,
            id: id.to_owned(),
        })
    }

    #[cfg(test)]
    pub(super) fn waiting(&self) -> usize {
        self.waiting.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.held.lock().unwrap().is_empty()
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.attempts
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
        self.attempts.freed.notify_all();
    }
}

/// Counts a caller as waiting for as long as it is inside `enter_within`, so
/// a test can start the competing launch only once the waiter is parked.
#[cfg(test)]
struct Counted<'a>(&'a std::sync::atomic::AtomicUsize);
#[cfg(test)]
impl<'a> Counted<'a> {
    fn new(count: &'a std::sync::atomic::AtomicUsize) -> Self {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self(count)
    }
}
#[cfg(test)]
impl Drop for Counted<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Different attempts never wait on each other; the same attempt is
    /// refused or waited for; a place is given back by drop and by unwinding.
    #[test]
    fn places_are_per_attempt_and_always_given_back() {
        let attempts = Attempts::default();
        let held: Vec<_> = (0..64)
            .map(|i| {
                attempts
                    .try_enter(&format!("attempt-{i}"))
                    .expect("a different attempt was refused")
            })
            .collect();
        assert_eq!(attempts.try_enter("attempt-7").err().unwrap().code, "busy");
        let timed_out = attempts
            .enter_within("attempt-7", Duration::from_millis(30))
            .err()
            .expect("waited past its limit");
        assert_eq!(timed_out.code, "busy");
        assert_eq!(attempts.waiting(), 0, "a timed-out waiter is still counted");
        drop(held);
        assert!(attempts.is_empty());
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _launch = attempts.try_enter("panics").unwrap();
            panic!("launch failed mid-way");
        }));
        assert!(unwound.is_err());
        assert!(
            attempts.try_enter("panics").is_ok(),
            "an unwound launch kept its place"
        );
    }

    /// Racing: of 32 callers that refuse rather than wait, exactly one gets
    /// the attempt; 32 that wait all get it, one at a time, never two at once.
    #[test]
    fn racers_for_one_attempt_are_admitted_once_or_one_at_a_time() {
        let attempts = Attempts::default();
        let (start, asked) = (std::sync::Barrier::new(32), std::sync::Barrier::new(32));
        let admitted = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..32)
                .map(|_| {
                    scope.spawn(|| {
                        start.wait();
                        let entered = attempts.try_enter("contended");
                        asked.wait();
                        entered.is_ok()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .filter(|ok| *ok)
                .count()
        });
        assert_eq!(admitted, 1);

        let inside = std::sync::atomic::AtomicUsize::new(0);
        let most = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..32 {
                scope.spawn(|| {
                    let _held = attempts
                        .enter_within("serial", Duration::from_secs(30))
                        .unwrap();
                    let now = inside.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    most.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(2));
                    inside.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                });
            }
        });
        assert_eq!(
            most.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "two launches of one attempt ran at once"
        );
        assert!(attempts.is_empty());
    }
}
