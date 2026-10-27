//! `notify` reports whose reply missed its bound.
//!
//! A hook the host is waiting on cannot wait for a socket that is not
//! answering, so `run_notify` gives up at the report's budget and abandons the
//! send (`within_budget`). For `SessionEnd` that budget is 300 ms
//! ([`super::SESSION_END_NOTIFY_BUDGET`]), and the report it carries is the one
//! that clears an approval GitPulse is still showing, so a miss is not
//! harmless. Until now it left one stderr line in the host's debug log and no
//! number anywhere.
//!
//! The count has the shape `ledger::LedgerStatus` gives its appends:
//! `dropped`, and a `dropped_scope` saying whose count it is. It mirrors that
//! shape rather than extending the ledger's own, because these are not ledger
//! appends and the ledger snapshot is read by surfaces that mean exactly that.
//!
//! Scope is the one the ledger's is: `this_process`. A hook is a one-shot
//! process, so a nonzero count is something only that run can say, and a
//! reader's 0 is not a verdict on any other. Carrying it to a person needs a
//! durable record or the app's own counters (`alerts::bridge`); see the lane
//! report for the follow-up. Nothing here pretends otherwise.

use std::sync::atomic::{AtomicU64, Ordering};

/// The count and whose it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropSnapshot {
    /// Reports this process abandoned at their deadline. Not confirmed
    /// delivered and not retried; the app may or may not have received them.
    pub dropped: u64,
    /// What `dropped` counts: [`crate::ledger::DROPPED_SCOPE`].
    pub dropped_scope: String,
}

/// Counts the reports a process gave up on.
#[derive(Debug, Default)]
pub struct NotifyDrops(AtomicU64);

impl NotifyDrops {
    pub const fn new() -> Self {
        NotifyDrops(AtomicU64::new(0))
    }

    /// Records one abandoned report and returns the count including it.
    pub fn record(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed).saturating_add(1)
    }

    pub fn snapshot(&self) -> DropSnapshot {
        DropSnapshot {
            dropped: self.0.load(Ordering::Relaxed),
            dropped_scope: crate::ledger::DROPPED_SCOPE.to_string(),
        }
    }
}

/// The process's own count, which `dispatch` passes in. Tests pass their own.
pub(super) static PROCESS_DROPS: NotifyDrops = NotifyDrops::new();
