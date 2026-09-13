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
//! ## Echo / failure cooldown
//!
//! A `repo_changed` tick that rebuilds and learns the index was already warm
//! (`unchanged: true`) — or that the build failed — used to reopen a 1 Hz
//! loop whenever the build itself wrote under a watched path the noise gate
//! missed. Per-repo exponential cooldown (1s → 2s → … capped at 60s) answers
//! further `repo_changed` ticks with [`LiveRefreshDecision::SkipCooldown`]
//! until the wait elapses.
//!
//! Activation (`repo_changed: false`) still reads status so a payload-obsolete
//! index can `--manifest` rebuild even while the wait is active. An
//! `unchanged: true` echo's wait *does* block activation incremental rebuilds:
//! the last build already proved the tree current, and treating focus as a
//! new stale signal restarted a 1 Hz status+build loop. Failed-build waits
//! still allow activation retry. Explicit `devmap build` / `refresh` commands
//! still call [`clear_live_echo_cooldown`] so the next watcher tick is not
//! stranded behind a stale wait.
//!
//! ## Store lifetime
//!
//! Never hold an open [`devmap_store::Store`] across a build. This module
//! only reads CLI status JSON, then shells out through [`super::refresh`] or
//! [`super::build`], which acquire [`super::cli::BuildGuard`] for the duration
//! of the child.

use super::cli::{self, is_build_in_flight, BuildOutcome, CliStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
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
    /// [`is_build_in_flight`] already true for this repo.
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

#[derive(Debug, Clone)]
struct EchoCooldown {
    until: Instant,
    /// Consecutive echo/failure builds that raised the wait (0 before first).
    streak: u32,
    /// Last raise was `unchanged: true` (not a failed build). Activation
    /// must not spawn another incremental rebuild while this wait holds;
    /// payload-obsolete still bypasses it after a status peek.
    unchanged_echo: bool,
}

fn echo_cooldowns() -> &'static Mutex<HashMap<String, EchoCooldown>> {
    static MAP: OnceLock<Mutex<HashMap<String, EchoCooldown>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Drop the echo/failure cooldown for `repo_path` (explicit build/refresh).
pub fn clear_live_echo_cooldown(repo_path: &str) {
    if let Ok(mut map) = echo_cooldowns().lock() {
        map.remove(repo_path);
    }
}

#[cfg(all(test, unix))]
pub(crate) fn clear_all_live_echo_cooldowns() {
    if let Ok(mut map) = echo_cooldowns().lock() {
        map.clear();
    }
}

/// Next cooldown length after `streak` consecutive echo/failure builds.
///
/// `streak` is the count already recorded before this raise (0 → 1s, 1 → 2s, …).
pub(crate) fn cooldown_secs_for_streak(streak: u32) -> u64 {
    let shift = streak.min(6);
    (1u64 << shift).min(COOLDOWN_CAP_SECS)
}

fn cooldown_state(repo_path: &str, now: Instant) -> Option<(Duration, bool)> {
    let map = echo_cooldowns().lock().ok()?;
    let entry = map.get(repo_path)?;
    if entry.until <= now {
        return None;
    }
    Some((
        entry.until.saturating_duration_since(now),
        entry.unchanged_echo,
    ))
}

fn raise_echo_cooldown(repo_path: &str, now: Instant, unchanged_echo: bool) {
    let Ok(mut map) = echo_cooldowns().lock() else {
        return;
    };
    let streak = map
        .get(repo_path)
        .map(|e| e.streak.saturating_add(1))
        .unwrap_or(0);
    let secs = cooldown_secs_for_streak(streak);
    map.insert(
        repo_path.to_string(),
        EchoCooldown {
            until: now + Duration::from_secs(secs),
            streak,
            unchanged_echo,
        },
    );
}

fn skip_cooldown_outcome(remaining: Duration) -> LiveRefreshOutcome {
    let ms = remaining.as_millis().min(u128::from(u64::MAX)) as u64;
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
        reason: Some(format!(
            "live index cooldown active; retry in {ms}ms (unchanged or failed builds)"
        )),
        cooldown_remaining_ms: Some(ms),
        artifacts_restored: false,
    }
}

fn note_productive_build(repo_path: &str) {
    clear_live_echo_cooldown(repo_path);
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
    /// Remaining echo/failure backoff when `decision` is [`SkipCooldown`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_remaining_ms: Option<u64>,
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

/// Decide and optionally run an incremental build after a repo change.
///
/// `repo_changed` is the watcher half of the stale signal: a settled write
/// means the working tree may diverge from the indexed hashes even when the
/// store still reports `is_fresh: true` (no daemon pending queue). Folded
/// into effective freshness before the gate runs. Watcher ticks (`true`) are
/// blocked by echo/failure cooldown. Activation (`false`) still peeks status:
/// an `unchanged` echo's wait blocks another incremental rebuild, but a
/// payload-obsolete index bypasses it so `--manifest` can restamp analyzer
/// identity. An `unchanged` or failed live build is not a productive restamp
/// and must not clear the wait.
pub fn maybe_refresh(repo_path: &str, repo_changed: bool) -> LiveRefreshOutcome {
    let already_building = is_build_in_flight(repo_path);
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
        };
    }

    let now = Instant::now();
    let cooldown = cooldown_state(repo_path, now);
    if repo_changed {
        if let Some((remaining, _)) = cooldown {
            return skip_cooldown_outcome(remaining);
        }
    }

    let cli = cli::status(repo_path);
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
    if !repo_changed {
        if let Some((remaining, unchanged_echo)) = cooldown {
            // A build that only rewrote artifacts reports the store
            // `unchanged`, so it raises this wait every time. Letting the wait
            // then block the next activation is what made a missing
            // `repo_map.json` unrecoverable without a manual build.
            if unchanged_echo && !needs_manifest && !artifacts_missing {
                return skip_cooldown_outcome(remaining);
            }
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
        already_building,
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
                LiveRefreshDecision::Refresh => unreachable!(),
            }),
            cooldown_remaining_ms: None,
            artifacts_restored: false,
        };
    }
    let spawned = match live_rebuild_kind(cli.status.as_ref(), artifacts_missing) {
        LiveRebuildKind::Manifest => cli::build(repo_path),
        LiveRebuildKind::Incremental => cli::refresh(repo_path),
    };
    match spawned {
        Ok(build) => {
            // Only a *successful* build can be credited with the artifacts,
            // and only ones that were actually absent beforehand count.
            let artifacts_restored =
                artifacts_missing && build.ok && missing_artifacts(repo_path).is_empty();
            if artifacts_restored {
                // Productive even when the store echoed `unchanged`.
                note_productive_build(repo_path);
            } else if artifacts_missing {
                // This build ran *because* artifacts were absent and they are
                // absent still. Whatever the store said about itself, the build
                // did not do the thing it was spawned to do, so it backs off on
                // the failure flavour rather than the benign-echo one — an
                // `unchanged: true` here describes the database, not the files
                // beside it that were the reason to run at all.
                raise_echo_cooldown(repo_path, Instant::now(), false);
            } else if build_is_echo_or_failure(&build) {
                raise_echo_cooldown(
                    repo_path,
                    Instant::now(),
                    build.ok
                        && build
                            .report
                            .as_ref()
                            .and_then(|report| report.get("unchanged"))
                            .and_then(Value::as_bool)
                            == Some(true),
                );
            } else if build.ok {
                note_productive_build(repo_path);
            }
            LiveRefreshOutcome {
                decision,
                facts: facts.into(),
                reason: if build.ok {
                    None
                } else {
                    Some(
                        build
                            .stderr
                            .trim()
                            .to_string()
                            .if_empty_then(|| "devmap refresh failed".into()),
                    )
                },
                build: Some(build),
                cooldown_remaining_ms: None,
                artifacts_restored,
            }
        }
        Err(e) => {
            // BuildGuard race: status said free, then another build won.
            let decision = if e.contains("already running") {
                LiveRefreshDecision::SkipBuilding
            } else {
                decision
            };
            LiveRefreshOutcome {
                decision,
                facts: LiveRefreshFactsDto {
                    already_building: decision == LiveRefreshDecision::SkipBuilding,
                    ..facts.into()
                },
                build: None,
                reason: Some(e),
                cooldown_remaining_ms: None,
                artifacts_restored: false,
            }
        }
    }
}

trait IfEmpty {
    fn if_empty_then(self, f: impl FnOnce() -> String) -> String;
}

impl IfEmpty for String {
    fn if_empty_then(self, f: impl FnOnce() -> String) -> String {
        if self.is_empty() {
            f()
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
            .output()
            .expect("git init");
        assert!(output.status.success());
        crate::test_support::trust_repo(repo.path());
        write_stub_artifacts(repo.path());
        let bin = repo.path().join("devmap");
        std::fs::write(
            &bin,
            r#"#!/bin/sh
if [ "$1" = "status" ]; then
  printf '%s\n' '{"is_fresh":true,"schema_outdated":false}'
  exit 0
fi
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
        let _bound = cli::bind_test_binary(bin.to_string_lossy().into_owned());
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

    #[test]
    #[cfg(unix)]
    fn failed_build_storm_enters_the_same_cooldown() {
        let _lock = crate::harness::sidecar::test_serial();
        clear_all_live_echo_cooldowns();
        let repo = tempfile::TempDir::new().unwrap();
        let output = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(repo.path())
            .output()
            .expect("git init");
        assert!(output.status.success());
        crate::test_support::trust_repo(repo.path());
        write_stub_artifacts(repo.path());
        let bin = repo.path().join("devmap");
        std::fs::write(
            &bin,
            r#"#!/bin/sh
if [ "$1" = "status" ]; then
  printf '%s\n' '{"is_fresh":false,"schema_outdated":false}'
  exit 0
fi
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
        let _bound = cli::bind_test_binary(bin.to_string_lossy().into_owned());
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
            .output()
            .expect("git init");
        assert!(output.status.success());
        crate::test_support::trust_repo(repo.path());
        write_stub_artifacts(repo.path());
        let bin = repo.path().join("devmap");
        std::fs::write(
            &bin,
            r#"#!/bin/sh
if [ "$1" = "status" ]; then
  printf '%s\n' '{"is_fresh":false,"schema_outdated":false}'
  exit 0
fi
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
        let _bound = cli::bind_test_binary(bin.to_string_lossy().into_owned());
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
            .output()
            .expect("git init");
        assert!(output.status.success());
        crate::test_support::trust_repo(repo.path());
        write_stub_artifacts(repo.path());
        let bin = repo.path().join("devmap");
        std::fs::write(
            &bin,
            r#"#!/bin/sh
printf '%s\n' "$*" >> argv.log
if [ "$1" = "status" ]; then
  if [ -f status.json ]; then cat status.json; else printf '%s\n' '{"is_fresh":false,"schema_outdated":false}'; fi
  exit 0
fi
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
        let _bound = cli::bind_test_binary(bin.to_string_lossy().into_owned());
        struct ResetCooldowns;
        impl Drop for ResetCooldowns {
            fn drop(&mut self) {
                clear_all_live_echo_cooldowns();
            }
        }
        let _reset = ResetCooldowns;
        let path = repo.path().to_string_lossy().into_owned();
        std::fs::write(
            repo.path().join("status.json"),
            r#"{"is_fresh":false,"schema_outdated":false}"#,
        )
        .unwrap();
        let first = maybe_refresh(&path, true);
        assert_eq!(first.decision, LiveRefreshDecision::Refresh);
        std::fs::write(
            repo.path().join("status.json"),
            r#"{"is_fresh":false,"schema_outdated":false,"rebuild_reason":"payload-obsolete"}"#,
        )
        .unwrap();
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
            .output()
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
if [ "$1" = "status" ]; then
  if [ -f built ]; then
    printf '%s\n' '{{"is_fresh":true,"schema_outdated":false,"rebuild_required":false}}'
  else
    printf '%s\n' '{{"is_fresh":false,"schema_outdated":false,"rebuild_required":false,"schema_relation":"missing","degraded_reason":"no devmap store at this path (run `devmap build`)"}}'
  fi
  exit 0
fi
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
        let _bound = cli::bind_test_binary(bin.to_string_lossy().into_owned());
        struct ResetCooldowns;
        impl Drop for ResetCooldowns {
            fn drop(&mut self) {
                clear_all_live_echo_cooldowns();
            }
        }
        let _reset = ResetCooldowns;
        let path = repo.path().to_string_lossy().into_owned();

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
