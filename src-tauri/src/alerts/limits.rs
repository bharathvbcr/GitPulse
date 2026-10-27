//! How fast one session, or the class of reports from outside any terminal,
//! may report through the hook socket.
//!
//! A token bucket per session key. The numbers are far above what an agent
//! does and far below what a script can: a busy Claude Code turn runs a handful
//! of tool calls in parallel, each reported by up to two hooks (the plugin's and
//! the injected one, [`super::intake::DUPLICATE_WINDOW_NS`]), and a hook is a
//! process spawn, so one session cannot sustain more than a few tens of reports
//! a second however fast its tools are. [`SESSION_BURST`] covers the largest
//! parallel fan-out seen with room to spare and [`SESSION_PER_SEC`] is several
//! times a spawn-bound agent's sustained rate.
//!
//! The map is bounded by [`CAPACITY`], the order book's own cap, and the
//! least recently used key makes room, so a flood of keys cannot grow it. A
//! forgotten key starts with a full bucket again, which is the same leniency
//! a session that has been quiet for a while gets.
//!
//! Reports from outside any terminal are keyed `hook-<agent>-<place>`; a peer
//! chooses `place`, so a bucket per such key would let it mint buckets (and
//! `Tracked` entries) at will. They share one bucket, [`OUTSIDE_BURST`] deep:
//! an external Claude Code asks for attention a few times an hour.

use std::collections::HashMap;
use std::time::Instant;

/// Reports a session may send at once.
pub(super) const SESSION_BURST: f64 = 256.0;

/// Reports a session's bucket regains each second.
pub(super) const SESSION_PER_SEC: f64 = 64.0;

/// Reports from outside any terminal, between all of them, at once.
pub(super) const OUTSIDE_BURST: f64 = 32.0;

/// Reports from outside any terminal regained each second, between all of them.
pub(super) const OUTSIDE_PER_SEC: f64 = 4.0;

/// Most session buckets kept: the notifier's own tracking bound.
pub(super) const CAPACITY: usize = super::MAX_TRACKED;

#[derive(Clone, Copy)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

impl Bucket {
    fn full(burst: f64, now: Instant) -> Self {
        Self {
            tokens: burst,
            at: now,
        }
    }

    /// [`Bucket::take`] with the clock held still: spends a token if there is
    /// one and refills nothing.
    #[cfg(all(test, unix))]
    fn take_without_refill(&mut self) -> bool {
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Takes one token if there is one, after refilling for the time since the
    /// last look. A clock that went backwards refills nothing.
    fn take(&mut self, burst: f64, per_sec: f64, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        self.tokens = (self.tokens + elapsed * per_sec).min(burst);
        self.at = self.at.max(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// The buckets: one per session, one for everything outside a terminal.
#[derive(Default)]
pub(super) struct Limits {
    sessions: HashMap<String, Bucket>,
    outside: Option<Bucket>,
    /// Test seam: while set, the outside bucket regains nothing with time, so a
    /// test that emptied it finds it empty for as long as it holds it, however
    /// slow the host. Never set outside `cfg(test)`.
    #[cfg(all(test, unix))]
    outside_held: bool,
}

impl Limits {
    /// Whether `key`'s report may go on. `outside` is a report that names no
    /// terminal session, which draws on the shared bucket instead.
    pub(super) fn admit(&mut self, key: &str, outside: bool, now: Instant) -> bool {
        if outside {
            #[cfg(all(test, unix))]
            let held = self.outside_held;
            let bucket = self
                .outside
                .get_or_insert_with(|| Bucket::full(OUTSIDE_BURST, now));
            #[cfg(all(test, unix))]
            if held {
                return bucket.take_without_refill();
            }
            return bucket.take(OUTSIDE_BURST, OUTSIDE_PER_SEC, now);
        }
        if !self.sessions.contains_key(key) && self.sessions.len() >= CAPACITY {
            let oldest = self
                .sessions
                .iter()
                .min_by_key(|(_, bucket)| bucket.at)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.sessions.remove(&oldest);
            }
        }
        self.sessions
            .entry(key.to_owned())
            .or_insert_with(|| Bucket::full(SESSION_BURST, now))
            .take(SESSION_BURST, SESSION_PER_SEC, now)
    }

    /// Test seam: holds the outside bucket's refill off (`true`) or lets it run
    /// again from now (`false`), without giving back the tokens it spent.
    #[cfg(all(test, unix))]
    pub(super) fn hold_outside_refill(&mut self, held: bool) {
        self.outside_held = held;
        if !held {
            if let Some(bucket) = self.outside.as_mut() {
                bucket.at = Instant::now();
            }
        }
    }

    /// The session has ended: its bucket is of no further use.
    pub(super) fn forget(&mut self, key: &str) {
        self.sessions.remove(key);
    }

    #[cfg(all(test, unix))]
    pub(super) fn len(&self) -> usize {
        self.sessions.len()
    }
}
