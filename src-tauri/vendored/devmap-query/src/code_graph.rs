//! `code_graph.json` — the symbol-level companion artifact to `repo_map.json`.
//!
//! The Python incumbent writes two files from one `dev map` run: the
//! file-level `repo_map.json` (see `manifest.rs`) and this symbol-level graph.
//! Eleven modules under `src/devcouncil/` read it, and `CLAUDE.md` names it as
//! the graph agents should consult, so the Rust kernel cannot replace the
//! Python mapper while emitting only half its output.
//!
//! The schema is taken from the live artifact and from
//! `src/devcouncil/indexing/graph/schema.py`, which is the pydantic model every
//! consumer validates against: unknown `NodeKind` or `Confidence` values make
//! `CodeGraph.model_validate` raise, so the mapping tables below are a
//! contract, not a convenience.
//!
//! **What this module refuses to do is as load-bearing as what it emits.** The
//! Rust kernel does not compute file-level reachability, does not persist an
//! edge's reason string, and produces no SHA-1 file fingerprints. Each of those
//! has a natural-looking zero value — an empty `unreachable_files`, an empty
//! `reason`, an empty `indexed_hash` — that reads to a consumer as a computed
//! answer. Every one is therefore paired with an explicit marker under
//! `meta.devmap_rust`, and `meta.liveness_unreachable_unreliable` is set
//! unconditionally, which is the flag `CLAUDE.md` already tells agents means
//! "ignore `unreachable_files`".

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::artifacts::write_atomic;
use crate::engine::{byte_span_to_line_range, resolve_source_path};
use crate::manifest::{entry_root_paths, is_entry_root, CONSUMER_MAP_ENGINE};
use crate::model::FreshnessInfo;
use devmap_analyze::model::{AnalysisStatus, AnalysisSummary};
use devmap_extract::model::{
    confidence_millis, EdgeKind, ExtractedSymbol, Extraction, SymbolKind, WiringKind,
};
use devmap_resolve::model::ResolvedEdge;
use serde_json::{json, Map, Value};

/// `SCHEMA_VERSION` in `src/devcouncil/indexing/graph/schema.py`.
pub const CODE_GRAPH_SCHEMA_VERSION: u32 = 2;

/// Consumer default path, relative to the indexed repository root.
pub const CODE_GRAPH_DEFAULT_OUTPUT: &str = ".devcouncil/graph/code_graph.json";

/// Milliconfidence floors for the Python tri-state `Confidence` enum.
///
/// Compared in milliconfidence rather than as `f32` for the reason
/// `Confidence::to_millis` exists: SQLite REAL cannot round-trip `f32` 0.9, so
/// an edge persisted at HIGH reads back as 0.89999997 and a `>= 0.9` float
/// comparison silently demotes every unique-global call to `inferred`.
const EXTRACTED_FLOOR_MILLIS: i64 = 900;
const INFERRED_FLOOR_MILLIS: i64 = 400;

/// Node kinds the Python `NodeKind` enum accepts.
///
/// Pinned as a list, not merely produced by the mapping function, so
/// `every_symbol_kind_maps_into_the_frozen_node_kind_set` can prove the mapping
/// stays inside it. A value outside this set does not degrade the artifact — it
/// makes `CodeGraph.model_validate` raise and every consumer lose the graph.
#[cfg(all(test, feature = "parse"))]
const PYTHON_NODE_KINDS: &[&str] = &[
    "file",
    "module",
    "namespace",
    "package",
    "function",
    "class",
    "method",
    "interface",
    "type",
    "struct",
    "enum",
    "trait",
    "property",
    "variable",
    "route",
    "event",
    "state",
    "provider",
    "component",
    "dynamic",
    "rationale",
];

/// Every `SymbolKind` the extractor can emit, as its Python `NodeKind` value.
fn node_kind_label(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::File => "file",
        SymbolKind::Module => "module",
        SymbolKind::Class => "class",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Interface => "interface",
        SymbolKind::Trait => "trait",
        SymbolKind::Function => "function",
        SymbolKind::Method => "method",
        // A field is a named member of a type, which is what Python's
        // `property` denotes. `variable` is the module-level bucket and would
        // lose the ownership the extractor recorded.
        SymbolKind::Field => "property",
        SymbolKind::Variable => "variable",
        SymbolKind::Route => "route",
        SymbolKind::Endpoint => "route",
        SymbolKind::EventSubscriber => "event",
        SymbolKind::Dependency => "package",
        SymbolKind::Subsystem => "namespace",
        SymbolKind::Community => "namespace",
    }
}

/// Rust `EdgeKind` as the edge-kind string Python consumers match on.
///
/// `GraphEdge.kind` is a free `str`, so this is not validated on import — which
/// makes it easier to get silently wrong. `Extends` becoming `inherits` is the
/// one non-identity rename and it is the name the Python resolver emits.
fn edge_kind_label(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Imports => "imports",
        EdgeKind::Calls => "calls",
        EdgeKind::Contains => "contains",
        EdgeKind::Defines => "defines",
        EdgeKind::Instantiates => "instantiates",
        EdgeKind::Extends => "inherits",
        EdgeKind::Implements => "implements",
        EdgeKind::SubscribesTo => "subscribes",
        EdgeKind::HandlesRoute => "routes_to",
        EdgeKind::WiredTo => "wired_to",
        EdgeKind::MemberOf => "member_of",
        EdgeKind::DependsOn => "depends_on",
        EdgeKind::TaintFlow => "taint_flow",
        EdgeKind::References => "references",
    }
}

/// Numeric confidence as Python's tri-state `Confidence`.
///
/// `DETERMINISTIC`/`HIGH` are the resolution ladder's evidence-backed rungs and
/// map to `extracted`; the `SPECULATIVE` fan-out of an ambiguous global is
/// exactly what `ambiguous` means, and must never read as `inferred` — a
/// consumer that trusts `inferred` edges as real callers is how a live symbol
/// stops looking dead for the wrong reason.
fn confidence_label(value: f32) -> &'static str {
    let millis = confidence_millis(value);
    if millis >= EXTRACTED_FLOOR_MILLIS {
        "extracted"
    } else if millis >= INFERRED_FLOOR_MILLIS {
        "inferred"
    } else {
        "ambiguous"
    }
}

/// Bucket label for a file, shared with `repo_map.json`'s `files[].area`.
fn file_area(path: &str) -> String {
    Path::new(path)
        .parent()
        .and_then(|parent| parent.to_str())
        .filter(|parent| !parent.is_empty() && *parent != ".")
        .unwrap_or(".")
        .replace('\\', "/")
}

/// A symbol's identity relative to its own file: `MyClass.execute`.
///
/// Mirrors `dead_symbol_identity` in `devmap-analyze`, which builds the
/// `symbol_name` carried on every `DeadSymbolReport`. The two must agree
/// because `dead_code[].id` is rebuilt as `file_path::symbol_name` and looked
/// up against the node ids produced here; `dead_code_ids_join_the_node_ids`
/// fails the moment they diverge.
fn relative_qualname(symbol: &ExtractedSymbol, file_path: &str) -> String {
    symbol
        .qualified_name
        .strip_prefix(file_path)
        .and_then(|rest| rest.strip_prefix("::"))
        .map(str::to_string)
        .unwrap_or_else(|| symbol.name.clone())
}

/// File path to community name, smallest name winning a tie.
///
/// `CommunityReport::members` is derived from clustering and a file can appear
/// in more than one report; picking by sorted name rather than by iteration
/// order is what keeps two cold builds byte-identical.
fn community_by_file(analysis: &AnalysisSummary) -> BTreeMap<&str, &str> {
    let mut ordered: Vec<&_> = analysis.communities.iter().collect();
    ordered.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.community_id.cmp(&right.community_id))
    });
    let mut by_file: BTreeMap<&str, &str> = BTreeMap::new();
    for community in ordered {
        for member in &community.members {
            by_file.entry(member.as_str()).or_insert(&community.name);
        }
    }
    by_file
}

/// Files nothing imports, that wiring does not explain.
///
/// Python's `file_liveness` answers this by discounting test-only importers, so
/// a module imported solely by its own test still reports unwired. The same
/// rule is applied here from the evidence the Rust store carries: an inbound
/// `Imports` edge counts only when its source file is not a `TestFile`.
///
/// Structurally exempt files are excluded outright — a test, a vendored
/// dependency or a generated file having no importer is its normal state, not a
/// finding.
fn unwired_candidates(extractions: &[Extraction], edges: &[ResolvedEdge]) -> Vec<String> {
    let test_files: BTreeSet<&str> = extractions
        .iter()
        .filter(|ext| ext.wiring.iter().any(|w| w.kind == WiringKind::TestFile))
        .map(|ext| ext.file_path.as_str())
        .collect();

    let mut imported_by_production: BTreeSet<&str> = BTreeSet::new();
    for edge in edges {
        if edge.edge_kind != EdgeKind::Imports || edge.source_file == edge.target_file {
            continue;
        }
        if test_files.contains(edge.source_file.as_str()) {
            continue;
        }
        imported_by_production.insert(edge.target_file.as_str());
    }

    let mut candidates: Vec<String> = extractions
        .iter()
        .filter(|ext| {
            !is_entry_root(ext)
                && !ext.wiring.iter().any(|w| {
                    matches!(
                        w.kind,
                        WiringKind::TestFile
                            | WiringKind::Vendored
                            | WiringKind::GeneratedFile
                            | WiringKind::ReExportPackage
                            | WiringKind::Launcher
                    )
                })
                && !imported_by_production.contains(ext.file_path.as_str())
        })
        .map(|ext| ext.file_path.clone())
        .collect();
    candidates.sort();
    candidates.dedup();
    candidates
}

/// Counters the artifact reports about its own completeness.
#[derive(Default)]
struct GraphProvenance {
    duplicate_node_ids_dropped: usize,
    duplicate_edges_dropped: usize,
    edge_endpoints_without_node: usize,
    files_without_readable_source: usize,
    dead_code_exempt_omitted: usize,
    dead_code_without_node: usize,
    dead_code_duplicates_dropped: usize,
}

/// Render `code_graph.json` from a committed generation.
///
/// Fails rather than emitting an empty-but-well-formed graph when there is no
/// generation behind it: a consumer reading `nodes: []` cannot tell an empty
/// repository from a store that was never built, and every liveness conclusion
/// drawn from the second is wrong.
pub fn generate_code_graph_json(
    extractions: &[Extraction],
    analysis: &AnalysisSummary,
    edges: &[ResolvedEdge],
    freshness: &FreshnessInfo,
    repo_root: Option<&str>,
) -> anyhow::Result<String> {
    if freshness.generation_id == 0 {
        anyhow::bail!("code graph unavailable: no committed generation (build a generation first)");
    }

    let mut provenance = GraphProvenance::default();
    let root = repo_root.map(str::to_string);
    let communities = community_by_file(analysis);

    let mut ordered: Vec<&Extraction> = extractions.iter().collect();
    ordered.sort_by(|left, right| left.file_path.cmp(&right.file_path));

    let mut nodes: Vec<Value> = Vec::new();
    // id -> (line, kind label), so `dead_code` can carry the line and kind its
    // schema declares instead of a zero that means nothing.
    let mut node_index: BTreeMap<String, (u32, &'static str)> = BTreeMap::new();

    for ext in &ordered {
        let source = std::fs::read_to_string(resolve_source_path(&root, &ext.file_path)).ok();
        if source.is_none() {
            provenance.files_without_readable_source += 1;
        }
        let area = file_area(&ext.file_path);
        let community = communities
            .get(ext.file_path.as_str())
            .copied()
            .unwrap_or_default();

        let mut symbols: Vec<&ExtractedSymbol> = ext.symbols.iter().collect();
        symbols.sort_by(|left, right| {
            left.qualified_name
                .cmp(&right.qualified_name)
                .then_with(|| left.span.start_byte.cmp(&right.span.start_byte))
                .then_with(|| left.span.end_byte.cmp(&right.span.end_byte))
        });

        for symbol in symbols {
            if node_index.contains_key(&symbol.qualified_name) {
                // SC14: nested and anonymous-scope definitions still collide on
                // identity. Two nodes sharing an id is worse than one — Python's
                // `node_by_id()` silently keeps the last, so every edge naming
                // that id would point at whichever copy won a dict insert.
                provenance.duplicate_node_ids_dropped += 1;
                continue;
            }

            let mut extras = Map::new();
            // A file node has no declaration line; Python emits 0/0 for one and
            // that is a fact about files, not an unknown.
            let (line, end_line) = if symbol.kind == SymbolKind::File {
                (0, 0)
            } else {
                match &source {
                    Some(text) => byte_span_to_line_range(text, &symbol.span),
                    None => {
                        // The span is bytes; without the file there is no line.
                        // Say so rather than reporting the top of the file.
                        extras.insert(
                            "line_resolution".to_string(),
                            Value::String(
                                "unavailable: source file could not be read from the \
                                 indexed repository root"
                                    .to_string(),
                            ),
                        );
                        (0, 0)
                    }
                }
            };

            if symbol.kind != SymbolKind::File {
                extras.insert(
                    "qualname".to_string(),
                    Value::String(relative_qualname(symbol, &ext.file_path)),
                );
            }

            let kind = node_kind_label(symbol.kind);
            node_index.insert(symbol.qualified_name.clone(), (line, kind));
            nodes.push(json!({
                "id": symbol.qualified_name,
                "kind": kind,
                "path": ext.file_path,
                "name": symbol.name,
                "line": line,
                "end_line": end_line,
                "area": area,
                "language": ext.language,
                "exported": symbol.is_exported,
                "community": community,
                "extras": Value::Object(extras),
            }));
        }
    }

    // (source, target, kind, confidence) — sorted so the artifact is stable and
    // deduplicated so a fan-out cannot report the same edge twice.
    let mut edge_keys: Vec<(&str, &str, &'static str, &'static str)> = edges
        .iter()
        .map(|edge| {
            (
                edge.source_symbol.as_str(),
                edge.target_symbol.as_str(),
                edge_kind_label(edge.edge_kind),
                confidence_label(edge.confidence.0),
            )
        })
        .collect();
    edge_keys.sort_unstable();
    let before_dedup = edge_keys.len();
    edge_keys.dedup();
    provenance.duplicate_edges_dropped = before_dedup - edge_keys.len();

    let mut edge_values: Vec<Value> = Vec::with_capacity(edge_keys.len());
    for (source, target, kind, confidence) in edge_keys {
        if !node_index.contains_key(source) || !node_index.contains_key(target) {
            provenance.edge_endpoints_without_node += 1;
        }
        edge_values.push(json!({
            "source": source,
            "target": target,
            "kind": kind,
            "confidence": confidence,
            // Not derivable: `generation_edges` persists no reason, and
            // `ResolvedEdge::resolution` is `None` on every edge read back from
            // the store. Python's own `compact` export tier blanks this field
            // for the same reason, so "" is a value consumers already handle.
            "reason": "",
            "extras": {},
        }));
    }

    let mut dead_rows: Vec<&_> = analysis
        .dead_symbols
        .iter()
        .filter(|report| {
            if report.is_exempt {
                provenance.dead_code_exempt_omitted += 1;
                false
            } else {
                true
            }
        })
        .collect();
    dead_rows.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.symbol_name.cmp(&right.symbol_name))
    });

    let mut dead_code: Vec<Value> = Vec::new();
    let mut dead_seen: BTreeSet<String> = BTreeSet::new();
    let mut legacy_dead: Vec<String> = Vec::new();
    for report in dead_rows {
        let id = format!("{}::{}", report.file_path, report.symbol_name);
        if !dead_seen.insert(id.clone()) {
            provenance.dead_code_duplicates_dropped += 1;
            continue;
        }
        let mut reason = report
            .exemption_reason
            .clone()
            .unwrap_or_else(|| "no inbound call edges and not exported".to_string());
        let (line, kind) = match node_index.get(&id) {
            Some((line, kind)) => (*line, *kind),
            None => {
                provenance.dead_code_without_node += 1;
                // The schema's defaults for these are 0 and "". Left bare they
                // are indistinguishable from a symbol genuinely at line 0, so
                // the reason carries the fact that they were never derived.
                reason.push_str(" [line and kind unavailable: no matching graph node]");
                (0, "")
            }
        };
        legacy_dead.push(id.clone());
        dead_code.push(json!({
            "id": id,
            "path": report.file_path,
            "line": line,
            "kind": kind,
            "confidence": confidence_label(report.confidence),
            "reason": reason,
        }));
    }

    let analysis_status = match &analysis.status {
        AnalysisStatus::Ok => "ok".to_string(),
        AnalysisStatus::Partial { reason } => format!("partial: {reason}"),
        AnalysisStatus::Timeout { reason } => format!("timeout: {reason}"),
    };

    let payload = json!({
        "schema_version": CODE_GRAPH_SCHEMA_VERSION,
        "nodes": nodes,
        "edges": edge_values,
        "dead_code": dead_code,
        "entry_roots": entry_root_paths(extractions),
        "unwired_candidates": unwired_candidates(extractions, edges),
        // Never computed. See `meta.devmap_rust.unavailable.unreachable_files`
        // and the unconditional `liveness_unreachable_unreliable` below.
        "unreachable_files": Vec::<String>::new(),
        "generated_head": freshness.head_sha,
        "indexed_hash": "",
        "content_fingerprint": "",
        "meta": {
            // Ownership marker. Python never writes this key, so its absence is
            // what identifies a foreign graph to the clobber guard.
            "map_engine": CONSUMER_MAP_ENGINE,
            // Unconditional: reachability was not computed, so an empty
            // `unreachable_files` must never be read as "everything is
            // reachable". `CLAUDE.md` already instructs agents to ignore that
            // list when this flag is set.
            "liveness_unreachable_unreliable": true,
            "legacy_dead_symbol_candidates": legacy_dead,
            "devmap_rust": {
                "engine": CONSUMER_MAP_ENGINE,
                "generation_id": freshness.generation_id,
                "analysis_status": analysis_status,
                "dead_code_scope": "non_exempt_only",
                "dead_code_exempt_omitted": provenance.dead_code_exempt_omitted,
                "dead_code_without_node": provenance.dead_code_without_node,
                "dead_code_duplicates_dropped": provenance.dead_code_duplicates_dropped,
                "duplicate_node_ids_dropped": provenance.duplicate_node_ids_dropped,
                "duplicate_edges_dropped": provenance.duplicate_edges_dropped,
                "edge_endpoints_without_node": provenance.edge_endpoints_without_node,
                "files_without_readable_source": provenance.files_without_readable_source,
                "unavailable": {
                    "unreachable_files":
                        "file-level reachability BFS is not implemented in the Rust \
                         kernel; the empty list is not a computed result",
                    "edge_reason":
                        "generation_edges persists no reason string, so every edge \
                         reports the empty reason Python's compact tier also uses",
                    "edge_extras":
                        "no per-edge extras are persisted",
                    "node_extras_bases_implements_decorators":
                        "the extractor records no base/interface/decorator lists on a \
                         symbol; the keys are omitted rather than emitted empty",
                    "indexed_hash":
                        "the Rust kernel computes no SHA-1 file-list digest; \
                         consumers treat the empty string as 'not fingerprinted' and \
                         skip the check rather than concluding freshness",
                    "content_fingerprint":
                        "the Rust kernel computes no SHA-1 size+mtime digest; see \
                         indexed_hash",
                },
            },
        },
    });

    Ok(serde_json::to_string_pretty(&payload)?)
}

/// Refuse to clobber a Python (or otherwise foreign) `code_graph.json` unless
/// `force` is set. Identity is `meta.map_engine == "devmap-rust"`; missing that
/// key is the live Python schema, which writes no engine marker at all.
pub fn write_code_graph_atomically(path: &Path, json: &str, force: bool) -> anyhow::Result<bool> {
    if path.exists() && !force && is_foreign_code_graph(path)? {
        anyhow::bail!(
            "refuse to overwrite a non-devmap-rust code graph at {} (pass --force to replace)",
            path.display()
        );
    }
    Ok(write_atomic(path, json.as_bytes())?)
}

fn is_foreign_code_graph(path: &Path) -> anyhow::Result<bool> {
    let existing = std::fs::read_to_string(path)?;
    let Ok(value) = serde_json::from_str::<Value>(&existing) else {
        // A check that could not run must never report the same result as a
        // check that ran and passed.
        return Ok(true);
    };
    Ok(value
        .get("meta")
        .and_then(|meta| meta.get("map_engine"))
        .and_then(Value::as_str)
        != Some(CONSUMER_MAP_ENGINE))
}

#[cfg(all(test, feature = "parse"))]
mod tests {
    use super::*;
    use devmap_analyze::model::{AnalysisStatus, CommunityReport, DeadSymbolReport};
    use devmap_extract::extract_file;
    use devmap_extract::model::Confidence;

    fn freshness() -> FreshnessInfo {
        FreshnessInfo {
            head_sha: "abc123".to_string(),
            generation_id: 1,
            pending_count: 0,
        }
    }

    fn analysis(dead: Vec<DeadSymbolReport>, communities: Vec<CommunityReport>) -> AnalysisSummary {
        AnalysisSummary {
            total_files: 0,
            total_symbols: 0,
            total_edges: 0,
            dead_symbols: dead,
            communities,
            status: AnalysisStatus::Ok,
            unresolved_calls: 0,
        }
    }

    fn empty_analysis() -> AnalysisSummary {
        analysis(Vec::new(), Vec::new())
    }

    fn edge(source: &str, target: &str, kind: EdgeKind, confidence: Confidence) -> ResolvedEdge {
        ResolvedEdge {
            source_file: source.split("::").next().unwrap_or(source).to_string(),
            target_file: target.split("::").next().unwrap_or(target).to_string(),
            source_symbol: source.to_string(),
            target_symbol: target.to_string(),
            edge_kind: kind,
            confidence,
            resolution: None,
            details: None,
        }
    }

    fn graph(
        extractions: &[Extraction],
        analysis: &AnalysisSummary,
        edges: &[ResolvedEdge],
    ) -> Value {
        let json =
            generate_code_graph_json(extractions, analysis, edges, &freshness(), None).unwrap();
        serde_json::from_str(&json).expect("code graph must be valid JSON")
    }

    /// A scratch directory holding a real source file, so line derivation has
    /// something to read.
    fn tmp_source_dir(name: &str, contents: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "devmap-cg-src-{}-{stamp}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), contents).unwrap();
        dir
    }

    fn tmp_graph(contents: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "devmap-code-graph-{}-{stamp}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("code_graph.json");
        std::fs::write(&path, contents).unwrap();
        path
    }

    /// The keys the Python `CodeGraph` model declares, all of them, exactly.
    ///
    /// This is the whole point of the artifact: `load_code_graph` runs
    /// `CodeGraph.model_validate(data)`, so a missing key falls back to a
    /// pydantic default that silently means something else, and eleven
    /// consumers read the result.
    #[test]
    fn top_level_keys_match_the_python_code_graph_model() {
        let value = graph(
            &[extract_file("k.py", "def a(): pass\n")],
            &empty_analysis(),
            &[],
        );
        let object = value.as_object().expect("graph is an object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "content_fingerprint",
                "dead_code",
                "edges",
                "entry_roots",
                "generated_head",
                "indexed_hash",
                "meta",
                "nodes",
                "schema_version",
                "unreachable_files",
                "unwired_candidates",
            ],
            "top-level keys must match schema.py's CodeGraph exactly"
        );
        assert_eq!(value["schema_version"], json!(CODE_GRAPH_SCHEMA_VERSION));
    }

    /// Node and dead-code entries carry every field their pydantic model
    /// declares, so no consumer silently reads a default.
    #[test]
    fn node_and_dead_code_entries_carry_every_declared_field() {
        let dead = vec![DeadSymbolReport {
            symbol_name: "a".to_string(),
            file_path: "k.py".to_string(),
            confidence: 0.9,
            is_exempt: false,
            exemption_reason: None,
        }];
        let value = graph(
            &[extract_file("k.py", "def a(): pass\n")],
            &analysis(dead, Vec::new()),
            &[],
        );

        let mut node_keys: Vec<&str> = value["nodes"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        node_keys.sort_unstable();
        assert_eq!(
            node_keys,
            [
                "area",
                "community",
                "end_line",
                "exported",
                "extras",
                "id",
                "kind",
                "language",
                "line",
                "name",
                "path",
            ],
            "GraphNode fields must match schema.py"
        );

        let mut dead_keys: Vec<&str> = value["dead_code"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        dead_keys.sort_unstable();
        assert_eq!(
            dead_keys,
            ["confidence", "id", "kind", "line", "path", "reason"],
            "DeadCodeEntry fields must match schema.py"
        );
    }

    /// Edge entries carry every field `GraphEdge` declares.
    ///
    /// `confidence` was omitted from the emitted object in the first draft and
    /// only the compiler's unused-variable warning caught it: every edge would
    /// have silently taken pydantic's `extracted` default, promoting every
    /// ambiguous fan-out edge to a deterministic one.
    #[test]
    fn edge_entries_carry_every_declared_field() {
        let value = graph(
            &[extract_file("k.py", "def a(): pass\n")],
            &empty_analysis(),
            &[edge(
                "k.py",
                "k.py::a",
                EdgeKind::Contains,
                Confidence::SPECULATIVE,
            )],
        );
        let entry = &value["edges"][0];
        let mut keys: Vec<&str> = entry
            .as_object()
            .expect("edge object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["confidence", "extras", "kind", "reason", "source", "target"],
            "GraphEdge fields must match schema.py"
        );
        assert_eq!(
            entry["confidence"], "ambiguous",
            "the edge's own confidence must reach the artifact, not pydantic's \
             `extracted` default: {entry}"
        );
    }

    /// Every `SymbolKind` lands on a value Python's `NodeKind` enum accepts.
    ///
    /// An unknown value is not a cosmetic defect: `CodeGraph.model_validate`
    /// raises on it and `load_code_graph` swallows the exception, so all eleven
    /// consumers lose the graph entirely and none of them says why.
    #[test]
    fn every_symbol_kind_maps_into_the_frozen_node_kind_set() {
        for kind in [
            SymbolKind::File,
            SymbolKind::Module,
            SymbolKind::Class,
            SymbolKind::Struct,
            SymbolKind::Enum,
            SymbolKind::Interface,
            SymbolKind::Trait,
            SymbolKind::Function,
            SymbolKind::Method,
            SymbolKind::Field,
            SymbolKind::Variable,
            SymbolKind::Route,
            SymbolKind::Endpoint,
            SymbolKind::EventSubscriber,
            SymbolKind::Dependency,
            SymbolKind::Subsystem,
            SymbolKind::Community,
        ] {
            let label = node_kind_label(kind);
            assert!(
                PYTHON_NODE_KINDS.contains(&label),
                "{kind:?} maps to {label:?}, which Python's NodeKind does not accept"
            );
        }
    }

    /// The confidence ladder is tri-state and its floors are the ones that
    /// separate the resolver's rungs.
    ///
    /// A speculative fan-out edge reading as `inferred` is the dangerous
    /// direction: it turns an ambiguous guess into something a consumer treats
    /// as a real caller.
    #[test]
    fn confidence_maps_the_resolution_ladder_onto_the_python_tri_state() {
        assert_eq!(confidence_label(Confidence::DETERMINISTIC.0), "extracted");
        assert_eq!(confidence_label(Confidence::HIGH.0), "extracted");
        assert_eq!(confidence_label(Confidence::MEDIUM.0), "inferred");
        assert_eq!(confidence_label(Confidence::LOW.0), "inferred");
        assert_eq!(confidence_label(Confidence::SPECULATIVE.0), "ambiguous");
        // The floor is milliconfidence, not a raw float compare. A value
        // fractionally under 0.9 that rounds to 900 millis is `extracted`; one
        // that rounds to 899 is not. A `value >= 0.9` implementation gets the
        // first of these wrong, which silently demotes persisted HIGH edges.
        assert_eq!(confidence_label(0.8996), "extracted");
        assert_eq!(confidence_label(0.8994), "inferred");
        assert_eq!(confidence_label(0.3996), "inferred");
        assert_eq!(confidence_label(0.3994), "ambiguous");
    }

    /// Nodes, edges and dead code all come out in a stable, sorted order.
    ///
    /// The determinism gate compares digests across two cold builds, so any
    /// collection reaching the artifact in hash order fails it — and by then
    /// the artifact has already been published.
    #[test]
    fn collections_are_emitted_in_a_stable_sorted_order() {
        let extractions = vec![
            extract_file("z.py", "def z1(): pass\ndef z2(): pass\n"),
            extract_file("a.py", "def a1(): pass\n"),
            extract_file("m.py", "def m1(): pass\n"),
        ];
        let dead = vec![
            DeadSymbolReport {
                symbol_name: "z2".to_string(),
                file_path: "z.py".to_string(),
                confidence: 0.9,
                is_exempt: false,
                exemption_reason: None,
            },
            DeadSymbolReport {
                symbol_name: "a1".to_string(),
                file_path: "a.py".to_string(),
                confidence: 0.9,
                is_exempt: false,
                exemption_reason: None,
            },
        ];
        let edges = vec![
            edge("z.py::z1", "a.py::a1", EdgeKind::Calls, Confidence::HIGH),
            edge("a.py::a1", "m.py::m1", EdgeKind::Calls, Confidence::HIGH),
            edge(
                "m.py",
                "m.py::m1",
                EdgeKind::Contains,
                Confidence::DETERMINISTIC,
            ),
        ];

        let value = graph(&extractions, &analysis(dead, Vec::new()), &edges);

        let node_ids: Vec<&str> = value["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap())
            .collect();
        let mut sorted = node_ids.clone();
        sorted.sort_unstable();
        assert_eq!(node_ids, sorted, "nodes must be emitted in sorted id order");

        let edge_keys: Vec<(&str, &str)> = value["edges"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| (e["source"].as_str().unwrap(), e["target"].as_str().unwrap()))
            .collect();
        let mut sorted_edges = edge_keys.clone();
        sorted_edges.sort_unstable();
        assert_eq!(edge_keys, sorted_edges, "edges must be emitted sorted");

        let dead_ids: Vec<&str> = value["dead_code"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["id"].as_str().unwrap())
            .collect();
        assert_eq!(dead_ids, ["a.py::a1", "z.py::z2"]);
    }

    /// The same inputs produce the identical string, twice.
    #[test]
    fn two_renders_of_one_generation_are_byte_identical() {
        let extractions: Vec<Extraction> = (0..40)
            .map(|i| extract_file(&format!("mod{i:03}.py"), "def f(): pass\ndef g(): f()\n"))
            .collect();
        let communities = vec![
            CommunityReport {
                community_id: 1,
                name: "beta".to_string(),
                members: (0..20).map(|i| format!("mod{i:03}.py")).collect(),
                cohesion_score: 0.5,
            },
            // Overlapping membership: the winner must be chosen by sorted name,
            // not by iteration order.
            CommunityReport {
                community_id: 0,
                name: "alpha".to_string(),
                members: (0..40).map(|i| format!("mod{i:03}.py")).collect(),
                cohesion_score: 0.5,
            },
        ];
        let summary = analysis(Vec::new(), communities);
        let first =
            generate_code_graph_json(&extractions, &summary, &[], &freshness(), None).unwrap();
        let second =
            generate_code_graph_json(&extractions, &summary, &[], &freshness(), None).unwrap();
        assert_eq!(first, second, "two renders must be byte-identical");

        let value: Value = serde_json::from_str(&first).unwrap();
        assert_eq!(
            value["nodes"][0]["community"], "alpha",
            "an overlapping community membership resolves by sorted name"
        );
    }

    /// No committed generation is an error, never an empty graph.
    ///
    /// `nodes: []` from an unbuilt store is indistinguishable from an empty
    /// repository, and every liveness answer drawn from the second is wrong.
    #[test]
    fn an_uncommitted_generation_fails_instead_of_emitting_an_empty_graph() {
        let unbuilt = FreshnessInfo {
            head_sha: "abc123".to_string(),
            generation_id: 0,
            pending_count: 0,
        };
        let error = generate_code_graph_json(
            &[extract_file("k.py", "def a(): pass\n")],
            &empty_analysis(),
            &[],
            &unbuilt,
            None,
        )
        .expect_err("an unbuilt store must not render a graph");
        assert!(
            error.to_string().contains("no committed generation"),
            "the error must name the cause: {error}"
        );
    }

    /// An empty `unreachable_files` is always flagged as uncomputed.
    ///
    /// The Rust kernel runs no reachability BFS. Emitting `[]` with no marker
    /// tells a consumer every file is reachable, which is a stronger claim than
    /// "we did not look" and the exact confusion this repository forbids.
    #[test]
    fn unreachable_files_is_never_presented_as_a_computed_result() {
        let value = graph(
            &[extract_file("k.py", "def a(): pass\n")],
            &empty_analysis(),
            &[],
        );
        assert_eq!(value["unreachable_files"], json!([]));
        assert_eq!(
            value["meta"]["liveness_unreachable_unreliable"],
            json!(true),
            "the empty list must always be marked unreliable"
        );
        assert!(
            value["meta"]["devmap_rust"]["unavailable"]["unreachable_files"]
                .as_str()
                .is_some_and(|reason| reason.contains("not implemented")),
            "the reason the list is empty must be stated"
        );
    }

    /// Exempt dead rows are omitted, and the omission is counted.
    ///
    /// An exempt symbol is explicitly *not* a candidate — listing it would
    /// propose deleting working code. Dropping it silently would present a
    /// filtered list as the complete one.
    #[test]
    fn exempt_dead_rows_are_omitted_and_the_count_is_reported() {
        let dead = vec![
            DeadSymbolReport {
                symbol_name: "live".to_string(),
                file_path: "k.py".to_string(),
                confidence: 0.3,
                is_exempt: true,
                exemption_reason: Some("Go init function".to_string()),
            },
            DeadSymbolReport {
                symbol_name: "gone".to_string(),
                file_path: "k.py".to_string(),
                confidence: 0.9,
                is_exempt: false,
                exemption_reason: None,
            },
        ];
        let value = graph(
            &[extract_file("k.py", "def gone(): pass\n")],
            &analysis(dead, Vec::new()),
            &[],
        );

        let ids: Vec<&str> = value["dead_code"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["k.py::gone"], "an exempt symbol is not a candidate");
        assert_eq!(
            value["meta"]["devmap_rust"]["dead_code_exempt_omitted"],
            json!(1),
            "the filtered rows must be counted, never silently dropped"
        );
        assert_eq!(
            value["meta"]["legacy_dead_symbol_candidates"],
            json!(["k.py::gone"]),
            "repo_mapper reads this key to build dead_symbol_candidates"
        );
    }

    /// A dead-code id joins the node ids, and a dead row with no node says so.
    ///
    /// `dead_code[].id` is rebuilt from `file_path::symbol_name`; if that rule
    /// drifts from the node identity the entries silently stop resolving, and
    /// `line`/`kind` quietly become 0 and "" for every row.
    #[test]
    fn dead_code_ids_join_the_node_ids_and_a_miss_is_explicit() {
        let dead = vec![
            DeadSymbolReport {
                symbol_name: "gone".to_string(),
                file_path: "k.py".to_string(),
                confidence: 0.9,
                is_exempt: false,
                exemption_reason: None,
            },
            DeadSymbolReport {
                symbol_name: "ghost".to_string(),
                file_path: "k.py".to_string(),
                confidence: 0.9,
                is_exempt: false,
                exemption_reason: None,
            },
        ];
        let source = "def a(): pass\ndef gone(): pass\n";
        let dir = tmp_source_dir("k.py", source);
        let json = generate_code_graph_json(
            &[extract_file("k.py", source)],
            &analysis(dead, Vec::new()),
            &[],
            &freshness(),
            Some(dir.to_str().unwrap()),
        )
        .unwrap();
        let value: Value = serde_json::from_str(&json).unwrap();

        let rows = value["dead_code"].as_array().unwrap();
        let joined = rows
            .iter()
            .find(|d| d["id"] == "k.py::gone")
            .expect("the real symbol must be present");
        assert_eq!(joined["kind"], "function", "a joined row carries its kind");
        assert_eq!(
            joined["line"], 2,
            "a joined row carries the symbol's real line"
        );
        assert!(
            !joined["reason"].as_str().unwrap().contains("unavailable"),
            "a joined row must not claim anything is unavailable"
        );

        let ghost = rows
            .iter()
            .find(|d| d["id"] == "k.py::ghost")
            .expect("the unmatched row is still reported");
        assert_eq!(ghost["line"], 0);
        assert_eq!(ghost["kind"], "");
        assert!(
            ghost["reason"]
                .as_str()
                .unwrap()
                .contains("line and kind unavailable"),
            "a zero line must never be indistinguishable from a derived one: {ghost}"
        );
        assert_eq!(
            value["meta"]["devmap_rust"]["dead_code_without_node"],
            json!(1)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Symbol nodes carry real 1-based lines; a file node has none by nature.
    #[test]
    fn symbol_nodes_carry_real_lines_from_their_byte_spans() {
        let source = "def first():\n    pass\n\n\ndef second():\n    pass\n";
        let dir = tmp_source_dir("k.py", source);

        let json = generate_code_graph_json(
            &[extract_file("k.py", source)],
            &empty_analysis(),
            &[],
            &freshness(),
            Some(dir.to_str().unwrap()),
        )
        .unwrap();
        let value: Value = serde_json::from_str(&json).unwrap();
        let nodes = value["nodes"].as_array().unwrap();

        let file_node = nodes.iter().find(|n| n["kind"] == "file").unwrap();
        assert_eq!(file_node["line"], 0, "a file has no declaration line");
        assert!(
            file_node["extras"]
                .as_object()
                .unwrap()
                .get("line_resolution")
                .is_none(),
            "a file's zero line is a fact, not an unknown"
        );

        let second = nodes.iter().find(|n| n["name"] == "second").unwrap();
        assert_eq!(second["line"], 5, "lines are 1-based from the byte span");
        assert_eq!(second["end_line"], 6);
        assert_eq!(second["extras"]["qualname"], "second");
        assert_eq!(
            value["meta"]["devmap_rust"]["files_without_readable_source"],
            json!(0)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A symbol whose source cannot be read reports no line and says why.
    #[test]
    fn an_unreadable_source_reports_an_explicit_unknown_line() {
        // No repo root and no such file on disk, so the read fails.
        let value = graph(
            &[extract_file(
                "definitely/not/on/disk.py",
                "def first(): pass\ndef second(): pass\n",
            )],
            &empty_analysis(),
            &[],
        );
        let symbol = value["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["name"] == "second")
            .unwrap();
        assert_eq!(symbol["line"], 0);
        assert!(
            symbol["extras"]["line_resolution"]
                .as_str()
                .is_some_and(|reason| reason.starts_with("unavailable:")),
            "a zero line with no source must be marked unavailable: {symbol}"
        );
        assert_eq!(
            value["meta"]["devmap_rust"]["files_without_readable_source"],
            json!(1)
        );
    }

    /// Duplicate node ids collapse to one node, and the collapse is counted.
    #[test]
    fn duplicate_node_ids_collapse_to_one_and_are_counted() {
        let mut extraction = extract_file("k.py", "def a(): pass\n");
        let duplicate = extraction
            .symbols
            .iter()
            .find(|s| s.kind != SymbolKind::File)
            .expect("a symbol to duplicate")
            .clone();
        extraction.symbols.push(duplicate);

        let value = graph(&[extraction], &empty_analysis(), &[]);
        let ids: Vec<&str> = value["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap())
            .collect();
        let unique: BTreeSet<&str> = ids.iter().copied().collect();
        assert_eq!(ids.len(), unique.len(), "node ids must be unique: {ids:?}");
        assert_eq!(
            value["meta"]["devmap_rust"]["duplicate_node_ids_dropped"],
            json!(1)
        );
    }

    /// Edge kinds are renamed to the strings Python's resolver emits.
    #[test]
    fn edge_kinds_use_the_python_vocabulary() {
        assert_eq!(edge_kind_label(EdgeKind::Extends), "inherits");
        assert_eq!(edge_kind_label(EdgeKind::Contains), "contains");
        assert_eq!(edge_kind_label(EdgeKind::Calls), "calls");
        assert_eq!(edge_kind_label(EdgeKind::Imports), "imports");
        assert_eq!(edge_kind_label(EdgeKind::HandlesRoute), "routes_to");
        assert_eq!(edge_kind_label(EdgeKind::SubscribesTo), "subscribes");
    }

    /// A file imported only by a test still counts as unwired, and an entry
    /// root never does.
    ///
    /// Discounting test-only importers is Python's rule. Without it, adding a
    /// test to an otherwise-unused module makes it disappear from the list that
    /// exists to find exactly that module.
    #[test]
    fn unwired_discounts_test_only_importers_and_spares_entry_roots() {
        let lib = extract_file("lib.py", "def f(): pass\n");
        let mut test = extract_file("test_lib.py", "import lib\n");
        test.wiring.push(devmap_extract::model::WiringAnnotation {
            kind: WiringKind::TestFile,
            target_symbol: "test_lib.py".to_string(),
            details: "test file".to_string(),
        });
        let mut main = extract_file("main.py", "def main(): pass\n");
        main.wiring.push(devmap_extract::model::WiringAnnotation {
            kind: WiringKind::ScriptEntry,
            target_symbol: "main.py".to_string(),
            details: "entry".to_string(),
        });
        let used = extract_file("used.py", "def g(): pass\n");
        let app = extract_file("app.py", "import used\n");

        let edges = vec![
            edge("test_lib.py", "lib.py", EdgeKind::Imports, Confidence::HIGH),
            edge("app.py", "used.py", EdgeKind::Imports, Confidence::HIGH),
        ];
        let value = graph(&[lib, test, main, used, app], &empty_analysis(), &edges);

        let unwired: Vec<&str> = value["unwired_candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(
            unwired.contains(&"lib.py"),
            "a module imported only by its own test is unwired: {unwired:?}"
        );
        assert!(
            !unwired.contains(&"used.py"),
            "a module a production file imports is wired: {unwired:?}"
        );
        assert!(
            !unwired.contains(&"main.py"),
            "an entry root is never unwired: {unwired:?}"
        );
        assert!(
            !unwired.contains(&"test_lib.py"),
            "a test file having no importer is its normal state: {unwired:?}"
        );
        assert_eq!(value["entry_roots"], json!(["main.py"]));
    }

    /// The guard that stops devmap overwriting the Python graph, both ways.
    #[test]
    fn foreign_code_graph_detection_distinguishes_both_directions() {
        let ours = tmp_graph(&format!(
            r#"{{"meta": {{"map_engine": "{CONSUMER_MAP_ENGINE}"}}}}"#
        ));
        assert!(
            !is_foreign_code_graph(&ours).unwrap(),
            "a graph devmap wrote itself must be refreshable"
        );

        // The live Python artifact: a real meta dict, with no engine marker.
        let python =
            tmp_graph(r#"{"schema_version": 2, "nodes": [], "meta": {"parse_cache_version": 8}}"#);
        assert!(
            is_foreign_code_graph(&python).unwrap(),
            "the Python-written graph must be protected from being clobbered"
        );

        let no_meta = tmp_graph(r#"{"schema_version": 2, "nodes": []}"#);
        assert!(
            is_foreign_code_graph(&no_meta).unwrap(),
            "no meta at all is foreign"
        );

        let broken = tmp_graph("not json {{{");
        assert!(
            is_foreign_code_graph(&broken).unwrap(),
            "unreadable content must fail closed"
        );

        // And the guard is actually applied by the writer.
        let err = write_code_graph_atomically(&python, "{}", false)
            .expect_err("a foreign graph must not be clobbered");
        assert!(err.to_string().contains("--force"), "{err}");
        assert!(
            write_code_graph_atomically(&python, r#"{"replaced": true}"#, true).unwrap(),
            "--force must replace it"
        );

        for path in [ours, python, no_meta, broken] {
            let _ = std::fs::remove_dir_all(path.parent().unwrap());
        }
    }
}
