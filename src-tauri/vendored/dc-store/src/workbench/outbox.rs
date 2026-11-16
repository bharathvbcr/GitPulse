//! Two-way GitHub issue synchronization and opt-in confirmed durable outbox.
//!
//! Inbound GitHub issue changes are observed against `last_seen` and stored
//! as explicit accept/dismiss proposals in `work_issue_sync` without silently
//! overwriting local tasks or violating `locked_fields`. Outbound writes are
//! permitted only after preview and explicit user confirmation (`confirmed = 1`),
//! recorded durably in `work_outbox` with an `idempotency_key` and canonical
//! `payload_digest`, transitioned to `outcome_unknown` across restart recovery,
//! and preflight-reconciled against remote state with 3-way conflict detection
//! before retrying.

use super::{
    Affected, Entity, Error, Input, MAX_INTEGER, Result, collect_page, page, page_response,
    put_body, record_change,
};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value};
use std::collections::HashSet;

const MAX_ATTEMPTS_CEILING: i64 = 10;
const DEFAULT_MAX_ATTEMPTS: i64 = 5;
const SYNC_FIELDS: &[&str] = &["title", "body", "state", "labels"];
const OUTBOX_STATES: &[&str] = &[
    "pending",
    "delivering",
    "outcome_unknown",
    "conflict",
    "delivered",
    "failed",
    "cancelled",
];

fn refuse(code: &'static str, message: impl Into<String>) -> Error {
    Error {
        code,
        message: message.into(),
    }
}

/// Pure-Rust FIPS 180-4 SHA-256 digest in lowercase hex (zero external crates).
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = data.to_vec();
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        let next = [a, b, c, d, e, f, g, hh];
        for (slot, delta) in h.iter_mut().zip(next) {
            *slot = slot.wrapping_add(delta);
        }
    }
    let mut hex = String::with_capacity(64);
    for word in h {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{word:08x}");
    }
    hex
}

fn validate_repo_slug(slug: &str) -> Result<()> {
    let valid_part = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    let Some((owner, repo)) = slug.split_once('/') else {
        return Err(Error::invalid("repo_slug must have owner/repo format"));
    };
    if repo.contains('/') || !valid_part(owner) || !valid_part(repo) {
        return Err(Error::invalid("repo_slug must have valid owner/repo format"));
    }
    Ok(())
}

#[derive(Clone, PartialEq, Eq)]
struct IssueSnapshot {
    title: String,
    body: String,
    state: String,
    labels: Vec<String>,
    updated_at: Option<String>,
}

impl IssueSnapshot {
    fn from_input(input: &Input<'_>) -> Result<Self> {
        input.fields(&["title", "body", "state", "labels", "updated_at"])?;
        let title = input.text("title", 4096)?.unwrap_or_default();
        let body = input.text("body", 65_536)?.unwrap_or_default();
        let raw_state = input.required_text("state", 16)?;
        let state = match raw_state.to_ascii_lowercase().as_str() {
            "open" => "open".to_string(),
            "closed" => "closed".to_string(),
            _ => {
                return Err(Error::invalid(
                    "GitHub issue state must be open or closed",
                ));
            }
        };
        let mut labels = input.strings("labels", 64, 128, false)?;
        labels.sort();
        labels.dedup();
        let updated_at = input.text("updated_at", 64)?;
        Ok(Self {
            title,
            body,
            state,
            labels,
            updated_at,
        })
    }

    fn from_json(conn: &Connection, raw: &str) -> Result<Self> {
        let input = Input { conn, raw };
        Self::from_input(&input)
    }

    fn from_task_body(conn: &Connection, task_body: &str) -> Result<Self> {
        let task_in = Input {
            conn,
            raw: task_body,
        };
        let title = task_in.required_text("title", 4096)?;
        let body = task_in.text("description", 65_536)?.unwrap_or_default();
        let status = task_in.required_text("status", 32)?;
        let state = if status == "done" { "closed" } else { "open" }.to_string();
        let mut labels = task_in.strings("labels", 64, 128, false)?;
        labels.sort();
        labels.dedup();
        Ok(Self {
            title,
            body,
            state,
            labels,
            updated_at: None,
        })
    }

    fn canonical_json(&self, conn: &Connection) -> Result<String> {
        let labels_json = encode_string_array(conn, &self.labels)?;
        Ok(conn.query_row(
            "SELECT json_object('state',?1,'title',?2,'body',?3,'labels',json(?4),'updated_at',?5)",
            params![self.state, self.title, self.body, labels_json, self.updated_at],
            |r| r.get(0),
        )?)
    }

    fn content_digest(&self, conn: &Connection) -> Result<String> {
        let labels_json = encode_string_array(conn, &self.labels)?;
        let canonical: String = conn.query_row(
            "SELECT json_object('state',?1,'title',?2,'body',?3,'labels',json(?4))",
            params![self.state, self.title, self.body, labels_json],
            |r| r.get(0),
        )?;
        Ok(sha256_hex(canonical.as_bytes()))
    }
}

fn encode_string_array(conn: &Connection, values: &[String]) -> Result<String> {
    let mut arr = "[]".to_string();
    for v in values {
        arr = conn.query_row(
            "SELECT json_insert(?1,'$[#]',?2)",
            params![arr, v],
            |r| r.get(0),
        )?;
    }
    Ok(arr)
}

fn task_locked_fields(conn: &Connection, task_body: &str) -> Result<Vec<String>> {
    let raw: String = conn.query_row(
        "SELECT coalesce(json_extract(?1,'$.locked_fields'),'[]')",
        [task_body],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare_cached("SELECT value FROM json_each(?1)")?;
    let mut rows = stmt.query([raw])?;
    let mut locked = Vec::new();
    while let Some(row) = rows.next()? {
        locked.push(row.get(0)?);
    }
    Ok(locked)
}

fn sync_field_locked(task_locked: &[String], remote_field: &str) -> bool {
    match remote_field {
        "title" => task_locked.iter().any(|f| f == "title"),
        "body" => task_locked.iter().any(|f| f == "description"),
        "state" => task_locked.iter().any(|f| f == "status"),
        "labels" => task_locked.iter().any(|f| f == "labels"),
        _ => false,
    }
}

fn snapshot_field_differs(a: &IssueSnapshot, b: &IssueSnapshot, field: &str) -> bool {
    match field {
        "state" => a.state != b.state,
        "title" => a.title != b.title,
        "body" => a.body != b.body,
        "labels" => a.labels != b.labels,
        _ => false,
    }
}

fn field_value_json(conn: &Connection, snap: &IssueSnapshot, field: &str) -> Result<String> {
    match field {
        "state" => Ok(conn.query_row("SELECT json_quote(?1)", [&snap.state], |r| r.get(0))?),
        "title" => Ok(conn.query_row("SELECT json_quote(?1)", [&snap.title], |r| r.get(0))?),
        "body" => Ok(conn.query_row("SELECT json_quote(?1)", [&snap.body], |r| r.get(0))?),
        "labels" => encode_string_array(conn, &snap.labels),
        _ => Err(Error::invalid("unknown sync field")),
    }
}

fn detect_conflicts(
    conn: &Connection,
    base: Option<&IssueSnapshot>,
    local: &IssueSnapshot,
    remote: &IssueSnapshot,
    fields: &[String],
) -> Result<(Vec<String>, String)> {
    let mut conflicting = Vec::new();
    let mut encoded = Vec::new();
    for field in fields {
        let is_conflict = match base {
            Some(base) => {
                snapshot_field_differs(local, base, field)
                    && snapshot_field_differs(remote, base, field)
                    && snapshot_field_differs(local, remote, field)
            }
            None => snapshot_field_differs(local, remote, field),
        };
        if is_conflict {
            conflicting.push(field.clone());
            let base_json = match base {
                Some(b) => field_value_json(conn, b, field)?,
                None => "null".to_string(),
            };
            let local_json = field_value_json(conn, local, field)?;
            let remote_json = field_value_json(conn, remote, field)?;
            let row: String = conn.query_row(
                "SELECT json_object('field',?1,'base',json(?2),'local',json(?3),'remote',json(?4))",
                params![field, base_json, local_json, remote_json],
                |r| r.get(0),
            )?;
            encoded.push(row);
        }
    }
    Ok((conflicting, format!("[{}]", encoded.join(","))))
}

fn normalize_sync_fields(input: &Input<'_>, key: &str, default_all: bool) -> Result<Vec<String>> {
    match input.kind(key)?.as_deref() {
        None | Some("null") if default_all => {
            Ok(SYNC_FIELDS.iter().map(|s| (*s).to_string()).collect())
        }
        None | Some("null") => Ok(Vec::new()),
        Some("array") => {
            let requested = input.strings(key, SYNC_FIELDS.len() + 1, 32, true)?;
            if requested.is_empty() {
                return Err(Error::invalid(format!("{key} must not be empty")));
            }
            let mut seen = HashSet::new();
            for f in &requested {
                if !SYNC_FIELDS.contains(&f.as_str()) {
                    return Err(Error::invalid(format!(
                        "unknown sync field {f}; expected title, body, state, or labels"
                    )));
                }
                if !seen.insert(f.as_str()) {
                    return Err(Error::invalid(format!("{key} contains duplicate field {f}")));
                }
            }
            Ok(requested)
        }
        _ => Err(Error::invalid(format!("{key} must be an array"))),
    }
}

fn require_live_task(conn: &Connection, task_id: &str) -> Result<(i64, String)> {
    conn.query_row(
        "SELECT revision, body FROM work_items WHERE id=?1 AND deleted=0",
        [task_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?
    .ok_or_else(|| Error::invalid(format!("linked task {task_id} does not exist or is deleted")))
}

fn load_sync_for_task(conn: &Connection, task_id: &str) -> Result<Option<(String, i64, String)>> {
    Ok(conn
        .query_row(
            "SELECT id, revision, body FROM work_issue_sync WHERE task_id=?1",
            [task_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?)
}

pub(super) fn mutate(
    input: &Input<'_>,
    entity: Entity,
    method: &str,
    id: &str,
    revision: i64,
    now: i64,
) -> Result<()> {
    match (entity, method) {
        (Entity::IssueSync, "github_sync.link") => sync_link(input, id, revision, now),
        (Entity::IssueSync, "github_sync.observe") => sync_observe(input, id, revision, now),
        (Entity::IssueSync, "github_sync.accept") => sync_accept(input, id, revision, now),
        (Entity::IssueSync, "github_sync.dismiss") => sync_dismiss(input, id, revision, now),
        (Entity::Outbox, "outbox.enqueue") => outbox_enqueue(input, id, revision, now),
        (Entity::Outbox, "outbox.claim") => outbox_claim(input, id, revision, now),
        (Entity::Outbox, "outbox.sweep_restart") => {
            outbox_mark_unknown(input, id, revision, now, "restarted while delivering")
        }
        (Entity::Outbox, "outbox.mark_unknown") => {
            outbox_mark_unknown(input, id, revision, now, "remote delivery outcome unknown")
        }
        (Entity::Outbox, "outbox.preflight") => outbox_preflight(input, id, revision, now),
        (Entity::Outbox, "outbox.complete") => outbox_complete(input, id, revision, now),
        (Entity::Outbox, "outbox.fail") => outbox_fail(input, id, revision, now),
        (Entity::Outbox, "outbox.cancel") => outbox_cancel(input, id, revision, now),
        _ => Err(Error {
            code: "unknown_method",
            message: format!("unknown sync/outbox method {method}"),
        }),
    }
}

fn sync_link(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&[
        "request_id",
        "id",
        "expected_revision",
        "task_id",
        "repo_slug",
        "issue_number",
        "issue_url",
        "outbound_opt_in",
        "last_seen",
    ])?;
    let task_id = input.id("task_id")?;
    let _ = require_live_task(input.conn, &task_id)?;
    let repo_slug = input.required_text("repo_slug", 256)?;
    validate_repo_slug(&repo_slug)?;
    let issue_number = input.integer("issue_number", None, MAX_INTEGER)?;
    if issue_number < 1 {
        return Err(Error::invalid("issue_number must be at least 1"));
    }
    let issue_url = input.text("issue_url", 2048)?;
    let outbound_opt_in = input.boolean("outbound_opt_in", false)?;
    let last_seen_raw: Option<String> = input.conn.query_row(
        "SELECT json_extract(?1,'$.last_seen')",
        [input.raw],
        |r| r.get(0),
    )?;
    let last_seen_json = match last_seen_raw {
        Some(raw) if raw != "null" => {
            let snap = IssueSnapshot::from_json(input.conn, &raw)?;
            Some(snap.canonical_json(input.conn)?)
        }
        _ => None,
    };

    if revision == 1 {
        let task_taken: bool = input.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_issue_sync WHERE task_id=?1)",
            [&task_id],
            |r| r.get(0),
        )?;
        if task_taken {
            return Err(refuse(
                "already_linked",
                format!("task {task_id} is already linked to a GitHub issue"),
            ));
        }
        let issue_taken: bool = input.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_issue_sync WHERE repo_slug=?1 AND issue_number=?2)",
            params![repo_slug, issue_number],
            |r| r.get(0),
        )?;
        if issue_taken {
            return Err(refuse(
                "already_linked",
                format!("{repo_slug}#{issue_number} is already linked to another task"),
            ));
        }
        let body: String = input.conn.query_row(
            "SELECT json_object('id',?1,'revision',?2,'task_id',?3,'repo_slug',?4,'issue_number',?5,'issue_url',?6,'outbound_opt_in',json(CASE WHEN ?7 THEN 'true' ELSE 'false' END),'last_seen',CASE WHEN ?8 IS NULL THEN NULL ELSE json(?8) END,'pending_proposal',NULL,'last_dismissed_digest',NULL,'created_at',?9,'updated_at',?9)",
            params![
                id,
                revision,
                task_id,
                repo_slug,
                issue_number,
                issue_url,
                outbound_opt_in,
                last_seen_json,
                now
            ],
            |r| r.get(0),
        )?;
        input.conn.execute(
            "INSERT INTO work_issue_sync(id,revision,body,task_id) VALUES(?1,?2,?3,?4)",
            params![id, revision, body, task_id],
        )?;
    } else {
        let prior: String = input
            .conn
            .query_row("SELECT body FROM work_issue_sync WHERE id=?1", [id], |r| r.get(0))
            .optional()?
            .ok_or_else(Error::missing)?;
        let prior_task: String = input.conn.query_row(
            "SELECT json_extract(?1,'$.task_id')",
            [&prior],
            |r| r.get(0),
        )?;
        if prior_task != task_id {
            return Err(Error::invalid("sync link task_id cannot change"));
        }
        let dup_issue: bool = input.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_issue_sync WHERE repo_slug=?1 AND issue_number=?2 AND id<>?3)",
            params![repo_slug, issue_number, id],
            |r| r.get(0),
        )?;
        if dup_issue {
            return Err(refuse(
                "already_linked",
                format!("{repo_slug}#{issue_number} is already linked to another task"),
            ));
        }
        let body: String = input.conn.query_row(
            "SELECT json_set(?1,'$.revision',?2,'$.repo_slug',?3,'$.issue_number',?4,'$.issue_url',coalesce(?5,json_extract(?1,'$.issue_url')),'$.outbound_opt_in',json(CASE WHEN ?6 THEN 'true' ELSE 'false' END),'$.last_seen',coalesce(CASE WHEN ?7 IS NULL THEN NULL ELSE json(?7) END,json_extract(?1,'$.last_seen')),'$.updated_at',?8)",
            params![
                prior,
                revision,
                repo_slug,
                issue_number,
                issue_url,
                outbound_opt_in,
                last_seen_json,
                now
            ],
            |r| r.get(0),
        )?;
        put_body(input.conn, Entity::IssueSync, id, revision, &body)?;
    }
    Ok(())
}

fn sync_observe(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&["request_id", "id", "expected_revision", "remote"])?;
    if input.kind("remote")?.as_deref() != Some("object") {
        return Err(Error::invalid("remote must be an issue snapshot object"));
    }
    let remote_raw: String = input.conn.query_row(
        "SELECT json_extract(?1,'$.remote')",
        [input.raw],
        |r| r.get(0),
    )?;
    let remote = IssueSnapshot::from_json(input.conn, &remote_raw)?;
    let prior: String = input
        .conn
        .query_row("SELECT body FROM work_issue_sync WHERE id=?1", [id], |r| r.get(0))
        .optional()?
        .ok_or_else(Error::missing)?;
    let (task_id, base_raw, dismissed): (String, Option<String>, Option<String>) = input.conn.query_row(
        "SELECT json_extract(?1,'$.task_id'), json_extract(?1,'$.last_seen'), json_extract(?1,'$.last_dismissed_digest')",
        [&prior],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let (_, task_body) = require_live_task(input.conn, &task_id)?;
    let local = IssueSnapshot::from_task_body(input.conn, &task_body)?;
    let base = match base_raw.as_deref() {
        Some(raw) if raw != "null" => Some(IssueSnapshot::from_json(input.conn, raw)?),
        _ => None,
    };
    let compare_ref = base.as_ref().unwrap_or(&local);
    let mut changed_fields = Vec::new();
    let mut locked_skipped = Vec::new();
    let mut applicable_fields = Vec::new();
    let locked = task_locked_fields(input.conn, &task_body)?;
    for field in SYNC_FIELDS {
        if snapshot_field_differs(compare_ref, &remote, field)
            || snapshot_field_differs(&local, &remote, field)
        {
            changed_fields.push((*field).to_string());
            if sync_field_locked(&locked, field) {
                locked_skipped.push((*field).to_string());
            } else {
                applicable_fields.push((*field).to_string());
            }
        }
    }
    let remote_digest = remote.content_digest(input.conn)?;
    let all_fields: Vec<String> = SYNC_FIELDS.iter().map(|s| (*s).to_string()).collect();
    let (conflict_fields, conflicts_json) =
        detect_conflicts(input.conn, base.as_ref(), &local, &remote, &all_fields)?;

    let proposal_json: Option<String> = if changed_fields.is_empty()
        || dismissed.as_deref() == Some(remote_digest.as_str())
    {
        None
    } else {
        let remote_canon = remote.canonical_json(input.conn)?;
        let changed_arr = encode_string_array(input.conn, &changed_fields)?;
        let locked_arr = encode_string_array(input.conn, &locked_skipped)?;
        let applicable_arr = encode_string_array(input.conn, &applicable_fields)?;
        let conflict_arr = encode_string_array(input.conn, &conflict_fields)?;
        Some(input.conn.query_row(
            "SELECT json_object('observed_at',?1,'remote',json(?2),'remote_digest',?3,'changed_fields',json(?4),'locked_skipped',json(?5),'applicable_fields',json(?6),'conflicting_fields',json(?7),'conflicts',json(?8))",
            params![
                now,
                remote_canon,
                remote_digest,
                changed_arr,
                locked_arr,
                applicable_arr,
                conflict_arr,
                conflicts_json
            ],
            |r| r.get(0),
        )?)
    };

    let body: String = input.conn.query_row(
        "SELECT json_set(?1,'$.revision',?2,'$.pending_proposal',CASE WHEN ?3 IS NULL THEN NULL ELSE json(?3) END,'$.updated_at',?4)",
        params![prior, revision, proposal_json, now],
        |r| r.get(0),
    )?;
    put_body(input.conn, Entity::IssueSync, id, revision, &body)
}

fn sync_accept(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&[
        "request_id",
        "id",
        "expected_revision",
        "fields",
        "expected_task_revision",
    ])?;
    let prior: String = input
        .conn
        .query_row("SELECT body FROM work_issue_sync WHERE id=?1", [id], |r| r.get(0))
        .optional()?
        .ok_or_else(Error::missing)?;
    let (task_id, proposal_raw): (String, Option<String>) = input.conn.query_row(
        "SELECT json_extract(?1,'$.task_id'), json_extract(?1,'$.pending_proposal')",
        [&prior],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let Some(proposal_raw) = proposal_raw.filter(|s| s != "null") else {
        return Err(refuse(
            "invalid_state",
            "no pending GitHub inbound proposal to accept",
        ));
    };
    let (task_rev, mut task_body) = require_live_task(input.conn, &task_id)?;
    if input.kind("expected_task_revision")?.is_some()
        && input.integer("expected_task_revision", None, MAX_INTEGER)? != task_rev
    {
        return Err(refuse(
            "revision_conflict",
            "the task changed before inbound GitHub proposal acceptance",
        ));
    }
    let remote_raw: String = input.conn.query_row(
        "SELECT json_extract(?1,'$.remote')",
        [&proposal_raw],
        |r| r.get(0),
    )?;
    let remote = IssueSnapshot::from_json(input.conn, &remote_raw)?;
    let locked = task_locked_fields(input.conn, &task_body)?;
    let explicit_fields = normalize_sync_fields(input, "fields", false)?;
    let chosen_fields = if explicit_fields.is_empty() {
        SYNC_FIELDS
            .iter()
            .filter(|f| !sync_field_locked(&locked, f))
            .map(|f| (*f).to_string())
            .collect::<Vec<_>>()
    } else {
        for f in &explicit_fields {
            if sync_field_locked(&locked, f) {
                return Err(refuse(
                    "field_locked",
                    format!("cannot accept inbound change to locked field {f}"),
                ));
            }
        }
        explicit_fields
    };

    for field in &chosen_fields {
        match field.as_str() {
            "title" => {
                if remote.title.trim().is_empty() || remote.title.chars().count() > 300 {
                    return Err(Error::invalid(
                        "inbound title must be nonblank and at most 300 characters",
                    ));
                }
                task_body = input.conn.query_row(
                    "SELECT json_set(?1,'$.title',?2)",
                    params![task_body, remote.title],
                    |r| r.get(0),
                )?;
            }
            "body" => {
                task_body = input.conn.query_row(
                    "SELECT json_set(?1,'$.description',?2)",
                    params![task_body, remote.body],
                    |r| r.get(0),
                )?;
            }
            "state" => {
                let current_status: String = input.conn.query_row(
                    "SELECT json_extract(?1,'$.status')",
                    [&task_body],
                    |r| r.get(0),
                )?;
                let next_status = if remote.state == "closed" {
                    "done"
                } else if current_status == "done" {
                    "ready"
                } else {
                    current_status.as_str()
                };
                task_body = input.conn.query_row(
                    "SELECT json_set(?1,'$.status',?2,'$.completed_at',CASE WHEN ?2='done' THEN coalesce(json_extract(?1,'$.completed_at'),?3) ELSE NULL END)",
                    params![task_body, next_status, now],
                    |r| r.get(0),
                )?;
            }
            "labels" => {
                let labels_arr = encode_string_array(input.conn, &remote.labels)?;
                task_body = input.conn.query_row(
                    "SELECT json_set(?1,'$.labels',json(?2))",
                    params![task_body, labels_arr],
                    |r| r.get(0),
                )?;
            }
            _ => {}
        }
    }

    let next_task_rev = task_rev + 1;
    input.conn.execute(
        "UPDATE work_items SET revision=?2, body=json_set(?3,'$.revision',?2,'$.updated_at',?4) WHERE id=?1",
        params![task_id, next_task_rev, task_body, now],
    )?;
    record_change(
        input.conn,
        Entity::Item,
        &task_id,
        next_task_rev,
        "items.github_sync_accepted",
        now,
        &Affected::none(),
    )?;

    let remote_canon = remote.canonical_json(input.conn)?;
    let next_body: String = input.conn.query_row(
        "SELECT json_set(?1,'$.revision',?2,'$.last_seen',json(?3),'$.pending_proposal',NULL,'$.last_dismissed_digest',NULL,'$.updated_at',?4)",
        params![prior, revision, remote_canon, now],
        |r| r.get(0),
    )?;
    put_body(input.conn, Entity::IssueSync, id, revision, &next_body)
}

fn sync_dismiss(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&["request_id", "id", "expected_revision"])?;
    let prior: String = input
        .conn
        .query_row("SELECT body FROM work_issue_sync WHERE id=?1", [id], |r| r.get(0))
        .optional()?
        .ok_or_else(Error::missing)?;
    let (proposal_raw, digest): (Option<String>, Option<String>) = input.conn.query_row(
        "SELECT json_extract(?1,'$.pending_proposal'), json_extract(?1,'$.pending_proposal.remote_digest')",
        [&prior],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if proposal_raw.filter(|s| s != "null").is_none() {
        return Err(refuse(
            "invalid_state",
            "no pending GitHub inbound proposal to dismiss",
        ));
    }
    let next_body: String = input.conn.query_row(
        "SELECT json_set(?1,'$.revision',?2,'$.pending_proposal',NULL,'$.last_dismissed_digest',?3,'$.updated_at',?4)",
        params![prior, revision, digest, now],
        |r| r.get(0),
    )?;
    put_body(input.conn, Entity::IssueSync, id, revision, &next_body)
}

fn build_outbound_payload(
    conn: &Connection,
    task_id: &str,
    op: &str,
    repo_slug_input: Option<String>,
    fields_input: Vec<String>,
    remote_override: Option<&IssueSnapshot>,
) -> Result<(
    Option<String>,
    bool,
    String,
    Option<i64>,
    Vec<String>,
    String,
    String,
    Vec<String>,
    String,
)> {
    let (_, task_body) = require_live_task(conn, task_id)?;
    let local = IssueSnapshot::from_task_body(conn, &task_body)?;
    let existing_sync = load_sync_for_task(conn, task_id)?;
    if op == "create_issue" {
        if existing_sync.is_some() {
            return Err(refuse(
                "already_linked",
                format!("task {task_id} already has a linked GitHub issue"),
            ));
        }
        let repo_slug = repo_slug_input
            .ok_or_else(|| Error::invalid("repo_slug is required for create_issue"))?;
        validate_repo_slug(&repo_slug)?;
        let selected: Vec<String> = if fields_input.is_empty() {
            SYNC_FIELDS.iter().map(|s| (*s).to_string()).collect()
        } else {
            fields_input
        };
        let labels_json = encode_string_array(conn, &local.labels)?;
        let fields_json = encode_string_array(conn, &selected)?;
        let payload_json: String = conn.query_row(
            "SELECT json_object('op','create_issue','task_id',?1,'repo_slug',?2,'issue_number',NULL,'fields',json(?3),'title',?4,'body',?5,'state',?6,'labels',json(?7))",
            params![
                task_id,
                repo_slug,
                fields_json,
                local.title,
                local.body,
                local.state,
                labels_json
            ],
            |r| r.get(0),
        )?;
        let digest = sha256_hex(payload_json.as_bytes());
        return Ok((
            None,
            true,
            repo_slug,
            None,
            selected,
            payload_json,
            digest,
            Vec::new(),
            "[]".into(),
        ));
    }

    // update_issue
    let Some((sync_id, _, sync_body)) = existing_sync else {
        return Err(refuse(
            "not_linked",
            format!("task {task_id} is not linked to a GitHub issue"),
        ));
    };
    let (link_slug, issue_number, opt_in, base_raw, pending_remote_raw): (
        String,
        i64,
        bool,
        Option<String>,
        Option<String>,
    ) = conn.query_row(
        "SELECT json_extract(?1,'$.repo_slug'), json_extract(?1,'$.issue_number'), json_extract(?1,'$.outbound_opt_in'), json_extract(?1,'$.last_seen'), json_extract(?1,'$.pending_proposal.remote')",
        [&sync_body],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    if let Some(supplied) = repo_slug_input {
        validate_repo_slug(&supplied)?;
        if supplied != link_slug {
            return Err(Error::invalid(
                "repo_slug does not match the task's linked GitHub issue",
            ));
        }
    }
    let selected = if fields_input.is_empty() {
        SYNC_FIELDS.iter().map(|s| (*s).to_string()).collect()
    } else {
        fields_input
    };
    let base = match base_raw.as_deref() {
        Some(raw) if raw != "null" => Some(IssueSnapshot::from_json(conn, raw)?),
        _ => None,
    };
    let remote = match remote_override {
        Some(r) => Some(r.clone()),
        None => match pending_remote_raw.as_deref() {
            Some(raw) if raw != "null" => Some(IssueSnapshot::from_json(conn, raw)?),
            _ => None,
        },
    };
    let (conflict_fields, conflicts_json) = if let Some(remote_snap) = remote.as_ref() {
        detect_conflicts(conn, base.as_ref(), &local, remote_snap, &selected)?
    } else {
        (Vec::new(), "[]".into())
    };
    let labels_json = encode_string_array(conn, &local.labels)?;
    let fields_json = encode_string_array(conn, &selected)?;
    let payload_json: String = conn.query_row(
        "SELECT json_object('op','update_issue','task_id',?1,'repo_slug',?2,'issue_number',?3,'fields',json(?4),'title',?5,'body',?6,'state',?7,'labels',json(?8))",
        params![
            task_id,
            link_slug,
            issue_number,
            fields_json,
            local.title,
            local.body,
            local.state,
            labels_json
        ],
        |r| r.get(0),
    )?;
    let digest = sha256_hex(payload_json.as_bytes());
    Ok((
        Some(sync_id),
        opt_in,
        link_slug,
        Some(issue_number),
        selected,
        payload_json,
        digest,
        conflict_fields,
        conflicts_json,
    ))
}

/// Pure read-only preview of an outbound GitHub issue operation. Writes nothing.
pub(super) fn preview(input: &Input<'_>) -> Result<String> {
    input.fields(&["task_id", "op", "repo_slug", "fields", "remote"])?;
    let task_id = input.id("task_id")?;
    let op = input.required_text("op", 32)?;
    if !matches!(op.as_str(), "create_issue" | "update_issue") {
        return Err(Error::invalid("op must be create_issue or update_issue"));
    }
    let repo_slug = input.text("repo_slug", 256)?;
    let fields = normalize_sync_fields(input, "fields", true)?;
    let remote_override = match input.kind("remote")?.as_deref() {
        None | Some("null") => None,
        Some("object") => {
            let raw: String = input.conn.query_row(
                "SELECT json_extract(?1,'$.remote')",
                [input.raw],
                |r| r.get(0),
            )?;
            Some(IssueSnapshot::from_json(input.conn, &raw)?)
        }
        _ => return Err(Error::invalid("remote must be an object")),
    };

    let (
        sync_id,
        outbound_opt_in,
        resolved_slug,
        issue_number,
        _,
        payload_json,
        digest,
        conflict_fields,
        conflicts_json,
    ) = build_outbound_payload(
        input.conn,
        &task_id,
        &op,
        repo_slug,
        fields,
        remote_override.as_ref(),
    )?;
    let can_enqueue = outbound_opt_in && conflict_fields.is_empty();
    Ok(input.conn.query_row(
        "SELECT json_object('ok',json('true'),'preview',json_object('sync_id',?1,'op',?2,'repo_slug',?3,'issue_number',?4,'outbound_opt_in',json(CASE WHEN ?5 THEN 'true' ELSE 'false' END),'can_enqueue',json(CASE WHEN ?6 THEN 'true' ELSE 'false' END),'payload',json(?7),'payload_digest',?8,'conflicts',json(?9)))",
        params![
            sync_id,
            op,
            resolved_slug,
            issue_number,
            outbound_opt_in,
            can_enqueue,
            payload_json,
            digest,
            conflicts_json
        ],
        |r| r.get(0),
    )?)
}

fn outbox_enqueue(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&[
        "request_id",
        "id",
        "expected_revision",
        "task_id",
        "op",
        "repo_slug",
        "fields",
        "confirmed",
        "confirm_payload_digest",
        "idempotency_key",
        "outbound_opt_in",
        "max_attempts",
    ])?;
    if revision != 1 {
        return Err(refuse(
            "invalid_state",
            "outbox entries are created once at revision 0",
        ));
    }
    if input.kind("confirmed")?.as_deref() != Some("true") {
        return Err(refuse(
            "confirmation_required",
            "outbound GitHub writes require explicit user confirmation (confirmed=true)",
        ));
    }
    let task_id = input.id("task_id")?;
    let op = input.required_text("op", 32)?;
    if !matches!(op.as_str(), "create_issue" | "update_issue") {
        return Err(Error::invalid("op must be create_issue or update_issue"));
    }
    let repo_slug = input.text("repo_slug", 256)?;
    let fields = normalize_sync_fields(input, "fields", true)?;
    let confirm_digest = input.text("confirm_payload_digest", 64)?;
    let idempotency_key = input.required_text("idempotency_key", 128)?;
    if idempotency_key.len() < 8
        || !idempotency_key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'.'))
    {
        return Err(Error::invalid(
            "idempotency_key must be 8..=128 ASCII identifier characters",
        ));
    }
    let create_opt_in = input.boolean("outbound_opt_in", false)?;
    let max_attempts = input.integer("max_attempts", Some(DEFAULT_MAX_ATTEMPTS), MAX_ATTEMPTS_CEILING)?;
    if max_attempts < 1 {
        return Err(Error::invalid("max_attempts must be at least 1"));
    }

    let (
        sync_id,
        link_opt_in,
        resolved_slug,
        issue_number,
        _,
        payload_json,
        digest,
        conflict_fields,
        _,
    ) = build_outbound_payload(input.conn, &task_id, &op, repo_slug, fields, None)?;

    if op == "update_issue" && !link_opt_in {
        return Err(refuse(
            "outbound_disabled",
            "outbound GitHub sync is disabled on this link; opt in explicitly before enqueueing updates",
        ));
    }
    if let Some(expected) = confirm_digest {
        if expected != digest {
            return Err(refuse(
                "payload_changed",
                "the task changed after preview; re-preview and re-confirm before sending",
            ));
        }
    }
    if !conflict_fields.is_empty() {
        return Err(refuse(
            "sync_conflict",
            format!(
                "task conflicts with pending inbound GitHub changes on {}; resolve before pushing",
                conflict_fields.join(", ")
            ),
        ));
    }

    if let Some((existing_id, existing_digest)) = input
        .conn
        .query_row(
            "SELECT id, payload_digest FROM work_outbox WHERE idempotency_key=?1",
            [&idempotency_key],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?
    {
        if existing_digest != digest {
            return Err(refuse(
                "idempotency_conflict",
                "idempotency_key was already used with a different outbound payload",
            ));
        }
        return Err(refuse(
            "duplicate_outbox_entry",
            format!("outbox entry {existing_id} already recorded this idempotent operation"),
        ));
    }

    let effective_opt_in = if op == "create_issue" {
        create_opt_in
    } else {
        true
    };
    let body: String = input.conn.query_row(
        "SELECT json_object('id',?1,'revision',?2,'sync_id',?3,'task_id',?4,'op',?5,'repo_slug',?6,'issue_number',?7,'state','pending','confirmed',json('true'),'outbound_opt_in',json(CASE WHEN ?8 THEN 'true' ELSE 'false' END),'idempotency_key',?9,'payload_digest',?10,'payload',json(?11),'attempts',0,'max_attempts',?12,'next_attempt_at',?13,'created_at',?13,'updated_at',?13,'claimed_at',NULL,'delivered_at',NULL,'last_preflight_at',NULL,'last_error',NULL,'conflict',NULL)",
        params![
            id,
            revision,
            sync_id,
            task_id,
            op,
            resolved_slug,
            issue_number,
            effective_opt_in,
            idempotency_key,
            digest,
            payload_json,
            max_attempts,
            now
        ],
        |r| r.get(0),
    )?;
    input.conn.execute(
        "INSERT INTO work_outbox(id,revision,body,sync_id,task_id,idempotency_key,payload_digest,confirmed)
         VALUES(?1,?2,?3,?4,?5,?6,?7,1)",
        params![
            id,
            revision,
            body,
            sync_id,
            task_id,
            idempotency_key,
            digest
        ],
    )?;
    Ok(())
}

fn load_outbox_body(conn: &Connection, id: &str) -> Result<String> {
    conn.query_row("SELECT body FROM work_outbox WHERE id=?1", [id], |r| r.get(0))
        .optional()?
        .ok_or_else(Error::missing)
}

fn update_outbox_body(
    conn: &Connection,
    id: &str,
    revision: i64,
    body: &str,
    sync_id: Option<&str>,
) -> Result<()> {
    let changed = conn.execute(
        "UPDATE work_outbox SET revision=?2, body=?3, sync_id=coalesce(?4, sync_id) WHERE id=?1",
        params![id, revision, body, sync_id],
    )?;
    if changed == 0 {
        return Err(Error::missing());
    }
    Ok(())
}

fn outbox_claim(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&["request_id", "id", "expected_revision"])?;
    let prior = load_outbox_body(input.conn, id)?;
    let (state, next_at, attempts, max_attempts): (String, i64, i64, i64) = input.conn.query_row(
        "SELECT json_extract(?1,'$.state'), json_extract(?1,'$.next_attempt_at'), json_extract(?1,'$.attempts'), json_extract(?1,'$.max_attempts')",
        [&prior],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    if state != "pending" {
        return Err(refuse(
            "invalid_state",
            format!("cannot claim outbox entry in state {state}; preflight or resolve conflicts first"),
        ));
    }
    if now < next_at {
        return Err(refuse("not_due", "outbox entry retry backoff has not elapsed"));
    }
    if attempts >= max_attempts {
        return Err(refuse("invalid_state", "outbox entry exhausted max_attempts"));
    }
    let next_body: String = input.conn.query_row(
        "SELECT json_set(?1,'$.revision',?2,'$.state','delivering','$.attempts',?3,'$.claimed_at',?4,'$.updated_at',?4)",
        params![prior, revision, attempts + 1, now],
        |r| r.get(0),
    )?;
    update_outbox_body(input.conn, id, revision, &next_body, None)
}

fn outbox_mark_unknown(
    input: &Input<'_>,
    id: &str,
    revision: i64,
    now: i64,
    default_reason: &str,
) -> Result<()> {
    input.fields(&["request_id", "id", "expected_revision", "reason"])?;
    let reason = input
        .text("reason", 2048)?
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default_reason.to_string());
    let prior = load_outbox_body(input.conn, id)?;
    let state: String = input.conn.query_row(
        "SELECT json_extract(?1,'$.state')",
        [&prior],
        |r| r.get(0),
    )?;
    if state != "delivering" {
        return Err(refuse(
            "invalid_state",
            format!("only delivering outbox entries transition to outcome_unknown (got {state})"),
        ));
    }
    let next_body: String = input.conn.query_row(
        "SELECT json_set(?1,'$.revision',?2,'$.state','outcome_unknown','$.last_error',?3,'$.updated_at',?4)",
        params![prior, revision, reason, now],
        |r| r.get(0),
    )?;
    update_outbox_body(input.conn, id, revision, &next_body, None)
}

fn upsert_sync_from_delivery(
    conn: &Connection,
    outbox_id: &str,
    prior_outbox: &str,
    issue_number: i64,
    issue_url: Option<&str>,
    remote: &IssueSnapshot,
    now: i64,
) -> Result<String> {
    let (sync_id, task_id, repo_slug, opt_in): (Option<String>, String, String, bool) = conn.query_row(
        "SELECT json_extract(?1,'$.sync_id'), json_extract(?1,'$.task_id'), json_extract(?1,'$.repo_slug'), json_extract(?1,'$.outbound_opt_in')",
        [prior_outbox],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let resolved_sync_id = sync_id.or_else(|| {
        conn.query_row(
            "SELECT id FROM work_issue_sync WHERE task_id=?1",
            [&task_id],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
    });
    let remote_canon = remote.canonical_json(conn)?;
    if let Some(sid) = resolved_sync_id {
        let (s_rev, s_body): (i64, String) = conn.query_row(
            "SELECT revision, body FROM work_issue_sync WHERE id=?1",
            [&sid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let next_rev = s_rev + 1;
        let next_s_body: String = conn.query_row(
            "SELECT json_set(?1,'$.revision',?2,'$.issue_number',?3,'$.issue_url',coalesce(?4,json_extract(?1,'$.issue_url')),'$.last_seen',json(?5),'$.pending_proposal',NULL,'$.last_dismissed_digest',NULL,'$.updated_at',?6)",
            params![s_body, next_rev, issue_number, issue_url, remote_canon, now],
            |r| r.get(0),
        )?;
        put_body(conn, Entity::IssueSync, &sid, next_rev, &next_s_body)?;
        record_change(
            conn,
            Entity::IssueSync,
            &sid,
            next_rev,
            "github_sync.delivered",
            now,
            &Affected::none(),
        )?;
        return Ok(sid);
    }
    let sid = format!("ghsync-{outbox_id}");
    let next_s_body: String = conn.query_row(
        "SELECT json_object('id',?1,'revision',1,'task_id',?2,'repo_slug',?3,'issue_number',?4,'issue_url',?5,'outbound_opt_in',json(CASE WHEN ?6 THEN 'true' ELSE 'false' END),'last_seen',json(?7),'pending_proposal',NULL,'last_dismissed_digest',NULL,'created_at',?8,'updated_at',?8)",
        params![
            sid,
            task_id,
            repo_slug,
            issue_number,
            issue_url,
            opt_in,
            remote_canon,
            now
        ],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO work_issue_sync(id,revision,body,task_id) VALUES(?1,1,?2,?3)",
        params![sid, next_s_body, task_id],
    )?;
    record_change(
        conn,
        Entity::IssueSync,
        &sid,
        1,
        "github_sync.linked_from_outbox",
        now,
        &Affected::none(),
    )?;
    Ok(sid)
}

fn outbox_preflight(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&[
        "request_id",
        "id",
        "expected_revision",
        "matched_remote",
        "issue_number",
        "issue_url",
        "remote",
    ])?;
    let prior = load_outbox_body(input.conn, id)?;
    let (state, op, task_id, sync_id, payload_raw): (String, String, String, Option<String>, String) =
        input.conn.query_row(
            "SELECT json_extract(?1,'$.state'), json_extract(?1,'$.op'), json_extract(?1,'$.task_id'), json_extract(?1,'$.sync_id'), json_extract(?1,'$.payload')",
            [&prior],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
    if !matches!(
        state.as_str(),
        "pending" | "delivering" | "outcome_unknown" | "conflict"
    ) {
        return Err(refuse(
            "invalid_state",
            format!("cannot preflight outbox entry in state {state}"),
        ));
    }

    let matched_remote = input.boolean("matched_remote", false)?;
    let issue_url = input.text("issue_url", 2048)?;
    let remote_snap = match input.kind("remote")?.as_deref() {
        None | Some("null") => None,
        Some("object") => {
            let raw: String = input.conn.query_row(
                "SELECT json_extract(?1,'$.remote')",
                [input.raw],
                |r| r.get(0),
            )?;
            Some(IssueSnapshot::from_json(input.conn, &raw)?)
        }
        _ => return Err(Error::invalid("remote must be an object")),
    };
    let payload_snap = {
        let trimmed: String = input.conn.query_row(
            "SELECT json_object('title',json_extract(?1,'$.title'),'body',json_extract(?1,'$.body'),'state',json_extract(?1,'$.state'),'labels',json_extract(?1,'$.labels'))",
            [&payload_raw],
            |r| r.get(0),
        )?;
        IssueSnapshot::from_json(input.conn, &trimmed)?
    };
    let selected_fields: Vec<String> = {
        let fields_raw: String = input.conn.query_row(
            "SELECT json_extract(?1,'$.fields')",
            [&payload_raw],
            |r| r.get(0),
        )?;
        let mut stmt = input.conn.prepare_cached("SELECT value FROM json_each(?1)")?;
        let mut rows = stmt.query([fields_raw])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(r.get(0)?);
        }
        out
    };

    if op == "create_issue" && state == "outcome_unknown" {
        if matched_remote {
            let issue_number = input.integer("issue_number", None, MAX_INTEGER)?;
            if issue_number < 1 {
                return Err(Error::invalid("issue_number must be at least 1 when matched_remote=true"));
            }
            let snap = remote_snap.as_ref().unwrap_or(&payload_snap);
            let sid = upsert_sync_from_delivery(
                input.conn,
                id,
                &prior,
                issue_number,
                issue_url.as_deref(),
                snap,
                now,
            )?;
            let next_body: String = input.conn.query_row(
                "SELECT json_set(?1,'$.revision',?2,'$.sync_id',?3,'$.issue_number',?4,'$.state','delivered','$.delivered_at',?5,'$.last_preflight_at',?5,'$.last_error',NULL,'$.conflict',NULL,'$.updated_at',?5)",
                params![prior, revision, sid, issue_number, now],
                |r| r.get(0),
            )?;
            return update_outbox_body(input.conn, id, revision, &next_body, Some(&sid));
        }
        let next_body: String = input.conn.query_row(
            "SELECT json_set(?1,'$.revision',?2,'$.state','pending','$.next_attempt_at',?3,'$.last_preflight_at',?3,'$.updated_at',?3)",
            params![prior, revision, now],
            |r| r.get(0),
        )?;
        return update_outbox_body(input.conn, id, revision, &next_body, None);
    }

    if let Some(remote_snap) = &remote_snap {
        let base_snap = match sync_id
            .as_deref()
            .and_then(|sid| {
                input
                    .conn
                    .query_row(
                        "SELECT json_extract(body,'$.last_seen') FROM work_issue_sync WHERE id=?1",
                        [sid],
                        |r| r.get::<_, Option<String>>(0),
                    )
                    .ok()
                    .flatten()
            })
            .or_else(|| {
                load_sync_for_task(input.conn, &task_id)
                    .ok()
                    .flatten()
                    .and_then(|(_, _, b)| {
                        input
                            .conn
                            .query_row("SELECT json_extract(?1,'$.last_seen')", [&b], |r| {
                                r.get::<_, Option<String>>(0)
                            })
                            .ok()
                            .flatten()
                    })
            })
            .filter(|s| s != "null")
        {
            Some(raw) => Some(IssueSnapshot::from_json(input.conn, &raw)?),
            None => None,
        };

        let all_payload_match = selected_fields
            .iter()
            .all(|f| !snapshot_field_differs(&payload_snap, remote_snap, f));
        if state == "outcome_unknown" && all_payload_match {
            let issue_number: i64 = input.conn.query_row(
                "SELECT json_extract(?1,'$.issue_number')",
                [&prior],
                |r| r.get(0),
            )?;
            let sid = upsert_sync_from_delivery(
                input.conn,
                id,
                &prior,
                issue_number,
                issue_url.as_deref(),
                remote_snap,
                now,
            )?;
            let next_body: String = input.conn.query_row(
                "SELECT json_set(?1,'$.revision',?2,'$.sync_id',?3,'$.state','delivered','$.delivered_at',?4,'$.last_preflight_at',?4,'$.last_error',NULL,'$.conflict',NULL,'$.updated_at',?4)",
                params![prior, revision, sid, now],
                |r| r.get(0),
            )?;
            return update_outbox_body(input.conn, id, revision, &next_body, Some(&sid));
        }

        let (conflict_fields, conflicts_json) = detect_conflicts(
            input.conn,
            base_snap.as_ref(),
            &payload_snap,
            remote_snap,
            &selected_fields,
        )?;
        if !conflict_fields.is_empty() {
            let remote_canon = remote_snap.canonical_json(input.conn)?;
            let c_arr = encode_string_array(input.conn, &conflict_fields)?;
            let conflict_obj: String = input.conn.query_row(
                "SELECT json_object('detected_at',?1,'fields',json(?2),'details',json(?3),'remote',json(?4))",
                params![now, c_arr, conflicts_json, remote_canon],
                |r| r.get(0),
            )?;
            let next_body: String = input.conn.query_row(
                "SELECT json_set(?1,'$.revision',?2,'$.state','conflict','$.conflict',json(?3),'$.last_preflight_at',?4,'$.updated_at',?4)",
                params![prior, revision, conflict_obj, now],
                |r| r.get(0),
            )?;
            return update_outbox_body(input.conn, id, revision, &next_body, None);
        }
    } else if state == "outcome_unknown" {
        return Err(Error::invalid(
            "preflighting an outcome_unknown entry requires remote state or matched_remote",
        ));
    }

    let next_body: String = input.conn.query_row(
        "SELECT json_set(?1,'$.revision',?2,'$.state','pending','$.conflict',NULL,'$.next_attempt_at',?3,'$.last_preflight_at',?3,'$.updated_at',?3)",
        params![prior, revision, now],
        |r| r.get(0),
    )?;
    update_outbox_body(input.conn, id, revision, &next_body, None)
}

fn outbox_complete(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&[
        "request_id",
        "id",
        "expected_revision",
        "issue_number",
        "issue_url",
        "remote",
    ])?;
    let prior = load_outbox_body(input.conn, id)?;
    let (state, payload_raw): (String, String) = input.conn.query_row(
        "SELECT json_extract(?1,'$.state'), json_extract(?1,'$.payload')",
        [&prior],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if !matches!(state.as_str(), "delivering" | "outcome_unknown") {
        return Err(refuse(
            "invalid_state",
            format!("cannot complete outbox entry in state {state}"),
        ));
    }
    let issue_number = input.integer("issue_number", None, MAX_INTEGER)?;
    if issue_number < 1 {
        return Err(Error::invalid("issue_number must be at least 1"));
    }
    let issue_url = input.text("issue_url", 2048)?;
    let remote = match input.kind("remote")?.as_deref() {
        Some("object") => {
            let raw: String = input.conn.query_row(
                "SELECT json_extract(?1,'$.remote')",
                [input.raw],
                |r| r.get(0),
            )?;
            IssueSnapshot::from_json(input.conn, &raw)?
        }
        None | Some("null") => {
            let trimmed: String = input.conn.query_row(
                "SELECT json_object('title',json_extract(?1,'$.title'),'body',json_extract(?1,'$.body'),'state',json_extract(?1,'$.state'),'labels',json_extract(?1,'$.labels'))",
                [&payload_raw],
                |r| r.get(0),
            )?;
            IssueSnapshot::from_json(input.conn, &trimmed)?
        }
        _ => return Err(Error::invalid("remote must be an object")),
    };
    let sid = upsert_sync_from_delivery(
        input.conn,
        id,
        &prior,
        issue_number,
        issue_url.as_deref(),
        &remote,
        now,
    )?;
    let next_body: String = input.conn.query_row(
        "SELECT json_set(?1,'$.revision',?2,'$.sync_id',?3,'$.issue_number',?4,'$.state','delivered','$.delivered_at',?5,'$.last_error',NULL,'$.conflict',NULL,'$.updated_at',?5)",
        params![prior, revision, sid, issue_number, now],
        |r| r.get(0),
    )?;
    update_outbox_body(input.conn, id, revision, &next_body, Some(&sid))
}

fn outbox_fail(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&[
        "request_id",
        "id",
        "expected_revision",
        "error",
        "retryable",
        "retry_after_seconds",
    ])?;
    let error = input.required_text("error", 2048)?;
    let retryable = input.boolean("retryable", true)?;
    let prior = load_outbox_body(input.conn, id)?;
    let (state, attempts, max_attempts): (String, i64, i64) = input.conn.query_row(
        "SELECT json_extract(?1,'$.state'), json_extract(?1,'$.attempts'), json_extract(?1,'$.max_attempts')",
        [&prior],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if !matches!(state.as_str(), "delivering" | "outcome_unknown") {
        return Err(refuse(
            "invalid_state",
            format!("cannot fail outbox entry in state {state}"),
        ));
    }
    let default_delay = (5i64).saturating_mul(1i64 << attempts.saturating_sub(1).clamp(0, 6)).min(300);
    let delay = input.integer("retry_after_seconds", Some(default_delay), 86_400)?;
    let will_retry = retryable && attempts < max_attempts;
    let next_state = if will_retry { "pending" } else { "failed" };
    let next_attempt_at = if will_retry { now.saturating_add(delay) } else { now };
    let next_body: String = input.conn.query_row(
        "SELECT json_set(?1,'$.revision',?2,'$.state',?3,'$.last_error',?4,'$.next_attempt_at',?5,'$.updated_at',?6)",
        params![prior, revision, next_state, error, next_attempt_at, now],
        |r| r.get(0),
    )?;
    update_outbox_body(input.conn, id, revision, &next_body, None)
}

fn outbox_cancel(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&["request_id", "id", "expected_revision", "reason"])?;
    let reason = input.text("reason", 1024)?;
    let prior = load_outbox_body(input.conn, id)?;
    let state: String = input.conn.query_row(
        "SELECT json_extract(?1,'$.state')",
        [&prior],
        |r| r.get(0),
    )?;
    if !matches!(
        state.as_str(),
        "pending" | "outcome_unknown" | "conflict" | "failed"
    ) {
        return Err(refuse(
            "invalid_state",
            format!("cannot cancel outbox entry in state {state}"),
        ));
    }
    let next_body: String = input.conn.query_row(
        "SELECT json_set(?1,'$.revision',?2,'$.state','cancelled','$.last_error',coalesce(?3,json_extract(?1,'$.last_error')),'$.updated_at',?4)",
        params![prior, revision, reason, now],
        |r| r.get(0),
    )?;
    update_outbox_body(input.conn, id, revision, &next_body, None)
}

pub(super) fn get_sync(input: &Input<'_>) -> Result<String> {
    input.fields(&["id", "task_id", "repo_slug", "issue_number"])?;
    let id = input.optional_id("id")?;
    let task_id = input.optional_id("task_id")?;
    let repo_slug = input.text("repo_slug", 256)?;
    let has_num = input.kind("issue_number")?.is_some();
    let body: Option<String> = if let Some(id) = id {
        if task_id.is_some() || repo_slug.is_some() || has_num {
            return Err(Error::invalid("give id, task_id, or (repo_slug, issue_number)"));
        }
        input
            .conn
            .query_row("SELECT body FROM work_issue_sync WHERE id=?1", [id], |r| r.get(0))
            .optional()?
    } else if let Some(tid) = task_id {
        if repo_slug.is_some() || has_num {
            return Err(Error::invalid("give id, task_id, or (repo_slug, issue_number)"));
        }
        input
            .conn
            .query_row(
                "SELECT body FROM work_issue_sync WHERE task_id=?1",
                [tid],
                |r| r.get(0),
            )
            .optional()?
    } else if let Some(slug) = repo_slug {
        validate_repo_slug(&slug)?;
        let num = input.integer("issue_number", None, MAX_INTEGER)?;
        if num < 1 {
            return Err(Error::invalid("issue_number must be at least 1"));
        }
        input
            .conn
            .query_row(
                "SELECT body FROM work_issue_sync WHERE repo_slug=?1 AND issue_number=?2",
                params![slug, num],
                |r| r.get(0),
            )
            .optional()?
    } else {
        return Err(Error::invalid("give id, task_id, or (repo_slug, issue_number)"));
    };
    let body = body.ok_or_else(Error::missing)?;
    Ok(input.conn.query_row(
        "SELECT json_object('ok',json('true'),'item',json(?1))",
        [body],
        |r| r.get(0),
    )?)
}

pub(super) fn list_sync(input: &Input<'_>) -> Result<String> {
    input.fields(&["limit", "cursor", "only_pending", "task_id"])?;
    let p = page(input)?;
    let only_pending = input.boolean("only_pending", false)?;
    let task_id = input.optional_id("task_id")?;
    let mut filters = Vec::new();
    let mut values: Vec<Value> = Vec::new();
    if only_pending {
        filters.push("json_type(body,'$.pending_proposal') = 'object'".to_string());
    }
    if let Some(tid) = task_id {
        values.push(Value::Text(tid));
        filters.push(format!("task_id=?{}", values.len()));
    }
    let where_clause = if filters.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", filters.join(" AND "))
    };
    let total: i64 = input.conn.query_row(
        &format!("SELECT count(*) FROM work_issue_sync{where_clause}"),
        params_from_iter(values.iter()),
        |r| r.get(0),
    )?;
    let bound = values.len();
    let sql = format!(
        "SELECT body, json_extract(body,'$.created_at'), id FROM work_issue_sync{} {} (json_extract(body,'$.created_at'), id)>(?{}, ?{}) ORDER BY json_extract(body,'$.created_at'), id LIMIT ?{}",
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
    let mut stmt = input.conn.prepare(&sql)?;
    let rows = collect_page(&mut stmt.query(params_from_iter(values.iter()))?, p.limit)?;
    page_response(input.conn, rows, total)
}

pub(super) fn list_outbox(input: &Input<'_>, now: i64) -> Result<String> {
    input.fields(&["limit", "cursor", "state", "task_id", "due_only"])?;
    let p = page(input)?;
    let state = input.text("state", 32)?;
    if let Some(s) = &state {
        if !OUTBOX_STATES.contains(&s.as_str()) {
            return Err(Error::invalid("unknown outbox state"));
        }
    }
    let task_id = input.optional_id("task_id")?;
    let due_only = input.boolean("due_only", false)?;
    let mut filters = Vec::new();
    let mut values: Vec<Value> = Vec::new();
    if let Some(s) = state {
        values.push(Value::Text(s));
        filters.push(format!("state=?{}", values.len()));
    }
    if let Some(tid) = task_id {
        values.push(Value::Text(tid));
        filters.push(format!("task_id=?{}", values.len()));
    }
    if due_only {
        values.push(Value::Integer(now));
        filters.push(format!("next_attempt_at<=?{}", values.len()));
    }
    let where_clause = if filters.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", filters.join(" AND "))
    };
    let total: i64 = input.conn.query_row(
        &format!("SELECT count(*) FROM work_outbox{where_clause}"),
        params_from_iter(values.iter()),
        |r| r.get(0),
    )?;
    let bound = values.len();
    let sql = format!(
        "SELECT body, created_at, id FROM work_outbox{} {} (created_at, id)>(?{}, ?{}) ORDER BY created_at, id LIMIT ?{}",
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
    let mut stmt = input.conn.prepare(&sql)?;
    let rows = collect_page(&mut stmt.query(params_from_iter(values.iter()))?, p.limit)?;
    page_response(input.conn, rows, total)
}

#[cfg(test)]
mod tests {
    use super::sha256_hex;

    #[test]
    fn sha256_matches_nist_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
