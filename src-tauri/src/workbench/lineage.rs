//! Where a board card sits in a task tree: its parent, and how many of its
//! subtasks are done.
//!
//! The card list (`items.list`) is the store's small projection and omits
//! `links`, so the board cannot read a card's parent or children from it. This
//! is the one read that answers both for a page of cards: a single query over
//! `work_item_links` (the store's own `parent` links), batched the way the
//! board's verdict read is (`verifications.board`), and read-only so a view the
//! board takes on every refresh commits nothing.
//!
//! What it never does is guess. A card the store does not hold, or has deleted,
//! is left out of the answer so the board shows it as unknown; a card with no
//! subtasks is answered as `0 of 0`, which is a reading, not an absence.

use super::WorkbenchError;
use dc_store::Store;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

/// Most cards one read answers for: a board page is at most 30 cards in each of
/// seven columns. Past it the read is refused, never cut short.
pub(crate) const MAX_BATCH: usize = 256;
/// The longest task id the store accepts.
const MAX_ID: usize = 128;

fn invalid(message: impl Into<String>) -> WorkbenchError {
    WorkbenchError::new("invalid_input", message.into())
}

/// The ids a request names, each once, in request order.
fn requested(input: &str) -> Result<Vec<String>, WorkbenchError> {
    let parsed: Value = serde_json::from_str(input).map_err(|e| invalid(e.to_string()))?;
    let named = parsed["ids"]
        .as_array()
        .ok_or_else(|| invalid("ids is required."))?;
    if named.len() > MAX_BATCH {
        return Err(invalid(format!(
            "A lineage read names at most {MAX_BATCH} tasks."
        )));
    }
    let mut seen = HashSet::new();
    let mut ids = Vec::with_capacity(named.len());
    for value in named {
        let id = value.as_str().ok_or_else(|| invalid("ids are strings."))?;
        if id.is_empty() || id.len() > MAX_ID {
            return Err(invalid("A task id is 1 to 128 characters."));
        }
        if seen.insert(id) {
            ids.push(id.to_owned());
        }
    }
    Ok(ids)
}

/// `{ok, items: {id: {parent_item_id, children: {total, done}}}}` for every
/// named task the store holds and has not deleted.
///
/// The parent is named only while it is itself live: the store keeps a link to
/// a task deleted since, and a card cannot point at a task the board no longer
/// has. A subtask counts only while live, and counts as done at status `done`
/// whether or not it has since been archived.
pub(crate) fn read(store: &Store, input: &str) -> Result<Value, WorkbenchError> {
    let ids = requested(input)?;
    let mut items = Map::new();
    if ids.is_empty() {
        return Ok(json!({"ok": true, "items": items}));
    }
    let unreadable = |e: rusqlite::Error| {
        WorkbenchError::new(
            "store_error",
            format!("the task lineage could not be read: {e}"),
        )
    };
    let listed = serde_json::to_string(&ids).map_err(|e| invalid(e.to_string()))?;
    let mut statement = store
        .connection()
        .prepare(
            "SELECT w.id,
                    (SELECT l.target_id FROM work_item_links l
                       JOIN work_items p ON p.id = l.target_id AND p.deleted = 0
                      WHERE l.item_id = w.id AND l.kind = 'parent'),
                    (SELECT COUNT(*) FROM work_item_links l
                       JOIN work_items c ON c.id = l.item_id AND c.deleted = 0
                      WHERE l.target_id = w.id AND l.kind = 'parent'),
                    (SELECT COUNT(*) FROM work_item_links l
                       JOIN work_items c ON c.id = l.item_id AND c.deleted = 0
                      WHERE l.target_id = w.id AND l.kind = 'parent' AND c.status = 'done')
               FROM work_items w
              WHERE w.deleted = 0 AND w.id IN (SELECT value FROM json_each(?1))",
        )
        .map_err(unreadable)?;
    let rows = statement
        .query_map([listed], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(unreadable)?;
    for row in rows {
        let (id, parent, total, done) = row.map_err(unreadable)?;
        items.insert(
            id,
            json!({"parent_item_id": parent, "children": {"total": total, "done": done}}),
        );
    }
    Ok(json!({"ok": true, "items": items}))
}
