//! One bounded retry for a read the spawn gate deferred.
//!
//! A deferral is the gate declining to start Git under load: nothing was
//! examined, and asking again once the budget has refilled may answer. The
//! MCP read tools and the policy gate's binding lookup both retry through
//! here, once, so neither grows a retry of its own.

/// How a deferred read is asked once more. Time, sleeping and randomness are
/// injected so a test is exact and instant.
pub(crate) struct DeferralRetry<'a> {
    pub sleep: &'a dyn Fn(std::time::Duration),
    /// A value in `0..bound`.
    pub jitter: &'a dyn Fn(u64) -> u64,
    pub now: &'a dyn Fn() -> std::time::Instant,
    /// The request's own deadline. A retry that could not finish before it is
    /// not started.
    pub deadline: std::time::Instant,
}

/// The soonest a deferred read is worth repeating: one refill interval of the
/// spawn rate budget (the time it takes to mint one token), rounded up. The
/// budget refills at a fixed rate, so this is derived from it rather than
/// guessed; it is a floor, not a promise that the next attempt is admitted.
pub fn retry_after_ms() -> u64 {
    let (_burst, per_sec) = super::production_spawn_rate();
    1000u64.div_ceil(u64::from(per_sec.max(1)))
}

/// A value in `0..bound` that differs between processes and between calls.
/// Not a CSPRNG and not meant to be: it only spreads retries apart.
pub(crate) fn process_jitter(bound: u64) -> u64 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static CALLS: AtomicU64 = AtomicU64::new(0);
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(CALLS.fetch_add(1, Ordering::Relaxed));
    hasher.finish() % bound.max(1)
}

/// Runs `read`; when its answer was a deferral, waits one refill interval plus
/// jitter of up to the same again, runs it once more and returns the second
/// answer, whatever it was. Otherwise, or when the deadline leaves no room,
/// the first answer.
pub(crate) fn retry_once_on_deferral<T>(
    mut read: impl FnMut() -> T,
    deferred: impl Fn(&T) -> bool,
    retry: &DeferralRetry<'_>,
) -> T {
    let first = read();
    if !deferred(&first) {
        return first;
    }
    let base = retry_after_ms();
    let delay = std::time::Duration::from_millis(base + (retry.jitter)(base));
    if (retry.now)() + delay >= retry.deadline {
        return first;
    }
    (retry.sleep)(delay);
    read()
}

/// Calls `body` with the production retry: real clock, real sleep, and a
/// deadline `window` from now.
pub(crate) fn with_production_retry_window<T>(
    window: std::time::Duration,
    body: impl FnOnce(&DeferralRetry<'_>) -> T,
) -> T {
    body(&DeferralRetry {
        sleep: &std::thread::sleep,
        jitter: &process_jitter,
        now: &std::time::Instant::now,
        deadline: std::time::Instant::now() + window,
    })
}
