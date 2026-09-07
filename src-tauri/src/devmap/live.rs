//! Live index: incremental `devmap build` off watcher `repo-changed`.
//!
//! ## Gate
//!
//! Refresh only when the index is believed stale, the schema is usable, and
//! no build is already running for that repo. "Fresh" means the caller has
//! already folded CLI `is_fresh` together with any watcher dirty signal —
//! a settled write makes the working tree diverge from the map even when the
//! store's pending queue is empty (no daemon).
//!
//! ## Store lifetime
//!
//! Never hold an open [`devmap_store::Store`] across a build. This module
//! only reads CLI status JSON, then shells out through [`super::refresh`],
//! which acquires [`super::cli::BuildGuard`] for the duration of the child.

use super::cli::{self, is_build_in_flight, BuildOutcome, CliStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
}

impl LiveRefreshDecision {
    pub fn should_refresh(self) -> bool {
        matches!(self, Self::Refresh)
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
        return (false, true, true);
    }
    let Some(payload) = status.status.as_ref() else {
        return (false, true, true);
    };
    let is_fresh = status_bool(payload, "is_fresh").unwrap_or(false);
    let schema_outdated = status_bool(payload, "schema_outdated").unwrap_or(false);
    (true, is_fresh, !schema_outdated)
}

/// Decide and optionally run an incremental build after a repo change.
///
/// `repo_changed` is the watcher half of the stale signal: a settled write
/// means the working tree may diverge from the indexed hashes even when the
/// store still reports `is_fresh: true` (no daemon pending queue). Folded
/// into effective freshness before the gate runs.
pub fn maybe_refresh(repo_path: &str, repo_changed: bool) -> LiveRefreshOutcome {
    let already_building = is_build_in_flight(repo_path);
    let cli = cli::status(repo_path);
    let (available, status_fresh, schema_ok) = freshness_from_cli_status(&cli);
    // Watcher dirty forces stale; status alone can also be stale (daemon).
    let is_fresh = status_fresh && !repo_changed;
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
                LiveRefreshDecision::SkipUnavailable => cli
                    .reason
                    .unwrap_or_else(|| "devmap status unavailable".into()),
                LiveRefreshDecision::Refresh => unreachable!(),
            }),
        };
    }
    match cli::refresh(repo_path) {
        Ok(build) => LiveRefreshOutcome {
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
        },
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
        assert_eq!(freshness_from_cli_status(&missing), (false, true, true));
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
}
