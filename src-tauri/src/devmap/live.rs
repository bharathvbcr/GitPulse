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
//! Incremental `devmap build --json` cannot restamp analyzer identity. When
//! status names an obsolete extraction payload, this module shells out through
//! [`super::build`] (`--manifest`) instead. Ordinary source-tree staleness
//! stays incremental.
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

#[cfg(test)]
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
        },
        build: None,
        reason: Some(format!(
            "live index cooldown active; retry in {ms}ms (unchanged or failed builds)"
        )),
        cooldown_remaining_ms: Some(ms),
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

/// True when `devmap status --json` says the stored extraction payload is from
/// an older analyzer. Other `degraded_reason` values, including source-tree
/// drift, must not force a full rebuild. Prefer the typed `rebuild_reason`
/// when present; keep the degraded-text match for binaries that only emit that.
pub fn needs_manifest_rebuild(payload: Option<&Value>) -> bool {
    let Some(status) = payload else {
        return false;
    };
    if status.get("rebuild_reason").and_then(Value::as_str) == Some("payload-obsolete") {
        return true;
    }
    status
        .get("degraded_reason")
        .and_then(Value::as_str)
        .is_some_and(|reason| reason.contains(OBSOLETE_PAYLOAD_REASON))
}

pub fn live_rebuild_kind(payload: Option<&Value>) -> LiveRebuildKind {
    if needs_manifest_rebuild(payload) {
        LiveRebuildKind::Manifest
    } else {
        LiveRebuildKind::Incremental
    }
}

/// Pure gate: stale → refresh, fresh → skip, in-flight → skip.
pub fn decide_live_refresh(facts: LiveRefreshFacts) -> LiveRefreshDecision {
    if facts.already_building {
        return LiveRefreshDecision::SkipBuilding;
    }
    if !facts.available {
        return LiveRefreshDecision::SkipUnavailable;
    }
    if !facts.schema_ok {
        return LiveRefreshDecision::SkipSchemaOutdated;
    }
    if facts.is_fresh {
        return LiveRefreshDecision::SkipFresh;
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
}

/// Serde mirror of [`LiveRefreshFacts`] for the command boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveRefreshFactsDto {
    pub available: bool,
    pub is_fresh: bool,
    pub schema_ok: bool,
    pub already_building: bool,
}

impl From<LiveRefreshFacts> for LiveRefreshFactsDto {
    fn from(facts: LiveRefreshFacts) -> Self {
        Self {
            available: facts.available,
            is_fresh: facts.is_fresh,
            schema_ok: facts.schema_ok,
            already_building: facts.already_building,
        }
    }
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
            },
            build: None,
            reason: Some("a devmap build is already running for this repo".into()),
            cooldown_remaining_ms: None,
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
    if !repo_changed {
        if let Some((remaining, unchanged_echo)) = cooldown {
            if unchanged_echo && !needs_manifest {
                return skip_cooldown_outcome(remaining);
            }
        }
    }
    // Watcher dirty forces stale; status alone can also be stale (daemon).
    // An obsolete extraction payload is a rebuild requirement even when the
    // CLI's `is_fresh` bit is inconsistent with `degraded_reason`.
    let is_fresh = status_fresh && !repo_changed && !needs_manifest;
    let facts = LiveRefreshFacts {
        available,
        is_fresh,
        schema_ok,
        already_building,
    };
    let decision = decide_live_refresh(facts);
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
                LiveRefreshDecision::SkipSchemaOutdated => {
                    "schema outdated; refuse incremental refresh".into()
                }
                LiveRefreshDecision::SkipUnavailable => cli.reason.unwrap_or_else(|| {
                    "devmap status unavailable or missing boolean freshness/schema fields".into()
                }),
                LiveRefreshDecision::SkipCooldown => unreachable!(),
                LiveRefreshDecision::Refresh => unreachable!(),
            }),
            cooldown_remaining_ms: None,
        };
    }
    let spawned = match live_rebuild_kind(cli.status.as_ref()) {
        LiveRebuildKind::Manifest => cli::build(repo_path),
        LiveRebuildKind::Incremental => cli::refresh(repo_path),
    };
    match spawned {
        Ok(build) => {
            if build_is_echo_or_failure(&build) {
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
        }
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
        cli::set_test_binary(Some(bin.to_string_lossy().into_owned()));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                cli::set_test_binary(None);
                clear_all_live_echo_cooldowns();
            }
        }
        let _reset = Reset;

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
        cli::set_test_binary(Some(bin.to_string_lossy().into_owned()));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                cli::set_test_binary(None);
                clear_all_live_echo_cooldowns();
            }
        }
        let _reset = Reset;
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
        cli::set_test_binary(Some(bin.to_string_lossy().into_owned()));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                cli::set_test_binary(None);
                clear_all_live_echo_cooldowns();
            }
        }
        let _reset = Reset;
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
        cli::set_test_binary(Some(bin.to_string_lossy().into_owned()));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                cli::set_test_binary(None);
                clear_all_live_echo_cooldowns();
            }
        }
        let _reset = Reset;
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
}
