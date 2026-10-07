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
//!   written its final `unresolved`.
//! - A managed attempt with no process is judged by what Manvi itself
//!   guarantees, whichever Manvi claimed it: Manvi stops the provider before
//!   it records an unactivated attempt `unresolved`, and gives up on any
//!   attempt not activated within five minutes of its claim. So `unresolved`
//!   releases, and so does `starting` once [`MANAGED_ACTIVATION_GRACE_SECS`]
//!   have passed. Before that, an owner naming its Manvi
//!   (`manvi-{pid}-{nanos}-{token}`, since Manvi 18fc2c6) releases as soon as
//!   that Manvi is provably gone; an older, opaque owner waits out the grace.
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
/// How long after its claim a managed attempt that recorded no process could
/// still become active.
///
/// Manvi abandons a preparation not activated within five minutes, closing
/// the provider before it writes `unresolved` (`serve/managed.go`, since that
/// file's first commit), and the write itself is bounded at five seconds.
/// Twice that, so a slow host or a clock step never releases an attempt its
/// Manvi is still about to give up on. A run that is still `starting` past it
/// was left by a Manvi that died, or whose final write failed after it had
/// already stopped the provider.
pub(crate) const MANAGED_ACTIVATION_GRACE_SECS: u64 = 10 * 60;
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

/// [`judge_at`] against the clock now.
pub(crate) fn judge(
    run: &Value,
    tracked_here: Option<bool>,
    child: impl Fn(u32, &str) -> Liveness,
    owner_since: impl Fn(u32, u128) -> Liveness,
) -> Verdict {
    judge_at(run, tracked_here, child, owner_since, now_secs())
}

/// The pure decision, with every observation injected — the clock included,
/// as the store's seconds — so it can be driven through each branch without
/// real processes or waiting.
pub(crate) fn judge_at(
    run: &Value,
    tracked_here: Option<bool>,
    child: impl Fn(u32, &str) -> Liveness,
    owner_since: impl Fn(u32, u128) -> Liveness,
    now_secs: u64,
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
    if run["kind"] == "managed" {
        // No process was recorded, so this attempt was never activated: the
        // store records one with the activation. What Manvi does with an
        // unactivated attempt is the evidence, whichever build claimed it.
        if state == "unresolved" {
            return Verdict::Release(
                "Manvi recorded it unresolved before any agent process was recorded; it stops the provider before writing that"
                    .into(),
            );
        }
        if let Some(claimed) = run["claimed_at"].as_u64() {
            let waited = now_secs.saturating_sub(claimed);
            if state == "starting" && waited >= MANAGED_ACTIVATION_GRACE_SECS {
                return Verdict::Release(format!(
                    "it was claimed {} minutes ago and never activated; Manvi stops the provider of any attempt not activated within five minutes",
                    waited / 60
                ));
            }
        }
        if manvi_owner(run["owner_id"].as_str().unwrap_or("")).is_none() {
            return Verdict::Keep(format!(
                "An older Manvi claimed it and its process cannot be identified; {}",
                abandonment(run)
            ));
        }
    }
    if let Some((manvi, stamp)) = run["owner_id"].as_str().and_then(manvi_owner) {
        // Before activation a managed attempt has no provider pid; Manvi alone
        // knows its provider, and its jobs live only in its memory. So the
        // Manvi process is the evidence: gone means the attempt cannot
        // finish, and its provider lost the pipe it was driven over.
        return match owner_since(manvi, stamp) {
            Liveness::Gone => Verdict::Release(format!(
                "the Manvi process {manvi} that ran it has exited, and no agent process was recorded; a provider exits when the Manvi driving it does"
            )),
            Liveness::Alive => Verdict::Keep(format!(
                "Manvi (pid {manvi}) is still running this attempt; {}",
                abandonment(run)
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

/// When an unactivated managed attempt that is kept for now will be released
/// — promised only where the grace rule in [`judge_at`] can actually fire.
fn abandonment(run: &Value) -> String {
    if run["kind"] == "managed" && run["claimed_at"].as_u64().is_some() {
        format!(
            "if it is never activated, it is released {} minutes after its claim.",
            MANAGED_ACTIVATION_GRACE_SECS / 60
        )
    } else {
        "it records no claim time, so nothing here can tell when it was abandoned.".into()
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
    // A receipt its own observer could not store is better evidence than
    // anything judged here, so it goes in first.
    super::receipts::replay(state, Some(&request.id));
    let (verdict, item) = settle(state, &request.id)?;
    Ok(match verdict {
        Verdict::Release(_) => {
            json!({"ok":true,"released":true,"reason":item["reason"],"item":item})
        }
        Verdict::Keep(reason) => json!({"ok":true,"released":false,"reason":reason,"item":item}),
    })
}

/// Removes the task brief directories a crashed GitPulse left in the temp
/// root, judged by the same writer-liveness evidence a native owner is.
///
/// Separate from [`sweep`] because it needs no task store: a profile that was
/// never created can still have briefs on disk from a launch that crashed
/// before anything else was saved.
pub(super) fn sweep_briefs() -> usize {
    let removed = super::terminal_command::remove_abandoned_briefs(
        &std::env::temp_dir(),
        process_birth::running_since,
    );
    if removed > 0 {
        log::info!(target: "workbench", "removed {removed} task brief(s) left behind by an earlier session");
    }
    removed
}

/// Every held run, judged; the ones the evidence allows are released.
///
/// Returns how many were released. Bounded by `MAX_PAGES` per state, and a
/// failure on one run never stops the others.
/// Walks the runs in `run_state`, at most `MAX_PAGES` pages of `PAGE`,
/// newest first when `newest`. `visit` returns false to stop early. Returns
/// whether every run in the state was seen — false when the walk stopped at
/// the page cap, which a caller must not read as "there are no more".
fn walk(
    state: &WorkbenchState,
    run_state: &str,
    newest: bool,
    mut visit: impl FnMut(&Value) -> bool,
) -> Result<bool, WorkbenchError> {
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut input = json!({"state": run_state, "limit": PAGE, "newest": newest});
        if let Some(cursor) = &cursor {
            input["cursor"] = json!(cursor);
        }
        let page = state.with_store(|store| query(store, "runs.list", &input.to_string()))?;
        for run in page["items"].as_array().into_iter().flatten() {
            if !visit(run) {
                return Ok(true);
            }
        }
        match page["next_cursor"].as_str() {
            Some(next) if page["has_more"] == true => cursor = Some(next.to_owned()),
            _ => return Ok(true),
        }
    }
    Ok(false)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// How many attempts the store counts against the live-run limit: held runs,
/// and preparations not yet expired. Read-only. It can only undercount (a
/// preparation past the page cap is not seen), so a caller may refuse early
/// on it but never admit in the store's place: the store's own count is the
/// one that decides.
pub(super) fn live_count(state: &WorkbenchState) -> Result<u64, WorkbenchError> {
    let mut live = 0;
    for held in HELD {
        let page = state.with_store(|store| {
            query(
                store,
                "runs.list",
                &json!({"state": held, "limit": 1}).to_string(),
            )
        })?;
        live += page["total"].as_u64().unwrap_or(0);
    }
    // A preparation expires a fixed time after it is made, so newest first,
    // the first expired one means every older one has expired too.
    let now = now_secs();
    walk(state, "prepared", true, |run| {
        let open = run["expires_at"].as_u64().is_some_and(|at| at > now);
        live += u64::from(open);
        open
    })?;
    Ok(live)
}

/// Gives back the worktrees of preparations that expired without ever being
/// claimed (see `agent_worktree::reclaim`). The run rows are left as they
/// are: an expired preparation already holds nothing, and the store keeps
/// its history. Returns how many worktrees were removed; one that cannot be
/// removed is logged with the reason and tried again next time.
pub(super) fn reclaim_expired(state: &WorkbenchState) -> usize {
    let now = now_secs();
    let mut expired = Vec::new();
    let walked = walk(state, "prepared", true, |run| {
        if run["expires_at"].as_u64().is_some_and(|at| at <= now) {
            expired.push(run.clone());
        }
        true
    });
    if let Err(error) = walked {
        log::warn!(target: "workbench", "expired preparations could not be listed: {}: {}", error.code, error.message);
    }
    let mut removed = 0;
    for run in expired {
        let id = run["id"].as_str().unwrap_or_default();
        // A retry of this very attempt may be rebuilding its tree right now.
        let Ok(_place) = state.0.preparations.try_enter(id) else {
            continue;
        };
        let open_files = |path: &std::path::Path| state.open_files(path);
        if let Some(outcome) = super::agent_worktree::reclaim(&run, &open_files) {
            log::info!(target: "workbench", "expired attempt {id}: {}", outcome.describe());
            removed += usize::from(outcome.removed());
        }
    }
    removed
}

/// The held run whose checkout is `target` or inside it and which the
/// evidence says may still be working, with that evidence. A run that
/// reconciliation would release (its owner and process provably gone) does
/// not count. Every held run must be seen: a listing cut short is an error,
/// because "not found in what was read" is not "not there".
pub(super) fn working_in(
    state: &WorkbenchState,
    target: &std::path::Path,
) -> Result<Option<(Value, String)>, WorkbenchError> {
    let mut inside = Vec::new();
    for held in HELD {
        let complete = walk(state, held, false, |run| {
            if run["cwd"]
                .as_str()
                .is_some_and(|cwd| std::path::Path::new(cwd).starts_with(target))
            {
                inside.push(run.clone());
            }
            true
        })?;
        if !complete {
            return Err(WorkbenchError::new(
                "too_many_runs",
                format!(
                    "more than {} {held} runs are recorded",
                    MAX_PAGES as u64 * PAGE
                ),
            ));
        }
    }
    Ok(inside
        .into_iter()
        .find_map(|run| match observe(state, &run) {
            Verdict::Keep(why) => Some((run, why)),
            Verdict::Release(_) => None,
        }))
}

pub(super) fn sweep(state: &WorkbenchState) -> Result<usize, WorkbenchError> {
    // Receipts that storage refused when their runs ended are stored first,
    // so a recorded exit code is never overwritten by `outcome_uncertain`.
    super::receipts::replay(state, None);
    let mut ids = Vec::new();
    for held in HELD {
        walk(state, held, false, |run| {
            if let Some(id) = run["id"].as_str() {
                ids.push(id.to_owned());
            }
            true
        })?;
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
