//! What a person does with a proposal Manvi made: file it, or apply it.
//!
//! Manvi's `work.scan.gaps` and `work.enhancements.refine` only *propose*; the
//! board is never written by them. This is the host side of the review: the
//! one place an accepted proposal becomes a task or a task edit, so the
//! protections the board already has hold for it too.
//!
//! * **Filing** goes through [`add_task`], the path `gitpulse_add_task` uses,
//!   with a key derived from the proposal's own identity (a scan fingerprint,
//!   or a refine request and part). Filing twice therefore finds the card the
//!   first filing made instead of making a second, and the related-task check
//!   still runs: a proposal that looks like open work is *not filed* until the
//!   person has seen those tasks and confirmed, by naming them.
//! * **Applying** a refinement is one `items.put` at the revision the person
//!   reviewed. A task that has changed since is refused with the revision it
//!   is at, never overwritten.
//! * Nothing here is reachable without a person's action: Manvi's ops are
//!   read-only proposals, and these methods are the only writers.

use super::*;
use serde::Deserialize;

/// Bound on one request's JSON, before any parsing or store access.
pub(super) const MAX_REQUEST_BYTES: usize = 64 * 1024;
/// Most criteria one apply may add; Manvi's own per-run cap is ten.
const MAX_APPLY_CRITERIA: usize = 20;
/// Most duplicate links one apply may add.
const MAX_APPLY_DUPLICATES: usize = 12;

fn invalid(message: impl Into<String>) -> WorkbenchError {
    WorkbenchError::new("invalid_input", message)
}

fn object_input<T: for<'de> Deserialize<'de>>(input: &str) -> Result<T, WorkbenchError> {
    if input.len() > MAX_REQUEST_BYTES || !input.trim_start().starts_with('{') {
        return Err(invalid("A proposal request is a JSON object under 64 KiB."));
    }
    serde_json::from_str(input).map_err(|e| invalid(e.to_string()))
}

fn valid_ref(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

// ---------------------------------------------------------------------------
// Scans: params for Manvi's work.scan.* ops
// ---------------------------------------------------------------------------

/// The canonical root of the repository a request names, as Manvi requires it
/// (absolute, no symlinks). Resolved by the host from the open repository.
fn canonical_root(repo_path: &str) -> Result<String, WorkbenchError> {
    let local = resolve_for_agent(repo_path)?;
    if local.is_bare {
        return Err(invalid("A bare repository has no work tree."));
    }
    let root = std::fs::canonicalize(&local.path).map_err(|e| {
        WorkbenchError::new(
            "repository_unavailable",
            format!("The repository root could not be resolved: {e}"),
        )
    })?;
    root.to_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid("Repository path is not valid Unicode."))
}

/// Splits a refine request into the repository root Manvi needs and the
/// remaining generation request (`id`, `request_id`, `expected_revision`,
/// optional `model`), which the host validates as it does for generation.
pub(crate) fn refine_split(input: &str) -> Result<(String, String), WorkbenchError> {
    let mut parsed: Value = object_input(input)?;
    let repo_path = parsed
        .as_object_mut()
        .and_then(|o| o.remove("repo_path"))
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or_else(|| invalid("Refinement names the open repository (repo_path)."))?;
    Ok((canonical_root(&repo_path)?, parsed.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScanStart {
    repo_path: String,
    since: Option<String>,
    max_proposals: Option<u32>,
}

/// Params for Manvi's `work.scan.gaps`: the open repository's own root, as the
/// host resolves it (never a renderer-supplied string), canonical because
/// Manvi refuses a path that passes through a symlink.
pub(crate) fn scan_start_params(input: &str) -> Result<Value, WorkbenchError> {
    let parsed: ScanStart = object_input(input)?;
    let root = canonical_root(&parsed.repo_path)?;
    let mut params = json!({ "repo_root": root });
    if let Some(since) = parsed
        .since
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        params["since"] = json!(since);
    }
    if let Some(max) = parsed.max_proposals {
        params["max_proposals"] = json!(max);
    }
    Ok(params)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScanRef {
    scan_id: String,
}

/// Params for `work.scan.get` and `work.scan.cancel`.
pub(crate) fn scan_ref_params(input: &str) -> Result<Value, WorkbenchError> {
    let parsed: ScanRef = object_input(input)?;
    if !valid_ref(&parsed.scan_id) {
        return Err(invalid("A scan is named by its scan id."));
    }
    Ok(json!({ "scan_id": parsed.scan_id }))
}

// ---------------------------------------------------------------------------
// Filing
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Origin {
    /// A gap-scan proposal, by Manvi's 16-hex fingerprint.
    Scan { fingerprint: String },
    /// One part of a refine proposal's `split_into`.
    Split {
        task_id: String,
        request_id: String,
        index: u32,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileInput {
    repo_path: String,
    origin: Origin,
    title: String,
    #[serde(default)]
    description: String,
    kind: Option<String>,
    priority: Option<u8>,
    #[serde(default)]
    labels: Vec<String>,
    /// Open tasks the person was shown as possibly the same work and says this
    /// is separate from. Honoured exactly as `gitpulse_add_task` honours it.
    #[serde(default)]
    reviewed_related: Vec<String>,
}

fn is_fingerprint(fp: &str) -> bool {
    fp.len() == 16 && fp.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// The task as the board will hold it, and the key that makes filing it twice
/// one card. A scan proposal always carries its fingerprint label and marker,
/// even edited: they are what the next scan dedupes on.
fn filed_task(input: FileInput) -> Result<(String, ExternalTask, Vec<String>), WorkbenchError> {
    let FileInput {
        repo_path,
        origin,
        title,
        description,
        kind,
        priority,
        mut labels,
        reviewed_related,
    } = input;
    let mut description = description;
    let key = match origin {
        Origin::Scan { fingerprint } => {
            if !is_fingerprint(&fingerprint) {
                return Err(invalid("A scan proposal is named by a 16-hex fingerprint."));
            }
            let label = format!("scan:{fingerprint}");
            if !labels.iter().any(|l| l == &label) {
                labels.push(label);
            }
            let marker = format!("manvi-scan: {fingerprint}");
            if !description.contains(&marker) {
                description = format!("{}\n\n{marker}", description.trim_end());
            }
            format!("scan-{fingerprint}")
        }
        Origin::Split {
            task_id,
            request_id,
            index,
        } => {
            if !valid_ref(&task_id) || !valid_ref(&request_id) || index >= 16 {
                return Err(invalid(
                    "A split part is named by its task, request and index.",
                ));
            }
            let note = format!("Split from {task_id}.");
            if !description.contains(&note) {
                description = format!("{}\n\n{note}", description.trim_end());
            }
            let id = item_id(&task_id, &format!("{request_id}:{index}"));
            format!("split-{}", &id[ITEM_PREFIX.len()..ITEM_PREFIX.len() + 16])
        }
    };
    let task = ExternalTask {
        key,
        title,
        description: description.trim_start().to_owned(),
        kind,
        priority,
        labels,
        ..ExternalTask::default()
    };
    Ok((repo_path, task, reviewed_related))
}

/// File one proposal on the board as an inbox task.
///
/// Outcomes (all `ok: true` except where nothing was filed):
/// `filed`, `already_filed` (this proposal's card exists; a double click or a
/// retry), and `related_tasks_exist` (`ok: false`, nothing filed: `related`
/// names the open tasks that may be the same work).
pub(crate) fn file_proposal(store: &Store, input: &str) -> Result<Value, WorkbenchError> {
    let parsed: FileInput = object_input(input)?;
    let (repo_path, task, reviewed) = filed_task(parsed)?;
    match add_task(store, &repo_path, task.clone(), false, &reviewed) {
        Ok(filed) => Ok(json!({
            "ok": true,
            "outcome": "filed",
            "item_id": filed["item_id"],
            "revision": filed["revision"],
            "title": filed["title"],
            "related_check": filed["related_check"],
            "sequence": filed["sequence"],
        })),
        Err(error) if error.code == "already_exists" => {
            let (repository_id, item) = existing(store, &repo_path, &task.key)?;
            Ok(json!({
                "ok": true,
                "outcome": "already_filed",
                "item_id": item_id(&repository_id, &task.key),
                "revision": item["revision"],
                "title": item["title"],
                "related_check": Value::Null,
            }))
        }
        Err(error) if error.code == "related_tasks_exist" => {
            let local = resolve_for_agent(&repo_path)?;
            let repository = find(store, &local.identity)?.ok_or(error)?;
            let repository_id = repository["id"].as_str().unwrap_or_default();
            let own = item_id(repository_id, &task.key);
            let related = related_open_tasks(store, repository_id, &own, &task)?;
            let unreviewed: Vec<Value> = related
                .tasks
                .into_iter()
                .filter(|t| {
                    !reviewed
                        .iter()
                        .any(|r| t["item_id"].as_str() == Some(r.trim()))
                })
                .take(MAX_RELATED)
                .collect();
            Ok(json!({
                "ok": false,
                "outcome": "related_tasks_exist",
                "related": unreviewed,
                "compared": related.compared,
                "scan_complete": related.complete,
            }))
        }
        Err(error) => Err(error),
    }
}

/// The repository's board id and the card already filed under `key`.
fn existing(store: &Store, repo_path: &str, key: &str) -> Result<(String, Value), WorkbenchError> {
    let repository = board_repository(store, repo_path)?;
    let repository_id = repository["id"].as_str().unwrap_or_default().to_owned();
    let (_, item) = find_live_task(store, &repository_id, key)?;
    Ok((repository_id, item))
}

// ---------------------------------------------------------------------------
// Applying a refinement
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyInput {
    repo_path: String,
    id: String,
    expected_revision: i64,
    #[serde(default)]
    acceptance_criteria: Vec<String>,
    #[serde(default)]
    duplicate_of: Vec<String>,
}

/// Apply the parts of a refinement the person accepted to the task they were
/// reviewed against: acceptance criteria appended, `duplicate_of` links added.
///
/// One `items.put` at `expected_revision`. A task at any other revision is
/// refused with `revision_conflict` naming the revision it is at, so the
/// caller re-reads and the person re-reviews; nothing is merged into a task
/// that moved. Everything already present is left alone, so applying the same
/// acceptance twice is `unchanged`, not a second write. A `duplicate_of` target
/// must be a live task of the same repository, re-checked here because the
/// proposal was made against the board as it was then.
pub(crate) fn apply_refinement(store: &Store, input: &str) -> Result<Value, WorkbenchError> {
    let parsed: ApplyInput = object_input(input)?;
    if !valid_ref(&parsed.id) || parsed.expected_revision < 1 {
        return Err(invalid(
            "Applying needs a task id and the revision reviewed.",
        ));
    }
    if parsed.acceptance_criteria.is_empty() && parsed.duplicate_of.is_empty() {
        return Err(invalid("Nothing was accepted."));
    }
    if parsed.acceptance_criteria.len() > MAX_APPLY_CRITERIA
        || parsed.duplicate_of.len() > MAX_APPLY_DUPLICATES
    {
        return Err(invalid("That is more than one refinement may apply."));
    }
    let repository = board_repository(store, &parsed.repo_path)?;
    let repository_id = repository["id"].as_str().unwrap_or_default().to_owned();
    let item = get_item(store, &parsed.id)?
        .filter(|item| links(item, &repository_id) && item["deleted"] != true)
        .ok_or_else(|| {
            WorkbenchError::new("not_found", "That task is not on this repository's board.")
        })?;
    let revision = item["revision"].as_i64().unwrap_or_default();
    if revision != parsed.expected_revision {
        return Err(WorkbenchError::new(
            "revision_conflict",
            format!(
                "The task is at revision {revision}, not {}; it changed after the proposal was reviewed. Re-read it and review again.",
                parsed.expected_revision
            ),
        ));
    }

    let mut criteria: Vec<String> = item["acceptance_criteria"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c.as_str().map(str::to_owned))
        .collect();
    let mut added_criteria = 0;
    for text in parsed.acceptance_criteria.iter().map(|c| c.trim()) {
        if text.is_empty() || text.chars().any(char::is_control) {
            return Err(invalid("An acceptance criterion is one non-empty line."));
        }
        if !criteria.iter().any(|c| c == text) {
            criteria.push(text.to_owned());
            added_criteria += 1;
        }
    }

    let mut task_links: Vec<Value> = item["links"].as_array().cloned().unwrap_or_default();
    let mut added_links = 0;
    for target in &parsed.duplicate_of {
        if !valid_ref(target) || target == &parsed.id {
            return Err(invalid("A duplicate link names another task by its id."));
        }
        let live = get_item(store, target)?
            .is_some_and(|t| links(&t, &repository_id) && t["deleted"] != true);
        if !live {
            return Err(WorkbenchError::new(
                "not_found",
                format!("{target} is not a live task of this repository, so it cannot be linked as the original."),
            ));
        }
        let present = task_links
            .iter()
            .any(|l| l["kind"] == "duplicate_of" && l["item_id"] == target.as_str());
        if !present {
            task_links.push(json!({"kind": "duplicate_of", "item_id": target}));
            added_links += 1;
        }
    }

    if added_criteria == 0 && added_links == 0 {
        return Ok(json!({
            "ok": true,
            "outcome": "unchanged",
            "item_id": parsed.id,
            "revision": revision,
            "criteria_added": 0,
            "links_added": 0,
        }));
    }
    let mut put = resend(&item, &parsed.id, revision, "refine");
    put.insert("acceptance_criteria".into(), json!(criteria));
    if added_links > 0 {
        put.insert("links".into(), json!(task_links));
    }
    let saved = query(store, "items.put", &Value::Object(put).to_string())?;
    Ok(json!({
        "ok": true,
        "outcome": "applied",
        "item_id": parsed.id,
        "revision": saved["item"]["revision"],
        "criteria_added": added_criteria,
        "links_added": added_links,
        "sequence": saved["sequence"],
    }))
}
