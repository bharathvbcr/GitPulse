//! Live index: `devmap build` off watcher `repo-changed` and repo activation.
//!
//! ## Gate
//!
//! Refresh only when the index is believed stale, the schema is usable, and
//! no build is already running for that repo. "Fresh" means the caller has
//! already folded CLI `is_fresh` together with any watcher dirty signal —
//! a settled write makes the working tree diverge from the map even when the
//! store's pending queue is empty (no daemon).
//!
//! Incremental `devmap build --json` cannot restamp analyzer identity, and it
//! writes the database *only* — no `repo_map.json`, no `code_graph.json`. So
//! this module shells out through [`super::build`] (`--manifest`) whenever the
//! store needs a full rebuild (`rebuild_required`) or a consumer artifact is
//! absent, which includes the first build of every newly opened repository.
//! Ordinary source-tree staleness on a repository that already has its
//! artifacts stays incremental.
//!
//! ## Duty cycle: every build sets a wait
//!
//! Every live build that runs sets a per-repository wait, and watcher ticks
//! inside it are answered [`LiveRefreshDecision::SkipCooldown`] without
//! probing anything; the frontend holds its next ask until the wait ends, so
//! every edit inside it is folded into one build.
//!
//! * A productive build rests `max(30 s, 2 x its duration)`, capped at 10 min
//!   ([`rest_after_build`]). It used to *clear* the wait instead, so an agent
//!   editing continuously kept a build running back to back.
//! * An `unchanged: true` echo or a failed build waits at least that long, and
//!   longer on a streak (1 s → 2 s → … capped at 60 s, under the same floor):
//!   both used to reopen a 1 Hz loop whenever the build wrote under a watched
//!   path the noise gate missed.
//! * A build that ran past [`cli::BUILD_DEADLINE`] pauses automatic indexing
//!   for at least 10 min, doubling on a streak, and the outcome says the index
//!   is too slow ([`LiveRefreshOutcome::index_too_slow`]).
//!
//! Activation (`repo_changed: false`) still reads status so a payload-obsolete
//! index can `--manifest` rebuild, and a missing artifact can be restored,
//! even while a wait is active. An echo's or a rest's wait *does* block
//! activation incremental rebuilds: the last build already indexed the tree,
//! and treating focus as a new stale signal restarted a status+build loop.
//! Failed-build waits still allow activation retry; a too-slow pause does not.
//! Explicit `devmap build` / `refresh` commands call
//! [`clear_live_echo_cooldown`], which drops every wait for the repository.
//!
//! ## Off the IPC path
//!
//! [`start_refresh`] — what the command calls — runs an authorized build on a
//! background thread and answers [`LiveRefreshDecision::Started`]; the next
//! ask collects the outcome. One live build runs at a time across every
//! repository ([`LiveBuildSlot`]), the bound the frontend's single queue slot
//! used to provide by awaiting each build.
//!
//! ## Store lifetime
//!
//! Never hold an open [`devmap_store::Store`] across a build. This module
//! only reads CLI status JSON, then shells out through [`super::refresh`] or
//! [`super::build`], which acquire [`super::cli::BuildGuard`] for the duration
//! of the child.

use super::cli::{self, BuildOutcome, CliStatus};
use crate::engine::git_cli::is_deferred_under_load;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Inputs the pure gate needs — assembled by [`maybe_refresh`] or tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveRefreshFacts {
    /// CLI/status answered at all (binary present, JSON parseable).
    pub available: bool,
    /// Effective freshness after folding status + watcher dirty.
    pub is_fresh: bool,
    /// `!schema_outdated` — an outdated schema needs a tooling upgrade, not
    /// an incremental rebuild against the wrong shape.
    pub schema_ok: bool,
    /// [`super::cli::is_build_in_flight`] already true for this repo.
    pub already_building: bool,
    /// The CLI's own `rebuild_required`: a full `devmap build` fixes this
    /// store. The kernel sets it for payload-obsolete and *migratable*
    /// schema-behind stores, and deliberately leaves it false for `newer`,
    /// `foreign` and `unsupported` schemas — rebuilding those either fails or
    /// downgrades a database a newer reader owns.
    pub rebuild_required: bool,
    /// A consumer artifact the Code → Map pane reads (`repo_map.json`,
    /// `code_graph.json`) is absent from this repository's resolved state
    /// directory. A store can report `is_fresh` with no artifacts at all —
    /// the incremental build the live path runs writes the database only — so
    /// store freshness alone must never authorize a skip.
    pub artifacts_missing: bool,
    /// Paths a `devmap serve` daemon has queued for this repository and not
    /// yet folded into a generation. `None` means no daemon answered, or that
    /// we could not ask — never "a daemon with nothing to do", which is `0`.
    ///
    /// The daemon watches the tree and rebuilds the store itself, and takes the
    /// same writer lock a build from here would, so rebuilding alongside it is
    /// not merely redundant — it queues behind it. But the stand-down is
    /// deliberately gated on *queued work* rather than on the daemon merely
    /// being alive: if the CLI says the store is stale and the daemon reports
    /// nothing pending, the two disagree, which means the daemon's watcher did
    /// not see the change. Standing down on that would leave the index
    /// silently stale with nobody rebuilding it.
    pub daemon_pending: Option<u64>,
}

/// What the gate decided — every skip reason is distinct so the strip and
/// tests can tell "already current" from "already running".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveRefreshDecision {
    Refresh,
    SkipFresh,
    SkipBuilding,
    SkipSchemaOutdated,
    SkipUnavailable,
    /// Watcher echo / failed-build backoff still active for this repo.
    SkipCooldown,
    /// A `devmap serve` daemon has this repository's changes queued and is
    /// rebuilding them itself. Not a permanent skip: the daemon drains, the
    /// store goes fresh, and the next tick answers `SkipFresh` instead.
    SkipDaemon,
    /// The spawn gate declined to start the status probe or the build under
    /// load (background work is shed first). Nothing was examined, so this is
    /// not [`Self::SkipUnavailable`]: the same request may succeed in a few
    /// seconds, and reporting it as unavailable let the frontend settle a
    /// repository that was never looked at.
    SkipDeferred,
    /// A build was authorized and started on a background thread; the IPC
    /// call did not wait for it. Its outcome — a [`Self::Refresh`] carrying the
    /// build — is handed to the next ask for this repository, and until then
    /// every ask answers [`Self::SkipBuilding`] without probing anything.
    Started,
}

impl LiveRefreshDecision {
    pub fn should_refresh(self) -> bool {
        matches!(self, Self::Refresh)
    }
}

/// Which CLI rebuild an authorized live refresh must spawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveRebuildKind {
    Incremental,
    Manifest,
}

/// The store's exact reason when grammar/analyzer identity no longer matches.
/// Incremental vacuum cannot clear it; only `devmap build --manifest` can.
const OBSOLETE_PAYLOAD_REASON: &str = "stored extraction payload is obsolete";

const COOLDOWN_CAP_SECS: u64 = 60;

/// Shortest rest after any live build, however quick.
pub(crate) const LIVE_REST_FLOOR: Duration = Duration::from_secs(30);

/// Longest rest a build's own duration can earn ("~10 min" in the audit).
pub(crate) const LIVE_REST_CAP: Duration = Duration::from_secs(10 * 60);

/// Rest after a live build that ran past [`cli::BUILD_DEADLINE`].
pub(crate) const TOO_SLOW_REST: Duration = Duration::from_secs(10 * 60);

/// Ceiling for consecutive timed-out builds, which double [`TOO_SLOW_REST`].
pub(crate) const TOO_SLOW_REST_CAP: Duration = Duration::from_secs(60 * 60);

/// What the last live build for a repository meant for the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitKind {
    /// The build did its job. Watcher ticks wait out the rest, and every edit
    /// inside it is folded into the one build that follows. Activation still
    /// reads status, so a fresh store reports fresh and a missing artifact or
    /// obsolete payload still heals; only an incremental rebuild waits.
    Rest,
    /// `unchanged: true`: the build proved the tree already current. Activation
    /// must not spawn another incremental rebuild while this holds;
    /// payload-obsolete and missing artifacts still bypass it.
    Echo,
    /// The build failed. Activation may retry.
    Failure,
    /// The build ran past its deadline. Nothing automatic runs until the wait
    /// elapses — retrying at the failure cadence is what kept a 15-minute
    /// child running back to back — and the Map strip says why.
    TooSlow,
}

#[derive(Debug, Clone)]
struct LiveWait {
    until: Instant,
    /// Consecutive echo/failure builds that raised the wait (0 before first).
    streak: u32,
    /// Consecutive timed-out builds (0 for the first).
    too_slow_streak: u32,
    kind: WaitKind,
}

fn live_waits() -> &'static Mutex<HashMap<String, LiveWait>> {
    static MAP: OnceLock<Mutex<HashMap<String, LiveWait>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The waits map, recovered from poisoning: a panic elsewhere must not turn
/// the governor off, which is what answering "no wait" would do.
fn lock_live_waits() -> std::sync::MutexGuard<'static, HashMap<String, LiveWait>> {
    live_waits()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Drop every live-build wait for `repo_path` (explicit build/refresh).
///
/// The user asking for a build is the one signal that outranks the governor,
/// including a "too slow" pause: they have chosen to spend the time.
pub fn clear_live_echo_cooldown(repo_path: &str) {
    lock_live_waits().remove(repo_path);
}

#[cfg(all(test, unix))]
pub(crate) fn clear_all_live_echo_cooldowns() {
    lock_live_waits().clear();
    lock_finished_builds().clear();
}

/// Next cooldown length after `streak` consecutive echo/failure builds.
///
/// `streak` is the count already recorded before this raise (0 → 1s, 1 → 2s, …).
pub(crate) fn cooldown_secs_for_streak(streak: u32) -> u64 {
    let shift = streak.min(6);
    (1u64 << shift).min(COOLDOWN_CAP_SECS)
}

/// Rest after a live build that took `elapsed`: `max(30 s, 2 x elapsed)`,
/// capped at [`LIVE_REST_CAP`]. A build that cost a minute of CPU earns two
/// minutes of quiet, so the index spends at most a third of the wall clock
/// building however fast an agent edits.
pub(crate) fn rest_after_build(elapsed: Duration) -> Duration {
    elapsed
        .saturating_mul(2)
        .clamp(LIVE_REST_FLOOR, LIVE_REST_CAP)
}

/// Rest after the `streak`-th consecutive timed-out build (0 for the first):
/// 10 min, doubling, capped at an hour.
pub(crate) fn too_slow_rest(streak: u32) -> Duration {
    TOO_SLOW_REST
        .saturating_mul(1u32 << streak.min(3))
        .min(TOO_SLOW_REST_CAP)
}

fn live_wait(repo_path: &str, now: Instant) -> Option<(Duration, WaitKind)> {
    let map = lock_live_waits();
    let entry = map.get(repo_path)?;
    if entry.until <= now {
        return None;
    }
    Some((entry.until.saturating_duration_since(now), entry.kind))
}

/// Record what a live build that finished at `finished` after `elapsed`
/// means for the next one, and return the wait it set.
fn raise_live_wait(
    repo_path: &str,
    finished: Instant,
    elapsed: Duration,
    kind: WaitKind,
) -> Duration {
    let mut map = lock_live_waits();
    let previous = map.get(repo_path);
    let streak = match (kind, previous) {
        (WaitKind::Echo | WaitKind::Failure, Some(prev))
            if matches!(prev.kind, WaitKind::Echo | WaitKind::Failure) =>
        {
            prev.streak.saturating_add(1)
        }
        _ => 0,
    };
    let too_slow_streak = match (kind, previous) {
        (WaitKind::TooSlow, Some(prev)) if prev.kind == WaitKind::TooSlow => {
            prev.too_slow_streak.saturating_add(1)
        }
        _ => 0,
    };
    let wait = match kind {
        WaitKind::Rest => rest_after_build(elapsed),
        WaitKind::Echo | WaitKind::Failure => {
            rest_after_build(elapsed).max(Duration::from_secs(cooldown_secs_for_streak(streak)))
        }
        WaitKind::TooSlow => too_slow_rest(too_slow_streak),
    };
    map.insert(
        repo_path.to_string(),
        LiveWait {
            until: finished + wait,
            streak,
            too_slow_streak,
            kind,
        },
    );
    wait
}

/// Outcomes of builds [`start_refresh`] ran on a background thread, waiting to
/// be handed to the next ask for that repository. At most one per repository.
fn finished_builds() -> &'static Mutex<HashMap<String, LiveRefreshOutcome>> {
    static MAP: OnceLock<Mutex<HashMap<String, LiveRefreshOutcome>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_finished_builds() -> std::sync::MutexGuard<'static, HashMap<String, LiveRefreshOutcome>> {
    finished_builds()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The one live build allowed to run at a time, across every repository.
///
/// The frontend used to provide this by awaiting each build inside its single
/// queue slot. Once a build no longer holds the IPC call, nothing else would
/// stop every tab restored at launch from starting its own child at once.
fn live_build_slot() -> &'static Mutex<Option<String>> {
    static SLOT: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

struct LiveBuildSlot;

impl LiveBuildSlot {
    /// Claim the slot for `repo_path`, or name the repository holding it.
    fn claim(repo_path: &str) -> Result<Self, String> {
        let mut slot = live_build_slot()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(holder) = slot.as_ref() {
            return Err(holder.clone());
        }
        *slot = Some(repo_path.to_string());
        Ok(Self)
    }
}

impl Drop for LiveBuildSlot {
    fn drop(&mut self) {
        *live_build_slot()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

/// Facts for a probe the spawn gate refused to start: nothing was learned
/// about the store, so nothing is claimed.
const UNEXAMINED: LiveRefreshFacts = LiveRefreshFacts {
    available: false,
    is_fresh: false,
    schema_ok: false,
    already_building: false,
    rebuild_required: false,
    artifacts_missing: false,
    daemon_pending: None,
};

/// A request the spawn gate refused to start, carrying whatever the status
/// probe did establish ([`UNEXAMINED`] when the probe itself was refused) and
/// the gate's own sentence as the reason.
fn skip_deferred_outcome(facts: LiveRefreshFacts, reason: String) -> LiveRefreshOutcome {
    LiveRefreshOutcome {
        decision: LiveRefreshDecision::SkipDeferred,
        facts: facts.into(),
        build: None,
        reason: Some(reason),
        cooldown_remaining_ms: None,
        artifacts_restored: false,
        index_too_slow: false,
    }
}

/// What a build that could not be started means for the decision that wanted
/// it. Only the gate's own predicate classifies a deferral, so this cannot
/// drift from the message the gate writes.
fn decision_for_spawn_error(error: &str, wanted: LiveRefreshDecision) -> LiveRefreshDecision {
    if error.contains("already running") {
        // BuildGuard race: status said free, then another build won.
        LiveRefreshDecision::SkipBuilding
    } else if is_deferred_under_load(error) {
        LiveRefreshDecision::SkipDeferred
    } else {
        wanted
    }
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

/// The Map strip's sentence for a repository whose live build timed out.
fn too_slow_reason(remaining: Duration) -> String {
    format!(
        "Index too slow: the last automatic build ran past its {}-minute limit, so automatic \
         indexing is paused for {} min. Press Build to index now.",
        cli::BUILD_DEADLINE.as_secs() / 60,
        remaining.as_secs().div_ceil(60)
    )
}

fn skip_cooldown_outcome(remaining: Duration, kind: WaitKind) -> LiveRefreshOutcome {
    let ms = duration_ms(remaining);
    LiveRefreshOutcome {
        decision: LiveRefreshDecision::SkipCooldown,
        facts: LiveRefreshFactsDto {
            available: false,
            is_fresh: false,
            schema_ok: false,
            already_building: false,
            rebuild_required: false,
            artifacts_missing: false,
            daemon_pending: None,
        },
        build: None,
        reason: Some(match kind {
            WaitKind::Rest => format!(
                "live index resting after a build; edits since are folded into one build in {ms}ms"
            ),
            WaitKind::Echo | WaitKind::Failure => {
                format!("live index cooldown active; retry in {ms}ms (unchanged or failed builds)")
            }
            WaitKind::TooSlow => too_slow_reason(remaining),
        }),
        cooldown_remaining_ms: Some(ms),
        artifacts_restored: false,
        index_too_slow: kind == WaitKind::TooSlow,
    }
}

/// True when a finished build should raise the echo cooldown.
///
/// Failures (`ok: false`) and warm-path echoes (`unchanged: true`) both feed
/// the loop the cooldown exists to stop. A missing `unchanged` field is not
/// treated as an echo — only an explicit true is.
pub(crate) fn build_is_echo_or_failure(build: &BuildOutcome) -> bool {
    if !build.ok {
        return true;
    }
    build
        .report
        .as_ref()
        .and_then(|report| report.get("unchanged"))
        .and_then(Value::as_bool)
        == Some(true)
}

/// True when `devmap status --json` says only a full rebuild can fix the store.
///
/// `rebuild_required` is the kernel's own judgement and the field to prefer: it
/// covers an obsolete extraction payload *and* a migratable schema-behind
/// store. Other `degraded_reason` values, including source-tree drift, must not
/// force a full rebuild. The typed `rebuild_reason` and the degraded text stay
/// as fallbacks for binaries predating the field.
pub fn needs_manifest_rebuild(payload: Option<&Value>) -> bool {
    let Some(status) = payload else {
        return false;
    };
    if schema_rebuild_authorized(Some(status)) {
        return true;
    }
    if status.get("rebuild_reason").and_then(Value::as_str) == Some("payload-obsolete") {
        return true;
    }
    status
        .get("degraded_reason")
        .and_then(Value::as_str)
        .is_some_and(|reason| reason.contains(OBSOLETE_PAYLOAD_REASON))
}

/// Whether a rebuild may be run against a store whose schema is *outdated*.
///
/// Strictly the explicit `rebuild_required` boolean, and deliberately without
/// the text fallbacks [`needs_manifest_rebuild`] accepts. The kernel sets that
/// field only for a schema it can migrate, and leaves it false for `newer`,
/// `foreign` and `unsupported` stores; rebuilding a `newer` store would
/// downgrade a database a newer reader owns. A binary old enough not to emit
/// the field has told us nothing about migratability, and "the degraded text
/// mentions an obsolete payload" is not evidence about the schema — so an
/// outdated schema stays refused rather than rebuilt on an inference.
fn schema_rebuild_authorized(payload: Option<&Value>) -> bool {
    payload
        .and_then(|status| status.get("rebuild_required"))
        .and_then(Value::as_bool)
        == Some(true)
}

/// Which rebuild an authorized refresh must spawn.
///
/// `artifacts_missing` is decisive on its own. `devmap build` without
/// `--manifest` writes the database and nothing else, so the incremental path
/// can run to completion, report success, and leave Code → Map with no
/// document to read — including on the very first build of a newly opened
/// repository, where there is no artifact yet by definition.
pub fn live_rebuild_kind(payload: Option<&Value>, artifacts_missing: bool) -> LiveRebuildKind {
    if artifacts_missing || needs_manifest_rebuild(payload) {
        LiveRebuildKind::Manifest
    } else {
        LiveRebuildKind::Incremental
    }
}

/// Pure gate: stale → refresh, fresh → skip, in-flight → skip.
///
/// Two escapes from a skip exist because a skip here is *permanent* — nothing
/// else in the app rebuilds an index — and both states it used to strand are
/// states an ordinary user reaches without doing anything wrong:
///
/// * A schema-behind store answered `SkipSchemaOutdated` forever. Refusing an
///   *incremental* rebuild against the wrong shape is right; refusing the full
///   rebuild that migrates it is not. `rebuild_required` is the kernel's own
///   "a full build fixes this", so a store it does not set stays refused.
/// * A store with no consumer artifacts answered `SkipFresh` forever, because
///   store freshness says nothing about whether `repo_map.json` was ever
///   written.
pub fn decide_live_refresh(facts: LiveRefreshFacts) -> LiveRefreshDecision {
    if facts.already_building {
        return LiveRefreshDecision::SkipBuilding;
    }
    if !facts.available {
        return LiveRefreshDecision::SkipUnavailable;
    }
    if !facts.schema_ok {
        return if facts.rebuild_required {
            LiveRefreshDecision::Refresh
        } else {
            LiveRefreshDecision::SkipSchemaOutdated
        };
    }
    if facts.is_fresh && !facts.artifacts_missing {
        return LiveRefreshDecision::SkipFresh;
    }
    // The store is stale and something else is already fixing it. Ordered
    // after the two checks above on purpose: schema migration is ours, because
    // the daemon's status reply does not carry `schema_outdated` at all, and
    // the consumer artifacts are ours, because the daemon persists generations
    // to the store and never writes `repo_map.json`.
    if facts.artifacts_missing {
        return LiveRefreshDecision::Refresh;
    }
    if facts.daemon_pending.is_some_and(|pending| pending > 0) {
        return LiveRefreshDecision::SkipDaemon;
    }
    LiveRefreshDecision::Refresh
}

/// Outcome of a live-refresh attempt, including skips that never spawned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveRefreshOutcome {
    pub decision: LiveRefreshDecision,
    pub facts: LiveRefreshFactsDto,
    /// Present only when a build was started.
    pub build: Option<BuildOutcome>,
    pub reason: Option<String>,
    /// How long this repository's live index now waits before the next
    /// automatic build: the remaining wait when `decision` is
    /// [`SkipCooldown`], and the rest a finished build just set when it is
    /// [`Refresh`]. A watcher tick inside the wait is answered without a build,
    /// so the frontend holds its next ask until the wait ends and folds every
    /// edit in between into that one ask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_remaining_ms: Option<u64>,
    /// The last live build ran past its deadline and automatic indexing is
    /// paused for at least [`TOO_SLOW_REST`]. Set on that build's outcome and on
    /// every [`SkipCooldown`] answered during the pause, so the Map strip can
    /// say the index is too slow rather than "queued".
    #[serde(default)]
    pub index_too_slow: bool,
    /// This build wrote consumer artifacts that were absent before it ran.
    ///
    /// Needed because such a build reports `unchanged: true` whenever the
    /// store itself was already current — the artifacts are rewritten from the
    /// existing generation. That is a *store* echo and a *map* publication at
    /// the same time, and the frontend must not suppress the reload that makes
    /// the new document visible.
    #[serde(default)]
    pub artifacts_restored: bool,
}

/// Serde mirror of [`LiveRefreshFacts`] for the command boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveRefreshFactsDto {
    pub available: bool,
    pub is_fresh: bool,
    pub schema_ok: bool,
    pub already_building: bool,
    #[serde(default)]
    pub rebuild_required: bool,
    #[serde(default)]
    pub artifacts_missing: bool,
    #[serde(default)]
    pub daemon_pending: Option<u64>,
}

impl From<LiveRefreshFacts> for LiveRefreshFactsDto {
    fn from(facts: LiveRefreshFacts) -> Self {
        Self {
            available: facts.available,
            is_fresh: facts.is_fresh,
            schema_ok: facts.schema_ok,
            already_building: facts.already_building,
            rebuild_required: facts.rebuild_required,
            artifacts_missing: facts.artifacts_missing,
            daemon_pending: facts.daemon_pending,
        }
    }
}

/// Consumer artifacts the Code → Map pane reads, in the state directory this
/// repository actually resolves to.
///
/// Path resolution is delegated to `devmap_query::paths` — the CLI's canonical
/// owner — so a legacy `.devcouncil` tree is probed exactly where the CLI would
/// write it, rather than at a hard-coded `.devmap`.
pub fn missing_artifacts(repo_path: impl AsRef<std::path::Path>) -> Vec<&'static str> {
    let repo = repo_path.as_ref();
    let mut missing = Vec::new();
    if !super::repo_map::repo_map_path(repo).is_file() {
        missing.push("repo_map.json");
    }
    if !super::viz::code_graph_path(repo).is_file() {
        missing.push("code_graph.json");
    }
    missing
}

fn status_bool(status: &Value, key: &str) -> Option<bool> {
    status.get(key).and_then(|v| v.as_bool())
}

/// Read freshness + schema honesty out of `devmap status --json`.
pub fn freshness_from_cli_status(status: &CliStatus) -> (bool, bool, bool) {
    if !status.available {
        return (false, false, false);
    }
    let Some(payload) = status.status.as_ref() else {
        return (false, false, false);
    };
    let (Some(is_fresh), Some(schema_outdated)) = (
        status_bool(payload, "is_fresh"),
        status_bool(payload, "schema_outdated"),
    ) else {
        // Parseable JSON alone does not establish freshness or compatibility.
        return (false, false, false);
    };
    (true, is_fresh, !schema_outdated)
}

/// How [`live_refresh`] runs a build it has authorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiveBuildRun {
    /// On the calling thread: the outcome carries the build.
    Inline,
    /// On a background thread: the call answers
    /// [`LiveRefreshDecision::Started`] and the next ask for the repository
    /// collects the outcome.
    Detached,
}

/// Decide and optionally run a build after a repo change, waiting for it.
///
/// `repo_changed` is the watcher half of the stale signal: a settled write
/// means the working tree may diverge from the indexed hashes even when the
/// store still reports `is_fresh: true` (no daemon pending queue). Folded
/// into effective freshness before the gate runs.
///
/// Every finished live build sets a wait (see [`WaitKind`]). Watcher ticks
/// (`true`) inside any wait are answered [`LiveRefreshDecision::SkipCooldown`]
/// without probing anything. Activation (`false`) still peeks status: an
/// `unchanged` echo's wait or a productive build's rest blocks another
/// incremental rebuild, but a payload-obsolete index or a missing artifact
/// bypasses it so `--manifest` can restamp analyzer identity or restore the
/// map. A timed-out build's pause blocks activation too.
pub fn maybe_refresh(repo_path: &str, repo_changed: bool) -> LiveRefreshOutcome {
    live_refresh(
        repo_path,
        repo_changed,
        Instant::now(),
        LiveBuildRun::Inline,
    )
}

/// [`maybe_refresh`] for the IPC command: the same gate and the same waits,
/// but an authorized build runs on a background thread and this returns
/// [`LiveRefreshDecision::Started`] as soon as the child's writer slot is held.
///
/// The command used to hold a blocking-pool thread, and the frontend's IPC
/// call, for as long as the child ran — up to [`cli::BUILD_DEADLINE`]. What
/// still runs inline is the status probe, bounded by [`cli::STATUS_DEADLINE`].
pub fn start_refresh(repo_path: &str, repo_changed: bool) -> LiveRefreshOutcome {
    live_refresh(
        repo_path,
        repo_changed,
        Instant::now(),
        LiveBuildRun::Detached,
    )
}

pub(crate) fn live_refresh(
    repo_path: &str,
    repo_changed: bool,
    now: Instant,
    run: LiveBuildRun,
) -> LiveRefreshOutcome {
    // A build an earlier ask started has finished: its outcome is the answer,
    // whatever this ask was for. It is stored before the writer slot is
    // released, so no ask can find the repository idle with it still pending.
    let collected = lock_finished_builds().remove(repo_path);
    if let Some(mut finished) = collected {
        // What is left of the wait it set, not the whole of it: the outcome
        // may have waited for this ask.
        finished.cooldown_remaining_ms =
            live_wait(repo_path, now).map(|(remaining, _)| duration_ms(remaining));
        return finished;
    }
    // Held from the first question to the last answer: the probe and the build
    // are one operation per repository, so a concurrent caller is turned away
    // here rather than running a status child of its own and then, if it
    // probed before the first build finished, a second build. A path that does
    // not validate claims nothing; the probe below reports why.
    let (permit, already_building) = match cli::claim_build(repo_path) {
        Ok(permit) => (Some(permit), false),
        Err(cli::ClaimRefusal::Busy(_)) => (None, true),
        Err(cli::ClaimRefusal::Invalid(_)) => (None, false),
    };
    if already_building {
        // Busy retries must not launch status children against the active
        // writer. No status was examined, so do not claim available/fresh/ok.
        return LiveRefreshOutcome {
            decision: LiveRefreshDecision::SkipBuilding,
            facts: LiveRefreshFactsDto {
                available: false,
                is_fresh: false,
                schema_ok: false,
                already_building: true,
                rebuild_required: false,
                artifacts_missing: false,
                daemon_pending: None,
            },
            build: None,
            reason: Some("a devmap build is already running for this repo".into()),
            cooldown_remaining_ms: None,
            artifacts_restored: false,
            index_too_slow: false,
        };
    }

    let wait = live_wait(repo_path, now);
    if let Some((remaining, kind)) = wait {
        // A watcher tick inside any wait is folded into the build at its end;
        // a timed-out build's pause holds against activation as well.
        if repo_changed || kind == WaitKind::TooSlow {
            return skip_cooldown_outcome(remaining, kind);
        }
    }

    let cli = cli::status(repo_path);
    // Shed or deferred by the spawn gate: the probe never ran. Answering
    // `SkipUnavailable` here is what stranded every tab restored at launch.
    if let Some(reason) = cli
        .reason
        .as_deref()
        .filter(|reason| !cli.available && is_deferred_under_load(reason))
    {
        return skip_deferred_outcome(UNEXAMINED, reason.to_string());
    }
    let (available, status_fresh, schema_ok) = freshness_from_cli_status(&cli);
    let needs_manifest = needs_manifest_rebuild(cli.status.as_ref());
    // Filesystem-only, and only after `cli::status` — which validates the repo
    // and requires trust — has already answered for this path.
    let missing = if available {
        missing_artifacts(repo_path)
    } else {
        Vec::new()
    };
    let artifacts_missing = !missing.is_empty();
    if let Some((remaining, WaitKind::Echo)) = wait {
        // A build that only rewrote artifacts reports the store
        // `unchanged`, so it raises this wait every time. Letting the wait
        // then block the next activation is what made a missing
        // `repo_map.json` unrecoverable without a manual build.
        if !needs_manifest && !artifacts_missing {
            return skip_cooldown_outcome(remaining, WaitKind::Echo);
        }
    }
    // Watcher dirty forces stale; status alone can also be stale (daemon).
    // An obsolete extraction payload is a rebuild requirement even when the
    // CLI's `is_fresh` bit is inconsistent with `degraded_reason`.
    let is_fresh = status_fresh && !repo_changed && !needs_manifest;
    let mut facts = LiveRefreshFacts {
        available,
        is_fresh,
        schema_ok,
        already_building: false,
        rebuild_required: schema_rebuild_authorized(cli.status.as_ref()),
        artifacts_missing,
        daemon_pending: None,
    };
    let mut decision = decide_live_refresh(facts);
    // Ask about a daemon only when the answer can still change something. The
    // probe is a socket round trip rather than a spawn, but the overwhelmingly
    // common path through here is a fresh store, and that path must not pay for
    // a question whose answer it would discard. `None` stays the honest reading
    // when nothing answered: it is "we did not learn", not "no daemon".
    if decision == LiveRefreshDecision::Refresh && !artifacts_missing {
        facts.daemon_pending = super::serve::probe(repo_path)
            .ok()
            .and_then(|state| state.pending);
        decision = decide_live_refresh(facts);
    }
    if !decision.should_refresh() {
        return LiveRefreshOutcome {
            decision,
            facts: facts.into(),
            build: None,
            reason: Some(match decision {
                LiveRefreshDecision::SkipFresh => "index is fresh; skip refresh".into(),
                LiveRefreshDecision::SkipBuilding => {
                    "a devmap build is already running for this repo".into()
                }
                // Reached only when the kernel did not set `rebuild_required`,
                // i.e. a newer, foreign or unsupported store. `devmap build`
                // cannot fix any of those, so carry the CLI's own sentence —
                // which names the actual remedy, usually a newer binary —
                // instead of advice the user cannot act on.
                LiveRefreshDecision::SkipSchemaOutdated => cli
                    .status
                    .as_ref()
                    .and_then(|status| status.get("degraded_reason"))
                    .and_then(Value::as_str)
                    .map(|reason| format!("schema outdated; a rebuild cannot fix it: {reason}"))
                    .unwrap_or_else(|| "schema outdated and not migratable; refuse rebuild".into()),
                LiveRefreshDecision::SkipUnavailable => cli.reason.unwrap_or_else(|| {
                    "devmap status unavailable or missing boolean freshness/schema fields".into()
                }),
                // Names the queue depth, because that is the whole basis for
                // standing down: a daemon that is merely *alive* earns nothing
                // here, and the number is what a reader needs to tell "it is
                // on it" from "it has no idea".
                LiveRefreshDecision::SkipDaemon => format!(
                    "devmap serve has {} path(s) queued for this repository and is rebuilding them",
                    facts.daemon_pending.unwrap_or_default()
                ),
                LiveRefreshDecision::SkipCooldown => unreachable!(),
                LiveRefreshDecision::SkipDeferred => unreachable!(),
                LiveRefreshDecision::Started => unreachable!(),
                LiveRefreshDecision::Refresh => unreachable!(),
            }),
            cooldown_remaining_ms: None,
            artifacts_restored: false,
            index_too_slow: false,
        };
    }
    let rebuild = live_rebuild_kind(cli.status.as_ref(), artifacts_missing);
    // Activation inside a productive build's rest. Checked after the gate, so
    // a fresh store still answers `SkipFresh` and a missing artifact or an
    // obsolete payload still heals; what waits is an incremental rebuild of a
    // tree a build indexed moments ago, which is what an activation that
    // follows a watcher tick would otherwise start on every focus change.
    if let Some((remaining, WaitKind::Rest)) = wait {
        if rebuild == LiveRebuildKind::Incremental {
            return skip_cooldown_outcome(remaining, WaitKind::Rest);
        }
    }
    let slot = match LiveBuildSlot::claim(repo_path) {
        Ok(slot) => slot,
        Err(holder) => {
            return LiveRefreshOutcome {
                decision: LiveRefreshDecision::SkipBuilding,
                facts: facts.into(),
                build: None,
                reason: Some(format!(
                    "the live index is building {holder}; one automatic build runs at a time"
                )),
                cooldown_remaining_ms: None,
                artifacts_restored: false,
                index_too_slow: false,
            };
        }
    };
    let settle = LiveSettle {
        repo_path: repo_path.to_string(),
        decision,
        facts,
        artifacts_missing,
        started: now,
    };
    let manifest = rebuild == LiveRebuildKind::Manifest;
    let prepared = match permit.map_or_else(
        || cli::prepare(repo_path, manifest),
        |permit| permit.prepare(manifest),
    ) {
        Ok(prepared) => prepared,
        Err(error) => return settle.record(Err(error), Duration::ZERO),
    };
    let build_clock = Instant::now();
    match run {
        LiveBuildRun::Inline => {
            let outcome =
                prepared.run_then(|spawned| settle.record(spawned, build_clock.elapsed()));
            drop(slot);
            outcome
        }
        LiveBuildRun::Detached => {
            let thread_settle = settle.clone();
            let started = crate::supervisor::spawn_once(
                "devmap-live-build",
                "devmap-live-build",
                move || {
                    crate::engine::git_cli::with_background_processes(|| {
                        run_detached_build(slot, thread_settle, build_clock, |settler| {
                            prepared.run_then(settler)
                        })
                    });
                },
            );
            match started {
                Ok(_) => LiveRefreshOutcome {
                    decision: LiveRefreshDecision::Started,
                    facts: facts.into(),
                    build: None,
                    reason: Some(
                        "devmap build started in the background; the next ask collects its outcome"
                            .into(),
                    ),
                    cooldown_remaining_ms: None,
                    artifacts_restored: false,
                    index_too_slow: false,
                },
                // The closure, and with it the writer slot and the live slot,
                // was dropped with the failed spawn.
                Err(error) => settle.record(
                    Err(format!(
                        "could not start the live index build thread: {error}"
                    )),
                    Duration::ZERO,
                ),
            }
        }
    }
}

/// What a detached build hands the child's result to once it has run.
type BuildSettler = Box<dyn FnOnce(Result<BuildOutcome, String>)>;

/// The body of the detached build thread: `run` runs the child and gives its
/// result to the settler, which records the outcome for the next ask and then
/// frees the live slot.
///
/// A panic in `run` unwinds through the settler, so the slot is released by
/// [`LiveBuildSlot`]'s `Drop` either way. What an unwind alone would lose is the
/// outcome and the wait: the next ask would find the repository idle with
/// nothing pending and start the same build again on every tick. So the panic is
/// recorded as a failed build, which sets the failure wait, and then resumed for
/// `supervisor::spawn_once` to list the thread as stopped in Diagnostics.
fn run_detached_build(
    slot: LiveBuildSlot,
    settle: LiveSettle,
    build_clock: Instant,
    run: impl FnOnce(BuildSettler),
) {
    let panicked = settle.clone();
    let settler: BuildSettler = Box::new(move |spawned| {
        let outcome = settle.record(spawned, build_clock.elapsed());
        lock_finished_builds().insert(settle.repo_path.clone(), outcome);
        // Free for the next repository the moment the outcome is
        // collectable, not when this thread finishes unwinding.
        drop(slot);
    });
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| run(settler))) {
        let outcome = panicked.record(
            Err("the live index build thread panicked".into()),
            build_clock.elapsed(),
        );
        lock_finished_builds().insert(panicked.repo_path.clone(), outcome);
        resume_unwind(payload);
    }
}

/// What a live build was authorized as, carried to wherever it finishes.
#[derive(Debug, Clone)]
struct LiveSettle {
    repo_path: String,
    decision: LiveRefreshDecision,
    facts: LiveRefreshFacts,
    artifacts_missing: bool,
    /// When the ask that authorized the build was made (the governor's clock).
    started: Instant,
}

impl LiveSettle {
    /// Turn what the build did into its outcome, and set the wait it earns.
    ///
    /// Every build that ran sets one: a productive build's rest is no longer
    /// cleared, which is what let an agent editing continuously keep a build
    /// running back to back.
    fn record(
        &self,
        spawned: Result<BuildOutcome, String>,
        elapsed: Duration,
    ) -> LiveRefreshOutcome {
        let finished = self.started + elapsed;
        let build = match spawned {
            Ok(build) => build,
            Err(error) => {
                let decision = decision_for_spawn_error(&error, self.decision);
                if decision == LiveRefreshDecision::SkipDeferred {
                    // Status answered, then the build itself was shed. Not a
                    // failed build: no wait; keep what status did establish.
                    return skip_deferred_outcome(self.facts, error);
                }
                // A build that could not run at all retries on every tick
                // unless it waits like a failed one. A writer-slot race is
                // somebody else's build, not a failure of this one.
                let wait = (decision != LiveRefreshDecision::SkipBuilding).then(|| {
                    raise_live_wait(&self.repo_path, finished, elapsed, WaitKind::Failure)
                });
                return LiveRefreshOutcome {
                    decision,
                    facts: LiveRefreshFactsDto {
                        already_building: decision == LiveRefreshDecision::SkipBuilding,
                        ..self.facts.into()
                    },
                    build: None,
                    reason: Some(error),
                    cooldown_remaining_ms: wait.map(duration_ms),
                    artifacts_restored: false,
                    index_too_slow: false,
                };
            }
        };
        // Only a *successful* build can be credited with the artifacts, and
        // only ones that were actually absent beforehand count.
        let artifacts_restored =
            self.artifacts_missing && build.ok && missing_artifacts(&self.repo_path).is_empty();
        let kind = if build.timed_out {
            WaitKind::TooSlow
        } else if artifacts_restored {
            // Productive even when the store echoed `unchanged`.
            WaitKind::Rest
        } else if self.artifacts_missing || !build.ok {
            // A build that ran *because* artifacts were absent and left them
            // absent did not do the thing it was spawned to do, so it backs
            // off on the failure flavour rather than the benign-echo one — an
            // `unchanged: true` here describes the database, not the files
            // beside it that were the reason to run at all.
            WaitKind::Failure
        } else if build_is_echo_or_failure(&build) {
            WaitKind::Echo
        } else {
            WaitKind::Rest
        };
        let wait = raise_live_wait(&self.repo_path, finished, elapsed, kind);
        let failure = (!build.ok).then(|| {
            super::cli::failure_detail(&build.stdout, &build.stderr, "devmap refresh failed".into())
        });
        LiveRefreshOutcome {
            decision: self.decision,
            facts: self.facts.into(),
            reason: match (kind, failure) {
                (WaitKind::TooSlow, Some(detail)) => {
                    Some(format!("{} ({detail})", too_slow_reason(wait)))
                }
                (WaitKind::TooSlow, None) => Some(too_slow_reason(wait)),
                (_, failure) => failure,
            },
            build: Some(build),
            cooldown_remaining_ms: Some(duration_ms(wait)),
            artifacts_restored,
            index_too_slow: kind == WaitKind::TooSlow,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::procguard::LockedSpawn;
    use serde_json::json;

    /// A build the gate would not start is a deferral, not a failed refresh:
    /// left as `Refresh` with no build, the frontend reported "Refresh returned
    /// no build outcome" and settled the repository. Fed the gate's real
    /// messages, so a reworded refusal fails here rather than in the field.
    #[test]
    fn a_build_the_gate_declined_is_deferred_not_failed() {
        use crate::engine::git_cli::{refusal_message, Refusal};
        let wanted = LiveRefreshDecision::Refresh;
        for refusal in [
            Refusal::Shed,
            Refusal::Refused {
                waited: Duration::from_secs(2),
            },
        ] {
            assert_eq!(
                decision_for_spawn_error(refusal_message("devmap", refusal).message(), wanted),
                LiveRefreshDecision::SkipDeferred,
                "{refusal:?}"
            );
        }
        // Neither a slot that never came free inside the deadline nor a
        // cancellation is load-shedding; both keep the decision that ran.
        for refusal in [
            Refusal::TimedOut {
                deadline: Duration::from_secs(30),
            },
            Refusal::Cancelled,
        ] {
            assert_eq!(
                decision_for_spawn_error(refusal_message("devmap", refusal).message(), wanted),
                wanted,
                "{refusal:?}"
            );
        }
        assert_eq!(
            decision_for_spawn_error("a devmap build is already running for /repo", wanted),
            LiveRefreshDecision::SkipBuilding
        );
        assert_eq!(
            serde_json::to_value(LiveRefreshDecision::SkipDeferred).unwrap(),
            "skip_deferred"
        );
    }

    /// Give a fixture repository the consumer artifacts a real build writes.
    ///
    /// The fake `devmap` binaries below answer `build` without touching the
    /// filesystem, so without this every fixture looks like a repository whose
    /// map was never written — which is its own refresh trigger, and would
    /// silently change what the cooldown tests are measuring.
    #[cfg(unix)]
    fn write_stub_artifacts(root: &std::path::Path) {
        for path in [
            super::super::repo_map::repo_map_path(root),
            super::super::viz::code_graph_path(root),
        ] {
            std::fs::create_dir_all(path.parent().expect("artifact parent")).expect("artifact dir");
            std::fs::write(&path, "{}").expect("artifact");
        }
        assert!(missing_artifacts(root).is_empty());
    }

    /// A repository whose artifacts are present and whose schema, if outdated,
    /// is not one a rebuild can fix — the shape every pre-existing case here
    /// was written against.
    fn facts(
        available: bool,
        is_fresh: bool,
        schema_ok: bool,
        already_building: bool,
    ) -> LiveRefreshFacts {
        LiveRefreshFacts {
            available,
            is_fresh,
            schema_ok,
            already_building,
            rebuild_required: false,
            artifacts_missing: false,
            // No daemon answered. Every pre-existing case here predates the
            // stand-down and must keep deciding exactly as it did.
            daemon_pending: None,
        }
    }

    /// A stale store the daemon has already queued is left to the daemon.
    #[test]
    fn a_daemon_with_queued_work_stands_the_gate_down() {
        let mut queued = facts(true, false, true, false);
        queued.daemon_pending = Some(4);
        assert_eq!(
            decide_live_refresh(queued),
            LiveRefreshDecision::SkipDaemon,
            "rebuilding alongside the daemon only queues behind its writer lock"
        );
    }

    /// The disagreement case, and the reason the stand-down is not gated on
    /// mere liveness: the CLI says the store is stale, the daemon says it has
    /// nothing queued. The daemon's watcher did not see the change, so leaving
    /// it alone would strand the index with nobody rebuilding it.
    #[test]
    fn a_daemon_with_nothing_queued_does_not_excuse_a_stale_store() {
        let mut idle = facts(true, false, true, false);
        idle.daemon_pending = Some(0);
        assert_eq!(decide_live_refresh(idle), LiveRefreshDecision::Refresh);
    }

    /// Consumer artifacts are ours whatever the daemon is doing: it persists
    /// generations to the store and never writes `repo_map.json`.
    #[test]
    fn a_busy_daemon_does_not_stand_down_a_missing_artifact() {
        let mut missing = facts(true, true, true, false);
        missing.artifacts_missing = true;
        missing.daemon_pending = Some(9);
        assert_eq!(decide_live_refresh(missing), LiveRefreshDecision::Refresh);
    }

    /// Schema migration is ours too — the daemon's status reply does not carry
    /// `schema_outdated` at all, so it can never be evidence about it.
    #[test]
    fn a_busy_daemon_does_not_override_a_schema_refusal() {
        let mut outdated = facts(true, false, false, false);
        outdated.daemon_pending = Some(7);
        assert_eq!(
            decide_live_refresh(outdated),
            LiveRefreshDecision::SkipSchemaOutdated
        );
    }

    /// A migratable schema still migrates: `rebuild_required` outranks the
    /// daemon, which is not going to do a schema rebuild on our behalf.
    #[test]
    fn a_busy_daemon_does_not_delay_a_schema_migration() {
        let mut migratable = facts(true, false, false, false);
        migratable.rebuild_required = true;
        migratable.daemon_pending = Some(7);
        assert_eq!(
            decide_live_refresh(migratable),
            LiveRefreshDecision::Refresh
        );
    }

    /// Absence of an answer is not an answer. `None` means the probe did not
    /// happen or did not land, and must decide exactly as it did before
    /// daemons were considered at all.
    #[test]
    fn an_unasked_daemon_changes_nothing() {
        let unasked = facts(true, false, true, false);
        assert_eq!(unasked.daemon_pending, None);
        assert_eq!(decide_live_refresh(unasked), LiveRefreshDecision::Refresh);
    }

    #[test]
    fn stale_index_refreshes() {
        assert_eq!(
            decide_live_refresh(facts(true, false, true, false)),
            LiveRefreshDecision::Refresh
        );
    }

    #[test]
    fn fresh_index_skips() {
        assert_eq!(
            decide_live_refresh(facts(true, true, true, false)),
            LiveRefreshDecision::SkipFresh
        );
    }

    #[test]
    fn in_flight_build_skips() {
        assert_eq!(
            decide_live_refresh(facts(true, false, true, true)),
            LiveRefreshDecision::SkipBuilding
        );
        // In-flight wins over every other signal — do not start a second child.
        assert_eq!(
            decide_live_refresh(facts(true, true, true, true)),
            LiveRefreshDecision::SkipBuilding
        );
        assert_eq!(
            decide_live_refresh(facts(false, false, false, true)),
            LiveRefreshDecision::SkipBuilding
        );
    }

    #[test]
    fn schema_outdated_skips_even_when_stale() {
        assert_eq!(
            decide_live_refresh(facts(true, false, false, false)),
            LiveRefreshDecision::SkipSchemaOutdated
        );
    }

    #[test]
    fn migratable_schema_behind_rebuilds_instead_of_stranding() {
        // `rebuild_required` is the kernel saying a full build fixes this. The
        // old gate answered SkipSchemaOutdated unconditionally, and because
        // nothing else in the app rebuilds an index, that skip was permanent:
        // the repository never recovered without a manual Build click.
        let migratable = LiveRefreshFacts {
            rebuild_required: true,
            ..facts(true, false, false, false)
        };
        assert_eq!(
            decide_live_refresh(migratable),
            LiveRefreshDecision::Refresh
        );
        assert_eq!(
            live_rebuild_kind(Some(&json!({"rebuild_required": true})), false),
            LiveRebuildKind::Manifest,
            "a migratable store needs the full build, not an incremental one"
        );
    }

    #[test]
    fn a_store_a_rebuild_cannot_fix_is_still_refused() {
        // `newer`, `foreign` and `unsupported` stores all report
        // `schema_outdated: true` with `rebuild_required: false`. Rebuilding a
        // newer store would downgrade a database a newer reader owns.
        for payload in [
            json!({"schema_outdated": true, "rebuild_required": false, "schema_relation": "newer"}),
            json!({"schema_outdated": true, "rebuild_required": false, "schema_relation": "foreign"}),
            json!({"schema_outdated": true, "rebuild_required": false, "schema_relation": "unsupported"}),
        ] {
            assert!(
                !needs_manifest_rebuild(Some(&payload)),
                "{payload} must not authorize a rebuild"
            );
            let refused = LiveRefreshFacts {
                rebuild_required: needs_manifest_rebuild(Some(&payload)),
                ..facts(true, false, false, false)
            };
            assert_eq!(
                decide_live_refresh(refused),
                LiveRefreshDecision::SkipSchemaOutdated,
                "{payload}"
            );
        }
    }

    #[test]
    fn a_fresh_store_with_no_artifacts_still_refreshes() {
        // Measured against the real CLI: delete `repo_map.json` from a built
        // repository and `devmap status` still answers `is_fresh: true`,
        // because store freshness says nothing about whether the consumer
        // artifacts were ever written. The old gate skipped, permanently, and
        // Code → Map stayed empty.
        let no_artifacts = LiveRefreshFacts {
            artifacts_missing: true,
            ..facts(true, true, true, false)
        };
        assert_eq!(
            decide_live_refresh(no_artifacts),
            LiveRefreshDecision::Refresh
        );
    }

    #[test]
    fn a_missing_artifact_always_takes_the_manifest_path() {
        // The second half of the same defect: even when the gate did authorize
        // a refresh, `devmap build` without `--manifest` writes the database
        // and nothing else, so the artifact stayed missing however many times
        // the live path ran. This covers the cold first build of a newly
        // opened repository, where `status` reports no store at all.
        let cold = json!({
            "is_fresh": false,
            "schema_outdated": false,
            "rebuild_required": false,
            "schema_relation": "missing",
            "degraded_reason": "no devmap store at this path (run `devmap build`)",
        });
        assert_eq!(
            live_rebuild_kind(Some(&cold), true),
            LiveRebuildKind::Manifest
        );
        // A healthy repository with its artifacts on disk stays incremental.
        assert_eq!(
            live_rebuild_kind(Some(&json!({"is_fresh": false})), false),
            LiveRebuildKind::Incremental
        );
    }

    #[test]
    fn missing_artifacts_names_each_absent_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        assert_eq!(
            missing_artifacts(root),
            vec!["repo_map.json", "code_graph.json"],
            "a repository with no state directory is missing both"
        );

        // Written where the resolver says, not at a hard-coded `.devmap`.
        let map = super::super::repo_map::repo_map_path(root);
        let graph = super::super::viz::code_graph_path(root);
        std::fs::create_dir_all(map.parent().expect("parent")).expect("map dir");
        std::fs::write(&map, "{}").expect("write map");
        assert_eq!(missing_artifacts(root), vec!["code_graph.json"]);

        std::fs::create_dir_all(graph.parent().expect("parent")).expect("graph dir");
        std::fs::write(&graph, "{}").expect("write graph");
        assert!(missing_artifacts(root).is_empty());

        // A directory where a file belongs is not an artifact.
        std::fs::remove_file(&map).expect("remove map");
        std::fs::create_dir(&map).expect("map as dir");
        assert_eq!(missing_artifacts(root), vec!["repo_map.json"]);
    }

    #[test]
    fn unavailable_status_skips() {
        assert_eq!(
            decide_live_refresh(facts(false, false, true, false)),
            LiveRefreshDecision::SkipUnavailable
        );
    }

    #[test]
    fn freshness_from_cli_status_reads_payload() {
        let status = CliStatus {
            available: true,
            binary: Some("/bin/devmap".into()),
            lookup: None,
            reason: None,
            status: Some(serde_json::json!({
                "is_fresh": false,
                "schema_outdated": false,
                "generation_id": 3,
            })),
        };
        assert_eq!(freshness_from_cli_status(&status), (true, false, true));

        let outdated = CliStatus {
            available: true,
            binary: Some("/bin/devmap".into()),
            lookup: None,
            reason: None,
            status: Some(serde_json::json!({
                "is_fresh": false,
                "schema_outdated": true,
            })),
        };
        assert_eq!(freshness_from_cli_status(&outdated), (true, false, false));

        let missing = CliStatus {
            available: false,
            binary: None,
            lookup: None,
            reason: Some("no binary".into()),
            status: None,
        };
        assert_eq!(freshness_from_cli_status(&missing), (false, false, false));
    }

    #[test]
    fn watcher_dirty_overrides_status_fresh() {
        // Effective freshness is assembled by the caller the same way
        // `maybe_refresh` does: status fresh + repo_changed ⇒ not fresh.
        let status_fresh = true;
        let repo_changed = true;
        let is_fresh = status_fresh && !repo_changed;
        assert_eq!(
            decide_live_refresh(facts(true, is_fresh, true, false)),
            LiveRefreshDecision::Refresh
        );
    }

    #[test]
    fn malformed_status_never_authorizes_automatic_builds() {
        for payload in [
            serde_json::json!({}),
            serde_json::json!(null),
            serde_json::json!([]),
            serde_json::json!({"is_fresh": false}),
            serde_json::json!({"is_fresh": "false", "schema_outdated": false}),
            serde_json::json!({"is_fresh": false, "schema_outdated": "false"}),
        ] {
            let status = CliStatus {
                available: true,
                binary: None,
                lookup: None,
                reason: None,
                status: Some(payload.clone()),
            };
            let (available, is_fresh, schema_ok) = freshness_from_cli_status(&status);
            assert!(
                !decide_live_refresh(facts(available, is_fresh, schema_ok, false)).should_refresh(),
                "malformed status authorized a build: {payload}"
            );
        }
    }

    #[test]
    fn needs_manifest_rebuild_reads_rebuild_reason_and_degraded_text() {
        assert!(needs_manifest_rebuild(Some(&json!({
            "rebuild_reason": "payload-obsolete"
        }))));
        assert!(needs_manifest_rebuild(Some(&json!({
            "degraded_reason": "stored extraction payload is obsolete; rebuild with the current analyzer"
        }))));
        assert!(!needs_manifest_rebuild(Some(&json!({
            "rebuild_reason": "schema-behind"
        }))));
        assert!(!needs_manifest_rebuild(Some(&json!({
            "rebuild_reason": null
        }))));
        assert!(!needs_manifest_rebuild(None));
    }

    #[test]
    fn cooldown_secs_grow_exponentially_and_cap() {
        assert_eq!(cooldown_secs_for_streak(0), 1);
        assert_eq!(cooldown_secs_for_streak(1), 2);
        assert_eq!(cooldown_secs_for_streak(2), 4);
        assert_eq!(cooldown_secs_for_streak(5), 32);
        assert_eq!(cooldown_secs_for_streak(6), 60);
        assert_eq!(cooldown_secs_for_streak(20), 60);
    }

    #[test]
    fn build_is_echo_or_failure_reads_unchanged_and_ok() {
        let echo = BuildOutcome {
            ok: true,
            binary: "/bin/devmap".into(),
            lookup: cli::DevmapLookup::PathSearch,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            report: Some(json!({"ok": true, "unchanged": true})),
        };
        assert!(build_is_echo_or_failure(&echo));
        let changed = BuildOutcome {
            report: Some(json!({"ok": true, "unchanged": false})),
            ..echo.clone()
        };
        assert!(!build_is_echo_or_failure(&changed));
        let failed = BuildOutcome {
            ok: false,
            report: Some(json!({"ok": false})),
            ..echo
        };
        assert!(build_is_echo_or_failure(&failed));
    }

    #[test]
    #[cfg(unix)]
    fn unchanged_repo_changed_storm_is_bounded_by_cooldown() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let repo = tempfile::TempDir::new().unwrap();
        let output = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(repo.path())
            .output_locked()
            .expect("git init");
        assert!(output.status.success());
        crate::test_support::trust_repo(repo.path());
        write_stub_artifacts(repo.path());
        let bin = repo.path().join("devmap");
        std::fs::write(
            &bin,
            r#"#!/bin/sh
if [ "$1" = "build" ]; then
  printf '%s\n' '{"ok":true,"unchanged":true}'
  exit 0
fi
echo unexpected: "$*" >&2
exit 2
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        let bound = cli::bind_test_binary(bin.to_string_lossy().into_owned());
        // The storm below calls `maybe_refresh` twenty-odd times, and every call
        // used to spawn a child just to be told the store is fresh. Each of those
        // was a chance for the probe to miss its deadline and answer
        // `SkipUnavailable`, which this loop reads as an unexpected decision and
        // panics on — a verdict about how busy the host was. The stub no longer
        // answers `status` at all, so a lost override fails loudly here instead
        // of quietly standing in for the override.
        bound.set_status(cli::available_status(
            r#"{"is_fresh":true,"schema_outdated":false}"#,
        ));
        struct ResetCooldowns;
        impl Drop for ResetCooldowns {
            fn drop(&mut self) {
                clear_all_live_echo_cooldowns();
            }
        }
        let _reset = ResetCooldowns;

        let path = repo.path().to_string_lossy().into_owned();
        let mut builds = 0u32;
        let mut cooldowns = 0u32;
        // A counted storm, not a wall-clock one: under llvm-cov a single
        // maybe_refresh can take longer than the old 3s window, so the loop
        // would finish with zero cooldown hits and look like the bound failed.
        for _ in 0..20 {
            let outcome = maybe_refresh(&path, true);
            match outcome.decision {
                LiveRefreshDecision::Refresh => builds += 1,
                LiveRefreshDecision::SkipCooldown => cooldowns += 1,
                other => panic!("unexpected decision during echo storm: {other:?}"),
            }
        }
        assert!(
            builds <= 4,
            "echo storm spawned {builds} builds (cooldown must bound them)"
        );
        assert!(
            cooldowns >= 10,
            "echo storm should mostly hit SkipCooldown, got {cooldowns}"
        );

        // Activation during an unchanged-echo wait must not spawn another
        // incremental rebuild: the last build already proved the tree current.
        clear_all_live_echo_cooldowns();
        let _ = maybe_refresh(&path, true); // raise cooldown again
        assert_eq!(
            maybe_refresh(&path, true).decision,
            LiveRefreshDecision::SkipCooldown
        );
        let activation = maybe_refresh(&path, false);
        assert_eq!(
            activation.decision,
            LiveRefreshDecision::SkipCooldown,
            "unchanged-echo cooldown blocks activation incremental rebuilds"
        );
        assert_eq!(
            maybe_refresh(&path, true).decision,
            LiveRefreshDecision::SkipCooldown,
            "activation must not reset watcher-echo cooldown"
        );
    }

    /// A trusted fixture repository with stub artifacts and a stub `devmap`
    /// whose body is `script`; the binding is returned so the caller can state
    /// the status the probe answers.
    #[cfg(unix)]
    fn stub_repository(script: &str) -> (tempfile::TempDir, cli::TestBinaryBinding) {
        use std::os::unix::fs::PermissionsExt;
        let repo = tempfile::TempDir::new().unwrap();
        let output = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(repo.path())
            .output_locked()
            .expect("git init");
        assert!(output.status.success());
        crate::test_support::trust_repo(repo.path());
        write_stub_artifacts(repo.path());
        let bin = repo.path().join("devmap");
        std::fs::write(&bin, script).unwrap();
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        let bound = cli::bind_test_binary(bin.to_string_lossy().into_owned());
        (repo, bound)
    }

    /// Lets a stub build that waits for a `release` file finish, and waits
    /// (bounded) for the live slot it holds to come free. A failed assertion
    /// must not leave a background build holding the slot for every test that
    /// runs after it.
    #[cfg(unix)]
    struct ReleaseStub(std::path::PathBuf);

    #[cfg(unix)]
    impl Drop for ReleaseStub {
        fn drop(&mut self) {
            let _ = std::fs::write(self.0.join("release"), "");
            let deadline = Instant::now() + Duration::from_secs(35);
            while live_build_slot()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some()
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    /// The duty cycle under an agent edit storm: one settled write per second
    /// for ten minutes, every build productive (`unchanged: false`) and the
    /// store reporting stale throughout, because the agent keeps editing.
    ///
    /// A productive build used to *clear* the cooldown, so every tick built:
    /// 600 builds in ten minutes, back to back. The clock is injected so the
    /// ten minutes cost what twenty stub builds cost, not ten minutes.
    #[test]
    #[cfg(unix)]
    fn productive_edit_storm_builds_at_most_once_per_rest_window() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        struct ResetCooldowns;
        impl Drop for ResetCooldowns {
            fn drop(&mut self) {
                clear_all_live_echo_cooldowns();
            }
        }
        let _reset = ResetCooldowns;
        let (repo, bound) = stub_repository(
            r#"#!/bin/sh
if [ "$1" = "build" ]; then
  printf '%s\n' '{"ok":true,"unchanged":false}'
  exit 0
fi
echo unexpected: "$*" >&2
exit 2
"#,
        );
        bound.set_status(cli::available_status(
            r#"{"is_fresh":false,"schema_outdated":false}"#,
        ));
        let path = repo.path().to_string_lossy().into_owned();
        let start = Instant::now();
        let mut built_at: Vec<u64> = Vec::new();
        for second in 0..600u64 {
            let outcome = live_refresh(
                &path,
                true,
                start + Duration::from_secs(second),
                LiveBuildRun::Inline,
            );
            match outcome.decision {
                LiveRefreshDecision::Refresh => {
                    assert!(
                        outcome.build.as_ref().is_some_and(|build| build.ok),
                        "{outcome:?}"
                    );
                    built_at.push(second);
                }
                LiveRefreshDecision::SkipCooldown => {}
                other => panic!("unexpected decision during the edit storm: {other:?}"),
            }
        }
        // `max(30 s, 2 x build)`: the stub builds in milliseconds, so the
        // floor governs. Every gap is measured, not only the count.
        for pair in built_at.windows(2) {
            assert!(
                pair[1] - pair[0] >= 30,
                "two builds {}s apart inside one rest window: {built_at:?}",
                pair[1] - pair[0]
            );
        }
        assert!(
            built_at.len() <= 20,
            "a 10-minute storm spawned {} builds: {built_at:?}",
            built_at.len()
        );
        assert!(
            !built_at.is_empty(),
            "the storm must still be indexed, not starved"
        );
    }

    #[test]
    fn rest_is_twice_the_build_floored_at_30s_and_capped_at_10min() {
        assert_eq!(rest_after_build(Duration::ZERO), LIVE_REST_FLOOR);
        assert_eq!(rest_after_build(Duration::from_secs(14)), LIVE_REST_FLOOR);
        assert_eq!(
            rest_after_build(Duration::from_secs(53)),
            Duration::from_secs(106),
            "the audit's average build earns twice its own length"
        );
        assert_eq!(
            rest_after_build(Duration::from_secs(821)),
            LIVE_REST_CAP,
            "the audit's slowest build is capped"
        );
        assert_eq!(rest_after_build(Duration::MAX), LIVE_REST_CAP);
    }

    #[test]
    fn a_timed_out_build_pauses_at_least_ten_minutes_and_doubles_to_an_hour() {
        assert_eq!(too_slow_rest(0), Duration::from_secs(10 * 60));
        assert_eq!(too_slow_rest(1), Duration::from_secs(20 * 60));
        assert_eq!(too_slow_rest(2), Duration::from_secs(40 * 60));
        assert_eq!(too_slow_rest(3), TOO_SLOW_REST_CAP);
        assert_eq!(too_slow_rest(u32::MAX), TOO_SLOW_REST_CAP);
    }

    /// A timed-out build used to get the 1-60 s failure cadence and then run
    /// again — a 15-minute child, back to back. Now nothing automatic runs for
    /// ten minutes, activation included, and the outcome says why in words
    /// the Map strip shows.
    #[test]
    #[cfg(unix)]
    fn a_timed_out_build_pauses_automatic_indexing_and_says_the_index_is_too_slow() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        struct ResetCooldowns;
        impl Drop for ResetCooldowns {
            fn drop(&mut self) {
                clear_all_live_echo_cooldowns();
            }
        }
        let _reset = ResetCooldowns;
        let (repo, bound) = stub_repository(
            r#"#!/bin/sh
if [ "$1" = "build" ]; then
  printf '%s\n' '{"ok":true,"unchanged":false}'
  exit 0
fi
echo unexpected: "$*" >&2
exit 2
"#,
        );
        bound.set_status(cli::available_status(
            r#"{"is_fresh":false,"schema_outdated":false}"#,
        ));
        let path = repo.path().to_string_lossy().into_owned();
        let start = Instant::now();
        let settle = LiveSettle {
            repo_path: path.clone(),
            decision: LiveRefreshDecision::Refresh,
            facts: facts(true, false, true, false),
            artifacts_missing: false,
            started: start,
        };
        // The outcome `PreparedBuild::run_then` hands over when the child
        // outlives `BUILD_DEADLINE`; the deadline itself is not waited out.
        let timed_out = BuildOutcome {
            ok: false,
            binary: "/bin/devmap".into(),
            lookup: cli::DevmapLookup::PathSearch,
            exit_code: None,
            stdout: String::new(),
            stderr: "devmap timed out after 900s".into(),
            timed_out: true,
            report: None,
        };
        let outcome = settle.record(Ok(timed_out.clone()), cli::BUILD_DEADLINE);
        assert!(outcome.index_too_slow, "{outcome:?}");
        assert_eq!(outcome.cooldown_remaining_ms, Some(10 * 60 * 1_000));
        let reason = outcome.reason.clone().unwrap_or_default();
        assert!(reason.starts_with("Index too slow"), "{reason}");
        assert!(reason.contains("timed out after 900s"), "{reason}");

        let finished = start + cli::BUILD_DEADLINE;
        for (after, repo_changed) in [(1u64, true), (1, false), (599, true), (599, false)] {
            let answer = live_refresh(
                &path,
                repo_changed,
                finished + Duration::from_secs(after),
                LiveBuildRun::Inline,
            );
            assert_eq!(
                answer.decision,
                LiveRefreshDecision::SkipCooldown,
                "{after}s after a timeout, repo_changed={repo_changed}"
            );
            assert!(answer.index_too_slow, "{answer:?}");
            assert!(answer.build.is_none());
        }
        // Ten minutes on, the next attempt is allowed.
        let retry = live_refresh(
            &path,
            false,
            finished + Duration::from_secs(601),
            LiveBuildRun::Inline,
        );
        assert_eq!(retry.decision, LiveRefreshDecision::Refresh, "{retry:?}");
        assert!(!retry.index_too_slow);
        // That retry built productively, so the timeout streak is over: the
        // next timeout starts again at ten minutes.
        let after_success = settle.record(Ok(timed_out.clone()), cli::BUILD_DEADLINE);
        assert_eq!(after_success.cooldown_remaining_ms, Some(10 * 60 * 1_000));
        // A second *consecutive* timeout doubles the pause.
        let again = settle.record(Ok(timed_out), cli::BUILD_DEADLINE);
        assert_eq!(again.cooldown_remaining_ms, Some(20 * 60 * 1_000));

        // The user pressing Build outranks the pause.
        clear_live_echo_cooldown(&path);
        assert_eq!(live_wait(&path, finished).map(|(_, kind)| kind), None);
    }

    /// A build that could not run at all — here a binary whose stdout is not
    /// JSON — used to answer `Refresh` with no wait, so every watcher tick
    /// spawned it again.
    #[test]
    #[cfg(unix)]
    fn a_build_that_cannot_run_waits_like_a_failed_one() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        struct ResetCooldowns;
        impl Drop for ResetCooldowns {
            fn drop(&mut self) {
                clear_all_live_echo_cooldowns();
            }
        }
        let _reset = ResetCooldowns;
        let (repo, bound) = stub_repository(
            r#"#!/bin/sh
if [ "$1" = "build" ]; then
  echo not json
  exit 0
fi
exit 2
"#,
        );
        bound.set_status(cli::available_status(
            r#"{"is_fresh":false,"schema_outdated":false}"#,
        ));
        let path = repo.path().to_string_lossy().into_owned();
        let first = maybe_refresh(&path, true);
        assert_eq!(first.decision, LiveRefreshDecision::Refresh);
        assert!(first.build.is_none(), "{first:?}");
        assert!(first.cooldown_remaining_ms.is_some_and(|ms| ms > 0));
        assert_eq!(
            maybe_refresh(&path, true).decision,
            LiveRefreshDecision::SkipCooldown
        );
    }

    /// The command never waits on the child: it answers `started` once the
    /// writer slot is held, every ask while the child runs is a probe-free
    /// `skip_building`, and the next ask after it exits collects the outcome
    /// together with the rest it set.
    #[test]
    #[cfg(unix)]
    fn start_refresh_answers_before_the_build_and_the_next_ask_collects_it() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        struct ResetCooldowns;
        impl Drop for ResetCooldowns {
            fn drop(&mut self) {
                clear_all_live_echo_cooldowns();
            }
        }
        let _reset = ResetCooldowns;
        let (repo, bound) = stub_repository(
            r#"#!/bin/sh
if [ "$1" = "build" ]; then
  i=0; while [ ! -f release ] && [ $i -lt 600 ]; do sleep 0.05; i=$((i+1)); done
  printf '%s\n' '{"ok":true,"unchanged":false}'
  exit 0
fi
exit 2
"#,
        );
        bound.set_status(cli::available_status(
            r#"{"is_fresh":false,"schema_outdated":false}"#,
        ));
        let path = repo.path().to_string_lossy().into_owned();
        let _release = ReleaseStub(repo.path().to_path_buf());

        let asked = Instant::now();
        let started = start_refresh(&path, true);
        assert_eq!(
            started.decision,
            LiveRefreshDecision::Started,
            "{started:?}"
        );
        assert!(started.build.is_none());
        assert!(
            asked.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(2)),
            "the command waited {:?} on a build that has not finished",
            asked.elapsed()
        );
        assert!(
            cli::is_build_in_flight(&path),
            "the writer slot is held on return"
        );

        let busy = start_refresh(&path, true);
        assert_eq!(busy.decision, LiveRefreshDecision::SkipBuilding);

        std::fs::write(repo.path().join("release"), "").unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let collected = loop {
            let answer = start_refresh(&path, true);
            if answer.decision != LiveRefreshDecision::SkipBuilding {
                break answer;
            }
            assert!(Instant::now() < deadline, "the build never finished");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(
            collected.decision,
            LiveRefreshDecision::Refresh,
            "{collected:?}"
        );
        assert!(collected.build.as_ref().is_some_and(|build| build.ok));
        // What is left of the 30 s floor the build set, measured at collection.
        assert!(
            collected
                .cooldown_remaining_ms
                .is_some_and(|ms| ms > 20_000 && ms <= duration_ms(LIVE_REST_FLOOR)),
            "{collected:?}"
        );
        // Collected once; the rest it set now answers the next tick.
        let next = start_refresh(&path, true);
        assert_eq!(next.decision, LiveRefreshDecision::SkipCooldown, "{next:?}");
    }

    /// A detached build whose thread panics must not hold the one live slot
    /// for the rest of the process, and must not be forgotten: the outcome is a
    /// failed build with its wait (so the next tick does not start the same
    /// build again), and Diagnostics lists the thread as stopped.
    #[test]
    #[cfg(unix)]
    fn a_panicking_detached_build_frees_the_slot_and_records_a_failure() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                clear_all_live_echo_cooldowns();
                crate::supervisor::note_recovered("devmap-live-build");
            }
        }
        let _reset = Reset;
        let path = "/gitpulse-test/panicking-detached-build".to_string();
        let slot = LiveBuildSlot::claim(&path).expect("the slot is free");
        assert!(
            LiveBuildSlot::claim("/gitpulse-test/other").is_err(),
            "the slot is held while the build runs"
        );
        let settle = LiveSettle {
            repo_path: path.clone(),
            decision: LiveRefreshDecision::Refresh,
            facts: facts(true, false, true, false),
            artifacts_missing: false,
            started: Instant::now(),
        };
        let started = Instant::now();
        let handle =
            crate::supervisor::spawn_once("devmap-live-build", "devmap-live-build", move || {
                run_detached_build(slot, settle, started, |_settler| {
                    panic!("injected build panic")
                })
            })
            .expect("thread starts");
        handle.join().expect("spawn_once records the panic itself");

        drop(LiveBuildSlot::claim("/gitpulse-test/other").expect("the panic freed the slot"));
        let outcome = lock_finished_builds()
            .remove(&path)
            .expect("a panicked build leaves an outcome for the next ask");
        assert_eq!(
            outcome.decision,
            LiveRefreshDecision::Refresh,
            "{outcome:?}"
        );
        assert!(outcome.build.is_none(), "{outcome:?}");
        assert!(
            outcome
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("panicked")),
            "{outcome:?}"
        );
        assert!(
            outcome.cooldown_remaining_ms.is_some_and(|ms| ms > 0),
            "a panicked build must wait like a failed one: {outcome:?}"
        );
        assert!(
            live_wait(&path, Instant::now()).is_some(),
            "the failure wait is recorded"
        );
        let listed = crate::supervisor::degraded()
            .into_iter()
            .find(|d| d.subsystem == "devmap-live-build")
            .expect("Diagnostics lists the stopped thread");
        assert!(listed.stopped, "{listed:?}");
        assert!(listed.reason.contains("injected build panic"), "{listed:?}");
    }

    /// A detached build that settles normally records its outcome and frees
    /// the slot exactly as before.
    #[test]
    #[cfg(unix)]
    fn a_settled_detached_build_records_its_outcome_and_frees_the_slot() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                clear_all_live_echo_cooldowns();
            }
        }
        let _reset = Reset;
        let path = "/gitpulse-test/settled-detached-build".to_string();
        let slot = LiveBuildSlot::claim(&path).expect("the slot is free");
        let settle = LiveSettle {
            repo_path: path.clone(),
            decision: LiveRefreshDecision::Refresh,
            facts: facts(true, false, true, false),
            artifacts_missing: false,
            started: Instant::now(),
        };
        run_detached_build(slot, settle, Instant::now(), |settler| {
            settler(Err("devmap could not run".into()))
        });
        drop(LiveBuildSlot::claim("/gitpulse-test/other").expect("the settler freed the slot"));
        let outcome = lock_finished_builds()
            .remove(&path)
            .expect("outcome stored");
        assert_eq!(outcome.reason.as_deref(), Some("devmap could not run"));
    }

    /// One automatic build at a time across every repository: the frontend
    /// used to provide this by awaiting each build in its single queue slot.
    #[test]
    #[cfg(unix)]
    fn one_live_build_runs_at_a_time_across_repositories() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        struct ResetCooldowns;
        impl Drop for ResetCooldowns {
            fn drop(&mut self) {
                clear_all_live_echo_cooldowns();
            }
        }
        let _reset = ResetCooldowns;
        let script = r#"#!/bin/sh
printf '%s\n' "$*" >> argv.log
if [ "$1" = "build" ]; then
  i=0; while [ ! -f release ] && [ $i -lt 600 ]; do sleep 0.05; i=$((i+1)); done
  printf '%s\n' '{"ok":true,"unchanged":false}'
  exit 0
fi
exit 2
"#;
        let (first, bound) = stub_repository(script);
        let _release_first = ReleaseStub(first.path().to_path_buf());
        bound.set_status(cli::available_status(
            r#"{"is_fresh":false,"schema_outdated":false}"#,
        ));
        // The second repository is served by the same bound stub binary.
        let second = tempfile::TempDir::new().unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(second.path())
            .output_locked()
            .expect("git init")
            .status
            .success());
        crate::test_support::trust_repo(second.path());
        write_stub_artifacts(second.path());
        let first_path = first.path().to_string_lossy().into_owned();
        let second_path = second.path().to_string_lossy().into_owned();

        assert_eq!(
            start_refresh(&first_path, true).decision,
            LiveRefreshDecision::Started
        );
        let refused = start_refresh(&second_path, true);
        assert_eq!(
            refused.decision,
            LiveRefreshDecision::SkipBuilding,
            "{refused:?}"
        );
        assert!(
            refused
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("one automatic build runs at a time")),
            "{refused:?}"
        );
        // The daemon probe may ask the stub for paths; a build it may not run.
        let second_log =
            std::fs::read_to_string(second.path().join("argv.log")).unwrap_or_default();
        assert!(
            !second_log.lines().any(|line| line.starts_with("build")),
            "the second repository built while the first was building:\n{second_log}"
        );

        std::fs::write(first.path().join("release"), "").unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        while start_refresh(&first_path, true).decision == LiveRefreshDecision::SkipBuilding {
            assert!(Instant::now() < deadline, "the first build never finished");
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::write(second.path().join("release"), "").unwrap();
        let after = maybe_refresh(&second_path, true);
        assert_eq!(after.decision, LiveRefreshDecision::Refresh, "{after:?}");
    }

    #[test]
    #[cfg(unix)]
    fn failed_build_storm_enters_the_same_cooldown() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let repo = tempfile::TempDir::new().unwrap();
        let output = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(repo.path())
            .output_locked()
            .expect("git init");
        assert!(output.status.success());
        crate::test_support::trust_repo(repo.path());
        write_stub_artifacts(repo.path());
        let bin = repo.path().join("devmap");
        std::fs::write(
            &bin,
            r#"#!/bin/sh
if [ "$1" = "build" ]; then
  echo boom >&2
  printf '%s\n' '{"ok":false}'
  exit 1
fi
echo unexpected: "$*" >&2
exit 2
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        let bound = cli::bind_test_binary(bin.to_string_lossy().into_owned());
        // Injected for the same reason as the storm above: this test is about
        // what a *failed build* does to the cooldown, so the probe answering
        // late must not be able to decide it.
        bound.set_status(cli::available_status(
            r#"{"is_fresh":false,"schema_outdated":false}"#,
        ));
        struct ResetCooldowns;
        impl Drop for ResetCooldowns {
            fn drop(&mut self) {
                clear_all_live_echo_cooldowns();
            }
        }
        let _reset = ResetCooldowns;
        let path = repo.path().to_string_lossy().into_owned();
        let first = maybe_refresh(&path, true);
        assert_eq!(first.decision, LiveRefreshDecision::Refresh);
        assert_eq!(first.build.as_ref().map(|b| b.ok), Some(false));
        let second = maybe_refresh(&path, true);
        assert_eq!(second.decision, LiveRefreshDecision::SkipCooldown);
        assert!(second.cooldown_remaining_ms.unwrap_or(0) > 0);
        let activation = maybe_refresh(&path, false);
        assert_eq!(
            activation.decision,
            LiveRefreshDecision::Refresh,
            "failed-build cooldown must still allow an activation retry"
        );
    }

    #[test]
    #[cfg(unix)]
    fn unchanged_echo_cooldown_blocks_activation_when_status_is_stale() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let repo = tempfile::TempDir::new().unwrap();
        let output = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(repo.path())
            .output_locked()
            .expect("git init");
        assert!(output.status.success());
        crate::test_support::trust_repo(repo.path());
        write_stub_artifacts(repo.path());
        let bin = repo.path().join("devmap");
        std::fs::write(
            &bin,
            r#"#!/bin/sh
if [ "$1" = "build" ]; then
  printf '%s\n' '{"ok":true,"unchanged":true}'
  exit 0
fi
echo unexpected: "$*" >&2
exit 2
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        let bound = cli::bind_test_binary(bin.to_string_lossy().into_owned());
        // "Stale status" is the premise of this test, so it is stated rather
        // than spawned for: a probe that missed its deadline would make the
        // premise `SkipUnavailable` instead, and the activation assertion below
        // would be about the wrong thing.
        bound.set_status(cli::available_status(
            r#"{"is_fresh":false,"schema_outdated":false}"#,
        ));
        struct ResetCooldowns;
        impl Drop for ResetCooldowns {
            fn drop(&mut self) {
                clear_all_live_echo_cooldowns();
            }
        }
        let _reset = ResetCooldowns;
        let path = repo.path().to_string_lossy().into_owned();
        let first = maybe_refresh(&path, true);
        assert_eq!(first.decision, LiveRefreshDecision::Refresh);
        let activation = maybe_refresh(&path, false);
        assert_eq!(
            activation.decision,
            LiveRefreshDecision::SkipCooldown,
            "stale status after an unchanged echo must not restart incremental rebuilds"
        );
        assert!(activation.build.is_none(), "{activation:?}");
    }

    #[test]
    #[cfg(unix)]
    fn unchanged_echo_cooldown_still_rebuilds_obsolete_payload_on_activation() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let repo = tempfile::TempDir::new().unwrap();
        let output = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(repo.path())
            .output_locked()
            .expect("git init");
        assert!(output.status.success());
        crate::test_support::trust_repo(repo.path());
        write_stub_artifacts(repo.path());
        let bin = repo.path().join("devmap");
        std::fs::write(
            &bin,
            r#"#!/bin/sh
printf '%s\n' "$*" >> argv.log
if [ "$1" = "build" ]; then
  printf '%s\n' '{"ok":true,"unchanged":true}'
  exit 0
fi
echo unexpected: "$*" >&2
exit 2
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        let bound = cli::bind_test_binary(bin.to_string_lossy().into_owned());
        struct ResetCooldowns;
        impl Drop for ResetCooldowns {
            fn drop(&mut self) {
                clear_all_live_echo_cooldowns();
            }
        }
        let _reset = ResetCooldowns;
        let path = repo.path().to_string_lossy().into_owned();
        // The two payloads the stub used to serve from a file it re-read on every
        // spawn. Stated directly, so what changes between the two calls is the
        // payload and nothing else.
        bound.set_status(cli::available_status(
            r#"{"is_fresh":false,"schema_outdated":false}"#,
        ));
        let first = maybe_refresh(&path, true);
        assert_eq!(first.decision, LiveRefreshDecision::Refresh);
        bound.set_status(cli::available_status(
            r#"{"is_fresh":false,"schema_outdated":false,"rebuild_reason":"payload-obsolete"}"#,
        ));
        let activation = maybe_refresh(&path, false);
        assert_eq!(
            activation.decision,
            LiveRefreshDecision::Refresh,
            "payload-obsolete must bypass an unchanged-echo wait"
        );
        let log = std::fs::read_to_string(repo.path().join("argv.log")).unwrap_or_default();
        assert!(
            log.lines().any(|line| {
                let tokens: Vec<&str> = line.split_whitespace().collect();
                tokens.first() == Some(&"build") && tokens.contains(&"--manifest")
            }),
            "activation must restamp with --manifest, argv log:\n{log}"
        );
    }

    /// The whole cold-start path, against a stub that reproduces the two
    /// behaviours of the real CLI this defect turned on: a plain `build` writes
    /// the database only, and `build --manifest` writes the artifacts even when
    /// it reports the store `unchanged`.
    ///
    /// Before the fix this repository could never publish a map: the first
    /// build was incremental (no artifacts), and every later attempt either
    /// skipped as fresh or echoed into a cooldown.
    #[test]
    #[cfg(unix)]
    fn a_newly_opened_repository_publishes_its_map_without_a_manual_build() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let repo = tempfile::TempDir::new().unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(repo.path())
            .output_locked()
            .expect("git init")
            .status
            .success());
        crate::test_support::trust_repo(repo.path());
        let map = super::super::repo_map::repo_map_path(repo.path());
        let graph = super::super::viz::code_graph_path(repo.path());
        // Deliberately NOT calling `write_stub_artifacts`: this is the cold
        // repository a user has just opened for the first time.
        assert_eq!(
            missing_artifacts(repo.path()),
            vec!["repo_map.json", "code_graph.json"]
        );

        let bin = repo.path().join("devmap");
        std::fs::write(
            &bin,
            format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> argv.log
if [ "$1" = "build" ]; then
  # A plain build writes the store and nothing else, exactly like the real
  # kernel; only --manifest writes the consumer artifacts.
  unchanged=false
  if [ -f built ]; then unchanged=true; fi
  : > built
  for arg in "$@"; do
    if [ "$arg" = "--manifest" ]; then
      mkdir -p '{map_dir}' '{graph_dir}'
      printf '%s\n' '{{}}' > '{map}'
      printf '%s\n' '{{}}' > '{graph}'
    fi
  done
  printf '%s\n' "{{\"ok\":true,\"unchanged\":$unchanged}}"
  exit 0
fi
echo unexpected: "$*" >&2
exit 2
"#,
                map = map.display(),
                graph = graph.display(),
                map_dir = map.parent().expect("map parent").display(),
                graph_dir = graph.parent().expect("graph parent").display(),
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        let bound = cli::bind_test_binary(bin.to_string_lossy().into_owned());
        struct ResetCooldowns;
        impl Drop for ResetCooldowns {
            fn drop(&mut self) {
                clear_all_live_echo_cooldowns();
            }
        }
        let _reset = ResetCooldowns;
        let path = repo.path().to_string_lossy().into_owned();

        // The store this repository does not have yet. The stub used to derive
        // this from a marker file its own `build` branch wrote, which made every
        // step here depend on a child answering in time; the sequence is short
        // and known, so each step states the store it is acting on.
        bound.set_status(cli::available_status(
            r#"{"is_fresh":false,"schema_outdated":false,"rebuild_required":false,"schema_relation":"missing","degraded_reason":"no devmap store at this path (run `devmap build`)"}"#,
        ));

        // Opening the tab is an activation, not a watcher tick.
        let cold = maybe_refresh(&path, false);
        assert_eq!(cold.decision, LiveRefreshDecision::Refresh);
        assert!(cold.facts.artifacts_missing, "cold repo had no artifacts");
        assert!(
            cold.artifacts_restored,
            "the first build must publish a map"
        );
        assert!(
            missing_artifacts(repo.path()).is_empty(),
            "both artifacts exist after one automatic build"
        );
        let log = std::fs::read_to_string(repo.path().join("argv.log")).unwrap_or_default();
        assert!(
            log.lines().any(|line| {
                let tokens: Vec<&str> = line.split_whitespace().collect();
                tokens.first() == Some(&"build") && tokens.contains(&"--manifest")
            }),
            "the cold build must be a --manifest build, argv log:\n{log}"
        );

        // The build above made the store current, so that is what the probe now
        // reports for the rest of this test.
        bound.set_status(cli::available_status(
            r#"{"is_fresh":true,"schema_outdated":false,"rebuild_required":false}"#,
        ));

        // A second activation costs one status call and no build: the store is
        // fresh and the artifacts are on disk.
        let settled = maybe_refresh(&path, false);
        assert_eq!(settled.decision, LiveRefreshDecision::SkipFresh);

        // Deleting an artifact behind the app's back is recoverable. The store
        // still answers `is_fresh: true`, so only the artifact check can see it.
        std::fs::remove_file(&map).expect("remove map");
        let healed = maybe_refresh(&path, false);
        assert_eq!(
            healed.decision,
            LiveRefreshDecision::Refresh,
            "a missing artifact must override store freshness"
        );
        assert!(healed.artifacts_restored);
        assert!(missing_artifacts(repo.path()).is_empty());
    }
}
