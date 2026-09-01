//! Read-only view of DevCouncil's task and lease store.
//!
//! A worktree stops being a directory and becomes *a task in flight* once it is
//! bound to one. This module supplies the task half of that: which tasks exist,
//! which are leased, and what scope each declares.
//!
//! # GitPulse never writes here
//!
//! Manvi owns `state.sqlite` — its schema, its migrations, and the partial
//! unique index the lease's mutual exclusion rests on. GitPulse links `dc-store`
//! to *read*, and must never acquire, renew or release a lease, and never call
//! `checkout_task` or `write_file`: those contend with an active agent's writer
//! lease, and a UI process that takes a lease strands the task when the window
//! closes. See `contracts/lease.schema.md`.
//!
//! # Absence is a state, not an error
//!
//! Most repositories have no DevCouncil store at all, and that is normal. A
//! missing store yields an empty view with `available: false`, never an error
//! and never an empty list that reads as "no tasks" — those are different facts
//! and the UI renders them differently.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Where a repository's DevCouncil state lives.
pub fn store_path(repo_path: &str) -> PathBuf {
    Path::new(repo_path)
        .join(".devcouncil")
        .join("state.sqlite")
}

/// A task's declared scope, as much of it as the gate needs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskScope {
    pub id: String,
    pub title: String,
    pub status: String,
    /// Paths the plan authorises, repo-relative.
    pub planned_files: Vec<String>,
    /// Of those, the ones an executor added to its *own* scope while working.
    ///
    /// Kept apart from `planned_files` rather than merged, because the two
    /// answer different questions: what the planner authorised, and what the
    /// worker authorised for itself. Merging them is what makes a self-granted
    /// widening read as an ordinary planned write.
    pub agent_appended_files: Vec<String>,
    pub forbidden_changes: Vec<String>,
    pub allowed_commands: Vec<String>,
}

/// An active lease, as the UI shows it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskLease {
    pub task_id: String,
    pub owner: String,
    pub agent: Option<String>,
    pub branch: Option<String>,
    pub status: String,
    pub created_at: String,
    /// ISO-8601, or null when this lease does not expire.
    ///
    /// Null means *never expires*, not "expired" and not "unknown". A caller
    /// computing "safe to reclaim" from expiry must treat null as never
    /// reclaimable on that basis.
    pub expires_at: Option<String>,
}

/// What GitPulse can see of a repository's DevCouncil state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskView {
    /// False when this repository has no DevCouncil store, which is the
    /// ordinary case and not a failure.
    pub available: bool,
    pub store_path: String,
    pub leases: Vec<TaskLease>,
    /// Empty when the store could be read; otherwise why it could not.
    ///
    /// Separate from `available`: "no store here" and "a store we failed to
    /// read" are different, and only the second is a problem to report.
    pub error: String,
}

fn parse_paths(json: &str) -> Vec<String> {
    // planned_files_json is an array of objects carrying `path`; the other
    // columns are arrays of plain strings. Accept both rather than assuming,
    // because a shape this code guessed wrong would silently yield an empty
    // scope — and an empty scope authorises nothing, so every write would be
    // refused with no hint why.
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| match item {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Object(o) => o.get("path")?.as_str().map(str::to_string),
            _ => None,
        })
        .collect()
}

/// Opens the store for `repo_path`, or reports why not.
fn open(repo_path: &str) -> Result<Option<dc_store::Store>, String> {
    let path = store_path(repo_path);
    if !path.exists() {
        return Ok(None);
    }
    dc_store::Store::open_existing(&path)
        .map(Some)
        .map_err(|e| format!("{e}"))
}

/// Active leases for a repository, with the store's availability.
pub fn view(repo_path: &str) -> TaskView {
    let path = store_path(repo_path).display().to_string();
    match open(repo_path) {
        Ok(None) => TaskView {
            available: false,
            store_path: path,
            leases: Vec::new(),
            error: String::new(),
        },
        Ok(Some(store)) => match store.active_leases() {
            Ok(leases) => TaskView {
                available: true,
                store_path: path,
                leases: leases
                    .into_iter()
                    .map(|l| TaskLease {
                        task_id: l.task_id,
                        owner: l.owner,
                        agent: l.agent,
                        branch: l.branch,
                        status: l.status,
                        created_at: l.created_at,
                        expires_at: l.expires_at,
                    })
                    .collect(),
                error: String::new(),
            },
            Err(e) => TaskView {
                available: true,
                store_path: path,
                leases: Vec::new(),
                error: format!("{e}"),
            },
        },
        Err(e) => TaskView {
            available: true,
            store_path: path,
            leases: Vec::new(),
            error: e,
        },
    }
}

/// One task's scope, or `None` when the store or the task is absent.
pub fn scope(repo_path: &str, task_id: &str) -> Result<Option<TaskScope>, String> {
    let Some(store) = open(repo_path)? else {
        return Ok(None);
    };
    let Some(task) = store.task(task_id).map_err(|e| format!("{e}"))? else {
        return Ok(None);
    };
    Ok(Some(TaskScope {
        id: task.id,
        title: task.title,
        status: task.status,
        planned_files: parse_paths(&task.planned_files_json),
        agent_appended_files: parse_paths(&task.agent_appended_planned_files_json),
        forbidden_changes: parse_paths(&task.forbidden_changes_json),
        allowed_commands: parse_paths(&task.allowed_commands_json),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_without_devcouncil_is_unavailable_not_broken() {
        // The ordinary case. It must not read as an error, and must not read
        // as a store that exists and happens to hold no leases.
        let dir = tempfile::tempdir().unwrap();
        let v = view(dir.path().to_str().unwrap());
        assert!(!v.available);
        assert!(v.error.is_empty(), "absence is not a failure: {}", v.error);
        assert!(v.leases.is_empty());
        assert!(v.store_path.ends_with("state.sqlite"));
    }

    #[test]
    fn scope_of_a_missing_store_is_none_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(scope(dir.path().to_str().unwrap(), "TASK-1").unwrap(), None);
    }

    #[test]
    fn reads_leases_and_scope_from_a_real_store() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_str().unwrap();
        let db = store_path(repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();

        // Written through dc-store itself, so this exercises the real schema
        // rather than a hand-rolled table that could drift from it.
        let store = dc_store::Store::open(&db).unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO tasks (id, title, description, planned_files_json,
                     forbidden_changes_json, allowed_commands_json, status)
                 VALUES ('TASK-1', 'Wire the ledger', '',
                     '[{\"path\":\"src/a.rs\",\"allowed_change\":\"modify\"}]',
                     '[\"src/secret.rs\"]', '[\"cargo test*\"]', 'in_progress')",
                [],
            )
            .unwrap();

        let s = scope(repo, "TASK-1").unwrap().expect("task");
        assert_eq!(s.id, "TASK-1");
        assert_eq!(s.planned_files, vec!["src/a.rs"]);
        assert_eq!(s.forbidden_changes, vec!["src/secret.rs"]);
        assert_eq!(s.allowed_commands, vec!["cargo test*"]);
        assert!(s.agent_appended_files.is_empty());

        let v = view(repo);
        assert!(v.available, "the store exists: {}", v.error);
        assert!(v.error.is_empty());
    }

    #[test]
    fn planned_files_parse_from_both_shapes() {
        // Objects carrying `path` (planned_files_json) and plain strings
        // (forbidden_changes_json) both occur in the real schema.
        assert_eq!(
            parse_paths(r#"[{"path":"a.rs"},{"path":"b.rs"}]"#),
            ["a.rs", "b.rs"]
        );
        assert_eq!(parse_paths(r#"["a.rs","b.rs"]"#), ["a.rs", "b.rs"]);
        assert_eq!(parse_paths("[]"), Vec::<String>::new());
    }

    #[test]
    fn unreadable_scope_json_yields_no_paths_rather_than_a_guess() {
        // An empty scope authorises nothing, so this fails closed. The
        // alternative — inventing a path list — would authorise writes nobody
        // planned.
        assert_eq!(parse_paths("not json"), Vec::<String>::new());
        assert_eq!(parse_paths(r#"{"path":"a.rs"}"#), Vec::<String>::new());
        assert_eq!(parse_paths(r#"[1,2,3]"#), Vec::<String>::new());
    }

    #[test]
    fn an_unreadable_store_is_reported_rather_than_shown_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_str().unwrap();
        let db = store_path(repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, b"this is not a sqlite database").unwrap();

        let v = view(repo);
        assert!(v.available, "the file is there, so this is not absence");
        assert!(
            !v.error.is_empty(),
            "a store we cannot read must not render as a store with no leases"
        );
    }
}
