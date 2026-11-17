//! Clock seam for the process spawn gate (C12).
//!
//! Production runs against [`SystemClock`], which reads monotonic wall time via
//! `Instant::now` and waits on the gate's `Condvar`. Tests inject
//! [`FakeClock`] so queue deadlines, refill ticks, post-action credits, and
//! throttled gate warnings advance deterministically without `thread::sleep`.

use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Monotonic time and bounded condvar waits for [`super::gate::SpawnGate`].
pub(crate) trait Clock: Send + Sync {
    fn now(&self) -> Instant;

    /// Simulates a wait of `timeout` when using a stepping virtual clock (`true`),
    /// or asks [`wait_timeout`] to block on the real `Condvar` (`false`).
    fn step_wait(&self, _timeout: Duration) -> bool {
        false
    }
}

/// Waits on `cond` using `clock`, advancing virtual time instead of sleeping
/// when `clock` is a stepping test clock.
pub(crate) fn wait_timeout<'a, T>(
    clock: &dyn Clock,
    mutex: &'a Mutex<T>,
    cond: &Condvar,
    guard: MutexGuard<'a, T>,
    timeout: Duration,
) -> MutexGuard<'a, T> {
    if clock.step_wait(timeout) {
        drop(guard);
        mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    } else {
        cond.wait_timeout(guard, timeout)
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0
    }
}

/// Real monotonic clock backed by [`Instant::now`] and [`Condvar::wait_timeout`].
pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Deterministic test clock for [`super::gate::SpawnGate`].
///
/// With `stepping == false`, waits sleep on the real `Condvar` (so concurrent
/// threads wake immediately on release) while elapsed timestamps still come from
/// [`Self::advance`]. With `stepping == true`, every wait releases the mutex,
/// advances the virtual clock by the requested `timeout` (at least 1 ns), and
/// re-acquires the mutex with zero wall-clock sleep.
#[cfg(test)]
pub(crate) struct FakeClock {
    now: Mutex<Instant>,
    stepping: bool,
}

#[cfg(test)]
impl FakeClock {
    pub(crate) fn new(start: Instant) -> Self {
        Self {
            now: Mutex::new(start),
            stepping: false,
        }
    }

    pub(crate) fn stepping(start: Instant) -> Self {
        Self {
            now: Mutex::new(start),
            stepping: true,
        }
    }

    pub(crate) fn advance(&self, delta: Duration) {
        let mut now = self
            .now
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *now += delta;
    }
}

#[cfg(test)]
impl Clock for FakeClock {
    fn now(&self) -> Instant {
        *self
            .now
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn step_wait(&self, timeout: Duration) -> bool {
        if self.stepping {
            self.advance(timeout.max(Duration::from_nanos(1)));
            true
        } else {
            false
        }
    }
}
