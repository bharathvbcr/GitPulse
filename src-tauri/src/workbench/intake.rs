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

/// For an update to an existing card: every field `task` left unset keeps
/// the card's value instead of `item_fields`' default. Without this, folding a
/// line of work into a person's card would move it back to inbox, reset its
/// priority, drop its criteria and owner, and clear its logs.
fn keep_unsent(fields: &mut Map<String, Value>, task: &ExternalTask, card: &Value) {
    let unsent = [
        ("status", task.status.is_none()),
        ("priority", task.priority.is_none()),
        ("severity", task.severity.is_none()),
        ("kind", task.kind.is_none()),
        ("owner", task.owner.is_none()),
        ("due_at", task.due.is_none()),
        ("labels", task.labels.is_empty()),
        ("acceptance_criteria", task.acceptance_criteria.is_empty()),
        (
            "description",
            task.description.trim().is_empty()
                && task.planned_files.is_empty()
                && task.repositories.is_empty(),
        ),
        ("logs", task.logs.is_none()),
    ];
    for (key, keep) in unsent {
        if !keep {
            continue;
        }
        if card[key].is_null() {
            fields.remove(key);
        } else {
            fields.insert(key.into(), card[key].clone());
        }
    }
}

/// The fields a person locked on a card (title, description). A lock says the
/// person's text stays as they wrote it; the board's Quick Enhance honours it,
/// and so does every agent write: one that would change a locked field is
/// refused rather than applied.
fn locked_fields(item: &Value) -> Vec<&str> {
    item["locked_fields"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn refuse_locked(
    item: &Value,
    id: &str,
    fields: &Map<String, Value>,
) -> Result<(), WorkbenchError> {
    let changed: Vec<&str> = locked_fields(item)
        .into_iter()
        .filter(|field| {
            fields
                .get(*field)
                .is_some_and(|value| *value != item[*field])
        })
        .collect();
    if changed.is_empty() {
        return Ok(());
    }
    Err(WorkbenchError::new(
        "field_locked",
        format!("The person locked the {} of task {id}, so an agent does not change it. Leave {} as it is, or ask them to unlock it.", changed.join(" and "), if changed.len() == 1 { "it" } else { "them" }),
    ))
}

/// The live board item whose id is `key` itself, when it links `repository_id`:
/// a key that is a board id rather than a filed task key.
fn linked_board_item(
    store: &Store,
    repository_id: &str,
    key: &str,
) -> Result<Option<Value>, WorkbenchError> {
    if !key
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Ok(None);
    }
    Ok(get_item(store, key)?.filter(|item| links(item, repository_id)))
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
    let mut fields = task
        .item_fields(repository_name)
        .map_err(|message| WorkbenchError::new("invalid_input", message))?;
    let id = item_id(repository_id, &task.key);
    let current = get_item(store, &id)?;
    // The id is derived from this repository, so an item under it that this
    // repository does not link was not made here. Refuse rather than report it
    // as this task, or replace someone else's: the hash's quality stops mattering.
    if current
        .as_ref()
        .is_some_and(|item| !links(item, repository_id))
    {
        return Err(WorkbenchError::new(
            "key_collision",
            format!("Board item {id} exists but is not linked to this repository; refusing to treat it as task {:?}.", task.key),
        ));
    }

    // A brief exported from the board names the board's own id as its key.
    // Importing it back must find that task, not mint a twin of it — and with
    // `replace`, its content is updated, which is how an agent folds new work
    // into a card it found on the board. Such a card may be the person's, so
    // only what the call actually sends changes; the rest stays as it was.
    let (id, current) = match current {
        None => match linked_board_item(store, repository_id, &task.key)? {
            Some(original) if !replace => {
                return Ok(Placed {
                    outcome: Outcome::AlreadyPresent,
                    item_id: task.key.clone(),
                    item: Some(original),
                    sequence: None,
                });
            }
            Some(original) => {
                keep_unsent(&mut fields, task, &original);
                (task.key.clone(), Some(original))
            }
            None => (id, None),
        },
        current => (id, current),
    };

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
            refuse_locked(item, &id, &fields)?;
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

/// Words that appear in titles of every kind, so sharing one says nothing
/// about two tasks being the same work.
const GENERIC_WORDS: &[&str] = &[
    "the",
    "and",
    "for",
    "with",
    "from",
    "into",
    "that",
    "this",
    "are",
    "not",
    "but",
    "its",
    "can",
    "has",
    "have",
    "should",
    "when",
    "all",
    "any",
    "add",
    "fix",
    "make",
    "use",
    "new",
    "task",
    "support",
    "update",
    "implement",
    "improve",
    "allow",
    "via",
    "per",
    "one",
    "get",
    "after",
    "before",
    "also",
    "only",
    "more",
    "than",
    "then",
    "does",
    "doesn",
    "isn",
    "won",
    "which",
    "there",
    "their",
    "them",
    "out",
    "now",
    "still",
];
/// Bound on the related tasks one refusal names.
pub(crate) const MAX_RELATED: usize = 25;
/// Bound on the open tasks read to look for related ones.
const MAX_SCANNED_TASKS: usize = 2000;

/// The words of a title that can say what it is about: lowercased, three
/// characters or more, a trailing plural `s` dropped, generic words removed.
fn title_words(title: &str) -> std::collections::BTreeSet<String> {
    title
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| w.chars().count() >= 3 && !GENERIC_WORDS.contains(&w.as_str()))
        .map(|w| match w.strip_suffix('s') {
            Some(stem)
                if stem.chars().count() >= 4
                    && !["ss", "us", "is"].iter().any(|end| w.ends_with(end)) =>
            {
                stem.to_owned()
            }
            _ => w,
        })
        .collect()
}

/// Open tasks of a repository that look like the same work as a task about
/// to be filed: two shared title words, or one shared title word and a shared
/// label that is not just that word again ("DevMap: …" labelled `devmap` on
/// both sides is one coincidence, not two). Deliberately generous — a false
/// match costs the agent one look and one `reviewed_related` entry; a missed
/// one costs the person a duplicate card.
pub(crate) struct Related {
    pub tasks: Vec<Value>,
    /// Open tasks compared, and whether that was every open task.
    pub compared: usize,
    pub complete: bool,
}

/// The board columns a task is still open in: every status but `done`.
fn open_statuses() -> impl Iterator<Item = &'static str> {
    file_tasks::STATUSES
        .into_iter()
        .filter(|status| *status != "done")
}

fn related_open_tasks(
    store: &Store,
    repository_id: &str,
    exclude_id: &str,
    task: &ExternalTask,
) -> Result<Related, WorkbenchError> {
    let words = title_words(&task.title);
    let labels: std::collections::BTreeSet<String> = task
        .labels
        .iter()
        .map(|l| l.trim().to_lowercase())
        .collect();
    let normalized = |title: &str| {
        title
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    let own_title = normalized(&task.title);
    let mut scored = Vec::new();
    let mut compared = 0;
    let mut complete = true;
    // Column by column, so the bound is spent on open work and never on an
    // archive of done cards.
    'columns: for status in open_statuses() {
        if compared >= MAX_SCANNED_TASKS {
            complete = false;
            break;
        }
        let mut cursor: Option<String> = None;
        loop {
            let mut input = json!({"repository_id": repository_id, "status": status, "limit": 200});
            if let Some(cursor) = &cursor {
                input["cursor"] = json!(cursor);
            }
            let page = query(store, "items.list", &input.to_string())?;
            let items = page["items"]
                .as_array()
                .ok_or_else(|| WorkbenchError::new("protocol_error", "Invalid task page."))?;
            for item in items {
                let id = item["id"].as_str().unwrap_or_default();
                if id == exclude_id {
                    continue;
                }
                compared += 1;
                let title = item["title"].as_str().unwrap_or_default();
                let shared_words: std::collections::BTreeSet<String> =
                    title_words(title).intersection(&words).cloned().collect();
                let shared_labels: Vec<String> = item["labels"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|l| l.as_str().map(|l| l.trim().to_lowercase()))
                    .filter(|l| labels.contains(l))
                    .collect();
                let distinct_labels = shared_labels
                    .iter()
                    .filter(|label| !title_words(label).is_subset(&shared_words))
                    .count();
                let same_title = normalized(title) == own_title;
                if same_title
                    || shared_words.len() >= 2
                    || (!shared_words.is_empty() && distinct_labels > 0)
                {
                    let score =
                        shared_words.len() + distinct_labels + usize::from(same_title) * 100;
                    scored.push((
                        score,
                        json!({
                            "item_id": id, "title": title, "status": item["status"],
                            "revision": item["revision"],
                            "shared_words": shared_words, "shared_labels": shared_labels,
                        }),
                    ));
                }
            }
            if page["has_more"] != true {
                break;
            }
            if compared >= MAX_SCANNED_TASKS {
                complete = false;
                break 'columns;
            }
            let next = page["next_cursor"]
                .as_str()
                .ok_or_else(|| WorkbenchError::new("protocol_error", "Missing task cursor."))?;
            if cursor.as_deref() == Some(next) {
                return Err(WorkbenchError::new(
                    "protocol_error",
                    "Task cursor did not advance.",
                ));
            }
            cursor = Some(next.to_owned());
        }
    }
    scored.sort_by_key(|a| std::cmp::Reverse(a.0));
    Ok(Related {
        tasks: scored.into_iter().map(|(_, task)| task).collect(),
        compared,
        complete,
    })
}

/// Add one task for an agent. An existing key is refused unless `replace`.
///
/// A new task is refused with `related_tasks_exist` while the repository has
/// open tasks that look like the same work, unless every one of them is named
/// in `reviewed_related` — the agent saying it read them and this is separate.
/// The point is the read: agents filed thirty-odd near-duplicates on one board
/// when nothing made them look first. Folding work into an existing task is
/// `replace` with that task's task_id or item_id, which this never gates.
pub(crate) fn add_task(
    store: &Store,
    repo_path: &str,
    task: ExternalTask,
    replace: bool,
    reviewed_related: &[String],
) -> Result<Value, WorkbenchError> {
    let local = resolve_for_agent(repo_path)?;
    // Validate before registering, so a bad request leaves no trace.
    task.item_fields(&local.name)
        .map_err(|message| WorkbenchError::new("invalid_input", message))?;
    let mut related_seen = None;
    if let Some(repository) = find(store, &local.identity)? {
        let repository_id = repository["id"].as_str().unwrap_or_default();
        let own = item_id(repository_id, &task.key);
        let creating = get_item(store, &own)?.is_none()
            && linked_board_item(store, repository_id, &task.key)?.is_none();
        // A key that is the board id of a card deleted or merged away names
        // that card, not a new one: filing under it would recreate exactly what
        // was removed. Say where it went instead. (A deleted card under the
        // key's derived id is `deleted_on_board`, below.)
        if creating {
            if let Some((gone, _)) = deleted_task(store, repository_id, &task.key)? {
                if gone == task.key {
                    return Err(find_live_task(store, repository_id, &task.key)
                        .err()
                        .unwrap_or_else(|| {
                            WorkbenchError::new(
                                "revision_conflict",
                                format!("Task {gone} was just restored on the board; read it again before filing."),
                            )
                        }));
                }
            }
        }
        if creating {
            let related = related_open_tasks(store, repository_id, &own, &task)?;
            let unreviewed: Vec<&Value> = related
                .tasks
                .iter()
                .filter(|t| {
                    !reviewed_related
                        .iter()
                        .any(|r| t["item_id"].as_str() == Some(r.trim()))
                })
                .collect();
            if !unreviewed.is_empty() {
                let listed: Vec<String> = unreviewed
                    .iter()
                    .take(MAX_RELATED)
                    .map(|t| {
                        format!(
                            "- {} [{}] {} (shared: {})",
                            t["item_id"].as_str().unwrap_or_default(),
                            t["status"].as_str().unwrap_or_default(),
                            t["title"].as_str().unwrap_or_default(),
                            t["shared_words"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .chain(t["shared_labels"].as_array().into_iter().flatten())
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })
                    .collect();
                let more = unreviewed.len().saturating_sub(MAX_RELATED);
                return Err(WorkbenchError::new(
                    "related_tasks_exist",
                    format!(
                        "Not filed: this repository already has {} open task{} that may be the same work. Read {} with gitpulse_get_task first.\n{}{}\nIf this belongs in one of them, fold it in instead: call gitpulse_add_task with that task's item_id as task_id and overwrite: true, carrying its existing content plus yours. If it is genuinely separate, call again with reviewed_related listing every item_id above.",
                        unreviewed.len(),
                        if unreviewed.len() == 1 { "" } else { "s" },
                        if unreviewed.len() == 1 { "it" } else { "them" },
                        listed.join("\n"),
                        if more > 0 { format!("\n…and {more} more; narrow the title or fold this into one of these.") } else { String::new() },
                    ),
                ));
            }
            related_seen = Some(json!({
                "related_open_tasks": related.tasks.len(),
                "open_tasks_compared": related.compared,
                "scan_complete": related.complete,
            }));
        }
    }
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
                // What the related-task check saw. Null when it did not run:
                // an overwrite, or the repository's first task.
                "related_check": related_seen,
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
    let repository = board_repository(store, repo_path)?;
    let repository_id = repository["id"].as_str().unwrap_or_default();
    let (id, item) = find_live_task(store, repository_id, task)?;
    let brief = query(
        store,
        "items.brief.get",
        &json!({"id": id, "expected_revision": item["revision"]}).to_string(),
    )?;
    Ok(json!({
        "ok": true,
        "item_id": id,
        "repository": repository_summary(&repository),
        "task": item,
        "brief": brief["item"]["markdown"],
    }))
}

/// A board task linked to `repository_id`, by the key it was filed with or by
/// its board id (the id a launched agent finds in its brief).
fn find_task(
    store: &Store,
    repository_id: &str,
    task: &str,
) -> Result<(String, Value), WorkbenchError> {
    let task = task.trim();
    for id in task_candidates(repository_id, task) {
        let Some(item) = get_item(store, &id)? else {
            continue;
        };
        if links(&item, repository_id) {
            return Ok((id, item));
        }
    }
    Err(WorkbenchError::new(
        "not_found",
        format!("No task {task:?} on the board for this repository. Pass the task_id you added it with, or an item_id from gitpulse_list_tasks."),
    ))
}

/// The item ids `task` can name: the id derived from a filed task key, and
/// the text itself as a board item id. Either may be absent.
fn task_candidates(repository_id: &str, task: &str) -> Vec<String> {
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
    candidates
}

/// Every field `items.put` replaces, sent back exactly as read under the
/// revision it was read at, so a write changes only what its caller sets on
/// top and a person's edit in between is a conflict rather than overwritten.
fn resend(item: &Value, id: &str, revision: i64, request: &str) -> Map<String, Value> {
    let mut input = Map::new();
    for key in [
        "title",
        "description",
        "kind",
        "status",
        "priority",
        "severity",
        "owner",
        "due_at",
        "labels",
        "acceptance_criteria",
        "logs",
        "repository_ids",
        "primary_repository_id",
        "position",
    ] {
        if !item[key].is_null() {
            input.insert(key.into(), item[key].clone());
        }
    }
    input.insert(
        "home_workspace_id".into(),
        item["home_workspace_id"].clone(),
    );
    input.insert("id".into(), json!(id));
    input.insert("request_id".into(), json!(fresh_id(request)));
    input.insert("expected_revision".into(), json!(revision));
    input
}

/// `logs` with `block` appended as its own paragraph, or `None` when the
/// result would not fit in a task's logs.
fn append_log(logs: &str, block: &str) -> Option<String> {
    let joined = if logs.trim().is_empty() {
        block.to_owned()
    } else {
        format!("{}\n\n{block}", logs.trim_end())
    };
    (joined.len() <= file_tasks::MAX_TASK_LOGS).then_some(joined)
}

/// The last block `marker` opens in `logs`: the last line that starts with
/// `marker` and ends with ` ---`, and everything after it as the body. A note a
/// person added after the block becomes part of that body, so the block no
/// longer reads as this tool's own last write. A body that itself quotes the
/// marker on a line of its own is not recognised and would be written again on
/// a retry: a duplicate, never a write reported that did not happen.
fn last_block<'a>(logs: &'a str, marker: &str) -> Option<(&'a str, &'a str)> {
    let logs = logs.trim_end();
    let start = logs.rfind(marker)?;
    if start > 0 && !logs[..start].ends_with('\n') {
        return None;
    }
    let (header, body) = logs[start..].split_once('\n')?;
    header.ends_with(" ---").then_some((header, body))
}

/// Whether `logs` already ends with the block `marker` opens and `body` fills —
/// exactly that body, as the last block. This is how a retry recognises its own
/// earlier write. Text that merely ends the logs is not that block: matching it
/// reported a summary or reason as recorded when it never was.
fn ends_with_block(logs: &str, marker: &str, body: &str) -> bool {
    last_block(logs, marker).is_some_and(|(_, last)| last == body.trim())
}

/// Statuses an agent may move its own task to. `done` is where a finished
/// task goes (and is the board's archive); `review` hands it to a person;
/// `in_progress` says work has started.
pub(crate) const AGENT_STATUSES: [&str; 3] = ["in_progress", "review", "done"];
/// Bound on the completion summary appended to the task's logs.
pub(crate) const MAX_SUMMARY_CHARS: usize = 4000;

/// The block a completion summary is recorded as in the task's logs.
fn summary_block(status: &str, summary: &str, at_ms: i64) -> String {
    let at = crate::ledger::ids::iso8601_utc(u64::try_from(at_ms).unwrap_or_default());
    format!(
        "--- Agent moved this task to {status} ({at}) ---\n{}",
        summary.trim()
    )
}

/// Move a board task to `status` on an agent's behalf, optionally recording
/// what the agent says it did.
///
/// Only the status changes. `items.put` replaces a whole item, so every other
/// stored field is sent back exactly as read, under the revision it was read
/// at — a person's edit that lands in between is a conflict that is retried
/// against the new revision, never overwritten. `expected_revision`, when the
/// agent passes one, is held to: a task that changed since the agent read it
/// is refused rather than moved, so the agent re-reads it first.
///
/// A task already in `done` is never moved back out by an agent: the person
/// closed it. The same move twice is `unchanged`, so a retried call cannot
/// append its summary twice.
pub(crate) fn complete_task(
    store: &Store,
    repo_path: &str,
    task: &str,
    status: &str,
    expected_revision: Option<i64>,
    summary: Option<&str>,
) -> Result<Value, WorkbenchError> {
    if !AGENT_STATUSES.contains(&status) {
        return Err(WorkbenchError::new(
            "invalid_input",
            format!("An agent can move its task to one of {AGENT_STATUSES:?}, not {status:?}."),
        ));
    }
    let summary = summary.map(str::trim).filter(|s| !s.is_empty());
    if summary.is_some_and(|s| s.chars().count() > MAX_SUMMARY_CHARS) {
        return Err(WorkbenchError::new(
            "invalid_input",
            format!("The summary exceeds {MAX_SUMMARY_CHARS} characters."),
        ));
    }
    let repository = board_repository(store, repo_path)?;
    let repository_id = repository["id"].as_str().unwrap_or_default().to_owned();
    for _ in 0..2 {
        let (id, item) = find_live_task(store, &repository_id, task)?;
        let revision = item["revision"].as_i64().unwrap_or_default();
        if expected_revision.is_some_and(|expected| expected != revision) {
            return Err(WorkbenchError::new(
                "revision_conflict",
                format!("The task is at revision {revision}, not {}; someone changed it. Re-read it with gitpulse_get_task before moving it.", expected_revision.unwrap_or_default()),
            ));
        }
        let previous = item["status"].as_str().unwrap_or_default().to_owned();
        let logs = item["logs"].as_str().unwrap_or_default();
        let block = summary.map(|s| summary_block(status, s, now_millis()));
        // The marker carries a timestamp, so compare the summary text itself:
        // the same summary as the last block, under the same status, is a retry.
        let recorded = summary.is_some_and(|s| {
            ends_with_block(logs, &format!("--- Agent moved this task to {status} ("), s)
        });
        let response = |outcome: &str, item: &Value, sequence: Option<u64>| {
            json!({
                "ok": true,
                "outcome": outcome,
                "item_id": id,
                "title": item["title"],
                "previous_status": previous,
                "status": item["status"],
                "revision": item["revision"],
                "summary_recorded": summary.is_some() && (outcome == "updated" || recorded),
                "repository": repository_summary(&repository),
                "sequence": sequence,
            })
        };
        if previous == status && (summary.is_none() || recorded) {
            return Ok(response("unchanged", &item, None));
        }
        if previous == "done" && status != "done" {
            return Err(WorkbenchError::new(
                "already_done",
                "This task is already done; an agent does not reopen a task a person closed. Ask them to move it back first.",
            ));
        }
        let mut input = resend(&item, &id, revision, "complete");
        input.insert("status".into(), json!(status));
        if let Some(block) = &block {
            let joined = append_log(logs, block).ok_or_else(|| {
                WorkbenchError::new(
                    "invalid_input",
                    "The task's logs are full; move it without a summary, or shorten the summary.",
                )
            })?;
            input.insert("logs".into(), json!(joined));
        }
        match query(store, "items.put", &Value::Object(input).to_string()) {
            Ok(saved) => {
                return Ok(response(
                    "updated",
                    &saved["item"],
                    saved["sequence"].as_u64(),
                ));
            }
            // A person saved it between our read and our write. Their
            // content wins; move it again from what they saved.
            Err(error) if error.code == "revision_conflict" && expected_revision.is_none() => {
                continue
            }
            Err(error) => return Err(error),
        }
    }
    Err(WorkbenchError::new(
        "revision_conflict",
        "The task kept changing while it was being moved. Re-read it and try again.",
    ))
}

/// Bound on the reason an agent gives for deleting or merging a task.
pub(crate) const MAX_REASON_CHARS: usize = 1000;
/// Opens the log block a deletion reason is recorded in. Present tense on
/// purpose: the block is written before the delete, and if the delete is then
/// refused the task is still there carrying it.
const DELETION_MARKER: &str = "--- Agent is deleting this task (";
/// Opens the log block a merged task carries as it is deleted. The target's
/// item id follows, so a deleted task can say where its work went.
const MERGE_MARKER: &str = "--- Agent merged this task into ";

fn timestamp(at_ms: i64) -> String {
    crate::ledger::ids::iso8601_utc(u64::try_from(at_ms).unwrap_or_default())
}

fn deletion_block(reason: &str, at_ms: i64) -> String {
    format!("{DELETION_MARKER}{}) ---\n{reason}", timestamp(at_ms))
}

/// A reason as both destructive tools require it: present, and bounded.
fn check_reason<'a>(reason: &'a str, doing: &str) -> Result<&'a str, WorkbenchError> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(WorkbenchError::new(
            "invalid_input",
            format!("A reason is required: say why {doing}. It is kept in the task's history."),
        ));
    }
    if reason.chars().count() > MAX_REASON_CHARS {
        return Err(WorkbenchError::new(
            "invalid_input",
            format!("The reason exceeds {MAX_REASON_CHARS} characters."),
        ));
    }
    Ok(reason)
}

/// The board record of the repository an agent names, which has to exist
/// already for any write but an add.
fn board_repository(store: &Store, repo_path: &str) -> Result<Value, WorkbenchError> {
    let local = resolve_for_agent(repo_path)?;
    find(store, &local.identity)?.ok_or_else(|| {
        WorkbenchError::new(
            "not_found",
            "This repository is not on the GitPulse board yet; no task has been filed under it.",
        )
    })
}

/// The last recorded revision of an item, deleted or not; `None` when the
/// store has never had it. `items.get` hides deleted items, `items.history`
/// does not.
fn last_revision(store: &Store, id: &str) -> Result<Option<Value>, WorkbenchError> {
    let mut after = 0_i64;
    let mut last = None;
    for _ in 0..50 {
        let input = json!({"id": id, "limit": 200, "after_revision": after});
        let page = match query(store, "items.history", &input.to_string()) {
            Ok(page) => page,
            Err(error) if error.code == "not_found" => return Ok(None),
            Err(error) => return Err(error),
        };
        let revisions = page["items"]
            .as_array()
            .ok_or_else(|| WorkbenchError::new("protocol_error", "Invalid task history page."))?;
        if let Some(revision) = revisions.last() {
            last = Some(revision.clone());
        }
        if page["has_more"] != true {
            return Ok(last);
        }
        let next = page["next_cursor"]
            .as_i64()
            .filter(|next| *next > after)
            .ok_or_else(|| {
                WorkbenchError::new("protocol_error", "Task history cursor did not advance.")
            })?;
        after = next;
    }
    Err(WorkbenchError::new(
        "registry_limit",
        "The task's history is longer than 10,000 revisions; delete it on the board.",
    ))
}

/// A deleted board task that was linked to `repository_id`, as last recorded.
fn deleted_task(
    store: &Store,
    repository_id: &str,
    task: &str,
) -> Result<Option<(String, Value)>, WorkbenchError> {
    for id in task_candidates(repository_id, task.trim()) {
        if let Some(body) = last_revision(store, &id)? {
            if body["deleted"] == true && links(&body, repository_id) {
                return Ok(Some((id, body)));
            }
        }
    }
    Ok(None)
}

/// Where a deleted task's work went, as its last log block says.
#[derive(Debug, PartialEq, Eq)]
enum Gone {
    Merged { into: String, reason: String },
    Deleted { reason: Option<String> },
}

fn why_gone(body: &Value) -> Gone {
    let logs = body["logs"].as_str().unwrap_or_default();
    if let Some((header, reason)) = last_block(logs, MERGE_MARKER) {
        if let Some(into) = header[MERGE_MARKER.len()..].split_whitespace().next() {
            return Gone::Merged {
                into: into.to_owned(),
                reason: reason.to_owned(),
            };
        }
    }
    Gone::Deleted {
        reason: last_block(logs, DELETION_MARKER).map(|(_, reason)| reason.to_owned()),
    }
}

/// A live task by key or item id; a deleted one is named as deleted, with
/// where its work went, rather than reported as never having existed. An
/// agent launched on a task that was merged or deleted under it is told so.
fn find_live_task(
    store: &Store,
    repository_id: &str,
    task: &str,
) -> Result<(String, Value), WorkbenchError> {
    match find_task(store, repository_id, task) {
        Err(error) if error.code == "not_found" => {
            let Some((id, body)) = deleted_task(store, repository_id, task)? else {
                return Err(error);
            };
            let title = body["title"].as_str().unwrap_or_default();
            Err(match why_gone(&body) {
                Gone::Merged { into, reason } => WorkbenchError::new(
                    "task_merged",
                    format!("Task {id} ({title:?}) was merged into {into}: {reason}\nIts work continues there; read it with gitpulse_get_task and task_id {into}."),
                ),
                Gone::Deleted { reason } => WorkbenchError::new(
                    "task_deleted",
                    match reason {
                        Some(reason) => format!("Task {id} ({title:?}) was deleted: {reason}\nIt cannot be read or completed; ask the person whether the work is still wanted."),
                        None => format!("Task {id} ({title:?}) was deleted on the board, with no reason recorded. It cannot be read or completed; ask the person whether the work is still wanted."),
                    },
                ),
            })
        }
        found => found,
    }
}

/// How many repositories other than `repository_id` a task is linked to.
fn other_repositories(item: &Value, repository_id: &str) -> usize {
    item["repository_ids"].as_array().map_or(0, |ids| {
        ids.iter()
            .filter(|v| v.as_str() != Some(repository_id))
            .count()
    })
}

/// Refuse a destructive write on a task other repositories see too: deleting
/// it here would delete it there.
fn refuse_shared(item: &Value, repository_id: &str) -> Result<(), WorkbenchError> {
    let others = other_repositories(item, repository_id);
    if others == 0 {
        return Ok(());
    }
    Err(WorkbenchError::new(
        "shared_task",
        format!("Task {} is linked to {others} other repositor{} as well, and deleting it here would delete it there too. Ask the person to do this on the board.", item["id"].as_str().unwrap_or_default(), if others == 1 { "y" } else { "ies" }),
    ))
}

/// Run states in which an agent may still be working on its task: prepared
/// and not yet expired, starting, running, or unresolved (its outcome is
/// uncertain, so its process may be alive).
const LIVE_RUN_STATES: [&str; 4] = ["prepared", "starting", "running", "unresolved"];

/// The live agent attempts on a task. `complete` is false when there were more
/// than one page could show, which counts as in use: unread is not idle.
pub(crate) struct LiveRuns {
    pub runs: Vec<Value>,
    pub complete: bool,
}

fn live_runs(store: &Store, task_id: &str) -> Result<LiveRuns, WorkbenchError> {
    let now = now_millis() / 1000;
    let mut runs = Vec::new();
    let mut complete = true;
    for state in LIVE_RUN_STATES {
        let input = json!({"task_id": task_id, "state": state, "limit": 200});
        let page = query(store, "runs.list", &input.to_string())?;
        let items = page["items"]
            .as_array()
            .ok_or_else(|| WorkbenchError::new("protocol_error", "Invalid run page."))?;
        runs.extend(
            items
                .iter()
                .filter(|run| state != "prepared" || run["expires_at"].as_i64().unwrap_or(i64::MAX) > now)
                .map(|run| json!({"run_id": run["id"], "state": run["state"], "provider": run["provider"]})),
        );
        complete &= page["has_more"] != true;
    }
    Ok(LiveRuns { runs, complete })
}

/// Refuse to delete a task an agent may still be working on, unless the
/// caller said it knows: the running agent is itself, or the person asked.
fn refuse_in_use(
    store: &Store,
    id: &str,
    item: &Value,
    even_if_running: bool,
) -> Result<(), WorkbenchError> {
    if even_if_running {
        return Ok(());
    }
    let live = live_runs(store, id)?;
    if live.runs.is_empty() && live.complete {
        return Ok(());
    }
    let listed: Vec<String> = live
        .runs
        .iter()
        .take(10)
        .map(|run| {
            format!(
                "{} ({}, {})",
                run["run_id"].as_str().unwrap_or_default(),
                run["state"].as_str().unwrap_or_default(),
                run["provider"].as_str().unwrap_or_default()
            )
        })
        .collect();
    Err(WorkbenchError::new(
        "task_in_use",
        format!(
            "Task {id} ({:?}) has {}{} agent attempt{} that may still be working on it: {}. Removing it would pull the task out from under {}. If the running agent is you, or the person asked for this, call again with even_if_running: true.",
            item["title"].as_str().unwrap_or_default(),
            if live.complete { "" } else { "at least " },
            live.runs.len().max(1),
            if live.runs.len() == 1 { "" } else { "s" },
            if listed.is_empty() { "more than one page of attempts".to_owned() } else { listed.join(", ") },
            if live.runs.len() == 1 { "it" } else { "them" },
        ),
    ))
}

/// What removing one task came to.
enum Removal {
    /// Deleted; the store's receipt.
    Deleted(Value),
    /// The task changed under one of the two writes, so it was not deleted.
    /// `recorded`: the block was written (and stays on the live task).
    Changed { recorded: bool },
}

/// Record `block` as the task's last log block (unless `recorded` says it
/// already is) and soft delete the task at the revision that write produced,
/// through the store's own `items.delete` — the same delete as the board's.
///
/// `items.delete` takes no payload, so that is two writes, each conditional on
/// the revision before it: a task that changes in between is left live and
/// reported as [`Removal::Changed`], never deleted at a revision this call did
/// not read. That is what lets a merge promise it deletes only the revision it
/// copied.
fn record_and_delete(
    store: &Store,
    id: &str,
    item: &Value,
    block: &str,
    recorded: bool,
    request: &str,
) -> Result<Removal, WorkbenchError> {
    let mut revision = item["revision"]
        .as_i64()
        .ok_or_else(|| WorkbenchError::new("protocol_error", "The task has no revision."))?;
    if !recorded {
        let logs = item["logs"].as_str().unwrap_or_default();
        let joined = append_log(logs, block).ok_or_else(|| {
            WorkbenchError::new(
                "invalid_input",
                format!("Task {id}'s logs are full, so the reason cannot be recorded; shorten the reason."),
            )
        })?;
        let mut input = resend(item, id, revision, request);
        input.insert("logs".into(), json!(joined));
        match query(store, "items.put", &Value::Object(input).to_string()) {
            Ok(saved) => {
                revision = saved["item"]["revision"].as_i64().ok_or_else(|| {
                    WorkbenchError::new(
                        "protocol_error",
                        "The store did not report the task's revision.",
                    )
                })?;
            }
            Err(error) if error.code == "revision_conflict" => {
                return Ok(Removal::Changed { recorded: false })
            }
            Err(error) => return Err(error),
        }
    }
    let input = json!({"id": id, "request_id": fresh_id(request), "expected_revision": revision});
    match query(store, "items.delete", &input.to_string()) {
        Ok(receipt) => {
            // The receipt the board's own delete holds the store to.
            if receipt["item"]["deleted"] != true
                || receipt["item"]["revision"].as_i64() != Some(revision + 1)
            {
                return Err(WorkbenchError::new(
                    "protocol_error",
                    "The store's delete receipt does not show the task deleted at the next revision.",
                ));
            }
            Ok(Removal::Deleted(receipt))
        }
        Err(error) if error.code == "revision_conflict" => Ok(Removal::Changed { recorded: true }),
        Err(error) => Err(error),
    }
}

/// Delete a board task on an agent's behalf, through the store's own
/// `items.delete` — the same soft delete the board's Delete performs. The row
/// and its revision history stay in the profile, the id is never reused, and
/// nothing over MCP brings it back: only the person can, from the profile.
///
/// The reason is required and is recorded first, as a block appended to the
/// task's logs; the deleted revision carries those logs, so the reason is in
/// the task's history and in the deletion event itself. When the task changes
/// between the two writes nothing is deleted, the task keeps the reason block,
/// and the call is refused (or, with no `expected_revision`, retried on top of
/// the person's edit — the block is recognised and not appended twice).
///
/// Refused: a task linked to more than this repository (deleting it here would
/// delete it from the others too), and one an agent may still be working on
/// unless `even_if_running`. A task already deleted is `unchanged`.
pub(crate) fn delete_task(
    store: &Store,
    repo_path: &str,
    task: &str,
    expected_revision: Option<i64>,
    reason: &str,
    even_if_running: bool,
) -> Result<Value, WorkbenchError> {
    let reason = check_reason(reason, "this task is being deleted")?;
    let repository = board_repository(store, repo_path)?;
    let repository_id = repository["id"].as_str().unwrap_or_default().to_owned();
    let response =
        |outcome: &str, id: &str, item: &Value, recorded: bool, sequence: Option<u64>| {
            json!({
                "ok": true,
                "outcome": outcome,
                "item_id": id,
                "title": item["title"],
                "status": item["status"],
                "deleted": true,
                "revision": item["revision"],
                "reason_recorded": recorded,
                "repository": repository_summary(&repository),
                "sequence": sequence,
            })
        };
    for _ in 0..2 {
        let (id, item) = match find_task(store, &repository_id, task) {
            Ok(found) => found,
            Err(error) if error.code == "not_found" => {
                return match deleted_task(store, &repository_id, task)? {
                    Some((id, body)) => {
                        let recorded = ends_with_block(
                            body["logs"].as_str().unwrap_or_default(),
                            DELETION_MARKER,
                            reason,
                        );
                        Ok(response("unchanged", &id, &body, recorded, None))
                    }
                    None => Err(error),
                };
            }
            Err(error) => return Err(error),
        };
        let revision = item["revision"].as_i64().unwrap_or_default();
        if expected_revision.is_some_and(|expected| expected != revision) {
            return Err(WorkbenchError::new(
                "revision_conflict",
                format!("The task is at revision {revision}, not {}; someone changed it. Re-read it with gitpulse_get_task before deleting it.", expected_revision.unwrap_or_default()),
            ));
        }
        refuse_shared(&item, &repository_id)?;
        refuse_in_use(store, &id, &item, even_if_running)?;
        // A retry after a refused delete finds its own reason already there.
        let recorded = ends_with_block(
            item["logs"].as_str().unwrap_or_default(),
            DELETION_MARKER,
            reason,
        );
        let block = deletion_block(reason, now_millis());
        match record_and_delete(store, &id, &item, &block, recorded, "delete")? {
            Removal::Deleted(receipt) => {
                return Ok(response(
                    "deleted",
                    &id,
                    &receipt["item"],
                    true,
                    receipt["sequence"].as_u64(),
                ));
            }
            // A person saved it between our read and our write. Their
            // content wins; delete again from what they saved.
            Removal::Changed { .. } if expected_revision.is_none() => continue,
            Removal::Changed { recorded: false } => {
                return Err(WorkbenchError::new(
                    "revision_conflict",
                    format!("The task changed after revision {revision} was read, so nothing was recorded or deleted. Re-read it with gitpulse_get_task and try again."),
                ));
            }
            Removal::Changed { recorded: true } => {
                return Err(WorkbenchError::new(
                    "revision_conflict",
                    "The reason was recorded, but the task changed before it could be deleted, so nothing was deleted. Re-read it with gitpulse_get_task and try again.",
                ));
            }
        }
    }
    Err(WorkbenchError::new(
        "revision_conflict",
        "The task kept changing while it was being deleted, so nothing was deleted. Re-read it and try again.",
    ))
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[path = "intake_merge.rs"]
mod merge;
pub(crate) use merge::{merge_tasks, MergeTask, MAX_MERGE_SOURCES};

#[cfg(test)]
#[path = "intake_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "intake_merge_tests.rs"]
mod merge_tests;
