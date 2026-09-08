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
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Default ranked node cap — matches `devmap_query::viz::VizOptions::default`.
pub const DEFAULT_VIZ_MAX_NODES: usize = 1_500;

/// Hard ceiling so a caller cannot ask the canvas for the uncapped graph.
pub const MAX_VIZ_MAX_NODES: usize = 5_000;

/// Match DevCouncil's default artifact-reader budget; never allocate an
/// unbounded JSON document before applying the much smaller canvas sample cap.
const MAX_VIZ_JSON_BYTES: u64 = 128 * 1024 * 1024;

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

/// Resolve `graph/code_graph.json` through devmap's canonical state-directory owner.
pub fn code_graph_path(repo: impl AsRef<Path>) -> PathBuf {
    devmap_query::paths::code_graph_path(repo)
}

fn read_json_file(path: &Path) -> Result<Value, String> {
    let file =
        std::fs::File::open(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|e| format!("failed to inspect {}: {e}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular graph file", path.display()));
    }
    let too_large = || {
        format!(
            "{} exceeds the 128 MiB visualization input limit; use a smaller graph export",
            path.display()
        )
    };
    if metadata.len() > MAX_VIZ_JSON_BYTES {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    file.take(MAX_VIZ_JSON_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    // The file may grow after metadata was read.
    if bytes.len() as u64 > MAX_VIZ_JSON_BYTES {
        return Err(too_large());
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("{} is not valid JSON: {e}", path.display()))
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
    let graph = match read_json_file(&path) {
        Ok(graph) => graph,
        Err(e) => {
            return GraphVizLoad::unavailable(GraphVizKind::CodeGraph, e, Some(path_str));
        }
    };
    if !graph.get("nodes").is_some_and(Value::is_array)
        || !graph.get("edges").is_some_and(Value::is_array)
    {
        return GraphVizLoad::unavailable(
            GraphVizKind::CodeGraph,
            "Invalid code graph: nodes and edges must be arrays; rebuild the map",
            Some(path_str),
        );
    }
    let tier = graph.pointer("/meta/compatibility_export_tier");
    if tier.is_some_and(|value| !value.is_null() && value.as_str() != Some("slim"))
        || graph
            .pointer("/meta/graph_export_incomplete_reason")
            .and_then(Value::as_str)
            .is_some_and(|reason| !reason.is_empty())
    {
        return GraphVizLoad::unavailable(GraphVizKind::CodeGraph, "Code graph is an incomplete or unsupported compatibility export; rebuild the map with a complete graph export", Some(path_str));
    }
    let options = devmap_query::viz::VizOptions {
        symbols: symbols.unwrap_or(false),
        max_nodes: clamp_max_nodes(max_nodes),
        title: "Code graph".to_string(),
    };
    let payload = devmap_query::viz::build_payload(&graph, &options);
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
    let repo_map = match read_json_file(&path) {
        Ok(map) => map,
        Err(e) => {
            return GraphVizLoad::unavailable(GraphVizKind::MapPreview, e, Some(path_str));
        }
    };
    if !repo_map.get("subsystems").is_some_and(Value::is_array) {
        return GraphVizLoad::unavailable(
            GraphVizKind::MapPreview,
            "Invalid repo map: subsystems must be an array; rebuild the map",
            Some(path_str),
        );
    }
    let payload = devmap_query::map_preview::build_preview_payload(&repo_map);
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
    fn code_graph_path_prefers_standalone_when_both_state_directories_exist() {
        let root = scratch("path");
        fs::create_dir_all(root.join(".devcouncil")).unwrap();
        fs::create_dir_all(root.join(".devmap")).unwrap();
        let path = code_graph_path(&root);
        assert!(path.ends_with(".devmap/graph/code_graph.json"));
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
    fn malformed_and_incomplete_graphs_are_unavailable() {
        let root = scratch("invalid");
        for graph in [json!(null), json!({}), json!({"nodes": [], "edges": {}})] {
            write_code_graph(&root, &graph);
            let load = load_code_graph_viz(root.to_str().unwrap(), None, None);
            assert!(
                !load.available,
                "malformed graph reported available: {graph}"
            );
            assert!(load.reason.unwrap().contains("nodes and edges"));
        }
        for tier in [
            json!("stub"),
            json!("compact"),
            json!("future-tier"),
            json!(12),
        ] {
            write_code_graph(
                &root,
                &json!({"meta":{"compatibility_export_tier":tier},"nodes":[],"edges":[]}),
            );
            let load = load_code_graph_viz(root.to_str().unwrap(), None, None);
            assert!(!load.available, "incomplete export reported available");
            assert!(load.reason.unwrap().contains("export"));
        }
        for tier in [Value::Null, json!("slim")] {
            write_code_graph(
                &root,
                &json!({"meta":{"compatibility_export_tier":tier},"nodes":[],"edges":[]}),
            );
            assert!(load_code_graph_viz(root.to_str().unwrap(), None, None).available);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_subsystem_shape_is_unavailable_but_an_empty_list_is_valid() {
        let root = scratch("invalid-map");
        let path = root.join(".devcouncil/repo_map.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        for text in ["null", "{}", "{\"subsystems\":{}}"] {
            fs::write(&path, text).unwrap();
            assert!(!load_map_preview(root.to_str().unwrap()).available);
        }
        fs::write(&path, "{\"subsystems\":[]}").unwrap();
        assert!(load_map_preview(root.to_str().unwrap()).available);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn oversized_json_is_rejected_before_parsing() {
        let root = scratch("oversized");
        let path = root.join("oversized.json");
        let file = fs::File::create(&path).unwrap();
        file.set_len(128 * 1024 * 1024 + 1).unwrap();
        let reason = read_json_file(&path).unwrap_err();
        assert!(reason.contains("exceeds"), "{reason}");
        fs::remove_dir_all(root).unwrap();
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
