//! GitPulse as a Lappi caller (`docs/caller-contract.md` in Lappi-decision).
//!
//! One decision point: a staged change the deterministic classifier in
//! [`crate::ai::commit_brief`] could not type. There, and only there, GitPulse
//! may ask a running Lappi agent which conventional type fits, and may write a
//! local caller record of what it saw, chose and later observed.
//!
//! Three rules shape everything here:
//!
//! * **Off by default.** Asking and recording are two separate settings
//!   ([`crate::tool_config::LappiSettings`]), both off. `LAPPI_COLLECT=0`
//!   forces recording off whatever the setting says.
//! * **Admission only** (§3). An answer may pre-select a type in the draft the
//!   user edits, through [`commit_brief::prefill_type`], and only when it is a
//!   known type and the classifier had none. It never commits, never touches
//!   the hook contract, and never changes anything else.
//! * **Never in-process.** No agent means `unavailable` and GitPulse's own path.
//!   No release trains `gitpulse.commit_type` yet, so in practice every request
//!   today is refused `task_not_trained` and the draft is unchanged.

pub mod client;
pub mod record;
pub mod request;

use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::ai::commit_brief::{self, CommitDraft};
use client::{NotAskedReason, Outcome, UnavailableReason};
use record::{Appended, DecisionInput, Pending, Store, StoreStatus};

/// How long a draft waits for Lappi. The ask happens inside
/// `generate_commit_message`, which already runs off the UI thread
/// (`cmd_ai_generate_commit_message` → `off_thread`) and may go on to a local
/// model; this bounds connect, write and the whole reply together.
pub const ASK_DEADLINE: Duration = Duration::from_secs(2);

/// Everything one consultation needs, passed in so tests name every input.
pub struct Consult<'a> {
    pub ask: bool,
    pub socket: Option<&'a Path>,
    pub deadline: Duration,
    /// `Some` only when recording is on and allowed.
    pub store: Option<&'a Store>,
    pub pending: &'a Mutex<Pending>,
}

/// What a consultation did, for tests and logs.
#[derive(Debug, Clone, PartialEq)]
pub struct Consulted {
    pub outcome: Outcome,
    pub prefilled: Option<&'static str>,
    pub recorded: Option<Appended>,
}

static PENDING: Mutex<Pending> = Mutex::new(Pending::new());
static STORE: OnceLock<Option<Store>> = OnceLock::new();

/// The process's store, or `None` when `$HOME` is unset.
fn default_store() -> Option<&'static Store> {
    STORE
        .get_or_init(|| {
            let dir = Store::default_dir()?;
            match Store::new(dir) {
                Ok(store) => Some(store),
                Err(error) => {
                    log::warn!(target: "lappi", "caller records disabled: {error}");
                    None
                }
            }
        })
        .as_ref()
}

/// The production entry point: reads the settings and the environment, then
/// consults. With both settings off this returns after the (cached) config
/// read and before any socket or store I/O.
pub fn consult_on_commit_type(repo_path: &str, diff: &[u8], draft: &mut CommitDraft) {
    if !is_decision_point(draft) {
        return;
    }
    let settings = crate::tool_config::lappi_settings();
    let recording = settings.record_caller_data && !record::collect_forced_off();
    if !settings.ask_on_ambiguous_commit_type && !recording {
        return;
    }
    let socket = client::socket_path();
    let ctx = Consult {
        ask: settings.ask_on_ambiguous_commit_type,
        socket: socket.as_deref(),
        deadline: ASK_DEADLINE,
        store: if recording { default_store() } else { None },
        pending: &PENDING,
    };
    consult(&ctx, repo_path, diff, draft);
}

/// Only a draft the classifier could not type is a decision point.
fn is_decision_point(draft: &CommitDraft) -> bool {
    !draft.high_confidence && draft.change_type.is_none()
}

/// Ask (when allowed), admit (when the answer allows it), record (when on).
pub fn consult(
    ctx: &Consult<'_>,
    repo_path: &str,
    diff: &[u8],
    draft: &mut CommitDraft,
) -> Consulted {
    if !is_decision_point(draft) {
        return Consulted {
            outcome: Outcome::NotAsked {
                reason: NotAskedReason::ClassifierTyped,
            },
            prefilled: None,
            recorded: None,
        };
    }
    let record_id = match record::new_record_id() {
        Ok(id) => Some(id),
        Err(error) => {
            log::warn!(target: "lappi", "no record id could be drawn: {error}");
            None
        }
    };
    let started = Instant::now();
    let outcome = if !ctx.ask {
        Outcome::NotAsked {
            reason: NotAskedReason::Disabled,
        }
    } else if draft.patch_truncated {
        Outcome::NotAsked {
            reason: NotAskedReason::PatchTruncated,
        }
    } else if let Some(id) = &record_id {
        ask_commit_type(ctx.socket, diff, id, ctx.deadline)
    } else {
        Outcome::NotAsked {
            reason: NotAskedReason::NoRecordId,
        }
    };
    let latency_ms = outcome
        .asked()
        .then(|| u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));

    let prefilled = admit(&outcome, draft);
    if let Some(kind) = prefilled {
        draft.warnings.push(format!(
            "Lappi suggested the commit type `{kind}`; check it before committing."
        ));
    }

    let recorded = match (ctx.store, &record_id) {
        (Some(store), Some(id)) => record_decision(
            store,
            ctx.pending,
            repo_path,
            &DecisionInput {
                record_id: id,
                created_at: &now_rfc3339(),
                draft,
                prefilled,
                outcome: &outcome,
                latency_ms,
            },
        ),
        _ => None,
    };
    Consulted {
        outcome,
        prefilled,
        recorded,
    }
}

/// Build the request and ask, or say why not.
fn ask_commit_type(
    socket: Option<&Path>,
    diff: &[u8],
    record_id: &str,
    deadline: Duration,
) -> Outcome {
    let line = match request::commit_type_request(diff, record_id) {
        Ok(line) => line,
        Err(reason) => return Outcome::NotAsked { reason },
    };
    match socket {
        Some(socket) => client::ask(socket, &line, deadline),
        None => Outcome::Unavailable {
            reason: UnavailableReason::SocketNotFound,
        },
    }
}

/// The one thing an answer may do: fill the type the classifier left empty.
fn admit(outcome: &Outcome, draft: &mut CommitDraft) -> Option<&'static str> {
    let chosen = outcome.chosen(request::COMMIT_TYPE_SLOT)?;
    let known = commit_brief::KNOWN_TYPES
        .iter()
        .copied()
        .find(|known| *known == chosen)?;
    commit_brief::prefill_type(draft, known).then_some(known)
}

fn record_decision(
    store: &Store,
    pending: &Mutex<Pending>,
    repo_path: &str,
    input: &DecisionInput<'_>,
) -> Option<Appended> {
    let line = match record::decision_line(input) {
        Ok(line) => line,
        Err(error) => {
            log_failure(store, &error.to_string());
            return None;
        }
    };
    let appended = append(store, input.created_at, &line)?;
    if appended == Appended::Written {
        pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remember(
                &repo_key(repo_path),
                input.record_id.to_string(),
                Instant::now(),
            );
    }
    Some(appended)
}

/// The production commit hook: pairs a committed message with its draft's
/// decision, when recording is on. A failure here never fails the commit.
pub fn record_commit_outcome(repo_path: &str, message: &str) {
    let settings = crate::tool_config::lappi_settings();
    let store = if settings.record_caller_data && !record::collect_forced_off() {
        default_store()
    } else {
        None
    };
    observe_commit(store, &PENDING, repo_path, message);
}

/// Write the outcome line for `repo_path`'s pending decision, if there is one.
///
/// The pending entry is consumed whether or not recording is on, so a record
/// switched back on later cannot pair a commit with a stale draft.
pub fn observe_commit(
    store: Option<&Store>,
    pending: &Mutex<Pending>,
    repo_path: &str,
    message: &str,
) -> Option<Appended> {
    let record_id = pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take(&repo_key(repo_path), Instant::now())?;
    let store = store?;
    if message.trim().is_empty() {
        return None;
    }
    let created_at = now_rfc3339();
    let line = match record::outcome_line(&record_id, &created_at, message) {
        Ok(line) => line,
        Err(error) => {
            log_failure(store, &error.to_string());
            return None;
        }
    };
    append(store, &created_at, &line)
}

fn append(store: &Store, created_at: &str, line: &[u8]) -> Option<Appended> {
    let day = created_at.get(..10).unwrap_or("");
    match store.append(day, line) {
        Ok(appended) => Some(appended),
        Err(error) => {
            log_failure(store, &error.to_string());
            None
        }
    }
}

/// A failed record is logged once per process; it never changes a decision.
fn log_failure(store: &Store, error: &str) {
    if store.first_failure() {
        log::warn!(
            target: "lappi",
            "a caller record was not written to {} ({error}); later failures are counted, not logged",
            store.dir().display()
        );
    }
}

/// Repositories are keyed by their canonical path, so a draft and a commit
/// that spell the same repository differently still pair.
fn repo_key(repo_path: &str) -> String {
    std::fs::canonicalize(repo_path)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| repo_path.to_string())
}

fn now_rfc3339() -> String {
    crate::ledger::ids::iso8601_utc(crate::ledger::ids::now_millis())
}

/// The settings panel's view: both switches, and what recording has done.
#[derive(Debug, Clone, Serialize)]
pub struct LappiView {
    pub settings: crate::tool_config::LappiSettings,
    /// `LAPPI_COLLECT=0` is set, so recording is off whatever the switch says.
    pub collect_forced_off: bool,
    pub socket: Option<String>,
    /// `None` until recording has been on in this process, or with no `$HOME`.
    pub store: Option<StoreStatus>,
    pub transport_supported: bool,
}

pub fn view() -> LappiView {
    LappiView {
        settings: crate::tool_config::lappi_settings(),
        collect_forced_off: record::collect_forced_off(),
        socket: client::socket_path().map(|path| path.display().to_string()),
        store: STORE.get().and_then(Option::as_ref).map(Store::status),
        transport_supported: cfg!(unix),
    }
}

#[cfg(all(test, unix))]
mod tests;
