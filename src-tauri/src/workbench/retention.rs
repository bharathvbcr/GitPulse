//! Bounded retention for the profile's append-only tables.
//!
//! dc-store's `store.compact` deletes expired receipts, events and revisions,
//! a bounded number per pass, and nothing else called it. This runs it from
//! the host, at most once per profile per day:
//!
//! * the last attempt is stamped in a file beside the profile, so a restart
//!   does not start another window, and two GitPulse processes share one;
//! * the stamp is written *before* the pass, so a failure is retried next
//!   window rather than on every tick, and is logged as a failure, never as a
//!   pass that deleted nothing;
//! * a pass runs on a thread of its own with a connection of its own, so the
//!   UI's store lock is never held for it, and a flag keeps it from running
//!   beside itself;
//! * it never creates a profile.
//!
//! The scheduler is the host's existing background loop
//! ([`super::external_changes`]); this adds no thread of its own beyond the
//! pass. There is no host-load signal on this branch to defer under: when one
//! lands, it belongs in [`Retention::tick`] beside the due check.

use super::WorkbenchState;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// One pass per profile per this long.
pub(crate) const WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
/// How often the loop looks at the stamp. Far under the window, far over a poll.
const CHECK_EVERY: Duration = Duration::from_secs(10 * 60);

/// What one call did. `Failed` is never folded into `Skipped`.
#[derive(Debug, PartialEq)]
pub(crate) enum Outcome {
    NotDue,
    /// Another pass is running.
    Busy,
    /// There is no profile yet; none is created.
    NoProfile,
    Ran(String),
    Failed(String),
}

fn stamp_path(profile: &Path) -> PathBuf {
    let mut name = profile.as_os_str().to_owned();
    name.push(".compacted");
    PathBuf::from(name)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Whether a window has passed since the stamped attempt. A missing or
/// unreadable stamp is due; so is one from the future beyond a window (a clock
/// set back), which would otherwise never expire.
fn due(stamp: &Path, now: u64) -> bool {
    let Some(last) = std::fs::read_to_string(stamp)
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
    else {
        return true;
    };
    now >= last.saturating_add(WINDOW.as_secs()) || last > now.saturating_add(WINDOW.as_secs())
}

fn write_stamp(stamp: &Path, now: u64) -> std::io::Result<()> {
    let temp = stamp.with_extension("compacted.tmp");
    std::fs::write(&temp, now.to_string())?;
    std::fs::rename(&temp, stamp)
}

/// The numbers of one pass: what was deleted, what was eligible, and whether
/// the pass stopped at its bound with more left.
fn summary(report: &Value) -> Result<String, String> {
    let item = &report["item"];
    let mut parts = Vec::new();
    for table in ["receipts", "events", "revisions"] {
        let row = &item[table];
        let (Some(deleted), Some(eligible), Some(truncated)) = (
            row["deleted"].as_i64(),
            row["eligible"].as_i64(),
            row["truncated"].as_bool(),
        ) else {
            return Err(format!("store.compact answered without {table} counts"));
        };
        parts.push(format!(
            "{table} deleted={deleted} eligible={eligible} truncated={truncated}"
        ));
    }
    Ok(parts.join("; "))
}

/// Run one pass if the window has passed and none is running. `pass` does the
/// compaction and returns the store's answer; it is called at most once.
pub(crate) fn run_once(
    profile: &Path,
    running: &AtomicBool,
    now: u64,
    pass: impl FnOnce() -> Result<Value, String>,
) -> Outcome {
    if running
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Outcome::Busy;
    }
    let outcome = (|| {
        if !profile.exists() {
            return Outcome::NoProfile;
        }
        let stamp = stamp_path(profile);
        if !due(&stamp, now) {
            return Outcome::NotDue;
        }
        if let Err(error) = write_stamp(&stamp, now) {
            // Without a stamp every tick would be another pass.
            return Outcome::Failed(format!("cannot record the attempt: {error}"));
        }
        match pass().and_then(|report| summary(&report)) {
            Ok(text) => Outcome::Ran(text),
            Err(error) => Outcome::Failed(error),
        }
    })();
    running.store(false, Ordering::Release);
    outcome
}

fn log_outcome(outcome: &Outcome) {
    match outcome {
        Outcome::Ran(text) => log::info!(target: "workbench", "retention: {text}"),
        Outcome::Failed(error) => log::warn!(
            target: "workbench",
            "retention failed and will be retried next window: {error}"
        ),
        Outcome::NotDue | Outcome::Busy | Outcome::NoProfile => {}
    }
}

/// The loop's side: looks at the stamp every [`CHECK_EVERY`] and hands a due
/// pass to a thread.
pub(crate) struct Retention {
    running: Arc<AtomicBool>,
    checked: Option<Instant>,
}

impl Retention {
    pub(crate) fn new() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            checked: None,
        }
    }

    /// Whether the stamp is due a look at `now`; when it is, records the look.
    pub(crate) fn due_to_check(&mut self, now: Instant) -> bool {
        if self
            .checked
            .is_some_and(|at| now.saturating_duration_since(at) < CHECK_EVERY)
        {
            return false;
        }
        self.checked = Some(now);
        true
    }

    pub(crate) fn tick(&mut self, host: &WorkbenchState) {
        if !self.due_to_check(Instant::now()) {
            return;
        }
        let Ok(profile) = host.profile_path() else {
            return;
        };
        if self.running.load(Ordering::Acquire) || !due(&stamp_path(&profile), now_secs()) {
            return;
        }
        let running = Arc::clone(&self.running);
        let host = host.clone();
        if let Err(error) =
            crate::supervisor::spawn_once("workbench-retention", "workbench retention", move || {
                let outcome = run_once(&profile, &running, now_secs(), || {
                    host.check_open().map_err(|e| e.message)?;
                    let store = dc_store::Store::open(&profile).map_err(|e| e.to_string())?;
                    super::query(&store, "store.compact", "{}").map_err(|e| e.message)
                });
                log_outcome(&outcome);
            })
        {
            log::warn!(target: "workbench", "retention did not start: {error}");
        }
    }
}

#[cfg(test)]
#[path = "retention_tests.rs"]
mod tests;
