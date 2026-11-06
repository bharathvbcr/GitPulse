//! Bounded retention for the three append-only workbench tables.
//!
//! No schema change: a receipt's age is the `at` of the event it points at, and
//! every rule below is a predicate over existing rows. See "Retention and
//! compaction" in docs/WORKBENCH.md for the policy this implements.

use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use std::time::Duration;

use super::input::Input;
use super::{Error, MAX_INTEGER, Result};

const HOUR: i64 = 3_600;
const DAY: i64 = 86_400;
/// One pass deletes at most this many rows per table unless asked for less.
const DEFAULT_LIMIT: i64 = 10_000;
const MAX_LIMIT: i64 = 100_000;
/// How long a truncating checkpoint waits for a reader. A longer wait is the
/// connection's own busy timeout, which is for writers, not for this receipt.
const CHECKPOINT_WAIT: Duration = Duration::from_millis(250);
/// Launch claims return `claim_consumed` on replay, which is only safe while
/// the receipt exists. They are never pruned.
const CLAIMS: &str = "('runs.claim','notifications.claim','decisions.claim')";
/// Runs that can still change or spawn work; every revision of one is kept.
const LIVE_RUN: &str = "('prepared','starting','running','unresolved')";
const OPEN_DECISION: &str = "('pending','decided','dispatching')";

pub(super) fn compact(input: &Input<'_>, now: i64) -> Result<String> {
    input.fields(&[
        "receipt_window_seconds",
        "event_retention_seconds",
        "keep_events",
        "keep_revisions",
        "limit",
    ])?;
    let receipt_window = input.integer("receipt_window_seconds", Some(7 * DAY), MAX_INTEGER)?;
    let event_retention = input.integer("event_retention_seconds", Some(30 * DAY), MAX_INTEGER)?;
    let keep_events = input.integer("keep_events", Some(1_000), MAX_INTEGER)?;
    let keep_revisions = input.integer("keep_revisions", Some(50), MAX_INTEGER)?;
    let limit = input.integer("limit", Some(DEFAULT_LIMIT), MAX_LIMIT)?;
    if receipt_window < HOUR {
        return Err(Error::invalid(
            "receipt_window_seconds must be at least 3600: a retry inside the window must find its receipt",
        ));
    }
    if event_retention < receipt_window {
        return Err(Error::invalid(
            "event_retention_seconds must not be shorter than receipt_window_seconds",
        ));
    }
    if keep_events < 1 || keep_revisions < 1 || limit < 1 {
        return Err(Error::invalid(
            "keep_events, keep_revisions and limit must be at least one",
        ));
    }
    let conn = input.conn;
    // The caller must not already be in a transaction: the deletes commit
    // before the checkpoint, and a checkpoint inside that transaction cannot
    // see the pages it would be reporting.
    if !conn.is_autocommit() {
        return Err(Error::invalid(
            "store.compact checkpoints the WAL and cannot run inside a transaction",
        ));
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let receipt_cutoff = now.saturating_sub(receipt_window);
    let event_cutoff = now.saturating_sub(event_retention);

    // Receipts first: an event is only deletable once no receipt points at it.
    // A receipt whose event is missing is kept (fail closed: its age is unknown).
    let receipt_rule = format!(
        "FROM work_requests r JOIN work_events e ON e.sequence=r.sequence
         WHERE e.at<?1 AND r.method NOT IN {CLAIMS}"
    );
    let receipts_eligible: i64 = conn.query_row(
        &format!("SELECT count(*) {receipt_rule}"),
        [receipt_cutoff],
        |r| r.get(0),
    )?;
    let receipts = conn.execute(
        &format!(
            "DELETE FROM work_requests WHERE id IN (SELECT r.id {receipt_rule} ORDER BY r.sequence LIMIT ?2)"
        ),
        params![receipt_cutoff, limit],
    )? as i64;

    // Events: old, not among the newest `keep_events`, not a receipt's event and
    // not an attention row's source (a foreign key).
    let event_rule = "FROM work_events e
         WHERE e.at<?1
           AND e.sequence<=(SELECT coalesce(max(sequence),0) FROM work_events)-?2
           AND NOT EXISTS(SELECT 1 FROM work_requests r WHERE r.sequence=e.sequence)
           AND NOT EXISTS(SELECT 1 FROM work_attention a WHERE a.source_sequence=e.sequence)";
    let events_eligible: i64 = conn.query_row(
        &format!("SELECT count(*) {event_rule}"),
        params![event_cutoff, keep_events],
        |r| r.get(0),
    )?;
    let events = conn.execute(
        &format!("DELETE FROM work_events WHERE sequence IN (SELECT e.sequence {event_rule} ORDER BY e.sequence LIMIT ?3)"),
        params![event_cutoff, keep_events, limit],
    )? as i64;

    // Revisions: keep the newest `keep_revisions` of every record (the newest is
    // the live row an expected_revision is checked against) and all of an open
    // decision's or live run's history.
    let revision_rule = format!(
        "FROM work_revisions v
         WHERE v.revision<=(SELECT max(x.revision) FROM work_revisions x
                             WHERE x.entity_type=v.entity_type AND x.entity_id=v.entity_id)-?1
           AND NOT EXISTS(SELECT 1 FROM work_decisions d WHERE v.entity_type='decision'
                             AND d.id=v.entity_id AND d.state IN {OPEN_DECISION})
           AND NOT EXISTS(SELECT 1 FROM work_runs u WHERE v.entity_type='run'
                             AND u.id=v.entity_id AND u.state IN {LIVE_RUN})"
    );
    let revisions_eligible: i64 = conn.query_row(
        &format!("SELECT count(*) {revision_rule}"),
        [keep_revisions],
        |r| r.get(0),
    )?;
    let revisions = conn.execute(
        &format!(
            "DELETE FROM work_revisions WHERE rowid IN (SELECT v.rowid {revision_rule} LIMIT ?2)"
        ),
        params![keep_revisions, limit],
    )? as i64;
    tx.commit()?;

    // The freelist above is not read here. Until a checkpoint copies the WAL
    // back into the main file, that count includes pages the WAL still holds,
    // and incremental_vacuum's truncation waits for the same checkpoint.
    let vacuum = reclaim(conn, limit)?;

    Ok(conn.query_row(
        "SELECT json_object('ok',json('true'),
            'item',json_object('id','compaction','revision',1,
              'receipts',json_object('deleted',?1,'eligible',?2,'truncated',json(CASE WHEN ?2>?1 THEN 'true' ELSE 'false' END)),
              'events',json_object('deleted',?3,'eligible',?4,'truncated',json(CASE WHEN ?4>?3 THEN 'true' ELSE 'false' END)),
              'revisions',json_object('deleted',?5,'eligible',?6,'truncated',json(CASE WHEN ?6>?5 THEN 'true' ELSE 'false' END)),
              'vacuum',json_object('incremental',json(CASE WHEN ?7 THEN 'true' ELSE 'false' END),'free_pages_before',?8,'free_pages_after',?9,'checkpoint',?10)),
            'sequence',(SELECT coalesce(max(sequence),0) FROM work_events))",
        params![
            receipts,
            receipts_eligible,
            events,
            events_eligible,
            revisions,
            revisions_eligible,
            vacuum.incremental,
            vacuum.free_before,
            vacuum.free_after,
            vacuum.checkpoint,
        ],
        |r| r.get(0),
    )?)
}

struct VacuumReport {
    incremental: bool,
    free_before: i64,
    free_after: i64,
    checkpoint: &'static str,
}

struct Checkpoint {
    busy: i64,
    log: i64,
    checkpointed: i64,
}

impl Checkpoint {
    /// Every frame was copied and the checkpoint was not blocked. `busy != 0`
    /// means a reader stopped the reset, so the WAL can still hold the pages.
    fn cleared(&self) -> bool {
        self.busy == 0 && self.checkpointed == self.log
    }
}

/// Checkpoint, then measure the freelist the main file actually holds.
///
/// A WAL database reports `incremental: false` and the on-disk header freelist
/// (the same value before and after) when the checkpoint is blocked. That
/// header does not include frames the WAL still holds. An in-memory or DELETE
/// database has no WAL, so the pragma counts are the file.
fn reclaim(conn: &Connection, limit: i64) -> Result<VacuumReport> {
    let journal: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    let wal = journal.eq_ignore_ascii_case("wal");
    if wal {
        let copied = checkpoint_wal(conn)?;
        if !copied.cleared() {
            let on_disk = main_file_freelist(conn)?;
            return Ok(VacuumReport {
                incremental: false,
                free_before: on_disk,
                free_after: on_disk,
                checkpoint: "blocked",
            });
        }
    }

    let mode: i64 = conn.query_row("PRAGMA auto_vacuum", [], |row| row.get(0))?;
    let free_before: i64 = conn.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
    if mode == 2 && free_before > 0 {
        // The pragma frees one page per step; draining its rows runs it out.
        // `limit` is a validated integer, not caller text.
        let mut vacuum = conn.prepare(&format!("PRAGMA incremental_vacuum({limit})"))?;
        let mut rows = vacuum.query([])?;
        while rows.next()?.is_some() {}
    }
    if wal {
        let copied = checkpoint_wal(conn)?;
        if !copied.cleared() {
            let on_disk = main_file_freelist(conn)?;
            return Ok(VacuumReport {
                incremental: false,
                free_before: on_disk,
                free_after: on_disk,
                checkpoint: "blocked",
            });
        }
    }
    let free_after: i64 = conn.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
    Ok(VacuumReport {
        incremental: mode == 2,
        free_before,
        free_after,
        checkpoint: if wal { "cleared" } else { "not_wal" },
    })
}

fn checkpoint_wal(conn: &Connection) -> Result<Checkpoint> {
    let previous: i64 = conn.query_row("PRAGMA busy_timeout", [], |row| row.get(0))?;
    let previous_ms = u64::try_from(previous)
        .map_err(|_| Error::invalid("SQLite returned a negative busy_timeout"))?;
    conn.busy_timeout(CHECKPOINT_WAIT)?;
    let attempt = (|| -> Result<Checkpoint> {
        let truncate = read_checkpoint(conn, "PRAGMA wal_checkpoint(TRUNCATE)")?;
        if truncate.cleared() {
            return Ok(truncate);
        }
        read_checkpoint(conn, "PRAGMA wal_checkpoint(PASSIVE)")
    })();
    let restored = conn.busy_timeout(Duration::from_millis(previous_ms));
    match (attempt, restored) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
        (Ok(checkpoint), Ok(())) => Ok(checkpoint),
    }
}

fn read_checkpoint(conn: &Connection, pragma: &str) -> Result<Checkpoint> {
    match conn.query_row(pragma, [], |row| {
        Ok(Checkpoint {
            busy: row.get(0)?,
            log: row.get(1)?,
            checkpointed: row.get(2)?,
        })
    }) {
        Ok(checkpoint) => Ok(checkpoint),
        Err(error) if checkpoint_blocked(&error) => Ok(Checkpoint {
            busy: 1,
            log: 0,
            checkpointed: 0,
        }),
        Err(error) => Err(error.into()),
    }
}

fn checkpoint_blocked(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked,
                ..
            },
            _
        )
    )
}

/// The freelist count in the main database header, byte offset 36.
///
/// SQLite updates this when a checkpoint copies page 1 back. The connection's
/// `freelist_count` pragma also sees uncheckpointed WAL frames, which is the
/// count this receipt must not publish.
fn main_file_freelist(conn: &Connection) -> Result<i64> {
    let path: String = conn.query_row(
        "SELECT file FROM pragma_database_list WHERE name='main'",
        [],
        |row| row.get(0),
    )?;
    if path.is_empty() {
        return Err(Error::invalid(
            "WAL checkpoint has no main database file to read",
        ));
    }
    let bytes = std::fs::read(&path).map_err(|error| {
        Error::invalid(format!("reading the main database header failed: {error}"))
    })?;
    if bytes.len() < 40 {
        return Err(Error::invalid(
            "main database header is shorter than 40 bytes",
        ));
    }
    let mut raw = [0u8; 4];
    raw.copy_from_slice(&bytes[36..40]);
    Ok(i64::from(u32::from_be_bytes(raw)))
}
