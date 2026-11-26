//! Cross-repository execution and integration queue.
//!
//! Snapshots candidate `(repository_id, git_dir, base_oid)` triples at enqueue,
//! acquires checkout leases across repositories in canonical ascending
//! `repository_id` order (all-or-none inside a single store transaction), bounds
//! concurrency (`2..=4`, default `2`) and execution time (`45` active minutes /
//! `2700s`, `2` elapsed hours / `7200s`), propagates DAG member failures to
//! downstream dependents, and requires an explicit combined verification step
//! with `publication` fixed to `"manual_per_repository"`.

use super::{
    Affected, Entity, Error, Input, MAX_INTEGER, Result, collect_page, page, page_response,
    put_body, record_change, runs,
};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value};
use std::collections::{HashMap, HashSet, VecDeque};

pub const DEFAULT_CONCURRENCY: i64 = 2;
pub const MIN_CONCURRENCY: i64 = 2;
pub const MAX_CONCURRENCY: i64 = 4;
pub const MAX_ACTIVE_TIMEOUT_SECONDS: i64 = 45 * 60;
pub const MAX_ELAPSED_TIMEOUT_SECONDS: i64 = 2 * 60 * 60;
const MAX_MEMBERS: usize = 32;

const QUEUE_STATES: &[&str] = &[
    "queued",
    "leased",
    "verifying",
    "passed",
    "failed",
    "cancelled",
    "timed_out",
];

fn refuse(code: &'static str, message: impl Into<String>) -> Error {
    Error {
        code,
        message: message.into(),
    }
}

fn lower_hex_oid(value: &str) -> bool {
    [40, 64].contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn decode_depends(conn: &Connection, raw: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare_cached("SELECT value FROM json_each(?1)")?;
    let mut rows = stmt.query([raw])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row.get(0)?);
    }
    Ok(out)
}

#[derive(Clone)]
struct MemberRow {
    member_id: String,
    repository_id: String,
    git_dir: String,
    base_oid: String,
    task_id: Option<String>,
    run_id: Option<String>,
    position: i64,
    state: String,
    lease_held: bool,
    depends_on_json: String,
    depends_on: Vec<String>,
    started_at: Option<i64>,
    active_seconds: i64,
    last_active_at: Option<i64>,
    verification_json: Option<String>,
}

fn load_members(conn: &Connection, queue_id: &str) -> Result<Vec<MemberRow>> {
    let mut stmt = conn.prepare(
        "SELECT member_id,repository_id,git_dir,base_oid,task_id,run_id,position,state,lease_held,depends_on,started_at,active_seconds,last_active_at,verification_json
         FROM work_queue_members WHERE queue_id=?1 ORDER BY position, member_id",
    )?;
    let mut rows = stmt.query([queue_id])?;
    let mut members = Vec::new();
    while let Some(row) = rows.next()? {
        let depends_on_json: String = row.get(9)?;
        let depends_on = decode_depends(conn, &depends_on_json)?;
        members.push(MemberRow {
            member_id: row.get(0)?,
            repository_id: row.get(1)?,
            git_dir: row.get(2)?,
            base_oid: row.get(3)?,
            task_id: row.get(4)?,
            run_id: row.get(5)?,
            position: row.get(6)?,
            state: row.get(7)?,
            lease_held: row.get::<_, i64>(8)? != 0,
            depends_on_json,
            depends_on,
            started_at: row.get(10)?,
            active_seconds: row.get(11)?,
            last_active_at: row.get(12)?,
            verification_json: row.get(13)?,
        });
    }
    Ok(members)
}

fn build_members_json(conn: &Connection, members: &[MemberRow]) -> Result<String> {
    let mut encoded = Vec::with_capacity(members.len());
    for m in members {
        let item: String = conn.query_row(
            "SELECT json_object('member_id',?1,'repository_id',?2,'git_dir',?3,'base_oid',?4,'task_id',?5,'run_id',?6,'position',?7,'state',?8,'lease_held',json(CASE WHEN ?9 THEN 'true' ELSE 'false' END),'depends_on',json(?10),'started_at',?11,'active_seconds',?12,'last_active_at',?13,'verification',CASE WHEN ?14 IS NULL THEN NULL ELSE json(?14) END)",
            params![
                m.member_id,
                m.repository_id,
                m.git_dir,
                m.base_oid,
                m.task_id,
                m.run_id,
                m.position,
                m.state,
                m.lease_held,
                m.depends_on_json,
                m.started_at,
                m.active_seconds,
                m.last_active_at,
                m.verification_json
            ],
            |r| r.get(0),
        )?;
        encoded.push(item);
    }
    Ok(format!("[{}]", encoded.join(",")))
}

#[allow(clippy::too_many_arguments)]
fn sync_queue_body(
    conn: &Connection,
    queue_id: &str,
    revision: i64,
    now: i64,
    state: &str,
    leased_at: Option<i64>,
    finished_at: Option<i64>,
    reason: Option<&str>,
    verification_doc: Option<&str>,
) -> Result<()> {
    let members = load_members(conn, queue_id)?;
    let members_json = build_members_json(conn, &members)?;
    let mut updated: String = conn.query_row(
        "SELECT json_set(body,'$.revision',?2,'$.updated_at',?3,'$.state',?4,'$.leased_at',?5,'$.finished_at',?6,'$.members',json(?7))
         FROM work_queue WHERE id=?1",
        params![queue_id, revision, now, state, leased_at, finished_at, members_json],
        |r| r.get(0),
    )?;
    if let Some(reason_text) = reason {
        updated = conn.query_row(
            "SELECT json_set(?1,'$.reason',?2)",
            params![updated, reason_text],
            |r| r.get(0),
        )?;
    }
    if let Some(doc) = verification_doc {
        updated = conn.query_row(
            "SELECT json_set(?1,'$.publication','manual_per_repository','$.verification',json(?2))",
            params![updated, doc],
            |r| r.get(0),
        )?;
    }
    put_body(conn, Entity::Queue, queue_id, revision, &updated)
}

fn is_active_member_state(state: &str) -> bool {
    matches!(state, "leased" | "running" | "verifying")
}

fn is_terminal_member_state(state: &str) -> bool {
    matches!(
        state,
        "passed" | "failed" | "blocked" | "cancelled" | "timed_out"
    )
}

fn member_active_at(m: &MemberRow, now: i64) -> i64 {
    if is_active_member_state(&m.state)
        && let Some(last) = m.last_active_at
    {
        return m
            .active_seconds
            .saturating_add(now.saturating_sub(last).max(0));
    }
    m.active_seconds
}

fn queue_expired(
    leased_at: Option<i64>,
    elapsed_limit: i64,
    active_limit: i64,
    members: &[MemberRow],
    now: i64,
) -> bool {
    if let Some(leased) = leased_at
        && now.saturating_sub(leased) >= elapsed_limit
    {
        return true;
    }
    members
        .iter()
        .any(|m| is_active_member_state(&m.state) && member_active_at(m, now) >= active_limit)
}

fn apply_timeout(conn: &Connection, queue_id: &str, revision: i64, now: i64) -> Result<()> {
    let members = load_members(conn, queue_id)?;
    for m in &members {
        if !is_terminal_member_state(&m.state) {
            let total_active = member_active_at(m, now);
            conn.execute(
                "UPDATE work_queue_members
                 SET state='timed_out', lease_held=0, active_seconds=?3, last_active_at=?4
                 WHERE queue_id=?1 AND member_id=?2",
                params![queue_id, m.member_id, total_active, now],
            )?;
        } else if m.lease_held {
            conn.execute(
                "UPDATE work_queue_members SET lease_held=0 WHERE queue_id=?1 AND member_id=?2",
                params![queue_id, m.member_id],
            )?;
        }
    }
    let leased_at: Option<i64> = conn.query_row(
        "SELECT json_extract(body,'$.leased_at') FROM work_queue WHERE id=?1",
        [queue_id],
        |r| r.get(0),
    )?;
    sync_queue_body(
        conn,
        queue_id,
        revision,
        now,
        "timed_out",
        leased_at,
        Some(now),
        Some("timeout exceeded"),
        None,
    )
}

fn sweep_expired_queues(conn: &Connection, now: i64) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT id, revision, json_extract(body,'$.leased_at'),
                json_extract(body,'$.elapsed_timeout_seconds'),
                json_extract(body,'$.active_timeout_seconds')
         FROM work_queue WHERE state IN ('leased','verifying') ORDER BY created_at, id",
    )?;
    let mut rows = stmt.query([])?;
    let mut candidates = Vec::new();
    while let Some(row) = rows.next()? {
        candidates.push((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
        ));
    }
    drop(rows);
    drop(stmt);
    for (qid, rev, leased_at, elapsed_limit, active_limit) in candidates {
        let members = load_members(conn, &qid)?;
        if queue_expired(leased_at, elapsed_limit, active_limit, &members, now) {
            let next_rev = rev + 1;
            apply_timeout(conn, &qid, next_rev, now)?;
            record_change(
                conn,
                Entity::Queue,
                &qid,
                next_rev,
                "queue.sweep",
                now,
                &Affected::none(),
            )?;
        }
    }
    Ok(())
}

fn checkout_or_repo_conflict(
    conn: &Connection,
    repository_id: &str,
    git_dir: &str,
    now: i64,
) -> Result<Option<String>> {
    let queue_conflict: Option<String> = conn
        .query_row(
            "SELECT queue_id || ':' || member_id FROM work_queue_members
             WHERE lease_held=1 AND (repository_id=?1 OR git_dir=?2) LIMIT 1",
            params![repository_id, git_dir],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(holder) = queue_conflict {
        return Ok(Some(format!(
            "repository {repository_id} or checkout {git_dir} already holds queue lease {holder}"
        )));
    }
    if runs::has_active_checkout(conn, repository_id, git_dir, now)? {
        return Ok(Some(format!(
            "repository {repository_id} checkout {git_dir} already has an active run"
        )));
    }
    Ok(None)
}

/// Whether a repository currently holds an active queue lease.
pub(super) fn active_on_repository(conn: &Connection, repository_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM work_queue_members WHERE repository_id=?1 AND lease_held=1)",
        [repository_id],
        |r| r.get(0),
    )?)
}

/// Count of queue memberships pointing at `repository_id` (used before absorb).
pub(super) fn member_count_for_repository(conn: &Connection, repository_id: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT count(*) FROM work_queue_members WHERE repository_id=?1",
        [repository_id],
        |r| r.get(0),
    )?)
}

/// Check queue lease ownership for `runs.prepare`.
///
/// When `claimed` is `None`, any held lease on `(repository_id, git_dir)` is
/// rejected. When `claimed` is `Some((queue_id, member_id))`, verifies that exact
/// member holds an active lease on that exact `(repository_id, git_dir, task_id)`.
pub(super) fn verify_run_lease(
    conn: &Connection,
    repository_id: &str,
    git_dir: &str,
    task_id: &str,
    claimed: Option<(&str, &str)>,
    now: i64,
) -> Result<()> {
    sweep_expired_queues(conn, now)?;
    match claimed {
        None => {
            let conflict: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM work_queue_members WHERE lease_held=1 AND (git_dir=?1 OR repository_id=?2))",
                params![git_dir, repository_id],
                |r| r.get(0),
            )?;
            if conflict {
                return Err(refuse(
                    "checkout_busy",
                    "this checkout or repository holds an active cross-repository queue lease; finish, verify or cancel the queue entry first",
                ));
            }
            Ok(())
        }
        Some((qid, mid)) => {
            let row: Option<(String, String, Option<String>, String, bool)> = conn
                .query_row(
                    "SELECT repository_id, git_dir, task_id, state, lease_held
                     FROM work_queue_members WHERE queue_id=?1 AND member_id=?2",
                    params![qid, mid],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get::<_, i64>(4)? != 0,
                        ))
                    },
                )
                .optional()?;
            let Some((repo, dir, m_task, m_state, held)) = row else {
                return Err(refuse(
                    "queue_lease_required",
                    "the named queue member does not exist",
                ));
            };
            if !held
                || !matches!(m_state.as_str(), "leased" | "running")
                || repo != repository_id
                || dir != git_dir
                || m_task.as_deref().is_some_and(|t| t != task_id)
            {
                return Err(refuse(
                    "queue_lease_required",
                    "the named queue member does not hold an active lease for this repository checkout and task",
                ));
            }
            Ok(())
        }
    }
}

/// Bind `run_id` to an already-leased `work_queue_members` row after `work_runs` insert.
pub(super) fn attach_run(
    conn: &Connection,
    queue_id: &str,
    member_id: &str,
    run_id: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE work_queue_members SET run_id=?1 WHERE queue_id=?2 AND member_id=?3",
        params![run_id, queue_id, member_id],
    )?;
    let (rev, state, leased_at, finished_at): (i64, String, Option<i64>, Option<i64>) = conn.query_row(
        "SELECT revision, state, json_extract(body,'$.leased_at'), json_extract(body,'$.finished_at') FROM work_queue WHERE id=?1",
        [queue_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    sync_queue_body(
        conn,
        queue_id,
        rev,
        now,
        &state,
        leased_at,
        finished_at,
        None,
        None,
    )
}

fn validate_dag(members: &[(String, Vec<String>)]) -> Result<()> {
    let mut indegree: HashMap<&str, usize> = HashMap::new();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
    for (id, _) in members {
        indegree.insert(id.as_str(), 0);
    }
    for (id, deps) in members {
        let mut seen_dep = HashSet::new();
        for dep in deps {
            if dep == id {
                return Err(Error::invalid("a queue member cannot depend on itself"));
            }
            if !indegree.contains_key(dep.as_str()) {
                return Err(Error::invalid(format!(
                    "member {id} depends_on unknown member {dep}"
                )));
            }
            if !seen_dep.insert(dep.as_str()) {
                return Err(Error::invalid(format!(
                    "member {id} lists duplicate dependency {dep}"
                )));
            }
            *indegree.get_mut(id.as_str()).unwrap() += 1;
            dependents
                .entry(dep.as_str())
                .or_default()
                .push(id.as_str());
        }
    }
    let mut ready = VecDeque::new();
    for (&id, &deg) in &indegree {
        if deg == 0 {
            ready.push_back(id);
        }
    }
    let mut visited = 0;
    while let Some(node) = ready.pop_front() {
        visited += 1;
        if let Some(children) = dependents.get(node) {
            for &child in children {
                let entry = indegree.get_mut(child).unwrap();
                *entry -= 1;
                if *entry == 0 {
                    ready.push_back(child);
                }
            }
        }
    }
    if visited != members.len() {
        return Err(Error::invalid(
            "queue member depends_on links contain a cycle",
        ));
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
    match method {
        "queue.enqueue" => enqueue(input, id, revision, now),
        "queue.acquire" => acquire(input, id, revision, now),
        "queue.member.update" => member_update(input, id, revision, now),
        "queue.verify" => verify(input, id, revision, now),
        "queue.sweep" => sweep_one(input, id, revision, now),
        "queue.cancel" => cancel(input, id, revision, now),
        _ => Err(Error {
            code: "unknown_method",
            message: format!("unknown queue method {method}"),
        }),
    }
}

fn enqueue(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&[
        "request_id",
        "id",
        "expected_revision",
        "title",
        "concurrency_limit",
        "active_timeout_seconds",
        "elapsed_timeout_seconds",
        "members",
    ])?;
    if revision != 1 {
        return Err(refuse(
            "invalid_state",
            "queue entries are created once at revision 0",
        ));
    }
    let title = input.required_text("title", 300)?;
    let concurrency_limit = input.integer(
        "concurrency_limit",
        Some(DEFAULT_CONCURRENCY),
        MAX_CONCURRENCY,
    )?;
    if concurrency_limit < MIN_CONCURRENCY {
        return Err(Error::invalid(format!(
            "concurrency_limit must be between {MIN_CONCURRENCY} and {MAX_CONCURRENCY}"
        )));
    }
    let active_timeout = input.integer(
        "active_timeout_seconds",
        Some(MAX_ACTIVE_TIMEOUT_SECONDS),
        MAX_ACTIVE_TIMEOUT_SECONDS,
    )?;
    let elapsed_timeout = input.integer(
        "elapsed_timeout_seconds",
        Some(MAX_ELAPSED_TIMEOUT_SECONDS),
        MAX_ELAPSED_TIMEOUT_SECONDS,
    )?;
    if active_timeout < 1 || elapsed_timeout < 1 {
        return Err(Error::invalid(
            "active_timeout_seconds and elapsed_timeout_seconds must be at least 1",
        ));
    }
    if input.kind("members")?.as_deref() != Some("array") {
        return Err(Error::invalid("members must be a non-empty array"));
    }

    struct PendingMember {
        member_id: String,
        repository_id: String,
        git_dir: String,
        base_oid: String,
        task_id: Option<String>,
        depends_on: Vec<String>,
        depends_on_json: String,
    }

    let mut stmt = input
        .conn
        .prepare("SELECT type, value FROM json_each(?1,'$.members')")?;
    let mut rows = stmt.query([input.raw])?;
    let mut parsed = Vec::new();
    let mut seen_member_ids = HashSet::new();
    while let Some(row) = rows.next()? {
        if parsed.len() >= MAX_MEMBERS || row.get::<_, String>(0)? != "object" {
            return Err(Error::invalid(format!(
                "members holds between 1 and {MAX_MEMBERS} member objects"
            )));
        }
        let raw_member: String = row.get(1)?;
        let m_in = Input {
            conn: input.conn,
            raw: &raw_member,
        };
        m_in.fields(&[
            "member_id",
            "repository_id",
            "git_dir",
            "base_oid",
            "task_id",
            "depends_on",
        ])?;
        let member_id = m_in.id("member_id")?;
        if !seen_member_ids.insert(member_id.clone()) {
            return Err(Error::invalid(format!("duplicate member_id {member_id}")));
        }
        let repository_id = m_in.id("repository_id")?;
        let repo_exists: bool = input.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_repositories WHERE id=?1)",
            [&repository_id],
            |r| r.get(0),
        )?;
        if !repo_exists {
            return Err(Error::invalid(format!(
                "repository {repository_id} is not registered"
            )));
        }
        let git_dir = runs::absolute_path(&m_in, "git_dir")?;
        let base_oid = m_in.required_text("base_oid", 64)?;
        if !lower_hex_oid(&base_oid) {
            return Err(Error::invalid(
                "base_oid must be a lowercase 40- or 64-hex Git object ID",
            ));
        }
        let task_id = m_in.optional_id("task_id")?;
        if let Some(tid) = &task_id {
            let live_task: bool = input.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM work_items WHERE id=?1 AND deleted=0)",
                [tid],
                |r| r.get(0),
            )?;
            if !live_task {
                return Err(Error::invalid(format!(
                    "linked task {tid} does not exist or is deleted"
                )));
            }
        }
        let depends_on = m_in.strings("depends_on", MAX_MEMBERS, 128, false)?;
        let mut depends_on_json = "[]".to_string();
        for dep in &depends_on {
            depends_on_json = input.conn.query_row(
                "SELECT json_insert(?1,'$[#]',?2)",
                params![depends_on_json, dep],
                |r| r.get(0),
            )?;
        }
        parsed.push(PendingMember {
            member_id,
            repository_id,
            git_dir,
            base_oid,
            task_id,
            depends_on,
            depends_on_json,
        });
    }
    if parsed.is_empty() {
        return Err(Error::invalid("members must contain at least one member"));
    }
    let dag_input: Vec<(String, Vec<String>)> = parsed
        .iter()
        .map(|m| (m.member_id.clone(), m.depends_on.clone()))
        .collect();
    validate_dag(&dag_input)?;

    let mut sorted_repos: Vec<(&str, &str)> = parsed
        .iter()
        .map(|m| (m.repository_id.as_str(), m.member_id.as_str()))
        .collect();
    sorted_repos.sort();
    let mut lease_order_json = "[]".to_string();
    for (repo_id, _) in &sorted_repos {
        lease_order_json = input.conn.query_row(
            "SELECT json_insert(?1,'$[#]',?2)",
            params![lease_order_json, repo_id],
            |r| r.get(0),
        )?;
    }

    let shell_body: String = input.conn.query_row(
        "SELECT json_object('id',?1,'revision',?2,'title',?3,'state','queued','concurrency_limit',?4,'active_timeout_seconds',?5,'elapsed_timeout_seconds',?6,'lease_order',json(?7),'publication','manual_per_repository','verification',NULL,'reason','','created_at',?8,'updated_at',?8,'leased_at',NULL,'finished_at',NULL,'members',json('[]'))",
        params![
            id,
            revision,
            title,
            concurrency_limit,
            active_timeout,
            elapsed_timeout,
            lease_order_json,
            now
        ],
        |r| r.get(0),
    )?;
    put_body(input.conn, Entity::Queue, id, revision, &shell_body)?;
    for (idx, m) in parsed.into_iter().enumerate() {
        let pos = i64::try_from(idx).unwrap_or(i64::MAX);
        input.conn.execute(
            "INSERT INTO work_queue_members(queue_id,member_id,repository_id,git_dir,base_oid,task_id,run_id,position,state,lease_held,depends_on,started_at,active_seconds,last_active_at,verification_json)
             VALUES(?1,?2,?3,?4,?5,?6,NULL,?7,'queued',0,?8,NULL,0,NULL,NULL)",
            params![
                id,
                m.member_id,
                m.repository_id,
                m.git_dir,
                m.base_oid,
                m.task_id,
                pos,
                m.depends_on_json
            ],
        )?;
    }
    sync_queue_body(
        input.conn, id, revision, now, "queued", None, None, None, None,
    )
}

fn acquire(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&["request_id", "id", "expected_revision"])?;
    sweep_expired_queues(input.conn, now)?;
    let (state, concurrency_limit, leased_at): (String, i64, Option<i64>) = input
        .conn
        .query_row(
            "SELECT state, concurrency_limit, json_extract(body,'$.leased_at') FROM work_queue WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or_else(Error::missing)?;
    if !matches!(state.as_str(), "queued" | "leased") {
        return Err(refuse(
            "invalid_state",
            format!("cannot acquire leases for queue in state {state}"),
        ));
    }
    let members = load_members(input.conn, id)?;
    let by_id: HashMap<&str, &MemberRow> =
        members.iter().map(|m| (m.member_id.as_str(), m)).collect();
    let active_count = i64::try_from(
        members
            .iter()
            .filter(|m| m.lease_held || is_active_member_state(&m.state))
            .count(),
    )
    .unwrap_or(i64::MAX);
    let slots = concurrency_limit.saturating_sub(active_count).max(0) as usize;
    let mut candidates: Vec<&MemberRow> = members
        .iter()
        .filter(|m| {
            m.state == "queued"
                && m.depends_on
                    .iter()
                    .all(|dep| by_id.get(dep.as_str()).is_some_and(|d| d.state == "passed"))
        })
        .collect();
    // Canonical ascending acquisition order by repository_id, then position & member_id.
    candidates.sort_by(|a, b| {
        a.repository_id
            .cmp(&b.repository_id)
            .then_with(|| a.position.cmp(&b.position))
            .then_with(|| a.member_id.cmp(&b.member_id))
    });
    // Ensure we never lease two members from the same repository concurrently in one batch.
    let mut selected: Vec<&MemberRow> = Vec::new();
    let mut seen_repos: HashSet<&str> = members
        .iter()
        .filter(|m| m.lease_held)
        .map(|m| m.repository_id.as_str())
        .collect();
    for cand in candidates {
        if selected.len() >= slots {
            break;
        }
        if !seen_repos.insert(cand.repository_id.as_str()) {
            continue;
        }
        selected.push(cand);
    }
    if selected.is_empty() {
        return Err(refuse(
            "invalid_state",
            "no eligible queue members or concurrency slots are available to lease",
        ));
    }
    // All-or-none preflight across all selected members before mutating any lease.
    for cand in &selected {
        if let Some(reason) =
            checkout_or_repo_conflict(input.conn, &cand.repository_id, &cand.git_dir, now)?
        {
            return Err(refuse("lease_conflict", reason));
        }
    }
    for cand in &selected {
        input.conn.execute(
            "UPDATE work_queue_members
             SET state='leased', lease_held=1, started_at=coalesce(started_at,?3), last_active_at=?3
             WHERE queue_id=?1 AND member_id=?2",
            params![id, cand.member_id, now],
        )?;
    }
    let next_leased_at = leased_at.or(Some(now));
    sync_queue_body(
        input.conn,
        id,
        revision,
        now,
        "leased",
        next_leased_at,
        None,
        None,
        None,
    )
}

fn member_update(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&[
        "request_id",
        "id",
        "expected_revision",
        "member_id",
        "state",
        "run_id",
    ])?;
    let member_id = input.id("member_id")?;
    let target_state = input.required_text("state", 32)?;
    let run_id = input.optional_id("run_id")?;
    if !matches!(
        target_state.as_str(),
        "running" | "verifying" | "passed" | "failed"
    ) {
        return Err(Error::invalid(
            "queue.member.update state must be running, verifying, passed, or failed",
        ));
    }

    let (q_state, concurrency_limit, active_limit, elapsed_limit, leased_at): (
        String,
        i64,
        i64,
        i64,
        Option<i64>,
    ) = input
        .conn
        .query_row(
            "SELECT state, concurrency_limit,
                    json_extract(body,'$.active_timeout_seconds'),
                    json_extract(body,'$.elapsed_timeout_seconds'),
                    json_extract(body,'$.leased_at')
             FROM work_queue WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?
        .ok_or_else(Error::missing)?;
    if !matches!(q_state.as_str(), "leased" | "verifying") {
        return Err(refuse(
            "invalid_state",
            format!("cannot update member while queue is in state {q_state}"),
        ));
    }

    let members = load_members(input.conn, id)?;
    let Some(target) = members.iter().find(|m| m.member_id == member_id) else {
        return Err(Error::missing());
    };
    if queue_expired(leased_at, elapsed_limit, active_limit, &members, now) {
        return apply_timeout(input.conn, id, revision, now);
    }

    let valid_transition = match (target.state.as_str(), target_state.as_str()) {
        ("leased", "running" | "verifying" | "passed" | "failed") => target.lease_held,
        ("running", "running" | "verifying" | "passed" | "failed") => target.lease_held,
        ("verifying", "verifying" | "passed" | "failed") => true,
        _ => false,
    };
    if !valid_transition {
        return Err(refuse(
            "invalid_state",
            format!(
                "cannot move queue member {member_id} from {} to {target_state}",
                target.state
            ),
        ));
    }
    if let Some(rid) = &run_id {
        let valid_run: bool = input.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_runs WHERE id=?1 AND repository_id=?2)",
            params![rid, target.repository_id],
            |r| r.get(0),
        )?;
        if !valid_run {
            return Err(Error::invalid(
                "run_id must belong to this member's repository",
            ));
        }
    }

    let accrued = member_active_at(target, now);
    let keep_lease = if matches!(target_state.as_str(), "passed" | "failed") {
        0
    } else if target.lease_held {
        1
    } else {
        0
    };
    input.conn.execute(
        "UPDATE work_queue_members
         SET state=?3, lease_held=?4, run_id=coalesce(?5, run_id),
             started_at=coalesce(started_at,?6), active_seconds=?7, last_active_at=?6
         WHERE queue_id=?1 AND member_id=?2",
        params![
            id,
            member_id,
            target_state,
            keep_lease,
            run_id,
            now,
            accrued
        ],
    )?;

    if target_state == "failed" {
        let mut queue_bfs = VecDeque::new();
        queue_bfs.push_back(member_id.as_str());
        let mut to_block = HashSet::new();
        while let Some(failed_id) = queue_bfs.pop_front() {
            for m in &members {
                if m.member_id != member_id
                    && !is_terminal_member_state(&m.state)
                    && m.depends_on.iter().any(|d| d == failed_id)
                    && to_block.insert(m.member_id.as_str())
                {
                    queue_bfs.push_back(m.member_id.as_str());
                }
            }
        }
        for blocked_id in to_block {
            input.conn.execute(
                "UPDATE work_queue_members SET state='blocked', lease_held=0
                 WHERE queue_id=?1 AND member_id=?2",
                params![id, blocked_id],
            )?;
        }
    } else if target_state == "passed" {
        let refreshed = load_members(input.conn, id)?;
        let by_id: HashMap<&str, &MemberRow> = refreshed
            .iter()
            .map(|m| (m.member_id.as_str(), m))
            .collect();
        let active_now = i64::try_from(
            refreshed
                .iter()
                .filter(|m| m.lease_held || is_active_member_state(&m.state))
                .count(),
        )
        .unwrap_or(i64::MAX);
        let mut slots = concurrency_limit.saturating_sub(active_now).max(0) as usize;
        if slots > 0 {
            let mut next_ready: Vec<&MemberRow> = refreshed
                .iter()
                .filter(|m| {
                    m.state == "queued"
                        && m.depends_on
                            .iter()
                            .all(|dep| by_id.get(dep.as_str()).is_some_and(|d| d.state == "passed"))
                })
                .collect();
            next_ready.sort_by(|a, b| {
                a.repository_id
                    .cmp(&b.repository_id)
                    .then_with(|| a.position.cmp(&b.position))
                    .then_with(|| a.member_id.cmp(&b.member_id))
            });
            let mut held_repos: HashSet<&str> = refreshed
                .iter()
                .filter(|m| m.lease_held)
                .map(|m| m.repository_id.as_str())
                .collect();
            for cand in next_ready {
                if slots == 0 {
                    break;
                }
                if held_repos.contains(cand.repository_id.as_str()) {
                    continue;
                }
                if checkout_or_repo_conflict(input.conn, &cand.repository_id, &cand.git_dir, now)?
                    .is_some()
                {
                    continue;
                }
                held_repos.insert(cand.repository_id.as_str());
                input.conn.execute(
                    "UPDATE work_queue_members
                     SET state='leased', lease_held=1, started_at=coalesce(started_at,?3), last_active_at=?3
                     WHERE queue_id=?1 AND member_id=?2",
                    params![id, cand.member_id, now],
                )?;
                slots -= 1;
            }
        }
    }

    let updated_members = load_members(input.conn, id)?;
    let any_active_or_queued = updated_members
        .iter()
        .any(|m| matches!(m.state.as_str(), "queued" | "leased" | "running"));
    let any_verifying = updated_members.iter().any(|m| m.state == "verifying");
    let any_failed = updated_members.iter().any(|m| m.state == "failed");
    let (next_q_state, next_finished_at) = if any_active_or_queued {
        ("leased", None)
    } else if any_verifying {
        ("verifying", None)
    } else if any_failed {
        // Release any lingering leases when queue cannot succeed.
        input.conn.execute(
            "UPDATE work_queue_members SET lease_held=0 WHERE queue_id=?1",
            [id],
        )?;
        ("failed", Some(now))
    } else {
        ("verifying", None)
    };
    sync_queue_body(
        input.conn,
        id,
        revision,
        now,
        next_q_state,
        leased_at,
        next_finished_at,
        None,
        None,
    )
}

fn verify(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&[
        "request_id",
        "id",
        "expected_revision",
        "results",
        "publication",
    ])?;
    if let Some(publication) = input.text("publication", 64)?
        && publication != "manual_per_repository"
    {
        return Err(Error::invalid(
            "cross-repository queue publication must be manual_per_repository",
        ));
    }
    let (q_state, active_limit, elapsed_limit, leased_at): (String, i64, i64, Option<i64>) = input
        .conn
        .query_row(
            "SELECT state, json_extract(body,'$.active_timeout_seconds'), json_extract(body,'$.elapsed_timeout_seconds'), json_extract(body,'$.leased_at')
             FROM work_queue WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .ok_or_else(Error::missing)?;
    if !matches!(q_state.as_str(), "leased" | "verifying" | "failed") {
        return Err(refuse(
            "invalid_state",
            format!("cannot verify queue in state {q_state}"),
        ));
    }
    let members = load_members(input.conn, id)?;
    if queue_expired(leased_at, elapsed_limit, active_limit, &members, now) {
        return Err(refuse(
            "queue_timed_out",
            "queue exceeded its active or elapsed timeout; run queue.sweep before finalizing",
        ));
    }
    if members
        .iter()
        .any(|m| matches!(m.state.as_str(), "queued" | "leased" | "running"))
    {
        return Err(refuse(
            "invalid_state",
            "all queue members must finish running before combined verification",
        ));
    }
    if input.kind("results")?.as_deref() != Some("array") {
        return Err(Error::invalid("results must be an array"));
    }

    struct VerifyRow {
        member_id: String,
        ok: bool,
        verification_json: String,
    }

    let by_id: HashMap<&str, &MemberRow> =
        members.iter().map(|m| (m.member_id.as_str(), m)).collect();
    let mut stmt = input
        .conn
        .prepare("SELECT type, value FROM json_each(?1,'$.results')")?;
    let mut rows = stmt.query([input.raw])?;
    let mut seen = HashSet::new();
    let mut results = Vec::new();
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(0)? != "object" {
            return Err(Error::invalid("results must contain objects"));
        }
        let raw_entry: String = row.get(1)?;
        let r_in = Input {
            conn: input.conn,
            raw: &raw_entry,
        };
        r_in.fields(&[
            "member_id",
            "ok",
            "head_oid",
            "command",
            "exit_code",
            "summary",
        ])?;
        let member_id = r_in.id("member_id")?;
        if !by_id.contains_key(member_id.as_str()) {
            return Err(Error::invalid(format!(
                "verification references unknown member {member_id}"
            )));
        }
        if !seen.insert(member_id.clone()) {
            return Err(Error::invalid(format!(
                "duplicate verification result for {member_id}"
            )));
        }
        if r_in.kind("ok")?.is_none() {
            return Err(Error::invalid("each verification result requires ok"));
        }
        let ok = r_in.boolean("ok", false)?;
        let head_oid = r_in.text("head_oid", 64)?;
        if head_oid.as_deref().is_some_and(|h| !lower_hex_oid(h)) {
            return Err(Error::invalid("head_oid must be a lowercase Git object ID"));
        }
        let command = r_in.text("command", 1024)?;
        let exit_code = if r_in.kind("exit_code")?.is_some() {
            Some(r_in.integer("exit_code", None, MAX_INTEGER)?)
        } else {
            None
        };
        let summary = r_in.text("summary", 4096)?.unwrap_or_default();
        let encoded: String = input.conn.query_row(
            "SELECT json_object('member_id',?1,'repository_id',?2,'ok',json(CASE WHEN ?3 THEN 'true' ELSE 'false' END),'head_oid',?4,'command',?5,'exit_code',?6,'summary',?7,'verified_at',?8)",
            params![
                member_id,
                by_id[member_id.as_str()].repository_id,
                ok,
                head_oid,
                command,
                exit_code,
                summary,
                now
            ],
            |r| r.get(0),
        )?;
        results.push(VerifyRow {
            member_id,
            ok,
            verification_json: encoded,
        });
    }
    for m in &members {
        if m.state == "passed" && !seen.contains(&m.member_id) {
            return Err(Error::invalid(format!(
                "combined verification is missing passed member {}",
                m.member_id
            )));
        }
    }
    for res in &results {
        input.conn.execute(
            "UPDATE work_queue_members SET verification_json=?3, lease_held=0 WHERE queue_id=?1 AND member_id=?2",
            params![id, res.member_id, res.verification_json],
        )?;
    }
    input.conn.execute(
        "UPDATE work_queue_members SET lease_held=0 WHERE queue_id=?1",
        [id],
    )?;

    let all_members_passed = members.iter().all(|m| m.state == "passed");
    let all_results_ok = !results.is_empty() && results.iter().all(|r| r.ok);
    let combined_ok = all_members_passed && all_results_ok;
    let final_state = if combined_ok { "passed" } else { "failed" };

    let per_member_json = format!(
        "[{}]",
        results
            .iter()
            .map(|r| r.verification_json.as_str())
            .collect::<Vec<_>>()
            .join(",")
    );
    let verification_doc: String = input.conn.query_row(
        "SELECT json_object('verified_at',?1,'ok',json(CASE WHEN ?2 THEN 'true' ELSE 'false' END),'publication','manual_per_repository','results',json(?3))",
        params![now, combined_ok, per_member_json],
        |r| r.get(0),
    )?;
    sync_queue_body(
        input.conn,
        id,
        revision,
        now,
        final_state,
        leased_at,
        Some(now),
        None,
        Some(&verification_doc),
    )
}

fn sweep_one(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&["request_id", "id", "expected_revision"])?;
    let (state, active_limit, elapsed_limit, leased_at): (String, i64, i64, Option<i64>) = input
        .conn
        .query_row(
            "SELECT state,
                    json_extract(body,'$.active_timeout_seconds'),
                    json_extract(body,'$.elapsed_timeout_seconds'),
                    json_extract(body,'$.leased_at')
             FROM work_queue WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .ok_or_else(Error::missing)?;
    if !matches!(state.as_str(), "leased" | "verifying") {
        return Err(refuse(
            "invalid_state",
            format!("cannot sweep queue in state {state}"),
        ));
    }
    let members = load_members(input.conn, id)?;
    if !queue_expired(leased_at, elapsed_limit, active_limit, &members, now) {
        return Err(refuse(
            "not_expired",
            "queue entry has not exceeded active or elapsed timeout",
        ));
    }
    apply_timeout(input.conn, id, revision, now)
}

fn cancel(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&["request_id", "id", "expected_revision", "reason"])?;
    let reason = input.text("reason", 1024)?.unwrap_or_default();
    let (state, leased_at): (String, Option<i64>) = input
        .conn
        .query_row(
            "SELECT state, json_extract(body,'$.leased_at') FROM work_queue WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(Error::missing)?;
    if !matches!(state.as_str(), "queued" | "leased" | "verifying") {
        return Err(refuse(
            "invalid_state",
            format!("cannot cancel queue in state {state}"),
        ));
    }
    let members = load_members(input.conn, id)?;
    for m in &members {
        if !is_terminal_member_state(&m.state) {
            let total_active = member_active_at(m, now);
            input.conn.execute(
                "UPDATE work_queue_members
                 SET state='cancelled', lease_held=0, active_seconds=?3, last_active_at=?4
                 WHERE queue_id=?1 AND member_id=?2",
                params![id, m.member_id, total_active, now],
            )?;
        } else if m.lease_held {
            input.conn.execute(
                "UPDATE work_queue_members SET lease_held=0 WHERE queue_id=?1 AND member_id=?2",
                params![id, m.member_id],
            )?;
        }
    }
    sync_queue_body(
        input.conn,
        id,
        revision,
        now,
        "cancelled",
        leased_at,
        Some(now),
        Some(&reason),
        None,
    )
}

pub(super) fn list(input: &Input<'_>) -> Result<String> {
    input.fields(&["limit", "cursor", "state"])?;
    let p = page(input)?;
    let state = input.text("state", 32)?;
    if let Some(s) = &state
        && !QUEUE_STATES.contains(&s.as_str())
    {
        return Err(Error::invalid("unknown queue state"));
    }
    let mut filters = Vec::new();
    let mut values: Vec<Value> = Vec::new();
    if let Some(s) = state {
        values.push(Value::Text(s));
        filters.push(format!("state=?{}", values.len()));
    }
    let where_clause = if filters.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", filters.join(" AND "))
    };
    let total: i64 = input.conn.query_row(
        &format!("SELECT count(*) FROM work_queue{where_clause}"),
        params_from_iter(values.iter()),
        |r| r.get(0),
    )?;
    let bound = values.len();
    let page_sql = format!(
        "SELECT body, created_at, id FROM work_queue{} {} (created_at, id)>(?{}, ?{}) ORDER BY created_at, id LIMIT ?{}",
        where_clause,
        if filters.is_empty() { "WHERE" } else { "AND" },
        bound + 1,
        bound + 2,
        bound + 3
    );
    values.extend([
        Value::Integer(p.position),
        Value::Text(p.id),
        Value::Integer(p.limit + 1),
    ]);
    let mut stmt = input.conn.prepare(&page_sql)?;
    let rows = collect_page(&mut stmt.query(params_from_iter(values.iter()))?, p.limit)?;
    page_response(input.conn, rows, total)
}
