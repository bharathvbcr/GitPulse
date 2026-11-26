// `manifest.rs` builds `repo_map.json` as one `json!` literal, which expands
// recursively once per key. The default limit of 128 is below what that
// artifact needs; raising it is a build knob, not a behaviour change, and
// splitting the literal to stay under the default would scatter one document
// across several builders.
#![recursion_limit = "512"]

pub mod api_routes;
pub mod artifacts;
pub mod ast;
pub mod block_layout;
pub mod bounded;
pub mod cancel;
pub mod cas;
pub mod code_graph;
pub mod cypher;
pub mod digest;
pub mod doc_drift;
pub mod engine;
pub mod escape;
pub mod evidence;
pub mod export;
pub mod federate;
pub mod freshness;
pub mod guides;
pub mod host;
pub mod hygiene;
pub mod inventory;
pub mod lean_tcb;
pub mod linguist;
pub mod manifest;
pub mod map_preview;
pub mod model;
pub mod overlay;
mod page;
pub mod query_match;
pub mod rung;
pub mod scope;
pub mod snapshots;
pub mod stacked;
pub mod stat_memo;

pub mod ask;
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
    should_regenerate, write_atomic, writer_identity, ArtifactCurrency, ArtifactFingerprint,
    ArtifactRecord, ArtifactStamp,
};
pub use ask::*;
pub use bounded::{
    bounded_reason, bounded_text, is_agent_unsafe, is_bidi_control, MAX_REASON_COMPONENT_BYTES,
};
pub use cancel::{cancelled_queries, Cancel, QueryCancelled};
pub use code_graph::{
    build_code_graph_value, build_graph_core_value, decode_compact, encode_compact,
    generate_code_graph_encodings, generate_code_graph_json, write_code_graph_atomically,
    write_verbose_code_graph_atomically, CODE_GRAPH_COMPACT_ENCODING, CODE_GRAPH_EXPORT_MAX_BYTES,
    CODE_GRAPH_SCHEMA_VERSION, CODE_GRAPH_TOP_LEVEL_KEYS, EDGE_KIND_LABELS,
};
pub use doc_drift::{DocDriftFinding, DocDriftReport, DriftKind};
pub use engine::{
    budget_take, clone_group_tokens, graph_core_for_store, graph_core_for_store_cancellable,
    is_test_path, link_candidates, parse_clone_kind, resolved_edge_from_stored, traversal_starts,
    traversed_resolution_edges, workspace_search, PathOutsideRepoRoot, QueryEngine,
    StoreQueryEngine, BYTES_PER_TOKEN, EVIDENCE_TEST_BUDGET_SHARE, EVIDENCE_TEST_DEPTH,
    MAX_NEIGHBOR_TARGETS, MAX_TOKEN_BUDGET, MAX_TRAVERSAL_DEPTH, PREVIEW_CALLER_MIN_CONFIDENCE,
};
pub use escape::{html_escape, render_symbol_label};
pub use evidence::{EvidenceFile, EvidencePack, EvidenceRole, EvidenceUnit};
pub use federate::{
    federate_impact, federate_impact_with_opts, federate_status, federate_status_with_opts,
    FederateOptions, FederatedDefinition, FederatedDegradedRepo, FederatedHitKind,
    FederatedImpactHit, FederatedImpactReport, FederatedRepoSpec, FederatedRepoStatus,
    FederatedStatusReport, FederatedWorkspaceManifest, DEFAULT_FEDERATE_LIMIT, MAX_FEDERATE_LIMIT,
};
pub use lean_tcb::{
    analyze_lean_tcb, audit_lean_tcb_tree, audit_lean_tcb_tree_with_mode, LeanKernelMode,
    LeanTcbDeclaration, LeanTcbFileAudit, LeanTcbFileDeclaration, LeanTcbFileHole, LeanTcbHole,
    LeanTcbHoleKind, LeanTcbReport, LeanTcbRole, LeanTcbSkippedPath, LeanTcbTreeReport,
    LeanVerificationTier, DEFAULT_MAX_LEAN_FILES,
};
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
pub use stacked::{
    base_generation_dir, bases_root_dir, gc_unused_bases, gc_unused_bases_waiting,
    legacy_base_store_anchor_root, provision_stacked_worktree, publish_base_snapshot,
    refuse_legacy_base_store_ref, seed_derived_artifacts_and_reuse_caches, BaseKey, BaseManifest,
    FileRecord, GcBasesReport, StackLease, StackedStatus, StackedStore, StackedSyncReport,
    SyncTarget, DEFAULT_REBASE_DELTA_BYTES, DEFAULT_REBASE_DIRTY_FILES, STACK_FORMAT_VERSION,
};

pub use cas::{blake3_hash, CasAstStore, CAS_TOMBSTONE};
pub use overlay::{OverlayEdge, RoaringBitmap, WorktreeOverlay};
pub use rung::{
    filter_by_rung, histogram as rung_histogram, LadderHistogram, ResolutionLadderTier, Rung,
    RungHistogram,
};
pub use scope::{
    missing_kind_message, NameQueryFilter, ResolvedScope, ScopeReport, SymbolScope,
    MAX_SCOPE_KINDS, MAX_SCOPE_LANGUAGES, MAX_SCOPE_PATHS,
};

/// Human-readable JSON type label (`"null"`, `"boolean"`, `"number"`, `"string"`,
/// `"array"`, `"object"`) for diagnostic messages.
pub fn json_value_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Indefinite-article form of [`json_value_kind`] (`"null"`, `"a boolean"`,
/// `"a number"`, `"a string"`, `"an array"`, `"an object"`).
pub fn json_value_kind_article(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod kind_tests {
    use super::{json_value_kind, json_value_kind_article};
    use serde_json::json;

    #[test]
    fn json_value_kind_covers_every_variant() {
        let cases = [
            (json!(null), "null", "null"),
            (json!(true), "boolean", "a boolean"),
            (json!(42), "number", "a number"),
            (json!("hi"), "string", "a string"),
            (json!([1]), "array", "an array"),
            (json!({"a": 1}), "object", "an object"),
        ];
        for (value, bare, article) in cases {
            assert_eq!(json_value_kind(&value), bare);
            assert_eq!(json_value_kind_article(&value), article);
        }
    }
}
