//! Retiring the legacy `tasks` table (dc-store schema 10).
//!
//! Before schema 10 a repository-local store kept its tasks in a table of its
//! own, with no way to create one or move one's status except hand-written SQL,
//! while the board kept the same kind of record as workbench items. There is
//! now one task definition: the workbench item. This archives every legacy row
//! as an item — `done` for what was finished, `ready` for what was open, nothing
//! the old status vocabulary cannot place — carries the executor's appended
//! scope into `task_scope_appends`, and drops the table.
//!
//! The ids are kept as they were, because leases, gaps, evidence and runs name
//! their task by that string and none of them is rewritten.
//!
//! All of it is one transaction: either every row is an item and the table is
//! gone, or the store is exactly as it was found. It keys on the table being
//! present rather than on a version, so an interrupted or half-applied attempt
//! (items written, table still there) is finished by the next open, which skips
//! ids that already have an item. A row that cannot become a valid item — an id
//! the workbench refuses, a scope that does not validate — refuses the whole
//! migration, naming the task, instead of being skipped or truncated.

use super::{
    Error, Input, LOCAL_REPOSITORY_ID, Result, backup_profile, ensure_schema,
    local_repository_request, mutate,
};
use crate::{Store, json};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashSet;

/// What a migration did, for a caller that wants to report it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Retired {
    /// Rows written as items.
    pub archived: usize,
    /// Rows whose id already had an item (an interrupted earlier attempt).
    pub already_items: usize,
}

struct LegacyRow {
    id: String,
    title: String,
    description: String,
    status: String,
    priority: Option<String>,
    difficulty: Option<String>,
    lists: [String; 6],
    appended: [String; 3],
    position: i64,
}

fn table_exists(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='tasks')",
        [],
        |r| r.get(0),
    )?)
}

/// The workbench status a legacy status becomes. `blocked` is not a status any
/// more — whether a task is blocked is derived from its gaps — so it is `ready`
/// like the rest of the open ones; a status nothing here places is `backlog`, so
/// it is never handed out as ready work by accident.
fn item_status(legacy: &str) -> &'static str {
    match legacy.trim().to_ascii_lowercase().as_str() {
        "done" | "verified" | "completed" | "closed" => "done",
        "in_progress" => "in_progress",
        "review" => "review",
        "planned" | "ready" | "blocked" => "ready",
        _ => "backlog",
    }
}

fn read_rows(conn: &Connection) -> Result<Vec<LegacyRow>> {
    let mut columns = HashSet::new();
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('tasks')")?;
    for name in stmt.query_map([], |r| r.get::<_, String>(0))? {
        columns.insert(name?);
    }
    // A store from before a column existed does not have it; that column's
    // contribution is its default, never a failed query.
    let column = |name: &str, default: &str| {
        if columns.contains(name) {
            format!("coalesce({name}, {default})")
        } else {
            default.to_string()
        }
    };
    let sql = format!(
        "SELECT id, {title}, {description}, {status}, {priority}, {difficulty},
                {planned}, {forbidden}, {commands}, {tests}, {requirements}, {criteria},
                {ap_planned}, {ap_commands}, {ap_tests}, rowid
         FROM tasks ORDER BY rowid",
        title = column("title", "''"),
        description = column("description", "''"),
        status = column("status", "'planned'"),
        priority = if columns.contains("priority") {
            "priority"
        } else {
            "NULL"
        },
        difficulty = if columns.contains("difficulty") {
            "difficulty"
        } else {
            "NULL"
        },
        planned = column("planned_files_json", "'[]'"),
        forbidden = column("forbidden_changes_json", "'[]'"),
        commands = column("allowed_commands_json", "'[]'"),
        tests = column("expected_tests_json", "'[]'"),
        requirements = column("requirement_ids_json", "'[]'"),
        criteria = column("acceptance_criterion_ids_json", "'[]'"),
        ap_planned = column("agent_appended_planned_files_json", "'[]'"),
        ap_commands = column("agent_appended_allowed_commands_json", "'[]'"),
        ap_tests = column("agent_appended_expected_tests_json", "'[]'"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(LegacyRow {
                id: r.get(0)?,
                title: r.get(1)?,
                description: r.get(2)?,
                status: r.get(3)?,
                priority: r.get(4)?,
                difficulty: r.get(5)?,
                lists: [
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                ],
                appended: [r.get(12)?, r.get(13)?, r.get(14)?],
                position: r.get(15)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn refuse(id: &str, detail: impl std::fmt::Display) -> Error {
    Error {
        code: "legacy_task_unmigratable",
        message: format!(
            "legacy task {id} cannot become a workbench item: {detail}; the store was left as it was found"
        ),
    }
}

/// The `items.put` request that carries one legacy row.
fn request(conn: &Connection, row: &LegacyRow) -> Result<String> {
    for (name, value) in [
        ("planned_files_json", &row.lists[0]),
        ("forbidden_changes_json", &row.lists[1]),
        ("allowed_commands_json", &row.lists[2]),
        ("expected_tests_json", &row.lists[3]),
        ("requirement_ids_json", &row.lists[4]),
        ("acceptance_criterion_ids_json", &row.lists[5]),
    ] {
        json::check_array(value)
            .map_err(|why| refuse(&row.id, format!("{name} is not a JSON array ({why})")))?;
    }
    let status = item_status(&row.status);
    let priority = row
        .priority
        .as_deref()
        .map(|p| p.trim().to_ascii_lowercase());
    let (rank, severity) = match priority.as_deref() {
        Some("critical") => (1, Some("critical")),
        Some("high") => (1, Some("high")),
        Some("medium") => (2, Some("medium")),
        Some("low") => (3, Some("low")),
        _ => (2, None),
    };
    let title: String = row.title.chars().take(300).collect();
    let title = if title.trim().is_empty() {
        row.id.clone()
    } else {
        title
    };
    if row.description.len() > 65_536 {
        return Err(refuse(&row.id, "its description exceeds 64 KiB"));
    }
    conn.query_row(
        "SELECT json_object(
            'request_id', 'legacy-task-' || ?1, 'id', ?1, 'expected_revision', 0,
            'title', ?2, 'description', ?3, 'status', ?4, 'priority', ?5, 'severity', ?6,
            'position', ?7, 'repository_ids', json_array(?8), 'primary_repository_id', ?8,
            'archived', json(?9),
            'scope', json_object(
                'planned_files', json(?10), 'forbidden_changes', json(?11),
                'allowed_commands', json(?12), 'expected_tests', json(?13),
                'requirement_ids', json(?14), 'acceptance_criterion_ids', json(?15),
                'difficulty', ?16))",
        params![
            row.id,
            title,
            row.description,
            status,
            rank,
            severity,
            row.position,
            LOCAL_REPOSITORY_ID,
            if status == "done" { "true" } else { "false" },
            row.lists[0],
            row.lists[1],
            row.lists[2],
            row.lists[3],
            row.lists[4],
            row.lists[5],
            row.difficulty,
        ],
        |r| r.get(0),
    )
    .map_err(|e| refuse(&row.id, e))
}

/// Archives the legacy `tasks` table as workbench items and drops it, if the
/// store still has one. Safe to call on every open: with no such table it
/// reads one catalog row and returns.
///
/// The store's columns and its `schema_version` stamp (`known`) commit in the
/// same transaction as the drop. Stamping afterwards would leave a window — a
/// failed stamp after a committed drop — in which the store reads as the old
/// version with no tasks table, and an older reader recreates an empty one and
/// answers "no ready tasks". `stamp_budget` fails the stamp after that many
/// write steps, for the test that proves the rollback.
pub(crate) fn retire(
    store: &Store,
    known: u32,
    stamp_budget: &mut Option<usize>,
) -> Result<Retired> {
    let conn = &store.conn;
    if !table_exists(conn)? {
        return Ok(Retired::default());
    }
    let held: i64 = conn.query_row("SELECT count(*) FROM tasks", [], |r| r.get(0))?;
    // Rows need the workbench profile to land in, created or brought current
    // (and backed up) before the transaction opens: its own steps are
    // transactions. An empty legacy table — every store before schema 10 had
    // one — is dropped without touching the profile, so a read does not
    // upgrade it for hosts that still speak the older one.
    let workbench_ready = held > 0;
    if workbench_ready {
        ensure_schema(conn, true)?;
        backup_profile(conn, "pre-tasks-retire", |backup, detail| {
            format!(
                "the legacy tasks table cannot be retired because its backup {} could not be written: {detail}",
                backup.display()
            )
        })?;
    }

    conn.execute_batch("BEGIN IMMEDIATE")?;
    let outcome = archive(store, workbench_ready).and_then(|done| {
        crate::schema::bring_current(conn, known, stamp_budget)
            .map(|()| done)
            .map_err(|detail| Error {
                code: "legacy_task_unmigratable",
                message: format!(
                    "the legacy tasks table was not retired because the schema could not be brought to version {known}: {detail}; the store was left as it was found"
                ),
            })
    });
    match outcome {
        Ok(done) => {
            conn.execute_batch("COMMIT")?;
            Ok(done)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn archive(store: &Store, workbench_ready: bool) -> Result<Retired> {
    let conn = &store.conn;
    // Re-checked under the write lock: another process may have finished it
    // while this one waited.
    if !table_exists(conn)? {
        return Ok(Retired::default());
    }
    let now_ms = store.workbench_now_millis()?;
    let now = now_ms / 1000;
    let rows = read_rows(conn)?;
    if !rows.is_empty() && !workbench_ready {
        return Err(Error {
            code: "legacy_task_unmigratable",
            message: "rows were added to the legacy tasks table while it was being retired; \
                      the store was left as it was found, and the next open retires them"
                .to_string(),
        });
    }
    if !rows.is_empty() {
        let raw = local_repository_request();
        let repository = Input::new(conn, &raw)?;
        match mutate(&repository, "repositories.put", now, now_ms) {
            Ok(_) => {}
            Err(error) if error.code == "revision_conflict" => {}
            Err(error) => return Err(error),
        }
    }
    let mut done = Retired::default();
    for row in &rows {
        let existing: Option<String> = conn
            .query_row(
                "SELECT primary_repository_id FROM work_items WHERE id=?1",
                [&row.id],
                |r| r.get(0),
            )
            .optional()?;
        match existing {
            Some(primary) if primary == LOCAL_REPOSITORY_ID => done.already_items += 1,
            Some(primary) => {
                return Err(refuse(
                    &row.id,
                    format!("an item with that id already belongs to repository {primary}"),
                ));
            }
            None => {
                let raw = request(conn, row)?;
                let put = Input::new(conn, &raw).map_err(|e| refuse(&row.id, &e.message))?;
                mutate(&put, "items.put", now, now_ms).map_err(|e| refuse(&row.id, &e.message))?;
                // A receipt left by an earlier attempt replays without writing;
                // an item that is still absent after the request is a refusal,
                // not a row to count as archived.
                let written: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM work_items WHERE id=?1)",
                    [&row.id],
                    |r| r.get(0),
                )?;
                if !written {
                    return Err(refuse(
                        &row.id,
                        "its request was replayed from an old receipt but no item exists",
                    ));
                }
                done.archived += 1;
            }
        }
        for (name, value) in ["planned files", "allowed commands", "expected tests"]
            .iter()
            .zip(&row.appended)
        {
            json::check_array(value).map_err(|why| {
                refuse(
                    &row.id,
                    format!("its appended {name} are not a JSON array ({why})"),
                )
            })?;
        }
        conn.execute(
            "INSERT OR IGNORE INTO task_scope_appends
               (task_id, planned_files_json, allowed_commands_json, expected_tests_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![row.id, row.appended[0], row.appended[1], row.appended[2]],
        )?;
    }
    conn.execute_batch("DROP TABLE tasks")?;
    Ok(done)
}

/// Refuses a handle that cannot migrate to read a store that still needs it:
/// answering "no ready tasks" from a store whose tasks are still in the legacy
/// table would be a confident zero.
pub(crate) fn require_retired(conn: &Connection) -> std::result::Result<(), String> {
    match table_exists(conn) {
        Ok(false) => Ok(()),
        Ok(true) => Err(
            "the legacy tasks table has not been retired into workbench items; open the store \
             with a writing handle (any dcstore command) to migrate it"
                .to_string(),
        ),
        Err(error) => Err(error.to_string()),
    }
}
