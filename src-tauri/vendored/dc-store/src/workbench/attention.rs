//! One transactional inbox for control events. No OS API or model runs here.
//! A notice is neither task acceptance nor a provider approval. Public methods
//! can only read, dismiss, restore or snooze it; source identities are immutable.
use super::{Error, Input, MAX_INTEGER, Result, collect_page, page, page_response};
use rusqlite::{Connection, OptionalExtension, params};

// Resolve the target each time the inbox is read. A stale banner remains useful
// history, but cannot be mistaken for the current proposal or run revision.
pub(super) const FROM: &str = "work_attention a JOIN work_items t ON t.id=a.task_id
    LEFT JOIN work_enhancements e ON json_extract(a.body,'$.target_type')='enhancement' AND e.id=json_extract(a.body,'$.target_id')
    LEFT JOIN work_runs r ON json_extract(a.body,'$.target_type')='run' AND r.id=json_extract(a.body,'$.target_id')
    LEFT JOIN work_decisions q ON json_extract(a.body,'$.target_type')='decision' AND q.id=json_extract(a.body,'$.target_id')";
pub(super) fn body_sql(clock_parameter: u8) -> String {
    let decision_current = super::decisions::fresh_notice(clock_parameter);
    format!("json_set(a.body,'$.target_status',CASE
    WHEN t.deleted=1 THEN 'task_deleted'
    WHEN coalesce(e.revision,r.revision,q.revision) IS NULL THEN 'unavailable'
    WHEN coalesce(e.revision,r.revision,q.revision)<>json_extract(a.body,'$.target_revision') THEN 'changed'
    WHEN q.id IS NOT NULL AND NOT ({decision_current}) THEN 'changed'
    WHEN t.revision<>json_extract(a.body,'$.task_revision') THEN 'task_changed'
    ELSE 'current' END)")
}

pub(super) fn record_event(
    conn: &Connection,
    method: &str,
    source: &str,
    sequence: i64,
    now: i64,
) -> Result<()> {
    if !matches!(
        method,
        "runs.finish" | "enhancements.complete" | "enhancements.recover" | "decisions.create"
    ) {
        return Ok(());
    }
    // A saved result can contain both a large source task and a large proposal.
    // The inbox only reads control metadata, never revalidates that full body
    // against the much smaller command-input limit or copies it into a preview.
    let metadata: String = conn.query_row("SELECT json_object('id',json_extract(?1,'$.id'),'task_id',json_extract(?1,'$.task_id'),'revision',json_extract(?1,'$.revision'),'source_revision',json_extract(?1,'$.source_revision'),'state',json_extract(?1,'$.state'),'kind',json_extract(?1,'$.kind'),'exit_code',json_extract(?1,'$.exit_code'),'provider_state',json_extract(?1,'$.provider_state'))", [source], |r|r.get(0))?;
    let input = Input::new(conn, &metadata)?;
    let state = input.required_text("state", 32)?;
    let exit_failed = input.integer("exit_code", Some(0), MAX_INTEGER)? != 0
        || input.text("provider_state", 32)?.as_deref() == Some("failed");
    let (target_type, kind, title) = match (method, state.as_str()) {
        ("decisions.create", "pending")
            if input.text("kind", 20)?.as_deref() == Some("question") =>
        {
            ("decision", "decision_question", "Coding agent needs input")
        }
        ("decisions.create", "pending") => (
            "decision",
            "decision_permission",
            "Coding agent needs permission",
        ),
        ("runs.finish", "exited") if exit_failed => (
            "run",
            "run_exit_failed",
            "Coding agent exited with an error",
        ),
        ("runs.finish", "exited") => ("run", "run_exited", "Coding agent exited"),
        ("runs.finish", "failed") => ("run", "run_failed", "Coding agent failed to start"),
        ("runs.finish", "unresolved") => (
            "run",
            "run_unresolved",
            "Coding agent outcome needs attention",
        ),
        ("enhancements.complete", "ready") => (
            "enhancement",
            "enhancement_ready",
            "Task enhancement ready to review",
        ),
        ("enhancements.complete", "failed") => (
            "enhancement",
            "enhancement_failed",
            "Task enhancement failed",
        ),
        ("enhancements.recover", "interrupted") => (
            "enhancement",
            "enhancement_interrupted",
            "Task enhancement outcome is uncertain",
        ),
        _ => return Ok(()),
    };
    let id = format!("event-{sequence}");
    let task = input.id("task_id")?;
    let target = input.id("id")?;
    let target_revision = input.integer("revision", None, MAX_INTEGER)?;
    let task_revision = input.integer("source_revision", None, MAX_INTEGER)?;
    let body:String=conn.query_row("SELECT json_object('id',?1,'revision',1,'source_sequence',?2,'task_id',?3,'task_revision',?4,'target_type',?5,'target_id',?6,'target_revision',?7,'kind',?8,'title',?9,'created_at',?10,'updated_at',?10,'read_at',NULL,'dismissed_at',NULL,'snoozed_until',NULL)",params![id,sequence,task,task_revision,target_type,target,target_revision,kind,title,now],|r|r.get(0))?;
    conn.execute("INSERT INTO work_attention(id,revision,source_sequence,task_id,body) VALUES(?1,1,?2,?3,?4)",params![id,sequence,task,body])?;
    conn.execute("INSERT INTO work_revisions(entity_type,entity_id,revision,body) VALUES('attention',?1,1,?2)",params![id,body])?;
    Ok(())
}

pub(super) fn body(conn: &Connection, id: &str, now: i64) -> Result<String> {
    conn.query_row(
        &format!("SELECT {} FROM {FROM} WHERE a.id=?1", body_sql(2)),
        params![id, now],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(Error::missing)
}
pub(super) fn get(input: &Input<'_>, now: i64) -> Result<String> {
    input.fields(&["id"])?;
    let body = body(input.conn, &input.id("id")?, now)?;
    Ok(input.conn.query_row(
        "SELECT json_object('ok',json('true'),'item',json(?1))",
        [body],
        |r| r.get(0),
    )?)
}
pub(super) fn update(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&["request_id", "id", "expected_revision", "action", "seconds"])?;
    if revision == 1 {
        return Err(Error::missing());
    }
    let action = input.required_text("action", 20)?;
    if action != "snooze" && input.kind("seconds")?.is_some() {
        return Err(Error::invalid("seconds is only valid for snooze"));
    }
    let (field, value) = match action.as_str() {
        "read" => ("$.read_at", Some(now)),
        "unread" => ("$.read_at", None),
        "dismiss" => ("$.dismissed_at", Some(now)),
        "restore" => ("$.dismissed_at", None),
        "unsnooze" => ("$.snoozed_until", None),
        "snooze" => {
            let seconds = input.integer("seconds", None, 604_800)?;
            if seconds == 0 {
                return Err(Error::invalid("snooze must be at least one second"));
            }
            (
                "$.snoozed_until",
                Some(
                    now.checked_add(seconds)
                        .filter(|n| *n <= MAX_INTEGER)
                        .ok_or_else(|| Error::invalid("snooze exceeds timestamp range"))?,
                ),
            )
        }
        _ => return Err(Error::invalid("unsupported attention action")),
    };
    input.conn.execute("UPDATE work_attention SET revision=?2,body=json_set(body,'$.revision',?2,'$.updated_at',?3,?4,?5) WHERE id=?1",params![id,revision,now,field,value])?;
    Ok(())
}

pub(super) fn list(input: &Input<'_>, now: i64) -> Result<String> {
    input.fields(&[
        "limit",
        "cursor",
        "filter",
        "task_id",
        "workspace_id",
        "repository_id",
    ])?;
    let p = page(input)?;
    let task = input.optional_id("task_id")?;
    let workspace = input.optional_id("workspace_id")?;
    let repository = input.optional_id("repository_id")?;
    if [task.is_some(), workspace.is_some(), repository.is_some()]
        .into_iter()
        .filter(|b| *b)
        .count()
        > 1
    {
        return Err(Error::invalid("select one attention scope"));
    }
    let filter = input.text("filter", 20)?.unwrap_or_else(|| "active".into());
    if !matches!(filter.as_str(), "active" | "all" | "unread") {
        return Err(Error::invalid("invalid attention filter"));
    }
    let scope = if let Some(task) = task {
        ("a.task_id=?1".to_owned(), Some(task))
    } else if let Some(repository) = repository {
        (
            "a.task_id IN (SELECT item_id FROM work_item_repositories WHERE repository_id=?1)"
                .to_owned(),
            Some(repository),
        )
    } else if let Some(workspace) = workspace {
        ("a.task_id IN (SELECT id FROM work_items WHERE home_workspace_id=?1 UNION SELECT ir.item_id FROM work_item_repositories ir JOIN work_workspace_repositories wr ON wr.repository_id=ir.repository_id WHERE wr.workspace_id=?1)".to_owned(),Some(workspace))
    } else {
        ("?1 IS NULL".to_owned(), None)
    };
    let visibility = match filter.as_str() {
        "all" => "1",
        "unread" => {
            "a.read_at IS NULL AND a.dismissed_at IS NULL AND (a.snoozed_until IS NULL OR a.snoozed_until<=?2)"
        }
        _ => "a.dismissed_at IS NULL AND (a.snoozed_until IS NULL OR a.snoozed_until<=?2)",
    };
    // Bind time even in all-history mode without adding it to caller input.
    let predicate = format!("{} AND {visibility} AND ?2>=0", scope.0);
    let total = input.conn.query_row(
        &format!("SELECT count(*) FROM work_attention a WHERE {predicate}"),
        params![scope.1, now],
        |r| r.get(0),
    )?;
    let position = if p.id.is_empty() {
        MAX_INTEGER
    } else {
        p.position
    };
    let mut statement=input.conn.prepare(&format!("SELECT {},a.source_sequence,a.id FROM {FROM} WHERE {predicate} AND (a.source_sequence,a.id)<(?3,?4) ORDER BY a.source_sequence DESC,a.id DESC LIMIT ?5",body_sql(2)))?;
    let rows = collect_page(
        &mut statement.query(params![scope.1, now, position, p.id, p.limit + 1])?,
        p.limit,
    )?;
    page_response(input.conn, rows, total)
}
