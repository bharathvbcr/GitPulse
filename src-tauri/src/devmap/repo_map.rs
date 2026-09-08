//! GitPulse-owned mirror of devmap's `repo_map.json`.
//!
//! Upstream builds that artifact as a hand-assembled `json!` payload
//! (`devmap_query::manifest`) — there is no single deserialize-all struct to
//! import. This module owns the consumer shape GitPulse navigates, and a
//! fixture test fails when the keys we depend on drift.

use crate::engine::git_cli::validate_repo;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Accept JSON booleans or the 0/1 integers some producer fields emit.
fn deserialize_truthy<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Bool(b) => Ok(b),
        serde_json::Value::Number(n) => Ok(n.as_i64().unwrap_or(0) != 0),
        serde_json::Value::Null => Ok(false),
        other => Err(serde::de::Error::custom(format!(
            "expected bool or 0/1, got {other}"
        ))),
    }
}

/// Shown / total / truncated triple carried on every capped list in the map.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapMeta {
    #[serde(default)]
    pub shown: usize,
    #[serde(default)]
    pub total: usize,
    #[serde(default)]
    pub truncated: bool,
}

/// Per-confidence census nested under `liveness_meta.dead_symbol`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfidenceBucket {
    #[serde(default)]
    pub extracted: usize,
    #[serde(default)]
    pub inferred: usize,
    #[serde(default)]
    pub ambiguous: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeadSymbolMeta {
    #[serde(default)]
    pub shown: usize,
    #[serde(default)]
    pub total: usize,
    #[serde(default)]
    pub truncated: bool,
    /// Legacy synonym for `total`; kept for readers that predate the split.
    #[serde(default)]
    pub count: usize,
    #[serde(default)]
    pub by_confidence: DeadByConfidence,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeadByConfidence {
    #[serde(default)]
    pub shown: ConfidenceBucket,
    #[serde(default)]
    pub total: ConfidenceBucket,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnwiredMeta {
    #[serde(default)]
    pub shown: usize,
    #[serde(default)]
    pub total: usize,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub excluded_coverage_loss: usize,
    #[serde(default)]
    pub excluded_import_blind: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubsystemsMeta {
    #[serde(default)]
    pub shown: usize,
    #[serde(default)]
    pub total: usize,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub dropped_no_area: usize,
    #[serde(default)]
    pub neighbors_shown: usize,
    #[serde(default)]
    pub neighbors_total: usize,
    #[serde(default)]
    pub neighbors_truncated: bool,
    #[serde(default)]
    pub handoff_paths_shown: usize,
    #[serde(default)]
    pub handoff_paths_total: usize,
    #[serde(default)]
    pub handoff_paths_truncated: bool,
    #[serde(default)]
    pub role_files_shown: usize,
    #[serde(default)]
    pub role_files_total: usize,
    #[serde(default)]
    pub role_files_truncated: bool,
    #[serde(default)]
    pub neighbors_endpoints_unresolved: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LivenessMeta {
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub dead_symbol: DeadSymbolMeta,
    #[serde(default)]
    pub entry_roots: CapMeta,
    #[serde(default)]
    pub subsystems: SubsystemsMeta,
    #[serde(default)]
    pub important_files: CapMeta,
    #[serde(default)]
    pub unwired: UnwiredMeta,
    /// Reasons a list was withheld (e.g. `unreachable_files` when unreliable).
    #[serde(default)]
    pub unavailable: BTreeMap<String, String>,
}

/// One subsystem area from the consumer map.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoMapSubsystem {
    pub area: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub entry_points: Vec<String>,
    #[serde(default)]
    pub critical_files: Vec<String>,
    #[serde(default)]
    pub neighbors: Vec<String>,
    #[serde(default)]
    pub handoff_paths: Vec<String>,
    /// Capped samples per role bucket (`tests`, `entry`, `api`, …).
    #[serde(default)]
    pub role_files: BTreeMap<String, Vec<String>>,
    /// Real per-role totals beside the capped samples — never paper over these.
    #[serde(default)]
    pub role_file_counts: BTreeMap<String, usize>,
}

impl RepoMapSubsystem {
    /// Test files for this area, with the honesty pair (sample length vs total).
    pub fn tests_sample(&self) -> (Vec<String>, usize) {
        let sample = self.role_files.get("tests").cloned().unwrap_or_default();
        let total = self
            .role_file_counts
            .get("tests")
            .copied()
            .unwrap_or(sample.len());
        (sample, total)
    }
}

/// Typed mirror of the consumer `repo_map.json` document.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RepoMapDocument {
    #[serde(default)]
    pub map_engine: Option<String>,
    #[serde(default)]
    pub generated_head: String,
    #[serde(default)]
    pub indexed_hash: String,
    #[serde(default)]
    pub content_fingerprint: String,
    #[serde(default)]
    pub graph_degraded: bool,
    #[serde(default)]
    pub graph_degraded_reason: String,
    /// When true, `unreachable_files` must be ignored entirely.
    #[serde(default)]
    pub liveness_unreachable_unreliable: bool,
    #[serde(default)]
    pub entry_roots: Vec<String>,
    #[serde(default)]
    pub subsystems: Vec<RepoMapSubsystem>,
    #[serde(default)]
    pub unwired_candidates: Vec<String>,
    #[serde(default)]
    pub dead_symbol_candidates: Vec<String>,
    /// Prefer `unwired_candidates` / `dead_symbol_candidates`. See
    /// [`RepoMapDocument::trusted_unreachable_files`].
    #[serde(default)]
    pub unreachable_files: Vec<String>,
    #[serde(default)]
    pub liveness_meta: LivenessMeta,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub important_files: Vec<String>,
    #[serde(default)]
    pub package_managers: Vec<String>,
    #[serde(default)]
    pub test_commands: Vec<String>,
    /// Opaque: shape varies; navigator does not render it today.
    #[serde(default)]
    pub resolution_rate: Option<serde_json::Value>,
    #[serde(default)]
    pub meta: Option<serde_json::Value>,
    #[serde(default)]
    pub dead_clusters: Option<serde_json::Value>,
    /// Upstream currently emits 0/1; accept bool or int.
    #[serde(default, deserialize_with = "deserialize_truthy")]
    pub dead_clusters_truncated: bool,
    #[serde(default)]
    pub dead_clusters_incomplete: Option<String>,
}

impl RepoMapDocument {
    /// `unreachable_files` only when the producer says the answer is reliable.
    ///
    /// When `liveness_unreachable_unreliable` is set, the list is ignored
    /// entirely — the same rule the agent guides state. Prefer
    /// [`Self::unwired_candidates`] / [`Self::dead_symbol_candidates`].
    pub fn trusted_unreachable_files(&self) -> &[String] {
        if self.liveness_unreachable_unreliable {
            &[]
        } else {
            &self.unreachable_files
        }
    }

    /// Prefer unwired / dead-symbol lists; never surface unreliable unreachable.
    pub fn preferred_dead_candidates(&self) -> PreferredDeadLists<'_> {
        PreferredDeadLists {
            unwired: &self.unwired_candidates,
            dead_symbols: &self.dead_symbol_candidates,
            unreachable: self.trusted_unreachable_files(),
            unwired_meta: &self.liveness_meta.unwired,
            dead_meta: &self.liveness_meta.dead_symbol,
            unreachable_suppressed: self.liveness_unreachable_unreliable,
        }
    }
}

/// Honesty-carrying view of the three dead-code candidate lists.
#[derive(Debug, Clone, Copy)]
pub struct PreferredDeadLists<'a> {
    pub unwired: &'a [String],
    pub dead_symbols: &'a [String],
    pub unreachable: &'a [String],
    pub unwired_meta: &'a UnwiredMeta,
    pub dead_meta: &'a DeadSymbolMeta,
    pub unreachable_suppressed: bool,
}

/// Outcome of loading the on-disk consumer map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoMapLoad {
    pub available: bool,
    pub reason: Option<String>,
    pub path: Option<String>,
    pub map: Option<RepoMapDocument>,
}

impl RepoMapLoad {
    fn unavailable(reason: impl Into<String>, path: Option<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
            path,
            map: None,
        }
    }
}

/// Resolve `repo_map.json` through devmap's canonical state-directory owner.
pub fn repo_map_path(repo: impl AsRef<Path>) -> PathBuf {
    devmap_query::paths::repo_map_path(repo)
}

/// Parse a consumer map document from JSON text.
pub fn parse_repo_map(text: &str) -> Result<RepoMapDocument, String> {
    serde_json::from_str(text).map_err(|err| format!("repo_map.json is not valid JSON: {err}"))
}

/// Read and deserialize the map for a validated repository.
pub fn load_repo_map(repo_path: &str) -> RepoMapLoad {
    let repo = match validate_repo(repo_path) {
        Ok(repo) => repo,
        Err(e) => return RepoMapLoad::unavailable(e, None),
    };
    let path = repo_map_path(&repo);
    let path_str = path.to_string_lossy().into_owned();
    if !path.is_file() {
        return RepoMapLoad::unavailable(
            format!("no repo map at {path_str}; run Build Map first"),
            Some(path_str),
        );
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => {
            return RepoMapLoad::unavailable(
                format!("failed to read {path_str}: {e}"),
                Some(path_str),
            );
        }
    };
    match parse_repo_map(&text) {
        Ok(map) => RepoMapLoad {
            available: true,
            reason: None,
            path: Some(path_str),
            map: Some(map),
        },
        Err(e) => RepoMapLoad::unavailable(e, Some(path_str)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("fixtures/repo_map_minimal.json");

    #[test]
    fn fixture_deserializes_into_the_owned_mirror() {
        let map = parse_repo_map(FIXTURE).expect("fixture must parse");
        assert_eq!(map.map_engine.as_deref(), Some("devmap-rust"));
        assert!(map.liveness_unreachable_unreliable);
        assert_eq!(map.subsystems.len(), 1);
        let sub = &map.subsystems[0];
        assert_eq!(sub.area, "src/lib");
        assert_eq!(sub.entry_points, vec!["src/lib/map.rs"]);
        assert_eq!(sub.critical_files, vec!["src/lib/map.rs"]);
        let (tests, tests_total) = sub.tests_sample();
        assert_eq!(tests, vec!["src/lib/map_test.rs"]);
        // Sample is capped; total comes from role_file_counts.
        assert_eq!(tests.len(), 1);
        assert_eq!(tests_total, 5);
        assert_eq!(map.unwired_candidates.len(), 2);
        assert_eq!(map.dead_symbol_candidates.len(), 2);
        assert_eq!(map.unreachable_files.len(), 1);
    }

    #[test]
    fn prefers_unwired_and_dead_over_unreliable_unreachable() {
        let map = parse_repo_map(FIXTURE).expect("fixture");
        assert!(map.trusted_unreachable_files().is_empty());
        let preferred = map.preferred_dead_candidates();
        assert_eq!(preferred.unwired.len(), 2);
        assert_eq!(preferred.dead_symbols.len(), 2);
        assert!(preferred.unreachable.is_empty());
        assert!(preferred.unreachable_suppressed);
        assert!(preferred.dead_meta.truncated);
        assert_eq!(preferred.dead_meta.shown, 2);
        assert_eq!(preferred.dead_meta.total, 7);
        assert!(!preferred.unwired_meta.truncated);
    }

    #[test]
    fn carries_liveness_meta_honesty_for_capped_lists() {
        let map = parse_repo_map(FIXTURE).expect("fixture");
        let meta = &map.liveness_meta;
        assert_eq!(meta.engine.as_deref(), Some("devmap-rust"));
        assert_eq!(
            meta.entry_roots,
            CapMeta {
                shown: 2,
                total: 4,
                truncated: true
            }
        );
        assert!(meta.subsystems.truncated);
        assert_eq!(meta.subsystems.shown, 1);
        assert_eq!(meta.subsystems.total, 3);
        assert!(meta.subsystems.role_files_truncated);
        assert_eq!(meta.subsystems.role_files_shown, 2);
        assert_eq!(meta.subsystems.role_files_total, 6);
        assert!(meta.unavailable.contains_key("unreachable_files"));
    }

    #[test]
    fn reliable_unreachable_is_surfaced_when_flag_is_clear() {
        let mut map = parse_repo_map(FIXTURE).expect("fixture");
        map.liveness_unreachable_unreliable = false;
        assert_eq!(
            map.trusted_unreachable_files(),
            &["vendor/legacy/dead.rs".to_string()]
        );
        assert!(!map.preferred_dead_candidates().unreachable_suppressed);
    }

    #[test]
    fn missing_required_shape_fails_loudly() {
        // Wrong type on a field we pin — not a missing key (those default).
        // Serde can deserialize structs from sequences, so an empty array for
        // an object field is not a reliable refusal; a string-for-bool is.
        let err = parse_repo_map(r#"{"graph_degraded":"yes"}"#).expect_err("string is not a bool");
        assert!(
            err.contains("repo_map.json is not valid JSON"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_drop_of_liveness_meta_object_shape() {
        // Renaming the honesty block to a scalar must fail the mirror.
        let err = parse_repo_map(r#"{"liveness_meta":"gone"}"#).expect_err("scalar meta");
        assert!(err.contains("repo_map.json is not valid JSON"), "{err}");
    }

    #[test]
    fn repo_map_path_uses_the_canonical_standalone_default_and_dual_dir_precedence() {
        let root = tempfile::TempDir::new().expect("tempdir");
        assert_eq!(
            repo_map_path(root.path()),
            root.path().join(".devmap/repo_map.json")
        );
        std::fs::create_dir_all(root.path().join(".devcouncil")).expect("legacy state");
        assert_eq!(
            repo_map_path(root.path()),
            root.path().join(".devcouncil/repo_map.json")
        );
        std::fs::create_dir_all(root.path().join(".devmap")).expect("standalone state");
        assert_eq!(
            repo_map_path(root.path()),
            root.path().join(".devmap/repo_map.json")
        );
    }

    #[test]
    fn real_gitpulse_repo_map_deserializes_when_present() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        let path = repo_map_path(&root);
        if !path.is_file() {
            eprintln!("SKIPPED: no repo map at {}", path.display());
            return;
        }
        let text = std::fs::read_to_string(&path).expect("read real map");
        let map = parse_repo_map(&text).unwrap_or_else(|e| panic!("real map failed to parse: {e}"));
        assert!(
            !map.subsystems.is_empty(),
            "a real map should list at least one subsystem"
        );
        assert!(
            map.liveness_meta.subsystems.total >= map.liveness_meta.subsystems.shown,
            "liveness_meta.subsystems totals must be honest"
        );
        // Prefer unwired/dead; only trust unreachable when the flag allows it.
        let _ = map.preferred_dead_candidates();
        for sub in &map.subsystems {
            let (sample, total) = sub.tests_sample();
            assert!(
                total >= sample.len(),
                "{}: role_file_counts.tests ({total}) must not undercount the sample ({})",
                sub.area,
                sample.len()
            );
        }
    }
}
