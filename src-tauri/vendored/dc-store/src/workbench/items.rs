//! Task fields added in schema 11 — the archive flag, completion time, the
//! checklist and links to other tasks — and restoring a deleted task.
//!
//! Every field here follows the `locked_fields` rule in `put_item`: a request
//! that omits it keeps the stored value, and only an explicit value changes
//! it. A host built before schema 11 cannot open a schema 11 profile, but a
//! host that knows the schema and simply does not edit a field (an agent's
//! status move, a merge) must not unarchive, uncheck or unlink the task.

use super::{Error, Input, Result};
use rusqlite::{OptionalExtension, params};
use std::collections::HashSet;

/// Upper bounds, matching the acceptance criteria a checklist sits beside.
const MAX_CHECKLIST: usize = 128;
const MAX_CHECKLIST_TEXT: usize = 4096;
const MAX_LINKS: usize = 64;
/// The most tasks one parent chain may hold, root to deepest subtask, and so
/// how far it is followed looking for a cycle. Deeper than any board a person
/// maintains; a new parent link that would pass it is refused.
const MAX_PARENT_DEPTH: i64 = 256;
/// Maximum chain depth for `blocks` dependency links when checking cycles.
const MAX_BLOCKS_DEPTH: i64 = 256;
const MAX_READINESS_BATCH: usize = 200;
const MAX_READINESS_SCOPED: usize = 500;
/// A task's outbound links as `(kind, target id)`, in request order.
pub(super) type Links = Vec<(String, String)>;
pub(super) const LINK_KINDS: &[&str] = &["parent", "blocks", "related", "duplicate_of"];

/// Apply the schema 11 fields to `body`, the document `put_item` built from
/// the fields every schema accepts. Validates the request's values, resolves
/// omissions against the stored task, and returns the finished body plus the
/// links to write to `work_item_links` when the request replaced them.
pub(super) fn apply(
    input: &Input<'_>,
    id: &str,
    status: &str,
    body: String,
    now: i64,
) -> Result<(String, Option<Links>)> {
    let archived = match input.kind("archived")?.as_deref() {
        None | Some("null") => None,
        Some(_) => Some(input.boolean("archived", false)?),
    };
    let checklist = checklist(input)?;
    let links = links(input, id)?;
    let links_json = links.as_ref().map(|links| {
        let rows = links
            .iter()
            .map(|(kind, target)| format!(r#"{{"kind":"{kind}","item_id":"{target}"}}"#))
            .collect::<Vec<_>>();
        format!("[{}]", rows.join(","))
    });
    // `completed_at` is the store's: it is set when the task enters Done,
    // kept while it stays there, and cleared when it leaves. A request
    // cannot name it, so a host clock cannot back-date a completion.
    let body: String = input.conn.query_row(
        "WITH prior AS (SELECT body,status FROM work_items WHERE id=?2)
         SELECT json_set(?1,
            '$.archived',json(CASE coalesce(?3,(SELECT json_extract(body,'$.archived') FROM prior),0) WHEN 1 THEN 'true' ELSE 'false' END),
            '$.completed_at',CASE WHEN ?4='done' THEN coalesce((SELECT json_extract(body,'$.completed_at') FROM prior WHERE status='done'),?5) END,
            '$.checklist',json(coalesce(?6,(SELECT json_extract(body,'$.checklist') FROM prior),'[]')),
            '$.links',json(coalesce(?7,(SELECT json_extract(body,'$.links') FROM prior),'[]')))",
        params![body, id, archived, status, now, checklist, links_json],
        |r| r.get(0),
    )?;
    Ok((body, links))
}

/// The request's checklist as stored JSON, or `None` when it named none.
/// Each entry is exactly `{"text": non-blank string, "done": boolean}`.
fn checklist(input: &Input<'_>) -> Result<Option<String>> {
    match input.kind("checklist")?.as_deref() {
        None | Some("null") => return Ok(None),
        Some("array") => {}
        _ => return Err(Error::invalid("checklist must be an array")),
    }
    let mut stmt = input
        .conn
        .prepare("SELECT type,value FROM json_each(?1,'$.checklist')")?;
    let mut rows = stmt.query([input.raw])?;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        count += 1;
        if count > MAX_CHECKLIST || row.get::<_, String>(0)? != "object" {
            return Err(Error::invalid(format!(
                "checklist holds at most {MAX_CHECKLIST} objects"
            )));
        }
        let entry: String = row.get(1)?;
        let item = Input {
            conn: input.conn,
            raw: &entry,
        };
        item.fields(&["text", "done"])?;
        item.required_text("text", MAX_CHECKLIST_TEXT)?;
        if !matches!(item.kind("done")?.as_deref(), Some("true" | "false")) {
            return Err(Error::invalid("each checklist entry needs a boolean done"));
        }
    }
    Ok(Some(input.conn.query_row(
        "SELECT json_extract(?1,'$.checklist')",
        [input.raw],
        |r| r.get(0),
    )?))
}

/// The request's links, validated against the board, or `None` when it named
/// none. A new link names a live task other than this one (a link the task
/// already holds may outlive its target); a task has at most one parent, and
/// a parent may not be one of its own descendants.
pub(super) fn links(input: &Input<'_>, id: &str) -> Result<Option<Links>> {
    match input.kind("links")?.as_deref() {
        None | Some("null") => return Ok(None),
        Some("array") => {}
        _ => return Err(Error::invalid("links must be an array")),
    }
    let mut stmt = input
        .conn
        .prepare("SELECT type,value FROM json_each(?1,'$.links')")?;
    let mut rows = stmt.query([input.raw])?;
    let mut links = Vec::new();
    let mut seen = HashSet::new();
    while let Some(row) = rows.next()? {
        if links.len() >= MAX_LINKS || row.get::<_, String>(0)? != "object" {
            return Err(Error::invalid(format!(
                "links holds at most {MAX_LINKS} objects"
            )));
        }
        let entry: String = row.get(1)?;
        let link = Input {
            conn: input.conn,
            raw: &entry,
        };
        link.fields(&["kind", "item_id"])?;
        let kind = link.required_text("kind", 32)?;
        if !LINK_KINDS.contains(&kind.as_str()) {
            return Err(Error::invalid(format!(
                "unknown link kind {kind}; expected one of {}",
                LINK_KINDS.join(", ")
            )));
        }
        let target = link.id("item_id")?;
        if target == id {
            return Err(Error::invalid("a task cannot link to itself"));
        }
        if !seen.insert((kind.clone(), target.clone())) {
            return Err(Error::invalid("links contains duplicates"));
        }
        // A new link names a live task. One the task already holds is kept
        // even if its target was deleted since: otherwise deleting a task
        // would make every task that links to it unsaveable until a person
        // found and removed the dead link.
        let allowed: bool = input.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_items WHERE id=?1 AND deleted=0)
                 OR EXISTS(SELECT 1 FROM work_item_links WHERE item_id=?2 AND kind=?3 AND target_id=?1)",
            params![target, id, kind],
            |r| r.get(0),
        )?;
        if !allowed {
            return Err(Error::invalid(format!(
                "linked task {target} does not exist or is deleted"
            )));
        }
        links.push((kind, target));
    }
    let parents: Vec<&String> = links
        .iter()
        .filter(|(kind, _)| kind == "parent")
        .map(|(_, target)| target)
        .collect();
    if parents.len() > 1 {
        return Err(Error::invalid("a task has at most one parent"));
    }
    // A parent the task already has was checked when it was set, and nothing
    // since can have made it a cycle: every new parent link is checked. It is
    // not checked again, so a chain another task's edit has since lengthened
    // never makes this task unsaveable.
    let new_parent = match parents.first() {
        Some(parent) => !input.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_item_links WHERE item_id=?1 AND kind='parent' AND target_id=?2)",
            params![id, parent],
            |r| r.get::<_, bool>(0),
        )?,
        None => false,
    };
    if let Some(parent) = parents.first().filter(|_| new_parent) {
        // Walk up from the proposed parent and down from this task. Reaching
        // this task going up means the parent is already beneath it. The
        // bound is on the whole chain the link would make — the parent's
        // ancestors, the parent, this task and its deepest subtask — so
        // hanging one chain under another cannot build one past it.
        let (cycle, above): (bool, i64) = input.conn.query_row(
            "WITH RECURSIVE up(id,depth) AS (
                SELECT ?1,0
                UNION SELECT l.target_id,up.depth+1 FROM work_item_links l JOIN up ON l.item_id=up.id
                 WHERE l.kind='parent' AND up.depth<?3
             ) SELECT coalesce(max(id=?2),0),max(depth) FROM up",
            params![parent, id, MAX_PARENT_DEPTH],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if cycle {
            return Err(Error::invalid(
                "that parent is already a subtask of this task",
            ));
        }
        let below: i64 = input.conn.query_row(
            "WITH RECURSIVE down(id,depth) AS (
                SELECT ?1,0
                UNION SELECT l.item_id,down.depth+1 FROM work_item_links l JOIN down ON l.target_id=down.id
                 WHERE l.kind='parent' AND down.depth<?2
             ) SELECT max(depth) FROM down",
            params![id, MAX_PARENT_DEPTH],
            |r| r.get(0),
        )?;
        // Tasks in the chain: `above` ancestors of the parent, the parent,
        // this task, and `below` levels under it.
        if above + below + 2 > MAX_PARENT_DEPTH {
            return Err(Error::invalid(format!(
                "the parent chain is deeper than {MAX_PARENT_DEPTH} tasks"
            )));
        }
    }
    // Check newly added `blocks` links for dependency cycles and chain depth.
    // Walking backward into `id` never touches `id`'s replaced outbound rows,
    // and stopping forward recursion once `id` is reached avoids traversing them.
    for target in links
        .iter()
        .filter(|(kind, _)| kind == "blocks")
        .map(|(_, target)| target)
    {
        let existing: bool = input.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_item_links WHERE item_id=?1 AND kind='blocks' AND target_id=?2)",
            params![id, target],
            |r| r.get(0),
        )?;
        if existing {
            continue;
        }
        let (cycle, below): (bool, i64) = input.conn.query_row(
            "WITH RECURSIVE forward(id,depth) AS (
                SELECT ?1,0
                UNION SELECT l.target_id,forward.depth+1 FROM work_item_links l JOIN forward ON l.item_id=forward.id
                 WHERE l.kind='blocks' AND forward.id<>?2 AND forward.depth<?3
             ) SELECT coalesce(max(id=?2),0),max(depth) FROM forward",
            params![target, id, MAX_BLOCKS_DEPTH],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if cycle {
            return Err(Error::invalid(
                "that blocks link would create a dependency cycle",
            ));
        }
        let above: i64 = input.conn.query_row(
            "WITH RECURSIVE backward(id,depth) AS (
                SELECT ?1,0
                UNION SELECT l.item_id,backward.depth+1 FROM work_item_links l JOIN backward ON l.target_id=backward.id
                 WHERE l.kind='blocks' AND backward.depth<?2
             ) SELECT max(depth) FROM backward",
            params![id, MAX_BLOCKS_DEPTH],
            |r| r.get(0),
        )?;
        if above + below + 2 > MAX_BLOCKS_DEPTH {
            return Err(Error::invalid(format!(
                "the blocks dependency chain is deeper than {MAX_BLOCKS_DEPTH} tasks"
            )));
        }
    }
    Ok(Some(links))
}

/// Replace `id`'s rows in `work_item_links` with `links`, in request order.
pub(super) fn replace_links(
    conn: &rusqlite::Connection,
    id: &str,
    links: &[(String, String)],
) -> Result<()> {
    conn.execute("DELETE FROM work_item_links WHERE item_id=?1", [id])?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO work_item_links(item_id,kind,target_id,position) VALUES(?1,?2,?3,?4)",
    )?;
    for (position, (kind, target)) in links.iter().enumerate() {
        let position =
            i64::try_from(position).map_err(|_| Error::invalid("too many task links"))?;
        insert.execute(params![id, kind, target, position])?;
    }
    Ok(())
}

/// Bring a soft-deleted task back at the next revision.
///
/// The row, its repository links, its own task links and its history never
/// left the profile, so restoring is clearing the flag. What the delete
/// discarded stays discarded: queued enhancement suggestions are not
/// re-queued. A workspace's delete leaves deleted tasks untouched, so a home
/// workspace deleted meanwhile is cleared here: the restored task never points
/// at one.
pub(super) fn restore(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&["request_id", "id", "expected_revision"])?;
    let changed = input.conn.execute(
        "UPDATE work_items SET deleted=0,revision=?2,
            home_workspace_id=(SELECT w.id FROM work_workspaces w WHERE w.id=work_items.home_workspace_id AND w.deleted=0),
            body=json_set(json_remove(body,'$.deleted'),'$.revision',?2,'$.updated_at',?3,
                '$.home_workspace_id',(SELECT w.id FROM work_workspaces w WHERE w.id=work_items.home_workspace_id AND w.deleted=0))
          WHERE id=?1 AND deleted=1",
        params![id, revision, now],
    )?;
    if changed != 1 {
        return Err(Error::missing());
    }
    Ok(())
}

/// Refuse `items.restore` on a task that is not deleted, before the revision
/// check: compared against the deleted row's revision, a live task would
/// otherwise read as a revision conflict, which is not what happened.
pub(super) fn refuse_live_restore(conn: &rusqlite::Connection, id: &str) -> Result<()> {
    let live: Option<i64> = conn
        .query_row(
            "SELECT revision FROM work_items WHERE id=?1 AND deleted=0",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    if live.is_some() {
        return Err(Error {
            code: "invalid_state",
            message: "the task is not deleted".into(),
        });
    }
    Ok(())
}

/// Summary of an open linked task blocking or nested under a candidate task.
pub(super) struct LinkedTaskSummary {
    pub id: String,
    pub title: String,
    pub status: String,
}

fn query_linked_tasks(
    conn: &rusqlite::Connection,
    sql: &str,
    task_id: &str,
) -> Result<Vec<LinkedTaskSummary>> {
    let mut stmt = conn.prepare_cached(sql)?;
    let rows = stmt.query_map([task_id], |row| {
        Ok(LinkedTaskSummary {
            id: row.get(0)?,
            title: row.get(1)?,
            status: row.get(2)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Live non-done tasks that have a `'blocks'` link targeting `task_id`.
pub(super) fn open_blockers(
    conn: &rusqlite::Connection,
    task_id: &str,
) -> Result<Vec<LinkedTaskSummary>> {
    query_linked_tasks(
        conn,
        "SELECT blocker.id, blocker.title, blocker.status
         FROM work_item_links l
         JOIN work_items blocker ON blocker.id = l.item_id
         WHERE l.target_id = ?1 AND l.kind = 'blocks' AND blocker.deleted = 0 AND blocker.status <> 'done'
         ORDER BY blocker.position, blocker.id",
        task_id,
    )
}

/// Live non-done tasks that `task_id` blocks via outbound `'blocks'` links.
fn open_blocking(conn: &rusqlite::Connection, task_id: &str) -> Result<Vec<LinkedTaskSummary>> {
    query_linked_tasks(
        conn,
        "SELECT target.id, target.title, target.status
         FROM work_item_links l
         JOIN work_items target ON target.id = l.target_id
         WHERE l.item_id = ?1 AND l.kind = 'blocks' AND target.deleted = 0 AND target.status <> 'done'
         ORDER BY target.position, target.id",
        task_id,
    )
}

/// Live non-done child tasks whose `'parent'` link targets `task_id`.
pub(super) fn open_subtasks(
    conn: &rusqlite::Connection,
    task_id: &str,
) -> Result<Vec<LinkedTaskSummary>> {
    query_linked_tasks(
        conn,
        "SELECT child.id, child.title, child.status
         FROM work_item_links l
         JOIN work_items child ON child.id = l.item_id
         WHERE l.target_id = ?1 AND l.kind = 'parent' AND child.deleted = 0 AND child.status <> 'done'
         ORDER BY child.position, child.id",
        task_id,
    )
}

fn task_readiness_json(conn: &rusqlite::Connection, task_id: &str) -> Result<String> {
    let blocked_by = open_blockers(conn, task_id)?;
    let blocking = open_blocking(conn, task_id)?;
    let open_children = open_subtasks(conn, task_id)?;
    let total_subtask_count: i64 = conn.query_row(
        "SELECT count(*) FROM work_item_links l JOIN work_items child ON child.id = l.item_id WHERE l.target_id = ?1 AND l.kind = 'parent' AND child.deleted = 0",
        [task_id],
        |r| r.get(0),
    )?;
    let ready = blocked_by.is_empty();
    let ready_with_subtasks = ready && open_children.is_empty();
    let reason = if !ready {
        "blocked_by_tasks"
    } else if !open_children.is_empty() {
        "open_subtasks"
    } else {
        "ready"
    };
    let encode_list = |items: &[LinkedTaskSummary]| -> Result<String> {
        let mut arr = "[]".to_string();
        for item in items {
            arr = conn.query_row(
                "SELECT json_insert(?1,'$[#]',json_object('id',?2,'title',?3,'status',?4))",
                params![arr, item.id, item.title, item.status],
                |r| r.get(0),
            )?;
        }
        Ok(arr)
    };
    let blocked_json = encode_list(&blocked_by)?;
    let blocking_json = encode_list(&blocking)?;
    let open_subtasks_json = encode_list(&open_children)?;
    let open_count = i64::try_from(open_children.len()).unwrap_or(i64::MAX);
    Ok(conn.query_row(
        "SELECT json_object('id',?1,'ready',json(CASE WHEN ?2 THEN 'true' ELSE 'false' END),'ready_with_subtasks',json(CASE WHEN ?3 THEN 'true' ELSE 'false' END),'readiness_reason',?4,'blocked_by',json(?5),'blocking',json(?6),'open_subtasks',json(?7),'open_subtask_count',?8,'total_subtask_count',?9)",
        params![
            task_id,
            ready,
            ready_with_subtasks,
            reason,
            blocked_json,
            blocking_json,
            open_subtasks_json,
            open_count,
            total_subtask_count
        ],
        |r| r.get(0),
    )?)
}

/// Compute task readiness (`blocked_by`, `blocking`, `open_subtasks`) for a
/// single task (`id`) or a batch (`ids`, `workspace_id`, `repository_id`).
pub(super) fn readiness(input: &Input<'_>) -> Result<String> {
    input.fields(&["id", "ids", "workspace_id", "repository_id"])?;
    let single = input.optional_id("id")?;
    let has_ids = input.kind("ids")?.is_some();
    let workspace_id = input.optional_id("workspace_id")?;
    let repository_id = input.optional_id("repository_id")?;
    let modes = usize::from(single.is_some())
        + usize::from(has_ids)
        + usize::from(workspace_id.is_some())
        + usize::from(repository_id.is_some());
    if modes != 1 {
        return Err(Error::invalid(
            "give exactly one of id, ids, workspace_id, or repository_id",
        ));
    }
    if let Some(id) = single {
        let exists: bool = input.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_items WHERE id=?1 AND deleted=0)",
            [&id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::missing());
        }
        let body = task_readiness_json(input.conn, &id)?;
        return Ok(input.conn.query_row(
            "SELECT json_object('ok',json('true'),'item',json(?1))",
            [body],
            |r| r.get(0),
        )?);
    }
    let mut target_ids = Vec::new();
    let mut truncated = false;
    if has_ids {
        let raw_ids = input.strings("ids", MAX_READINESS_BATCH + 1, 128, true)?;
        if raw_ids.len() > MAX_READINESS_BATCH {
            return Err(Error::invalid(format!(
                "ids accepts at most {MAX_READINESS_BATCH} entries"
            )));
        }
        let mut seen = HashSet::new();
        for tid in raw_ids {
            if !tid
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            {
                return Err(Error::invalid("ids contains an invalid identifier"));
            }
            if !seen.insert(tid.clone()) {
                continue;
            }
            let exists: bool = input.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM work_items WHERE id=?1 AND deleted=0)",
                [&tid],
                |r| r.get(0),
            )?;
            if exists {
                target_ids.push(tid);
            }
        }
    } else if let Some(ws) = workspace_id {
        let mut stmt = input.conn.prepare(
            "SELECT DISTINCT t.id FROM (
                SELECT id FROM work_items WHERE home_workspace_id=?1 AND deleted=0
                UNION
                SELECT ir.item_id FROM work_workspace_repositories wr
                JOIN work_item_repositories ir ON ir.repository_id=wr.repository_id
                WHERE wr.workspace_id=?1
             ) sel JOIN work_items t ON t.id=sel.id WHERE t.deleted=0
             ORDER BY t.position, t.id LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![ws, i64::try_from(MAX_READINESS_SCOPED + 1).unwrap_or(501)],
            |r| r.get::<_, String>(0),
        )?;
        for row in rows {
            if target_ids.len() >= MAX_READINESS_SCOPED {
                truncated = true;
                break;
            }
            target_ids.push(row?);
        }
    } else if let Some(repo) = repository_id {
        let mut stmt = input.conn.prepare(
            "SELECT t.id FROM work_item_repositories ir
             JOIN work_items t ON t.id=ir.item_id
             WHERE ir.repository_id=?1 AND t.deleted=0
             ORDER BY t.position, t.id LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![repo, i64::try_from(MAX_READINESS_SCOPED + 1).unwrap_or(501)],
            |r| r.get::<_, String>(0),
        )?;
        for row in rows {
            if target_ids.len() >= MAX_READINESS_SCOPED {
                truncated = true;
                break;
            }
            target_ids.push(row?);
        }
    }
    let mut encoded = Vec::with_capacity(target_ids.len());
    for tid in &target_ids {
        encoded.push(task_readiness_json(input.conn, tid)?);
    }
    let joined = format!("[{}]", encoded.join(","));
    let total = i64::try_from(target_ids.len()).unwrap_or(i64::MAX);
    Ok(input.conn.query_row(
        "SELECT json_object('ok',json('true'),'items',json(?1),'total',?2,'truncated',json(CASE WHEN ?3 THEN 'true' ELSE 'false' END))",
        params![joined, total, truncated],
        |r| r.get(0),
    )?)
}
