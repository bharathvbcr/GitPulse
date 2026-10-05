//! Task intake: the one path by which a task authored outside the board — an
//! agent over MCP, or a brief in a repository's `tasks/` folder — becomes a
//! workbench item, which is the only thing the task board renders.
//!
//! Before this existed, `gitpulse_add_task` wrote `tasks/<id>.md` and nothing
//! in GitPulse read that folder: an agent was told its task was recorded, and
//! the person it was recorded for could not see it anywhere.
//!
//! # Identity
//!
//! An external task is keyed by (repository, task key), and its workbench id is
//! derived from both. A retried add or a re-run import therefore finds the item
//! it made instead of making a second one; the same key in another repository
//! is a different task; and a task someone deleted on the board stays deleted,
//! because the store refuses to reuse a deleted id.
//!
//! # What does not fit
//!
//! The workbench has no planned-files field, and it links registered
//! repositories rather than names. Planned files, and repository names other
//! than the one the task is filed under, are folded into the description under
//! their own headings — kept visible, not dropped.

use super::{query, WorkbenchError};
use crate::tasks::file_tasks::{self, TaskBrief};
use dc_store::Store;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The store refuses a deleted id with exactly this message; `place` turns it
/// into an outcome. Pinned by a test so a wording change upstream fails loudly.
const DELETED_ID_REFUSAL: &str = "deleted task IDs cannot be reused";
const ITEM_PREFIX: &str = "ft-";

/// Where the profile database lives when no test overrides it.
pub(crate) fn default_profile_path() -> Result<PathBuf, WorkbenchError> {
    crate::tool_config::default_config_dir()
        .map(|dir| dir.join("workbench.sqlite"))
        .ok_or_else(|| {
            WorkbenchError::new(
                "store_error",
                "Cannot resolve the GitPulse profile directory.",
            )
        })
}

// ---------------------------------------------------------------------------
// Repositories
// ---------------------------------------------------------------------------

/// A local checkout, resolved to the identity the board registers it under.
pub(crate) struct LocalRepository {
    pub path: String,
    pub name: String,
    pub is_bare: bool,
    /// `local:<common git dir>`, so every linked worktree is one repository.
    pub identity: String,
}

pub(crate) fn resolve(repo_path: &str) -> Result<LocalRepository, WorkbenchError> {
    let resolved = crate::engine::git_cli::resolve_repo(repo_path)
        .map_err(|e| WorkbenchError::new("repository_unavailable", e))?;
    let common = crate::engine::git_cli::resolve_git_common_dir(Path::new(&resolved.path))
        .map_err(|e| WorkbenchError::new("repository_unavailable", e))?;
    let common = common.to_str().ok_or_else(|| {
        WorkbenchError::new("invalid_input", "Repository path is not valid Unicode.")
    })?;
    Ok(LocalRepository {
        identity: format!("local:{common}"),
        path: resolved.path,
        name: resolved.name,
        is_bare: resolved.is_bare,
    })
}

/// The registered record for a repository, if the board has one. Never writes.
pub(crate) fn find(store: &Store, identity: &str) -> Result<Option<Value>, WorkbenchError> {
    let mut cursor: Option<String> = None;
    for _ in 0..50 {
        let mut input = json!({"limit":200});
        if let Some(cursor) = &cursor {
            input["cursor"] = json!(cursor);
        }
        let page = query(store, "repositories.list", &input.to_string())?;
        let records = page["items"]
            .as_array()
            .ok_or_else(|| WorkbenchError::new("protocol_error", "Invalid repository page."))?;
        if let Some(repository) = records
            .iter()
            .find(|r| r["identity_key"].as_str() == Some(identity))
        {
            return Ok(Some(repository.clone()));
        }
        if page["has_more"] != true {
            return Ok(None);
        }
        let next = page["next_cursor"]
            .as_str()
            .ok_or_else(|| WorkbenchError::new("protocol_error", "Missing repository cursor."))?;
        if cursor.as_deref() == Some(next) {
            return Err(WorkbenchError::new(
                "protocol_error",
                "Repository cursor did not advance.",
            ));
        }
        cursor = Some(next.into());
    }
    Err(WorkbenchError::new(
        "registry_limit",
        "Repository registration requires an indexed identity lookup beyond 10,000 repositories.",
    ))
}

/// Find or register a repository. The board's own registration goes through
/// here too, so an agent's task and a person's land under one record.
pub(crate) fn register(
    store: &Store,
    local: &LocalRepository,
    id: &str,
    request_id: &str,
) -> Result<Value, WorkbenchError> {
    if let Some(repository) = find(store, &local.identity)? {
        return Ok(json!({"repository":repository,"path":local.path,"is_bare":local.is_bare}));
    }
    let input = json!({"id":id,"request_id":request_id,"expected_revision":0,"name":local.name,"identity_key":local.identity});
    match query(store, "repositories.put", &input.to_string()) {
        Ok(response) => Ok(
            json!({"repository":response["item"],"path":local.path,"is_bare":local.is_bare,"sequence":response["sequence"]}),
        ),
        // Two first registrations raced — the board opening the repository
        // while an agent files into it. The loser adopts the winner's record.
        Err(error) => match find(store, &local.identity)? {
            Some(repository) => {
                Ok(json!({"repository":repository,"path":local.path,"is_bare":local.is_bare}))
            }
            None => Err(error),
        },
    }
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

/// A task from outside the board, before it is placed on it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ExternalTask {
    pub key: String,
    pub title: String,
    pub description: String,
    pub status: Option<String>,
    pub priority: Option<u8>,
    pub severity: Option<String>,
    pub kind: Option<String>,
    pub owner: Option<String>,
    pub due: Option<String>,
    pub labels: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub planned_files: Vec<String>,
    pub repositories: Vec<String>,
    pub logs: Option<String>,
}

impl ExternalTask {
    pub(crate) fn from_brief(key: String, brief: TaskBrief) -> Self {
        Self {
            key,
            title: brief.title,
            description: brief.description,
            status: brief.status,
            priority: brief.priority,
            severity: brief.severity,
            kind: brief.kind,
            owner: brief.owner,
            due: brief.due,
            labels: brief.labels,
            acceptance_criteria: brief.acceptance_criteria,
            planned_files: brief.planned_files,
            repositories: brief.repositories,
            logs: brief.logs,
        }
    }

    /// The fields an `items.put` carries, checked against the board's rules
    /// here so a refusal names the field rather than surfacing as a store code.
    fn item_fields(&self, repository_name: &str) -> Result<Map<String, Value>, String> {
        file_tasks::validate_task_key(&self.key)?;
        let title = file_tasks::validate_title(&self.title)?;
        let status = match &self.status {
            Some(status) => file_tasks::normalize_status(status)?,
            None => "inbox".into(),
        };
        let priority = self.priority.unwrap_or(2);
        if priority > 3 {
            return Err(format!(
                "priority {priority} is out of range; use 0 (urgent) to 3 (low)"
            ));
        }
        let severity = match &self.severity {
            Some(severity) => file_tasks::parse_severity(severity)?,
            None => None,
        };
        let kind = self
            .kind
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .unwrap_or("feature")
            .to_ascii_lowercase();
        if kind.len() > file_tasks::MAX_KIND_BYTES || kind.chars().any(char::is_control) {
            return Err(format!(
                "type exceeds {} bytes or spans lines",
                file_tasks::MAX_KIND_BYTES
            ));
        }
        let owner = self
            .owner
            .as_deref()
            .map(str::trim)
            .filter(|o| !o.is_empty());
        if owner.is_some_and(|o| {
            o.len() > file_tasks::MAX_OWNER_BYTES || o.chars().any(char::is_control)
        }) {
            return Err(format!(
                "owner exceeds {} bytes or spans lines",
                file_tasks::MAX_OWNER_BYTES
            ));
        }
        let bounded = |items: &[String],
                       max: usize,
                       bytes: usize,
                       name: &str|
         -> Result<Vec<String>, String> {
            let mut out: Vec<String> = Vec::new();
            for item in items.iter().map(|i| i.trim()).filter(|i| !i.is_empty()) {
                if item.len() > bytes || item.chars().any(char::is_control) {
                    return Err(format!(
                        "a {name} item exceeds {bytes} bytes or spans lines"
                    ));
                }
                if !out.iter().any(|seen| seen == item) {
                    out.push(item.to_string());
                }
            }
            if out.len() > max {
                return Err(format!("{name} has {} items; at most {max}", out.len()));
            }
            Ok(out)
        };
        let labels = bounded(
            &self.labels,
            file_tasks::MAX_TASK_LABELS,
            file_tasks::MAX_LABEL_BYTES,
            "labels",
        )?;
        let criteria = bounded(
            &self.acceptance_criteria,
            file_tasks::MAX_TASK_CRITERIA,
            file_tasks::MAX_CRITERION_BYTES,
            "acceptance_criteria",
        )?;
        let planned = bounded(
            &self.planned_files,
            file_tasks::MAX_TASK_PLANNED_FILES,
            file_tasks::MAX_PLANNED_FILE_BYTES,
            "planned_files",
        )?;
        let others: Vec<String> = bounded(
            &self.repositories,
            file_tasks::MAX_TASK_REPOSITORIES,
            file_tasks::MAX_REPOSITORY_BYTES,
            "repositories",
        )?
        .into_iter()
        .filter(|name| !name.eq_ignore_ascii_case(repository_name))
        .collect();
        let description = fold_description(&self.description, &planned, &others);
        if description.contains('\0') || description.len() > file_tasks::MAX_TASK_DESCRIPTION {
            return Err(format!(
                "description with planned files exceeds {} bytes",
                file_tasks::MAX_TASK_DESCRIPTION
            ));
        }
        let logs = self.logs.as_deref().map(str::trim_end).unwrap_or("");
        if logs.contains('\0') || logs.len() > file_tasks::MAX_TASK_LOGS {
            return Err(format!(
                "raw logs exceed {} bytes or contain NUL",
                file_tasks::MAX_TASK_LOGS
            ));
        }
        let mut fields = Map::new();
        fields.insert("title".into(), json!(title));
        fields.insert("description".into(), json!(description));
        fields.insert("kind".into(), json!(kind));
        fields.insert("status".into(), json!(status));
        fields.insert("priority".into(), json!(priority));
        fields.insert("severity".into(), json!(severity));
        fields.insert("owner".into(), json!(owner));
        if let Some(due_at) = parse_due(self.due.as_deref())? {
            fields.insert("due_at".into(), json!(due_at));
        }
        fields.insert("labels".into(), json!(labels));
        fields.insert("acceptance_criteria".into(), json!(criteria));
        // An explicit empty string clears; omission would keep stale evidence.
        fields.insert("logs".into(), json!(logs));
        Ok(fields)
    }
}

fn fold_description(description: &str, planned: &[String], repositories: &[String]) -> String {
    let mut out = description.trim().to_string();
    for (heading, items) in [
        ("Planned files", planned),
        ("Related repositories", repositories),
    ] {
        if items.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str("## ");
        out.push_str(heading);
        for item in items {
            out.push_str("\n- ");
            out.push_str(item);
        }
    }
    out
}

/// `YYYY-MM-DD` (placed at 12:00 UTC, which is that calendar day in every
/// time zone from UTC−11 to UTC+11), or Unix seconds.
pub(crate) fn parse_due(raw: Option<&str>) -> Result<Option<i64>, String> {
    let Some(raw) = raw
        .map(str::trim)
        .filter(|r| !r.is_empty() && !r.eq_ignore_ascii_case("none"))
    else {
        return Ok(None);
    };
    const MAX: i64 = 9_007_199_254_740_990;
    if raw.bytes().all(|b| b.is_ascii_digit()) {
        return raw
            .parse::<i64>()
            .ok()
            .filter(|v| *v <= MAX)
            .map(Some)
            .ok_or_else(|| format!("due {raw:?} is out of range"));
    }
    let parts: Vec<&str> = raw.split('-').collect();
    let date = match parts.as_slice() {
        [y, m, d] if y.len() == 4 && m.len() == 2 && d.len() == 2 => {
            match (y.parse::<i64>(), m.parse::<u32>(), d.parse::<u32>()) {
                (Ok(y), Ok(m), Ok(d)) => Some((y, m, d)),
                _ => None,
            }
        }
        _ => None,
    };
    let (year, month, day) =
        date.ok_or_else(|| format!("due {raw:?} is not YYYY-MM-DD or Unix seconds"))?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1970..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month[month as usize - 1]
    {
        return Err(format!(
            "due {raw:?} is not a calendar date on or after 1970-01-01"
        ));
    }
    // Days from civil (Howard Hinnant), proleptic Gregorian.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Ok(Some(days * 86_400 + 12 * 3_600))
}

/// The workbench id of an external task: one per (repository, key).
pub(crate) fn item_id(repository_id: &str, key: &str) -> String {
    let fnv = |offset: u64| {
        let mut hash = offset;
        for byte in repository_id.bytes().chain([0x1f]).chain(key.bytes()) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    };
    format!(
        "{ITEM_PREFIX}{:016x}{:016x}",
        fnv(0xcbf2_9ce4_8422_2325),
        fnv(0x84222325_cbf29ce4)
    )
}

/// A fresh opaque id: process, clock and a counter, so two writers never share one.
fn fresh_id(prefix: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{prefix}-{}-{nanos:x}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// What placing a task did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Outcome {
    Created,
    Updated,
    Unchanged,
    /// On the board already and `replace` was not asked for; left alone.
    AlreadyPresent,
    /// Someone deleted it on the board. Deletion wins over re-import.
    DeletedOnBoard,
}

pub(crate) struct Placed {
    pub outcome: Outcome,
    pub item_id: String,
    pub item: Option<Value>,
    pub sequence: Option<u64>,
}

fn links(item: &Value, repository_id: &str) -> bool {
    item["repository_ids"]
        .as_array()
        .is_some_and(|ids| ids.iter().any(|v| v.as_str() == Some(repository_id)))
}

fn get_item(store: &Store, id: &str) -> Result<Option<Value>, WorkbenchError> {
    match query(store, "items.get", &json!({"id":id}).to_string()) {
        Ok(response) => Ok(Some(response["item"].clone())),
        Err(error) if error.code == "not_found" => Ok(None),
        Err(error) => Err(error),
    }
}

/// Put one task on the board under `repository` (a registered record).
pub(crate) fn place(
    store: &Store,
    repository: &Value,
    task: &ExternalTask,
    replace: bool,
    position: i64,
) -> Result<Placed, WorkbenchError> {
    place_once(store, repository, task, replace, position, false)
}

fn place_once(
    store: &Store,
    repository: &Value,
    task: &ExternalTask,
    replace: bool,
    position: i64,
    retried: bool,
) -> Result<Placed, WorkbenchError> {
    let repository_id = repository["id"]
        .as_str()
        .ok_or_else(|| WorkbenchError::new("protocol_error", "Repository record has no id."))?;
    let repository_name = repository["name"].as_str().unwrap_or_default();
    let fields = task
        .item_fields(repository_name)
        .map_err(|message| WorkbenchError::new("invalid_input", message))?;
    let id = item_id(repository_id, &task.key);
    let current = get_item(store, &id)?;
    // The id is derived from this repository, so an item under it that this
    // repository does not link was not made here. Refuse rather than report it
    // as this task, or replace someone else's: the hash's quality stops mattering.
    if current.as_ref().is_some_and(|item| !links(item, repository_id)) {
        return Err(WorkbenchError::new(
            "key_collision",
            format!("Board item {id} exists but is not linked to this repository; refusing to treat it as task {:?}.", task.key),
        ));
    }

    // A brief exported from the board names the board's own id as its key.
    // Importing it back must find that task, not mint a twin of it.
    if current.is_none()
        && task
            .key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        if let Some(original) = get_item(store, &task.key)? {
            if links(&original, repository_id) {
                return Ok(Placed {
                    outcome: Outcome::AlreadyPresent,
                    item_id: task.key.clone(),
                    item: Some(original),
                    sequence: None,
                });
            }
        }
    }

    let mut input = fields.clone();
    input.insert("id".into(), json!(id));
    input.insert("request_id".into(), json!(fresh_id("intake")));
    match &current {
        None => {
            input.insert("expected_revision".into(), json!(0));
            input.insert("repository_ids".into(), json!([repository_id]));
            input.insert("primary_repository_id".into(), json!(repository_id));
            input.insert("home_workspace_id".into(), Value::Null);
            input.insert("position".into(), json!(position));
        }
        Some(_) if !replace => {
            return Ok(Placed {
                outcome: Outcome::AlreadyPresent,
                item_id: id,
                item: current,
                sequence: None,
            });
        }
        Some(item) => {
            let same = fields.iter().all(|(key, value)| match key.as_str() {
                "logs" => item["logs"].as_str().unwrap_or("") == value.as_str().unwrap_or(""),
                _ => &item[key] == value,
            }) && (fields.contains_key("due_at") || item["due_at"].is_null());
            if same {
                return Ok(Placed {
                    outcome: Outcome::Unchanged,
                    item_id: id,
                    item: current,
                    sequence: None,
                });
            }
            // Where it lives on the board is the board's business; only the
            // task's own content is replaced.
            for key in [
                "repository_ids",
                "primary_repository_id",
                "home_workspace_id",
                "position",
            ] {
                input.insert(key.into(), item[key].clone());
            }
            input.insert("expected_revision".into(), item["revision"].clone());
        }
    }
    let created = current.is_none();
    match query(store, "items.put", &Value::Object(input).to_string()) {
        Ok(response) => Ok(Placed {
            outcome: if created {
                Outcome::Created
            } else {
                Outcome::Updated
            },
            item_id: id,
            item: Some(response["item"].clone()),
            sequence: response["sequence"].as_u64(),
        }),
        Err(error) if created && error.message.contains(DELETED_ID_REFUSAL) => Ok(Placed {
            outcome: Outcome::DeletedOnBoard,
            item_id: id,
            item: None,
            sequence: None,
        }),
        // Another writer created it between our read and our write. Without
        // replace that is simply "already there"; with it, one fresh attempt
        // against the revision that won.
        Err(error) if created && !retried => match get_item(store, &id)? {
            Some(_) if !replace => {
                let item = get_item(store, &id)?;
                Ok(Placed {
                    outcome: Outcome::AlreadyPresent,
                    item_id: id,
                    item,
                    sequence: None,
                })
            }
            Some(_) => place_once(store, repository, task, replace, position, true),
            None => Err(error),
        },
        Err(error) => Err(error),
    }
}

/// Resolve a repository for an agent. `resolve_repo` already refuses one the
/// person has not trusted in GitPulse; this names that refusal with its own
/// code so an agent can tell "ask the person to trust it" from "no such repo".
fn resolve_for_agent(repo_path: &str) -> Result<LocalRepository, WorkbenchError> {
    resolve(repo_path).map_err(|error| {
        if error.message.contains(crate::repository_trust::REQUIRED) {
            WorkbenchError::new("untrusted_repository", error.message)
        } else {
            error
        }
    })
}

fn repository_summary(repository: &Value) -> Value {
    json!({"id": repository["id"], "name": repository["name"]})
}

/// Add one task for an agent. An existing key is refused unless `replace`.
pub(crate) fn add_task(
    store: &Store,
    repo_path: &str,
    task: ExternalTask,
    replace: bool,
) -> Result<Value, WorkbenchError> {
    let local = resolve_for_agent(repo_path)?;
    // Validate before registering, so a bad request leaves no trace.
    task.item_fields(&local.name)
        .map_err(|message| WorkbenchError::new("invalid_input", message))?;
    let registered = register(store, &local, &fresh_id("repo"), &fresh_id("intake"))?;
    let repository = &registered["repository"];
    let placed = place(store, repository, &task, replace, now_millis())?;
    match placed.outcome {
        Outcome::AlreadyPresent => Err(WorkbenchError::new(
            "already_exists",
            format!(
                "Task {:?} is already on the board as {}. Pass overwrite: true to replace its content.",
                task.key, placed.item_id
            ),
        )),
        Outcome::DeletedOnBoard => Err(WorkbenchError::new(
            "deleted_on_board",
            format!("Task {:?} was deleted on the board and stays deleted; choose another task_id.", task.key),
        )),
        outcome => {
            let item = placed.item.unwrap_or(Value::Null);
            Ok(json!({
                "ok": true,
                "outcome": outcome,
                "task_id": task.key,
                "item_id": placed.item_id,
                "revision": item["revision"],
                "title": item["title"],
                "status": item["status"],
                "priority": item["priority"],
                "repository": repository_summary(repository),
                "folded_into_description": {
                    "planned_files": task.planned_files.len(),
                    "repositories": task.repositories.iter().filter(|r| !r.eq_ignore_ascii_case(&local.name)).count(),
                },
                "sequence": placed.sequence,
            }))
        }
    }
}

/// One brief's fate in an import.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ImportEntry {
    pub file: String,
    pub task_id: Option<String>,
    pub item_id: Option<String>,
    pub title: Option<String>,
    /// An [`Outcome`], or `invalid` (the brief did not parse) or `failed`
    /// (the store refused it).
    pub outcome: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ImportReport {
    /// True only when every candidate file was read and placed: nothing
    /// invalid, nothing failed, nothing left unread past the scan cap.
    pub ok: bool,
    pub repository: Value,
    pub tasks_dir: String,
    pub directory_exists: bool,
    pub found: usize,
    pub read: usize,
    pub truncated: bool,
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub already_present: usize,
    pub deleted_on_board: usize,
    pub invalid: usize,
    pub failed: usize,
    pub entries: Vec<ImportEntry>,
}

/// Import a repository's briefs onto the board.
pub(crate) fn import_briefs(
    store: &Store,
    repo_path: &str,
    tasks_dir: Option<&str>,
    replace: bool,
) -> Result<ImportReport, WorkbenchError> {
    let local = resolve_for_agent(repo_path)?;
    let scan = file_tasks::scan_briefs(&local.path, tasks_dir)
        .map_err(|message| WorkbenchError::new("invalid_input", message))?;
    let mut entries = Vec::new();
    let mut valid = Vec::new();
    for file in scan.files {
        match (file.brief, file.key, file.error) {
            (Some(brief), Some(key), None) => valid.push((file.file, key, brief)),
            (_, key, error) => entries.push(ImportEntry {
                file: file.file,
                task_id: key,
                item_id: None,
                title: None,
                outcome: "invalid".into(),
                reason: Some(error.unwrap_or_else(|| "no brief was read".into())),
            }),
        }
    }
    // Urgent first, so each column keeps the briefs' priority order.
    valid.sort_by(|a, b| {
        a.2.priority
            .unwrap_or(2)
            .cmp(&b.2.priority.unwrap_or(2))
            .then_with(|| a.0.cmp(&b.0))
    });
    let mut repository = Value::Null;
    if !valid.is_empty() {
        repository =
            register(store, &local, &fresh_id("repo"), &fresh_id("intake"))?["repository"].clone();
    }
    let base = now_millis();
    for (index, (file, key, brief)) in valid.into_iter().enumerate() {
        let title = brief.title.clone();
        let task = ExternalTask::from_brief(key.clone(), brief);
        let entry = match place(store, &repository, &task, replace, base + index as i64) {
            Ok(placed) => ImportEntry {
                file,
                task_id: Some(key),
                item_id: Some(placed.item_id),
                title: Some(title),
                outcome: serde_json::to_value(placed.outcome)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
                reason: None,
            },
            Err(error) => ImportEntry {
                file,
                task_id: Some(key),
                item_id: None,
                title: Some(title),
                outcome: if error.code == "invalid_input" {
                    "invalid"
                } else {
                    "failed"
                }
                .into(),
                reason: Some(format!("{}: {}", error.code, error.message)),
            },
        };
        entries.push(entry);
    }
    entries.sort_by(|a, b| a.file.cmp(&b.file));
    let count = |name: &str| entries.iter().filter(|e| e.outcome == name).count();
    let (invalid, failed) = (count("invalid"), count("failed"));
    Ok(ImportReport {
        ok: invalid == 0 && failed == 0 && !scan.truncated,
        repository: if repository.is_null() {
            Value::Null
        } else {
            repository_summary(&repository)
        },
        tasks_dir: scan.tasks_dir,
        directory_exists: scan.directory_exists,
        found: scan.found,
        read: scan.read,
        truncated: scan.truncated,
        created: count("created"),
        updated: count("updated"),
        unchanged: count("unchanged"),
        already_present: count("already_present"),
        deleted_on_board: count("deleted_on_board"),
        invalid,
        failed,
        entries,
    })
}

/// Board tasks for a repository, for an agent. Never registers anything.
pub(crate) fn list_tasks(
    store: &Store,
    repo_path: &str,
    status: Option<&str>,
    limit: u64,
    cursor: Option<&str>,
) -> Result<Value, WorkbenchError> {
    let local = resolve_for_agent(repo_path)?;
    let Some(repository) = find(store, &local.identity)? else {
        return Ok(json!({
            "ok": true, "repository": null, "returned": 0, "total": 0, "has_more": false,
            "next_cursor": null, "tasks": [],
            "note": "This repository is not on the GitPulse board yet: no task has been filed under it. That is not the same as a repository whose tasks are all done.",
        }));
    };
    let mut input = json!({"repository_id": repository["id"], "limit": limit.clamp(1, 200)});
    if let Some(status) = status {
        input["status"] = json!(file_tasks::normalize_status(status)
            .map_err(|m| WorkbenchError::new("invalid_input", m))?);
    }
    if let Some(cursor) = cursor {
        input["cursor"] = json!(cursor);
    }
    let page = query(store, "items.list", &input.to_string())?;
    let items = page["items"]
        .as_array()
        .ok_or_else(|| WorkbenchError::new("protocol_error", "Invalid task page."))?;
    let tasks: Vec<Value> = items
        .iter()
        .map(|item| {
            json!({
                "item_id": item["id"], "title": item["title"], "status": item["status"],
                "priority": item["priority"], "severity": item["severity"], "kind": item["kind"],
                "owner": item["owner"], "labels": item["labels"], "due_at": item["due_at"],
                "revision": item["revision"], "updated_at": item["updated_at"],
            })
        })
        .collect();
    Ok(json!({
        "ok": true,
        "repository": repository_summary(&repository),
        "returned": tasks.len(),
        "total": page["total"],
        "has_more": page["has_more"],
        "next_cursor": page["next_cursor"],
        "tasks": tasks,
    }))
}

/// One board task with its canonical agent brief, by task key or item id.
pub(crate) fn get_task(
    store: &Store,
    repo_path: &str,
    task: &str,
) -> Result<Value, WorkbenchError> {
    let local = resolve_for_agent(repo_path)?;
    let repository = find(store, &local.identity)?.ok_or_else(|| {
        WorkbenchError::new(
            "not_found",
            "This repository is not on the GitPulse board yet; no task has been filed under it.",
        )
    })?;
    let repository_id = repository["id"].as_str().unwrap_or_default();
    let task = task.trim();
    let mut candidates = Vec::new();
    if file_tasks::validate_task_key(task).is_ok() {
        candidates.push(item_id(repository_id, task));
    }
    if !task.is_empty()
        && task.len() <= 128
        && task
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        candidates.push(task.to_string());
    }
    for id in candidates {
        let Some(item) = get_item(store, &id)? else {
            continue;
        };
        if !links(&item, repository_id) {
            continue;
        }
        let brief = query(
            store,
            "items.brief.get",
            &json!({"id": id, "expected_revision": item["revision"]}).to_string(),
        )?;
        return Ok(json!({
            "ok": true,
            "item_id": id,
            "repository": repository_summary(&repository),
            "task": item,
            "brief": brief["item"]["markdown"],
        }));
    }
    Err(WorkbenchError::new(
        "not_found",
        format!("No task {task:?} on the board for this repository. Pass the task_id you added it with, or an item_id from gitpulse_list_tasks."),
    ))
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "intake_tests.rs"]
mod tests;
