//! CI results on an agent attempt's branch, recorded as task-scoped attention.
//!
//! A host watcher (GitHub Actions run conclusions, a local CI verdict) reports
//! what it observed with `signals.record`. The store turns it into one inbox
//! notice through the same `attention::record_event` every other control event
//! uses, so `notifications.*` delivers it with no delivery code of its own. A
//! signal is a fact the store was told, not an acceptance: it never changes a
//! task, a run or a proposal, and it carries no authority.
//!
//! **Target and staleness.** The notice targets the attempt (`target_type`
//! `run`), but it is not tied to that attempt's revision. Two reasons it still
//! says `run`, both about the consumers rather than this crate: every inbox
//! decoder holds the closed set `run | enhancement | decision` and requires
//! `kind` to start with `<target_type>_`, so a `task` target (or an unprefixed
//! `ci_failed`) would make a reader refuse the whole page, not just the row;
//! the stored kinds are therefore `run_ci_passed`, `run_ci_failed` and
//! `run_ci_cancelled`.
//!
//! A CI result describes a branch, and it usually arrives while the attempt is
//! still live. Comparing the run's revision would mark it `changed` on the next
//! claim, start or finish, and native delivery skips anything not `current`.
//! `attention::body_sql` therefore gives a signal its own rule: it stays
//! `current` through any run or task write, and becomes `changed` only when a
//! later signal for the same `(task, source, workflow, branch)` exists. That
//! is the whole supersession mechanism; this module records, the inbox reads.
//!
//! Nothing here adds a table, column or index: the signal's facts ride in the
//! attention body's `signal` object, and de-duplication reads them back through
//! the existing `work_attention_task` index.
use super::{Error, Input, Result, attention, decisions};
use rusqlite::{OptionalExtension, params};

/// Where a result was observed. Closed: a reader decides what each means.
const SOURCES: [&str; 2] = ["github", "ci_local"];
/// The stored attention kind is this with `run_` in front (see the module docs).
const KINDS: [&str; 3] = ["ci_passed", "ci_failed", "ci_cancelled"];
/// Signals one task may hold. A branch that is pushed to all day, polled by a
/// watcher that never stops, must not grow the inbox without bound; at the
/// limit the call is refused loudly rather than dropped.
const MAX_PER_TASK: i64 = 2048;
const WORKFLOW_MAX: usize = 200;
const BRANCH_MAX: usize = 1024;
const URL_MAX: usize = 2048;
const CONCLUSION_MAX: usize = 32;

fn control_free(value: &str) -> bool {
    !value.chars().any(char::is_control)
}

/// Records one CI result, or returns the notice an earlier call already made
/// for the same `(task, source, external_id, conclusion)`.
///
/// A re-poll with a new `request_id` finds the earlier signal and records
/// nothing; the first write wins and later differing details are ignored,
/// except a different `kind` for the same key, which is a caller fault and is
/// refused instead of silently keeping the first.
///
/// A new result for the same `(task, source, workflow, branch)` as an earlier
/// one supersedes it: the earlier notice reads `changed` (see
/// `attention::body_sql`) and stops being pending. Different keys coexist.
pub(super) fn record(input: &Input<'_>, now: i64) -> Result<String> {
    input.fields(&[
        "request_id",
        "run_id",
        "task_id",
        "source",
        "external_id",
        "kind",
        "conclusion",
        "workflow",
        "branch",
        "url",
        "head_sha",
    ])?;
    let run_id = input.id("run_id")?;
    let task_id = input.id("task_id")?;
    let source = input.required_text("source", 16)?;
    if !SOURCES.contains(&source.as_str()) {
        return Err(Error::invalid("source must be github or ci_local"));
    }
    let external_id = input.id("external_id")?;
    let kind = input.required_text("kind", 16)?;
    if !KINDS.contains(&kind.as_str()) {
        return Err(Error::invalid(
            "kind must be ci_passed, ci_failed or ci_cancelled",
        ));
    }
    let conclusion = input.required_text("conclusion", CONCLUSION_MAX)?;
    if !conclusion
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
    {
        return Err(Error::invalid(
            "conclusion must be lowercase letters, digits, _ or -",
        ));
    }
    let workflow = input.required_text("workflow", WORKFLOW_MAX)?;
    if !control_free(&workflow) {
        return Err(Error::invalid(
            "workflow must not contain control characters",
        ));
    }
    // Part of the supersession key, so it is required: a missing branch would
    // let two branches' verdicts replace each other.
    let branch = input.required_text("branch", BRANCH_MAX)?;
    if !control_free(&branch) {
        return Err(Error::invalid("branch must not contain control characters"));
    }
    let url = input.text("url", URL_MAX)?;
    if url.as_deref().is_some_and(|u| {
        !u.starts_with("https://") || u.chars().any(|c| c.is_control() || c.is_whitespace())
    }) {
        return Err(Error::invalid(
            "url must be an https URL without whitespace or control characters",
        ));
    }
    let head_sha = input.required_text("head_sha", 64)?;
    if !decisions::lower_hex(&head_sha, &[40, 64]) {
        return Err(Error::invalid(
            "head_sha must be a lowercase 40- or 64-hex commit id",
        ));
    }

    let conn = input.conn;
    let task_revision: i64 = conn
        .query_row(
            "SELECT revision FROM work_items WHERE id=?1 AND deleted=0",
            [&task_id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(Error::missing)?;
    let (run_task, run_revision): (String, i64) = conn
        .query_row(
            "SELECT task_id,revision FROM work_runs WHERE id=?1",
            [&run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(Error::missing)?;
    if run_task != task_id {
        return Err(Error::invalid("the run belongs to a different task"));
    }

    let existing: Option<(String, i64, String)> = conn
        .query_row(
            "SELECT id,source_sequence,json_extract(body,'$.signal.kind') FROM work_attention
             WHERE task_id=?1 AND json_extract(body,'$.signal.source')=?2
               AND json_extract(body,'$.signal.external_id')=?3
               AND json_extract(body,'$.signal.conclusion')=?4",
            params![task_id, source, external_id, conclusion],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((id, sequence, recorded_kind)) = existing {
        if recorded_kind != kind {
            return Err(Error {
                code: "idempotency_conflict",
                message: "this CI result was already recorded with a different kind".into(),
            });
        }
        return response(conn, &id, sequence, false, now);
    }

    let held: i64 = conn.query_row(
        "SELECT count(*) FROM work_attention WHERE task_id=?1 AND json_extract(body,'$.signal') IS NOT NULL",
        [&task_id],
        |r| r.get(0),
    )?;
    if held >= MAX_PER_TASK {
        return Err(Error {
            code: "capacity_reached",
            message: "this task reached its bounded CI signal capacity".into(),
        });
    }

    let signal: String = conn.query_row(
        "SELECT json_object('source',?1,'external_id',?2,'kind',?3,'conclusion',?4,'workflow',?5,'branch',?6,'url',?7,'head_sha',?8)",
        params![source, external_id, kind, conclusion, workflow, branch, url, head_sha],
        |r| r.get(0),
    )?;
    // The event belongs to the attempt the result describes; the attention row
    // below keys its own foreign key on this event's sequence.
    let payload: String = conn.query_row(
        "SELECT json_object('item',json_object('run_id',?1,'task_id',?2,'signal',json(?3)),'affected_item_ids',json('[]'),'affected_item_count',0,'affected_item_ids_truncated',json('false'))",
        params![run_id, task_id, signal],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO work_events(kind,entity_id,revision,at,payload) VALUES('signals.record',?1,?2,?3,?4)",
        params![run_id, run_revision, now, payload],
    )?;
    let sequence = conn.last_insert_rowid();
    // `state` carries the kind so record_event's closed mapping picks the
    // notice; the task revision is read now so a later task edit is visible.
    let metadata: String = conn.query_row(
        "SELECT json_object('id',?1,'task_id',?2,'revision',?3,'source_revision',?4,'state',?5,'signal',json(?6))",
        params![run_id, task_id, run_revision, task_revision, kind, signal],
        |r| r.get(0),
    )?;
    attention::record_event(conn, "signals.record", &metadata, sequence, now)?;
    response(conn, &format!("event-{sequence}"), sequence, true, now)
}

/// The reply, in the shape every other mutation uses: the attention row as the
/// inbox shows it now, plus whether this call made it. Reading the row back is
/// also the check that `record_event` produced one, which it silently does not
/// for a kind it has no mapping for.
fn response(
    conn: &rusqlite::Connection,
    attention_id: &str,
    sequence: i64,
    recorded: bool,
    now: i64,
) -> Result<String> {
    let item = attention::body(conn, attention_id, now)?;
    Ok(conn.query_row(
        "SELECT json_object('ok',json('true'),'sequence',?1,'recorded',json(CASE WHEN ?2 THEN 'true' ELSE 'false' END),'item',json(?3),'affected_item_ids',json('[]'),'affected_item_count',0,'affected_item_ids_truncated',json('false'))",
        params![sequence, recorded, item],
        |r| r.get(0),
    )?)
}
