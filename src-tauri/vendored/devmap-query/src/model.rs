use devmap_resolve::model::ResolvedEdge;
use serde::{Deserialize, Serialize};

pub struct Budget;

impl Budget {
    pub const SEARCH: u32 = 2000;
    pub const DEPS: u32 = 2000;
    pub const DEAD: u32 = 2000;
    pub const MANIFEST: u32 = 2000;
    /// `explore` is four answers in one — ranked definitions with their source,
    /// both call-graph directions per definition, and a layered blast radius —
    /// and every one of them is paid for out of this single number (see
    /// [`ExploreBudget`]). At the 2000 the single-answer surfaces use, the
    /// definition list alone consumes the whole allowance on a repository with
    /// ordinary function bodies and the edge lists come back empty-but-counted.
    /// Four times that keeps a five-definition answer readable while staying
    /// far under the daemon's 100,000 ceiling.
    pub const EXPLORE: u32 = 8000;
    /// `affected` returns one row per test file plus its blast radius; the rows
    /// are small, so the single-answer default is the right size.
    pub const AFFECTED: u32 = 2000;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request<Q> {
    pub query: Q,
    pub token_budget: u32, // T1-T4: required token budget primitive
    pub min_confidence: f32,
    pub max_depth: usize,
}

/// Both call-graph directions for one target, answered in a single pass.
///
/// The MCP `graph_query` view needs callers *and* callees for each of the
/// first few definitions a search returns. Asking for them one at a time cost
/// two round trips per definition — eleven for a five-definition view — and
/// under the CLI transport a round trip is a process spawn, which is why that
/// view stayed at ~1.1 s no matter how fast the store got. The composition is
/// the cost, so the composition is what moved into the kernel.
///
/// `callers` and `callees` are whole [`Response`] values, not bare edge lists,
/// so each direction keeps its own `resolution`, budget counters and
/// `walk_incomplete`. A target whose inbound walk was capped must not be
/// readable as one that has no callers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Neighbors {
    /// Echoed verbatim from the request, so a caller can align the answers
    /// with what it asked for without assuming the kernel preserved order.
    pub target: String,
    /// Inbound edges — what reaches this target.
    pub callers: Response<ResolvedEdge>,
    /// Outbound edges — what this target reaches.
    pub callees: Response<ResolvedEdge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolutionAvailability {
    Available,
    Unavailable { reason: String },
}

/// Whole-tree source freshness attached to a query envelope.
///
/// Status is the surface that *runs* the check. Queries carry the last
/// verified verdict for the generation they answered from when the store
/// knows one, or an explicit reason when they do not. `fresh: null` alone is
/// not enough — a check that did not run must say so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFreshness {
    /// `Some(true/false)` when verified against the working tree; `None` when
    /// this answer did not (or could not) verify.
    pub fresh: Option<bool>,
    /// Generation the verdict describes, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_id: Option<u32>,
    /// Why `fresh` is null, or why a verified mismatch was reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl SourceFreshness {
    pub fn unverified(reason: impl Into<String>) -> Self {
        Self {
            fresh: None,
            generation_id: None,
            reason: Some(reason.into()),
        }
    }

    pub fn verified(fresh: bool, generation_id: u32) -> Self {
        Self {
            fresh: Some(fresh),
            generation_id: Some(generation_id),
            reason: None,
        }
    }

    pub fn verified_with_reason(
        fresh: bool,
        generation_id: u32,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            fresh: Some(fresh),
            generation_id: Some(generation_id),
            reason: Some(reason.into()),
        }
    }

    pub fn from_store(value: devmap_store::QuerySourceFreshness) -> Self {
        Self {
            fresh: value.fresh,
            generation_id: value.generation_id,
            reason: value.reason,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response<T> {
    /// Last verified whole-tree freshness for the generation behind this
    /// answer, or an explicit unverified reason. Never infer freshness from an
    /// empty result or from `walk_incomplete` being absent.
    pub source_freshness: SourceFreshness,
    pub items: Vec<T>,
    pub shown: u32,
    pub hidden: u32,
    pub total: u32, // what a complete answer would have held
    pub truncated: bool,
    pub tokens_used: u32,
    pub resolution: ResolutionAvailability,
    /// Set when the *producer* of `items` stopped early, as distinct from the
    /// token budgeter trimming a complete set.
    ///
    /// `shown`/`hidden`/`total`/`truncated` describe the budget, and clients
    /// enforce `shown + hidden == total` against them, so a walk that withheld
    /// an unknown quantity cannot be expressed there without breaking that
    /// invariant. It is `None` on a complete answer and omitted from the wire
    /// form, so nothing changes for a query that ran to completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub walk_incomplete: Option<String>,
    /// How the edges behind this answer were distributed across the resolution
    /// ladder, **before** any `min_rung` floor was applied.
    ///
    /// The population, not the survivors: its job is to answer "what did I not
    /// see", which a histogram of what came back cannot do. Without it,
    /// `min_rung: deterministic` returning three edges is indistinguishable
    /// from a graph that only had three — and the second reading is the one
    /// that leads somebody to conclude the code is unreferenced.
    ///
    /// `None` on a query that does not walk edges, and omitted from the wire
    /// form, so nothing changes for callers that predate it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rungs: Option<crate::rung::RungHistogram>,
    /// Abandoned cycles found in the same generation as `items`.
    ///
    /// Set by `dead_symbols` alone, exactly as `rungs` is set only by the
    /// edge-walking queries: this envelope is the answer plus whatever that
    /// answer's producer also knows, and a second shape for "a dead-code answer"
    /// is a second thing for every consumer to miss.
    ///
    /// **It had no consumer at all.** `dead_clusters.rs` is the highest-recall
    /// pass in the analysis — the one that finds subsystems a one-hop inbound
    /// join structurally cannot see — and its output reached two artifacts and
    /// no query path: not `StoreQueryEngine::dead_symbols`, not `devmap dead`,
    /// not the IPC `Dead` response, not `DevMapClient`, not the MCP envelope,
    /// not `dev graph dead`. `CodeGraph` had no field for it, so pydantic
    /// dropped it at load. The only reader in the repository was a benchmark
    /// script.
    ///
    /// `None` means the generation could not be read for clusters — it predates
    /// the pass, or has no analysis row. An empty `Vec` means the pass ran and
    /// found none, which is a finding. The two must not render alike.
    ///
    /// Bounded at the source (`DEAD_CLUSTER_CAP` × `DEAD_CLUSTER_MEMBER_CAP`),
    /// so it needs no budget of its own; `dead_clusters_truncated` carries what
    /// the cap left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dead_clusters: Option<Vec<devmap_analyze::DeadClusterReport>>,
    /// Clusters found but not listed, because of `DEAD_CLUSTER_CAP`.
    ///
    /// Beside the list rather than folded into `hidden`, which counts what the
    /// *token budget* trimmed. A capped producer and a trimmed page are
    /// different failures and `shown + hidden == total` is enforced against the
    /// second.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dead_clusters_truncated: usize,
    /// Why `dead_clusters` is absent, when the pass ran and refused.
    ///
    /// The scan has three outcomes and `Option<Vec<_>>` holds two. A graph
    /// past `DEAD_CLUSTER_MAX_NODES` comes back with an empty `clusters` and a
    /// `refused_oversized_graph` flag, so mapping the struct field-for-field
    /// would render "too large to walk" as `Some([])` — *the pass ran and found
    /// no abandoned subsystems* — which is the strongest possible reading of
    /// the weakest possible evidence, and the one that gets acted on.
    ///
    /// So a refusal sets `dead_clusters` to `None`, which every consumer
    /// already reads as "not measured", and puts the reason here. Old consumers
    /// fail closed on the absence; new ones can say why. Parallel to
    /// `walk_incomplete`, which does the same job for `items` — kept separate
    /// because the two producers fail independently and a caller that conflated
    /// them would report a complete dead-symbol list as partial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dead_clusters_incomplete: Option<String>,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// What a map query cost, against what answering it by reading files would have.
///
/// Every number here is a count of bytes divided by [`BYTES_PER_TOKEN`], not a
/// tokenizer's output. That is an estimate and is labelled one; a real count
/// depends on the model doing the reading, and quoting a precise-looking
/// figure derived from a divisor would be a fabricated precision.
///
/// The comparison is deliberately conservative. `files_tokens` is the cost of
/// reading only the files the map *already named* — it does not charge the
/// alternative for the work of finding them, which without an index means a
/// grep over the tree and reading candidates that turn out not to match. So the
/// reported saving is a floor, not a best case, and the field names say which
/// side is which rather than presenting a single triumphant ratio.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavingsReport {
    /// How token counts here were derived. Always an estimate.
    pub basis: String,
    /// Files in the latest generation.
    pub indexed_files: usize,
    /// Bytes of indexed source that could be read from disk.
    pub corpus_bytes: u64,
    /// Indexed files that could not be read to size them — deleted, moved, or
    /// unreadable since the generation was built. Carried so `corpus_bytes` is
    /// never mistaken for a complete measurement of the tree.
    pub corpus_files_unreadable: usize,
    /// Size of the artifact agents are instructed to open, when it exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_map_bytes: Option<u64>,
    /// Present only when a query was named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<QuerySavings>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuerySavings {
    pub query: String,
    /// Symbols the query returned within its budget.
    pub hits: usize,
    /// What the map's answer cost, as the engine itself accounted for it.
    pub answer_tokens: u32,
    /// Distinct files the answer pointed into.
    pub files_named: usize,
    /// Bytes of those files. The alternative this is compared against is
    /// "read the files the map named" — nothing cheaper would answer the same
    /// question, and anything an unindexed reader did would cost more.
    pub files_bytes: u64,
    /// Files among those named that could not be read to size them.
    pub files_unreadable: usize,
}

/// How a symbol differs between the indexed file and a candidate buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PreviewChange {
    /// Not in the indexed file. Nothing can be depending on it yet.
    Added,
    /// In the indexed file, absent from the buffer. Callers break.
    Removed,
    /// Declaration changed. Callers may break — this is the interesting case.
    SignatureChanged,
    /// Same declaration, different body. Callers still compile; behaviour moved.
    BodyChanged,
    /// Changed, and nothing could separate the declaration from the body —
    /// the grammar exposes no body field on this construct. Grouped with the
    /// caller-affecting changes, because reporting a break that did not happen
    /// costs a reader a look, and missing one costs them a build.
    Changed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewSymbol {
    pub symbol_name: String,
    pub qualified_name: String,
    pub kind: String,
    pub change: PreviewChange,
    /// The indexed declaration, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was: Option<String>,
    /// The buffer's declaration, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewCaller {
    /// The symbol in the edited file that this call targets.
    pub target_symbol: String,
    pub caller_file: String,
    pub caller_symbol: String,
    pub confidence: f32,
}

/// What a candidate edit would do to the graph, computed without writing it.
///
/// The delta is withheld entirely when the buffer does not parse. A file that
/// fails to parse yields no symbols, so a naive diff reports every symbol in it
/// as removed and every caller as breaking — the most alarming possible output,
/// produced by a typo. `delta_available` is the gate, and `parse_status` says
/// why when it is false.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewReport {
    pub file_path: String,
    /// `Clean`, `Partial`, `Fallback` or `Failed`, from the buffer's parse.
    pub parse_status: String,
    /// False when the buffer did not parse well enough to diff against.
    pub delta_available: bool,
    /// Whether the file is in the latest generation. Governs whether the
    /// caller graph below has anything to say about it; the symbol delta does
    /// not come from the index.
    pub file_is_indexed: bool,
    /// What the buffer was diffed against: `disk` (the file's current content),
    /// `nothing` (no such file, so every symbol is an addition), or
    /// `unreadable` (the file exists and could not be read).
    ///
    /// `unreadable` is a distinct value rather than a reuse of `nothing`
    /// because the two license opposite conclusions: `nothing` means there was
    /// genuinely no prior content, so "every symbol is an addition" is true;
    /// `unreadable` means the comparison did not happen, and reporting it as
    /// `nothing` made a real removal disappear behind a clean bill of health.
    /// `delta_available` is `false` whenever this is `unreadable`.
    pub compared_against: String,
    /// Set when the delta is reported but should be read with care — a partial
    /// parse can hide a symbol and make it look removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
    pub symbols: Vec<PreviewSymbol>,
    /// Symbols present in both versions with an identical declaration, whose
    /// *bodies* nothing compared — one side or the other carries no body
    /// signature, because the body is under the size floor or the file has no
    /// linked grammar.
    ///
    /// A count rather than a row each: on a real file most symbols are small,
    /// and a line per uncompared accessor would bury the handful of findings
    /// the report exists for. But it is not nothing, either — these symbols
    /// were not found to be unchanged, they were not examined — so the number
    /// is carried instead of dropped.
    #[serde(default)]
    pub bodies_not_compared: usize,
    /// Call edges into the affected symbols that fell below the confidence
    /// floor, and so are not listed above.
    ///
    /// Almost always name-only attribution — the resolver matching a bare
    /// `.get(...)` to every `get` in the tree. Counted so that "no callers
    /// affected" cannot quietly mean "none we were willing to vouch for".
    #[serde(default)]
    pub ambiguous_callers: usize,
    /// Calls from other files into symbols this edit removes or re-declares.
    pub broken_callers: Response<PreviewCaller>,
}

/// A duplicate-code report, with the coverage it was computed over.
///
/// The coverage is not a footnote. `groups: []` on its own is unreadable: it
/// says the same thing for a tree with no duplication and for a tree where
/// nothing could be signed, and those call for opposite reactions. Carrying the
/// denominator makes the difference visible without the reader having to know
/// how signing works.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloneReport {
    pub groups: Response<devmap_analyze::CloneGroup>,
    /// Symbols that carried a body signature.
    pub signed_symbols: usize,
    /// Symbols with none: below the size floor, a kind with no comparable body,
    /// or a file no grammar parsed.
    pub unsigned_symbols: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolHit {
    pub symbol_name: String,
    pub file_path: String,
    pub kind: String,
    pub span: (u32, u32),
    pub source_span: String, // R2: verbatim source lines grouped by file
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_unavailable_reason: Option<String>,
    /// Bytes of the symbol's source that were **not** included in
    /// `source_span`, when it had to be capped to fit the token budget.
    ///
    /// `None` means `source_span` is the whole symbol, which is what R2's
    /// "verbatim" promises. A capped span that said nothing would break that
    /// promise silently — a consumer would read a truncated function body as
    /// the complete one — so the omission is reported rather than implied.
    /// Serialized only when it happened, so existing consumers see no new key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_span_omitted_bytes: Option<u32>,
    pub score: f32,
}

/// One distance band of an inbound blast radius.
///
/// `nodes` is what the walk reached at exactly this depth; `node_count` is how
/// many it reached, which stays exact even when the band's list was trimmed to
/// fit the budget. `lowest_confidence` is the weakest edge that reached
/// anything in the band — a blast radius held together by name-only
/// attribution should not read like one built from resolved calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlastLayer {
    pub depth: usize,
    pub nodes: Vec<String>,
    /// Nodes first reached at this depth, before any per-layer trimming.
    pub node_count: u32,
    /// Nodes omitted from `nodes` by the per-layer cap. Zero means the list is
    /// the whole band.
    #[serde(default)]
    pub nodes_omitted: u32,
    /// Weakest confidence among the edges that reached this band. `None` when
    /// the band is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lowest_confidence: Option<f32>,
}

/// What an inbound walk from a set of seeds reaches, banded by distance.
///
/// Distance is the point of the shape: "42 symbols are affected" is far less
/// useful than "3 call it directly and 39 are reached through those 3". The
/// kernel's [`crate::StoreQueryEngine::impact`] answers reachability as a flat
/// edge list; this answers *how far*, which is what a blast radius is for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlastRadius {
    /// Node ids the walk started from, after matching the caller's targets
    /// against the graph.
    pub seeds: Vec<String>,
    /// Targets that matched no traversal start. Carried rather than dropped:
    /// a blast radius computed from two of three targets must never read as
    /// one computed from all three.
    #[serde(default)]
    pub unmatched_targets: Vec<String>,
    pub layers: Response<BlastLayer>,
    /// Distinct nodes reached, excluding the seeds themselves.
    pub total_impacted: u32,
}

/// An `impact` answer together with how far each reached symbol actually is.
///
/// [`crate::StoreQueryEngine::impact`] answers *what* reaches a target, as a
/// flat edge list. That is the whole answer to "show me the callers" and it is
/// not the answer to "how big is this change": a consumer needing distance
/// bands had to invent them, and the one in this repository invented them
/// wrongly — it relabelled everything a depth-3 reverse walk reached as
/// `depth: 1, confidence: extracted`, publishing a three-hop transitive
/// dependent as a direct, deterministically-resolved caller.
///
/// So the bands come from the kernel, and they come from the [`BlastRadius`]
/// that `explore` and `affected` already return rather than from a second shape
/// that could drift from it. `edges` is flattened, so the wire form is exactly
/// the `impact` object every existing consumer reads plus one new key: a client
/// that predates this reads the response it always read.
///
/// Both halves are budgeted out of the *same* allowance — a caller that asked
/// for 2,000 tokens gets 2,000, not 2,000 per half. See
/// [`crate::StoreQueryEngine::impact_layered`] for the split and for why this
/// surface takes no rung floor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayeredImpact {
    /// Exactly what `impact` returns, under its own share of the budget.
    #[serde(flatten)]
    pub edges: Response<ResolvedEdge>,
    /// The same walk, banded by distance from the seeds.
    pub blast_radius: BlastRadius,
}

/// One matched definition, with its source and both call-graph directions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExploreDefinition {
    /// `file::symbol`, the identity every other devmap surface uses.
    pub id: String,
    pub symbol_name: String,
    pub qualified_name: String,
    pub file_path: String,
    pub kind: String,
    /// Language recorded for the file in this generation. `None` means the
    /// generation holds no row for it — "not recorded", never "no language".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// 1-based inclusive line range, or `(0, 0)` when the source could not be
    /// read — in which case `source_unavailable_reason` says why.
    pub span: (u32, u32),
    pub source: String,
    /// Set when the file behind this definition could not be read at query
    /// time. An empty `source` with no reason means the symbol's span is
    /// genuinely empty; an empty `source` *with* a reason means nothing was
    /// examined. Those are different answers and must not share a shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_unavailable_reason: Option<String>,
    /// Bytes of the symbol's source omitted to fit the budget. Absent means
    /// `source` is the whole span.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_omitted_bytes: Option<u32>,
    pub score: f32,
    /// Inbound edges. A whole [`Response`], so `total` stays exact even when
    /// the budget bought no items — "0 shown of 42" is not "no callers".
    pub callers: Response<ResolvedEdge>,
    /// Outbound edges, on the same terms.
    pub callees: Response<ResolvedEdge>,
}

/// How one caller-supplied token budget was divided across `explore`'s parts.
///
/// Published rather than implied. `explore` returns four things and a single
/// `tokens_used` on the definition list would describe only one of them; a
/// reader who cannot see the split cannot tell a thin edge list caused by the
/// graph from one caused by the allowance.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ExploreBudget {
    /// What the caller asked for.
    pub total: u32,
    /// Share spent packing definitions and their source spans.
    pub definitions: u32,
    /// Share each caller/callee list of each returned definition may spend.
    pub edges_per_direction: u32,
    /// Share spent on the blast radius layers.
    pub blast_radius: u32,
}

/// Definitions matching a query, their neighbourhoods, and their blast radius.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExploreReport {
    pub query: String,
    /// Ranked before truncation, and `total` is the measured match count for
    /// the whole index — not the size of the page the budget could show.
    pub definitions: Response<ExploreDefinition>,
    /// Cap the caller asked for, echoed so a short list is attributable.
    pub limit: u32,
    pub blast_radius: BlastRadius,
    pub budget: ExploreBudget,
}

/// A test file the inbound walk reached, and how far away it was.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AffectedTest {
    pub path: String,
    /// Shortest distance from any seed to a symbol in this file. `0` means the
    /// target itself lives in a test file.
    pub depth: usize,
    /// Symbols in this file the walk reached. Bounded; `reached_symbols` is the
    /// exact count.
    pub symbols: Vec<String>,
    pub reached_symbols: u32,
}

/// Test files reachable through the inbound blast radius of some targets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AffectedTestsReport {
    pub targets: Vec<String>,
    /// Ranked nearest-first, then by path, before truncation.
    pub tests: Response<AffectedTest>,
    pub blast_radius: BlastRadius,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub subsystems: Vec<SubsystemEntry>,
    pub entry_roots: Vec<String>,
    pub important_files: Vec<String>,
    pub freshness: FreshnessInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubsystemEntry {
    pub name: String,
    pub path: String,
    pub entry_points: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreshnessInfo {
    pub head_sha: String,
    pub generation_id: u32,
    pub pending_count: usize,
    /// Freshness fields the *caller* computed, stamped verbatim into both
    /// artifacts when present.
    ///
    /// The kernel cannot compute these — they are SHA-1 digests over the git
    /// file set and no hashing crate is linked in this workspace — and it must
    /// not invent them, because an empty `indexed_hash` that reads as a
    /// computed answer is precisely the confusion `meta.devmap_rust.unavailable`
    /// exists to prevent. So each is `None` until a caller supplies a real
    /// value, and the corresponding "unavailable" marker is emitted only while
    /// it stays `None`.
    ///
    /// This lives on `FreshnessInfo` rather than being passed beside it so that
    /// the map and the graph cannot be stamped from different sources: one
    /// freshness identity, written once, for both artifacts.
    #[serde(default)]
    pub stamped: StampedFreshness,
}

/// Freshness digests supplied by the caller, each independently optional.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StampedFreshness {
    pub generated_head: Option<String>,
    pub indexed_hash: Option<String>,
    pub content_fingerprint: Option<String>,
}

impl StampedFreshness {
    /// Whether any digest was supplied. Used to decide whether the artifact is
    /// carrying caller-computed freshness at all.
    pub fn is_empty(&self) -> bool {
        self.generated_head.is_none()
            && self.indexed_hash.is_none()
            && self.content_fingerprint.is_none()
    }
}

impl FreshnessInfo {
    /// Construct with no caller-supplied digests — the kernel-only identity.
    pub fn new(head_sha: String, generation_id: u32, pending_count: usize) -> Self {
        Self {
            head_sha,
            generation_id,
            pending_count,
            stamped: StampedFreshness::default(),
        }
    }

    /// `generated_head` as it should appear in an artifact: the caller's value
    /// when it supplied one, otherwise the newest persisted generation's head.
    ///
    /// These answer different questions and the difference is load-bearing.
    /// The kernel's `head_sha` is the head of the last generation actually
    /// persisted; a consumer's staleness check asks whether the artifact
    /// describes the tree at the *current* `git rev-parse HEAD`. An incremental
    /// build that finds nothing changed used to persist no generation, so after
    /// a commit touching nothing indexed the kernel value pointed at the
    /// previous commit and the map read stale the moment it was written. The
    /// skip path now restamps that generation's `head_sha` once file hashes
    /// have proved the tree unchanged, so the two agree.
    pub fn generated_head(&self) -> &str {
        self.stamped
            .generated_head
            .as_deref()
            .unwrap_or(&self.head_sha)
    }
}
