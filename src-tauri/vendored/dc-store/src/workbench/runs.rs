//! Durable launch intent and process receipts. These records never complete or
//! accept a task. The native host owns filesystem checks, process identity and
//! termination evidence; storage alone cannot prove a process exists or stopped.

use super::{
    Entity, Error, Input, MAX_INTEGER, MAX_PAGE_DOCUMENTS, Result, briefs, collect_page, page,
    page_response, put_body,
};
use rusqlite::{OptionalExtension, params, params_from_iter, types::Value};

const MAX_ACTIVE_RUNS: i64 = 2;
const ACTIVE: &str =
    "(state IN ('starting','running','unresolved') OR (state='prepared' AND expires_at>?1))";
const STATES: &[&str] = &[
    "prepared",
    "starting",
    "running",
    "exited",
    "failed",
    "cancelled",
    "unresolved",
];

fn refuse(code: &'static str, message: &str) -> Error {
    Error {
        code,
        message: message.into(),
    }
}

pub(super) fn mutate(
    input: &Input<'_>,
    method: &str,
    id: &str,
    revision: i64,
    now: i64,
) -> Result<()> {
    if method == "runs.prepare" {
        return prepare(input, id, revision, now);
    }
    let body: String = input
        .conn
        .query_row("SELECT body FROM work_runs WHERE id=?1", [id], |r| r.get(0))
        .optional()?
        .ok_or_else(Error::missing)?;
    let prior = Input {
        conn: input.conn,
        raw: &body,
    };
    let state = prior.required_text("state", 32)?;
    let next: String = match method {
        "runs.claim" if state == "prepared" => {
            input.fields(&[
                "id",
                "request_id",
                "expected_revision",
                "owner_id",
                "session_id",
                "kind",
            ])?;
            let kind = input
                .text("kind", 32)?
                .unwrap_or_else(|| "external_terminal".into());
            if kind != prior.required_text("kind", 32)? {
                return Err(refuse(
                    "run_kind_mismatch",
                    "this attempt belongs to a different execution adapter",
                ));
            }
            if now >= prior.integer("expires_at", None, MAX_INTEGER)? {
                return Err(refuse(
                    "expired",
                    "the launch preparation expired; prepare a new attempt",
                ));
            }
            let owner = input.id("owner_id")?;
            let session = input.id("session_id")?;
            let original: String = input.conn.query_row(
                "SELECT brief FROM work_run_inputs WHERE run_id=?1",
                [id],
                |r| r.get(0),
            )?;
            let current = snapshot(
                input,
                &prior.id("task_id")?,
                prior.integer("source_revision", None, MAX_INTEGER)?,
            )?;
            if current != original {
                return Err(refuse(
                    "revision_conflict",
                    "the task, repository or workspace snapshot changed before launch",
                ));
            }
            input.conn.query_row("SELECT json_set(?1,'$.state','starting','$.owner_id',?2,'$.session_id',?3,'$.claimed_at',?4)",params![body,owner,session,now],|r|r.get(0))?
        }
        "runs.started" if state == "starting" => {
            input.fields(&[
                "id",
                "request_id",
                "expected_revision",
                "owner_id",
                "session_id",
                "process_id",
                "process_start",
            ])?;
            owns(input, &prior)?;
            let pid = input.integer("process_id", None, u32::MAX.into())?;
            if pid == 0 {
                return Err(Error::invalid("process_id must be positive"));
            }
            let birth = input.required_text("process_start", 256)?;
            input.conn.query_row("SELECT json_set(?1,'$.state','running','$.process_id',?2,'$.process_start',?3,'$.started_at',?4)",params![body,pid,birth,now],|r|r.get(0))?
        }
        "runs.cancel" if state == "prepared" => {
            input.fields(&["id", "request_id", "expected_revision"])?;
            input.conn.query_row("SELECT json_set(?1,'$.state','cancelled','$.finished_at',?2,'$.reason','Cancelled before a launch claim')",params![body,now],|r|r.get(0))?
        }
        "runs.finish" if matches!(state.as_str(), "starting" | "running" | "unresolved") => {
            finish(input, &prior, &state, now)?
        }
        "runs.protocol" => protocol(input, &prior, &state)?,
        _ => {
            return Err(refuse(
                "invalid_state",
                "the requested transition is not valid for this run state",
            ));
        }
    };
    let updated: String = input.conn.query_row(
        "SELECT json_set(?1,'$.revision',?2,'$.updated_at',?3)",
        params![next, revision, now],
        |r| r.get(0),
    )?;
    put_body(input.conn, Entity::Run, id, revision, &updated)
}

fn protocol(input: &Input<'_>, prior: &Input<'_>, state: &str) -> Result<String> {
    input.fields(&[
        "id",
        "request_id",
        "expected_revision",
        "owner_id",
        "session_id",
        "provider_thread_id",
        "provider_turn_id",
        "provider_state",
        "effective_configuration",
        "output",
        "output_truncated",
    ])?;
    owns(input, prior)?;
    if prior.required_text("kind", 32)? != "managed"
        || !matches!(state, "starting" | "running" | "unresolved")
    {
        return Err(refuse(
            "invalid_state",
            "protocol evidence requires an owned managed attempt",
        ));
    }
    let thread = input.required_text("provider_thread_id", 256)?;
    let turn = input.text("provider_turn_id", 256)?;
    let status = input.required_text("provider_state", 32)?;
    let configuration = input.required_text("effective_configuration", 65536)?;
    let output = input.text("output", 131072)?.unwrap_or_default();
    let truncated = input.boolean("output_truncated", false)?;
    let config = Input::new(input.conn, &configuration)?;
    if config.kind("cwd")?.as_deref() != Some("text")
        || config.required_text("cwd", 4096)? != prior.required_text("cwd", 4096)?
        || config.kind("sandbox")?.as_deref() != Some("object")
    {
        return Err(Error::invalid(
            "effective configuration must identify the selected cwd and sandbox",
        ));
    }
    let old_status = prior.text("provider_state", 32)?;
    let valid = match old_status.as_deref() {
        None => status == "ready" && state == "running" && turn.is_none(),
        Some("ready") => matches!(
            status.as_str(),
            "running" | "failed" | "interrupted" | "unresolved"
        ),
        Some("running") => matches!(
            status.as_str(),
            "running" | "completed" | "failed" | "interrupted" | "unresolved"
        ),
        Some("unresolved") => status == "unresolved",
        _ => false,
    };
    if !valid
        || (matches!(status.as_str(), "running" | "completed")
            && (state != "running" || turn.as_ref().is_none_or(|s| s.trim().is_empty())))
    {
        return Err(refuse(
            "invalid_state",
            "provider progress must follow its managed run lifecycle",
        ));
    }
    for (key, value) in [
        ("provider_thread_id", Some(thread.as_str())),
        ("provider_turn_id", turn.as_deref()),
        ("effective_configuration", Some(configuration.as_str())),
    ] {
        if let Some(original) = prior.text(key, 65536)?
            && Some(original.as_str()) != value
        {
            return Err(refuse(
                "revision_conflict",
                "the provider identity and effective configuration are immutable for this attempt",
            ));
        }
    }
    input.conn.query_row("SELECT json_set(?1,'$.provider_thread_id',?2,'$.provider_turn_id',?3,'$.provider_state',?4,'$.effective_configuration',?5,'$.output',?6,'$.output_truncated',json(CASE WHEN ?7 THEN 'true' ELSE 'false' END))",params![prior.raw,thread,turn,status,configuration,output,truncated],|r|r.get(0)).map_err(Into::into)
}

fn owns(input: &Input<'_>, prior: &Input<'_>) -> Result<()> {
    if input.id("owner_id")? != prior.id("owner_id")?
        || input.id("session_id")? != prior.id("session_id")?
    {
        return Err(refuse(
            "owner_mismatch",
            "the lifecycle report does not match the launch owner and session",
        ));
    }
    Ok(())
}

fn finish(input: &Input<'_>, prior: &Input<'_>, state: &str, now: i64) -> Result<String> {
    input.fields(&[
        "id",
        "request_id",
        "expected_revision",
        "owner_id",
        "session_id",
        "outcome",
        "exit_code",
        "reason",
        "provider_state",
    ])?;
    owns(input, prior)?;
    let outcome = input.required_text("outcome", 32)?;
    let reason = input.required_text("reason", 2048)?;
    let provider = input.text("provider_state", 32)?;
    if let Some(provider) = &provider {
        // Initialization can fail before a verified thread exists. Only the
        // owning managed host may record that failure; finish cannot fabricate
        // success or overwrite an initialized provider's protocol receipt.
        if prior.required_text("kind", 32)? != "managed"
            || !matches!(provider.as_str(), "failed" | "interrupted" | "unresolved")
            || prior.text("provider_state", 32)?.is_some()
            || prior.text("provider_thread_id", 256)?.is_some()
            || (outcome == "failed" && provider != "failed")
        {
            return Err(refuse(
                "invalid_state",
                "finish may only record an uninitialized managed provider failure",
            ));
        }
    }
    let exit = match input.kind("exit_code")?.as_deref() {
        Some("integer") => Some(input.integer("exit_code", None, u32::MAX.into())?),
        None | Some("null") => None,
        _ => return Err(Error::invalid("exit_code must be an integer or null")),
    };
    // An unresolved launch keeps its slot. Only the recorded host/session may
    // later provide a terminal outcome; expiry never proves writer termination.
    let uncertain = outcome == "unresolved";
    let valid = match outcome.as_str() {
        "exited" => {
            state == "running"
                || (state == "unresolved"
                    && prior.kind("process_id")?.as_deref() == Some("integer"))
        }
        "failed" => state == "starting",
        "unresolved" => state != "unresolved",
        _ => false,
    };
    if !valid || (outcome != "exited" && exit.is_some()) {
        return Err(refuse(
            "invalid_state",
            "the outcome is inconsistent with the recorded process lifecycle",
        ));
    }
    let updated: String = input.conn.query_row("SELECT json_set(?1,'$.state',?2,'$.reason',?3,'$.exit_code',?4,'$.outcome_uncertain',json(CASE WHEN ?5 THEN 'true' ELSE 'false' END),'$.finished_at',CASE WHEN ?5 THEN NULL ELSE ?6 END)",params![prior.raw,outcome,reason,exit,uncertain,now],|r|r.get(0))?;
    match provider {
        Some(provider) => input
            .conn
            .query_row(
                "SELECT json_set(?1,'$.provider_state',?2)",
                params![updated, provider],
                |r| r.get(0),
            )
            .map_err(Into::into),
        None => Ok(updated),
    }
}

fn snapshot(input: &Input<'_>, task_id: &str, revision: i64) -> Result<String> {
    let raw: String = input.conn.query_row(
        "SELECT json_object('id',?1,'expected_revision',?2)",
        params![task_id, revision],
        |r| r.get(0),
    )?;
    let response = briefs::get(&Input {
        conn: input.conn,
        raw: &raw,
    })?;
    Ok(input
        .conn
        .query_row("SELECT json_extract(?1,'$.item')", [response], |r| r.get(0))?)
}

fn absolute_path(input: &Input<'_>, key: &str) -> Result<String> {
    let path = input.required_text(key, 4096)?;
    if !std::path::Path::new(&path).is_absolute() {
        return Err(Error::invalid(format!(
            "{key} must be absolute; the native host must validate its repository identity"
        )));
    }
    Ok(path)
}

fn prepare(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&[
        "id",
        "request_id",
        "expected_revision",
        "task_id",
        "source_revision",
        "repository_id",
        "repository_revision",
        "provider",
        "permission_mode",
        "acknowledge_bypass",
        "cwd",
        "git_dir",
        "git_common_dir",
        "head_oid",
        "head_ref",
        "kind",
    ])?;
    if revision != 1 {
        return Err(refuse(
            "invalid_state",
            "a launch attempt is immutable; prepare a new run ID",
        ));
    }
    let task_id = input.id("task_id")?;
    let source_revision = input.integer("source_revision", None, MAX_INTEGER)?;
    let repository_id = input.id("repository_id")?;
    let repository_revision = input.integer("repository_revision", None, MAX_INTEGER)?;
    let provider = input.required_text("provider", 32)?;
    let mode = input.required_text("permission_mode", 32)?;
    let kind = input
        .text("kind", 32)?
        .unwrap_or_else(|| "external_terminal".into());
    if !["external_terminal", "managed"].contains(&kind.as_str()) {
        return Err(Error::invalid("unsupported execution adapter"));
    }
    if !["codex", "claude"].contains(&provider.as_str())
        || ![
            "inspect",
            "ask",
            "edit",
            "auto_review",
            "preapproved",
            "bypass",
        ]
        .contains(&mode.as_str())
    {
        return Err(Error::invalid(
            "unsupported terminal provider or permission mode",
        ));
    }
    let bypass = input.boolean("acknowledge_bypass", false)?;
    if (mode == "bypass") != bypass {
        return Err(Error::invalid(
            "bypass requires explicit acknowledgment for this attempt only",
        ));
    }
    let cwd = absolute_path(input, "cwd")?;
    let git_dir = absolute_path(input, "git_dir")?;
    let common = absolute_path(input, "git_common_dir")?;
    let head = input.text("head_oid", 64)?;
    let head_ref = input.text("head_ref", 1024)?;
    if head_ref.as_ref().is_some_and(|s| {
        !s.starts_with("refs/heads/")
            || s.len() <= "refs/heads/".len()
            || s.contains("..")
            || s.contains("@{")
            || s.chars()
                .any(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\".contains(c))
    }) {
        return Err(Error::invalid(
            "head_ref must be a branch reference or null for detached or unobserved HEAD",
        ));
    }
    if head
        .as_ref()
        .is_some_and(|s| ![40, 64].contains(&s.len()) || !s.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(Error::invalid(
            "head_oid must be a full Git object ID or null for an unborn branch",
        ));
    }
    let brief = snapshot(input, &task_id, source_revision)?;
    let linked: bool = input.conn.query_row("SELECT EXISTS(SELECT 1 FROM json_each(?1,'$.repositories') WHERE json_extract(value,'$.id')=?2 AND json_extract(value,'$.revision')=?3)",params![brief,repository_id,repository_revision],|r|r.get(0))?;
    if !linked {
        return Err(refuse(
            "revision_conflict",
            "the selected repository is not linked at the requested revision",
        ));
    }
    let identity: String = input.conn.query_row(
        "SELECT identity_key FROM work_repositories WHERE id=?1",
        [&repository_id],
        |r| r.get(0),
    )?;
    if identity != format!("local:{common}") {
        return Err(refuse(
            "repository_mismatch",
            "the supplied Git common directory does not match the registered repository",
        ));
    }
    let busy: bool = input.conn.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM work_runs WHERE repository_id=?2 AND {ACTIVE})"),
        params![now, repository_id],
        |r| r.get(0),
    )?;
    if busy {
        return Err(refuse(
            "repository_busy",
            "this repository already has a prepared, active or unresolved run",
        ));
    }
    let active: i64 = input.conn.query_row(
        &format!("SELECT count(*) FROM work_runs WHERE {ACTIVE}"),
        [now],
        |r| r.get(0),
    )?;
    if active >= MAX_ACTIVE_RUNS {
        return Err(refuse(
            "capacity_reached",
            "two runs are already prepared, active or unresolved; finish or reconcile one before launching another",
        ));
    }
    let body: String = input.conn.query_row("SELECT json_object('id',?1,'revision',?2,'updated_at',?3,'created_at',?3,'expires_at',?4,'kind','external_terminal','task_id',?5,'source_revision',?6,'task_title',json_extract(?7,'$.task.title'),'repository_id',?8,'provider',?9,'permission_mode',?10,'bypass_acknowledged',json(CASE WHEN ?11 THEN 'true' ELSE 'false' END),'cwd',?12,'git_dir',?13,'git_common_dir',?14,'head_oid',?15,'head_ref',?16,'state','prepared','owner_id',NULL,'session_id',NULL,'process_id',NULL,'process_start',NULL,'claimed_at',NULL,'started_at',NULL,'finished_at',NULL,'exit_code',NULL,'reason','','outcome_uncertain',json('false'))",params![id,revision,now,now.saturating_add(300),task_id,source_revision,brief,repository_id,provider,mode,bypass,cwd,git_dir,common,head,head_ref],|r|r.get(0))?;
    let body: String = input.conn.query_row(
        "SELECT json_set(?1,'$.kind',?2)",
        params![body, kind],
        |r| r.get(0),
    )?;
    put_body(input.conn, Entity::Run, id, revision, &body)?;
    input.conn.execute(
        "INSERT INTO work_run_inputs(run_id,brief) VALUES(?1,?2)",
        params![id, brief],
    )?;
    Ok(())
}

pub(super) fn get(input: &Input<'_>) -> Result<String> {
    input.fields(&["id"])?;
    let id = input.id("id")?;
    let result: String = input.conn.query_row("SELECT json_object('ok',json('true'),'item',json_set(r.body,'$.brief',json(i.brief))) FROM work_runs r JOIN work_run_inputs i ON i.run_id=r.id WHERE r.id=?1",[id],|r|r.get(0)).optional()?.ok_or_else(Error::missing)?;
    if result.len() > MAX_PAGE_DOCUMENTS {
        return Err(refuse(
            "response_too_large",
            "the complete run and brief exceed the response budget",
        ));
    }
    Ok(result)
}

pub(super) fn list(input: &Input<'_>) -> Result<String> {
    input.fields(&[
        "task_id",
        "repository_id",
        "state",
        "limit",
        "cursor",
        "newest",
    ])?;
    let task = input.optional_id("task_id")?;
    let repo = input.optional_id("repository_id")?;
    // Deleted tasks keep their run history. Unknown identifiers remain errors.
    super::require_record(input, Entity::Item, task.as_deref(), true)?;
    super::require_record(input, Entity::Repository, repo.as_deref(), false)?;
    let state = input.text("state", 32)?;
    if state.as_deref().is_some_and(|s| !STATES.contains(&s)) {
        return Err(Error::invalid("unknown run state"));
    }
    let newest = input.boolean("newest", false)?;
    let mut p = page(input)?;
    if newest && input.text("cursor", 180)?.is_none() {
        p.position = MAX_INTEGER;
        p.id = "~".into();
    }
    let mut clauses = Vec::new();
    let mut bindings = Vec::new();
    for (column, value) in [("task_id", task), ("repository_id", repo), ("state", state)] {
        if let Some(value) = value {
            bindings.push(Value::Text(value));
            clauses.push(format!("{column}=?{}", bindings.len()));
        }
    }
    // Only supplied filters enter the statement, so task/repository indexes
    // remain usable instead of hiding equality behind optional OR clauses.
    let filter = if clauses.is_empty() {
        "1".into()
    } else {
        clauses.join(" AND ")
    };
    let total = input.conn.query_row(
        &format!("SELECT count(*) FROM work_runs WHERE {filter}"),
        params_from_iter(&bindings),
        |r| r.get(0),
    )?;
    let (position, id, limit) = (bindings.len() + 1, bindings.len() + 2, bindings.len() + 3);
    let (compare, direction) = if newest { ("<", "DESC") } else { (">", "ASC") };
    let mut statement = input.conn.prepare(&format!("SELECT json_remove(body,'$.output','$.effective_configuration'),created_at,id FROM work_runs WHERE {filter} AND (created_at,id){compare}(?{position},?{id}) ORDER BY created_at {direction},id {direction} LIMIT ?{limit}"))?;
    bindings.extend([
        Value::Integer(p.position),
        Value::Text(p.id),
        Value::Integer(p.limit + 1),
    ]);
    let mut rows = statement.query(params_from_iter(&bindings))?;
    page_response(input.conn, collect_page(&mut rows, p.limit)?, total)
}
