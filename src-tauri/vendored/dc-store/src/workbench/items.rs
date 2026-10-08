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
/// How far a parent chain is followed looking for a cycle. Deeper than any
/// board a person maintains; a chain this long is refused rather than walked.
const MAX_PARENT_DEPTH: i64 = 256;
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
fn links(input: &Input<'_>, id: &str) -> Result<Option<Links>> {
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
    if let Some(parent) = parents.first() {
        // Walk up from the proposed parent. Reaching this task means the
        // parent is already beneath it; running past the bound is refused
        // rather than treated as "no cycle".
        let (cycle, depth): (bool, i64) = input.conn.query_row(
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
        if depth >= MAX_PARENT_DEPTH {
            return Err(Error::invalid(format!(
                "the parent chain is deeper than {MAX_PARENT_DEPTH} tasks"
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
/// re-queued. A home workspace deleted meanwhile was already cleared from the
/// task by that workspace's delete, so the restored task never points at one.
pub(super) fn restore(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&["request_id", "id", "expected_revision"])?;
    let changed = input.conn.execute(
        "UPDATE work_items SET deleted=0,revision=?2,body=json_set(json_remove(body,'$.deleted'),'$.revision',?2,'$.updated_at',?3) WHERE id=?1 AND deleted=1",
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
