//! One canonical export for clipboard and agent handoff. The caller's read
//! transaction binds the task and its repository/workspace revision vector.
//! Repository identities and remotes are deliberately absent from this export.

use super::{Entity, Error, Input, MAX_INTEGER, MAX_PAGE_DOCUMENTS, Result};
use rusqlite::{OptionalExtension, params};

pub(super) fn get(input: &Input<'_>) -> Result<String> {
    input.fields(&["id", "expected_revision"])?;
    let id = input.id("id")?;
    let expected = input.integer("expected_revision", None, MAX_INTEGER)?;
    if expected == 0 {
        return Err(Error::invalid("a brief requires a saved task revision"));
    }
    let (revision, body): (i64, String) = input
        .conn
        .query_row(
            "SELECT revision,body FROM work_items WHERE id=?1 AND deleted=0",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(Error::missing)?;
    if revision != expected {
        return Err(Error {
            code: "revision_conflict",
            message: format!(
                "expected revision {expected}; current revision is {revision}; reload the task before exporting"
            ),
        });
    }
    if body.len() > MAX_PAGE_DOCUMENTS {
        return Err(too_large());
    }
    // This is persisted, validated JSON, not a new request. Enhancements can
    // grow a saved document beyond the original request's 256 KiB input bound.
    let task = Input {
        conn: input.conn,
        raw: &body,
    };
    if task.id("id")? != id || task.integer("revision", None, MAX_INTEGER)? != revision {
        return Err(inconsistent());
    }
    let ids = task.strings("repository_ids", 64, 128, true)?;
    let primary = task.id("primary_repository_id")?;
    if ids.is_empty() || !ids.contains(&primary) {
        return Err(inconsistent());
    }
    let linked = input.conn.prepare(
        "SELECT repository_id FROM work_item_repositories WHERE item_id=?1 ORDER BY position,repository_id LIMIT 65",
    )?.query_map([&id], |r| r.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
    if linked != ids {
        return Err(inconsistent());
    }
    let updated = task.integer("updated_at", None, MAX_INTEGER)?;
    let title = task.required_text("title", 1200)?;
    let mut markdown = format!(
        "# Task brief v1\n\n## Title\n{title}\n\nTask: {id} (revision {revision})\nUpdated (Unix seconds): {updated}\n"
    );
    for (label, key, bound, fallback) in [
        ("Type", "kind", 64, "Unspecified"),
        ("Status", "status", 32, "Unspecified"),
        ("Severity", "severity", 32, "None"),
        ("Owner", "owner", 300, "Unassigned"),
    ] {
        markdown.push_str(&format!(
            "{label}: {}\n",
            task.text(key, bound)?.as_deref().unwrap_or(fallback)
        ));
    }
    let priority = task.integer("priority", None, 3)?;
    markdown.push_str(&format!(
        "Priority: {priority} ({})\n",
        ["Urgent", "High", "Normal", "Low"][priority as usize]
    ));
    let due = if task.kind("due_at")?.as_deref() == Some("integer") {
        task.integer("due_at", None, MAX_INTEGER)?.to_string()
    } else {
        "None".into()
    };
    markdown.push_str(&format!(
        "Due (Unix seconds): {due}\nLabels: {}\nEnhancement field locks: {}\n\n## Repositories\n",
        task.strings("labels", 64, 128, true)?.join(", "),
        task.strings("locked_fields", 2, 32, false)?.join(", ")
    ));
    let mut repositories = Vec::with_capacity(ids.len());
    for repo_id in &ids {
        let (snapshot, label) = reference(input, Entity::Repository, repo_id)?;
        markdown.push_str(&format!(
            "- {label}{}\n",
            if *repo_id == primary {
                " — primary"
            } else {
                ""
            }
        ));
        repositories.push(snapshot);
    }
    let workspace = if let Some(home) = task.optional_id("home_workspace_id")? {
        let (snapshot, label) = reference(input, Entity::Workspace, &home)?;
        markdown.push_str(&format!("\nHome workspace: {label}\n"));
        snapshot
    } else {
        markdown.push_str("\nHome workspace: None\n");
        "null".into()
    };
    markdown.push_str(&format!(
        "\n## Description\n{}\n\n## Acceptance criteria\n",
        task.text("description", 65_536)?.unwrap_or_default()
    ));
    let criteria = task.strings("acceptance_criteria", 128, 4096, true)?;
    if criteria.is_empty() {
        markdown.push_str("No acceptance criteria recorded.\n");
    }
    for criterion in criteria {
        markdown.push_str(&format!("- [ ] {criterion}\n"));
    }
    let response: String = input.conn.query_row(
        "SELECT json_object('ok',json('true'),'item',json_object('id',?1,'revision',?2,'updated_at',?3,'format_version',1,'task',json(?4),'repositories',json(?5),'workspace',json(?6),'markdown',?7))",
        params![id,revision,updated,body,format!("[{}]",repositories.join(",")),workspace,markdown], |r| r.get(0),
    )?;
    if response.len() > MAX_PAGE_DOCUMENTS {
        return Err(too_large());
    }
    Ok(response)
}

fn reference(input: &Input<'_>, entity: Entity, id: &str) -> Result<(String, String)> {
    let snapshot: String = input.conn.query_row(
        &format!("SELECT json_object('id',id,'revision',revision,'updated_at',json_extract(body,'$.updated_at'),'name',json_extract(body,'$.name')) FROM {} WHERE id=?1 AND {}",entity.table(),entity.live()),
        [id], |r| r.get(0),
    ).optional()?.ok_or_else(inconsistent)?;
    let value = Input {
        conn: input.conn,
        raw: &snapshot,
    };
    let name = value.required_text("name", 300)?;
    let revision = value.integer("revision", None, MAX_INTEGER)?;
    value.integer("updated_at", None, MAX_INTEGER)?;
    Ok((snapshot, format!("{name} [{id}] (revision {revision})")))
}

fn inconsistent() -> Error {
    Error {
        code: "store_error",
        message: "task and repository records are inconsistent; no brief was exported".into(),
    }
}
fn too_large() -> Error {
    Error {
        code: "response_too_large",
        message:
            "the complete brief exceeds the response byte budget; no partial brief was exported"
                .into(),
    }
}
