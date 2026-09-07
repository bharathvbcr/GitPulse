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
use crate::engine::{byte_span_to_line_range_in, resolve_source_path};
use crate::manifest::{entry_root_paths, is_entry_root, CONSUMER_MAP_ENGINE};
use crate::model::FreshnessInfo;
use devmap_analyze::model::{AnalysisStatus, AnalysisSummary};
use devmap_extract::languages::Capability;
use devmap_extract::model::{
    confidence_millis, EdgeKind, ExtractedSymbol, Extraction, LineIndex, ParseOutcome, SymbolKind,
    WiringKind,
};
use devmap_resolve::model::ResolvedEdge;
use serde_json::{json, Map, Value};

/// Every top-level key `code_graph.json` carries, sorted.
///
/// The contract with `schema.py`'s `CodeGraph`, declared once. It was written
/// out by hand in three places — this module's own test, the CLI artifact test,
/// and the pydantic model — and adding `dead_clusters_incomplete` to the writer
/// broke two of them separately, which is the drift this list exists to end.
///
/// Additive by convention: pydantic ignores unknown keys, so a consumer that
/// predates a key reads the artifact unchanged and `CODE_GRAPH_SCHEMA_VERSION`
/// does not move. What must not happen is a key on one side and not the other,
/// because *that* is silent — the field simply loads as its default, and for
/// `dead_clusters` that default said "the component pass never ran".
pub const CODE_GRAPH_TOP_LEVEL_KEYS: &[&str] = &[
    "content_fingerprint",
    "dead_clusters",
    "dead_clusters_incomplete",
    "dead_clusters_truncated",
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
];

/// `SCHEMA_VERSION` in `src/devcouncil/indexing/graph/schema.py`.
pub const CODE_GRAPH_SCHEMA_VERSION: u32 = 2;

// The consumer default paths that used to live here — `CODE_GRAPH_DEFAULT_OUTPUT`
// and `CODE_GRAPH_COMPACT_DEFAULT_OUTPUT` — are gone rather than updated. Which
// directory holds an artifact is now resolved per repository by
// `devmap_extract::paths`, so a fixed relative string could only be right for
// one of the two layouts, and a caller reading it would write the graph
// somewhere the reader of that repository does not look. Use
// `devmap_extract::paths::code_graph_path` / `compact_code_graph_path`.

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

/// Every edge-kind string this writer can emit, in the order of [`EdgeKind`].
///
/// `pub` so a query surface can validate a caller's relationship name against
/// exactly what the emitter produces. Restating the list somewhere else is how
/// `EXTENDS` came to be an accepted relationship type in the Python Cypher
/// subset while the graph emitted `inherits`: the query parsed, the name was
/// "supported", and it matched zero edges — reported as an empty result rather
/// than as a name nothing can match.
pub const EDGE_KIND_LABELS: &[&str] = &[
    "imports",
    "calls",
    "contains",
    "defines",
    "instantiates",
    "inherits",
    "implements",
    "subscribes",
    "routes_to",
    "wired_to",
    "member_of",
    "depends_on",
    "taint_flow",
    "references",
];

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
pub(crate) fn confidence_label(value: f32) -> &'static str {
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
/// up against the node ids produced here;
/// `dead_code_ids_join_the_node_ids_and_a_miss_is_explicit` fails the moment
/// they diverge.
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
///
/// So is a file whose imports were never extracted. "Nothing imports it" is
/// only evidence when imports were looked for, which is the same sentence
/// `analyze_liveness` already applies to a parse-failed file's own symbols
/// (X6). Without this, a file the extractor refused reported as unwired next to
/// genuinely orphaned modules, under an `{shown, total, truncated}` triple that
/// was arithmetically honest about a population that was not — and the count of
/// what the filter removed travels with the list for exactly that reason.
pub(crate) struct UnwiredScan {
    pub(crate) paths: Vec<String>,
    /// Files dropped because their own imports were never extracted.
    ///
    /// Reported rather than silently subtracted: a filtered list under a bare
    /// total is how "we did not look" comes to read as "we looked and found
    /// nothing".
    pub(crate) excluded_coverage_loss: usize,
    /// Files dropped because **no importer could ever have been seen** — their
    /// language has no import extractor in this build.
    ///
    /// The largest false-positive surface the kernel had. `unwired_candidates`
    /// asks whether a file has an inbound `Imports` edge from a non-test file,
    /// and there are only five `imports.push` sites in the entire extractor:
    /// Python, JS/TS/TSX, Rust `use`, Go `import_spec`, and the embedded-script
    /// merge. For the other 24 of 35 languages the answer was structurally
    /// always no, so in a Java, C++, Ruby, Swift or C# repository *every*
    /// non-entry-root, non-exempt file was reported unwired — and these files
    /// parse `Clean`, so `excluded_coverage_loss` above never fired for them.
    /// Capped at 200, the agent saw 200 confidently-wrong filenames.
    ///
    /// Counted apart from `excluded_coverage_loss` rather than added to it,
    /// because the two are different facts with different remedies: one is a
    /// file this run could not read and a re-index might, the other is a
    /// language this build cannot read imports for and no re-run will change.
    pub(crate) excluded_import_blind: usize,
}

/// Whether any dynamic reference in the corpus names this file.
///
/// Three spellings, because a specifier does not have to name a file the way
/// the filesystem does:
///
/// * the path as written — `import('./src/App.tsx')`;
/// * the path without its extension — `import('./src/App')`, and `pkg.mod`
///   arriving as the form `pkg/mod` for `pkg/mod.py`;
/// * the package directory, for `importlib.import_module("pkg")` reaching
///   `pkg/__init__.py`.
fn reached_dynamically(path: &str, forms: &BTreeSet<&str>) -> bool {
    if forms.contains(path) {
        return true;
    }
    if let Some((stem, _)) = path.rsplit_once('.') {
        if forms.contains(stem) {
            return true;
        }
    }
    for init in [
        "/__init__.py",
        "/index.ts",
        "/index.tsx",
        "/index.js",
        "/mod.rs",
    ] {
        if let Some(package) = path.strip_suffix(init) {
            if forms.contains(package) {
                return true;
            }
        }
    }
    false
}

/// Whether one resolved edge is evidence that another file depends on this one.
///
/// An `Imports` edge is not the only such evidence, and treating it as the only
/// one was a defect that stayed invisible while most languages had no import
/// extraction at all. It surfaced the moment they did: `Main.java` reaching
/// `new Helper().run()` produces a resolved `References` edge and no import,
/// because **Java files in one package import each other not at all**. Under
/// the import-only rule `Helper.java` went straight from "excluded, because
/// nothing could have imported it" to "reported, because nothing imported it" —
/// the same wrong answer with a new reason, on a file whose caller is sitting
/// right there in the graph.
///
/// The same shape is ordinary in C++ (a `.cpp` defining what a header
/// declares), Go (one package, no imports), C#, Swift and Kotlin. So the
/// question this scan asks is "does anything depend on this file", and every
/// resolved cross-file dependency edge answers it.
///
/// Two restrictions keep the widening honest:
///
/// * **Structural edges are not dependencies.** `Contains`, `Defines` and
///   `MemberOf` record that a file holds its own symbols. Cross-file instances
///   exist for re-exports, and counting them would let a file's own declaration
///   wire it to itself through a third party.
/// * **A guess is not evidence.** An ambiguous call fans out to as many as
///   `AMBIGUOUS_FANOUT_CAP` candidate files of which at most one is right, so
///   accepting it would mark up to fifteen files wired on the strength of a
///   name collision. Only the confident tier counts — the same line
///   `EXTRACTED_FLOOR_MILLIS` draws for a dead-code verdict, and for the same
///   reason: this scan's output is also a delete-this suggestion.
fn is_wiring_evidence(edge: &ResolvedEdge) -> bool {
    if matches!(
        edge.edge_kind,
        EdgeKind::Contains | EdgeKind::Defines | EdgeKind::MemberOf
    ) {
        return false;
    }
    // `Imports` is textual and exact: the specifier was written in the source
    // and resolved to an indexed path, so it carries no ambiguity to score.
    // Every other kind must clear the confident tier.
    //
    // Compared in milliconfidence, not as `f32`, for the reason
    // `EXTRACTED_FLOOR_MILLIS` above records: an edge persisted at HIGH reads
    // back from SQLite as 0.89999997, so a `>= 0.9` float comparison would
    // silently stop counting every unique-global call as wiring on a store
    // round trip while counting it in-process.
    edge.edge_kind == EdgeKind::Imports
        || confidence_millis(edge.confidence.0) >= EXTRACTED_FLOOR_MILLIS
}

/// Whether this file sits in a Go package something imports.
///
/// A Go package is a directory, and an import names the package, not a file —
/// there is no statement a Go author could write that would name
/// `store/helpers.go` specifically. So the inbound edge lands on the synthetic
/// package node and the files behind it are reachable with nothing pointing at
/// them, which is the same shape as the same-package Java case and needs the
/// same answer: the question is whether anything depends on this file, and
/// something depends on the package it constitutes.
///
/// Matched on the file's own directory, not on a prefix: a package does not
/// include its subdirectories, and treating `app/store` as covering
/// `app/store/internal/x.go` would exempt a genuinely stranded file one level
/// down.
fn file_is_in_imported_go_package(ext: &Extraction, imported: &BTreeSet<&str>) -> bool {
    if ext.language != "go" || imported.is_empty() {
        return false;
    }
    let directory = match ext.file_path.rsplit_once('/') {
        Some((directory, _file)) => directory,
        None => "",
    };
    imported.contains(directory)
}

pub(crate) fn unwired_candidates(
    extractions: &[Extraction],
    edges: &[ResolvedEdge],
) -> UnwiredScan {
    // Files whose *import* of something is not evidence that a human wired it.
    //
    // `TestFile` was here alone, and the same sentence is true of the other two
    // — more strongly, if anything. A test importing a module is a real
    // dependency that says nothing about production wiring; a **vendored**
    // bundle importing one is a third party's dependency in a tree this
    // repository does not author, and a **generated** file's import was written
    // by a code generator from a spec, not by anyone deciding the module should
    // exist. Either one silently cleared a genuinely stranded module: one
    // vendored blob with a broad import surface can mark half a tree wired.
    //
    // Pre-existing, and `is_wiring_evidence` widened the surface it applies to,
    // which is what makes it worth closing now rather than noting.
    //
    // Only the *source* side is filtered. A vendored file can still be an
    // unwired candidate itself — that question is answered by the
    // `WiringKind::Vendored` exemption further down, on its own grounds.
    let non_authoring_importers: BTreeSet<&str> = extractions
        .iter()
        .filter(|ext| {
            ext.wiring.iter().any(|w| {
                matches!(
                    w.kind,
                    WiringKind::TestFile | WiringKind::Vendored | WiringKind::GeneratedFile
                )
            })
        })
        .map(|ext| ext.file_path.as_str())
        .collect();
    // Kept under its old name for the two later reads that mean exactly "a
    // test", so widening this set could not silently widen those.
    let test_files: BTreeSet<&str> = extractions
        .iter()
        .filter(|ext| ext.wiring.iter().any(|w| w.kind == WiringKind::TestFile))
        .map(|ext| ext.file_path.as_str())
        .collect();

    let mut depended_on_by_production: BTreeSet<&str> = BTreeSet::new();
    // Directories named by a Go package import. `go_import_edge_targets`
    // collapses an `import "app/store"` onto one synthetic
    // `package:app/store/store` node instead of one edge per file, so the files
    // in an imported package have **no inbound file-level edge at all** and
    // every one of them was reported as unwired. Measured on this repository:
    // nine of the eleven remaining Go candidates were files in packages that
    // other packages import.
    //
    // The collapse itself is right — it is what keeps a 200-file package from
    // fanning one import into 200 edges — so this reads the node back rather
    // than undoing it.
    let mut imported_go_packages: BTreeSet<&str> = BTreeSet::new();
    for edge in edges {
        if !is_wiring_evidence(edge) || edge.source_file == edge.target_file {
            continue;
        }
        if non_authoring_importers.contains(edge.source_file.as_str()) {
            continue;
        }
        if let Some(package_node) = edge.target_file.strip_prefix("package:") {
            // `package:<dir>/<pkg>` — the package name is the last segment and
            // the directory is what precedes it.
            if let Some((directory, _package)) = package_node.rsplit_once('/') {
                imported_go_packages.insert(directory);
            }
            continue;
        }
        depended_on_by_production.insert(edge.target_file.as_str());
    }

    // W3.3: files a *dynamic* reference reaches, which no import edge records.
    //
    // A lazily imported plugin, a code-split route and a worker entry point are
    // all reachable and all invisible to the edge walk above. The Python wiring
    // module has cleared them since it was written; the kernel did not, so it
    // called them unwired on every build. Test files are skipped here for the
    // same reason they are skipped for import edges: a reference from a test is
    // not production wiring.
    let dynamic_forms: BTreeSet<&str> = extractions
        .iter()
        .filter(|ext| !test_files.contains(ext.file_path.as_str()))
        .flat_map(|ext| ext.wiring.iter())
        .filter(|wiring| wiring.kind == WiringKind::DynamicImport)
        .map(|wiring| wiring.target_symbol.as_str())
        .collect();

    let mut excluded_coverage_loss = 0usize;
    let mut excluded_import_blind = 0usize;
    let mut candidates: Vec<String> = extractions
        .iter()
        .filter(|ext| {
            if is_entry_root(ext)
                || ext.wiring.iter().any(|w| {
                    matches!(
                        w.kind,
                        WiringKind::TestFile
                            | WiringKind::Vendored
                            | WiringKind::GeneratedFile
                            | WiringKind::ReExportPackage
                            | WiringKind::Launcher
                            | WiringKind::AllowUnwired
                    )
                })
                || depended_on_by_production.contains(ext.file_path.as_str())
                || file_is_in_imported_go_package(ext, &imported_go_packages)
                || reached_dynamically(&ext.file_path, &dynamic_forms)
            {
                return false;
            }
            // Counted only among files that would otherwise have been reported,
            // so the number answers "how much did this filter remove from the
            // finding" rather than "how many unreadable files exist" — the
            // second is `meta.devmap_rust.parse_failed_files`, and conflating
            // the two would let a vendored unparseable file inflate it.
            if ext.is_parse_failure() || matches!(ext.parse_outcome, ParseOutcome::Fallback { .. })
            {
                excluded_coverage_loss += 1;
                return false;
            }
            // A file no grammar read is not a candidate for anything.
            //
            // Prose and data formats have no imports because they are prose,
            // which is a different fact from a source language whose imports
            // this build cannot read — and charging them to the capability
            // counter below made `unwired_excluded_import_blind` disagree with
            // `coverage_gaps.import_blind` by five times on this repository,
            // 355 against 71, for one question with one answer.
            //
            // The exclusion itself is load-bearing and predates the reason
            // given for it: before the capability gate landed, every `.md`,
            // `.json` and `.yaml` in every repository was an unwired candidate,
            // and the gate swept them up by accident. They are excluded here on
            // their own grounds — nothing read them, they declare nothing to
            // strand — and counted in neither number, exactly as
            // `extraction_coverage` already keeps them out of both sides of its
            // own ratio.
            if !ext.grammar_read_this_file() {
                return false;
            }
            // The kernel never looked for an import of this file, so its
            // absence is not evidence of one.
            //
            // Gated on the file's **own** language rather than on its potential
            // importers'. An import names a module in the importer's own
            // language — no `.ts` file imports a `.java` — so the set of files
            // that could ever produce an inbound `Imports` edge for this one
            // shares its language, and that language's capability is the whole
            // answer. Checked after the parse-failure branch so a file with
            // both holes is charged once, to the more specific of the two.
            if !ext.capabilities().contains(Capability::Imports) {
                excluded_import_blind += 1;
                return false;
            }
            true
        })
        .map(|ext| ext.file_path.clone())
        .collect();
    candidates.sort();
    candidates.dedup();
    UnwiredScan {
        paths: candidates,
        excluded_coverage_loss,
        excluded_import_blind,
    }
}

/// Counters the artifact reports about its own completeness.
#[derive(Default)]
struct GraphProvenance {
    duplicate_node_ids_dropped: usize,
    duplicate_edges_dropped: usize,
    /// Edges with at least one endpoint that names no node.
    ///
    /// Counting edges rather than identities is deliberate and is the contract
    /// the Go consumer decodes (`repomap.go` → `OrphanEndpoints`, "counts edges
    /// the producer wrote whose endpoints are not nodes"). The key's *name*
    /// reads as a count of endpoints, and the two differ by more than 2x on
    /// this repository, so `distinct_edge_endpoints_without_node` carries that
    /// second number rather than leaving the name to be misread.
    edge_endpoints_without_node: usize,
    /// Distinct endpoint identities that name no node.
    distinct_edge_endpoints_without_node: usize,
    files_without_readable_source: usize,
    dead_code_exempt_omitted: usize,
    dead_code_without_node: usize,
    dead_code_duplicates_dropped: usize,
    /// Files whose symbols were recovered by line pattern because no grammar is
    /// linked for their language.
    ///
    /// Surfaced because those symbols are a weaker claim than parsed ones —
    /// names and spans only, no calls, no nesting — and a consumer that treats
    /// the graph uniformly would over-trust them. It is also the number that
    /// says how much of the tree the engine can only see coarsely.
    regex_fallback_files: usize,
    /// Files a grammar was wanted for and did not get to read.
    ///
    /// The counter the audit found missing entirely: `regex_fallback_files`
    /// covered one half of extraction loss and nothing counted the other, so a
    /// generation that lost whole files' call edges rendered a graph
    /// indistinguishable from one that read everything.
    ///
    /// Asked of `Extraction::is_parse_failure`, so a Markdown or JSON file —
    /// which reports `ParseOutcome::Failed` for want of a grammar that will
    /// never exist — is not counted here. 294 of this repository's 1,310 files
    /// are that shape.
    parse_failed_files: usize,
    /// Files the unwired filter dropped because their imports were never read.
    unwired_excluded_coverage_loss: usize,
    /// Files excluded because their language has no import extractor. See
    /// `UnwiredScan::excluded_import_blind`.
    unwired_excluded_import_blind: usize,
}

/// Render `code_graph.json` from a committed generation.
///
/// Fails rather than emitting an empty-but-well-formed graph when there is no
/// generation behind it: a consumer reading `nodes: []` cannot tell an empty
/// repository from a store that was never built, and every liveness conclusion
/// drawn from the second is wrong.
/// Build the code-graph model once.
///
/// Both encodings — the verbose JSON eleven Python consumers read, and the
/// interned form `encode_compact` produces — are rendered from *this* value.
/// A second traversal would be a second place for a field to be dropped, and
/// the two artifacts would then disagree with nothing to notice it.
/// The graph model both artifacts and the HTML view are projections of.
///
/// `pub` so `viz` can render from the same value the writer serializes rather
/// than reading `code_graph.json` back off disk. The picture and the artifact
/// must not be able to describe different generations, and the cheapest way to
/// guarantee that is for there to be only one of them.
/// The two arrays every reader of the graph consumes, and the bookkeeping the
/// artifact hangs its panels on. One owner for the node and edge rows: the
/// artifact ([`build_code_graph_value`]) and the query surfaces
/// ([`build_graph_core_value`]) both take theirs from [`graph_core`], so a row
/// cannot differ between what `export` writes and what `cypher` answers from.
struct GraphCore {
    nodes: Vec<Value>,
    edges: Vec<Value>,
    /// id -> (line, kind label), so `dead_code` can carry the line and kind its
    /// schema declares instead of a zero that means nothing.
    node_index: BTreeMap<String, (u32, &'static str)>,
    provenance: GraphProvenance,
}

/// `nodes` and `edges` as the artifact emits them, and nothing else.
///
/// What `cypher`, `routes`, `shape-check` and `api-impact` answer from. They
/// read the two arrays; the rest of the artifact — the churn panel's `git log`,
/// the intel panels, the dead-code list, the subsystem summary — was built and
/// discarded on every call: 643 ms for a `cypher` on this repository's store
/// against 8 ms for a `search`, 122 ms of it the git log. The rows are the
/// artifact's rows by construction (one [`graph_core`]), and
/// `tests/the_core_graph_is_the_artifacts_nodes_and_edges.rs` pins it.
pub fn build_graph_core_value(
    extractions: &[Extraction],
    analysis: &AnalysisSummary,
    edges: &[ResolvedEdge],
    repo_root: Option<&str>,
) -> Value {
    let core = graph_core(extractions, analysis, edges, repo_root);
    json!({
        "schema_version": CODE_GRAPH_SCHEMA_VERSION,
        "nodes": core.nodes,
        "edges": core.edges,
    })
}

fn graph_core(
    extractions: &[Extraction],
    analysis: &AnalysisSummary,
    edges: &[ResolvedEdge],
    repo_root: Option<&str>,
) -> GraphCore {
    let mut provenance = GraphProvenance::default();
    // One owner for "which files contributed no call edges", shared with
    // `analyze_liveness`, which caps dead-code confidence from the same two
    // numbers. Counted here rather than inline in the render loop below because
    // a second `matches!` chain over `parse_outcome` is precisely where the
    // `Failed`-vs-`NotApplicable` distinction gets dropped in one of the copies.
    let coverage = devmap_analyze::extraction_coverage(extractions);
    provenance.regex_fallback_files = coverage.pattern_recovered_files;
    provenance.parse_failed_files = coverage.parse_failed_files;
    let root = repo_root.map(str::to_string);
    let communities = community_by_file(analysis);

    let mut ordered: Vec<&Extraction> = extractions.iter().collect();
    ordered.sort_by(|left, right| left.file_path.cmp(&right.file_path));

    let mut nodes: Vec<Value> = Vec::new();
    let mut node_index: BTreeMap<String, (u32, &'static str)> = BTreeMap::new();

    for ext in &ordered {
        // One pass over the file's bytes for every span in it; the string form
        // of the conversion scans from the top of the file per call.
        let lines = std::fs::read_to_string(resolve_source_path(&root, &ext.file_path))
            .ok()
            .map(|text| LineIndex::new(&text));
        if lines.is_none() {
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
                match &lines {
                    Some(lines) => byte_span_to_line_range_in(lines, &symbol.span),
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

        // Route nodes.
        //
        // The one node kind that is not a declaration: the extractor records a
        // route from a framework decorator or registration, and the resolver
        // names it as the source of the `HandlesRoute` edge. Without the node,
        // that edge names nothing — so `route_map`, `shape_check` and
        // `api_impact` read an empty graph out of a generation that has the
        // routes in it, and the handler endpoint dangles beside it.
        //
        // `ExtractedRoute::node_id` owns the id shape for both sides.
        for route in &ext.routes {
            let id = route.node_id(&ext.file_path);
            if node_index.contains_key(&id) {
                // The same method and path declared twice in one file. Two
                // nodes sharing an id is worse than one, exactly as above.
                provenance.duplicate_node_ids_dropped += 1;
                continue;
            }

            let mut extras = Map::new();
            let (line, end_line) = match &lines {
                Some(lines) => byte_span_to_line_range_in(lines, &route.span),
                None => {
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
            };
            // The three fields a route consumer reads. `verb` is whatever the
            // extractor recorded: `ANY` when the source declares no single
            // method — a Flask `@app.route` with no `methods=` — which
            // consumers already read as "matches any verb" rather than as a
            // missing value.
            extras.insert(
                "route".to_string(),
                Value::String(route.path_pattern.clone()),
            );
            extras.insert("verb".to_string(), Value::String(route.http_method.clone()));
            extras.insert(
                "framework".to_string(),
                Value::String(route.framework.clone()),
            );

            let kind = node_kind_label(SymbolKind::Route);
            node_index.insert(id.clone(), (line, kind));
            nodes.push(json!({
                "id": id,
                "kind": kind,
                "path": ext.file_path,
                "name": format!("{} {}", route.http_method, route.path_pattern),
                "line": line,
                "end_line": end_line,
                "area": area,
                "language": ext.language,
                // An HTTP route is reached from outside the program by
                // definition; there is no module boundary for it to be
                // private to.
                "exported": true,
                "community": community,
                "extras": Value::Object(extras),
            }));
        }
    }

    // (source, target, kind, confidence, resolution, resolution_source) —
    // sorted so the artifact is stable and deduplicated so a fan-out cannot
    // report the same edge twice. The edge's identity is the first four; the
    // evidence pair rides along, and when two edges share an identity but not
    // an evidence tier (an ambiguous fan-out beside a scoped hit, say) the
    // sort makes the survivor the smallest label rather than whichever came
    // first out of the store — deterministic (R4), and stated here.
    let mut edge_keys: Vec<(
        &str,
        &str,
        &'static str,
        &'static str,
        &'static str,
        &'static str,
    )> = edges
        .iter()
        .map(|edge| {
            let (resolution, resolution_source) = edge
                .evidence
                .map(|evidence| (evidence.kind.label(), evidence.source.label()))
                .unwrap_or(("", ""));
            (
                edge.source_symbol.as_str(),
                edge.target_symbol.as_str(),
                edge_kind_label(edge.edge_kind),
                confidence_label(edge.confidence.0),
                resolution,
                resolution_source,
            )
        })
        .collect();
    edge_keys.sort_unstable();
    let before_dedup = edge_keys.len();
    edge_keys.dedup_by(|left, right| {
        (left.0, left.1, left.2, left.3) == (right.0, right.1, right.2, right.3)
    });
    provenance.duplicate_edges_dropped = before_dedup - edge_keys.len();

    let mut edge_values: Vec<Value> = Vec::with_capacity(edge_keys.len());
    // Sorted, so the count is a function of the identities and not of edge
    // order, and so a future emitter can list them without a second pass.
    let mut missing_endpoints: BTreeSet<&str> = BTreeSet::new();
    for (source, target, kind, confidence, resolution, resolution_source) in edge_keys {
        let source_missing = !node_index.contains_key(source);
        let target_missing = !node_index.contains_key(target);
        if source_missing || target_missing {
            provenance.edge_endpoints_without_node += 1;
        }
        if source_missing {
            missing_endpoints.insert(source);
        }
        if target_missing {
            missing_endpoints.insert(target);
        }
        edge_values.push(json!({
            "source": source,
            "target": target,
            "kind": kind,
            "confidence": confidence,
            // Not derivable: `generation_edges` persists no reason text.
            // Python's own `compact` export tier blanks this field for the
            // same reason, so "" is a value consumers already handle.
            "reason": "",
            // What the confidence rests on — the resolver's evidence tier
            // (`ResolutionKind::label`) — and whether that tier was resolved
            // in this process, read back from the store's column, or
            // reconstructed from a generation that predates it. "" only on
            // an edge built by hand with no evidence at all.
            "resolution": resolution,
            "resolution_source": resolution_source,
            "extras": {},
        }));
    }

    provenance.distinct_edge_endpoints_without_node = missing_endpoints.len();

    GraphCore {
        nodes,
        edges: edge_values,
        node_index,
        provenance,
    }
}

pub fn build_code_graph_value(
    extractions: &[Extraction],
    analysis: &AnalysisSummary,
    edges: &[ResolvedEdge],
    freshness: &FreshnessInfo,
    repo_root: Option<&str>,
) -> anyhow::Result<Value> {
    if freshness.generation_id == 0 {
        anyhow::bail!("code graph unavailable: no committed generation (build a generation first)");
    }

    let GraphCore {
        nodes,
        edges: edge_values,
        node_index,
        mut provenance,
    } = graph_core(extractions, analysis, edges, repo_root);

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

    let unwired = unwired_candidates(extractions, edges);
    provenance.unwired_excluded_coverage_loss = unwired.excluded_coverage_loss;
    provenance.unwired_excluded_import_blind = unwired.excluded_import_blind;

    let analysis_status = match &analysis.status {
        AnalysisStatus::Ok => "ok".to_string(),
        AnalysisStatus::Partial { reason } => format!("partial: {reason}"),
        AnalysisStatus::Timeout { reason } => format!("timeout: {reason}"),
    };

    // Reachability is answerable exactly when the component pass ran over a
    // corpus whose calls were fully extracted. Both halves matter: an oversized
    // graph means no answer at all, and a coverage hole means a missing edge
    // into a component could invalidate every cluster in the result.
    let unreachable_unreliable = analysis.dead_clusters.refused_oversized_graph
        || !matches!(analysis.status, AnalysisStatus::Ok);

    // The `unavailable` map is built rather than written as a literal because
    // two of its entries are now conditional. A digest the caller supplied is a
    // computed answer; leaving its "not fingerprinted" note in place would have
    // the artifact assert both at once, which is worse than either alone — a
    // consumer reading the marker would skip a check the value could have
    // satisfied. The unconditional entries stay unconditional: nothing about a
    // caller-supplied digest makes reachability or edge reasons computable.
    let mut unavailable = serde_json::Map::new();
    // Conditional since W1.1. Reachability *is* computed now — by the
    // strongly-connected-component pass, not by the entry-root BFS this note
    // used to describe — so the marker is written only when that pass could not
    // answer. Leaving it unconditional beside a populated list would have the
    // artifact assert both at once, which the comment above already calls worse
    // than either alone.
    if unreachable_unreliable {
        unavailable.insert(
            "unreachable_files".to_string(),
            json!(
                "the component pass could not answer reachability for this \
                 generation: it refused an oversized graph, or call coverage \
                 had a hole and one missing edge into a component invalidates \
                 the whole finding"
            ),
        );
    }
    unavailable.insert(
        "edge_reason".to_string(),
        json!(
            "generation_edges persists no reason string, so every edge \
             reports the empty reason Python's compact tier also uses"
        ),
    );
    unavailable.insert(
        "edge_extras".to_string(),
        json!("no per-edge extras are persisted"),
    );
    unavailable.insert(
        "node_extras_bases_implements_decorators".to_string(),
        json!(
            "the extractor records no base/interface/decorator lists on a \
             symbol; the keys are omitted rather than emitted empty"
        ),
    );
    if freshness.stamped.indexed_hash.is_none() {
        unavailable.insert(
            "indexed_hash".to_string(),
            json!(
                "the Rust kernel computes no SHA-1 file-list digest; \
                 consumers treat the empty string as 'not fingerprinted' and \
                 skip the check rather than concluding freshness"
            ),
        );
    }
    if freshness.stamped.content_fingerprint.is_none() {
        unavailable.insert(
            "content_fingerprint".to_string(),
            json!(
                "the Rust kernel computes no SHA-1 size+mtime digest; see \
                 indexed_hash"
            ),
        );
    }

    // Graph intelligence: the hubs the repository leans on, its import cycles,
    // and its churn x coupling hotspots. See `devmap_analyze::graph_intel` for
    // why this moved into the kernel — `enrich_graph_intel` lost both its
    // callers in `d232dea`, and `viz.py:520-522` has been rendering `(none)`
    // into the Intel tab of every graph written since, regardless of what the
    // repository holds.
    //
    // The churn half is a `git log` and is therefore the one part of this
    // artifact that costs a subprocess. It is paid here rather than skipped
    // because this is the only place that writes the panel, and it is paid at
    // most once per artifact regeneration: `write_consumer_artifacts` returns
    // on its stamp before reaching this function when nothing changed, so an
    // unchanged `dev map` does not run git at all.
    let churn = match repo_root {
        Some(root) => crate::inventory::churn(std::path::Path::new(root)),
        None => devmap_analyze::FileChurn::unavailable(
            "no repository root was recorded for this generation, so no history \
             was read",
        ),
    };
    // The paths this generation actually indexed. Churn names every path git
    // touched in the window, including files deleted since and files no
    // extractor can read; a hotspot naming one of those is a row no other list
    // in this artifact mentions.
    let known_files: std::collections::BTreeSet<&str> = extractions
        .iter()
        .map(|ext| ext.file_path.as_str())
        .collect();
    let intel = devmap_analyze::graph_intel(edges, &churn, &known_files);

    let payload = json!({
        "schema_version": CODE_GRAPH_SCHEMA_VERSION,
        "nodes": nodes,
        "edges": edge_values,
        "dead_code": dead_code,
        // One finding per abandoned cycle, beside the per-symbol list rather
        // than inside it: a 40-symbol dead subsystem is one thing a reader acts
        // on, and forty entries would push real single-symbol findings past the
        // cap. Top level, with the other finding lists.
        //
        // `null`, not `[]`, when the pass refused. The scan comes back with an
        // empty `clusters` beside `refused_oversized_graph`, so writing the
        // field straight through puts "the graph was too large to walk" into
        // the artifact as "there are no abandoned subsystems" — and `CodeGraph`
        // loads it as a computed empty finding with nothing to say otherwise.
        // The same distinction `dead` makes on the query path.
        "dead_clusters": analysis.dead_clusters.reported_clusters(),
        "dead_clusters_truncated": analysis.dead_clusters.truncated_clusters,
        // Why the list above is absent, when it is absent because the pass ran
        // and refused. A reader told only "not recorded" would rebuild, and the
        // rebuild walks the same graph and refuses again.
        "dead_clusters_incomplete": analysis.dead_clusters.incomplete_reason(),
        "entry_roots": entry_root_paths(extractions),
        "unwired_candidates": unwired.paths,
        // Computed by the component pass — files whose every declared symbol
        // sits in a cycle nothing outside reaches. Not the entry-root BFS the
        // `unreliable` flag was warning about.
        "unreachable_files": analysis.dead_clusters.unreachable_files.clone(),
        "generated_head": freshness.generated_head(),
        // Empty unless the caller computed one. See `StampedFreshness`; the
        // paired `meta.devmap_rust.unavailable` entries below are removed for
        // exactly the fields that carry a real value, so "not fingerprinted"
        // and "fingerprinted" are never both claimed at once.
        "indexed_hash": freshness.stamped.indexed_hash.clone().unwrap_or_default(),
        "content_fingerprint": freshness
            .stamped
            .content_fingerprint
            .clone()
            .unwrap_or_default(),
        "meta": {
            // Ownership marker. Python never writes this key, so its absence is
            // what identifies a foreign graph to the clobber guard.
            "map_engine": CONSUMER_MAP_ENGINE,
            // No longer unconditional; see `unreachable_unreliable` above. It
            // now means the answer cannot be trusted, rather than that no
            // answer was attempted — which is what let four Python consumers
            // suppress the key permanently and leave it a third state.
            "liveness_unreachable_unreliable": unreachable_unreliable,
            "legacy_dead_symbol_candidates": legacy_dead,
            // The two keys `viz.py:520-522` reads for the Intel tab's God
            // Nodes and Cycles panels. At the top of `meta`, where the Python
            // producer put them, so the reader needs no change.
            //
            // The third panel, `hotspots`, is not filled: it is churn x
            // coupling and the churn half is `git log --since=90.days`, which
            // this producer does not read. `viz.py` already renders that panel
            // as "(no churn data - needs git history)", and
            // `hotspots_computed: false` below says the same in the artifact.
            "god_nodes": intel.god_nodes,
            "circular_imports": intel.circular_imports,
            // The third panel. `viz.py:521` has read this key since before the
            // cutover and found it absent on every graph the Rust kernel has
            // written, so the Hotspots tab said "(no churn data)" on a
            // repository with three months of history.
            "hotspots": intel.hotspots,
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
                "distinct_edge_endpoints_without_node":
                    provenance.distinct_edge_endpoints_without_node,
                "files_without_readable_source": provenance.files_without_readable_source,
                "regex_fallback_files": provenance.regex_fallback_files,
                // The coverage half of `analysis_status`. Both numbers travel
                // with the artifact so a reader can size the hole rather than
                // only learning that one exists, and `unwired_candidates` says
                // how much of *it* the same hole removed.
                "parse_failed_files": provenance.parse_failed_files,
                "unwired_excluded_coverage_loss": provenance.unwired_excluded_coverage_loss,
                // Reported beside it rather than summed into it: a re-index can
                // fix the first number and can never fix this one.
                "unwired_excluded_import_blind": provenance.unwired_excluded_import_blind,
                // Provenance for the intel panels above. `god_nodes: []` from
                // a pass that ran and `god_nodes: []` from a pass that never
                // happened were the same bytes for the whole life of the
                // post-cutover graph; these are what tell them apart.
                "god_nodes_computed": true,
                "god_nodes_shown": intel.god_nodes.len(),
                // Ranked candidates before the cap, so a reader never mistakes
                // the fifteen shown for the population they came from.
                "god_nodes_total": intel.god_nodes_total,
                "god_nodes_truncated": intel.god_nodes_truncated(),
                "circular_imports_computed": true,
                "circular_imports_shown": intel.circular_imports.len(),
                "circular_imports_total": intel.circular_imports_total,
                "circular_imports_truncated": intel.circular_imports_truncated(),
                // Churn is repository history, read by one bounded `git log`
                // in `inventory::churn`. `false` here means that read did not
                // happen — no repository root, no git, no commits in the
                // window — and the reason below says which.
                "hotspots_computed": intel.hotspots_computed,
                "hotspots_unavailable_reason": intel.hotspots_unavailable_reason,
                "hotspots_shown": intel.hotspots.len(),
                // Scored candidates before the cap, so fifteen rows are never
                // mistaken for the population they came from.
                "hotspots_total": intel.hotspots_total,
                "hotspots_truncated": intel.hotspots_truncated(),
                // Whether a churn bound cut the history short. `true` makes
                // every `churn` count a lower bound rather than the window's.
                "hotspots_churn_truncated": intel.hotspots_churn_truncated,
                "unavailable": unavailable,
            },
        },
    });

    // Compact, not pretty — unlike `repo_map.json`, which stays indented.
    //
    // The two artifacts have different readers. `repo_map.json` is small
    // (0.4 MB here) and `CLAUDE.md` tells agents to open it, so its indentation
    // buys something. This graph is 27 MB on DevCouncil and 105 MB on a
    // 4,300-file repository; nobody reads that by hand, and every one of its
    // twelve consumers under `src/devcouncil/` reaches it through `json.load`,
    // which cannot tell the two apart.
    //
    // Indentation was therefore 23.3% of the file (27.30 MB → 20.94 MB
    // measured) spent on whitespace no reader sees, paid again on every write,
    // every read, and every byte of disk churn the watcher causes.
    Ok(payload)
}

/// Verbose encoding: the artifact eleven Python consumers under
/// `src/devcouncil/` read with `json.load`.
pub fn generate_code_graph_json(
    extractions: &[Extraction],
    analysis: &AnalysisSummary,
    edges: &[ResolvedEdge],
    freshness: &FreshnessInfo,
    repo_root: Option<&str>,
) -> anyhow::Result<String> {
    let payload = build_code_graph_value(extractions, analysis, edges, freshness, repo_root)?;
    // Compact, not pretty — unlike `repo_map.json`, which stays indented.
    //
    // The two artifacts have different readers. `repo_map.json` is small
    // (0.3 MB here) and `CLAUDE.md` tells agents to open it, so its indentation
    // buys something. This graph is 20.9 MB on DevCouncil and 105 MB on a
    // 4,300-file repository; nobody reads that by hand, and every one of its
    // consumers reaches it through `json.load`, which cannot tell the two apart.
    Ok(serde_json::to_string(&payload)?)
}

/// Both encodings from one traversal.
///
/// The verbose form stays canonical for interchange — every existing consumer
/// reads it, and `DIVERGENCES.md` records which is which. The interned form is
/// written *beside* it, never instead of it, for readers that pay for the
/// artifact by the byte.
///
/// One entry point rather than two, because two would mean two traversals of
/// the same model whenever a caller wants both, and the second traversal is
/// exactly where the two encodings would eventually disagree.
pub fn generate_code_graph_encodings(
    extractions: &[Extraction],
    analysis: &AnalysisSummary,
    edges: &[ResolvedEdge],
    freshness: &FreshnessInfo,
    repo_root: Option<&str>,
    want_compact: bool,
) -> anyhow::Result<(String, Option<String>)> {
    let payload = build_code_graph_value(extractions, analysis, edges, freshness, repo_root)?;
    let compact = if want_compact {
        Some(serde_json::to_string(&encode_compact(&payload)?)?)
    } else {
        None
    };
    Ok((serde_json::to_string(&payload)?, compact))
}

/// Identity of the interned layout. A decoder written against `v1` must refuse
/// a later version rather than read it as `v1` and answer from a layout it does
/// not understand.
pub const CODE_GRAPH_COMPACT_ENCODING: &str = "devmap-compact-v1";

/// The tables worth interning. Both are arrays of uniformly-shaped objects and
/// together they are 99.7% of the artifact (measured on DevCouncil: edges
/// 75.6%, nodes 24.1%); every other key is under 0.25% and is copied verbatim,
/// because interning it would add decoder surface for no measurable return.
const COMPACT_TABLES: &[&str] = &["nodes", "edges"];

/// How one column is stored.
///
/// `Interned` columns hold an index into the shared string table; `Raw` columns
/// hold the value itself. The choice is **derived from the data** — a column is
/// interned only when every row in it is a string — rather than declared from a
/// field list that could drift out of step with what the emitter produces.
const COLUMN_INTERNED: &str = "s";
const COLUMN_RAW: &str = "j";

/// Interned encoding of the same model.
///
/// The verbose artifact repeats every symbol identity once per edge endpoint:
/// on DevCouncil, 14,324 distinct endpoint strings are written 147,726 times,
/// and `source` + `target` alone are 52.6% of the file. This encoding writes
/// each distinct string once and refers to it by index.
///
/// **What it deliberately does not do is decide anything.** It carries no
/// field list of its own, drops no key, and applies no filter: a table whose
/// rows are not uniformly shaped is copied through verbatim and named in
/// `verbatim_tables`, so a reader is told which tables were interned rather
/// than having to infer it. `decode_compact` reverses this exactly, and the
/// round-trip is what the tests assert against a real artifact — a field the
/// encoder failed to carry cannot pass that check.
pub fn encode_compact(payload: &Value) -> anyhow::Result<Value> {
    let object = payload
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("code graph payload is not a JSON object"))?;

    // Sorted, not first-seen: the table is then a function of the *set* of
    // strings and not of the traversal that found them, so two builds of the
    // same graph are byte-identical even if the emitter's visit order changes
    // (R4). It is also diffable, which first-seen order is not.
    let mut pool: BTreeSet<&str> = BTreeSet::new();
    let mut specs: BTreeMap<&str, Vec<(String, &'static str)>> = BTreeMap::new();
    let mut verbatim: Vec<&str> = Vec::new();

    for &table in COMPACT_TABLES {
        let Some(rows) = object.get(table).and_then(Value::as_array) else {
            // Absent is not malformed: a payload without the key simply has no
            // such table, and it is neither interned nor listed as skipped.
            continue;
        };
        match column_spec(rows) {
            Some(spec) => {
                for row in rows {
                    let Some(row) = row.as_object() else { continue };
                    for (name, kind) in &spec {
                        if *kind == COLUMN_INTERNED {
                            if let Some(text) = row.get(name).and_then(Value::as_str) {
                                pool.insert(text);
                            }
                        }
                    }
                }
                specs.insert(table, spec);
            }
            None => verbatim.push(table),
        }
    }

    let strings: Vec<&str> = pool.into_iter().collect();
    let index: BTreeMap<&str, usize> = strings
        .iter()
        .enumerate()
        .map(|(position, &text)| (text, position))
        .collect();

    let mut out = Map::new();
    for (key, value) in object {
        if let Some(spec) = specs.get(key.as_str()) {
            let rows = value
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("table {key} stopped being an array"))?;
            let mut encoded: Vec<Value> = Vec::with_capacity(rows.len());
            for row in rows {
                let row = row
                    .as_object()
                    .ok_or_else(|| anyhow::anyhow!("row in {key} is not an object"))?;
                let mut cells: Vec<Value> = Vec::with_capacity(spec.len());
                for (name, kind) in spec {
                    let cell = row
                        .get(name)
                        .ok_or_else(|| anyhow::anyhow!("row in {key} is missing {name}"))?;
                    if *kind == COLUMN_INTERNED {
                        let text = cell.as_str().ok_or_else(|| {
                            anyhow::anyhow!("{key}.{name} was typed as a string and is not one")
                        })?;
                        let position = index.get(text).ok_or_else(|| {
                            anyhow::anyhow!("{key}.{name} holds a string absent from the pool")
                        })?;
                        cells.push(json!(position));
                    } else {
                        cells.push(cell.clone());
                    }
                }
                encoded.push(Value::Array(cells));
            }
            out.insert(
                key.clone(),
                json!({
                    "fields": spec
                        .iter()
                        .map(|(name, kind)| json!([name, kind]))
                        .collect::<Vec<Value>>(),
                    "rows": encoded,
                }),
            );
        } else {
            out.insert(key.clone(), value.clone());
        }
    }

    out.insert("encoding".to_string(), json!(CODE_GRAPH_COMPACT_ENCODING));
    out.insert("strings".to_string(), json!(strings));
    out.insert(
        "interned_tables".to_string(),
        json!(specs.keys().copied().collect::<Vec<&str>>()),
    );
    // Named rather than left to inference: a consumer that finds `nodes` shaped
    // like the verbose artifact needs to know that is the encoder reporting it
    // could not intern the table, not the encoder having silently changed form.
    verbatim.sort_unstable();
    out.insert("verbatim_tables".to_string(), json!(verbatim));
    Ok(Value::Object(out))
}

/// Column layout for a table, or `None` when the rows are not uniformly shaped.
///
/// Uniformity is required in both directions — same key set, and a column is
/// interned only when *every* row holds a string there. A single row breaking
/// either rule sends the whole table through verbatim, which costs bytes and
/// keeps the artifact readable; guessing per row would make the layout depend
/// on data the decoder cannot see.
fn column_spec(rows: &[Value]) -> Option<Vec<(String, &'static str)>> {
    let first = rows.first()?.as_object()?;
    let names: Vec<&String> = first.keys().collect();
    let mut interned = vec![true; names.len()];
    for row in rows {
        let row = row.as_object()?;
        if row.len() != names.len() {
            return None;
        }
        for (position, name) in names.iter().enumerate() {
            let cell = row.get(name.as_str())?;
            if !cell.is_string() {
                interned[position] = false;
            }
        }
    }
    Some(
        names
            .into_iter()
            .zip(interned)
            .map(|(name, is_interned)| {
                (
                    name.clone(),
                    if is_interned {
                        COLUMN_INTERNED
                    } else {
                        COLUMN_RAW
                    },
                )
            })
            .collect(),
    )
}

/// Reverse `encode_compact`, or fail saying why.
///
/// Every failure here is a refusal rather than a partial answer: a decoder that
/// returns half a graph when an index is out of range hands its caller a
/// smaller graph with no signal that it is smaller, and every downstream
/// "no callers" answer would then be wrong in the confident direction.
pub fn decode_compact(compact: &Value) -> anyhow::Result<Value> {
    let object = compact
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("compact code graph is not a JSON object"))?;

    let encoding = object
        .get("encoding")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("compact code graph carries no `encoding`"))?;
    if encoding != CODE_GRAPH_COMPACT_ENCODING {
        anyhow::bail!(
            "compact code graph is encoded as {encoding}; this decoder reads \
             {CODE_GRAPH_COMPACT_ENCODING} only"
        );
    }

    let strings: Vec<&str> = object
        .get("strings")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("compact code graph carries no string table"))?
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("string table holds a non-string"))
        })
        .collect::<anyhow::Result<Vec<&str>>>()?;

    let interned: BTreeSet<&str> = object
        .get("interned_tables")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let mut out = Map::new();
    for (key, value) in object {
        if key == "encoding" || key == "strings" || key == "interned_tables" {
            continue;
        }
        if key == "verbatim_tables" {
            continue;
        }
        if !interned.contains(key.as_str()) {
            out.insert(key.clone(), value.clone());
            continue;
        }
        let table = value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("interned table {key} is not an object"))?;
        let fields = table
            .get("fields")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("interned table {key} carries no field list"))?;
        let mut spec: Vec<(String, bool)> = Vec::with_capacity(fields.len());
        for field in fields {
            let pair = field
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("field spec in {key} is not a pair"))?;
            let name = pair
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("field spec in {key} has no name"))?;
            let kind = pair
                .get(1)
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("field spec in {key} has no storage kind"))?;
            match kind {
                COLUMN_INTERNED => spec.push((name.to_string(), true)),
                COLUMN_RAW => spec.push((name.to_string(), false)),
                other => anyhow::bail!("field {name} in {key} declares unknown storage {other}"),
            }
        }
        let rows = table
            .get("rows")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("interned table {key} carries no rows"))?;
        let mut decoded: Vec<Value> = Vec::with_capacity(rows.len());
        for (position, row) in rows.iter().enumerate() {
            let cells = row
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("row {position} of {key} is not an array"))?;
            if cells.len() != spec.len() {
                anyhow::bail!(
                    "row {position} of {key} has {} cells for {} fields",
                    cells.len(),
                    spec.len()
                );
            }
            let mut object = Map::new();
            for ((name, is_interned), cell) in spec.iter().zip(cells) {
                if *is_interned {
                    let slot = cell.as_u64().ok_or_else(|| {
                        anyhow::anyhow!("{key}.{name} in row {position} is not a string index")
                    })? as usize;
                    let text = strings.get(slot).ok_or_else(|| {
                        anyhow::anyhow!(
                            "{key}.{name} in row {position} points at string {slot} of {}",
                            strings.len()
                        )
                    })?;
                    object.insert(name.clone(), json!(text));
                } else {
                    object.insert(name.clone(), cell.clone());
                }
            }
            decoded.push(Value::Object(object));
        }
        out.insert(key.clone(), Value::Array(decoded));
    }
    Ok(Value::Object(out))
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
    // Only the tests construct caller-supplied freshness; the emitters read it
    // off `FreshnessInfo`.
    use crate::model::StampedFreshness;
    use devmap_analyze::model::{AnalysisStatus, CommunityReport, DeadSymbolReport};
    use devmap_extract::extract_file;
    use devmap_extract::model::Confidence;

    fn freshness() -> FreshnessInfo {
        FreshnessInfo {
            head_sha: "abc123".to_string(),
            generation_id: 1,
            pending_count: 0,
            stamped: Default::default(),
        }
    }

    fn analysis(dead: Vec<DeadSymbolReport>, communities: Vec<CommunityReport>) -> AnalysisSummary {
        AnalysisSummary {
            // No discovery step ran over this hand-built corpus, so there is no
            // refusal count to report. `None` says that; `0` would claim a walk.
            discovery_refused_files: None,
            total_files: 0,
            total_symbols: 0,
            total_edges: 0,
            dead_symbols: dead,
            communities,
            status: AnalysisStatus::Ok,
            unresolved_calls: 0,
            clone_coverage: Default::default(),
            // Fields this fixture does not exercise. Spread rather than
            // enumerated so a new analysis field does not break every test
            // literal in the workspace; the one production construction in
            // `analyze()` still names every field exhaustively.
            ..Default::default()
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
            evidence: None,
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
            keys, CODE_GRAPH_TOP_LEVEL_KEYS,
            "the writer must emit exactly the keys `CODE_GRAPH_TOP_LEVEL_KEYS` \
             declares, which is the contract with schema.py's CodeGraph"
        );
        assert_eq!(value["schema_version"], json!(CODE_GRAPH_SCHEMA_VERSION));
    }

    /// A caller-supplied digest is stamped, and stops being advertised as
    /// unavailable.
    ///
    /// The two halves are one contract. Stamping the value while leaving
    /// `meta.devmap_rust.unavailable.indexed_hash` in place would have the
    /// artifact assert both "here is the digest" and "this kernel computes no
    /// digest" — and a consumer that reads the marker skips the very check the
    /// value would have satisfied, so the map reads permanently unverifiable
    /// while carrying a perfectly good fingerprint.
    ///
    /// `generated_head` is asserted to *override* the kernel's `head_sha`
    /// rather than merely fill a blank. They answer different questions: the
    /// kernel's is the head of the last persisted generation, the caller's is
    /// the head the artifact describes, and an incremental build that persists
    /// no generation makes them differ.
    #[test]
    fn caller_supplied_freshness_is_stamped_and_drops_its_unavailable_marker() {
        let extractions = [extract_file("k.py", "def a(): pass\n")];
        let analysis = empty_analysis();

        let mut stamped_freshness = freshness();
        stamped_freshness.stamped = StampedFreshness {
            generated_head: Some("cafe1234".to_string()),
            indexed_hash: Some("files-digest".to_string()),
            content_fingerprint: Some("content-digest".to_string()),
        };
        let json = generate_code_graph_json(&extractions, &analysis, &[], &stamped_freshness, None)
            .unwrap();
        let value: Value = serde_json::from_str(&json).unwrap();

        assert_eq!(value["indexed_hash"], json!("files-digest"));
        assert_eq!(value["content_fingerprint"], json!("content-digest"));
        assert_eq!(
            value["generated_head"],
            json!("cafe1234"),
            "a caller-supplied head must win over the kernel's generation head"
        );

        let unavailable = &value["meta"]["devmap_rust"]["unavailable"];
        assert!(
            unavailable.get("indexed_hash").is_none(),
            "a stamped indexed_hash must not also be declared unavailable"
        );
        assert!(
            unavailable.get("content_fingerprint").is_none(),
            "a stamped content_fingerprint must not also be declared unavailable"
        );
        // Retired from "unconditional" to "conditional, and independent of
        // stamping" (W1.1/W2.4). Reachability *is* computed now, by the
        // component pass, so this fixture — an `Ok` analysis with no clusters —
        // carries no marker. The axis the assertion protects is unchanged: a
        // supplied digest must not change what the artifact claims about
        // reachability, in either direction.
        assert!(
            unavailable.get("unreachable_files").is_none(),
            "this fixture's analysis is `Ok`, so reachability was answered and \
             no unavailability marker belongs here"
        );
        assert_eq!(
            value["meta"]["liveness_unreachable_unreliable"],
            json!(false),
            "stamping a digest must not make reachability unreliable"
        );
    }

    /// Without caller-supplied digests the artifact is exactly what it was:
    /// empty values, markers present.
    ///
    /// The stamping path must not become the only correct path. `dev map` is
    /// not the sole caller — the daemon and any direct `devmap manifest` run
    /// pass nothing — and for those the honest answer is still "not
    /// fingerprinted", never a plausible-looking empty string with no warning
    /// attached.
    #[test]
    fn unstamped_freshness_still_reports_the_digests_as_unavailable() {
        let value = graph(
            &[extract_file("k.py", "def a(): pass\n")],
            &empty_analysis(),
            &[],
        );
        assert_eq!(value["indexed_hash"], json!(""));
        assert_eq!(value["content_fingerprint"], json!(""));

        let unavailable = &value["meta"]["devmap_rust"]["unavailable"];
        assert!(
            unavailable.get("indexed_hash").is_some(),
            "an uncomputed indexed_hash must stay declared unavailable"
        );
        assert!(
            unavailable.get("content_fingerprint").is_some(),
            "an uncomputed content_fingerprint must stay declared unavailable"
        );
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
            [
                "confidence",
                "extras",
                "kind",
                "reason",
                "resolution",
                "resolution_source",
                "source",
                "target"
            ],
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
            stamped: Default::default(),
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

    /// An `unreachable_files` answer is flagged exactly when it is unreliable.
    ///
    /// Retired from `unreachable_files_is_never_presented_as_a_computed_result`
    /// (W1.1/W2.4). That pinned the field as permanently uncomputed, which was
    /// true of a hardcoded `[]` and is not true of the component pass. The axis
    /// it protected is kept and inverted into both directions, because that is
    /// where the danger actually lives: an empty list read as "everything is
    /// reachable" is a stronger claim than "we did not look", and a populated
    /// list published beside a "never computed" marker asserts both at once.
    #[test]
    fn unreachable_files_is_flagged_exactly_when_the_answer_is_unreliable() {
        // A complete analysis: the answer is computed, so no marker.
        let computed = graph(
            &[extract_file("k.py", "def a(): pass\n")],
            &empty_analysis(),
            &[],
        );
        assert_eq!(computed["unreachable_files"], json!([]));
        assert_eq!(
            computed["meta"]["liveness_unreachable_unreliable"],
            json!(false),
            "an `Ok` analysis answered the question; saying otherwise makes the \
             flag meaningless and four Python consumers suppress the key forever"
        );
        assert!(
            computed["meta"]["devmap_rust"]["unavailable"]
                .get("unreachable_files")
                .is_none(),
            "a computed answer must not also be declared unavailable"
        );

        // A degraded analysis: one missing edge into a component invalidates
        // the whole finding, so the answer is marked unreliable and the reason
        // is stated.
        let mut degraded = empty_analysis();
        degraded.status = AnalysisStatus::Partial {
            reason: "call extraction did not cover the whole corpus".to_string(),
        };
        let flagged = graph(&[extract_file("k.py", "def a(): pass\n")], &degraded, &[]);
        assert_eq!(
            flagged["meta"]["liveness_unreachable_unreliable"],
            json!(true)
        );
        assert!(
            flagged["meta"]["devmap_rust"]["unavailable"]["unreachable_files"]
                .as_str()
                .is_some_and(|reason| reason.contains("coverage")),
            "the reason the answer cannot be trusted must be stated: {:?}",
            flagged["meta"]["devmap_rust"]["unavailable"]["unreachable_files"]
        );
    }

    /// A refused component scan is written as an absence, not as an empty list.
    ///
    /// The artifact had the same hole the query surface did, two lines from a
    /// mechanism built to close it: `unreachable_files` gets an `unavailable`
    /// marker when the scan refuses, and `dead_clusters` was written straight
    /// through — so `code_graph.json` recorded "the graph was too large to
    /// walk" as `[]`, and `CodeGraph` loaded it as a computed empty finding.
    ///
    /// Both directions, because writing `null` unconditionally would pass the
    /// first half and make every ordinary artifact claim it had no scan.
    #[test]
    fn a_refused_component_scan_is_written_as_null_with_its_reason() {
        let files = [extract_file("k.py", "def a(): pass\n")];

        let computed = graph(&files, &empty_analysis(), &[]);
        assert_eq!(
            computed["dead_clusters"],
            json!([]),
            "a scan that ran and found none is a finding and must stay a list"
        );
        assert_eq!(computed["dead_clusters_incomplete"], json!(null));

        let mut refused = empty_analysis();
        refused.dead_clusters.refused_oversized_graph = true;
        refused.dead_clusters.clusters.clear();
        let value = graph(&files, &refused, &[]);
        assert_eq!(
            value["dead_clusters"],
            json!(null),
            "nothing was walked, so the artifact knows nothing about components; \
             an empty list would say it walked and found none"
        );
        assert!(
            value["dead_clusters_incomplete"]
                .as_str()
                .is_some_and(|reason| reason
                    .contains(&devmap_analyze::dead_clusters::DEAD_CLUSTER_MAX_NODES.to_string())),
            "and the reason must name the ceiling: {:?}",
            value["dead_clusters_incomplete"]
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

    // ---- G6: interned wire format -------------------------------------------
    //
    // Measured on DevCouncil itself before this encoding existed
    // (1,311 files / 14,488 nodes / 73,863 edges): `code_graph.json` is
    // 20,899,318 bytes, of which `edges` is 75.6% and `nodes` 24.1%. Inside
    // `edges`, `source` + `target` alone are 68.3% — 11.0 MB spent writing
    // 14,324 distinct endpoint strings 147,726 times.

    /// A graph with enough repetition for interning to have something to do.
    fn compact_fixture() -> (Vec<Extraction>, AnalysisSummary, Vec<ResolvedEdge>) {
        let extractions = vec![
            extract_file(
                "pkg/alpha.py",
                "def one():\n    two()\n\ndef two():\n    pass\n",
            ),
            extract_file(
                "pkg/beta.py",
                "def three():\n    one()\n\ndef four():\n    one()\n",
            ),
        ];
        let edges = vec![
            edge(
                "pkg/alpha.py::one",
                "pkg/alpha.py::two",
                EdgeKind::Calls,
                Confidence::HIGH,
            ),
            edge(
                "pkg/beta.py::three",
                "pkg/alpha.py::one",
                EdgeKind::Calls,
                Confidence::HIGH,
            ),
            edge(
                "pkg/beta.py::four",
                "pkg/alpha.py::one",
                EdgeKind::Calls,
                Confidence::MEDIUM,
            ),
        ];
        (extractions, empty_analysis(), edges)
    }

    /// The round trip is the field-completeness proof.
    ///
    /// An encoder that drops a key, reorders a row, or loses a type produces a
    /// decoded value that is no longer equal to the model it came from. Nothing
    /// weaker would do: asserting on a hand-written list of expected fields
    /// tests the list, and the list is exactly what would go stale when the
    /// emitter grows a field.
    #[test]
    fn the_interned_encoding_round_trips_to_the_verbose_model_exactly() {
        let (extractions, analysis, edges) = compact_fixture();
        let verbose = graph(&extractions, &analysis, &edges);
        let compact = encode_compact(&verbose).unwrap();
        let decoded = decode_compact(&compact).unwrap();
        assert_eq!(
            decoded, verbose,
            "the interned encoding must lose nothing; a decoded graph that \
             differs from its model is a field the encoder failed to carry"
        );
    }

    /// Interning must actually pay, and it must pay on the tables that hold the
    /// bytes. A test that only round-trips would pass on an encoder that
    /// rewrote the artifact unchanged.
    #[test]
    fn the_interned_encoding_is_smaller_and_names_the_tables_it_interned() {
        let (extractions, analysis, edges) = compact_fixture();
        let verbose =
            generate_code_graph_json(&extractions, &analysis, &edges, &freshness(), None).unwrap();
        let (_, compact) = generate_code_graph_encodings(
            &extractions,
            &analysis,
            &edges,
            &freshness(),
            None,
            true,
        )
        .unwrap();
        let compact = compact.expect("compact was requested");
        assert!(
            compact.len() < verbose.len(),
            "interned {} bytes vs verbose {} bytes",
            compact.len(),
            verbose.len()
        );

        let value: Value = serde_json::from_str(&compact).unwrap();
        assert_eq!(value["encoding"], json!(CODE_GRAPH_COMPACT_ENCODING));
        assert_eq!(value["interned_tables"], json!(["edges", "nodes"]));
        assert_eq!(value["verbatim_tables"], json!([]));
        // The clobber guard reads `meta.map_engine`; an artifact that lost it
        // would be indistinguishable from a foreign one.
        assert_eq!(value["meta"]["map_engine"], json!(CONSUMER_MAP_ENGINE));
        // Every endpoint string appears once in the pool, not once per edge.
        let pool = value["strings"].as_array().unwrap();
        let occurrences = pool
            .iter()
            .filter(|entry| entry.as_str() == Some("pkg/alpha.py::one"))
            .count();
        assert_eq!(occurrences, 1, "the string table must deduplicate");
    }

    /// R4: build twice, byte-identical.
    ///
    /// The string table is sorted rather than first-seen precisely so this
    /// holds even if the emitter's visit order changes.
    #[test]
    fn the_interned_encoding_is_byte_identical_across_builds() {
        let (extractions, analysis, edges) = compact_fixture();
        let first = generate_code_graph_encodings(
            &extractions,
            &analysis,
            &edges,
            &freshness(),
            None,
            true,
        )
        .unwrap()
        .1;
        let second = generate_code_graph_encodings(
            &extractions,
            &analysis,
            &edges,
            &freshness(),
            None,
            true,
        )
        .unwrap()
        .1;
        assert_eq!(first, second);
    }

    /// A table whose rows are not uniformly shaped is copied through rather
    /// than interned, and the artifact says so.
    ///
    /// The alternative — interning per row against a spec taken from the first
    /// one — writes cells the decoder cannot place, and the failure surfaces as
    /// a graph with silently missing fields rather than as an error.
    #[test]
    fn a_ragged_table_is_carried_verbatim_and_named_rather_than_interned() {
        let payload = json!({
            "nodes": [
                {"id": "a", "kind": "function", "line": 1},
                {"id": "b", "kind": "function"},
            ],
            "edges": [
                {"source": "a", "target": "b", "kind": "calls"},
            ],
            "meta": {"map_engine": CONSUMER_MAP_ENGINE},
        });
        let compact = encode_compact(&payload).unwrap();
        assert_eq!(compact["interned_tables"], json!(["edges"]));
        assert_eq!(compact["verbatim_tables"], json!(["nodes"]));
        assert_eq!(compact["nodes"], payload["nodes"]);
        assert_eq!(decode_compact(&compact).unwrap(), payload);
    }

    /// Class A at the decoder: a decode that could not run must not look like
    /// one that ran and found an empty graph.
    ///
    /// Each of these was demonstrated red by removing the corresponding guard.
    #[test]
    fn the_decoder_refuses_a_damaged_artifact_rather_than_returning_part_of_one() {
        let (extractions, analysis, edges) = compact_fixture();
        let verbose = graph(&extractions, &analysis, &edges);
        let good = encode_compact(&verbose).unwrap();

        let mut wrong_version = good.clone();
        wrong_version["encoding"] = json!("devmap-compact-v99");
        let error = decode_compact(&wrong_version).unwrap_err().to_string();
        assert!(error.contains("devmap-compact-v99"), "{error}");

        let mut no_encoding = good.clone();
        no_encoding.as_object_mut().unwrap().remove("encoding");
        assert!(decode_compact(&no_encoding)
            .unwrap_err()
            .to_string()
            .contains("no `encoding`"));

        let mut no_pool = good.clone();
        no_pool.as_object_mut().unwrap().remove("strings");
        assert!(decode_compact(&no_pool)
            .unwrap_err()
            .to_string()
            .contains("no string table"));

        // An index one past the end of the pool: the shape a truncated string
        // table produces, and the one that would otherwise decode to whatever
        // string happened to sit at a wrapped offset.
        let mut short_pool = good.clone();
        let pool_len = short_pool["strings"].as_array().unwrap().len();
        short_pool["strings"] = json!(Vec::<String>::new());
        let error = decode_compact(&short_pool).unwrap_err().to_string();
        assert!(error.contains("points at string"), "{error}");
        assert!(pool_len > 0);

        // A row with a cell removed: arity is checked against the spec, not
        // zipped short.
        let mut ragged = good.clone();
        ragged["edges"]["rows"][0].as_array_mut().unwrap().pop();
        let error = decode_compact(&ragged).unwrap_err().to_string();
        assert!(error.contains("cells for"), "{error}");

        // A storage kind the decoder does not know is a refusal, not a guess.
        let mut unknown_storage = good;
        unknown_storage["edges"]["fields"][0] = json!(["source", "z"]);
        let error = decode_compact(&unknown_storage).unwrap_err().to_string();
        assert!(error.contains("unknown storage z"), "{error}");
    }

    /// The ratio is the point, and it has to be measured at a scale where the
    /// repetition exists.
    ///
    /// The small fixture above proves the encoding is *correct*; it proves
    /// almost nothing about whether it is *worth* anything, because a two-file
    /// graph has little repetition to collapse. This one builds a graph whose
    /// edge count dominates its node count — the real shape: DevCouncil is
    /// 14,488 nodes and 73,863 edges, so every symbol identity is written about
    /// ten times as an endpoint.
    ///
    /// The floor is deliberately well under the measured result. It is a
    /// regression gate against a column quietly losing its interning, not a
    /// pinned number that has to be re-tuned whenever the emitter changes.
    #[test]
    fn interning_pays_at_the_scale_the_artifact_is_actually_written_at() {
        const FILES: usize = 60;
        const FUNCTIONS: usize = 12;

        let mut extractions = Vec::with_capacity(FILES);
        for file in 0..FILES {
            let mut source = String::new();
            for function in 0..FUNCTIONS {
                source.push_str(&format!(
                    "def deeply_nested_handler_name_{file}_{function}():\n    pass\n\n"
                ));
            }
            extractions.push(extract_file(
                &format!("src/subsystem/package/module_with_a_long_path_{file}.py"),
                &source,
            ));
        }

        // Every function calls every function in the next file over: N*M edges
        // across a fixed identity set, which is what interning collapses.
        let mut edges = Vec::new();
        for file in 0..FILES {
            let next = (file + 1) % FILES;
            for function in 0..FUNCTIONS {
                for callee in 0..FUNCTIONS {
                    edges.push(edge(
                        &format!(
                            "src/subsystem/package/module_with_a_long_path_{file}.py::deeply_nested_handler_name_{file}_{function}"
                        ),
                        &format!(
                            "src/subsystem/package/module_with_a_long_path_{next}.py::deeply_nested_handler_name_{next}_{callee}"
                        ),
                        EdgeKind::Calls,
                        Confidence::HIGH,
                    ));
                }
            }
        }

        let analysis = empty_analysis();
        let verbose =
            generate_code_graph_json(&extractions, &analysis, &edges, &freshness(), None).unwrap();
        let (_, compact) = generate_code_graph_encodings(
            &extractions,
            &analysis,
            &edges,
            &freshness(),
            None,
            true,
        )
        .unwrap();
        let compact = compact.expect("compact was requested");

        let ratio = verbose.len() as f64 / compact.len() as f64;
        assert!(
            ratio >= 3.0,
            "interning recovered only {ratio:.2}x ({} verbose vs {} compact bytes) \
             over {} nodes and {} edges; a column has probably stopped being interned",
            verbose.len(),
            compact.len(),
            FILES * (FUNCTIONS + 1),
            edges.len()
        );

        // Correctness must survive the scale, not just the two-file fixture.
        let decoded = decode_compact(&serde_json::from_str(&compact).unwrap()).unwrap();
        let model: Value = serde_json::from_str(&verbose).unwrap();
        assert_eq!(decoded, model);
    }

    /// Two counts, because one name cannot honestly carry both.
    ///
    /// `edge_endpoints_without_node` counts *edges* with a dangling endpoint —
    /// the number the Go consumer decodes into `OrphanEndpoints`, documented
    /// there as "counts edges the producer wrote whose endpoints are not
    /// nodes". The name reads as a count of *endpoints*, and on this repository
    /// the two differ by more than 2x: 360 edges reference 167 distinct
    /// endpoints that have no node. Renaming the key would break the consumer
    /// that already reads it correctly, so the distinct count travels beside it
    /// instead and neither number can be mistaken for the other.
    #[test]
    fn a_dangling_endpoint_is_counted_once_per_edge_and_once_per_identity() {
        let extractions = [extract_file("m.py", "def caller(): pass\n")];
        // Three edges, two distinct absent targets: the arithmetic separates
        // the two counts, which an equal-count fixture could not.
        let edges = [
            edge(
                "m.py::caller",
                "m.py::gone",
                EdgeKind::Calls,
                Confidence::HIGH,
            ),
            edge(
                "m.py::caller",
                "m.py::gone",
                EdgeKind::References,
                Confidence::HIGH,
            ),
            edge(
                "m.py::caller",
                "m.py::alsogone",
                EdgeKind::Calls,
                Confidence::HIGH,
            ),
        ];
        let value = graph(&extractions, &empty_analysis(), &edges);
        let provenance = &value["meta"]["devmap_rust"];

        assert_eq!(
            provenance["edge_endpoints_without_node"],
            json!(3),
            "the existing key counts edges, and the Go consumer depends on that"
        );
        assert_eq!(
            provenance["distinct_edge_endpoints_without_node"],
            json!(2),
            "the number the key's name reads as must be carried too"
        );
    }

    /// A route is a node, and the edge naming it finds it.
    ///
    /// `HandlesRoute` names the route as its source and the handler as its
    /// target, and neither used to name a node: the source was a bare
    /// `"VERB path"` and the target a bare handler name. So every consumer
    /// that walks route nodes — `route_map`, `shape_check`, `api_impact` —
    /// read an empty graph out of a generation that had the routes in it, and
    /// the handler endpoint dangled beside it.
    ///
    /// The identity belongs to `ExtractedRoute::node_id`, so this runs the
    /// real resolver over a real file rather than restating the format: if the
    /// two sides ever disagree, the edge stops finding its node here.
    #[test]
    #[cfg(feature = "parse")]
    fn a_route_is_a_node_and_its_edge_endpoints_resolve() {
        use std::collections::BTreeSet;

        let source = "@app.route(\"/api/users/<uid>\", methods=[\"POST\"])\n\
                      def create_user(uid):\n    return uid\n";
        let dir = tmp_source_dir("api.py", source);
        let extractions = [extract_file("api.py", source)];
        assert_eq!(
            extractions[0].routes.len(),
            1,
            "fixture must extract one route, or this tests nothing: {:?}",
            extractions[0].routes
        );

        let mut resolver = devmap_resolve::Resolver::new();
        resolver.index_extractions(&extractions);
        let resolution = resolver.resolve_all(&extractions);

        let json = generate_code_graph_json(
            &extractions,
            &empty_analysis(),
            &resolution.edges,
            &freshness(),
            Some(dir.to_str().unwrap()),
        )
        .unwrap();
        let value: Value = serde_json::from_str(&json).unwrap();
        let nodes = value["nodes"].as_array().unwrap();

        let route = nodes
            .iter()
            .find(|node| node["kind"] == "route")
            .unwrap_or_else(|| panic!("the graph must carry a route node: {nodes:?}"));
        assert_eq!(route["path"], "api.py");
        assert_eq!(route["line"], 1, "a route's line is its decorator's");
        assert_eq!(route["extras"]["route"], "/api/users/<uid>");
        assert_eq!(route["extras"]["verb"], "POST");
        assert!(
            route["extras"]["framework"]
                .as_str()
                .is_some_and(|f| !f.is_empty()),
            "the node carries the framework the store drops: {route}"
        );

        let edges = value["edges"].as_array().unwrap();
        let routes_to = edges
            .iter()
            .find(|edge| edge["kind"] == "routes_to")
            .unwrap_or_else(|| panic!("the graph must carry a routes_to edge: {edges:?}"));
        assert_eq!(routes_to["source"], route["id"]);
        assert_eq!(routes_to["target"], "api.py::create_user");

        let ids: BTreeSet<&str> = nodes
            .iter()
            .map(|node| node["id"].as_str().unwrap())
            .collect();
        for endpoint in ["source", "target"] {
            let name = routes_to[endpoint].as_str().unwrap();
            assert!(
                ids.contains(name),
                "the route edge's {endpoint} {name:?} must name a node: {ids:?}"
            );
        }
        assert_eq!(
            value["meta"]["devmap_rust"]["edge_endpoints_without_node"],
            json!(0),
            "a bound route leaves no dangling endpoint"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Two files declaring the same verb and path are two nodes.
    ///
    /// The id carries the file for exactly this reason. Were it just
    /// `"VERB path"`, the second would collide with the first, be dropped as a
    /// duplicate, and leave its own `routes_to` edge pointing at the other
    /// file's route.
    #[test]
    #[cfg(feature = "parse")]
    fn the_same_route_in_two_files_is_two_nodes() {
        let source = "@app.get(\"/health\")\ndef ping():\n    return 1\n";
        let dir = tmp_source_dir("a.py", source);
        std::fs::write(dir.join("b.py"), source).unwrap();
        let extractions = [extract_file("a.py", source), extract_file("b.py", source)];

        let json = generate_code_graph_json(
            &extractions,
            &empty_analysis(),
            &[],
            &freshness(),
            Some(dir.to_str().unwrap()),
        )
        .unwrap();
        let value: Value = serde_json::from_str(&json).unwrap();
        let routes: Vec<&Value> = value["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|node| node["kind"] == "route")
            .collect();
        assert_eq!(routes.len(), 2, "one node per declaration: {routes:?}");
        assert_eq!(
            value["meta"]["devmap_rust"]["duplicate_node_ids_dropped"],
            json!(0)
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
