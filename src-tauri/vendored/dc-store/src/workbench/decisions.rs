//! Durable human decisions for one live provider callback. Recording a decision
//! is separate from consuming delivery authority and from provider resolution.
//! The owning protocol adapter computes and rechecks the SHA-256 payload digest;
//! this store also preserves the complete immutable payload and run binding.
use super::{
    Entity, Error, Input, MAX_INTEGER, Result, collect_page, page, page_response, put_body,
};
use rusqlite::{OptionalExtension, params};

fn stale() -> Error {
    Error {
        code: "decision_stale",
        message:
            "the request expired or its run, task or repository changed; no permission was granted"
                .into(),
    }
}
fn digest(input: &Input<'_>) -> Result<String> {
    let value = input.required_text("payload_digest", 64)?;
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::invalid(
            "payload_digest must be a lowercase SHA-256 digest",
        ));
    }
    Ok(value)
}
// Time is always supplied by the store clock. The request carries a snapshot,
// not authority to keep acting after a task/repository changes or the run ends.
const FRESH: &str = "r.state='running' AND t.deleted=0
 AND t.revision=json_extract(d.body,'$.source_revision')
 AND json_extract(r.body,'$.owner_id')=json_extract(d.body,'$.owner_id')
 AND json_extract(r.body,'$.session_id')=json_extract(d.body,'$.session_id')
 AND json_extract(r.body,'$.cwd')=json_extract(d.body,'$.cwd')
 AND json_extract(r.body,'$.permission_mode')=json_extract(d.body,'$.permission_mode')
 AND repo.revision=json_extract(d.body,'$.repository_revision')
 AND json_extract(d.body,'$.expires_at')>?1";
const FROM: &str = "work_decisions d JOIN work_runs r ON r.id=d.run_id
 JOIN work_items t ON t.id=json_extract(d.body,'$.task_id')
 JOIN work_repositories repo ON repo.id=json_extract(d.body,'$.repository_id')";
// The inbox uses this same validity predicate. Its static SQL aliases the
// callback q; only a trusted parameter index varies between the read paths.
pub(super) fn fresh_notice(clock_parameter: u8) -> String {
    let fresh = FRESH.replace("?1", &format!("?{clock_parameter}"));
    format!("EXISTS(SELECT 1 FROM {FROM} WHERE d.id=q.id AND d.state='pending' AND {fresh})")
}
fn body() -> String {
    format!(
        "json_set(d.body,'$.actionable',json(CASE WHEN d.state='pending' AND {FRESH} THEN 'true' ELSE 'false' END))"
    )
}
pub(super) fn projection(conn: &rusqlite::Connection, id: &str, now: i64) -> Result<String> {
    conn.query_row(
        &format!("SELECT {} FROM {FROM} WHERE d.id=?2", body()),
        params![now, id],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(Error::missing)
}
fn require_fresh(input: &Input<'_>, id: &str, now: i64) -> Result<()> {
    let valid: bool = input.conn.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {FROM} WHERE d.id=?2 AND {FRESH})"),
        params![now, id],
        |r| r.get(0),
    )?;
    if valid { Ok(()) } else { Err(stale()) }
}
fn owns(input: &Input<'_>, prior: &Input<'_>) -> Result<()> {
    for key in ["owner_id", "session_id"] {
        if input.id(key)? != prior.id(key)? {
            return Err(Error {
                code: "owner_mismatch",
                message: "the decision callback belongs to another owner or session".into(),
            });
        }
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
    if method == "decisions.create" {
        return create(input, id, revision, now);
    }
    let raw: String = input
        .conn
        .query_row("SELECT body FROM work_decisions WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .optional()?
        .ok_or_else(Error::missing)?;
    let prior = Input {
        conn: input.conn,
        raw: &raw,
    };
    let state = prior.required_text("state", 32)?;
    let next = match method {
        "decisions.decide" => {
            input.fields(&[
                "id",
                "request_id",
                "expected_revision",
                "payload_digest",
                "decision",
                "answer",
            ])?;
            require_fresh(input, id, now)?;
            if state != "pending" || digest(input)? != prior.required_text("payload_digest", 64)? {
                return Err(stale());
            }
            let decision = input.required_text("decision", 20)?;
            let kind = prior.required_text("kind", 20)?;
            let answer = input.text("answer", 16384)?;
            if !(decision == "deny"
                || kind == "permission" && decision == "allow_once"
                || kind == "question" && decision == "answer")
                || (decision == "answer") != answer.as_ref().is_some_and(|s| !s.trim().is_empty())
                || decision != "answer" && answer.is_some()
            {
                return Err(Error::invalid(
                    "choose a one-time permission decision or provide the requested answer",
                ));
            }
            input.conn.query_row("SELECT json_set(?1,'$.state','decided','$.decision',?2,'$.answer',?3,'$.decided_at',?4)",params![raw,decision,answer,now],|r|r.get::<_,String>(0))?
        }
        "decisions.claim" => {
            input.fields(&[
                "id",
                "request_id",
                "expected_revision",
                "owner_id",
                "session_id",
                "provider_thread_id",
                "provider_turn_id",
                "protocol_request_id",
                "payload_digest",
            ])?;
            owns(input, &prior)?;
            require_fresh(input, id, now)?;
            if state != "decided" || digest(input)? != prior.required_text("payload_digest", 64)? {
                return Err(stale());
            }
            for key in [
                "provider_thread_id",
                "provider_turn_id",
                "protocol_request_id",
            ] {
                if input.required_text(key, 256)? != prior.required_text(key, 256)? {
                    return Err(stale());
                }
            }
            input.conn.query_row(
                "SELECT json_set(?1,'$.state','dispatching','$.dispatched_at',?2)",
                params![raw, now],
                |r| r.get::<_, String>(0),
            )?
        }
        "decisions.resolve" => {
            input.fields(&[
                "id",
                "request_id",
                "expected_revision",
                "owner_id",
                "session_id",
                "state",
                "reason",
            ])?;
            owns(input, &prior)?;
            let next = input.required_text("state", 20)?;
            if !["pending", "decided", "dispatching"].contains(&state.as_str())
                || !["resolved", "cancelled"].contains(&next.as_str())
            {
                return Err(Error::invalid(
                    "only a live callback can be resolved or cancelled",
                ));
            }
            let reason = input.required_text("reason", 2048)?;
            input.conn.query_row(
                "SELECT json_set(?1,'$.state',?2,'$.reason',?3,'$.resolved_at',?4)",
                params![raw, next, reason, now],
                |r| r.get::<_, String>(0),
            )?
        }
        _ => return Err(Error::invalid("unsupported decision transition")),
    };
    let updated: String = input.conn.query_row(
        "SELECT json_set(?1,'$.revision',?2,'$.updated_at',?3)",
        params![next, revision, now],
        |r| r.get(0),
    )?;
    put_body(input.conn, Entity::Decision, id, revision, &updated)
}

fn create(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&[
        "id",
        "request_id",
        "expected_revision",
        "run_id",
        "owner_id",
        "session_id",
        "provider_thread_id",
        "provider_turn_id",
        "protocol_request_id",
        "kind",
        "payload",
        "payload_digest",
        "deadline",
    ])?;
    if revision != 1 {
        return Err(Error::invalid(
            "a provider request is immutable; use a new request identity",
        ));
    }
    let run_id = input.id("run_id")?;
    let raw: String = input
        .conn
        .query_row("SELECT body FROM work_runs WHERE id=?1", [&run_id], |r| {
            r.get(0)
        })
        .optional()?
        .ok_or_else(Error::missing)?;
    let run = Input {
        conn: input.conn,
        raw: &raw,
    };
    owns(input, &run)?;
    if run.required_text("state", 32)? != "running" {
        return Err(stale());
    }
    let task = run.id("task_id")?;
    let repo = run.id("repository_id")?;
    let repo_revision:i64=input.conn.query_row("SELECT json_extract(j.value,'$.revision') FROM work_run_inputs i,json_each(i.brief,'$.repositories') j WHERE i.run_id=?1 AND json_extract(j.value,'$.id')=?2",params![run_id,repo],|r|r.get(0))?;
    let thread = input.required_text("provider_thread_id", 256)?;
    let turn = input.required_text("provider_turn_id", 256)?;
    let protocol = input.required_text("protocol_request_id", 256)?;
    let kind = input.required_text("kind", 20)?;
    if !["permission", "question"].contains(&kind.as_str()) {
        return Err(Error::invalid("unsupported decision kind"));
    }
    let payload = input.required_text("payload", 65536)?;
    Input::new(input.conn, &payload)?;
    let digest = digest(input)?;
    let deadline = input.integer("deadline", None, MAX_INTEGER)?;
    let expires = deadline.min(now.saturating_add(300));
    if expires <= now {
        return Err(stale());
    }
    let count: i64 = input.conn.query_row(
        "SELECT count(*) FROM work_decisions WHERE run_id=?1",
        [&run_id],
        |r| r.get(0),
    )?;
    let pending:i64=input.conn.query_row("SELECT count(*) FROM work_decisions WHERE run_id=?1 AND state IN ('pending','decided','dispatching')",[&run_id],|r|r.get(0))?;
    if count >= 2048 || pending >= 32 {
        return Err(Error {
            code: "capacity_reached",
            message: "this run reached its bounded decision capacity".into(),
        });
    }
    let body:String=input.conn.query_row("SELECT json_object('id',?1,'revision',1,'run_id',?2,'task_id',?3,'source_revision',json_extract(?4,'$.source_revision'),'repository_id',?5,'repository_revision',?6,'owner_id',json_extract(?4,'$.owner_id'),'session_id',json_extract(?4,'$.session_id'),'cwd',json_extract(?4,'$.cwd'),'permission_mode',json_extract(?4,'$.permission_mode'),'policy_revision',1,'provider',json_extract(?4,'$.provider'),'provider_thread_id',?7,'provider_turn_id',?8,'protocol_request_id',?9,'kind',?10,'payload',?11,'payload_digest',?12,'created_at',?13,'updated_at',?13,'expires_at',?14,'state','pending','decision',NULL,'answer',NULL,'decided_at',NULL,'dispatched_at',NULL,'resolved_at',NULL,'reason','')",params![id,run_id,task,raw,repo,repo_revision,thread,turn,protocol,kind,payload,digest,now,expires],|r|r.get(0))?;
    put_body(input.conn, Entity::Decision, id, 1, &body)?;
    require_fresh(input, id, now)
}

pub(super) fn get(input: &Input<'_>, now: i64) -> Result<String> {
    input.fields(&["id"])?;
    input
        .conn
        .query_row(
            &format!(
                "SELECT json_object('ok',json('true'),'item',{}) FROM {FROM} WHERE d.id=?2",
                body()
            ),
            params![now, input.id("id")?],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(Error::missing)
}
pub(super) fn list(input: &Input<'_>, now: i64) -> Result<String> {
    input.fields(&["run_id", "state", "limit", "cursor"])?;
    let run = input.id("run_id")?;
    super::require_record(input, Entity::Run, Some(&run), true)?;
    let state = input.text("state", 20)?;
    if state.as_deref().is_some_and(|s| {
        !["pending", "decided", "dispatching", "resolved", "cancelled"].contains(&s)
    }) {
        return Err(Error::invalid("unsupported decision state"));
    }
    let p = page(input)?;
    let total: i64 = input.conn.query_row(
        "SELECT count(*) FROM work_decisions WHERE run_id=?1 AND (?2 IS NULL OR state=?2)",
        params![run, state],
        |r| r.get(0),
    )?;
    let mut stmt=input.conn.prepare(&format!("SELECT {},d.created_at,d.id FROM {FROM} WHERE d.run_id=?2 AND (?3 IS NULL OR d.state=?3) AND (d.created_at,d.id)>(?4,?5) ORDER BY d.created_at,d.id LIMIT ?6",body()))?;
    let rows = collect_page(
        &mut stmt.query(params![now, run, state, p.position, p.id, p.limit + 1])?,
        p.limit,
    )?;
    page_response(input.conn, rows, total)
}
