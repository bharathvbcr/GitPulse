//! What a person does with a proposal Manvi made: file it, or apply it.
//!
//! Manvi's `work.scan.gaps` and `work.enhancements.refine` only *propose*; the
//! board is never written by them. This is the host side of the review: the
//! one place an accepted proposal becomes a task or a task edit, so the
//! protections the board already has hold for it too.
//!
//! * **Filing** goes through [`add_task`], the path `gitpulse_add_task` uses,
//!   with a key derived from the proposal's own identity (a scan fingerprint).
//!   Filing twice therefore finds the card the first filing made instead of making a second, and the related-task check
//!   still runs: a proposal that looks like open work is *not filed* until the
//!   person has seen those tasks and confirmed, by naming them.
//! * **Applying** a refinement is dc-store's own accept of the proposal Manvi
//!   stored, at the proposal and task revisions the person reviewed. A task
//!   that has changed since is refused, never overwritten.
//! * Nothing here is reachable without a person's action: Manvi's ops are
//!   read-only proposals, and these methods are the only writers.

use super::*;
use serde::Deserialize;

/// Bound on one request's JSON, before any parsing or store access.
pub(super) const MAX_REQUEST_BYTES: usize = 64 * 1024;

fn invalid(message: impl Into<String>) -> WorkbenchError {
    WorkbenchError::new("invalid_input", message)
}

fn object_input<T: for<'de> Deserialize<'de>>(input: &str) -> Result<T, WorkbenchError> {
    if input.len() > MAX_REQUEST_BYTES || !input.trim_start().starts_with('{') {
        return Err(invalid("A proposal request is a JSON object under 64 KiB."));
    }
    serde_json::from_str(input).map_err(|e| invalid(e.to_string()))
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
    if !crate::workbench::is_record_id(&parsed.scan_id) {
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

/// The parts of a refinement proposal (dc-store schema 12) a person selects
/// among.
const REFINE_PARTS: [&str; 3] = ["acceptance_criteria", "split_into", "duplicate_of"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyInput {
    repo_path: String,
    /// The stored proposal (`enhancement_id` of Manvi's refine reply).
    id: String,
    /// The proposal's own revision, as reviewed.
    expected_revision: i64,
    /// The task's revision, as reviewed.
    expected_task_revision: i64,
    fields: Vec<String>,
    /// Open tasks the person was shown as possibly the same work as a split
    /// and says it is separate from. Honoured as `file_proposal` honours it.
    #[serde(default)]
    reviewed_related: Vec<String>,
}

/// Accept the parts of a stored refinement proposal a person selected.
///
/// The one apply path: dc-store's `enhancements.accept` for a refinement
/// appends the criteria, merges `duplicate_of` links and creates each split as
/// an inbox task linked to this one as its parent, in one transaction, with the
/// proposal and task revisions checked. A task that moved since review is
/// refused (`revision_conflict`) and nothing is written. The host's part is
/// what the store leaves to its caller: the proposal must be of this
/// repository's board, and the new tasks' ids are made here, never by the
/// renderer. They are derived from the proposal and the split's index, so a
/// retry of the same accept names the same tasks.
pub(crate) fn apply_refinement(store: &Store, input: &str) -> Result<Value, WorkbenchError> {
    let parsed: ApplyInput = object_input(input)?;
    if !crate::workbench::is_record_id(&parsed.id)
        || parsed.expected_revision < 1
        || parsed.expected_task_revision < 1
    {
        return Err(invalid(
            "Applying needs a proposal id and the revisions reviewed.",
        ));
    }
    if parsed.fields.is_empty()
        || parsed.fields.len() > REFINE_PARTS.len()
        || parsed
            .fields
            .iter()
            .any(|f| !REFINE_PARTS.contains(&f.as_str()))
        || parsed
            .fields
            .iter()
            .enumerate()
            .any(|(i, f)| parsed.fields[..i].contains(f))
    {
        return Err(invalid(
            "Select distinct parts among acceptance_criteria, split_into and duplicate_of.",
        ));
    }
    let repository = board_repository(store, &parsed.repo_path)?;
    let repository_id = repository["id"].as_str().unwrap_or_default().to_owned();
    let proposal = query(
        store,
        "enhancements.get",
        &json!({"id": parsed.id}).to_string(),
    )?["item"]
        .clone();
    let task_id = proposal["task_id"].as_str().unwrap_or_default();
    let task = get_item(store, task_id)?
        .filter(|item| links(item, &repository_id) && item["deleted"] != true)
        .ok_or_else(|| {
            WorkbenchError::new(
                "not_found",
                "That proposal is not on this repository's board.",
            )
        })?;
    if proposal["kind"] != "refine" {
        return Err(invalid("That proposal is not a refinement."));
    }
    let proposed = &proposal["proposed"];
    let selected = |part: &str| parsed.fields.iter().any(|f| f == part);
    let mut accept = json!({
        "id": parsed.id,
        "request_id": item_id(&repository_id, &format!("refine-accept:{}:{}", parsed.id, parsed.expected_revision))
            .replacen(ITEM_PREFIX, "rfa-", 1),
        "expected_revision": parsed.expected_revision,
        "expected_task_revision": parsed.expected_task_revision,
        "fields": parsed.fields,
    });
    if selected("split_into") {
        let count = proposed["split_into"].as_array().map_or(0, Vec::len);
        accept["split_ids"] = json!((0..count)
            .map(|index| item_id(
                &repository_id,
                &format!("refine-split:{}:{index}", parsed.id)
            ))
            .collect::<Vec<_>>());
    }
    let have_links = |target: &str| {
        task["links"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|l| l["kind"] == "duplicate_of" && l["item_id"] == target)
    };
    let criteria_added = if selected("acceptance_criteria") {
        proposed["acceptance_criteria"]
            .as_array()
            .map_or(0, Vec::len)
    } else {
        0
    };
    let links_added = if selected("duplicate_of") {
        proposed["duplicate_of"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|d| !d["id"].as_str().is_some_and(have_links))
            .count()
    } else {
        0
    };
    // A split becomes an inbox task, so it gets the check `add_task` gives any
    // task: one that looks like open work is not created until the person has
    // seen those tasks and named them.
    if selected("split_into") {
        let mut held = Vec::new();
        let mut scan_complete = true;
        for (index, split) in proposed["split_into"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let title = split["title"].as_str().unwrap_or_default();
            let candidate = ExternalTask {
                key: format!("refine-split-{index}"),
                title: title.to_owned(),
                description: split["description"].as_str().unwrap_or_default().to_owned(),
                ..ExternalTask::default()
            };
            let related = related_open_tasks(store, &repository_id, task_id, &candidate)?;
            scan_complete &= related.complete;
            let unreviewed: Vec<Value> = related
                .tasks
                .into_iter()
                .filter(|t| {
                    !parsed
                        .reviewed_related
                        .iter()
                        .any(|r| t["item_id"].as_str() == Some(r.trim()))
                })
                .take(MAX_RELATED)
                .collect();
            if !unreviewed.is_empty() {
                held.push(json!({"index": index, "title": title, "related": unreviewed}));
            }
        }
        if !held.is_empty() {
            return Ok(json!({
                "ok": false,
                "outcome": "related_tasks_exist",
                "splits": held,
                "scan_complete": scan_complete,
            }));
        }
    }
    let saved = query(store, "enhancements.accept", &accept.to_string())?;
    let accepted = &saved["item"];
    Ok(json!({
        "ok": true,
        "outcome": "applied",
        "item_id": task_id,
        "enhancement_id": parsed.id,
        "revision": accepted["applied_task_revision"],
        "accepted_fields": accepted["accepted_fields"],
        "created_item_ids": accepted["created_item_ids"],
        "criteria_added": criteria_added,
        "links_added": links_added,
        "sequence": saved["sequence"],
    }))
}
