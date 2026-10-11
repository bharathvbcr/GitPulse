//! Read-only view of DevCouncil's task and lease store.
//!
//! A worktree stops being a directory and becomes *a task in flight* once it is
//! bound to one. This module supplies the task half of that: which tasks exist,
//! which are leased, and what scope each declares — plus the evidence, gaps,
//! and verification runs that sit beside them in `state.sqlite`.
//!
//! # GitPulse never writes here
//!
//! Manvi / `dcstore` own `state.sqlite` — its schema, its migrations, and the
//! partial unique index the lease's mutual exclusion rests on. GitPulse links
//! `dc-store` to *read*, and must never acquire, renew or release a lease, and
//! never call `checkout_task` or `write_file`: those contend with an active
//! agent's writer lease, and a UI process that takes a lease strands the task
//! when the window closes. See `contracts/lease.schema.md`.
//!
//! # Absence is a state, not an error
//!
//! Most repositories have no DevCouncil store at all, and that is normal. A
//! missing store yields an empty view with `available: false`, never an error
//! and never an empty list that reads as "no tasks" — those are different facts
//! and the UI renders them differently.

pub mod file_tasks;

pub use file_tasks::{parse_brief, scan_briefs, BriefFile, BriefScan, TaskBrief};

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
    /// Set when the stored `expires_at` is not a timestamp: the lease is still
    /// marked active and this row alone cannot say whether it has lapsed, so it
    /// is listed with the stored text rather than dropped or called live. A
    /// readable lease carries null.
    pub expiry_error: Option<String>,
}

/// One evidence row from `dc-store`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskEvidence {
    pub id: i64,
    pub kind: String,
    pub task_id: Option<String>,
    pub requirement_id: Option<String>,
    pub acceptance_criterion_id: Option<String>,
    pub data_json: String,
}

/// One gap row from `dc-store`: every field `GapRow` stores, none dropped.
///
/// [`read_view`] destructures `GapRow` without `..`, so a field dc-store adds
/// is a compile error here until this struct carries it, and
/// `scripts/check-coverage-types.mjs` holds the TypeScript twin to this shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskGap {
    pub id: String,
    pub severity: String,
    pub gap_type: String,
    pub requirement_id: Option<String>,
    pub task_id: Option<String>,
    pub description: String,
    pub evidence_json: String,
    pub recommended_fix: String,
    pub blocking: bool,
    pub file: Option<String>,
    pub line: Option<i64>,
    pub suggested_command: Option<String>,
    pub acceptance_criterion_id: Option<String>,
    pub expected_verification_method: Option<String>,
    /// Where a failed verification command's captured output was written.
    pub stdout_path: Option<String>,
    pub stderr_path: Option<String>,
}

/// One verification run from `dc-store`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskRun {
    pub id: String,
    pub task_id: String,
    pub sandbox: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub commands_json: String,
}

/// What GitPulse can see of a repository's DevCouncil state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskView {
    /// False when this repository has no DevCouncil store, which is the
    /// ordinary case and not a failure.
    pub available: bool,
    pub store_path: String,
    pub leases: Vec<TaskLease>,
    pub evidence: Vec<TaskEvidence>,
    /// True when more evidence rows matched than the list ceiling returned.
    pub evidence_truncated: bool,
    pub gaps: Vec<TaskGap>,
    pub gaps_truncated: bool,
    pub runs: Vec<TaskRun>,
    pub runs_truncated: bool,
    /// Empty when the store could be read; otherwise why it could not.
    ///
    /// Separate from `available`: "no store here" and "a store we failed to
    /// read" are different, and only the second is a problem to report.
    pub error: String,
}

fn empty_view(path: String, available: bool, error: String) -> TaskView {
    TaskView {
        available,
        store_path: path,
        leases: Vec::new(),
        evidence: Vec::new(),
        evidence_truncated: false,
        gaps: Vec::new(),
        gaps_truncated: false,
        runs: Vec::new(),
        runs_truncated: false,
        error,
    }
}

/// Reads one scope column, or says which part of it cannot be read.
///
/// A column that cannot be read is an error, never an empty list: an empty
/// `forbidden_changes` means "nothing is forbidden" and an empty
/// `allowed_commands` drops the task's allowlist, so reading damage as empty
/// would widen what the gate permits. Callers surface the error as a broken
/// scope (see `harness::scope_for`).
fn parse_paths(column: &str, json: &str) -> Result<Vec<String>, String> {
    // planned_files_json is an array of objects carrying `path`; the other
    // columns are arrays of plain strings. Accept both.
    let value = serde_json::from_str::<serde_json::Value>(json)
        .map_err(|e| format!("task scope column {column} is not valid JSON: {e}"))?;
    let items = value
        .as_array()
        .ok_or_else(|| format!("task scope column {column} is not a JSON array"))?;
    items
        .iter()
        .enumerate()
        .map(|(i, item)| match item {
            serde_json::Value::String(s) => Ok(s.clone()),
            serde_json::Value::Object(o) => o
                .get("path")
                .and_then(|p| p.as_str())
                .map(str::to_string)
                .ok_or_else(|| {
                    format!("task scope column {column} entry {i} has no string `path`")
                }),
            _ => Err(format!(
                "task scope column {column} entry {i} is neither a string nor an object"
            )),
        })
        .collect()
}

/// Opens the store for `repo_path` for reading only, or reports why not.
///
/// `SQLITE_OPEN_READ_ONLY`: a view can never migrate the store, sweep a lease
/// or contend for its write lock, whatever the agent that owns it is doing.
fn open(repo_path: &str) -> Result<Option<dc_store::Store>, String> {
    let path = store_path(repo_path);
    if !path.exists() {
        return Ok(None);
    }
    dc_store::Store::open_readonly(&path)
        .map(Some)
        .map_err(|e| format!("{e}"))
}

/// Active leases for a repository, with the store's availability.
pub fn view(repo_path: &str) -> TaskView {
    view_filtered(repo_path, None)
}

/// Like [`view`], optionally filtering evidence / gaps / runs by task id.
pub fn view_filtered(repo_path: &str, task_id: Option<&str>) -> TaskView {
    let path = store_path(repo_path).display().to_string();
    match open(repo_path) {
        Ok(None) => empty_view(path, false, String::new()),
        Ok(Some(store)) => match read_view(&store, path.clone(), task_id) {
            Ok(view) => view,
            Err(e) => empty_view(path, false, e),
        },
        Err(e) => empty_view(path, false, e),
    }
}

fn read_view(
    store: &dc_store::Store,
    path: String,
    task_id: Option<&str>,
) -> Result<TaskView, String> {
    // `observe_leases` judges expiry in memory and never writes, which a
    // read-only handle requires. A lapsed lease is left as it is and not
    // listed (it is no longer held); one whose expiry cannot be read is listed
    // with that text, for its row alone.
    let leases = store
        .observe_leases()
        .map_err(|e| format!("{e}"))?
        .into_iter()
        .filter(|(_, state)| *state != dc_store::LeaseState::Expired)
        .map(|(l, state)| TaskLease {
            task_id: l.task_id,
            owner: l.owner,
            agent: l.agent,
            branch: l.branch,
            status: l.status,
            created_at: l.created_at,
            expires_at: l.expires_at,
            expiry_error: match state {
                dc_store::LeaseState::BadExpiry(stored) => Some(stored),
                _ => None,
            },
        })
        .collect();

    let (evidence_rows, evidence_truncated) = store
        .evidence_list(task_id)
        .map_err(|e| format!("evidence: {e}"))?;
    let evidence = evidence_rows
        .into_iter()
        .map(|row| TaskEvidence {
            id: row.id,
            kind: row.kind,
            task_id: row.task_id,
            requirement_id: row.requirement_id,
            acceptance_criterion_id: row.acceptance_criterion_id,
            data_json: row.data_json,
        })
        .collect();

    let (gap_rows, gaps_truncated) = store.gaps_list(task_id).map_err(|e| format!("gaps: {e}"))?;
    let gaps = gap_rows
        .into_iter()
        .map(|row| {
            // Exhaustive on purpose — no `..`. A field dc-store starts keeping
            // must fail to compile here, not vanish from the view.
            let dc_store::records::GapRow {
                id,
                severity,
                gap_type,
                requirement_id,
                task_id,
                description,
                evidence_json,
                recommended_fix,
                blocking,
                file,
                line,
                suggested_command,
                acceptance_criterion_id,
                expected_verification_method,
                stdout_path,
                stderr_path,
            } = row;
            TaskGap {
                id,
                severity,
                gap_type,
                requirement_id,
                task_id,
                description,
                evidence_json,
                recommended_fix,
                blocking,
                file,
                line,
                suggested_command,
                acceptance_criterion_id,
                expected_verification_method,
                stdout_path,
                stderr_path,
            }
        })
        .collect();

    let (run_rows, runs_truncated) = store.runs_list(task_id).map_err(|e| format!("runs: {e}"))?;
    let runs = run_rows
        .into_iter()
        .map(|row| TaskRun {
            id: row.id,
            task_id: row.task_id,
            sandbox: row.sandbox,
            status: row.status,
            started_at: row.started_at,
            finished_at: row.finished_at,
            commands_json: row.commands_json,
        })
        .collect();

    Ok(TaskView {
        available: true,
        store_path: path,
        leases,
        evidence,
        evidence_truncated,
        gaps,
        gaps_truncated,
        runs,
        runs_truncated,
        error: String::new(),
    })
}

/// One task's scope, or `None` when the store or the task is absent.
pub fn scope(repo_path: &str, task_id: &str) -> Result<Option<TaskScope>, String> {
    let Some(store) = open(repo_path)? else {
        return Ok(None);
    };
    scope_in(&store, task_id)
}

/// Most task ids one [`scopes`] call reads.
pub const MAX_SCOPE_IDS: usize = 64;

/// The scopes of several tasks, read from one opening of the store.
///
/// Ids the store does not hold are absent from the answer, as [`scope`]
/// answers `None` for them, and a repeated id is read once. More distinct ids
/// than [`MAX_SCOPE_IDS`] are refused rather than cut, so a caller can never
/// take a capped answer for a complete one.
pub fn scopes(repo_path: &str, task_ids: &[String]) -> Result<Vec<TaskScope>, String> {
    let mut wanted: Vec<&str> = Vec::new();
    for id in task_ids {
        if !wanted.contains(&id.as_str()) {
            wanted.push(id);
        }
    }
    if wanted.len() > MAX_SCOPE_IDS {
        return Err(format!(
            "{} task ids were asked for in one read; the limit is {MAX_SCOPE_IDS}",
            wanted.len()
        ));
    }
    let Some(store) = open(repo_path)? else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    for id in wanted {
        if let Some(scope) = scope_in(&store, id)? {
            found.push(scope);
        }
    }
    Ok(found)
}

fn scope_in(store: &dc_store::Store, task_id: &str) -> Result<Option<TaskScope>, String> {
    let Some(task) = store.task(task_id).map_err(|e| format!("{e}"))? else {
        return Ok(None);
    };
    Ok(Some(TaskScope {
        id: task.id,
        title: task.title,
        status: task.status,
        planned_files: parse_paths("planned_files_json", &task.planned_files_json)?,
        agent_appended_files: parse_paths(
            "agent_appended_planned_files_json",
            &task.agent_appended_planned_files_json,
        )?,
        forbidden_changes: parse_paths("forbidden_changes_json", &task.forbidden_changes_json)?,
        allowed_commands: parse_paths("allowed_commands_json", &task.allowed_commands_json)?,
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
        assert!(v.evidence.is_empty());
        assert!(v.gaps.is_empty());
        assert!(v.runs.is_empty());
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
    fn reads_evidence_gaps_and_runs_without_taking_a_lease() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_str().unwrap();
        let db = store_path(repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = dc_store::Store::open(&db).unwrap();
        store
            .evidence_append(
                "command",
                Some("TASK-1"),
                None,
                None,
                r#"{"cmd":"cargo test"}"#,
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO gaps (id, severity, gap_type, description, evidence_json,
                     recommended_fix, blocking, task_id)
                 VALUES ('g1', 'high', 'coverage', 'missing test', '{}', 'add test', 1, 'TASK-1')",
                [],
            )
            .unwrap();
        store
            .run_record(&dc_store::records::VerificationRun {
                id: "run-1".into(),
                task_id: "TASK-1".into(),
                sandbox: "local".into(),
                environment_json: "{}".into(),
                commands_json: r#"["cargo test"]"#.into(),
                status: "passed".into(),
                started_at: "2026-01-01T00:00:00Z".into(),
                finished_at: Some("2026-01-01T00:01:00Z".into()),
            })
            .unwrap();
        drop(store);

        let v = view_filtered(repo, Some("TASK-1"));
        assert!(v.available, "{}", v.error);
        assert_eq!(v.evidence.len(), 1);
        assert_eq!(v.evidence[0].kind, "command");
        assert!(!v.evidence_truncated);
        assert_eq!(v.gaps.len(), 1);
        assert_eq!(v.gaps[0].id, "g1");
        assert_eq!(v.runs.len(), 1);
        assert_eq!(v.runs[0].id, "run-1");
        // Still read-only: no lease was created by this view.
        assert!(v.leases.is_empty());
    }

    #[test]
    fn a_store_from_before_the_gap_output_columns_still_reads_its_gaps() {
        // `stdout_path` / `stderr_path` were added to `gaps` after schema 9
        // shipped, without a schema bump, so a long-lived store can lack them
        // while claiming the current version. The view opens the store read-only
        // and so cannot add them; what makes that safe is dc-store's `gaps_list`
        // reading a missing column as absent. Without it this view reads "no such
        // column" and reports the store unavailable.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_str().unwrap();
        let db = store_path(repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = dc_store::Store::open(&db).unwrap();
        store
            .connection()
            .execute_batch(
                "ALTER TABLE gaps DROP COLUMN stdout_path;
                 ALTER TABLE gaps DROP COLUMN stderr_path;
                 INSERT INTO gaps (id, severity, gap_type, description, evidence_json,
                     recommended_fix, blocking, task_id)
                 VALUES ('g-old', 'high', 'coverage', 'missing test', '{}', 'add test', 1, 'TASK-1');",
            )
            .unwrap();
        drop(store);

        let v = view_filtered(repo, Some("TASK-1"));
        assert!(
            v.available,
            "a pre-column store must still read: {}",
            v.error
        );
        assert_eq!(v.gaps.len(), 1);
        assert_eq!(v.gaps[0].id, "g-old");
        assert_eq!(v.gaps[0].stdout_path, None);
        assert_eq!(v.gaps[0].stderr_path, None);
    }

    #[test]
    fn every_gap_field_dc_store_keeps_reaches_the_view() {
        // Three rounds of the same bug: dc-store learned to keep a gap field
        // and this view dropped it. Each optional field is written through
        // dc-store's own upsert and must come back as itself, and an unset one
        // must come back as None — "" would read as a field set to nothing.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_str().unwrap();
        let db = store_path(repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = dc_store::Store::open(&db).unwrap();
        store
            .gap_upsert(&dc_store::records::GapRow {
                id: "g-full".into(),
                severity: "high".into(),
                gap_type: "failed_command".into(),
                requirement_id: Some("REQ-7".into()),
                task_id: Some("TASK-1".into()),
                description: "cargo test failed".into(),
                evidence_json: r#"{"exit":101}"#.into(),
                recommended_fix: "fix the failing test".into(),
                blocking: true,
                file: Some("src/a.rs".into()),
                line: Some(42),
                suggested_command: Some("cargo test -p a".into()),
                acceptance_criterion_id: Some("AC-3".into()),
                expected_verification_method: Some("unit_test".into()),
                stdout_path: Some(".devcouncil/runs/r1/stdout.log".into()),
                stderr_path: Some(".devcouncil/runs/r1/stderr.log".into()),
            })
            .unwrap();
        store
            .gap_upsert(&dc_store::records::GapRow {
                id: "g-bare".into(),
                severity: "low".into(),
                gap_type: "coverage".into(),
                requirement_id: None,
                task_id: Some("TASK-1".into()),
                description: "missing test".into(),
                evidence_json: "{}".into(),
                recommended_fix: "add a test".into(),
                blocking: false,
                file: None,
                line: None,
                suggested_command: None,
                acceptance_criterion_id: None,
                expected_verification_method: None,
                stdout_path: None,
                stderr_path: None,
            })
            .unwrap();
        drop(store);

        let v = view_filtered(repo, Some("TASK-1"));
        assert!(v.available, "{}", v.error);
        let gap = |id: &str| {
            v.gaps
                .iter()
                .find(|g| g.id == id)
                .unwrap_or_else(|| panic!("gap {id} missing from {:?}", v.gaps))
        };

        let full = gap("g-full");
        assert_eq!(
            full,
            &TaskGap {
                id: "g-full".into(),
                severity: "high".into(),
                gap_type: "failed_command".into(),
                requirement_id: Some("REQ-7".into()),
                task_id: Some("TASK-1".into()),
                description: "cargo test failed".into(),
                evidence_json: r#"{"exit":101}"#.into(),
                recommended_fix: "fix the failing test".into(),
                blocking: true,
                file: Some("src/a.rs".into()),
                line: Some(42),
                suggested_command: Some("cargo test -p a".into()),
                acceptance_criterion_id: Some("AC-3".into()),
                expected_verification_method: Some("unit_test".into()),
                stdout_path: Some(".devcouncil/runs/r1/stdout.log".into()),
                stderr_path: Some(".devcouncil/runs/r1/stderr.log".into()),
            }
        );

        let bare = gap("g-bare");
        assert_eq!(bare.requirement_id, None);
        assert_eq!(bare.file, None);
        assert_eq!(bare.line, None);
        assert_eq!(bare.suggested_command, None);
        assert_eq!(bare.acceptance_criterion_id, None);
        assert_eq!(bare.expected_verification_method, None);
        assert_eq!(bare.stdout_path, None);
        assert_eq!(bare.stderr_path, None);

        // And on the wire every field is present, unset ones as null, so the
        // MCP tool and resource (which serialize this view) carry them too.
        let wire = serde_json::to_value(bare).unwrap();
        for key in [
            "requirement_id",
            "suggested_command",
            "acceptance_criterion_id",
            "expected_verification_method",
            "stdout_path",
            "stderr_path",
        ] {
            assert_eq!(wire.get(key), Some(&serde_json::Value::Null), "{key}");
        }
        let wire = serde_json::to_value(full).unwrap();
        assert_eq!(wire["stdout_path"], ".devcouncil/runs/r1/stdout.log");
        assert_eq!(wire["requirement_id"], "REQ-7");
    }

    #[test]
    fn planned_files_parse_from_both_shapes() {
        // Objects carrying `path` (planned_files_json) and plain strings
        // (forbidden_changes_json) both occur in the real schema.
        assert_eq!(
            parse_paths("c", r#"[{"path":"a.rs"},{"path":"b.rs"}]"#).unwrap(),
            ["a.rs", "b.rs"]
        );
        assert_eq!(
            parse_paths("c", r#"["a.rs","b.rs"]"#).unwrap(),
            ["a.rs", "b.rs"]
        );
        assert_eq!(parse_paths("c", "[]").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn unreadable_scope_json_is_an_error_not_an_empty_list() {
        // Reason this test changed: it used to assert these parse to an empty
        // list "because an empty scope authorises nothing". That holds for
        // planned_files but is backwards for forbidden_changes ("nothing
        // forbidden") and allowed_commands (allowlist dropped), so every
        // unreadable column must be reported and the gate must treat the scope
        // as broken.
        for bad in [
            "not json",
            r#"["secrets/**""#,
            r#"{"path":"a.rs"}"#,
            "[1,2,3]",
            r#"["ok",{"nopath":1}]"#,
            "",
        ] {
            let err = parse_paths("forbidden_changes_json", bad).unwrap_err();
            assert!(err.contains("forbidden_changes_json"), "{bad}: {err}");
        }
    }

    #[test]
    fn a_task_with_a_damaged_scope_column_has_no_readable_scope() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_str().unwrap();
        let db = store_path(repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = dc_store::Store::open(&db).unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO tasks (id, title, description, planned_files_json,
                     forbidden_changes_json, allowed_commands_json, status)
                 VALUES ('TASK-2', 'Damaged', '', '[]', '[\"secrets/**\"', '[]', 'in_progress')",
                [],
            )
            .unwrap();
        let err = scope(repo, "TASK-2").unwrap_err();
        assert!(err.contains("forbidden_changes_json"), "{err}");
        let err = scopes(repo, &["TASK-2".to_string()]).unwrap_err();
        assert!(err.contains("forbidden_changes_json"), "{err}");
    }

    #[test]
    fn an_unreadable_store_is_reported_rather_than_shown_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_str().unwrap();
        let db = store_path(repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, b"this is not a sqlite database").unwrap();

        let v = view(repo);
        assert!(
            !v.available,
            "a store we could not read must not claim its lease list was read"
        );
        assert!(
            !v.error.is_empty(),
            "a store we cannot read must not render as a store with no leases"
        );
        assert!(v.leases.is_empty());
    }

    /// The three answers this view has to keep apart.
    ///
    /// `available: true` with an empty list is the machine-readable statement
    /// "I read the store; there are no active leases". Two of these three
    /// cases never read anything, and neither may say that. `error` is what
    /// separates the two that did not: empty means there was nothing to read,
    /// non-empty means there was and we could not.
    #[test]
    fn available_means_the_leases_were_read_not_that_a_file_was_present() {
        let absent = tempfile::tempdir().unwrap();
        let absent_view = view(absent.path().to_str().unwrap());
        assert!(!absent_view.available);
        assert!(absent_view.error.is_empty(), "absence is not a failure");

        let broken = tempfile::tempdir().unwrap();
        let broken_db = store_path(broken.path().to_str().unwrap());
        std::fs::create_dir_all(broken_db.parent().unwrap()).unwrap();
        std::fs::write(&broken_db, b"not a database").unwrap();
        let broken_view = view(broken.path().to_str().unwrap());
        assert!(!broken_view.available);
        assert!(!broken_view.error.is_empty(), "a failure must say so");

        let real = tempfile::tempdir().unwrap();
        let real_db = store_path(real.path().to_str().unwrap());
        std::fs::create_dir_all(real_db.parent().unwrap()).unwrap();
        dc_store::Store::open(&real_db).unwrap();
        let real_view = view(real.path().to_str().unwrap());
        assert!(
            real_view.available,
            "a store that opened and answered must be available: {}",
            real_view.error
        );
        assert!(real_view.error.is_empty());
        assert!(real_view.leases.is_empty(), "nothing was leased");
    }

    /// A lease row as a writer would have left it.
    fn lease_row(db: &Path, task: &str, expires_at: Option<&str>) {
        let conn = rusqlite::Connection::open(db).unwrap();
        conn.execute(
            "INSERT INTO task_leases (id, task_id, owner, lease_token, status, created_at, expires_at)
             VALUES (?1, ?1, 'agent', 'token', 'active', '2026-01-01T00:00:00Z', ?2)",
            rusqlite::params![task, expires_at],
        )
        .unwrap();
    }

    fn lease_status(db: &Path, task: &str) -> String {
        rusqlite::Connection::open(db)
            .unwrap()
            .query_row(
                "SELECT status FROM task_leases WHERE id = ?1",
                [task],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// The view is a read. It used to open the store read-write and sweep every
    /// lapsed lease to `stale` on the way, so looking at a repository changed
    /// the file an agent was working against; and one lease with an unreadable
    /// expiry failed the whole listing.
    #[test]
    fn a_view_writes_nothing_and_judges_each_lease_by_itself() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_str().unwrap();
        let db = store_path(repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        drop(dc_store::Store::open(&db).unwrap());
        lease_row(&db, "live", Some("2999-01-01T00:00:00Z"));
        lease_row(&db, "lapsed", Some("2001-01-01T00:00:00Z"));
        lease_row(&db, "garbled", Some("sometime next week"));
        lease_row(&db, "forever", None);

        let v = view(repo);
        assert!(v.available, "{}", v.error);
        assert!(
            v.error.is_empty(),
            "one bad row does not fail the view: {}",
            v.error
        );
        let listed: Vec<(&str, Option<&str>)> = v
            .leases
            .iter()
            .map(|l| (l.task_id.as_str(), l.expiry_error.as_deref()))
            .collect();
        assert_eq!(
            listed,
            vec![
                ("forever", None),
                ("garbled", Some("sometime next week")),
                ("live", None),
            ],
            "a lapsed lease is not held; an unreadable one is shown, not hidden"
        );

        // Nothing was swept: the lapsed row is still marked active.
        assert_eq!(lease_status(&db, "lapsed"), "active");
        // And reading again says the same thing.
        assert_eq!(view(repo).leases.len(), 3);
    }

    /// Scopes are read through the same read-only handle.
    #[test]
    fn scope_reads_a_store_it_cannot_write() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_str().unwrap();
        let db = store_path(repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = dc_store::Store::open(&db).unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO tasks (id, title, description, planned_files_json,
                     forbidden_changes_json, allowed_commands_json, status)
                 VALUES ('TASK-7', 'Read only', '', '[\"a.rs\"]', '[]', '[]', 'in_progress')",
                [],
            )
            .unwrap();
        drop(store);
        let mut permissions = std::fs::metadata(&db).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&db, permissions).unwrap();

        let found = scopes(repo, &["TASK-7".to_owned()]).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].planned_files, vec!["a.rs"]);
        assert_eq!(scope(repo, "TASK-7").unwrap().unwrap().id, "TASK-7");
        let mut permissions = std::fs::metadata(&db).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&db, permissions).unwrap();
    }
}
