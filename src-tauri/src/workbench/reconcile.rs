//! Releasing run slots whose owner can no longer report.
//!
//! A run holds its checkout while it is `starting`, `running` or `unresolved`,
//! and only its recorded owner may `finish` it. An owner that crashed, was
//! force-quit or lost its receipt write never does, so before this module the
//! checkout stayed busy forever ("this repository already has a prepared,
//! active or unresolved run") and the only way out was editing the database.
//!
//! The store will release such an attempt (`runs.reconcile`) but cannot prove
//! anything stopped. This host gathers that proof, and the rule is one-sided:
//! a slot is released only on a definite *gone*.
//!
//! - A run that recorded its process is released when that process — matched
//!   by its creation identity, not just its PID — no longer exists. Alive keeps
//!   it ("still running as pid N"); a probe that could not run keeps it too.
//! - A run that never recorded a process is released only when the GitPulse
//!   process that launched it is gone, or is this process and has already
//!   written its final `unresolved`. A Manvi-owned attempt with no process is
//!   released only when the Manvi process its owner names is gone (owners
//!   are `manvi-{pid}-{nanos}-{token}` since Manvi 18fc2c6); one with an older
//!   opaque owner is left alone, because nothing here can prove it ended.
//! - A run whose PTY session this process still holds is never touched — its
//!   observer may be about to record the real exit code, which is better
//!   evidence than anything a reconciler has.

use super::process_birth::{self, Liveness};
use super::{query, WorkbenchError, WorkbenchState};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

/// The states the store counts as holding a checkout, apart from `prepared`
/// (which expires on its own and is ended with `cancel`).
const HELD: [&str; 3] = ["starting", "running", "unresolved"];
/// Bounds one sweep: three states, at most this many pages of 200 each.
const MAX_PAGES: usize = 8;
const PAGE: u64 = 200;

/// What reconciliation decided about one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The evidence shows nothing is running; release with this reason.
    Release(String),
    /// Something may still be running or reporting; keep the slot.
    Keep(String),
}

/// `native-{pid}-{nanos}-{seq}`: the owner a terminal launch records.
fn native_owner(owner: &str) -> Option<(u32, u128)> {
    let mut parts = owner.strip_prefix("native-")?.split('-');
    let pid = parts.next()?.parse::<u32>().ok().filter(|p| *p > 0)?;
    let stamp = parts.next()?.parse::<u128>().ok()?;
    parts.next()?.parse::<u64>().ok()?;
    parts.next().is_none().then_some((pid, stamp))
}

/// `manvi-{pid}-{nanos}-{token}`: the owner a managed claim records — the
/// Manvi process that claimed and when. The token is the random part that
/// keeps owners unique; only its shape is checked.
fn manvi_owner(owner: &str) -> Option<(u32, u128)> {
    let mut parts = owner.strip_prefix("manvi-")?.split('-');
    let pid = parts.next()?.parse::<u32>().ok().filter(|p| *p > 0)?;
    let stamp = parts.next()?.parse::<u128>().ok()?;
    let token = parts.next()?;
    let opaque =
        !token.is_empty() && token.len() <= 64 && token.bytes().all(|b| b.is_ascii_hexdigit());
    (opaque && parts.next().is_none()).then_some((pid, stamp))
}

/// The pure decision, with every observation injected so it can be driven
/// through each branch without real processes.
pub(crate) fn judge(
    run: &Value,
    tracked_here: Option<bool>,
    child: impl Fn(u32, &str) -> Liveness,
    owner_since: impl Fn(u32, u128) -> Liveness,
) -> Verdict {
    let state = run["state"].as_str().unwrap_or("");
    if !HELD.contains(&state) {
        return Verdict::Keep(format!("A {state} attempt does not hold its checkout."));
    }
    match tracked_here {
        Some(true) => {
            return Verdict::Keep(
                "This GitPulse window still holds the agent's terminal; its own exit record comes first."
                    .into(),
            )
        }
        Some(false) => {}
        None => {
            // Without the terminal registry, a run this very process owns
            // cannot be told apart from one it has finished observing.
            if let Some((pid, _)) = run["owner_id"].as_str().and_then(native_owner) {
                if pid == std::process::id() {
                    return Verdict::Keep(
                        "This GitPulse launched it and could not check its terminal registry."
                            .into(),
                    );
                }
            }
        }
    }
    if let (Some(pid), Some(birth)) = (
        run["process_id"]
            .as_u64()
            .and_then(|p| u32::try_from(p).ok()),
        run["process_start"].as_str(),
    ) {
        return match child(pid, birth) {
            Liveness::Gone => Verdict::Release(format!(
                "agent process {pid} is no longer running; how it ended was not observed"
            )),
            Liveness::Alive => Verdict::Keep(format!(
                "The agent process is still running as pid {pid}. Stop it to release this checkout."
            )),
            Liveness::Unknown(reason) => Verdict::Keep(format!(
                "Could not check whether agent process {pid} is still running: {reason}"
            )),
        };
    }
    if let Some((manvi, stamp)) = run["owner_id"].as_str().and_then(manvi_owner) {
        // Before activation a managed attempt has no provider pid; Manvi alone
        // knows its provider, and its jobs live only in its memory. So the
        // Manvi process is the evidence: gone means the attempt cannot
        // finish, and its provider lost the pipe it was driven over.
        return match owner_since(manvi, stamp) {
            Liveness::Gone => Verdict::Release(format!(
                "the Manvi process {manvi} that ran it has exited, and no agent process was recorded; its provider lost its connection with it"
            )),
            Liveness::Alive => Verdict::Keep(format!(
                "Manvi (pid {manvi}) is still running this attempt; it ends an attempt that is never activated within five minutes."
            )),
            Liveness::Unknown(reason) => Verdict::Keep(format!(
                "Could not check whether the Manvi process {manvi} running it is still alive: {reason}"
            )),
        };
    }
    let Some((owner, stamp)) = run["owner_id"].as_str().and_then(native_owner) else {
        return Verdict::Keep(
            "No agent process was recorded, and the host that launched it is not a GitPulse terminal, so nothing here can prove it ended."
                .into(),
        );
    };
    match owner_since(owner, stamp) {
        Liveness::Gone => Verdict::Release(format!(
            "the GitPulse process {owner} that launched it has exited, and no agent process was recorded; anything it started lost its terminal with it"
        )),
        Liveness::Alive if owner == std::process::id() && state == "unresolved" => {
            Verdict::Release(
                "this GitPulse recorded the launch as unresolved after stopping the agent, and no agent process was recorded"
                    .into(),
            )
        }
        Liveness::Alive if owner == std::process::id() => {
            Verdict::Keep("This launch is still starting.".into())
        }
        Liveness::Alive => Verdict::Keep(format!(
            "Another running GitPulse (pid {owner}) launched it and still owns its outcome."
        )),
        Liveness::Unknown(reason) => Verdict::Keep(format!(
            "Could not check whether the GitPulse process {owner} that launched it is still running: {reason}"
        )),
    }
}

/// One identity per reconciling write, never equal to any launch owner.
fn reconciler() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("reconciler-{}-{nanos}", std::process::id())
}

fn observe(state: &WorkbenchState, run: &Value) -> Verdict {
    let tracked = state
        .terminals()
        .map(|terminals| crate::terminal::tracks_run(terminals, run["id"].as_str().unwrap_or("")));
    judge(
        run,
        tracked,
        process_birth::probe,
        process_birth::running_since,
    )
}

/// Judges one run and, when the evidence allows, releases it.
///
/// A revision conflict means someone — most likely the owner — wrote in the
/// meantime; the run is read and judged once more rather than overwritten.
fn settle(state: &WorkbenchState, id: &str) -> Result<(Verdict, Value), WorkbenchError> {
    for _ in 0..2 {
        let run = state
            .with_store(|store| query(store, "runs.get", &json!({"id":id}).to_string()))?["item"]
            .clone();
        let verdict = observe(state, &run);
        let Verdict::Release(reason) = &verdict else {
            return Ok((verdict, run));
        };
        let input = json!({
            "id": id,
            "request_id": reconciler(),
            "expected_revision": run["revision"],
            "owner_id": reconciler(),
            "prior_owner_id": run["owner_id"],
            "reason": reason,
        });
        match state.with_store(|store| query(store, "runs.reconcile", &input.to_string())) {
            Ok(saved) => return Ok((verdict, saved["item"].clone())),
            Err(error) if error.code == "revision_conflict" => continue,
            Err(error) => return Err(error),
        }
    }
    Err(WorkbenchError::new(
        "revision_conflict",
        "The attempt kept changing while it was being checked. Try again.",
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Release {
    id: String,
}

/// `runs.release`: the explicit action on one stuck run.
pub(super) fn release(state: &WorkbenchState, input: &str) -> Result<Value, WorkbenchError> {
    if input.len() > 1024 {
        return Err(WorkbenchError::new(
            "invalid_input",
            "A release request exceeds 1 KiB.",
        ));
    }
    let request: Release = serde_json::from_str(input)
        .map_err(|e| WorkbenchError::new("invalid_input", e.to_string()))?;
    if request.id.is_empty()
        || request.id.len() > 128
        || !request
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(WorkbenchError::new(
            "invalid_input",
            "Select a saved attempt.",
        ));
    }
    let (verdict, item) = settle(state, &request.id)?;
    Ok(match verdict {
        Verdict::Release(_) => {
            json!({"ok":true,"released":true,"reason":item["reason"],"item":item})
        }
        Verdict::Keep(reason) => json!({"ok":true,"released":false,"reason":reason,"item":item}),
    })
}

/// Every held run, judged; the ones the evidence allows are released.
///
/// Returns how many were released. Bounded by `MAX_PAGES` per state, and a
/// failure on one run never stops the others.
pub(super) fn sweep(state: &WorkbenchState) -> Result<usize, WorkbenchError> {
    let mut ids = Vec::new();
    for held in HELD {
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut input = json!({"state": held, "limit": PAGE});
            if let Some(cursor) = &cursor {
                input["cursor"] = json!(cursor);
            }
            let page = state.with_store(|store| query(store, "runs.list", &input.to_string()))?;
            for run in page["items"].as_array().into_iter().flatten() {
                if let Some(id) = run["id"].as_str() {
                    ids.push(id.to_owned());
                }
            }
            match page["next_cursor"].as_str() {
                Some(next) if page["has_more"] == true => cursor = Some(next.to_owned()),
                _ => break,
            }
        }
    }
    let mut released = 0;
    for id in ids {
        match settle(state, &id) {
            Ok((Verdict::Release(_), _)) => released += 1,
            Ok((Verdict::Keep(reason), _)) => {
                log::info!(target: "workbench", "run {id} keeps its checkout: {reason}");
            }
            Err(error) => {
                log::warn!(target: "workbench", "run {id} could not be reconciled: {}: {}", error.code, error.message);
            }
        }
    }
    Ok(released)
}

#[cfg(test)]
#[path = "reconcile_tests.rs"]
mod tests;
