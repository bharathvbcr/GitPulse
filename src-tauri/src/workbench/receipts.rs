//! A terminal run's receipts, and the ones storage refused to take.
//!
//! The native observer is the only witness to how a task terminal's process
//! started and ended. It used to write that straight to the store and, when the
//! write failed — the store closed for shutdown, a lock, a full disk — log it
//! and lose it. The attempt then held its checkout until reconciliation
//! released it as `outcome_uncertain` with no exit code: durable, but missing
//! the one fact the observer actually had.
//!
//! So what is journaled is the *observation* — what the observer saw — not a
//! store request. A request is built from the run's current revision, which
//! needs a `runs.get`, and when the store is what failed that read fails too:
//! a request journal would be empty in exactly the case it exists for. Replay
//! feeds the observation back through [`apply`], the same decision the live
//! observer makes, against whatever the store holds by then.
//!
//! Replaying is idempotent by three facts this module relies on rather than
//! adds to: the request ids are fixed per owner (`{owner}-started`,
//! `{owner}-finished`) and the store answers an exact replay with its stored
//! response; a run already `exited`/`failed`/`cancelled` is never written
//! again; and a start already recorded with the same identity is left alone.
//! A refusal that names the store's truth (`invalid_state`, `owner_mismatch`,
//! …) retires the receipt; anything that may be transient keeps it.

use super::{query, WorkbenchError, WorkbenchState};
use crate::terminal::TerminalExitPayload;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Directory, beside the profile database, holding unstored observations.
const JOURNAL_DIR: &str = "run-receipts";
/// An observation is a few hundred bytes; anything larger was not written here.
const MAX_RECEIPT_BYTES: u64 = 16 * 1024;
/// Bounds one replay pass.
const MAX_RECEIPTS_PER_REPLAY: usize = 256;
/// Store refusals that state what the run already is. Retrying cannot change
/// them, so the observation is retired.
const DEFINITIVE: &[&str] = &[
    "invalid_state",
    "owner_mismatch",
    "idempotency_conflict",
    "not_found",
    "invalid_input",
    "run_kind_mismatch",
];

/// How the observed process ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ObservedExit {
    pub exit_code: Option<i32>,
    pub signal: String,
    pub error: Option<String>,
    pub reaped: bool,
}

impl From<&TerminalExitPayload> for ObservedExit {
    fn from(payload: &TerminalExitPayload) -> Self {
        Self {
            exit_code: payload.exit_code,
            signal: payload.signal.clone(),
            error: payload.error.clone(),
            reaped: payload.reaped,
        }
    }
}

/// Everything one native observer has seen of its run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Observed {
    pub run_id: String,
    pub owner: String,
    pub session: String,
    /// The process id and creation identity, once the PTY reported them.
    pub start: Option<(u32, String)>,
    pub exit: Option<ObservedExit>,
    pub spawn_failure: Option<String>,
}

type StoreCall<'a> = dyn Fn(&str, &Value) -> Result<Value, WorkbenchError> + 'a;

fn current(store: &StoreCall<'_>, run_id: &str) -> Result<Value, WorkbenchError> {
    store("runs.get", &json!({ "id": run_id }))
}

/// Records the start, unless the store already holds this exact one.
fn record_start(
    store: &StoreCall<'_>,
    observed: &Observed,
    written: &mut Vec<Value>,
) -> Result<(), WorkbenchError> {
    let (pid, birth) = observed.start.clone().ok_or_else(|| {
        WorkbenchError::new(
            "process_error",
            "Process creation identity was unavailable.",
        )
    })?;
    let current = current(store, &observed.run_id)?;
    let item = &current["item"];
    if item["state"] == "running"
        && item["session_id"] == observed.session.as_str()
        && item["process_id"] == pid
        && item["process_start"] == birth.as_str()
    {
        return Ok(());
    }
    written.push(store(
        "runs.started",
        &json!({"id":observed.run_id,"request_id":format!("{}-started",observed.owner),"expected_revision":item["revision"],"owner_id":observed.owner,"session_id":observed.session,"process_id":pid,"process_start":birth}),
    )?);
    Ok(())
}

/// Records the run's ending, deriving the outcome from what was observed and
/// what the store holds now.
fn finish(
    store: &StoreCall<'_>,
    observed: &Observed,
    written: &mut Vec<Value>,
) -> Result<(), WorkbenchError> {
    let mut current = current(store, &observed.run_id)?;
    if observed.exit.is_some() && current["item"]["state"] == "starting" {
        // Retry only storage bookkeeping using retained, actually observed
        // process identity. This path can never execute another child.
        match record_start(store, observed, written) {
            Ok(()) => current = self::current(store, &observed.run_id)?,
            Err(error) => {
                log::warn!(target: "workbench", "Process-start receipt remains unavailable at exit: {}: {}", error.code, error.message)
            }
        }
    }
    let state = current["item"]["state"].as_str().unwrap_or("");
    if matches!(state, "exited" | "failed" | "cancelled") {
        return Ok(());
    }
    let exit = observed.exit.as_ref();
    let spawn_failure = observed.spawn_failure.as_deref();
    let outcome = if spawn_failure.is_some() {
        "failed"
    } else if exit.is_some_and(|e| e.reaped)
        && (state == "running" || (state == "unresolved" && current["item"]["process_id"].is_u64()))
    {
        "exited"
    } else {
        "unresolved"
    };
    let reason = match (spawn_failure, exit) {
        (Some(reason), _) => reason.to_owned(),
        (_, Some(exit)) if outcome == "exited" => format!(
            "Terminal process exited. Signal: {}. {}",
            exit.signal,
            exit.error.as_deref().unwrap_or("")
        ),
        (_, Some(exit)) if !exit.reaped => format!(
            "Process exit could not be confirmed. {}",
            exit.error
                .as_deref()
                .unwrap_or("Execution needs reconciliation.")
        ),
        _ => "Terminal ended without a durable process-start receipt; execution outcome needs reconciliation.".into(),
    };
    let reason: String = reason.chars().take(512).collect();
    let code = if outcome == "exited" {
        exit.and_then(|e| e.exit_code)
            .and_then(|c| u32::try_from(c).ok())
    } else {
        None
    };
    written.push(store(
        "runs.finish",
        &json!({"id":observed.run_id,"request_id":format!("{}-finished",observed.owner),"expected_revision":current["item"]["revision"],"owner_id":observed.owner,"session_id":observed.session,"outcome":outcome,"reason":reason,"exit_code":code}),
    )?);
    Ok(())
}

/// Brings the store up to date with `observed`: the start alone while the
/// process runs, the ending once it has one. Returns every response written,
/// for the caller to announce.
pub(super) fn apply(
    store: &StoreCall<'_>,
    observed: &Observed,
) -> Result<Vec<Value>, WorkbenchError> {
    let mut written = Vec::new();
    if observed.exit.is_none() && observed.spawn_failure.is_none() {
        record_start(store, observed, &mut written)?;
    } else {
        finish(store, observed, &mut written)?;
    }
    Ok(written)
}

/// Whether `id` is safe to use as a file name: the run-id charset `release`
/// already enforces.
fn valid_run_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn journal_dir(state: &WorkbenchState) -> Result<PathBuf, WorkbenchError> {
    let profile = state.profile_path()?;
    let parent = profile
        .parent()
        .ok_or_else(|| WorkbenchError::new("store_error", "Invalid profile database path."))?;
    Ok(parent.join(JOURNAL_DIR))
}

/// Durably keeps an observation the store would not take: synced temporary
/// file, rename, synced directory. One file per run; a later observation of
/// the same run replaces an earlier one, because it contains it.
pub(super) fn journal(state: &WorkbenchState, observed: &Observed) -> Result<(), String> {
    if !valid_run_id(&observed.run_id) {
        return Err(format!(
            "run id {:?} cannot name a receipt",
            observed.run_id
        ));
    }
    let dir = journal_dir(state).map_err(|e| e.message)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{}.json", observed.run_id));
    let temporary = dir.join(format!(".{}.{}.tmp", observed.run_id, std::process::id()));
    let data = serde_json::to_vec(observed).map_err(|e| e.to_string())?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(&data)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, &path)?;
        // The rename is what a crash before the directory is flushed loses.
        #[cfg(unix)]
        std::fs::File::open(&dir)?.sync_all()?;
        Ok::<(), std::io::Error>(())
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    Ok(())
}

/// Removes a run's journaled observation once the store holds it.
pub(super) fn retire(state: &WorkbenchState, run_id: &str) {
    if !valid_run_id(run_id) {
        return;
    }
    let Ok(dir) = journal_dir(state) else {
        return;
    };
    match std::fs::remove_file(dir.join(format!("{run_id}.json"))) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            log::warn!(target: "workbench", "could not retire the stored receipt for run {run_id}: {error}");
        }
    }
}

fn read(path: &Path) -> Result<Observed, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.file_type().is_file() || meta.len() > MAX_RECEIPT_BYTES {
        return Err("it is not a receipt this host wrote".into());
    }
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    serde_json::from_slice(&data).map_err(|e| e.to_string())
}

/// Replays journaled observations into the store; `only` limits it to one
/// run. Returns how many were applied.
///
/// Runs before reconciliation judges anything, so a recorded exit code beats
/// a reconciler's `outcome_uncertain`. A run whose terminal this process
/// still holds is skipped: its live observer is the better writer.
pub(super) fn replay(state: &WorkbenchState, only: Option<&str>) -> usize {
    let Ok(dir) = journal_dir(state) else {
        return 0;
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(error) => {
            log::warn!(target: "workbench", "could not read stored run receipts: {error}");
            return 0;
        }
    };
    let mut applied = 0;
    let mut considered = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(run_id) = name
            .to_str()
            .and_then(|n| n.strip_suffix(".json"))
            .filter(|id| valid_run_id(id))
        else {
            continue;
        };
        if only.is_some_and(|wanted| wanted != run_id) {
            continue;
        }
        considered += 1;
        if considered > MAX_RECEIPTS_PER_REPLAY {
            log::warn!(target: "workbench", "more than {MAX_RECEIPTS_PER_REPLAY} stored run receipts; the rest wait for the next pass");
            break;
        }
        if state
            .terminals()
            .is_some_and(|terminals| crate::terminal::tracks_run(terminals, run_id))
        {
            continue;
        }
        let observed = match read(&entry.path()) {
            Ok(observed) if observed.run_id == run_id => observed,
            Ok(_) | Err(_) => {
                log::warn!(target: "workbench", "discarded an unreadable stored receipt for run {run_id}");
                retire(state, run_id);
                continue;
            }
        };
        let store = |method: &str, input: &Value| {
            state.with_store(|store| query(store, method, &input.to_string()))
        };
        let mut outcome = apply(&store, &observed);
        // Someone wrote between the read and the write; read and decide again.
        for _ in 0..2 {
            match &outcome {
                Err(error) if error.code == "revision_conflict" => {
                    outcome = apply(&store, &observed);
                }
                _ => break,
            }
        }
        match outcome {
            Ok(_) => {
                retire(state, run_id);
                applied += 1;
                log::info!(target: "workbench", "stored the receipt run {run_id} could not record when it ended");
            }
            Err(error) if DEFINITIVE.contains(&error.code.as_str()) => {
                log::warn!(target: "workbench", "retired the stored receipt for run {run_id}; the store already holds otherwise: {}: {}", error.code, error.message);
                retire(state, run_id);
            }
            Err(error) => {
                log::warn!(target: "workbench", "the stored receipt for run {run_id} still could not be recorded; kept for the next pass: {}: {}", error.code, error.message);
            }
        }
    }
    applied
}
