pub mod artifacts;
pub mod code_graph;
pub mod engine;
pub mod escape;
pub mod manifest;
pub mod model;
pub mod query_match;
pub mod snapshots;

pub use artifacts::{
    render_subsystem_map_html, render_symbol_explorer_html, should_regenerate, write_atomic,
    ArtifactFingerprint,
};
pub use code_graph::{
    generate_code_graph_json, write_code_graph_atomically, CODE_GRAPH_DEFAULT_OUTPUT,
    CODE_GRAPH_SCHEMA_VERSION,
};
pub use engine::{budget_take, resolved_edge_from_stored, QueryEngine, StoreQueryEngine};
pub use escape::{html_escape, render_symbol_label};
pub use manifest::{
    generate_lean_manifest_json, generate_manifest, generate_manifest_with_edges,
    resolve_manifest_output, write_manifest_atomically,
};
pub use model::*;
pub use snapshots::{semantic_snapshot_for_file, semantic_snapshots, SemanticSnapshot};
