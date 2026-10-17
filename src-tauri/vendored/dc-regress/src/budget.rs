//! One bound over a whole analysis: a deadline and a cancel hook.
//!
//! Every git read in this crate has a wall clock of its own, which bounds one
//! command and says nothing about the analysis made of hundreds of them. A
//! host that answers within a timeout, and walks away from the work when the
//! timeout fires, needs the work to stop: a detached analysis keeps spawning
//! git for a client that is no longer listening.
//!
//! A [`Budget`] is checked between units of work — one changed file, one
//! blamed file — so the latency of a cancel is one unit, which is itself
//! bounded by [`crate::history::HISTORY_DEADLINE`]. A budget that ran out is
//! never a short list: the analysis names it as
//! [`crate::Unavailable::BudgetExhausted`] and the report is incomplete.

use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long an analysis may run when its host names no bound of its own.
///
/// Far past any answer this crate gives in practice, and finite: an
/// unbounded analysis of a repository with a thousand changed files is a
/// process that never returns.
pub const DEFAULT_ANALYSIS_DEADLINE: Duration = Duration::from_secs(120);

/// Why a [`Budget`] stopped the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exhausted {
    Deadline(Duration),
    Cancelled,
}

impl Exhausted {
    pub fn describe(&self) -> String {
        match self {
            Exhausted::Deadline(limit) => format!("the analysis exceeded its {limit:?} deadline"),
            Exhausted::Cancelled => "the analysis was cancelled by its caller".to_string(),
        }
    }
}

type CancelHook = Arc<dyn Fn() -> bool + Send + Sync>;

/// A deadline and a cancel hook, shared by everything one analysis runs.
#[derive(Clone)]
pub struct Budget {
    limit: Duration,
    deadline: Instant,
    cancelled: Option<CancelHook>,
}

impl std::fmt::Debug for Budget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Budget")
            .field("limit", &self.limit)
            .field("cancellable", &self.cancelled.is_some())
            .finish()
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self::within(DEFAULT_ANALYSIS_DEADLINE)
    }
}

impl Budget {
    /// A budget that ends `limit` from now.
    pub fn within(limit: Duration) -> Self {
        Self {
            limit,
            deadline: Instant::now()
                .checked_add(limit)
                .unwrap_or_else(|| Instant::now() + DEFAULT_ANALYSIS_DEADLINE),
            cancelled: None,
        }
    }

    /// Also ends the moment `cancelled` answers true. The hook is polled
    /// between units of work, so it must be cheap.
    pub fn with_cancel(mut self, cancelled: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        self.cancelled = Some(Arc::new(cancelled));
        self
    }

    /// `Err` once the deadline has passed or the hook says cancel. Cancel is
    /// reported in preference to the deadline: it is the caller's own word.
    pub fn check(&self) -> Result<(), Exhausted> {
        if self.cancelled.as_ref().is_some_and(|hook| hook()) {
            return Err(Exhausted::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(Exhausted::Deadline(self.limit));
        }
        Ok(())
    }

    /// Time left, for clamping a command's own wall clock to the budget.
    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn a_fresh_budget_is_open() {
        assert_eq!(Budget::within(Duration::from_secs(60)).check(), Ok(()));
    }

    #[test]
    fn a_spent_deadline_is_named() {
        let budget = Budget::within(Duration::ZERO);
        assert!(matches!(budget.check(), Err(Exhausted::Deadline(_))));
    }

    #[test]
    fn the_cancel_hook_ends_the_budget_before_its_deadline() {
        let flag = Arc::new(AtomicBool::new(false));
        let seen = flag.clone();
        let budget = Budget::within(Duration::from_secs(60))
            .with_cancel(move || seen.load(Ordering::SeqCst));
        assert_eq!(budget.check(), Ok(()));
        flag.store(true, Ordering::SeqCst);
        assert_eq!(budget.check(), Err(Exhausted::Cancelled));
    }
}
