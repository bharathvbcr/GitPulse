//! Bounded retention for the three append-only workbench tables.
//!
//! No schema change: a receipt's age is the `at` of the event it points at, and
//! every rule below is a predicate over existing rows. See "Retention and
//! compaction" in docs/WORKBENCH.md for the policy this implements.

use rusqlite::params;

use super::input::Input;
use super::{Error, MAX_INTEGER, Result};

const HOUR: i64 = 3_600;
const DAY: i64 = 86_400;
/// One pass deletes at most this many rows per table unless asked for less.
const DEFAULT_LIMIT: i64 = 10_000;
const MAX_LIMIT: i64 = 100_000;
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

    // Deleting rows frees pages inside the file; only an auto_vacuum=INCREMENTAL
    // database can hand them back, and then no more than `limit` of them per pass.
    let mode: i64 = conn.query_row("PRAGMA auto_vacuum", [], |r| r.get(0))?;
    let free_before: i64 = conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
    if mode == 2 && free_before > 0 {
        // The pragma frees one page per step; draining its rows runs it out.
        let mut vacuum = conn.prepare(&format!("PRAGMA incremental_vacuum({limit})"))?;
        let mut rows = vacuum.query([])?;
        while rows.next()?.is_some() {}
    }
    let free_after: i64 = conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;

    Ok(conn.query_row(
        "SELECT json_object('ok',json('true'),
            'item',json_object('id','compaction','revision',1,
              'receipts',json_object('deleted',?1,'eligible',?2,'truncated',json(CASE WHEN ?2>?1 THEN 'true' ELSE 'false' END)),
              'events',json_object('deleted',?3,'eligible',?4,'truncated',json(CASE WHEN ?4>?3 THEN 'true' ELSE 'false' END)),
              'revisions',json_object('deleted',?5,'eligible',?6,'truncated',json(CASE WHEN ?6>?5 THEN 'true' ELSE 'false' END)),
              'vacuum',json_object('incremental',json(CASE WHEN ?7=2 THEN 'true' ELSE 'false' END),'free_pages_before',?8,'free_pages_after',?9)),
            'sequence',(SELECT coalesce(max(sequence),0) FROM work_events))",
        params![receipts, receipts_eligible, events, events_eligible, revisions, revisions_eligible, mode, free_before, free_after],
        |r| r.get(0),
    )?)
}
