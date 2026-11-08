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
//! The `devmap status` probe is off the command too. It runs on its own thread
//! and the ask waits [`PROBE_GRACE`] for it, so an ordinary probe is answered in
//! the same call; a probe that has not answered by then (it is bounded only by
//! [`cli::STATUS_DEADLINE`]) is answered [`LiveRefreshDecision::SkipDeferred`]
//! with [`LiveRefreshOutcome::probe_pending`], and the next ask collects the
//! result. One probe runs per repository at a time, however many edits arrive
//! while it does, and a build or an explicit refresh forgets the last result.
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
use std::collections::{HashMap, HashSet};
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};
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
    /// load (background work is shed first), or the probe was started and has
    /// not answered yet ([`LiveRefreshOutcome::probe_pending`]). Nothing was
    /// examined, so this is
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
    forget_probe(repo_path);
}

/// Test reset for all three process-wide maps. Not unix-specific (it only
/// clears in-process state), so the ungated `concurrency_stress_*` test can
/// use it on Windows too.
#[cfg(test)]
pub(crate) fn clear_all_live_echo_cooldowns() {
    lock_live_waits().clear();
    lock_finished_builds().clear();
    lock_live_probes().clear();
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

#[derive(Debug)]
pub(crate) struct ParkedBuilds<V> {
    entries: HashMap<String, V>,
    order: std::collections::VecDeque<String>,
}

impl<V> Default for ParkedBuilds<V> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            order: std::collections::VecDeque::new(),
        }
    }
}

impl<V> ParkedBuilds<V> {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    pub(crate) fn remove(&mut self, key: &str) -> Option<V> {
        let removed = self.entries.remove(key);
        if removed.is_some() {
            self.order.retain(|k| k != key);
        }
        removed
    }

    #[cfg(test)]
    pub(crate) fn contains_key(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn keys(&self) -> impl Iterator<Item = &String> {
        self.order.iter()
    }
}

#[cfg(test)]
impl<V> std::ops::Index<&str> for ParkedBuilds<V> {
    type Output = V;

    fn index(&self, key: &str) -> &Self::Output {
        &self.entries[key]
    }
}

impl<V> std::ops::Index<&String> for ParkedBuilds<V> {
    type Output = V;

    fn index(&self, key: &String) -> &Self::Output {
        &self.entries[key.as_str()]
    }
}

/// Outcomes of builds [`start_refresh`] ran on a background thread, waiting to
/// be handed to the next ask for that repository. At most one per repository.
fn finished_builds() -> &'static Mutex<ParkedBuilds<LiveRefreshOutcome>> {
    static MAP: OnceLock<Mutex<ParkedBuilds<LiveRefreshOutcome>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(ParkedBuilds::default()))
}

fn lock_finished_builds() -> std::sync::MutexGuard<'static, ParkedBuilds<LiveRefreshOutcome>> {
    finished_builds()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Most outcomes [`finished_builds`] holds at once.
pub(crate) const MAX_FINISHED_BUILDS: usize = 256;

/// Park a background build's outcome for the next ask about `repo_path`.
///
/// An outcome is only collected by a later ask for the same path, and a
/// worktree seeded and refreshed in the background
/// ([`refresh_seeded_worktree`]) is often never asked about again: a lane
/// removed after its run, an MCP worktree nobody opens. Unbounded, the map kept
/// one outcome (with up to a build's captured stdout) per such worktree for the
/// life of the app or `gitpulse-mcp`. So a park first drops outcomes for paths
/// that no longer exist, then, if the map is still full, evicts the oldest
/// parked entry (LRU/FIFO). Dropping one is safe: an ask that finds nothing
/// parked probes `devmap status` and decides from that, exactly as it does for
/// a repository no background build ever ran for, so the cost is one extra probe.
fn park_finished_build(repo_path: &str, outcome: LiveRefreshOutcome) {
    park_bounded(
        &mut lock_finished_builds(),
        repo_path,
        outcome,
        MAX_FINISHED_BUILDS,
    );
}

/// [`park_finished_build`]'s rule over any [`ParkedBuilds`] and cap, so it is
/// testable without filling the process-wide map other tests' builds park in.
fn park_bounded<V>(parked: &mut ParkedBuilds<V>, repo_path: &str, value: V, cap: usize) {
    if cap == 0 {
        parked.clear();
        return;
    }
    if parked.entries.contains_key(repo_path) {
        parked.order.retain(|k| k != repo_path);
        parked.order.push_back(repo_path.to_string());
        parked.entries.insert(repo_path.to_string(), value);
        return;
    }
    if parked.entries.len() >= cap {
        parked
            .entries
            .retain(|path, _| std::path::Path::new(path).exists());
        parked
            .order
            .retain(|path| parked.entries.contains_key(path));
        while parked.entries.len() >= cap {
            if let Some(evicted) = parked.order.pop_front() {
                parked.entries.remove(&evicted);
            } else {
                break;
            }
        }
    }
    parked.order.push_back(repo_path.to_string());
    parked.entries.insert(repo_path.to_string(), value);
}

/// How long [`start_refresh`] waits for the `devmap status` probe it started
/// before answering that the probe is pending. Long enough that an ordinary
/// probe (tens of milliseconds) never shows the caller a pending answer, short
/// enough that a hung one costs the command this and not its 30 s deadline.
pub(crate) const PROBE_GRACE: Duration = Duration::from_secs(1);

/// How long an available probe answer stands in for a new probe: long enough
/// that the ask which collected it and a watcher tick right behind it share one
/// child, short enough that a `devmap build` run from a terminal is not masked.
const PROBE_FRESH: Duration = Duration::from_secs(2);

/// Status probes in flight at once across every repository. A probe that hangs
/// holds a thread for up to [`cli::STATUS_DEADLINE`]; this is what keeps a
/// restore of many tabs from holding one each.
const PROBE_LIMIT: usize = 4;

/// What [`start_refresh`] knows about the `devmap status` probe of one repository.
enum ProbeSlot {
    /// Running on its own thread. `last` is the answer before it, kept only to
    /// describe what the pending ask could not learn.
    Pending { id: u64, last: Option<CliStatus> },
    /// Answered. The first ask after it takes the result whatever its age (the
    /// ask that started the probe may have given up waiting); after that an
    /// available result is reused for [`PROBE_FRESH`] and an unavailable one
    /// never is, so a probe that could not run is retried by the next ask.
    Done {
        status: CliStatus,
        finished: Instant,
        served: bool,
    },
}

fn live_probes() -> &'static Mutex<HashMap<String, ProbeSlot>> {
    static MAP: OnceLock<Mutex<HashMap<String, ProbeSlot>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_live_probes() -> std::sync::MutexGuard<'static, HashMap<String, ProbeSlot>> {
    live_probes()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Drop what is known about `repo_path`'s status, running probe included: a
/// build changed the store, so a reading from before it describes nothing now,
/// and a probe that finishes after this is not recorded (see [`finish_probe`]).
fn forget_probe(repo_path: &str) {
    lock_live_probes().remove(repo_path);
}

/// What [`probe_status`] has for the ask.
enum ProbeAnswer {
    Status(CliStatus),
    /// The probe is running and has not answered; carries the answer before it.
    Pending(Option<CliStatus>),
    /// No probe was started, for this reason.
    Refused(String),
}

/// The status for one ask, without running a `devmap status` child on the
/// caller's thread. A result at hand is used; otherwise one probe is started
/// (single-flight per repository) and waited for [`PROBE_GRACE`].
fn probe_status(repo_path: &str, now: Instant) -> ProbeAnswer {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let (id, last) = {
        let mut probes = lock_live_probes();
        let last = match probes.get_mut(repo_path) {
            Some(ProbeSlot::Pending { last, .. }) => return ProbeAnswer::Pending(last.clone()),
            Some(ProbeSlot::Done {
                status,
                finished,
                served,
            }) => {
                let current =
                    status.available && now.saturating_duration_since(*finished) < PROBE_FRESH;
                if !*served || current {
                    *served = true;
                    return ProbeAnswer::Status(status.clone());
                }
                Some(status.clone())
            }
            None => None,
        };
        let running = probes
            .values()
            .filter(|slot| matches!(slot, ProbeSlot::Pending { .. }))
            .count();
        if running >= PROBE_LIMIT {
            return ProbeAnswer::Refused(format!(
                "{running} devmap status probes are already running; the index check waits for one to finish"
            ));
        }
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        probes.insert(
            repo_path.to_string(),
            ProbeSlot::Pending {
                id,
                last: last.clone(),
            },
        );
        (id, last)
    };
    let (answer, answered) = mpsc::channel();
    let repo = repo_path.to_string();
    let started =
        crate::supervisor::spawn_once("devmap-live-status", "devmap-live-status", move || {
            crate::engine::git_cli::with_background_processes(|| run_probe(&repo, id, &answer));
        });
    if let Err(error) = started {
        let mut probes = lock_live_probes();
        if matches!(probes.get(repo_path), Some(ProbeSlot::Pending { id: held, .. }) if *held == id)
        {
            probes.remove(repo_path);
        }
        return ProbeAnswer::Refused(format!(
            "could not start the devmap status probe thread: {error}"
        ));
    }
    match answered.recv_timeout(PROBE_GRACE) {
        Ok(status) => {
            if let Some(ProbeSlot::Done { served, .. }) = lock_live_probes().get_mut(repo_path) {
                *served = true;
            }
            ProbeAnswer::Status(status)
        }
        Err(_) => ProbeAnswer::Pending(last),
    }
}

/// The body of the probe thread: run the child, record the result for the next
/// ask, and hand it to the ask still waiting for it, if there is one. A panic is
/// recorded as a probe that could not run, then resumed for
/// `supervisor::spawn_once` to list the thread as stopped.
fn run_probe(repo_path: &str, id: u64, answer: &mpsc::Sender<CliStatus>) {
    let status = match catch_unwind(AssertUnwindSafe(|| cli::status(repo_path))) {
        Ok(status) => status,
        Err(payload) => {
            finish_probe(
                repo_path,
                id,
                CliStatus {
                    available: false,
                    binary: None,
                    lookup: None,
                    reason: Some("the devmap status probe thread panicked".into()),
                    status: None,
                },
            );
            resume_unwind(payload);
        }
    };
    finish_probe(repo_path, id, status.clone());
    // The ask may have stopped waiting; the result is in the slot either way.
    let _ = answer.send(status);
}

/// Record a finished probe, unless [`forget_probe`] has dropped it since: a
/// build that ran meanwhile made its reading stale before anyone saw it.
fn finish_probe(repo_path: &str, id: u64, status: CliStatus) {
    let mut probes = lock_live_probes();
    if matches!(probes.get(repo_path), Some(ProbeSlot::Pending { id: held, .. }) if *held == id) {
        probes.insert(
            repo_path.to_string(),
            ProbeSlot::Done {
                status,
                finished: Instant::now(),
                served: false,
            },
        );
    }
}

/// The answer to an ask whose status probe has not finished. Carries what the
/// previous probe established, if any, and says so; never a reading of the
/// repository now.
fn pending_probe_outcome(last: Option<&CliStatus>, repo_changed: bool) -> LiveRefreshOutcome {
    let previous = last.filter(|status| status.available);
    let facts = previous.map_or(UNEXAMINED, |status| {
        let (available, status_fresh, schema_ok) = freshness_from_cli_status(status);
        LiveRefreshFacts {
            available,
            is_fresh: status_fresh
                && !repo_changed
                && !needs_manifest_rebuild(status.status.as_ref()),
            schema_ok,
            already_building: false,
            rebuild_required: schema_rebuild_authorized(status.status.as_ref()),
            artifacts_missing: false,
            daemon_pending: None,
        }
    });
    let reason = if previous.is_some() {
        "devmap status has not answered yet; these facts are from the previous probe. Ask again to collect the new one."
    } else {
        "devmap status has not answered yet. Ask again to collect it."
    };
    LiveRefreshOutcome {
        probe_pending: true,
        ..skip_deferred_outcome(facts, reason.into())
    }
}

/// Active live builds keyed by repository canonical path.
/// Enforces single-flight serialization within the same repository while
/// allowing concurrent builds across distinct repositories.
fn live_build_slots() -> &'static Mutex<HashSet<PathBuf>> {
    static SLOTS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    SLOTS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Normalizes repository paths across symlinks and platform aliases (e.g. macOS /var vs /private/var).
fn canonical_repo_path(repo_path: &Path) -> PathBuf {
    std::fs::canonicalize(repo_path).unwrap_or_else(|_| repo_path.to_path_buf())
}

pub(crate) struct LiveBuildSlot {
    repo_path: PathBuf,
}

impl LiveBuildSlot {
    /// Claim a build slot for `repo_path`.
    /// Returns `Ok(Self)` if claimed, or `Err(holder)` if this repository is already building.
    pub(crate) fn claim(repo_path: &str) -> Result<Self, String> {
        let path = Path::new(repo_path);
        let canonical = canonical_repo_path(path);
        let mut slots = live_build_slots()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if slots.contains(&canonical) {
            return Err(canonical.display().to_string());
        }

        slots.insert(canonical.clone());
        Ok(Self {
            repo_path: canonical,
        })
    }
}

impl Drop for LiveBuildSlot {
    fn drop(&mut self) {
        live_build_slots()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.repo_path);
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
        probe_pending: false,
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
        probe_pending: false,
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
    let Some(report) = build.report.as_ref() else {
        return false;
    };
    if report.get("unchanged").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    report.get("synced_files").and_then(Value::as_u64) == Some(0)
        && report.get("deleted_files").and_then(Value::as_u64) == Some(0)
        && report.get("cleaned_files").and_then(Value::as_u64) == Some(0)
        && report
            .get("rebased_to_standalone")
            .is_some_and(Value::is_null)
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
    /// The `devmap status` probe this ask needed had not answered yet. `facts`
    /// are then the previous probe's (all unexamined when there is none), never
    /// a reading of the repository now, and `decision` is
    /// [`LiveRefreshDecision::SkipDeferred`]: ask again and the finished probe
    /// is collected. Absent from the JSON when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub probe_pending: bool,
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
fn is_stacked_zero_copy_lane(repo: &std::path::Path) -> bool {
    devmap_query::paths::stack_lease_path(repo).is_file()
        && !devmap_query::paths::store_path(repo).is_file()
}

pub fn missing_artifacts(repo_path: impl AsRef<std::path::Path>) -> Vec<&'static str> {
    let repo = repo_path.as_ref();
    if is_stacked_zero_copy_lane(repo) {
        return Vec::new();
    }
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
/// call, for as long as the child ran — up to [`cli::BUILD_DEADLINE`]. The
/// status probe is bounded the same way: it runs on its own thread and the call
/// waits at most [`PROBE_GRACE`] for it (see "Off the IPC path").
pub fn start_refresh(repo_path: &str, repo_changed: bool) -> LiveRefreshOutcome {
    live_refresh(
        repo_path,
        repo_changed,
        Instant::now(),
        LiveBuildRun::Detached,
    )
}

/// How a caller that just seeded a worktree's store waits for that worktree's
/// own build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeededRefresh {
    /// A long-lived process (the app, `gitpulse-mcp`): the build runs on a
    /// supervised background thread and this returns at once.
    Background,
    /// A short-lived process (the `gitpulse-worktree` CLI), whose background
    /// thread would die with it: the build runs here, within the build
    /// deadline.
    Inline,
}

/// Bring a worktree whose DevMap store was just seeded (cloned from its main
/// checkout by `codeintel::provision_worktree_devmap`) to the worktree's own
/// tree.
///
/// A seed is the main checkout's latest generation, which can trail that
/// checkout's HEAD and carry its uncommitted edits. DevMap reports such a store
/// honestly (`is_fresh: false`), but its SessionStart hook builds only where
/// there is no store at all, so nothing else would build it, and the worktree
/// would answer from the main checkout's working state indefinitely (measured
/// 2026-11-06: "ready (source moved)", still stale 20 s later). This is the
/// same gate every open repository gets: it builds only when `devmap status`
/// says the store is not fresh, incrementally (~2.5 s on GitPulse itself,
/// against ~7.6 s for the cold build a missing store costs). Every seeding
/// path calls this rather than its own build.
pub fn refresh_seeded_worktree(path: &str, run: SeededRefresh) -> LiveRefreshOutcome {
    let outcome = match run {
        SeededRefresh::Background => start_refresh(path, false),
        SeededRefresh::Inline => maybe_refresh(path, false),
    };
    log::info!(
        target: "devmap",
        "seeded worktree {path}: index refresh {:?}{}",
        outcome.decision,
        outcome
            .reason
            .as_deref()
            .map(|reason| format!(" ({reason})"))
            .unwrap_or_default()
    );
    outcome
}

/// How long a worktree's removal waits for a live index build still writing
/// into it before refusing. An incremental refresh takes ~2.5 s on GitPulse
/// itself unloaded, and the refresh this waits on is the one a just-created
/// worktree starts, so the bound covers that build under load: a refusal costs
/// the user a manual retry, a longer wait only delays a rare removal.
const REMOVAL_QUIESCE_BOUND: Duration = Duration::from_secs(8);

/// How often a waiting removal retries the writer slot.
const REMOVAL_QUIESCE_POLL: Duration = Duration::from_millis(25);

/// What a worktree's removal holds from the moment it quiesces index work
/// until the value drops: the in-process writer slot and DevMap's own on-disk
/// writer lock(s) for the worktree's store.
///
/// While it lives, every refresh of that path in this process (probe or build)
/// is turned away as `skip_building`, and every other process's `devmap build`,
/// `seed --sync-delta` or daemon drain queues behind the lock, so nothing
/// starts writing `.devcouncil/` into a directory that is about to be renamed
/// aside and deleted.
///
/// The on-disk lock is held across the removal on unix, where deleting a file
/// another descriptor has open is fine. On every other platform an open handle
/// blocks the delete (Windows), so there the lock is taken only to prove that
/// no writer is active and is released before this value is returned: a build
/// that starts in the instant between that release and the delete is not
/// excluded. That is the weaker guarantee, stated rather than hidden.
#[derive(Debug)]
pub struct RemovalHold {
    _permit: Option<cli::BuildPermit>,
    _writer_locks: Vec<devmap_query::devmap_store::WriterLock>,
}

/// The stores a build or delta sync in `root` writes, each with its own
/// `<db>.writer.lock`: a full or cloned store, and a stacked lane's delta.
/// Resolved through DevMap's own path owner, never spelled here.
fn store_dbs(root: &Path) -> [PathBuf; 2] {
    [
        devmap_query::paths::store_path(root),
        devmap_query::paths::stack_delta_path(root),
    ]
}

/// Why a store's writer lock was not taken, said as what happened.
///
/// DevMap's contention timeout is the only failure that means another process
/// is writing; a lock file that could not be opened (a symlink refused, a
/// read-only directory) or whose `flock` call failed says nothing about other
/// writers, and naming one would send the person to look for a process that is
/// not there. Both refuse, and both leave the worktree in place. The two are
/// told apart by the kernel's own contention wording, so a rewording upstream
/// degrades to the second message, which still carries the cause.
fn writer_lock_refusal(path: &str, bound: Duration, error: &dyn std::fmt::Display) -> String {
    if error.to_string().contains("another devmap writer holds") {
        format!(
            "another process is still writing the devmap index of {path} \
             ({error}) after {:.1}s; the worktree was left in place, retry the removal shortly",
            bound.as_secs_f32()
        )
    } else {
        format!(
            "the devmap index writer lock of {path} could not be taken ({error}); \
             the worktree was left in place. Check that the lock file is a regular \
             file in a writable directory, then retry the removal"
        )
    }
}

/// Take the writer lock of every store `root` has, waiting no longer than until `deadline`.
///
/// A store counts as present when its db or its lock file exists: a stacked
/// lane has the lock before its first delta. A tree with neither gets nothing
/// taken and nothing created (the lock file would otherwise be made, and its
/// directory with it): a build that starts later in such a tree is the
/// in-process slot's job, not this lock's.
fn lock_store_writers(
    root: &Path,
    deadline: Instant,
    bound: Duration,
    path: &str,
) -> Result<Vec<devmap_query::devmap_store::WriterLock>, String> {
    use devmap_query::devmap_store::Store;
    let mut locks = Vec::new();
    for db in store_dbs(root) {
        let lock_file = Store::writer_lock_path(&db);
        let (has_db, has_lock) = (db.exists(), lock_file.exists());
        if !has_db && !has_lock {
            continue;
        }
        let wait = deadline.saturating_duration_since(Instant::now());
        // The db when there is one, exactly as the writers resolve it; the
        // bare lock file for a stacked lane that has not made its delta yet.
        let taken = if has_db {
            Store::lock_writer_at(&db, wait)
        } else {
            Store::lock_file_at(&lock_file, wait)
        };
        match taken {
            Ok(lock) => locks.push(lock),
            Err(error) => return Err(writer_lock_refusal(path, bound, &error)),
        }
    }
    Ok(locks)
}

/// Quiesce live index work in `path` before its worktree is removed.
///
/// Two claims, one deadline of `bound` for both. First the repository's
/// in-process writer slot, the one a probe and a build hold for their whole run
/// ([`cli::claim_build`]): a refused claim waits for the build in flight,
/// polling. Then DevMap's own on-disk writer lock for each store the worktree
/// has ([`lock_store_writers`]), which the `devmap` CLI takes for the whole of a
/// build (measured, not assumed) and so is the only thing that sees a build
/// from another GitPulse process, `gitpulse-mcp`, a hook, or Claude Code's own
/// SessionStart. The lock waits only what is left of the bound.
///
/// There is no cancel: the build child is owned by procguard's run, which
/// exposes no handle to stop it, so waiting it out is the only stop that leaves
/// its files consistent. Past the bound this returns an error, the slot is
/// given back, and the caller must leave the worktree exactly as it was.
///
/// A path that is not a valid, trusted repository cannot have a build keyed to
/// it in this process, so it holds no slot; its stores, if it has any, are
/// still locked.
fn quiesce_for_removal_within(path: &str, bound: Duration) -> Result<RemovalHold, String> {
    let deadline = Instant::now() + bound;
    let permit = loop {
        match cli::claim_build(path) {
            Ok(permit) => {
                // An outcome parked for a worktree that is going away is
                // never collected.
                lock_finished_builds().remove(path);
                break Some(permit);
            }
            Err(cli::ClaimRefusal::Invalid(_)) => break None,
            Err(cli::ClaimRefusal::Busy(_)) => {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "a devmap index build is still writing into {path} after {:.1}s; the worktree was left in place, retry the removal shortly",
                        bound.as_secs_f32()
                    ));
                }
                std::thread::sleep(REMOVAL_QUIESCE_POLL);
            }
        }
    };
    let locks = lock_store_writers(Path::new(path), deadline, bound, path)?;
    Ok(RemovalHold {
        _permit: permit,
        // Released here, before the delete, where an open handle would block it.
        _writer_locks: if cfg!(unix) { locks } else { Vec::new() },
    })
}

/// [`quiesce_for_removal_within`] with the standard bound
/// ([`REMOVAL_QUIESCE_BOUND`]).
pub fn quiesce_for_removal(path: &str) -> Result<RemovalHold, String> {
    quiesce_for_removal_within(path, REMOVAL_QUIESCE_BOUND)
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
            probe_pending: false,
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

    let cli = match run {
        LiveBuildRun::Inline => cli::status(repo_path),
        LiveBuildRun::Detached => match probe_status(repo_path, now) {
            ProbeAnswer::Status(status) => status,
            ProbeAnswer::Pending(last) => {
                return pending_probe_outcome(last.as_ref(), repo_changed)
            }
            ProbeAnswer::Refused(reason) => return skip_deferred_outcome(UNEXAMINED, reason),
        },
    };
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
            probe_pending: false,
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
                    "the live index is already building {holder}; one automatic build runs at a time for this repository"
                )),
                cooldown_remaining_ms: None,
                artifacts_restored: false,
                index_too_slow: false,
                probe_pending: false,
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
    let use_stacked_delta_sync =
        !manifest && is_stacked_zero_copy_lane(std::path::Path::new(repo_path));
    let prepared = match permit {
        Some(permit) if use_stacked_delta_sync => permit.prepare_sync_delta(),
        Some(permit) => permit.prepare(manifest),
        None if use_stacked_delta_sync => cli::prepare_sync_delta(repo_path),
        None => cli::prepare(repo_path, manifest),
    };
    let prepared = match prepared {
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
                    probe_pending: false,
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
        park_finished_build(&settle.repo_path, outcome);
        // Free for the next repository the moment the outcome is
        // collectable, not when this thread finishes unwinding.
        drop(slot);
    });
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| run(settler))) {
        let outcome = panicked.record(
            Err("the live index build thread panicked".into()),
            build_clock.elapsed(),
        );
        park_finished_build(&panicked.repo_path, outcome);
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
        // Whatever the build did, the reading taken before it is stale now.
        forget_probe(&self.repo_path);
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
                    probe_pending: false,
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
            probe_pending: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::procguard::LockedSpawn;

    /// A seeded worktree that is never asked about again (removed after its
    /// run) must not keep its parked outcome for the life of the process: the
    /// next park drops outcomes for paths that are gone.
    #[test]
    fn a_full_park_drops_outcomes_for_worktrees_that_are_gone() {
        const CAP: usize = 8;
        // Each fresh map iterates in its own random order, so a rule that
        // evicted an arbitrary entry instead of the gone one would hide a live
        // outcome's eviction only about once in eight; ten maps make a chance
        // pass (1/8)^10.
        for round in 0..10 {
            let dir = tempfile::tempdir().unwrap();
            let mut parked: ParkedBuilds<usize> = ParkedBuilds::default();
            let gone = dir.path().join("removed-lane");
            std::fs::create_dir(&gone).unwrap();
            let gone = gone.to_string_lossy().into_owned();
            park_bounded(&mut parked, &gone, 0, CAP);
            let live: Vec<String> = (1..CAP)
                .map(|i| {
                    let path = dir.path().join(format!("lane-{i}"));
                    std::fs::create_dir(&path).unwrap();
                    path.to_string_lossy().into_owned()
                })
                .collect();
            for (i, path) in live.iter().enumerate() {
                park_bounded(&mut parked, path, i + 1, CAP);
            }
            std::fs::remove_dir(&gone).unwrap();

            let newest = dir.path().join("lane-new");
            std::fs::create_dir(&newest).unwrap();
            park_bounded(&mut parked, &newest.to_string_lossy(), CAP, CAP);

            assert!(
                !parked.contains_key(&gone),
                "round {round}: a removed worktree's outcome stayed"
            );
            assert!(
                live.iter().all(|path| parked.contains_key(path)),
                "round {round}: a live outcome was dropped while a gone one could go"
            );
            assert_eq!(parked.len(), CAP);
        }
    }

    /// However many worktrees park outcomes, the map stays at its cap; and a
    /// repository already parked is replaced in place, never evicting another.
    #[test]
    fn parked_outcomes_never_exceed_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let mut parked: ParkedBuilds<usize> = ParkedBuilds::default();
        let paths: Vec<String> = (0..10)
            .map(|i| {
                let path = dir.path().join(format!("lane-{i}"));
                std::fs::create_dir(&path).unwrap();
                path.to_string_lossy().into_owned()
            })
            .collect();
        for (i, path) in paths.iter().enumerate() {
            park_bounded(&mut parked, path, i, 4);
            assert!(parked.len() <= 4, "{} parked over a cap of 4", parked.len());
        }
        let kept: Vec<String> = parked.keys().cloned().collect();
        park_bounded(&mut parked, &kept[0], 99, 4);
        assert_eq!(parked.len(), 4);
        assert!(
            kept.iter().all(|path| parked.contains_key(path)),
            "re-parking one evicted another"
        );
        assert_eq!(parked[&kept[0]], 99);
    }

    #[test]
    fn parked_outcomes_hold_256_and_evict_oldest_lru_entry_first() {
        assert_eq!(
            MAX_FINISHED_BUILDS, 256,
            "finished_builds cap must be 256 for multi-lane agent worktrees"
        );
        let dir = tempfile::tempdir().unwrap();
        let mut parked = Default::default();
        let paths: Vec<String> = (0..6)
            .map(|i| {
                let path = dir.path().join(format!("lane-{i}"));
                std::fs::create_dir(&path).unwrap();
                path.to_string_lossy().into_owned()
            })
            .collect();
        // Park 0, 1, 2, 3 with cap 4 -> oldest is 0.
        for (i, path) in paths[..4].iter().enumerate() {
            park_bounded(&mut parked, path, i, 4);
        }
        // Refresh 0 -> recency order is now [1, 2, 3, 0], so oldest is 1!
        park_bounded(&mut parked, &paths[0], 100, 4);
        // Insert 4 -> must deterministically evict 1 (never 0, 2, or 3).
        park_bounded(&mut parked, &paths[4], 4, 4);
        assert!(
            !parked.contains_key(&paths[1]),
            "LRU eviction must drop oldest entry lane-1 first"
        );
        assert!(parked.contains_key(&paths[0]));
        assert!(parked.contains_key(&paths[2]));
        assert!(parked.contains_key(&paths[3]));
        assert!(parked.contains_key(&paths[4]));
    }
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
    fn build_is_echo_or_failure_treats_noop_stacked_sync_report_as_echo() {
        let noop_sync = BuildOutcome {
            ok: true,
            binary: "/bin/devmap".into(),
            lookup: cli::DevmapLookup::PathSearch,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            report: Some(json!({
                "synced_files": 0,
                "deleted_files": 0,
                "cleaned_files": 0,
                "rebased_to_standalone": null
            })),
        };
        assert!(
            build_is_echo_or_failure(&noop_sync),
            "0-change StackedSyncReport must raise echo cooldown to prevent watcher spin"
        );

        let synced_one = BuildOutcome {
            report: Some(json!({
                "synced_files": 1,
                "deleted_files": 0,
                "cleaned_files": 0,
                "rebased_to_standalone": null
            })),
            ..noop_sync.clone()
        };
        assert!(!build_is_echo_or_failure(&synced_one));

        let rebased = BuildOutcome {
            report: Some(json!({
                "synced_files": 0,
                "deleted_files": 0,
                "cleaned_files": 0,
                "rebased_to_standalone": "/tmp/.devcouncil/codeintel/devmap.sqlite"
            })),
            ..noop_sync
        };
        assert!(!build_is_echo_or_failure(&rebased));
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
            let canon = canonical_repo_path(&self.0);
            while live_build_slots()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(&canon)
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

    /// The command never waits on `devmap status` either. A probe that hangs
    /// (it is bounded only by a 30 s deadline) used to hold the IPC call, and a
    /// background permit, for all of it. The ask answers `skip_deferred` with
    /// `probe_pending` and facts that claim nothing; asks while the probe runs
    /// start no second one; the next ask after it answers carries its result.
    #[test]
    #[cfg(unix)]
    fn start_refresh_answers_before_a_hung_status_probe_and_collects_it_once() {
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
        let release_probe = bound.set_status_delayed(
            cli::available_status(r#"{"is_fresh":false,"schema_outdated":false}"#),
            Duration::from_secs(10),
        );
        let path = repo.path().to_string_lossy().into_owned();
        let _release = ReleaseStub(repo.path().to_path_buf());
        let probes_before = cli::test_status_calls();

        let asked = Instant::now();
        let first = start_refresh(&path, true);
        assert!(
            asked.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(2)),
            "the command waited {:?} on a status probe that has not answered",
            asked.elapsed()
        );
        assert!(first.probe_pending, "{first:?}");
        assert_eq!(
            first.decision,
            LiveRefreshDecision::SkipDeferred,
            "{first:?}"
        );
        assert!(first.build.is_none());
        assert!(
            !first.facts.available && !first.facts.is_fresh && !first.facts.schema_ok,
            "no probe has answered, so the facts claim nothing: {first:?}"
        );

        // An edit storm during the pending probe: still pending, no second probe.
        for _ in 0..3 {
            let during = start_refresh(&path, true);
            assert!(during.probe_pending, "{during:?}");
            assert_eq!(during.decision, LiveRefreshDecision::SkipDeferred);
        }
        assert_eq!(
            cli::test_status_calls() - probes_before,
            1,
            "a pending probe is single-flight per repository"
        );

        release_probe.store(true, std::sync::atomic::Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(60);
        let collected = loop {
            let answer = start_refresh(&path, true);
            if !answer.probe_pending {
                break answer;
            }
            assert!(
                Instant::now() < deadline,
                "the status probe never reported back"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        // The stale store the probe read is acted on: the build it authorizes
        // starts, from the probe's own answer rather than a second probe.
        assert_eq!(
            collected.decision,
            LiveRefreshDecision::Started,
            "{collected:?}"
        );
        assert!(collected.facts.available && !collected.facts.is_fresh);
        assert_eq!(
            cli::test_status_calls() - probes_before,
            1,
            "the next ask collects the finished probe instead of asking again"
        );
    }

    #[test]
    #[cfg(unix)]
    fn adversarial_hung_30s_status_probe_returns_within_grace_and_does_not_block_ipc() {
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
exit 2
"#,
        );
        // Simulate a hung or 30-second delayed probe
        let release_probe = bound.set_status_delayed(
            cli::available_status(r#"{"is_fresh":false,"schema_outdated":false}"#),
            Duration::from_secs(30),
        );
        let path = repo.path().to_string_lossy().into_owned();
        let _release = ReleaseStub(repo.path().to_path_buf());
        let probes_before = cli::test_status_calls();

        // Step 1: start_refresh must return within PROBE_GRACE + slack (< 1.5s), NOT 30s!
        let asked = Instant::now();
        let first = start_refresh(&path, true);
        let elapsed = asked.elapsed();

        assert!(
            elapsed >= PROBE_GRACE,
            "must wait at least PROBE_GRACE (1s), waited {:?}",
            elapsed
        );
        assert!(
            elapsed < crate::test_support::coverage_relaxed(Duration::from_millis(1500)),
            "command blocked past PROBE_GRACE! Took {:?}",
            elapsed
        );
        assert!(
            first.probe_pending,
            "must report probe_pending: true: {first:?}"
        );
        assert_eq!(
            first.decision,
            LiveRefreshDecision::SkipDeferred,
            "{first:?}"
        );
        assert!(first.build.is_none());

        // Step 2: Edit storm during hung probe: all return instantly (< 50ms total), no duplicate probes
        let burst_start = Instant::now();
        for _ in 0..5 {
            let burst_answer = start_refresh(&path, true);
            assert!(burst_answer.probe_pending);
            assert_eq!(burst_answer.decision, LiveRefreshDecision::SkipDeferred);
        }
        assert!(
            burst_start.elapsed()
                < crate::test_support::coverage_relaxed(Duration::from_millis(100)),
            "subsequent calls during pending probe must be instant: took {:?}",
            burst_start.elapsed()
        );
        assert_eq!(
            cli::test_status_calls() - probes_before,
            1,
            "must remain single-flight, no duplicate probes spawned"
        );

        // Step 3: Interactive execution is not starved while probe is hung in background
        let interactive_ran = crate::engine::git_cli::with_admission(
            crate::engine::git_cli::Admission::Interactive,
            || true,
        );
        assert!(interactive_ran);

        // Release the probe
        release_probe.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// An available probe answer serves the asks right behind it, so a watcher
    /// tick does not spawn a second child; one that could not run is never
    /// reused, so the next ask tries again; and an explicit refresh forgets it.
    #[test]
    #[cfg(unix)]
    fn a_probe_answer_is_reused_briefly_unless_it_could_not_run_or_a_build_intervened() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        struct ResetCooldowns;
        impl Drop for ResetCooldowns {
            fn drop(&mut self) {
                clear_all_live_echo_cooldowns();
            }
        }
        let _reset = ResetCooldowns;
        let (repo, bound) = stub_repository("#!/bin/sh\nexit 2\n");
        let path = repo.path().to_string_lossy().into_owned();

        bound.set_status(cli::available_status(
            r#"{"is_fresh":true,"schema_outdated":false}"#,
        ));
        let before = cli::test_status_calls();
        for _ in 0..3 {
            let answer = start_refresh(&path, false);
            assert_eq!(
                answer.decision,
                LiveRefreshDecision::SkipFresh,
                "{answer:?}"
            );
            assert!(!answer.probe_pending);
        }
        assert_eq!(
            cli::test_status_calls() - before,
            1,
            "one child served all three"
        );

        clear_live_echo_cooldown(&path);
        assert_eq!(
            start_refresh(&path, false).decision,
            LiveRefreshDecision::SkipFresh
        );
        assert_eq!(
            cli::test_status_calls() - before,
            2,
            "an explicit refresh drops the reading taken before it"
        );

        clear_all_live_echo_cooldowns();
        bound.set_status(cli::unavailable_status("devmap status exited 2"));
        let before = cli::test_status_calls();
        for _ in 0..2 {
            let answer = start_refresh(&path, false);
            assert_eq!(
                answer.decision,
                LiveRefreshDecision::SkipUnavailable,
                "{answer:?}"
            );
        }
        assert_eq!(
            cli::test_status_calls() - before,
            2,
            "a probe that could not run is not a reading to reuse"
        );
    }

    /// Hung probes hold a thread each, so only a few run at once; the rest are
    /// deferred (not pending: nothing was started for them) and ask again.
    #[test]
    #[cfg(unix)]
    fn only_a_few_status_probes_run_at_once() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        struct ResetCooldowns;
        impl Drop for ResetCooldowns {
            fn drop(&mut self) {
                clear_all_live_echo_cooldowns();
            }
        }
        let _reset = ResetCooldowns;
        let bound = cli::bind_test_binary("/gitpulse-test/no-such-devmap");
        let release = bound.set_status_delayed(
            cli::available_status(r#"{"is_fresh":true,"schema_outdated":false}"#),
            Duration::from_secs(10),
        );
        let before = cli::test_status_calls();
        for index in 0..PROBE_LIMIT {
            let pending = start_refresh(&format!("/gitpulse-test/probe-limit/{index}"), true);
            assert!(pending.probe_pending, "{pending:?}");
        }
        let refused = start_refresh("/gitpulse-test/probe-limit/over", true);
        assert_eq!(refused.decision, LiveRefreshDecision::SkipDeferred);
        assert!(!refused.probe_pending, "{refused:?}");
        assert_eq!(cli::test_status_calls() - before, PROBE_LIMIT);
        release.store(true, std::sync::atomic::Ordering::SeqCst);
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
            LiveBuildSlot::claim(&path).is_err(),
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

        drop(LiveBuildSlot::claim(&path).expect("the panic freed the slot"));
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
        drop(LiveBuildSlot::claim(&path).expect("the settler freed the slot"));
        let outcome = lock_finished_builds()
            .remove(&path)
            .expect("outcome stored");
        assert_eq!(outcome.reason.as_deref(), Some("devmap could not run"));
    }

    /// One automatic build at a time per repository: distinct repositories build concurrently,
    /// while duplicate builds on the same repository are serialized.
    #[test]
    #[cfg(unix)]
    fn one_live_build_runs_at_a_time_per_repository() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();

        let path_a = "/gitpulse-test/repo-a";
        let path_b = "/gitpulse-test/repo-b";

        let slot_a = LiveBuildSlot::claim(path_a).expect("slot a claims");
        assert!(
            LiveBuildSlot::claim(path_a).is_err(),
            "duplicate build on repo a must be refused"
        );

        // Concurrency verification: Repo B must succeed while Repo A is active!
        let slot_b = LiveBuildSlot::claim(path_b).expect("slot b claims concurrently with slot a");

        drop(slot_a);
        let slot_a2 = LiveBuildSlot::claim(path_a).expect("repo a claims again after drop");
        drop(slot_b);
        drop(slot_a2);
    }

    #[test]
    fn concurrency_stress_multiple_distinct_repositories() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();

        let num_repos = 8;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(num_repos));
        let mut handles = Vec::new();

        for i in 0..num_repos {
            let b = std::sync::Arc::clone(&barrier);
            let handle = std::thread::spawn(move || {
                let repo_path = format!("/gitpulse-test/concurrent-repo-{i}");
                let slot =
                    LiveBuildSlot::claim(&repo_path).expect("each distinct repo claims slot");

                // Duplicate claim on same repo must fail while slot is held
                assert!(LiveBuildSlot::claim(&repo_path).is_err());

                // Synchronize: all 8 slots are held concurrently across 8 threads
                b.wait();

                // Simulate indexing work
                std::thread::sleep(Duration::from_millis(25));

                drop(slot);
            });
            handles.push(handle);
        }

        for h in handles {
            h.join().expect("thread completes");
        }

        // Verify that after threads exit and drop slots, all repos can be reclaimed
        for i in 0..num_repos {
            let repo_path = format!("/gitpulse-test/concurrent-repo-{i}");
            let slot = LiveBuildSlot::claim(&repo_path).expect("slot can be reclaimed after drop");
            drop(slot);
        }
    }

    #[test]
    #[cfg(unix)]
    fn path_canonicalization_and_collision_rejection() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();

        let dir = tempfile::TempDir::new().expect("create tempdir");
        let dir_path = dir.path();

        // Create a symlink to the repo dir
        let symlink_dir = tempfile::TempDir::new().expect("create symlink parent dir");
        let symlink_path = symlink_dir.path().join("repo-symlink");
        std::os::unix::fs::symlink(dir_path, &symlink_path).expect("create symlink");

        let raw_str = dir_path.to_str().expect("valid utf8 path");
        let trailing_slash_str = format!("{}/", raw_str);
        let symlink_str = symlink_path.to_str().expect("valid utf8 symlink");
        let dot_rel_str = format!("{}/.", raw_str);

        // 1. Base path claims slot
        let slot = LiveBuildSlot::claim(raw_str).expect("claim base path succeeds");

        // 2. Trailing slash, symlink, and dot-relative MUST collide and be rejected
        assert!(
            LiveBuildSlot::claim(&trailing_slash_str).is_err(),
            "trailing slash collision must be rejected"
        );
        assert!(
            LiveBuildSlot::claim(symlink_str).is_err(),
            "symlink collision must be rejected"
        );
        assert!(
            LiveBuildSlot::claim(&dot_rel_str).is_err(),
            "dot-relative collision must be rejected"
        );

        // 3. Drop base slot, claim via symlink
        drop(slot);

        let slot_symlink = LiveBuildSlot::claim(symlink_str).expect("claim via symlink succeeds");
        assert!(
            LiveBuildSlot::claim(raw_str).is_err(),
            "base path rejected while symlink holds slot"
        );
        assert!(
            LiveBuildSlot::claim(&trailing_slash_str).is_err(),
            "trailing slash rejected while symlink holds slot"
        );
        drop(slot_symlink);

        // 4. Claim via trailing slash, verify base path and symlink are rejected
        let slot_slash =
            LiveBuildSlot::claim(&trailing_slash_str).expect("claim via trailing slash succeeds");
        assert!(
            LiveBuildSlot::claim(raw_str).is_err(),
            "base path rejected while trailing slash holds slot"
        );
        assert!(
            LiveBuildSlot::claim(symlink_str).is_err(),
            "symlink rejected while trailing slash holds slot"
        );
        drop(slot_slash);
    }

    #[test]
    #[cfg(unix)]
    fn symlink_and_trailing_slash_skip_building_in_maybe_refresh() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();

        let (repo, bound) = stub_repository("#!/bin/sh\nexit 0\n");
        let repo_path = repo.path();
        let raw_str = repo_path.to_string_lossy().into_owned();
        let slash_str = format!("{}/", raw_str);

        let symlink_dir = tempfile::TempDir::new().expect("create symlink parent dir");
        let symlink_path = symlink_dir.path().join("repo-symlink");
        std::os::unix::fs::symlink(repo_path, &symlink_path).expect("create symlink");
        let symlink_str = symlink_path.to_string_lossy().into_owned();

        bound.set_status(cli::available_status(
            r#"{"is_fresh":false,"schema_outdated":false,"rebuild_required":false}"#,
        ));

        // Claim slot via raw_str
        let slot = LiveBuildSlot::claim(&raw_str).expect("claim succeeds");

        // Attempting refresh via symlink or trailing slash must yield SkipBuilding
        let outcome_symlink = start_refresh(&symlink_str, false);
        assert_eq!(
            outcome_symlink.decision,
            LiveRefreshDecision::SkipBuilding,
            "symlink must yield SkipBuilding while base path holds slot"
        );

        let outcome_slash = start_refresh(&slash_str, false);
        assert_eq!(
            outcome_slash.decision,
            LiveRefreshDecision::SkipBuilding,
            "trailing slash must yield SkipBuilding while base path holds slot"
        );

        drop(slot);
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

    #[cfg(unix)]
    #[test]
    fn stacked_zero_copy_lane_skips_manifest_artifacts_and_syncs_delta() {
        use crate::procguard::LockedSpawn;
        let _lock = crate::harness::sidecar::test_serial();
        let repo = tempfile::tempdir().unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(repo.path())
            .status_locked()
            .unwrap()
            .success());
        crate::test_support::trust_repo(repo.path());

        let lease_path = devmap_query::paths::stack_lease_path(repo.path());
        std::fs::create_dir_all(lease_path.parent().unwrap()).unwrap();
        std::fs::write(&lease_path, r#"{"format_version":1}"#).unwrap();

        assert!(
            missing_artifacts(repo.path()).is_empty(),
            "stacked zero-copy lane must not require local repo_map/code_graph artifacts"
        );

        let bin = repo.path().join("devmap");
        std::fs::write(
            &bin,
            r#"#!/bin/sh
printf '%s\n' "$*" >> argv.log
if [ "$1" = "seed" ]; then
  printf '%s\n' '{"synced_files":1,"deleted_files":0,"cleaned_files":0,"rebased_to_standalone":null}'
  exit 0
fi
echo "unexpected full build on stacked lane: $*" >&2
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

        bound.set_status(cli::available_status(
            r#"{"is_fresh":true,"schema_outdated":false,"rebuild_required":false}"#,
        ));
        let clean = maybe_refresh(&path, false);
        assert_eq!(clean.decision, LiveRefreshDecision::SkipFresh);

        let dirty = maybe_refresh(&path, true);
        assert_eq!(dirty.decision, LiveRefreshDecision::Refresh);
        assert!(dirty.build.as_ref().is_some_and(|b| b.ok), "{dirty:?}");
        let log = std::fs::read_to_string(repo.path().join("argv.log")).unwrap_or_default();
        assert!(
            log.lines().any(|line| {
                let tokens: Vec<&str> = line.split_whitespace().collect();
                tokens.starts_with(&["seed", "--sync-delta", "--json"])
            }),
            "expected seed --sync-delta --json on stacked lane, got:\n{log}"
        );
    }
    /// A just-created worktree whose seeded background refresh is still
    /// writing into it, with a stub `devmap` that keeps writing new files
    /// under the worktree until `release` appears (or `iterations` pass).
    #[cfg(unix)]
    struct RemovalFixture {
        main: tempfile::TempDir,
        stub_dir: tempfile::TempDir,
        wt_parent: tempfile::TempDir,
        wt: PathBuf,
        /// The worktree's slot key, taken while the directory still exists.
        canon: PathBuf,
        /// The lane's DevMap store, which exists (empty) the way a seeded
        /// lane's does, so the writer lock a real build takes has a file.
        store: PathBuf,
        _bound: cli::TestBinaryBinding,
        _harness: crate::test_support::IsolatedHarnessGuard,
    }

    /// Env that turns [`xproc_writer_lock_holder`] from a no-op into a process
    /// that takes `lock_writer_at(db)`, announces it by creating `ready`, and
    /// keeps it until `release` exists.
    const XPROC_HOLD_DB: &str = "GITPULSE_XPROC_HOLD_DB";
    const XPROC_HOLD_READY: &str = "GITPULSE_XPROC_HOLD_READY";
    const XPROC_HOLD_RELEASE: &str = "GITPULSE_XPROC_HOLD_RELEASE";
    const XPROC_HOLDER_FILTER: &str = "devmap::live::tests::xproc_writer_lock_holder";

    /// The helper a separate process runs: the store's writer lock, held the
    /// way `devmap build` holds it. Ignored, so it only ever runs when a test
    /// re-executes this binary with `--ignored --exact`. It gives up on its own
    /// after a minute, so an orphan cannot hold a lock for good.
    #[test]
    #[ignore = "helper process for the cross-process removal tests"]
    fn xproc_writer_lock_holder() {
        let db = std::env::var_os(XPROC_HOLD_DB).expect("helper env: db");
        let ready = std::env::var_os(XPROC_HOLD_READY).expect("helper env: ready");
        let release =
            PathBuf::from(std::env::var_os(XPROC_HOLD_RELEASE).expect("helper env: release"));
        let _writer = devmap_query::devmap_store::Store::lock_writer_at(
            Path::new(&db),
            std::env::var("GITPULSE_XPROC_HOLD_WAIT_MS")
                .ok()
                .and_then(|ms| ms.parse().ok())
                .map_or(Duration::from_secs(20), Duration::from_millis),
        )
        .expect("helper takes the writer lock");
        std::fs::write(ready, "").expect("helper announces the lock");
        let give_up = Instant::now() + Duration::from_secs(60);
        while !release.exists() && Instant::now() < give_up {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A separate process holding a store's writer lock, as a `devmap build`
    /// from another GitPulse process or a Claude Code hook would.
    #[cfg(unix)]
    struct LockHolder {
        child: Option<std::process::Child>,
        release: PathBuf,
        _dir: tempfile::TempDir,
        _harness: crate::test_support::IsolatedHarnessGuard,
    }

    #[cfg(unix)]
    impl LockHolder {
        /// Starts the helper and returns once it holds the lock.
        fn hold(db: &Path) -> LockHolder {
            let dir = tempfile::tempdir().unwrap();
            let ready = dir.path().join("ready");
            let release = dir.path().join("release");
            let (mut command, harness) =
                crate::test_support::isolated_libtest_command(XPROC_HOLDER_FILTER);
            let child = command
                .arg("--ignored")
                .env(XPROC_HOLD_DB, db)
                .env(XPROC_HOLD_READY, &ready)
                .env(XPROC_HOLD_RELEASE, &release)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn_locked()
                .expect("spawn the lock holder");
            let mut holder = LockHolder {
                child: Some(child),
                release,
                _dir: dir,
                _harness: harness,
            };
            let give_up =
                Instant::now() + crate::test_support::coverage_relaxed(Duration::from_secs(30));
            while !ready.exists() {
                assert!(
                    Instant::now() < give_up,
                    "the lock holder never took the lock"
                );
                let exited = holder.child.as_mut().unwrap().try_wait().unwrap();
                assert!(exited.is_none(), "the lock holder exited early: {exited:?}");
                std::thread::sleep(Duration::from_millis(10));
            }
            holder
        }

        /// Path whose creation lets the holder go.
        fn release_path(&self) -> PathBuf {
            self.release.clone()
        }

        /// Releases the lock, then reaps the process and asserts it exited
        /// cleanly: no helper outlives its test.
        fn release_and_reap(mut self) {
            let _ = std::fs::write(&self.release, "");
            let mut child = self.child.take().expect("holder child");
            let give_up =
                Instant::now() + crate::test_support::coverage_relaxed(Duration::from_secs(30));
            loop {
                if let Some(status) = child.try_wait().expect("wait for the lock holder") {
                    assert!(status.success(), "the lock holder failed: {status:?}");
                    return;
                }
                assert!(Instant::now() < give_up, "the lock holder never exited");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    /// A test that fails part-way still reaps its helper.
    #[cfg(unix)]
    impl Drop for LockHolder {
        fn drop(&mut self) {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    #[cfg(unix)]
    fn removal_fixture(iterations: u32) -> RemovalFixture {
        use std::os::unix::fs::PermissionsExt;
        let git = |cwd: &std::path::Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output_locked()
                .expect("git");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let main = tempfile::tempdir().unwrap();
        git(main.path(), &["init", "-b", "main", "."]);
        git(main.path(), &["config", "user.email", "t@gitpulse.local"]);
        git(main.path(), &["config", "user.name", "Tester"]);
        std::fs::write(main.path().join("README.md"), "hi\n").unwrap();
        git(main.path(), &["add", "."]);
        git(main.path(), &["commit", "-m", "init"]);
        crate::test_support::trust_repo(main.path());
        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("lane");
        git(
            main.path(),
            &["worktree", "add", "-b", "lane", wt.to_str().unwrap()],
        );
        crate::test_support::trust_repo(&wt);
        // The index state is generated, not work: it must not make the lane
        // "dirty" to a merge.
        let exclude = main.path().join(".git/info/exclude");
        std::fs::create_dir_all(exclude.parent().unwrap()).unwrap();
        std::fs::write(&exclude, ".devcouncil/\n.devmap/\n").unwrap();
        write_stub_artifacts(&wt);
        let store = devmap_query::paths::store_path(&wt);
        std::fs::create_dir_all(store.parent().unwrap()).unwrap();
        std::fs::write(&store, "").unwrap();
        let stub_dir = tempfile::tempdir().unwrap();
        let bin = stub_dir.path().join("devmap");
        // The stub holds the store's writer lock for the whole build, as the
        // real `devmap build` does (measured: held from its first 100 ms to its
        // exit), by running the lock-holder helper beside it.
        let (holder_command, harness) =
            crate::test_support::isolated_libtest_command(XPROC_HOLDER_FILTER);
        let holder_exe = holder_command.get_program().to_string_lossy().into_owned();
        let script = format!(
            r#"#!/bin/sh
if [ "$1" = "build" ]; then
  {db_env}="{db}" {ready_env}="{stub}/hold-ready" {release_env}="{stub}/hold-release" \
    "{holder}" --exact --ignored {filter} >/dev/null 2>&1 &
  holder=$!
  n=0
  while [ ! -f "{stub}/hold-ready" ] && [ $n -lt 400 ]; do sleep 0.05; n=$((n+1)); done
  i=0
  while [ $i -lt {iterations} ] && [ ! -f "{stub}/release" ]; do
    mkdir -p "{wt}/.devcouncil/codeintel/d$i"
    echo x > "{wt}/.devcouncil/codeintel/d$i/f"
    sleep 0.1
    i=$((i+1))
  done
  : > "{stub}/hold-release"
  wait $holder
  printf '%s\n' '{{"ok":true,"unchanged":false}}'
  exit 0
fi
exit 2
"#,
            stub = stub_dir.path().display(),
            wt = wt.display(),
            db = store.display(),
            holder = holder_exe,
            filter = XPROC_HOLDER_FILTER,
            db_env = XPROC_HOLD_DB,
            ready_env = XPROC_HOLD_READY,
            release_env = XPROC_HOLD_RELEASE,
        );
        std::fs::write(&bin, script).unwrap();
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        let bound = cli::bind_test_binary(bin.to_string_lossy().into_owned());
        bound.set_status(cli::available_status(
            r#"{"is_fresh":false,"schema_outdated":false}"#,
        ));
        let canon = canonical_repo_path(&wt);
        RemovalFixture {
            canon,
            store,
            main,
            stub_dir,
            wt_parent,
            wt,
            _bound: bound,
            _harness: harness,
        }
    }

    #[cfg(unix)]
    /// A test that fails part-way still stops its stub build before the
    /// directories it writes into are deleted.
    #[cfg(unix)]
    impl Drop for RemovalFixture {
        fn drop(&mut self) {
            self.release_and_wait();
        }
    }

    #[cfg(unix)]
    impl RemovalFixture {
        /// Whether no live build still holds this worktree's writer slot.
        fn slot_free(&self) -> bool {
            !live_build_slots()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(&self.canon)
        }

        /// Releases the stub build and waits, bounded, for its slot to come
        /// free. Keyed on the worktree's own slot: the stub's `release` file
        /// lives in `stub_dir`, whose path is not the slot's key.
        fn release_and_wait(&self) {
            let _ = std::fs::write(self.stub_dir.path().join("release"), "");
            let deadline = Instant::now() + Duration::from_secs(35);
            while !self.slot_free() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        /// The end of every test: release, wait, and prove nothing is left
        /// running to write into a fixture that is about to be deleted.
        fn finish(&self) {
            self.release_and_wait();
            self.assert_idle();
        }

        /// What every test ends on: the stub build is over and its slot free,
        /// so nothing outlives the test and writes into a deleted fixture.
        fn assert_idle(&self) {
            assert!(
                self.slot_free(),
                "a stub build still holds the worktree's slot at the end of the test"
            );
        }

        fn start_background_refresh(&self) {
            let started =
                refresh_seeded_worktree(self.wt.to_str().unwrap(), SeededRefresh::Background);
            assert_eq!(
                started.decision,
                LiveRefreshDecision::Started,
                "{started:?}"
            );
            // Let the child begin writing, as it does moments after create.
            std::thread::sleep(Duration::from_millis(250));
        }

        fn remove(
            &self,
        ) -> Result<crate::engine::worktree::WorktreeRemoval, crate::ipc_error::CommandError>
        {
            crate::engine::worktree::remove_worktree_with(
                self.main.path().to_str().unwrap(),
                self.wt.to_str().unwrap(),
                true,
                &mut |aside: &std::path::Path| std::fs::remove_dir_all(aside),
            )
        }

        fn registered(&self) -> bool {
            let out = std::process::Command::new("git")
                .args(["worktree", "list", "--porcelain"])
                .current_dir(self.main.path())
                .output_locked()
                .expect("git worktree list");
            String::from_utf8_lossy(&out.stdout).contains("/lane")
        }
    }

    /// Removing a worktree moments after it was created used to race the
    /// seeded background refresh: the build kept writing `.devcouncil/` into
    /// the directory being deleted, and the delete failed "Directory not
    /// empty" after the worktree was already deregistered.
    #[test]
    #[cfg(unix)]
    fn removal_waits_for_the_seeded_refresh_and_leaves_no_directory() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let fixture = removal_fixture(600);
        fixture.start_background_refresh();
        // The refresh finishes a second after removal begins: well inside the
        // wait's bound, long after an unguarded removal has returned.
        let release_file = fixture.stub_dir.path().join("release");
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1000));
            std::fs::write(release_file, "").unwrap();
        });

        let canon = canonical_repo_path(&fixture.wt);
        let begun = Instant::now();
        let removal = fixture.remove().expect("removal succeeds");
        assert_eq!(removal.cleanup_error, None);
        assert!(
            begun.elapsed() >= Duration::from_millis(900),
            "removal returned after {:?}, before the refresh was released",
            begun.elapsed()
        );
        // Whatever the refresh still has to write, it writes before this.
        let settled = Instant::now() + Duration::from_secs(30);
        while live_build_slots()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&canon)
        {
            assert!(Instant::now() < settled, "the refresh never finished");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!fixture.wt.exists(), "worktree directory is left behind");
        assert!(!fixture.registered());
        releaser.join().unwrap();
        fixture.finish();
    }

    /// Once removal has begun nothing writes into the path again: no write
    /// from the refresh in flight and no new refresh may recreate it.
    #[test]
    #[cfg(unix)]
    fn no_refresh_writes_into_the_path_after_removal_starts() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let fixture = removal_fixture(600);
        fixture.start_background_refresh();
        let release_file = fixture.stub_dir.path().join("release");
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            std::fs::write(release_file, "").unwrap();
        });

        fixture.remove().expect("removal succeeds");
        for _ in 0..20 {
            assert!(
                !fixture.wt.exists(),
                "a refresh wrote into the removed worktree path"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        releaser.join().unwrap();
        let _ = &fixture.wt_parent;
        fixture.finish();
    }

    /// A refresh that outlives the bound: removal reports why and the
    /// worktree stays registered and intact. It never deregisters and then
    /// fails half-way.
    #[test]
    #[cfg(unix)]
    fn removal_refuses_when_the_refresh_outlives_the_bound() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let fixture = removal_fixture(600);
        let path = fixture.wt.to_string_lossy().into_owned();
        fixture.start_background_refresh();

        let started = Instant::now();
        let err = fixture
            .remove()
            .expect_err("a refresh still writing must refuse the removal");
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(30)),
            "the wait is unbounded: {:?}",
            started.elapsed()
        );
        let message = err.into_message();
        assert!(message.contains("devmap"), "{message}");
        assert!(fixture.wt.exists(), "the worktree was touched");
        assert!(
            fixture.wt.join(".git").exists(),
            "the worktree lost its .git link"
        );
        assert!(fixture.registered(), "the worktree was deregistered");
        assert!(cli::is_build_in_flight(&path));
        fixture.finish();
    }

    #[cfg(unix)]
    impl RemovalFixture {
        fn merge_and_teardown(
            &self,
        ) -> Result<
            crate::engine::worktree::MergeTeardownResult,
            crate::engine::worktree::MergeTeardownError,
        > {
            crate::engine::worktree::merge_and_teardown_worktree(
                self.main.path().to_str().unwrap(),
                self.wt.to_str().unwrap(),
                Some("main"),
                false,
                None,
                &mut |_args| Ok(()),
            )
        }
    }

    /// The path a lane normally ends by: merge, then teardown. It used to
    /// return while the seeded refresh was still writing into the lane.
    #[test]
    #[cfg(unix)]
    fn merge_teardown_waits_for_the_seeded_refresh() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let fixture = removal_fixture(600);
        fixture.start_background_refresh();
        let release_file = fixture.stub_dir.path().join("release");
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1000));
            std::fs::write(release_file, "").unwrap();
        });

        let begun = Instant::now();
        let merged = fixture.merge_and_teardown().expect("merge and teardown");
        assert!(merged.worktree_removed, "{merged:?}");
        assert!(
            begun.elapsed() >= Duration::from_millis(900),
            "teardown returned after {:?}, before the refresh was released",
            begun.elapsed()
        );
        releaser.join().unwrap();
        fixture.finish();
        assert!(!fixture.wt.exists(), "worktree directory is left behind");
        assert!(!fixture.registered());
    }

    /// A refresh that outlives the bound refuses before anything lands: the
    /// lane is neither merged nor removed.
    #[test]
    #[cfg(unix)]
    fn merge_teardown_refuses_before_merging_when_the_refresh_outlives_the_bound() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let fixture = removal_fixture(600);
        fixture.start_background_refresh();

        let err = fixture
            .merge_and_teardown()
            .expect_err("a refresh still writing must refuse the teardown");
        let message = format!("{err:?}");
        assert!(message.contains("devmap"), "{message}");
        assert!(fixture.wt.join(".git").exists(), "the lane was touched");
        assert!(fixture.registered(), "the lane was deregistered");
        fixture.finish();
    }

    /// Another PROCESS (the MCP server, a hook, a `devmap build` from Claude
    /// Code's SessionStart) is writing the index: the in-process slot cannot
    /// see it, DevMap's own writer lock can. Removal waits it out.
    #[test]
    #[cfg(unix)]
    fn removal_waits_for_a_writer_lock_held_by_another_process() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let fixture = removal_fixture(0);
        let holder = LockHolder::hold(&fixture.store);
        let release = holder.release_path();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1000));
            std::fs::write(release, "").unwrap();
        });

        let begun = Instant::now();
        let removal = fixture.remove().expect("removal succeeds");
        assert_eq!(removal.cleanup_error, None);
        assert!(
            begun.elapsed() >= Duration::from_millis(900),
            "removal returned after {:?}, while another process held the writer lock",
            begun.elapsed()
        );
        releaser.join().unwrap();
        holder.release_and_reap();
        assert!(!fixture.wt.exists(), "worktree directory is left behind");
        assert!(!fixture.registered());
        fixture.finish();
    }

    /// The normal end of a lane, with another process writing its index.
    #[test]
    #[cfg(unix)]
    fn merge_teardown_waits_for_a_writer_lock_held_by_another_process() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let fixture = removal_fixture(0);
        let holder = LockHolder::hold(&fixture.store);
        let release = holder.release_path();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1000));
            std::fs::write(release, "").unwrap();
        });

        let begun = Instant::now();
        let merged = fixture.merge_and_teardown().expect("merge and teardown");
        assert!(merged.worktree_removed, "{merged:?}");
        assert!(
            begun.elapsed() >= Duration::from_millis(900),
            "teardown returned after {:?}, while another process held the writer lock",
            begun.elapsed()
        );
        releaser.join().unwrap();
        holder.release_and_reap();
        assert!(!fixture.wt.exists(), "worktree directory is left behind");
        assert!(!fixture.registered());
        fixture.finish();
    }

    /// A writer in another process that outlives the bound: both removal
    /// paths refuse with the worktree intact and registered, the lane unmerged.
    #[test]
    #[cfg(unix)]
    fn removal_and_teardown_refuse_while_another_process_outlives_the_bound() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let fixture = removal_fixture(0);
        let holder = LockHolder::hold(&fixture.store);

        let started = Instant::now();
        let message = fixture
            .remove()
            .expect_err("a foreign writer must refuse the removal")
            .into_message();
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(30)),
            "the wait is unbounded: {:?}",
            started.elapsed()
        );
        assert!(message.contains("devmap"), "{message}");
        assert!(message.contains("retry"), "{message}");
        assert!(fixture.wt.join(".git").exists(), "the worktree was touched");
        assert!(fixture.registered(), "the worktree was deregistered");

        let torn = format!(
            "{:?}",
            fixture
                .merge_and_teardown()
                .expect_err("a foreign writer must refuse the teardown")
        );
        assert!(torn.contains("devmap"), "{torn}");
        assert!(fixture.wt.join(".git").exists(), "the lane was touched");
        assert!(fixture.registered(), "the lane was deregistered");
        holder.release_and_reap();
        fixture.finish();
    }

    /// A short bound, so the refusal's shape is cheap to pin: it names the
    /// path and the wait, says the tree was left, and costs about the bound.
    #[test]
    #[cfg(unix)]
    fn writer_lock_refusal_names_the_path_and_costs_one_bound() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let fixture = removal_fixture(0);
        let path = fixture.wt.to_string_lossy().into_owned();
        let holder = LockHolder::hold(&fixture.store);

        let begun = Instant::now();
        let message = quiesce_for_removal_within(&path, Duration::from_millis(400))
            .expect_err("a foreign writer must refuse");
        let waited = begun.elapsed();
        assert!(waited >= Duration::from_millis(350), "{waited:?}");
        assert!(
            waited < crate::test_support::coverage_relaxed(Duration::from_secs(5)),
            "{waited:?}"
        );
        assert!(message.contains(&path), "{message}");
        assert!(message.contains("0.4s"), "{message}");
        assert!(message.contains("left in place"), "{message}");
        // The slot taken first was given back with the refusal.
        assert!(fixture.slot_free(), "a refused removal kept the slot");
        holder.release_and_reap();
        let hold = quiesce_for_removal_within(&path, Duration::from_secs(5))
            .expect("the writer is gone: the guard is taken");
        drop(hold);
        fixture.finish();
    }

    /// One deadline for both waits. The in-process slot is busy for 0.8 s of a
    /// 1 s bound and a foreign process holds the lock throughout: the lock wait
    /// gets the remaining 0.2 s, so the refusal lands at ~1 s, not at 0.8 + 1.
    #[test]
    #[cfg(unix)]
    fn the_slot_wait_and_the_lock_wait_share_one_deadline() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let fixture = removal_fixture(0);
        let path = fixture.wt.to_string_lossy().into_owned();
        let holder = LockHolder::hold(&fixture.store);
        let permit = cli::claim_build(&path).expect("the slot is free");
        let waiting = {
            let path = path.clone();
            std::thread::spawn(move || {
                let begun = Instant::now();
                let outcome =
                    quiesce_for_removal_within(&path, Duration::from_millis(1000)).map(|_| ());
                (outcome, begun.elapsed())
            })
        };
        std::thread::sleep(Duration::from_millis(800));
        drop(permit);
        let (outcome, waited) = waiting.join().unwrap();
        assert!(outcome.is_err(), "the foreign writer still holds the lock");
        assert!(waited >= Duration::from_millis(950), "{waited:?}");
        assert!(
            waited < crate::test_support::coverage_relaxed(Duration::from_millis(1500)),
            "the two waits did not share a deadline: {waited:?}"
        );
        holder.release_and_reap();
        fixture.finish();
    }

    /// A stacked lane has no `devmap.sqlite`: its builds write `delta.sqlite`
    /// under `delta.sqlite.writer.lock`, so that is the lock the guard takes.
    #[test]
    #[cfg(unix)]
    fn a_stacked_lane_is_guarded_by_its_delta_writer_lock() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let fixture = removal_fixture(0);
        let path = fixture.wt.to_string_lossy().into_owned();
        std::fs::remove_file(&fixture.store).unwrap();
        std::fs::write(devmap_query::paths::stack_lease_path(&fixture.wt), "{}").unwrap();
        let delta = devmap_query::paths::stack_delta_path(&fixture.wt);
        let holder = LockHolder::hold(&delta);

        let message = quiesce_for_removal_within(&path, Duration::from_millis(300))
            .expect_err("a foreign delta writer must refuse");
        assert!(message.contains("delta.sqlite"), "{message}");
        holder.release_and_reap();
        drop(quiesce_for_removal_within(&path, Duration::from_secs(5)).expect("delta writer gone"));
        fixture.finish();
    }

    /// While the removal holds the guard, no other process can take the
    /// store's writer lock: a build that starts mid-removal queues behind it.
    #[test]
    #[cfg(unix)]
    fn the_guard_holds_the_writer_lock_until_it_drops() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let fixture = removal_fixture(0);
        let path = fixture.wt.to_string_lossy().into_owned();
        let hold = quiesce_for_removal_within(&path, Duration::from_secs(5)).expect("guard");
        assert!(
            devmap_query::devmap_store::Store::lock_writer_at(&fixture.store, Duration::ZERO)
                .is_err(),
            "the guard does not hold the writer lock"
        );
        drop(hold);
        drop(
            devmap_query::devmap_store::Store::lock_writer_at(&fixture.store, Duration::ZERO)
                .expect("the lock is free once the guard drops"),
        );
        fixture.finish();
    }

    /// A tree with no DevMap store gets no lock and nothing created: no
    /// `.devcouncil/`, no db, no lock file.
    #[test]
    #[cfg(unix)]
    fn a_tree_without_a_store_takes_no_lock_and_creates_nothing() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let listing = |root: &Path| {
            let mut found = Vec::new();
            let mut stack = vec![root.to_path_buf()];
            while let Some(dir) = stack.pop() {
                for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                    if entry.file_name() == ".git" {
                        continue;
                    }
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path.clone());
                    }
                    found.push(path);
                }
            }
            found.sort();
            found
        };

        // A real worktree whose store was never made.
        let fixture = removal_fixture(0);
        std::fs::remove_file(&fixture.store).unwrap();
        let path = fixture.wt.to_string_lossy().into_owned();
        let before = listing(&fixture.wt);
        drop(quiesce_for_removal_within(&path, Duration::from_secs(5)).expect("no store"));
        assert_eq!(listing(&fixture.wt), before, "the guard created files");

        // And one with no state directory at all.
        let bare = fixture.wt_parent.path().join("bare");
        std::fs::create_dir_all(bare.join("src")).unwrap();
        std::fs::write(bare.join("src/a.rs"), "fn a() {}\n").unwrap();
        let before = listing(&bare);
        drop(
            quiesce_for_removal_within(&bare.to_string_lossy(), Duration::from_secs(5))
                .expect("nothing to guard"),
        );
        assert_eq!(listing(&bare), before, "the guard created files");
        assert!(!bare.join(".devcouncil").exists() && !bare.join(".devmap").exists());
        fixture.finish();
    }

    /// The pre-merge hook runs in the lane and may run `devmap build`,
    /// `seed --sync-delta` or `dev verify` there: each needs the lane's writer
    /// lock. The teardown's own hold must not exist yet while it runs, or the
    /// hook's child waits on its parent.
    #[test]
    #[cfg(unix)]
    fn a_pre_merge_hook_can_take_the_lanes_writer_lock() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let fixture = removal_fixture(0);
        let scratch = tempfile::tempdir().unwrap();
        let ready = scratch.path().join("hook-ready");
        let release = scratch.path().join("hook-release");
        std::fs::write(&release, "").unwrap();
        let hook = format!(
            "{db_env}={db} {ready_env}={ready} {release_env}={release} {wait_env}=3000 {exe} --exact --ignored {filter}",
            db_env = XPROC_HOLD_DB,
            db = fixture.store.display(),
            ready_env = XPROC_HOLD_READY,
            ready = ready.display(),
            release_env = XPROC_HOLD_RELEASE,
            release = release.display(),
            wait_env = "GITPULSE_XPROC_HOLD_WAIT_MS",
            exe = fixture._harness.path().display(),
            filter = XPROC_HOLDER_FILTER,
        );
        let hooks = serde_json::json!({ "worktree": { "pre_merge": [hook] } }).to_string();
        std::fs::create_dir_all(fixture.main.path().join(".gitpulse")).unwrap();
        std::fs::write(fixture.main.path().join(".gitpulse/hooks.json"), &hooks).unwrap();
        let exclude = fixture.main.path().join(".git/info/exclude");
        std::fs::write(&exclude, ".devcouncil/\n.devmap/\n.gitpulse/\n").unwrap();
        crate::engine::approve_worktree_hooks(fixture.main.path(), &hooks)
            .expect("approve the fixture hook");

        let merged = fixture
            .merge_and_teardown()
            .expect("the hook took the lock and the teardown went on");
        assert!(merged.worktree_removed, "{merged:?}");
        assert!(ready.exists(), "the hook never ran");
        assert!(!fixture.wt.exists(), "worktree directory is left behind");
        fixture.finish();
    }

    /// Removing the primary checkout is refused before anything is quiesced:
    /// no wait on main's index, no slot, no write to main's lock file.
    #[test]
    #[cfg(unix)]
    fn removing_the_primary_checkout_is_refused_before_any_quiesce() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let fixture = removal_fixture(0);
        let main_store = devmap_query::paths::store_path(fixture.main.path());
        std::fs::create_dir_all(main_store.parent().unwrap()).unwrap();
        std::fs::write(&main_store, "").unwrap();
        let holder = LockHolder::hold(&main_store);
        let lock_file = devmap_query::devmap_store::Store::writer_lock_path(&main_store);
        let before = std::fs::read(&lock_file).unwrap();
        let main_text = fixture.main.path().to_str().unwrap();

        let begun = Instant::now();
        let message = crate::engine::worktree::remove_worktree_with(
            main_text,
            main_text,
            true,
            &mut |aside: &std::path::Path| std::fs::remove_dir_all(aside),
        )
        .expect_err("the primary checkout is never removed")
        .into_message();
        let waited = begun.elapsed();
        assert!(
            waited < crate::test_support::coverage_relaxed(Duration::from_secs(3)),
            "the refusal waited on main's index: {waited:?}"
        );
        assert!(message.contains("primary checkout"), "{message}");
        assert!(!message.contains("still writing"), "{message}");
        assert_eq!(
            std::fs::read(&lock_file).unwrap(),
            before,
            "main's lock file was written"
        );
        assert!(fixture.main.path().join(".git").exists());
        holder.release_and_reap();
        fixture.finish();
    }

    /// A lock that cannot be opened is not "another process writing": the
    /// refusal says what failed, still leaves the worktree, and never follows
    /// the symlink.
    #[test]
    #[cfg(unix)]
    fn a_symlinked_lock_file_is_refused_as_unopenable_not_as_a_writer() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let fixture = removal_fixture(0);
        let path = fixture.wt.to_string_lossy().into_owned();
        let outside = fixture.wt_parent.path().join("outside.txt");
        std::fs::write(&outside, "UNTOUCHED").unwrap();
        let lock = devmap_query::devmap_store::Store::writer_lock_path(&fixture.store);
        let _ = std::fs::remove_file(&lock);
        std::os::unix::fs::symlink(&outside, &lock).unwrap();

        let begun = Instant::now();
        // The bound is far above the most `coverage_relaxed` can stretch one
        // second (8x instrumented, then up to 4x for load), so the assertion
        // below still tells an immediate refusal from a wait-out of the bound.
        let message = quiesce_for_removal_within(&path, Duration::from_secs(60))
            .expect_err("a lock that cannot be opened must refuse");
        assert!(
            begun.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(1)),
            "an open failure waited out the bound: {:?}",
            begun.elapsed()
        );
        assert!(message.contains("could not be taken"), "{message}");
        assert!(message.contains("left in place"), "{message}");
        assert!(!message.contains("still writing"), "{message}");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "UNTOUCHED");
        assert!(fixture.slot_free(), "a refused removal kept the slot");
        fixture.finish();
    }

    /// Store free, delta held by another process: the refusal must not leave
    /// the store's lock held.
    #[test]
    #[cfg(unix)]
    fn a_partial_acquisition_releases_the_first_lock() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let fixture = removal_fixture(0);
        let path = fixture.wt.to_string_lossy().into_owned();
        let delta = devmap_query::paths::stack_delta_path(&fixture.wt);
        std::fs::write(&delta, "").unwrap();
        let holder = LockHolder::hold(&delta);

        quiesce_for_removal_within(&path, Duration::from_millis(300))
            .expect_err("the foreign delta writer must refuse");
        assert!(fixture.slot_free(), "a refused removal kept the slot");
        drop(
            devmap_query::devmap_store::Store::lock_writer_at(&fixture.store, Duration::ZERO)
                .expect("the refusal leaked the store's lock"),
        );
        holder.release_and_reap();
        fixture.finish();
    }
}
