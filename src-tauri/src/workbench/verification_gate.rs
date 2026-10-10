//! Whether a board task has been verified, read from the evidence Manvi stores.
//!
//! An agent's `gitpulse_complete_task(done)` is a claim, not a result. Manvi's
//! `work.verify.task` gathers its own evidence for a task (the operator's
//! verification command, the rigor gates, DevMap's affected tests and a
//! read-only critic over each acceptance criterion) and persists the verdict
//! as a dc-store evidence row of type `work.verify.task`, keyed by the board
//! item id. This module reads that row and nothing else: GitPulse never
//! re-judges a task, it only declines to take an agent's word where Manvi has
//! not vouched for it.
//!
//! How the record is read. The profile workbench store the sidecar writes is
//! the store GitPulse already opens in-process (`dc_store::Store`), so the
//! newest `work.verify.task` evidence row for the item is read with the same
//! `evidence_list` reader `gitpulse_task_view` uses. No sidecar call is needed
//! to complete a task, so completing one works with the sidecar down.
//!
//! The gate fails closed. Everything short of a settled `passed` record whose
//! head is still the head of the worktree it was taken in is a hold, and a
//! check that could not run (unreadable store, unreadable head, unparseable
//! record) is a hold that says so, never a quiet pass.

use super::intake::resolve;
use super::WorkbenchError;
use dc_store::Store;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

/// The dc-store evidence type Manvi stores a verification record under.
pub(crate) const EVIDENCE_TYPE: &str = "work.verify.task";
/// The state the host's own record carries when verification could not be
/// started or never answered. Manvi never writes it; it is a different thing
/// from a `failed` or `degraded` verdict, which Manvi reached by running.
pub(crate) const STATE_COULD_NOT_RUN: &str = "could_not_run";
/// Bound on the per-criterion text carried into a task's history.
const MAX_SUMMARY_CHARS: usize = 1500;

/// One acceptance criterion's finding, as Manvi recorded it.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, serde::Serialize)]
pub(crate) struct Criterion {
    #[serde(default)]
    pub index: i64,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub findings: Vec<String>,
}

/// The fields of Manvi's `VerifyRecord` the board needs.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, serde::Serialize)]
pub(crate) struct Record {
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub item_id: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub worktree: String,
    #[serde(default)]
    pub head: String,
    #[serde(default)]
    pub criteria: Vec<Criterion>,
    #[serde(default)]
    pub degraded: Vec<String>,
    #[serde(default)]
    pub critic_summary: String,
}

/// What the gate decided for a claim of `done`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Gate {
    /// A settled `passed` record for the current head; the summary goes into
    /// the task's history.
    Passed(String),
    /// Not established. The reason says which check failed or could not run.
    Held(String),
}

/// The newest verification record for `item_id`, `None` when the item has
/// never been verified.
///
/// Evidence rows are append-only and the newest row for a request is its
/// current state, so the newest row of this type decides. An unparseable row
/// is an error rather than a skip: skipping it would let an older `passed`
/// row stand in for a newer verdict nobody could read.
pub(crate) fn latest(store: &Store, item_id: &str) -> Result<Option<Record>, String> {
    let (rows, truncated) = store
        .evidence_list(Some(item_id))
        .map_err(|e| format!("the verification evidence could not be read: {e}"))?;
    for row in rows {
        if row.kind != EVIDENCE_TYPE {
            continue;
        }
        let record: Record = serde_json::from_str(&row.data_json).map_err(|e| {
            format!("the newest verification record could not be parsed, so no verdict is established: {e}")
        })?;
        return Ok(Some(record));
    }
    if truncated {
        return Err(
            "the task's evidence history is too long to establish whether it was ever verified"
                .into(),
        );
    }
    Ok(None)
}

/// The gate for a claim of `done` on `item_id`, a task of the repository whose
/// identity is `repository_identity`.
pub(crate) fn check(store: &Store, item_id: &str, repository_identity: &str) -> Gate {
    match latest(store, item_id) {
        Err(reason) => Gate::Held(reason),
        Ok(None) => Gate::Held(
            "no verification record exists for this task: Manvi's work.verify.task has not judged it".into(),
        ),
        Ok(Some(record)) => judge(&record, item_id, repository_identity, &current_head),
    }
}

/// HEAD of the worktree a verification was taken in, as it is now.
pub(crate) fn current_head(worktree: &str) -> Result<String, String> {
    crate::engine::git_cli::git_text(
        Path::new(worktree),
        &["rev-parse", "--verify", "HEAD^{commit}"],
    )
    .map(|head| head.trim().to_owned())
}

fn judge(
    record: &Record,
    item_id: &str,
    repository_identity: &str,
    head_of: &dyn Fn(&str) -> Result<String, String>,
) -> Gate {
    let verdict = summarize(record);
    if record.item_id != item_id {
        return Gate::Held(format!(
            "the newest verification record is for another task ({:?}), not this one",
            record.item_id
        ));
    }
    if record.state == STATE_COULD_NOT_RUN {
        return Gate::Held(format!(
            "verification could not run, so no verdict exists. {verdict}"
        ));
    }
    if record.state != "settled" {
        return Gate::Held(format!(
            "verification is still {:?}; no verdict yet. {verdict}",
            record.state
        ));
    }
    if record.outcome != "passed" {
        return Gate::Held(format!(
            "verification did not pass (outcome {:?}). {verdict}",
            record.outcome
        ));
    }
    if record.head.trim().is_empty() || record.worktree.trim().is_empty() {
        return Gate::Held(
            "the verification record names no worktree or head, so it cannot be tied to the work"
                .into(),
        );
    }
    match resolve(&record.worktree) {
        Ok(local) if local.identity == repository_identity => {}
        Ok(_) => {
            return Gate::Held(format!(
                "the verified worktree {:?} belongs to another repository",
                record.worktree
            ))
        }
        Err(error) => {
            return Gate::Held(format!(
                "the verified worktree {:?} could not be resolved: {}",
                record.worktree, error.message
            ))
        }
    }
    match head_of(&record.worktree) {
        Err(error) => Gate::Held(format!(
            "the head of the verified worktree could not be read, so the verdict cannot be tied to the work: {error}"
        )),
        Ok(head) if head != record.head => Gate::Held(format!(
            "verification passed for {} but the worktree is now at {}; the work changed after it was verified. Verify again",
            short(&record.head),
            short(&head)
        )),
        Ok(_) => Gate::Passed(verdict),
    }
}

fn short(oid: &str) -> &str {
    oid.get(..12).unwrap_or(oid)
}

/// One bounded paragraph for the task's history: outcome, head, the criteria
/// that were not met and what Manvi could not check.
pub(crate) fn summarize(record: &Record) -> String {
    if record.state == STATE_COULD_NOT_RUN {
        return format!(
            "Verification could not run for {}: {}",
            short(&record.head),
            record.degraded.join("; ")
        );
    }
    let mut text = format!(
        "Verification {} for {} (request {}).",
        if record.outcome.is_empty() {
            "unsettled"
        } else {
            &record.outcome
        },
        short(&record.head),
        record.request_id
    );
    for criterion in &record.criteria {
        text.push_str(&format!(
            "\n- criterion {}: {}",
            criterion.index, criterion.status
        ));
        if criterion.status != "met" {
            text.push_str(&format!(" - {}", criterion.text));
            for finding in &criterion.findings {
                text.push_str(&format!("\n    {finding}"));
            }
        }
    }
    for item in &record.degraded {
        text.push_str(&format!("\n- not checked: {item}"));
    }
    if text.chars().count() > MAX_SUMMARY_CHARS {
        text = text.chars().take(MAX_SUMMARY_CHARS - 1).collect::<String>() + "…";
    }
    text
}

/// A host request to start verifying a task. The request names no command and
/// no criteria: Manvi takes both from the stored item and from the operator's
/// own scope, so the only thing GitPulse supplies is which work to judge.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartInput {
    id: String,
    request_id: String,
    worktree: String,
    base: String,
    /// The commit to judge; the worktree's current head when absent, which is
    /// the head the gate compares a later claim against.
    head: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GetInput {
    id: String,
    request_id: Option<String>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

fn invalid(message: &str) -> WorkbenchError {
    WorkbenchError::new("invalid_input", message)
}

fn object_input<T: for<'de> Deserialize<'de>>(input: &str) -> Result<T, WorkbenchError> {
    if input.len() > 8192 || !input.trim_start().starts_with('{') {
        return Err(invalid(
            "A verification request is a JSON object under 8 KiB.",
        ));
    }
    serde_json::from_str(input).map_err(|e| invalid(&e.to_string()))
}

/// Params for Manvi's `work.verify.task`, validated before any process I/O.
pub(crate) fn start_params(input: &str) -> Result<Value, WorkbenchError> {
    let parsed: StartInput = object_input(input)?;
    if !valid_id(&parsed.id) || !valid_id(&parsed.request_id) {
        return Err(invalid(
            "Verification requires valid task and request identities.",
        ));
    }
    if parsed.base.trim().is_empty() || parsed.worktree.trim().is_empty() {
        return Err(invalid(
            "Verification requires a worktree and a base revision.",
        ));
    }
    let head = match parsed
        .head
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
    {
        Some(head) => head.to_owned(),
        None => current_head(&parsed.worktree).map_err(|e| {
            WorkbenchError::new(
                "repository_unavailable",
                format!("The worktree's head could not be read: {e}"),
            )
        })?,
    };
    Ok(json!({
        "item_id": parsed.id,
        "request_id": parsed.request_id,
        "worktree": parsed.worktree,
        "base": parsed.base.trim(),
        "head": head,
    }))
}

/// Params for Manvi's `work.verify.get`.
pub(crate) fn get_params(input: &str) -> Result<Value, WorkbenchError> {
    let parsed: GetInput = object_input(input)?;
    if !valid_id(&parsed.id) || parsed.request_id.as_deref().is_some_and(|r| !valid_id(r)) {
        return Err(invalid(
            "Verification requires valid task and request identities.",
        ));
    }
    Ok(match parsed.request_id {
        Some(request_id) => json!({"item_id": parsed.id, "request_id": request_id}),
        None => json!({"item_id": parsed.id}),
    })
}
