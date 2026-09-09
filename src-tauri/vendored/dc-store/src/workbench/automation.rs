//! Durable intent to enhance a saved task. Preparing consumes a queue entry and
//! reserves the existing proposal lifecycle in the same transaction. No model
//! executes here, and acceptance never enters the text-save scheduling path.

use super::{
    Affected, Entity, Error, Input, MAX_INTEGER, Result, collect_page, enhancements, page,
    page_response, put_body, record_change,
};
use rusqlite::{Connection, OptionalExtension, params};

pub(super) fn put(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&[
        "id",
        "request_id",
        "expected_revision",
        "enabled",
        "provider",
        "model",
    ])?;
    if id != "profile" || !matches!(input.kind("enabled")?.as_deref(), Some("true" | "false")) {
        return Err(Error::invalid(
            "profile automation settings require an explicit enabled boolean",
        ));
    }
    let enabled = input.boolean("enabled", true)?;
    let prior: String = input.conn.query_row(
        "SELECT body FROM work_automation WHERE id='profile'",
        [],
        |r| r.get(0),
    )?;
    let selection: String = match (
        input.kind("provider")?.as_deref(),
        input.kind("model")?.as_deref(),
    ) {
        (None, None) => prior,
        (Some("null"), Some("null")) => input.raw.to_owned(),
        (Some("text"), Some("text")) => {
            input.required_text("provider", 128)?;
            input.required_text("model", 512)?;
            input.raw.to_owned()
        }
        _ => {
            return Err(Error::invalid(
                "provider and model must be supplied together, or both cleared with null",
            ));
        }
    };
    let body: String = input.conn.query_row("SELECT json_object('id','profile','revision',?1,'enabled',json(CASE WHEN ?2 THEN 'true' ELSE 'false' END),'provider',json_extract(?3,'$.provider'),'model',json_extract(?3,'$.model'),'updated_at',?4)", params![revision,enabled,selection,now], |r|r.get(0))?;
    put_body(input.conn, Entity::Automation, id, revision, &body)?;
    if !enabled {
        input
            .conn
            .execute("DELETE FROM work_enhancement_queue", [])?;
        cancel_automatic(input, now)?;
    }
    Ok(())
}

fn cancel_automatic(input: &Input<'_>, now: i64) -> Result<()> {
    // The shared proposal gate permits at most one active or unexpired pending
    // job. Expired pending records cannot claim; ready proposals remain reviewable.
    let mut statement = input.conn.prepare("SELECT id,revision FROM work_enhancements WHERE automatic=1 AND (state='running' OR (state='pending' AND expires_at>?1)) LIMIT 2")?;
    let jobs = statement
        .query_map([now], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if jobs.len() > 1 {
        return Err(Error {
            code: "store_error",
            message: "multiple active profile enhancements violate the worker invariant".into(),
        });
    }
    for (id, revision) in jobs {
        let raw: String = input.conn.query_row(
            "SELECT json_object('id',?1,'request_id',?2,'expected_revision',?3)",
            params![id, input.id("request_id")?, revision],
            |r| r.get(0),
        )?;
        enhancements::mutate(
            &Input::new(input.conn, &raw)?,
            "enhancements.dismiss",
            &id,
            revision + 1,
            now,
        )?;
        record_change(
            input.conn,
            Entity::Enhancement,
            &id,
            revision + 1,
            "enhancements.dismiss",
            now,
            &Affected::none(),
        )?;
    }
    Ok(())
}

pub(super) fn check_selection(
    input: &Input<'_>,
    provider: &str,
    model: &str,
    expected: Option<i64>,
) -> Result<i64> {
    let body: String = input.conn.query_row(
        "SELECT body FROM work_automation WHERE id='profile'",
        [],
        |r| r.get(0),
    )?;
    let settings = Input {
        conn: input.conn,
        raw: &body,
    };
    let revision = settings.integer("revision", None, MAX_INTEGER)?;
    if expected.is_some_and(|value| value != revision) {
        return Err(Error {
            code: "revision_conflict",
            message: "automation settings changed before execution".into(),
        });
    }
    if !settings.boolean("enabled", false)? {
        return Err(Error {
            code: "automation_disabled",
            message: "automatic enhancements are disabled".into(),
        });
    }
    if settings
        .text("provider", 128)?
        .is_some_and(|value| value != provider)
        || settings
            .text("model", 512)?
            .is_some_and(|value| value != model)
    {
        return Err(Error::invalid(
            "the selected provider and model do not match the explicit automation settings",
        ));
    }
    Ok(revision)
}

pub(super) fn after_save(
    input: &Input<'_>,
    id: &str,
    body: &str,
    text_changed: bool,
    now_ms: i64,
) -> Result<()> {
    let enabled: bool = input.conn.query_row(
        "SELECT json_extract(body,'$.enabled') FROM work_automation WHERE id='profile'",
        [],
        |r| r.get(0),
    )?;
    let task = Input {
        conn: input.conn,
        raw: body,
    };
    let locked = enhancements::fields(&task, "locked_fields", false)?;
    if !enabled || locked.len() == 2 {
        input
            .conn
            .execute("DELETE FROM work_enhancement_queue WHERE task_id=?1", [id])?;
    } else if text_changed {
        let due = now_ms
            .checked_add(1000)
            .filter(|v| *v <= MAX_INTEGER)
            .ok_or_else(|| Error::invalid("clock exceeds the enhancement scheduling range"))?;
        input.conn.execute("INSERT INTO work_enhancement_queue(task_id,not_before_ms,enqueued_request_id) VALUES(?1,?2,?3) ON CONFLICT(task_id) DO UPDATE SET not_before_ms=excluded.not_before_ms,enqueued_request_id=excluded.enqueued_request_id", params![id,due,input.id("request_id")?])?;
    }
    Ok(())
}

pub(super) fn receipt(
    conn: &Connection,
    response: &str,
    id: &str,
    request: &str,
) -> Result<String> {
    Ok(conn.query_row("SELECT json_set(?1,'$.automatic_enhancement_queued',json(CASE WHEN EXISTS(SELECT 1 FROM work_enhancement_queue WHERE task_id=?2 AND enqueued_request_id=?3) THEN 'true' ELSE 'false' END))", params![response,id,request], |r|r.get(0))?)
}

pub(super) fn prepare(
    input: &Input<'_>,
    id: &str,
    revision: i64,
    now: i64,
    now_ms: i64,
) -> Result<()> {
    input.fields(&[
        "id",
        "request_id",
        "expected_revision",
        "task_id",
        "expected_task_revision",
        "expected_settings_revision",
        "provider",
        "model",
    ])?;
    let task_id = input.id("task_id")?;
    let task_revision = input.integer("expected_task_revision", None, MAX_INTEGER)?;
    let settings_revision = input.integer("expected_settings_revision", None, MAX_INTEGER)?;
    let provider = input.required_text("provider", 128)?;
    let model = input.required_text("model", 512)?;
    check_selection(input, &provider, &model, Some(settings_revision))?;
    let (source, due): (String, i64) = input.conn.query_row("SELECT i.body,q.not_before_ms FROM work_enhancement_queue q JOIN work_items i ON i.id=q.task_id WHERE q.task_id=?1 AND i.deleted=0", [&task_id], |r|Ok((r.get(0)?,r.get(1)?))).optional()?.ok_or_else(Error::missing)?;
    let task = Input {
        conn: input.conn,
        raw: &source,
    };
    if task.integer("revision", None, MAX_INTEGER)? != task_revision {
        return Err(Error {
            code: "revision_conflict",
            message: "the queued task changed before preparation".into(),
        });
    }
    if now_ms < due {
        return Err(Error {
            code: "not_ready",
            message: "the task is still within its enhancement debounce".into(),
        });
    }
    let locked = enhancements::fields(&task, "locked_fields", false)?;
    if locked.len() == 2 {
        return Err(Error {
            code: "field_locked",
            message: "all enhancement fields are locked".into(),
        });
    }
    let create: String = input.conn.query_row("SELECT json_object('id',?1,'request_id',?2,'expected_revision',?3,'task_id',?4,'source_revision',?5,'fields',json((SELECT json_group_array(value) FROM json_each('[\"title\",\"description\"]') f WHERE NOT EXISTS(SELECT 1 FROM json_each(?6,'$.locked_fields') l WHERE l.value=f.value))),'provider',?7,'model',?8,'automatic',json('true'))", params![id,input.id("request_id")?,revision-1,task_id,task_revision,source,provider,model], |r|r.get(0))?;
    // Reuse the manual proposal's snapshot, source checks, active slot and quota.
    // The outer mutation owns the receipt/event and rolls both writes back.
    enhancements::mutate(
        &Input::new(input.conn, &create)?,
        "enhancements.create",
        id,
        revision,
        now,
    )?;
    input.conn.execute(
        "DELETE FROM work_enhancement_queue WHERE task_id=?1",
        [&task_id],
    )?;
    Ok(())
}

pub(super) fn list(input: &Input<'_>) -> Result<String> {
    input.fields(&["limit", "cursor", "task_id"])?;
    let p = page(input)?;
    let task = input.optional_id("task_id")?;
    let total = input.conn.query_row("SELECT count(*) FROM work_enhancement_queue q JOIN work_items i ON i.id=q.task_id WHERE i.deleted=0 AND (?1 IS NULL OR q.task_id=?1)", [&task], |r|r.get(0))?;
    // Revision comes from the task in this read transaction. Moves and workspace
    // deletion can update it without resetting debounce or invalidating the job.
    let mut statement = input.conn.prepare("SELECT json_object('id',q.task_id,'task_id',q.task_id,'revision',i.revision,'not_before_ms',q.not_before_ms),q.not_before_ms,q.task_id FROM work_enhancement_queue q JOIN work_items i ON i.id=q.task_id WHERE i.deleted=0 AND (?1 IS NULL OR q.task_id=?1) AND (q.not_before_ms,q.task_id)>(?2,?3) ORDER BY q.not_before_ms,q.task_id LIMIT ?4")?;
    let rows = collect_page(
        &mut statement.query(params![task, p.position, p.id, p.limit + 1])?,
        p.limit,
    )?;
    page_response(input.conn, rows, total)
}
