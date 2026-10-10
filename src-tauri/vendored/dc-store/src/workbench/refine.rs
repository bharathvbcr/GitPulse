//! Refinement proposals (schema 12): acceptance criteria, task splits and
//! `duplicate_of` links proposed for one task. They share the enhancement
//! lifecycle, source snapshot, stale-revision and lock checks, and the one
//! profile-wide generation slot; this module owns what differs — the proposed
//! content, and an accept that writes it through the ordinary task paths.

use super::enhancements::{self, refusal};
use super::{Affected, Entity, Error, Input, MAX_INTEGER, Result, items, put_item, record_change};
use rusqlite::params;
use std::collections::HashSet;

/// What a refinement may propose. A proposal's `fields` names the parts it
/// was asked for; accepting selects among them.
pub(super) const PARTS: [&str; 3] = ["acceptance_criteria", "split_into", "duplicate_of"];

/// Hard bounds. A caller's own caps are tighter; these keep one proposal, and
/// the board growth one accept can cause, bounded whoever calls the store.
const MAX_CRITERIA: usize = 32;
const MAX_SPLITS: usize = 16;
const MAX_DUPLICATES: usize = 16;
/// A task holds at most this many criteria (`put_item`'s own bound).
const MAX_TASK_CRITERIA: usize = 128;
const MAX_CRITERION_BYTES: usize = 4096;
const MAX_GROUNDING_BYTES: usize = 16 << 10;

pub(super) fn parts(input: &Input<'_>, key: &str, required: bool) -> Result<Vec<String>> {
    enhancements::names(input, key, required, &PARTS)
}

/// The objects of the array at `key`, at most `max`, each carrying only keys
/// from `allowed`.
fn objects(input: &Input<'_>, key: &str, max: usize, allowed: &[&str]) -> Result<Vec<String>> {
    if input.kind(key)?.as_deref() != Some("array") {
        return Err(Error::invalid(format!("{key} must be an array")));
    }
    let mut stmt = input
        .conn
        .prepare("SELECT type,value FROM json_each(?1,?2)")?;
    let mut rows = stmt.query([input.raw, &format!("$.{key}")])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        if out.len() >= max || row.get::<_, String>(0)? != "object" {
            return Err(Error::invalid(format!("{key} holds at most {max} objects")));
        }
        let entry: String = row.get(1)?;
        Input {
            conn: input.conn,
            raw: &entry,
        }
        .fields(allowed)?;
        out.push(entry);
    }
    Ok(out)
}

/// Validate a finished refinement and store it as `ready`. Every requested
/// part must be present (an empty array means the model had nothing to
/// propose); a part that was not requested is refused.
pub(super) fn complete(input: &Input<'_>, prior: &Input<'_>) -> Result<String> {
    let requested = parts(prior, "fields", true)?;
    let task_id = prior.id("task_id")?;
    let source_raw: String =
        input
            .conn
            .query_row("SELECT json_extract(?1,'$.source')", [prior.raw], |r| {
                r.get(0)
            })?;
    let source = Input {
        conn: input.conn,
        raw: &source_raw,
    };
    let mut proposed = "{}".to_string();
    for part in PARTS {
        let wanted = requested.iter().any(|r| r == part);
        if input.kind(part)?.is_some() != wanted {
            return Err(Error::invalid(format!(
                "{part} must be supplied exactly when it was requested"
            )));
        }
        if !wanted {
            continue;
        }
        match part {
            "acceptance_criteria" => {
                let criteria = input.strings(part, MAX_CRITERIA, MAX_CRITERION_BYTES, true)?;
                let existing =
                    source.strings(part, MAX_TASK_CRITERIA, MAX_CRITERION_BYTES, false)?;
                let mut seen = HashSet::new();
                if criteria
                    .iter()
                    .any(|c| existing.contains(c) || !seen.insert(c))
                {
                    return Err(Error::invalid(
                        "acceptance criteria must not repeat each other or the task's own",
                    ));
                }
                if existing.len() + criteria.len() > MAX_TASK_CRITERIA {
                    return Err(Error::invalid(format!(
                        "a task holds at most {MAX_TASK_CRITERIA} acceptance criteria"
                    )));
                }
            }
            "split_into" => {
                for entry in objects(input, part, MAX_SPLITS, &["title", "description"])? {
                    let split = Input {
                        conn: input.conn,
                        raw: &entry,
                    };
                    let title = split.required_text("title", 1200)?;
                    if title.chars().count() > 300 {
                        return Err(Error::invalid("split title exceeds 300 characters"));
                    }
                    split.text("description", 65_536)?;
                }
            }
            _ => {
                let mut seen = HashSet::new();
                for entry in objects(input, part, MAX_DUPLICATES, &["id", "reason"])? {
                    let dup = Input {
                        conn: input.conn,
                        raw: &entry,
                    };
                    let id = dup.id("id")?;
                    dup.text("reason", 2048)?;
                    // A duplicate is a live task on the same board as this one.
                    let valid: bool = input.conn.query_row(
                        "SELECT ?1<>?2 AND EXISTS(SELECT 1 FROM work_items WHERE id=?1 AND deleted=0)
                           AND EXISTS(SELECT 1 FROM work_item_repositories a JOIN work_item_repositories b ON a.repository_id=b.repository_id WHERE a.item_id=?1 AND b.item_id=?2)",
                        params![id, task_id],
                        |r| r.get(0),
                    )?;
                    if !valid || !seen.insert(id) {
                        return Err(Error::invalid(
                            "duplicate_of must name distinct live tasks that share a repository with this one",
                        ));
                    }
                }
            }
        }
        proposed = input.conn.query_row(
            "SELECT json_set(?1,?2,json(json_extract(?3,?2)))",
            params![proposed, format!("$.{part}"), input.raw],
            |r| r.get(0),
        )?;
    }
    let rationale = input.text("rationale", 2048)?.unwrap_or_default();
    let mut body: String = input.conn.query_row(
        "SELECT json_set(?1,'$.state','ready','$.proposed',json(?2),'$.rationale',?3)",
        params![prior.raw, proposed, rationale],
        |r| r.get(0),
    )?;
    // The model's grounding is evidence for the reviewer. Opaque to the store,
    // so bounded rather than interpreted.
    match input.kind("grounding")?.as_deref() {
        None | Some("null") => {}
        Some("object") => {
            let grounding: String = input.conn.query_row(
                "SELECT json_extract(?1,'$.grounding')",
                [input.raw],
                |r| r.get(0),
            )?;
            if grounding.len() > MAX_GROUNDING_BYTES {
                return Err(Error::invalid("grounding exceeds its bound"));
            }
            body = input.conn.query_row(
                "SELECT json_set(?1,'$.grounding',json(?2))",
                params![body, grounding],
                |r| r.get(0),
            )?;
        }
        Some(_) => return Err(Error::invalid("grounding must be an object")),
    }
    Ok(body)
}

fn valid_new_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// Apply the selected parts of a ready refinement. Criteria and links are
/// added to the task in one revisioned update, each split becomes a new task
/// linked to this one as its parent, and nothing is removed or rewritten: a
/// refinement only adds. Returns the accepted proposal body.
pub(super) fn accept(input: &Input<'_>, prior: &Input<'_>, now: i64) -> Result<String> {
    input.fields(&[
        "id",
        "request_id",
        "expected_revision",
        "expected_task_revision",
        "fields",
        "split_ids",
    ])?;
    let requested = parts(prior, "fields", true)?;
    let selected = parts(input, "fields", true)?;
    if selected.iter().any(|part| !requested.contains(part)) {
        return Err(Error::invalid("acceptance may only select proposed fields"));
    }
    let has = |part: &str| selected.iter().any(|s| s == part);
    let task_id = prior.id("task_id")?;
    let current_body = enhancements::task_body(input, &task_id)?;
    let current = Input {
        conn: input.conn,
        raw: &current_body,
    };
    let task_revision = current.integer("revision", None, MAX_INTEGER)?;
    if input.integer("expected_task_revision", None, MAX_INTEGER)? != task_revision {
        return Err(refusal(
            "revision_conflict",
            "the task changed before refinement acceptance",
        ));
    }
    enhancements::unlocked(&current, &selected)?;
    let same_as_source = |field: &str| -> Result<bool> {
        Ok(input.conn.query_row(
            "SELECT coalesce(json_extract(?1,?3),'null') IS coalesce(json_extract(?2,?4),'null')",
            params![
                current_body,
                prior.raw,
                format!("$.{field}"),
                format!("$.source.{field}")
            ],
            |r| r.get::<_, bool>(0),
        )?)
    };
    // The proposal was drafted against the source snapshot: a changed field it
    // builds on is a conflict, never silently merged over the newer edit.
    let building_on: &[&str] = match (has("acceptance_criteria"), has("split_into")) {
        (true, true) => &["acceptance_criteria", "title", "description"],
        (true, false) => &["acceptance_criteria"],
        (false, true) => &["title", "description"],
        (false, false) => &[],
    };
    for field in building_on {
        if !same_as_source(field)? {
            return Err(refusal(
                "field_conflict",
                "a field the refinement builds on has changed; preserve the newer edit and request a fresh proposal",
            ));
        }
    }
    let proposed_raw: String =
        input
            .conn
            .query_row("SELECT json_extract(?1,'$.proposed')", [prior.raw], |r| {
                r.get(0)
            })?;
    let proposed = Input {
        conn: input.conn,
        raw: &proposed_raw,
    };
    let mut task = current_body.clone();
    let mut changed = false;
    if has("acceptance_criteria") {
        let mut criteria = current.strings(
            "acceptance_criteria",
            MAX_TASK_CRITERIA,
            MAX_CRITERION_BYTES,
            false,
        )?;
        let added = proposed.strings(
            "acceptance_criteria",
            MAX_CRITERIA,
            MAX_CRITERION_BYTES,
            true,
        )?;
        if criteria.len() + added.len() > MAX_TASK_CRITERIA {
            return Err(Error::invalid(format!(
                "a task holds at most {MAX_TASK_CRITERIA} acceptance criteria"
            )));
        }
        criteria.extend(added);
        let mut list = "[]".to_string();
        for criterion in &criteria {
            list = input.conn.query_row(
                "SELECT json_insert(?1,'$[#]',?2)",
                params![list, criterion],
                |r| r.get(0),
            )?;
        }
        task = input.conn.query_row(
            "SELECT json_set(?1,'$.acceptance_criteria',json(?2))",
            params![task, list],
            |r| r.get(0),
        )?;
        changed = true;
    }
    let mut links = None;
    if has("duplicate_of") {
        // Merge, never replace: the task's own links are kept as they are.
        let mut merged: String = input.conn.query_row(
            "SELECT coalesce(json_extract(?1,'$.links'),'[]')",
            [&current_body],
            |r| r.get(0),
        )?;
        let mut stmt = input
            .conn
            .prepare("SELECT json_extract(value,'$.id') FROM json_each(?1,'$.duplicate_of')")?;
        let targets = stmt
            .query_map([&proposed_raw], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for target in targets {
            let held: bool = input.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM json_each(?1) WHERE json_extract(value,'$.kind')='duplicate_of' AND json_extract(value,'$.item_id')=?2)",
                params![merged, target],
                |r| r.get(0),
            )?;
            if !held {
                merged = input.conn.query_row(
                    "SELECT json_insert(?1,'$[#]',json_object('kind','duplicate_of','item_id',?2))",
                    params![merged, target],
                    |r| r.get(0),
                )?;
            }
        }
        let request = format!(r#"{{"links":{merged}}}"#);
        links = items::links(
            &Input {
                conn: input.conn,
                raw: &request,
            },
            &task_id,
        )?;
        task = input.conn.query_row(
            "SELECT json_set(?1,'$.links',json(?2))",
            params![task, merged],
            |r| r.get(0),
        )?;
        changed = true;
    }
    let mut created = Vec::new();
    if has("split_into") {
        let ids = input.strings("split_ids", MAX_SPLITS, 128, true)?;
        let count: i64 = input.conn.query_row(
            "SELECT json_array_length(?1,'$.split_into')",
            [&proposed_raw],
            |r| r.get(0),
        )?;
        if i64::try_from(ids.len()).ok() != Some(count)
            || ids.iter().any(|id| !valid_new_id(id))
            || ids.iter().enumerate().any(|(i, id)| ids[..i].contains(id))
        {
            return Err(Error::invalid(
                "split_ids must hold one distinct identifier for each proposed split",
            ));
        }
        for (index, id) in ids.iter().enumerate() {
            let taken: bool = input.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM work_items WHERE id=?1)",
                [id],
                |r| r.get(0),
            )?;
            if taken {
                return Err(refusal(
                    "id_taken",
                    "a split identifier already names a task; deleted task IDs are never reused",
                ));
            }
            let request: String = input.conn.query_row(
                "SELECT json_object('request_id',?6,'id',?1,'title',json_extract(?2,?3||'.title'),'description',coalesce(json_extract(?2,?3||'.description'),''),
                    'kind',json_extract(?4,'$.kind'),'priority',json_extract(?4,'$.priority'),'severity',json_extract(?4,'$.severity'),
                    'repository_ids',json(json_extract(?4,'$.repository_ids')),'primary_repository_id',json_extract(?4,'$.primary_repository_id'),
                    'home_workspace_id',json_extract(?4,'$.home_workspace_id'),
                    'links',json_array(json_object('kind','parent','item_id',?5)))",
                params![
                    id,
                    proposed_raw,
                    format!("$.split_into[{index}]"),
                    current_body,
                    task_id,
                    input.id("request_id")?
                ],
                |r| r.get(0),
            )?;
            put_item(
                &Input {
                    conn: input.conn,
                    raw: &request,
                },
                id,
                1,
                now,
                now.saturating_mul(1000),
            )?;
            record_change(
                input.conn,
                Entity::Item,
                id,
                1,
                "items.refine_split_created",
                now,
                &Affected::none(),
            )?;
            created.push(id.clone());
        }
    } else if input.kind("split_ids")?.is_some() {
        return Err(Error::invalid(
            "split_ids apply only to a selected split_into",
        ));
    }
    let applied = if changed {
        let next = task_revision + 1;
        input.conn.execute(
            "UPDATE work_items SET revision=?2,body=json_set(?3,'$.revision',?2,'$.updated_at',?4) WHERE id=?1",
            params![task_id, next, task, now],
        )?;
        if let Some(links) = &links {
            items::replace_links(input.conn, &task_id, links)?;
        }
        record_change(
            input.conn,
            Entity::Item,
            &task_id,
            next,
            "items.enhancement_accepted",
            now,
            &Affected::none(),
        )?;
        next
    } else {
        task_revision
    };
    let mut ids = "[]".to_string();
    for id in &created {
        ids = input
            .conn
            .query_row("SELECT json_insert(?1,'$[#]',?2)", params![ids, id], |r| {
                r.get(0)
            })?;
    }
    Ok(input.conn.query_row(
        "SELECT json_set(?1,'$.state','accepted','$.accepted_fields',json(json_extract(?2,'$.fields')),'$.applied_task_revision',?3,'$.created_item_ids',json(?4))",
        params![prior.raw, input.raw, applied, ids],
        |r| r.get(0),
    )?)
}
