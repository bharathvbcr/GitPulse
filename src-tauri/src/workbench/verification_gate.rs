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
//! newest `work.verify.task` evidence row for the item is read from it
//! directly. No sidecar call is needed to complete a task, so completing one
//! works with the sidecar down.
//!
//! The gate fails closed. Everything short of a settled `passed` record whose
//! head is still the head of the worktree it was taken in, or whose head has
//! landed on the default branch, is a hold, and a check that could not run
//! (unreadable store, unreadable head, unparseable record, unreadable
//! ancestry) is a hold that says so, never a quiet pass.
//!
//! Landed work. A lane's commit M is merged and the lane's worktree moves on
//! or is removed: the worktree no longer answers for M. A settled `passed`
//! record for this task and repository whose head M is an ancestor of (or is)
//! the default branch's tip still counts. The default branch and its tip are
//! read from the card's own repository (the checkout its identity names),
//! never from the record's worktree, which may be gone. If the default branch
//! cannot be resolved, M is unknown to the repository, or git fails, the card
//! is held and the message says which.
//!
//! What a claim verifies. The target of a claim is the `repo_path` the agent
//! passed to `gitpulse_complete_task`: that checkout, at its HEAD when the
//! claim was made. Pass the lane's own worktree, not the repository's main
//! checkout, or the commit judged is the main checkout's tip.

pub(crate) mod memo;
pub(crate) use memo::Memo;

use super::intake::resolve;
use super::WorkbenchError;
use crate::ipc_error::CommandError;
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

impl Record {
    /// Bytes of heap this record holds, for the memo's budget: the text of each
    /// field and of each criterion, finding and degraded line, and their
    /// headers.
    pub(crate) fn heap_bytes(&self) -> usize {
        const STRING: usize = std::mem::size_of::<String>();
        let fields = [
            &self.request_id,
            &self.item_id,
            &self.state,
            &self.outcome,
            &self.worktree,
            &self.head,
            &self.critic_summary,
        ]
        .iter()
        .map(|text| text.len())
        .sum::<usize>();
        let criteria: usize = self
            .criteria
            .iter()
            .map(|criterion| {
                std::mem::size_of::<Criterion>()
                    + criterion.text.len()
                    + criterion.status.len()
                    + criterion
                        .findings
                        .iter()
                        .map(|finding| STRING + finding.len())
                        .sum::<usize>()
            })
            .sum();
        let degraded: usize = self.degraded.iter().map(|line| STRING + line.len()).sum();
        std::mem::size_of::<Self>() + fields + criteria + degraded
    }
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

fn parse_record(data: &str) -> Result<Record, String> {
    serde_json::from_str(data).map_err(|e| {
        format!(
            "the newest verification record could not be parsed, so no verdict is established: {e}"
        )
    })
}

/// The gate for a claim of `done` on `item_id`, a task of the repository whose
/// identity is `repository_identity`, judged on the newest record `memo` holds
/// for it ([`Memo::latest`]: the one reader, a single newest-first walk held
/// while the store has not changed). The caller names the memo: a caller that
/// carries a backend asks the backend's, so a test can give each run its own,
/// and one with none asks [`Memo::shared`].
///
/// `claimant_head` is the commit the caller claims done for: the claim's head
/// for the driver, the checkout's HEAD for a claim made through
/// `complete_task`. `None` is a head that could not be read. It matters only
/// to the landed path: a pass whose worktree no longer answers for it counts
/// only when it is for exactly the claimed commit.
pub(crate) fn check(
    memo: &Memo,
    store: &Store,
    item_id: &str,
    repository_identity: &str,
    claimant_head: Option<&str>,
) -> Gate {
    match memo.latest(store, item_id) {
        Err(reason) => Gate::Held(reason),
        Ok(None) => Gate::Held(
            "no verification record exists for this task: Manvi's work.verify.task has not judged it".into(),
        ),
        Ok(Some(record)) => judge(
            &record,
            item_id,
            repository_identity,
            &current_head,
            &landed_on_default,
            claimant_head,
        ),
    }
}

/// What the spawn-gate report calls the launches [`current_head`] makes.
pub(crate) const CURRENT_HEAD_SOURCE: &str = "verification_gate.current_head";

/// HEAD of the worktree a verification was taken in, as it is now.
pub(crate) fn current_head(worktree: &str) -> Result<String, CommandError> {
    // The driver asks this of every held card every sweep, and the board asks
    // it per card on every refresh. HEAD is a function of HEAD and the one ref
    // it names (plus packed-refs), so only those files are stamped: stamping the
    // whole ref store made every ref move anywhere in the repository, by any
    // of the agents sharing it, a miss and a process. Anything the narrow stamp
    // cannot name exactly (a symref to a symref, reftable) falls back to the
    // whole-store stamp inside `git_text_head`. An unchanged worktree is
    // answered from the ref cache by file stats, with no process.
    crate::engine::process::with_spawn_source(CURRENT_HEAD_SOURCE, || {
        crate::engine::ref_cache::git_text_head(
            Path::new(worktree),
            &["rev-parse", "--verify", "HEAD^{commit}"],
        )
    })
    .map(|head| head.trim().to_owned())
}

/// What the spawn-gate report calls the launches [`landed_on_default`] makes.
pub(crate) const LANDED_SOURCE: &str = "verification_gate.landed_on_default";

/// A full, lowercase commit id: 40 hex digits (SHA-1) or 64 (SHA-256), exactly.
pub(crate) fn is_full_commit_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64)
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The local checkout a repository identity (`local:<git dir>`) names, when it
/// is a directory on this machine.
pub(crate) fn checkout_of(repository_identity: &str) -> Option<String> {
    let git_dir = Path::new(repository_identity.strip_prefix("local:")?);
    // By path component, not text: the identity spells a Windows checkout
    // `\\?\C:\...\.git`, with a backslash before `.git`.
    let checkout = match git_dir.file_name() {
        Some(name) if name == ".git" => git_dir
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())?,
        _ => git_dir,
    };
    if !checkout.is_dir() {
        return None;
    }
    checkout.to_str().map(str::to_owned)
}

/// Whether `commit` is the tip of, or an ancestor of, the default branch of the
/// repository `repository_identity` names. `Err` says what could not be read:
/// that is not a "no".
pub(crate) fn landed_on_default(repository_identity: &str, commit: &str) -> Result<bool, String> {
    if !is_full_commit_id(commit) {
        return Err(format!("{commit:?} is not a full commit id"));
    }
    let checkout = checkout_of(repository_identity)
        .ok_or_else(|| "the task's repository has no local checkout to read".to_owned())?;
    let checkout = Path::new(&checkout);
    let tip = crate::engine::git_reader::default_base(checkout)
        .checked()
        .map_err(|e| format!("the default branch could not be read: {}", e.message()))?
        .base
        .map(|(_, oid)| oid)
        .ok_or_else(|| "the repository has no default branch".to_owned())?;
    // A name that is shaped like an id (a tag, a branch) resolves to whatever
    // it names; only the id itself, byte for byte, is the verified commit.
    let spec = format!("{commit}^{{commit}}");
    let resolved = crate::engine::process::with_spawn_source(LANDED_SOURCE, || {
        crate::engine::git_cli::git_text(
            checkout,
            &["rev-parse", "--verify", "--end-of-options", &spec],
        )
    })
    .map_err(|e| format!("the verified commit could not be resolved: {}", e.message()))?;
    if resolved.trim() != commit {
        return Err(format!(
            "the verified head {commit:?} does not resolve to itself (it resolves to {:?})",
            resolved.trim()
        ));
    }
    crate::engine::process::with_spawn_source(LANDED_SOURCE, || {
        crate::harness::review::contains(checkout, &tip, commit)
    })
    .map_err(|e| e.message().to_owned())
}

pub(crate) fn judge(
    record: &Record,
    item_id: &str,
    repository_identity: &str,
    head_of: &dyn Fn(&str) -> Result<String, CommandError>,
    landed: &dyn Fn(&str, &str) -> Result<bool, String>,
    claimant_head: Option<&str>,
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
    // Why the worktree no longer answers for the verified commit. Everything
    // short of "it is still at that head" lands here, and only landed work
    // can still be a pass.
    let moved = match resolve(&record.worktree) {
        Ok(local) if local.identity != repository_identity => {
            return Gate::Held(format!(
                "the verified worktree {:?} belongs to another repository",
                record.worktree
            ))
        }
        Ok(_) => match head_of(&record.worktree) {
            Err(error) => format!(
                "the head of the verified worktree could not be read, so the verdict cannot be tied to the work: {error}", error = error.message()
            ),
            Ok(head) if head != record.head => format!(
                "verification passed for {} but the worktree is now at {}; the work changed after it was verified. Verify again",
                short(&record.head),
                short(&head)
            ),
            Ok(_) => return Gate::Passed(verdict),
        },
        Err(error) => format!(
            "the verified worktree {:?} could not be resolved: {}",
            record.worktree, error.message
        ),
    };
    // Landed work is a pass only for the commit it was verified at: commits
    // that landed on top of it were never judged.
    match claimant_head {
        None => {
            return Gate::Held(format!(
                "{moved}; the head the claim is for could not be read, so the verified commit {} cannot be tied to it",
                short(&record.head)
            ))
        }
        Some(claimed) if claimed != record.head => {
            return Gate::Held(format!(
                "{moved}; the claim is for {} but the verified commit is {}. Verify again",
                short(claimed),
                short(&record.head)
            ))
        }
        Some(_) => {}
    }
    match landed(repository_identity, &record.head) {
        Ok(true) => Gate::Passed(format!(
            "{verdict}\nThe verified commit {} is on the default branch.",
            short(&record.head)
        )),
        Ok(false) => Gate::Held(format!(
            "{moved}; the verified commit {} is not on the default branch either",
            short(&record.head)
        )),
        Err(why) => Gate::Held(format!(
            "{moved}; whether the verified commit {} reached the default branch could not be read: {why}",
            short(&record.head)
        )),
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
    if !super::is_record_id(&parsed.id) || !super::is_record_id(&parsed.request_id) {
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
                format!(
                    "The worktree's head could not be read: {e}",
                    e = e.message()
                ),
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
    if !super::is_record_id(&parsed.id)
        || parsed
            .request_id
            .as_deref()
            .is_some_and(|r| !super::is_record_id(r))
    {
        return Err(invalid(
            "Verification requires valid task and request identities.",
        ));
    }
    Ok(match parsed.request_id {
        Some(request_id) => json!({"item_id": parsed.id, "request_id": request_id}),
        None => json!({"item_id": parsed.id}),
    })
}
