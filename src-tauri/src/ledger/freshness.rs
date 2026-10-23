//! How current a ledger read is, and what the last catch-up covered.
//!
//! A ledger answer with no time on it reads as "this is everything". Nothing
//! launches `gitpulsed` on its own, so the rows a read-only surface (the MCP
//! tools, the insights snapshot) can see are only as new as the last pass of
//! whichever process records. This module computes that, from data the ledger
//! already holds: the newest event row and the `catch_up_state` rows each
//! catch-up leaves behind. Nothing here writes except [`record_catch_up`], and
//! nothing here opens a database for writing to answer a read.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::{ids, with_conn, write_tx, LedgerError};

/// Minutes, in seconds, after which a ledger with no newer evidence of a
/// recorder is called stale: ten, twice the daemon's default five-minute cycle,
/// so one missed cycle is not an alarm but two are.
pub const STALE_AFTER_SECS: u64 = 600;

/// How far into the future evidence may be dated before the clock, not the
/// recorder, is suspected. Small: a step of a minute is NTP's business; more
/// means the clock was set backwards and "age" has no meaning.
const FUTURE_TOLERANCE_SECS: u64 = 60;

/// What the latest catch-up passes covered, summed over the worktrees whose
/// last pass is current (see `stale_worktrees` and `future_dated_worktrees`). Every field is the figure the pass itself reported.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatchUpSummary {
    /// Worktrees with a recorded pass.
    pub worktrees: i64,
    /// True when any of those passes was cut short or left files unlisted.
    pub truncated: bool,
    /// Events written by those passes.
    pub recorded: i64,
    /// Transcript files read.
    pub transcripts: i64,
    /// Transcript files the corpus walk listed.
    pub transcripts_listed: i64,
    /// Transcript files the walk saw and did not list (depth bound or file
    /// cap). A lower bound when the pass said so in `error`.
    pub transcripts_unlisted: i64,
    /// Transcript lines this build could not read. Non-zero means the history
    /// is known to be incomplete.
    pub skipped_lines: i64,
    /// Records whose ledger write failed.
    pub failed_records: i64,
    /// What stopped or limited the first current row that has one, empty when
    /// none did.
    pub error: String,
    /// Rows whose last completed pass is older than the stale window: idle or
    /// removed worktrees. Counted here and left out of the figures above, so a
    /// day-old failure is not reported as the current coverage. The table keeps
    /// one row per worktree (by a hash of its path), so this is a count, not a
    /// list: the figures are per ledger, not per worktree.
    pub stale_worktrees: i64,
    /// Rows whose completion time is in the future (a skewed clock): flagged,
    /// left out of the figures and out of freshness.
    pub future_dated_worktrees: i64,
}

/// The freshness block carried by `gitpulse_ledger_events`,
/// `gitpulse_status`, the insights ledger facet and the `ledger` resource.
///
/// The default is "not read": a surface that never computed freshness must not
/// serialise as fresh.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Freshness {
    /// `ts_utc` of the newest event row this read could see; null for an empty
    /// or unreadable ledger. This is when the *newest row was appended*, not
    /// the time of the event it describes and not the time of this read.
    pub as_of: Option<String>,
    /// When the last catch-up pass completed, by `last_catch_up_basis`:
    /// `catch_up_state` is the newest `completed_at` any pass persisted;
    /// `newest_row` is a lower bound taken from the newest event row, used when
    /// no pass has been persisted (an older writer, or none has run) and blind
    /// to passes that found nothing to write; `none` is null.
    pub last_catch_up: Option<String>,
    pub last_catch_up_basis: String,
    /// True when no evidence of a recorder (the newer of `last_catch_up` and
    /// `as_of`) is within `stale_after_secs` of now, or when there is none, or
    /// when it cannot be trusted (future-dated, unparseable, unreadable). Never
    /// false for a read that could not run.
    pub stale: bool,
    /// Why `stale` is true; empty when it is false.
    pub stale_reason: String,
    pub stale_after_secs: u64,
    /// False when the ledger could not be read to compute this; `stale` is then
    /// true and `freshness_error` says why.
    pub freshness_ok: bool,
    pub freshness_error: String,
    /// What the persisted catch-up passes covered; null when none persisted.
    pub catch_up: Option<CatchUpSummary>,
}

impl Default for Freshness {
    fn default() -> Self {
        Freshness::unavailable("freshness was not read for this answer")
    }
}

impl Freshness {
    /// A read that could not establish freshness: stale, with the reason.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Freshness {
            as_of: None,
            last_catch_up: None,
            last_catch_up_basis: "none".to_string(),
            stale: true,
            stale_reason: reason.clone(),
            stale_after_secs: STALE_AFTER_SECS,
            freshness_ok: false,
            freshness_error: reason,
            catch_up: None,
        }
    }
}

/// One persisted pass as read back.
struct StateRow {
    completed_at: Option<String>,
    truncated: i64,
    recorded: i64,
    transcripts: i64,
    listed: i64,
    unlisted: i64,
    skipped: i64,
    failed: i64,
    error: String,
}

/// Computes freshness from an open read-only connection at `now_ms`.
///
/// The clock is a parameter so a test can set it, including backwards.
pub(super) fn read(conn: &Connection, now_ms: u64) -> Result<Freshness, LedgerError> {
    let failed = |e: rusqlite::Error| LedgerError::new("freshness_failed", e.to_string());
    let as_of: Option<String> = conn
        .query_row(
            "SELECT ts_utc FROM events ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(failed)?;
    let rows = if super::has_table(conn, "catch_up_state")? {
        let mut stmt = conn
            .prepare(
                "SELECT completed_at, COALESCE(truncated, 0), COALESCE(recorded, 0),
                        COALESCE(transcripts, 0), COALESCE(transcripts_listed, 0),
                        COALESCE(transcripts_unlisted, 0), COALESCE(skipped_lines, 0),
                        COALESCE(failed_records, 0), COALESCE(error, '')
                 FROM catch_up_state",
            )
            .map_err(failed)?;
        let mapped = stmt
            .query_map([], |row| {
                Ok(StateRow {
                    completed_at: row.get(0)?,
                    truncated: row.get(1)?,
                    recorded: row.get(2)?,
                    transcripts: row.get(3)?,
                    listed: row.get(4)?,
                    unlisted: row.get(5)?,
                    skipped: row.get(6)?,
                    failed: row.get(7)?,
                    error: row.get(8)?,
                })
            })
            .map_err(failed)?;
        mapped.collect::<Result<Vec<_>, _>>().map_err(failed)?
    } else {
        Vec::new()
    };
    Ok(assess(as_of, rows, now_ms))
}

fn assess(as_of: Option<String>, rows: Vec<StateRow>, now_ms: u64) -> Freshness {
    // A timestamp that is not in the ledger's shape is not evidence of anything.
    let well_formed = |ts: &Option<String>| {
        ts.as_deref()
            .filter(|ts| super::valid_utc_timestamp(ts))
            .map(str::to_owned)
    };
    let as_of = well_formed(&as_of);
    let future_after = now_ms.saturating_add(FUTURE_TOLERANCE_SECS * 1000);
    let window = STALE_AFTER_SECS * 1000;

    // Each row is one worktree's newest pass. A pass dated in the future is
    // flagged and left out of every figure below: it has no age, and it may not
    // veto another row's genuine freshness. A pass that completed longer ago
    // than the stale window belongs to a worktree that is idle or gone; it is
    // counted, not summed, so its day-old failure is not today's coverage.
    // A row with no completion (an attempt that did not finish) is current.
    enum Kind {
        Current,
        Dormant,
        Future,
    }
    let kind = |row: &StateRow| match well_formed(&row.completed_at)
        .and_then(|ts| ids::parse_event_time(&ts))
    {
        None => Kind::Current,
        Some(at) if at > future_after => Kind::Future,
        Some(at) if now_ms.saturating_sub(at) > window => Kind::Dormant,
        Some(_) => Kind::Current,
    };
    let mut future_dated = 0i64;
    let mut dormant = 0i64;
    let mut genuine_pass: Option<String> = None;
    let mut summary = CatchUpSummary {
        worktrees: i64::try_from(rows.len()).unwrap_or(i64::MAX),
        ..CatchUpSummary::default()
    };
    for row in &rows {
        match kind(row) {
            Kind::Future => {
                future_dated += 1;
                continue;
            }
            Kind::Dormant => dormant += 1,
            Kind::Current => {}
        }
        let completed = well_formed(&row.completed_at);
        if completed > genuine_pass {
            genuine_pass = completed;
        }
        if matches!(kind(row), Kind::Dormant) {
            continue;
        }
        summary.truncated |= row.truncated != 0 || row.unlisted > 0;
        summary.recorded = summary.recorded.saturating_add(row.recorded);
        summary.transcripts = summary.transcripts.saturating_add(row.transcripts);
        summary.transcripts_listed = summary.transcripts_listed.saturating_add(row.listed);
        summary.transcripts_unlisted = summary.transcripts_unlisted.saturating_add(row.unlisted);
        summary.skipped_lines = summary.skipped_lines.saturating_add(row.skipped);
        summary.failed_records = summary.failed_records.saturating_add(row.failed);
        if summary.error.is_empty() && !row.error.is_empty() {
            summary.error = row.error.clone();
        }
    }
    summary.stale_worktrees = dormant;
    summary.future_dated_worktrees = future_dated;
    let catch_up = (!rows.is_empty()).then_some(summary);

    let (last_catch_up, basis) = match (&genuine_pass, &as_of) {
        (Some(pass), _) => (Some(pass.clone()), "catch_up_state"),
        (None, Some(row)) => (Some(row.clone()), "newest_row"),
        (None, None) => (None, "none"),
    };
    // With a skewed writer in the ledger, its event rows are as suspect as its
    // passes: only a genuine pass counts as evidence. Without one, the newer of
    // the last pass and the newest row does.
    let evidence = if future_dated > 0 {
        genuine_pass.as_ref()
    } else {
        [&genuine_pass, &as_of].into_iter().flatten().max()
    };
    let (stale, stale_reason) = match evidence {
        None if future_dated > 0 => (
            true,
            format!(
                "{future_dated} catch-up row(s) are dated in the future (the clock was set \
                 backwards) and no genuine pass exists, so freshness cannot be told"
            ),
        ),
        None => (
            true,
            "the ledger holds no events and no catch-up has completed, so nothing shows a \
             recorder has run"
                .to_string(),
        ),
        Some(evidence) => match ids::parse_event_time(evidence) {
            None => (
                true,
                format!("the newest evidence timestamp {evidence:?} could not be parsed"),
            ),
            Some(at) => {
                if at > future_after {
                    (
                        true,
                        format!(
                            "the newest evidence ({evidence}) is {}s in the future: the clock \
                             was set backwards, so its age cannot be told",
                            (at - now_ms) / 1000
                        ),
                    )
                } else if now_ms.saturating_sub(at) > window {
                    (
                        true,
                        format!(
                            "no catch-up or recorded event newer than {}s (newest {evidence})",
                            STALE_AFTER_SECS
                        ),
                    )
                } else {
                    (false, String::new())
                }
            }
        },
    };
    Freshness {
        as_of,
        last_catch_up,
        last_catch_up_basis: basis.to_string(),
        stale,
        stale_reason,
        stale_after_secs: STALE_AFTER_SECS,
        freshness_ok: true,
        freshness_error: String::new(),
        catch_up,
    }
}

/// [`read`] for a repository, opening the ledger read-only. A ledger that is
/// absent, locked or unreadable yields [`Freshness::unavailable`] with the
/// reason, never a fresh-looking default.
pub fn freshness_readonly_at(repo_path: &str, now_ms: u64) -> Freshness {
    let opened = super::open_readonly_initialised(&super::ledger_path(repo_path));
    match opened.and_then(|conn| read(&conn, now_ms)) {
        Ok(freshness) => freshness,
        Err(error) => Freshness::unavailable(error.to_string()),
    }
}

/// [`freshness_readonly_at`] at the wall clock.
pub fn freshness_readonly(repo_path: &str) -> Freshness {
    freshness_readonly_at(repo_path, ids::now_millis())
}

/// One finished pass, as persisted per worktree.
#[derive(Debug, Clone, Default)]
pub struct CatchUpRecord {
    /// False for a pass that did not run (the family could not be resolved, no
    /// home directory): its counts and error are kept, but it is not a
    /// completion, so it leaves `completed_at` as it was.
    pub completed: bool,
    pub truncated: bool,
    pub recorded: i64,
    pub transcripts: i64,
    pub transcripts_listed: i64,
    pub transcripts_unlisted: i64,
    pub skipped_lines: i64,
    pub reflog_entries: i64,
    pub failed_records: i64,
    pub duplicates_refused: i64,
    pub error: String,
}

/// Persists a pass's outcome for `worktree_path` into the ledger at
/// `ledger_repo`, stamped at `now_ms`. Does nothing for a repository with no
/// ledger: a pass is not a reason to create one.
pub fn record_catch_up(
    ledger_repo: &str,
    worktree_path: &str,
    record: &CatchUpRecord,
    now_ms: u64,
) -> Result<(), LedgerError> {
    if !super::is_initialised(ledger_repo) {
        return Ok(());
    }
    let worktree = super::transcript_key(std::path::Path::new(worktree_path));
    let completed_at = record.completed.then(|| ids::iso8601_utc(now_ms));
    let failed = |e: rusqlite::Error| LedgerError::new("catch_up_state_failed", e.to_string());
    with_conn(ledger_repo, |conn| {
        if !super::has_table(conn, "catch_up_state")? {
            return Ok(());
        }
        let tx = write_tx(conn).map_err(failed)?;
        tx.execute(
            "INSERT INTO catch_up_state (
                worktree_key, completed_at, truncated, recorded, transcripts,
                transcripts_listed, transcripts_unlisted, skipped_lines,
                reflog_entries, failed_records, duplicates_refused, error
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(worktree_key) DO UPDATE SET
                completed_at = COALESCE(excluded.completed_at, completed_at),
                truncated = excluded.truncated,
                recorded = excluded.recorded,
                transcripts = excluded.transcripts,
                transcripts_listed = excluded.transcripts_listed,
                transcripts_unlisted = excluded.transcripts_unlisted,
                skipped_lines = excluded.skipped_lines,
                reflog_entries = excluded.reflog_entries,
                failed_records = excluded.failed_records,
                duplicates_refused = excluded.duplicates_refused,
                error = excluded.error",
            params![
                worktree,
                completed_at,
                i64::from(record.truncated),
                record.recorded,
                record.transcripts,
                record.transcripts_listed,
                record.transcripts_unlisted,
                record.skipped_lines,
                record.reflog_entries,
                record.failed_records,
                record.duplicates_refused,
                record.error,
            ],
        )
        .map_err(failed)?;
        tx.commit().map_err(failed)
    })
}

#[cfg(test)]
#[path = "freshness_tests.rs"]
mod tests;
