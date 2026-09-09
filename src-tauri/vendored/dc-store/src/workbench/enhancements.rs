//! Durable advisory changes. Only explicit acceptance can change a task.
//! Provider execution lives above this store; pending work has a bounded lease.

use super::{
    Affected, Entity, Error, Input, MAX_INTEGER, Result, automation, collect_page, page,
    page_response, record_change,
};
use rusqlite::{OptionalExtension, params};

pub(super) fn fields(input: &Input<'_>, key: &str, required: bool) -> Result<Vec<String>> {
    let fields = input.strings(key, 2, 32, required)?;
    if (required && fields.is_empty())
        || fields
            .iter()
            .any(|f| !matches!(f.as_str(), "title" | "description"))
        || (fields.len() == 2 && fields[0] == fields[1])
    {
        return Err(Error::invalid(
            "fields must name distinct title or description fields",
        ));
    }
    Ok(fields)
}

fn refusal(code: &'static str, message: &str) -> Error {
    Error {
        code,
        message: message.into(),
    }
}

fn task_body(input: &Input<'_>, id: &str) -> Result<String> {
    input
        .conn
        .query_row(
            "SELECT body FROM work_items WHERE id=?1 AND deleted=0",
            [id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(Error::missing)
}

fn unlocked(task: &Input<'_>, requested: &[String]) -> Result<()> {
    let locked = fields(task, "locked_fields", false)?;
    if requested.iter().any(|field| locked.contains(field)) {
        return Err(refusal("field_locked", "a selected task field is locked"));
    }
    Ok(())
}

pub(super) fn mutate(
    input: &Input<'_>,
    method: &str,
    id: &str,
    revision: i64,
    now: i64,
) -> Result<()> {
    if method == "enhancements.create" {
        return create(input, id, revision, now);
    }
    let body: String = input
        .conn
        .query_row(
            "SELECT body FROM work_enhancements WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(Error::missing)?;
    let prior = Input {
        conn: input.conn,
        raw: &body,
    };
    let state = prior.required_text("state", 32)?;
    let updated = match method {
        "enhancements.claim" if state == "pending" => {
            input.fields(&["id", "request_id", "expected_revision", "worker_id"])?;
            if now >= prior.integer("expires_at", None, MAX_INTEGER)? {
                return Err(refusal(
                    "expired",
                    "the pending enhancement expired before execution",
                ));
            }
            if prior.boolean("automatic", false)? {
                automation::check_selection(
                    input,
                    &prior.required_text("provider", 128)?,
                    &prior.required_text("model", 512)?,
                    Some(prior.integer("automation_settings_revision", Some(0), MAX_INTEGER)?),
                )?;
            }
            let worker = input.id("worker_id")?;
            let current_body = task_body(input, &prior.id("task_id")?)?;
            let current = Input {
                conn: input.conn,
                raw: &current_body,
            };
            unlocked(&current, &fields(&prior, "fields", true)?)?;
            if current.integer("revision", None, MAX_INTEGER)?
                != prior.integer("source_revision", None, MAX_INTEGER)?
            {
                return Err(refusal(
                    "revision_conflict",
                    "the source task changed before generation began",
                ));
            }
            input.conn.query_row("SELECT json_set(?1,'$.state','running','$.worker_id',?2,'$.started_at',?3,'$.expires_at',?4)", params![body,worker,now,now.saturating_add(120)], |r|r.get(0))?
        }
        "enhancements.complete"
            if matches!(state.as_str(), "pending" | "running" | "cancel_requested") =>
        {
            complete(input, &prior, now)?
        }
        "enhancements.dismiss" if state == "running" => {
            input.fields(&["id", "request_id", "expected_revision"])?;
            input.conn.query_row(
                "SELECT json_set(?1,'$.state','cancel_requested')",
                [&body],
                |r| r.get(0),
            )?
        }
        "enhancements.recover" if matches!(state.as_str(), "running" | "cancel_requested") => {
            input.fields(&[
                "id",
                "request_id",
                "expected_revision",
                "worker_id",
                "acknowledge_uncertain",
            ])?;
            if !input.boolean("acknowledge_uncertain", false)? {
                return Err(Error::invalid(
                    "recovery requires explicit acknowledgment of an uncertain provider outcome",
                ));
            }
            check_worker(input, &prior)?;
            if now < prior.integer("expires_at", None, MAX_INTEGER)? {
                return Err(refusal(
                    "busy",
                    "request cancellation while the generation deadline is still active",
                ));
            }
            input.conn.query_row("SELECT json_set(?1,'$.state','interrupted','$.outcome_uncertain',json('true'),'$.failure','The previous provider outcome is unknown; a new attempt may incur another model call.')", [&body], |r|r.get(0))?
        }
        "enhancements.dismiss" if matches!(state.as_str(), "pending" | "ready" | "failed") => {
            input.fields(&["id", "request_id", "expected_revision"])?;
            input
                .conn
                .query_row("SELECT json_set(?1,'$.state','dismissed')", [&body], |r| {
                    r.get(0)
                })?
        }
        "enhancements.accept" if state == "ready" => apply(input, &prior, now, false)?,
        "enhancements.revise" if state == "ready" => revise(input, &prior)?,
        "enhancements.undo" if state == "accepted" => apply(input, &prior, now, true)?,
        _ => {
            return Err(refusal(
                "invalid_transition",
                "this enhancement state does not permit the requested action",
            ));
        }
    };
    input.conn.execute("UPDATE work_enhancements SET revision=?2,body=json_set(?3,'$.revision',?2,'$.updated_at',?4) WHERE id=?1", params![id,revision,updated,now])?;
    Ok(())
}

fn revise(input: &Input<'_>, prior: &Input<'_>) -> Result<String> {
    input.fields(&[
        "id",
        "request_id",
        "expected_revision",
        "title",
        "description",
    ])?;
    let requested = fields(prior, "fields", true)?;
    let mut changes = Vec::new();
    let original: String = input.conn.query_row(
        "SELECT coalesce(json_extract(?1,'$.original_proposed'),json_extract(?1,'$.proposed'))",
        [prior.raw],
        |r| r.get(0),
    )?;
    let mut body: String = input.conn.query_row(
        "SELECT json_set(?1,'$.original_proposed',json(?2))",
        params![prior.raw, original],
        |r| r.get(0),
    )?;
    for (field, bound) in [("title", 1200), ("description", 65_536)] {
        if input.kind(field)?.is_none() {
            continue;
        }
        let value = input
            .text(field, bound)?
            .ok_or_else(|| Error::invalid("edited fields must be strings"))?;
        if !requested.iter().any(|f| f == field) {
            return Err(Error::invalid(
                "only requested suggestion fields may be edited",
            ));
        }
        if field == "title" && (value.trim().is_empty() || value.chars().count() > 300) {
            return Err(Error::invalid(
                "edited title must be nonblank and at most 300 characters",
            ));
        }
        changes.push(field.to_string());
        body = input.conn.query_row(
            "SELECT json_set(?1,?2,?3)",
            params![body, format!("$.proposed.{field}"), value],
            |r| r.get(0),
        )?;
    }
    if changes.is_empty() {
        return Err(Error::invalid(
            "supply at least one suggestion field to edit",
        ));
    }
    let current = task_body(input, &prior.id("task_id")?)?;
    unlocked(
        &Input {
            conn: input.conn,
            raw: &current,
        },
        &changes,
    )?;
    // Attribution follows actual differences from the original proposal. The
    // revision/event history still records an edit that restores original text.
    Ok(input.conn.query_row(
        "SELECT json_set(?1,'$.edited_fields',json((SELECT json_group_array(p.key) FROM json_each(?1,'$.proposed') p WHERE p.value IS NOT json_extract(?2,'$.'||p.key))))",
        params![body,original], |r| r.get(0),
    )?)
}

fn create(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&[
        "id",
        "request_id",
        "expected_revision",
        "task_id",
        "source_revision",
        "fields",
        "provider",
        "model",
        "automatic",
    ])?;
    if revision != 1 {
        return Err(Error::invalid(
            "enhancement identities cannot be replaced or reused",
        ));
    }
    let task_id = input.id("task_id")?;
    let source_revision = input.integer("source_revision", None, MAX_INTEGER)?;
    let source = task_body(input, &task_id)?;
    let task = Input {
        conn: input.conn,
        raw: &source,
    };
    if task.integer("revision", None, MAX_INTEGER)? != source_revision {
        return Err(refusal(
            "revision_conflict",
            "the task changed before enhancement preparation",
        ));
    }
    let requested = fields(input, "fields", true)?;
    unlocked(&task, &requested)?;
    let provider = input.required_text("provider", 128)?;
    let model = input.required_text("model", 512)?;
    let automatic = input.boolean("automatic", false)?;
    let settings_revision = if automatic {
        Some(automation::check_selection(input, &provider, &model, None)?)
    } else {
        None
    };
    let busy: bool = input.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM work_enhancements WHERE state IN ('running','cancel_requested') OR (state='pending' AND expires_at>?1))",
        [now],
        |r| r.get(0),
    )?;
    if busy {
        return Err(refusal(
            "busy",
            "another profile enhancement is still pending",
        ));
    }
    let count: i64 = input.conn.query_row(
        "SELECT count(*) FROM work_enhancements WHERE automatic=1 AND created_at>?1",
        [now.saturating_sub(3600)],
        |r| r.get(0),
    )?;
    if automatic && count >= 20 {
        return Err(refusal(
            "enhancement_limit",
            "the profile has reached 20 automatic enhancements in the last hour",
        ));
    }
    let body: String = input.conn.query_row("SELECT json_object('id',?1,'revision',1,'task_id',?2,'source_revision',?3,'source',json(?4),'fields',json(json_extract(?5,'$.fields')),'provider',?6,'model',?7,'automatic',json(CASE WHEN ?8 THEN 'true' ELSE 'false' END),'state','pending','created_at',?9,'updated_at',?9,'expires_at',?10)", params![id,task_id,source_revision,source,input.raw,provider,model,automatic,now,now.saturating_add(120)], |r|r.get(0))?;
    let body = if let Some(settings_revision) = settings_revision {
        input.conn.query_row(
            "SELECT json_set(?1,'$.automation_settings_revision',?2)",
            params![body, settings_revision],
            |r| r.get(0),
        )?
    } else {
        body
    };
    input.conn.execute(
        "INSERT INTO work_enhancements(id,revision,task_id,body) VALUES(?1,1,?2,?3)",
        params![id, task_id, body],
    )?;
    if !automatic {
        // An explicit request supersedes the queued suggestion for this exact
        // current source. Receipt replay bypasses mutation, so it cannot consume
        // a later save; the surrounding transaction also owns the event write.
        input.conn.execute(
            "DELETE FROM work_enhancement_queue WHERE task_id=?1",
            [&task_id],
        )?;
    }
    Ok(())
}

fn complete(input: &Input<'_>, prior: &Input<'_>, now: i64) -> Result<String> {
    input.fields(&[
        "id",
        "request_id",
        "expected_revision",
        "title",
        "description",
        "rationale",
        "failure",
        "worker_id",
    ])?;
    let state = prior.required_text("state", 32)?;
    if state != "pending" {
        check_worker(input, prior)?;
    }
    let failure = input.text("failure", 2048)?;
    if state == "cancel_requested" && failure.is_none() {
        return Err(refusal(
            "cancel_requested",
            "the worker must acknowledge cancellation instead of publishing a proposal",
        ));
    }
    if failure.is_none() && now >= prior.integer("expires_at", None, MAX_INTEGER)? {
        return Err(refusal(
            "expired",
            "enhancement execution has expired; create a new attempt",
        ));
    }
    if let Some(failure) = failure {
        if failure.trim().is_empty()
            || input.text("title", 1200)?.is_some()
            || input.text("description", 65_536)?.is_some()
            || input.text("rationale", 2048)?.is_some()
        {
            return Err(Error::invalid(
                "failed enhancements need a reason and cannot include proposed changes",
            ));
        }
        return Ok(input.conn.query_row(
            "SELECT json_set(?1,'$.state',?3,'$.failure',?2)",
            params![
                prior.raw,
                failure,
                if state == "cancel_requested" {
                    "cancelled"
                } else {
                    "failed"
                }
            ],
            |r| r.get(0),
        )?);
    }
    let requested = fields(prior, "fields", true)?;
    let mut proposed = "{}".to_string();
    for (field, bound) in [("title", 1200), ("description", 65_536)] {
        let value = input.text(field, bound)?;
        if requested.iter().any(|f| f == field) {
            let value = value.ok_or_else(|| Error::invalid(format!("missing proposed {field}")))?;
            if field == "title" && (value.trim().is_empty() || value.chars().count() > 300) {
                return Err(Error::invalid(
                    "proposed title must be nonblank and at most 300 characters",
                ));
            }
            proposed = input.conn.query_row(
                "SELECT json_set(?1,?2,?3)",
                params![proposed, format!("$.{field}"), value],
                |r| r.get(0),
            )?;
        } else if value.is_some() {
            return Err(Error::invalid(
                "a proposal cannot change a field that was not requested",
            ));
        }
    }
    let rationale = input.text("rationale", 2048)?.unwrap_or_default();
    Ok(input.conn.query_row(
        "SELECT json_set(?1,'$.state','ready','$.proposed',json(?2),'$.rationale',?3)",
        params![prior.raw, proposed, rationale],
        |r| r.get(0),
    )?)
}

fn check_worker(input: &Input<'_>, prior: &Input<'_>) -> Result<()> {
    if input.id("worker_id")? != prior.id("worker_id")? {
        return Err(refusal(
            "worker_mismatch",
            "the result belongs to another generation attempt",
        ));
    }
    Ok(())
}

fn apply(input: &Input<'_>, prior: &Input<'_>, now: i64, undo: bool) -> Result<String> {
    if undo {
        input.fields(&[
            "id",
            "request_id",
            "expected_revision",
            "expected_task_revision",
        ])?;
    } else {
        input.fields(&[
            "id",
            "request_id",
            "expected_revision",
            "expected_task_revision",
            "fields",
        ])?;
    }
    let requested = fields(prior, "fields", true)?;
    let selected = if undo {
        fields(prior, "accepted_fields", true)?
    } else {
        fields(input, "fields", true)?
    };
    if selected.iter().any(|field| !requested.contains(field)) {
        return Err(Error::invalid("acceptance may only select proposed fields"));
    }
    let task_id = prior.id("task_id")?;
    let current_body = task_body(input, &task_id)?;
    let current = Input {
        conn: input.conn,
        raw: &current_body,
    };
    let task_revision = current.integer("revision", None, MAX_INTEGER)?;
    if input.integer("expected_task_revision", None, MAX_INTEGER)? != task_revision {
        return Err(refusal(
            "revision_conflict",
            "the task changed before enhancement acceptance or undo",
        ));
    }
    unlocked(&current, &selected)?;
    let mut task_body = current_body.clone();
    for field in selected {
        let base: String = input.conn.query_row(
            "SELECT json_extract(?1,?2)",
            params![prior.raw, format!("$.source.{field}")],
            |r| r.get(0),
        )?;
        let proposed: String = input.conn.query_row(
            "SELECT json_extract(?1,?2)",
            params![prior.raw, format!("$.proposed.{field}")],
            |r| r.get(0),
        )?;
        let expected = if undo { &proposed } else { &base };
        if current.text(&field, 65_536)?.as_ref() != Some(expected) {
            return Err(refusal(
                "field_conflict",
                "a selected field has changed; preserve the newer edit and request a fresh proposal",
            ));
        }
        let replacement = if undo { &base } else { &proposed };
        task_body = input.conn.query_row(
            "SELECT json_set(?1,?2,?3)",
            params![task_body, format!("$.{field}"), replacement],
            |r| r.get(0),
        )?;
    }
    let next = task_revision + 1;
    input.conn.execute("UPDATE work_items SET revision=?2,body=json_set(?3,'$.revision',?2,'$.updated_at',?4) WHERE id=?1", params![task_id,next,task_body,now])?;
    record_change(
        input.conn,
        Entity::Item,
        &task_id,
        next,
        if undo {
            "items.enhancement_undone"
        } else {
            "items.enhancement_accepted"
        },
        now,
        &Affected::none(),
    )?;
    if undo {
        Ok(input.conn.query_row(
            "SELECT json_set(?1,'$.state','undone','$.undone_task_revision',?2)",
            params![prior.raw, next],
            |r| r.get(0),
        )?)
    } else {
        Ok(input.conn.query_row("SELECT json_set(?1,'$.state','accepted','$.accepted_fields',json(json_extract(?2,'$.fields')),'$.applied_task_revision',?3)", params![prior.raw,input.raw,next], |r|r.get(0))?)
    }
}

pub(super) fn list(input: &Input<'_>) -> Result<String> {
    input.fields(&[
        "task_id",
        "limit",
        "cursor",
        "states",
        "automatic",
        "newest",
    ])?;
    let task = input.optional_id("task_id")?;
    super::require_record(input, Entity::Item, task.as_deref(), true)?;
    let newest = input.boolean("newest", false)?;
    let mut p = page(input)?;
    if newest && input.text("cursor", 180)?.is_none() {
        p.position = MAX_INTEGER;
    }
    let automatic = match input.kind("automatic")?.as_deref() {
        None => None,
        Some("true" | "false") => Some(input.boolean("automatic", false)?),
        _ => return Err(Error::invalid("automatic must be a boolean when supplied")),
    };
    if input.kind("states")?.is_some() {
        let states = input.strings("states", 10, 32, true)?;
        if states.is_empty()
            || states
                .iter()
                .enumerate()
                .any(|(index, state)| states[..index].contains(state))
            || states.iter().any(|state| {
                ![
                    "pending",
                    "running",
                    "cancel_requested",
                    "ready",
                    "failed",
                    "cancelled",
                    "interrupted",
                    "dismissed",
                    "accepted",
                    "undone",
                ]
                .contains(&state.as_str())
            })
        {
            return Err(Error::invalid(
                "states must be a nonempty list of enhancement states",
            ));
        }
    }
    let states: Option<String> =
        input
            .conn
            .query_row("SELECT json_extract(?1,'$.states')", [input.raw], |r| {
                r.get(0)
            })?;
    let total = input.conn.query_row(
        "SELECT count(*) FROM work_enhancements WHERE (?1 IS NULL OR task_id=?1) AND (?2 IS NULL OR automatic=?2) AND (?3 IS NULL OR state IN (SELECT value FROM json_each(?3)))",
        params![task,automatic,states],
        |r| r.get(0),
    )?;
    let (compare, direction) = if newest { ("<", "DESC") } else { (">", "ASC") };
    let mut statement = input.conn.prepare(&format!("SELECT json_remove(body,'$.source','$.proposed','$.original_proposed','$.rationale'),created_at,id FROM work_enhancements WHERE (?1 IS NULL OR task_id=?1) AND (?2 IS NULL OR automatic=?2) AND (?3 IS NULL OR state IN (SELECT value FROM json_each(?3))) AND (created_at,id){compare}(?4,?5) ORDER BY created_at {direction},id {direction} LIMIT ?6"))?;
    let rows = collect_page(
        &mut statement.query(params![
            task,
            automatic,
            states,
            p.position,
            p.id,
            p.limit + 1
        ])?,
        p.limit,
    )?;
    page_response(input.conn, rows, total)
}
