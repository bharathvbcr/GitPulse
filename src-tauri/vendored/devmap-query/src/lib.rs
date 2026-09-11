// `manifest.rs` builds `repo_map.json` as one `json!` literal, which expands
// recursively once per key. The default limit of 128 is below what that
// artifact needs; raising it is a build knob, not a behaviour change, and
// splitting the literal to stay under the default would scatter one document
// across several builders.
#![recursion_limit = "512"]

pub mod api_routes;
pub mod artifacts;
pub mod ast;
pub mod cancel;
pub mod code_graph;
pub mod cypher;
pub mod digest;
pub mod engine;
pub mod escape;
pub mod export;
pub mod freshness;
pub mod guides;
pub mod host;
pub mod hygiene;
pub mod inventory;
pub mod linguist;
pub mod manifest;
pub mod map_preview;
pub mod model;
pub mod query_match;
pub mod rung;
pub mod snapshots;

pub mod semantic;
pub mod viz;
pub mod workspace;

// Embedder facade: a host that depends on `devmap-query` should not need a
// second path dependency merely to name the Store accepted by
// `StoreQueryEngine`, or to resolve the canonical store path. These re-export
// the exact crate instances used here.
pub use devmap_extract::paths;
pub use devmap_store;

pub use artifacts::{
    should_regenerate, write_atomic, writer_identity, ArtifactFingerprint, ArtifactRecord,
    ArtifactStamp,
};
pub use cancel::{cancelled_queries, Cancel, QueryCancelled};
pub use code_graph::{
    build_code_graph_value, build_graph_core_value, decode_compact, encode_compact,
    generate_code_graph_encodings, generate_code_graph_json, write_code_graph_atomically,
    CODE_GRAPH_COMPACT_ENCODING, CODE_GRAPH_EXPORT_MAX_BYTES, CODE_GRAPH_SCHEMA_VERSION,
    CODE_GRAPH_TOP_LEVEL_KEYS, EDGE_KIND_LABELS,
};
pub use engine::{
    budget_take, clone_group_tokens, is_test_path, link_candidates, parse_clone_kind,
    resolved_edge_from_stored, traversal_starts, traversed_resolution_edges, workspace_search,
    PathOutsideRepoRoot, QueryEngine, StoreQueryEngine, BYTES_PER_TOKEN, MAX_NEIGHBOR_TARGETS,
    MAX_TOKEN_BUDGET, MAX_TRAVERSAL_DEPTH, PREVIEW_CALLER_MIN_CONFIDENCE,
};
pub use escape::{html_escape, render_symbol_label};
pub use linguist::{palette, swatch, Swatch, LINGUIST_VERSION, NEUTRAL_COLOR};
pub use manifest::{
    generate_lean_manifest_json, generate_manifest, generate_manifest_with_edges,
    resolve_manifest_output, write_manifest_atomically,
};
pub use map_preview::{
    build_preview_payload, fingerprint_for, render_map_preview_html, write_map_preview,
    FileAttribution,
};
pub use model::*;
pub use snapshots::{semantic_snapshot_for_file, semantic_snapshots, SemanticSnapshot};

pub use rung::{filter_by_rung, histogram as rung_histogram, Rung, RungHistogram};
