//! Code-graph and subsystem-map canvas payloads.
//!
//! Wraps `devmap_query::viz::build_payload` and
//! `devmap_query::map_preview::build_preview_payload`. Both are renderer-agnostic
//! JSON — GitPulse draws them on its own canvas; it does not serve the HTML
//! pages those modules can also emit.
//!
//! The viz node cap defaults to 1500, ranked by degree. The payload always
//! carries `counts.nodes_truncated` so a capped picture cannot be mistaken for
//! the whole graph.

use crate::engine::git_cli::validate_repo;
use devmap_query::host::{ArtifactProvider, FilesystemArtifactProvider};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Default ranked node cap — matches `devmap_query::viz::VizOptions::default`.
pub const DEFAULT_VIZ_MAX_NODES: usize = 1_500;

/// Hard ceiling so a caller cannot ask the canvas for the uncapped graph.
pub const MAX_VIZ_MAX_NODES: usize = 5_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GraphVizKind {
    CodeGraph,
    MapPreview,
}

/// Envelope around a viz / map-preview payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphVizLoad {
    pub available: bool,
    pub reason: Option<String>,
    pub path: Option<String>,
    pub kind: GraphVizKind,
    /// Renderer-agnostic payload from `build_payload` / `build_preview_payload`.
    pub payload: Option<Value>,
}

impl GraphVizLoad {
    fn unavailable(kind: GraphVizKind, reason: impl Into<String>, path: Option<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
            path,
            kind,
            payload: None,
        }
    }
}

/// Resolve `graph/code_graph.json` the same way `repo_map_path` resolves the map.
pub fn code_graph_path(repo: impl AsRef<Path>) -> PathBuf {
    devmap_query::paths::code_graph_path(repo)
}

fn clamp_max_nodes(requested: Option<usize>) -> usize {
    requested
        .unwrap_or(DEFAULT_VIZ_MAX_NODES)
        .clamp(1, MAX_VIZ_MAX_NODES)
}

/// Build the code-graph canvas payload from on-disk `code_graph.json`.
pub fn load_code_graph_viz(
    repo_path: &str,
    symbols: Option<bool>,
    max_nodes: Option<usize>,
) -> GraphVizLoad {
    let repo = match validate_repo(repo_path) {
        Ok(repo) => repo,
        Err(e) => return GraphVizLoad::unavailable(GraphVizKind::CodeGraph, e, None),
    };
    let path = code_graph_path(&repo);
    let path_str = path.to_string_lossy().into_owned();
    if !path.is_file() {
        return GraphVizLoad::unavailable(
            GraphVizKind::CodeGraph,
            format!("no code graph at {path_str}; run Build Map first"),
            Some(path_str),
        );
    }
    let options = devmap_query::viz::VizOptions {
        symbols: symbols.unwrap_or(false),
        max_nodes: clamp_max_nodes(max_nodes),
        title: "Code graph".to_string(),
    };
    let provider = FilesystemArtifactProvider::for_repo(&repo);
    let payload = match provider.code_graph_payload(&options) {
        Ok(payload) => payload,
        Err(error) => {
            return GraphVizLoad::unavailable(
                GraphVizKind::CodeGraph,
                error.to_string(),
                Some(path_str),
            );
        }
    };
    GraphVizLoad {
        available: true,
        reason: None,
        path: Some(path_str),
        kind: GraphVizKind::CodeGraph,
        payload: Some(payload),
    }
}

/// Build the subsystem map-preview payload from on-disk `repo_map.json`.
pub fn load_map_preview(repo_path: &str) -> GraphVizLoad {
    let repo = match validate_repo(repo_path) {
        Ok(repo) => repo,
        Err(e) => return GraphVizLoad::unavailable(GraphVizKind::MapPreview, e, None),
    };
    let path = super::repo_map::repo_map_path(&repo);
    let path_str = path.to_string_lossy().into_owned();
    if !path.is_file() {
        return GraphVizLoad::unavailable(
            GraphVizKind::MapPreview,
            format!("no repo map at {path_str}; run Build Map first"),
            Some(path_str),
        );
    }
    let provider = FilesystemArtifactProvider::for_repo(&repo);
    let payload = match provider.repo_map_payload() {
        Ok(payload) => payload,
        Err(error) => {
            return GraphVizLoad::unavailable(
                GraphVizKind::MapPreview,
                error.to_string(),
                Some(path_str),
            );
        }
    };
    GraphVizLoad {
        available: true,
        reason: None,
        path: Some(path_str),
        kind: GraphVizKind::MapPreview,
        payload: Some(payload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("gitpulse-viz-{label}-{stamp}"));
        fs::create_dir_all(dir.join(".git")).unwrap();
        dir
    }

    fn write_code_graph(root: &Path, graph: &Value) {
        let path = root.join(".devcouncil/graph/code_graph.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, serde_json::to_string(graph).unwrap()).unwrap();
    }

    fn sample_graph() -> Value {
        json!({
            "meta": { "generation_id": 7 },
            "entry_roots": ["src/a.rs"],
            "unwired_candidates": ["src/orphan.rs"],
            "dead_code": [{ "id": "src/dead.rs", "confidence": "extracted" }],
            "nodes": [
                { "id": "src/a.rs", "name": "a.rs", "kind": "file", "path": "src/a.rs",
                  "area": "src", "community": "core", "language": "rust", "line": 1 },
                { "id": "src/b.rs", "name": "b.rs", "kind": "file", "path": "src/b.rs",
                  "area": "src", "community": "core", "language": "rust", "line": 1 },
                { "id": "src/orphan.rs", "name": "orphan.rs", "kind": "file", "path": "src/orphan.rs",
                  "area": "src", "community": "", "language": "rust", "line": 1 },
                { "id": "src/dead.rs", "name": "dead.rs", "kind": "file", "path": "src/dead.rs",
                  "area": "src", "community": "", "language": "rust", "line": 1 },
                { "id": "src/a.rs::foo", "name": "foo", "kind": "function", "path": "src/a.rs",
                  "area": "src", "community": "core", "language": "rust", "line": 10 },
                { "id": "src/b.rs::bar", "name": "bar", "kind": "function", "path": "src/b.rs",
                  "area": "src", "community": "core", "language": "rust", "line": 20 },
            ],
            "edges": [
                { "source": "src/a.rs", "target": "src/b.rs", "kind": "imports",
                  "confidence": 0.9, "resolution": "extracted" },
                { "source": "src/a.rs::foo", "target": "src/b.rs::bar", "kind": "calls",
                  "confidence": 0.95, "resolution": "extracted" },
            ]
        })
    }

    #[test]
    fn code_graph_path_prefers_devcouncil_when_present() {
        let root = scratch("path");
        fs::create_dir_all(root.join(".devcouncil")).unwrap();
        let path = code_graph_path(&root);
        assert!(path.ends_with(".devcouncil/graph/code_graph.json"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_graph_is_unavailable_not_empty() {
        let root = scratch("missing");
        let load = load_code_graph_viz(root.to_str().unwrap(), None, None);
        assert!(!load.available);
        assert!(load.payload.is_none());
        assert!(load
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("no code graph"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn file_level_payload_carries_truncation_honesty() {
        let root = scratch("files");
        write_code_graph(&root, &sample_graph());
        let load = load_code_graph_viz(root.to_str().unwrap(), Some(false), Some(2));
        assert!(load.available, "{:?}", load.reason);
        let payload = load.payload.expect("payload");
        assert_eq!(payload["level"], "file");
        assert_eq!(payload["counts"]["nodes_shown"], 2);
        assert_eq!(payload["counts"]["nodes_total"], 4);
        assert_eq!(payload["counts"]["nodes_truncated"], true);
        assert_eq!(payload["counts"]["max_nodes"], 2);
        assert_eq!(payload["generation_id"], 7);
        // Ranked by degree: a.rs and b.rs (the import edge) beat isolates.
        let ids: Vec<&str> = payload["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&"src/a.rs"));
        assert!(ids.contains(&"src/b.rs"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn symbol_level_keeps_calls_not_imports() {
        let root = scratch("symbols");
        write_code_graph(&root, &sample_graph());
        let load = load_code_graph_viz(root.to_str().unwrap(), Some(true), None);
        assert!(load.available);
        let payload = load.payload.expect("payload");
        assert_eq!(payload["level"], "symbol");
        assert_eq!(payload["counts"]["nodes_truncated"], false);
        let links = payload["links"].as_array().unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0]["kind"], "calls");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn map_preview_builds_subsystem_nodes() {
        let root = scratch("map");
        let map_path = root.join(".devcouncil/repo_map.json");
        fs::create_dir_all(map_path.parent().unwrap()).unwrap();
        fs::write(&map_path, include_str!("fixtures/repo_map_minimal.json")).unwrap();
        let load = load_map_preview(root.to_str().unwrap());
        assert!(load.available, "{:?}", load.reason);
        assert_eq!(load.kind, GraphVizKind::MapPreview);
        let payload = load.payload.expect("payload");
        let nodes = payload["nodes"].as_array().unwrap();
        assert!(!nodes.is_empty());
        assert!(nodes.iter().any(|n| n["id"] == "src/lib"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn max_nodes_clamps_to_ceiling() {
        assert_eq!(clamp_max_nodes(Some(0)), 1);
        assert_eq!(
            clamp_max_nodes(Some(DEFAULT_VIZ_MAX_NODES)),
            DEFAULT_VIZ_MAX_NODES
        );
        assert_eq!(clamp_max_nodes(Some(999_999)), MAX_VIZ_MAX_NODES);
        assert_eq!(clamp_max_nodes(None), DEFAULT_VIZ_MAX_NODES);
    }
}
