use super::{blast_layer_tokens, budget_take, edge_kind_name, stored_edge_to_resolved};
use crate::cancel::Cancel;
use crate::model::{BlastLayer, BlastRadius, ResolutionAvailability, UnresolvedNamesakes};
use devmap_analyze::traversal::TraversalStop;
use devmap_resolve::model::{LangFamily, ResolvedEdge};
use devmap_store::GenerationEdges;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Every node a walk reached, with the file it was reached in: its starts and
/// both endpoints of every edge it measured. One rule for both engines.
pub(super) fn reached_by(
    starts: BTreeSet<(String, String)>,
    edges: &[ResolvedEdge],
) -> BTreeSet<(String, String)> {
    let mut reached = starts;
    for edge in edges {
        reached.insert((edge.source_symbol.clone(), edge.source_file.clone()));
        reached.insert((edge.target_symbol.clone(), edge.target_file.clone()));
    }
    reached
}

/// The traversed identities, mapped back to the full edges they name.
///
/// Public so `examples/query_bench.rs` can time this phase of an `impact`
/// call against the real function rather than against a copy of it that
/// could drift from it.
pub fn traversed_resolution_edges(
    traversal: &devmap_analyze::traversal::TraversalResult,
    edges: &[ResolvedEdge],
    min_confidence: f32,
) -> Vec<ResolvedEdge> {
    // Borrowed keys. The set is built from the walk, which outlives this call,
    // and probed with slices of `edges`, which the caller owns — so the scan
    // allocates nothing. The previous spelling built an owned `(String, String,
    // String)` key for *every edge in the generation* purely to ask a question
    // and then dropped it: three allocations per edge, so ~2.0M per `impact`
    // on a 660,000-edge store, measured at 42.9 ms against 11.2 ms here
    // (`examples/query_phase_ab.rs`, hypothesis H2).
    //
    // `edge_kind_name` has to spell a kind exactly as `traverse_graph` recorded
    // it — that side uses `format!("{:?}", kind)`. If the two ever diverge this
    // filter matches nothing and `impact` answers "no callers" from a
    // comparison that never ran, which is the Class A failure this codebase
    // treats as worse than a visible gap. Both directions are pinned by
    // `edge_kind_name_is_the_spelling_traverse_graph_records`.
    let traversed: std::collections::BTreeSet<(&str, &str, &str)> = traversal
        .traversed_edges
        .iter()
        .map(|edge| {
            (
                edge.source.as_str(),
                edge.target.as_str(),
                edge.edge_kind.as_str(),
            )
        })
        .collect();
    edges
        .iter()
        .filter(|edge| {
            edge.confidence.0 >= min_confidence
                && traversed.contains(&(
                    edge.source_symbol.as_str(),
                    edge.target_symbol.as_str(),
                    edge_kind_name(edge.edge_kind),
                ))
        })
        .cloned()
        .collect()
}

/// Ceiling on nodes any one walk in this engine may visit.
///
/// Named because three surfaces now share it — `impact`, `trace` and the blast
/// radius — and a second literal would let one of them cap somewhere else while
/// reporting the first number in its `walk_incomplete` sentence.
pub(super) const TRAVERSAL_MAX_NODES: usize = 5_000;

/// Token cost the budgeter charges for one graph edge, everywhere.
pub(super) const EDGE_TOKENS: u32 = 25;
/// Token cost of one dead-symbol row, and so the divisor that turns a budget
/// into how many rows are worth reading.
pub(super) const DEAD_SYMBOL_TOKENS: u32 = 30;

/// Node ids listed per blast-radius band. The band's exact size travels in
/// `node_count` regardless, so this trims the listing, never the count.
pub(super) const BLAST_LAYER_NODE_SAMPLE: usize = 50;

/// Reached symbols listed per affected test file; `reached_symbols` stays exact.
pub(super) const AFFECTED_SYMBOL_SAMPLE: usize = 8;

/// Fixed per-definition cost in `explore`'s packer: identity, kind, span, score
/// and the two edge-response envelopes, before any source text.
pub(super) const EXPLORE_DEFINITION_OVERHEAD_TOKENS: u32 = 40;

/// The `file::symbol` identity every traversal surface resolves.
pub(super) fn node_id_of(file_path: &str, symbol_name: &str) -> String {
    if file_path.is_empty() {
        return symbol_name.to_string();
    }
    if symbol_name.is_empty() {
        return file_path.to_string();
    }
    // A stored symbol is usually qualified already (`file::name`). Prefixing
    // it again produced `file::file::name` — an id no traversal accepts, and
    // the one the ambiguous-name refusal told agents to paste back.
    if symbol_name == file_path
        || symbol_name
            .strip_prefix(file_path)
            .is_some_and(|rest| rest.starts_with("::"))
    {
        return symbol_name.to_string();
    }
    format!("{file_path}::{symbol_name}")
}

/// The edges a walk crossed, read out of the index instead of scanned for.
///
/// Same answer as [`traversed_resolution_edges`] over the whole generation,
/// and the same *set*: every stored edge whose `(source, target, kind)` the
/// walk crossed, including duplicates in other files, at or above the caller's
/// floor. What changes is the cost — the out-edges of the symbols the walk
/// actually reached, rather than every row in the generation.
///
/// Two thresholds, deliberately, because the code this replaces applied two:
/// `admits` is the store's rounded comparison, which decides what the walk was
/// allowed to cross, and the plain `>= min_confidence` below decides what the
/// answer may contain. An edge can pass the first and fail the second, and it
/// did before, so it still must.
pub(super) fn indexed_traversed_edges(
    index: &GenerationEdges,
    traversal: &devmap_analyze::traversal::TraversalResult,
    min_confidence: f32,
    cancel: &Cancel,
) -> anyhow::Result<Vec<ResolvedEdge>> {
    let traversed: std::collections::BTreeSet<(&str, &str, &str)> = traversal
        .traversed_edges
        .iter()
        .map(|edge| {
            (
                edge.source.as_str(),
                edge.target.as_str(),
                edge.edge_kind.as_str(),
            )
        })
        .collect();
    let sources: std::collections::BTreeSet<&str> =
        traversed.iter().map(|(source, _, _)| *source).collect();
    let mut ids: Vec<u32> = Vec::new();
    for (checked, source) in sources.iter().enumerate() {
        cancel.check_every(checked)?;
        for id in index.from_source_symbol(source) {
            if !index.admits(*id, min_confidence) || index.confidence(*id) < min_confidence {
                continue;
            }
            if traversed.contains(&(
                index.source_symbol(*id),
                index.target_symbol(*id),
                index.kind_label(*id),
            )) {
                ids.push(*id);
            }
        }
    }
    // Ascending ids are the generation's own edge order, which is the order the
    // scan this replaces produced and the final tie-break of the sort that
    // follows (R4).
    ids.sort_unstable();
    let mut edges = Vec::with_capacity(ids.len());
    for (checked, id) in ids.into_iter().enumerate() {
        cancel.check_every(checked)?;
        edges.push(stored_edge_to_resolved(index.stored_edge(id))?);
    }
    Ok(edges)
}

/// Traversal starts, found through the index rather than by scanning.
///
/// For path and `file::symbol` queries this is identical to the
/// [`traversal_starts`] scan it replaced — same pairs, same order and
/// duplicates. The traversal admits distinct seeds before applying its node
/// cap, so repeated edge endpoints cannot consume that capacity. What changes
/// is the cost: a symbol query tests the distinct symbols and a path query the
/// distinct files, instead of testing every edge.
///
/// A bare **symbol** query is different. Matching more than one definition
/// (distinct `(symbol, file)` pairs) is refused rather than walked: a union of
/// every namesake's blast radius is a wrong answer to "what does this change
/// reach". The error lists candidate `file::symbol` ids so the caller can
/// disambiguate. Search and explore still return the set; they do not go
/// through this function for their result lists.
///
/// Lifted out of `traverse` so the blast radius resolves its seeds through the
/// same matcher the traversal does. Resolving them two ways is how a radius
/// ends up seeded from a symbol the trace never visits.
/// What a ledger read for `impact` came back with.
pub(super) enum NamesakeRead {
    /// The target names no callee (a file or blank query).
    NotApplicable,
    /// The walk's generation is no longer retained, so no consistent read.
    GenerationGone,
    Read(UnresolvedNamesakes),
}

/// The name a call site would record for a symbol: the last segment of a
/// qualified name, past `::` and past `.`. `a/job.go::YoloJob.record` and
/// `YoloJob.record` both call `record`. `None` for an empty tail.
pub(super) fn bare_callee_name(symbol: &str) -> Option<&str> {
    let tail = symbol.rsplit("::").next().unwrap_or(symbol);
    let tail = tail.rsplit('.').next().unwrap_or(tail).trim();
    (!tail.is_empty()).then_some(tail)
}

/// The resolution family a repository path belongs to, by its language.
pub(super) fn family_of_path(path: &str) -> LangFamily {
    LangFamily::from_lang(devmap_extract::languages::detect_language(Path::new(path)))
}

pub(super) fn indexed_traversal_starts(
    index: &GenerationEdges,
    target: &str,
    reverse: bool,
    min_confidence: f32,
    cancel: &Cancel,
) -> anyhow::Result<Vec<(String, String)>> {
    let query = crate::query_match::classify(target);
    let mut ids: Vec<u32> = Vec::new();
    match query {
        crate::query_match::StartQuery::Nothing => {}
        crate::query_match::StartQuery::Qualified { file, symbol } => {
            for (checked, (candidate, group)) in index.symbols(reverse).enumerate() {
                cancel.check_every(checked)?;
                if !crate::query_match::symbol_matches(candidate, symbol) {
                    continue;
                }
                for id in group {
                    let path = if reverse {
                        index.target_file(*id)
                    } else {
                        index.source_file(*id)
                    };
                    if crate::query_match::path_matches(path, file) {
                        ids.push(*id);
                    }
                }
            }
        }
        crate::query_match::StartQuery::Path(path) => {
            for (checked, (candidate, group)) in index.files(reverse).enumerate() {
                cancel.check_every(checked)?;
                if crate::query_match::path_matches(candidate, path) {
                    ids.extend_from_slice(group);
                }
            }
        }
        crate::query_match::StartQuery::Symbol(name) => {
            for (checked, (candidate, group)) in index.symbols(reverse).enumerate() {
                cancel.check_every(checked)?;
                if crate::query_match::symbol_matches(candidate, name) {
                    ids.extend_from_slice(group);
                }
            }
        }
    }
    ids.retain(|id| index.admits(*id, min_confidence));
    ids.sort_unstable();
    let starts: Vec<(String, String)> = ids
        .into_iter()
        .map(|id| {
            if reverse {
                (
                    index.target_symbol(id).to_string(),
                    index.target_file(id).to_string(),
                )
            } else {
                (
                    index.source_symbol(id).to_string(),
                    index.source_file(id).to_string(),
                )
            }
        })
        .collect();
    // Bare names only. A file seed expanding to every member, or a qualified
    // `file::symbol`, is a deliberate scope — unioning those is correct.
    if matches!(query, crate::query_match::StartQuery::Symbol(_)) {
        let unique: BTreeSet<&(String, String)> = starts.iter().collect();
        if unique.len() > 1 {
            let candidates: Vec<String> = unique
                .into_iter()
                .map(|(symbol, file)| node_id_of(file, symbol))
                .collect();
            anyhow::bail!(
                "ambiguous symbol '{target}'; use an exact ID or file::symbol; candidates: {}",
                candidates.join(", ")
            );
        }
    }
    Ok(starts)
}

pub fn traversal_starts(
    edges: &[ResolvedEdge],
    target: &str,
    reverse: bool,
) -> Vec<(String, String)> {
    edges
        .iter()
        .filter(|edge| {
            if reverse {
                crate::query_match::traversal_start_matches(
                    target,
                    &edge.target_symbol,
                    &edge.target_file,
                )
            } else {
                crate::query_match::traversal_start_matches(
                    target,
                    &edge.source_symbol,
                    &edge.source_file,
                )
            }
        })
        .map(|edge| {
            if reverse {
                (edge.target_symbol.clone(), edge.target_file.clone())
            } else {
                (edge.source_symbol.clone(), edge.source_file.clone())
            }
        })
        .collect()
}

/// One distance band of a [`BlastWalk`], before sampling or budgeting.
///
/// `members` pairs each reached symbol with the file the reaching edge named,
/// so a derived answer never has to guess which file a qualified name lives in.
pub(super) struct BlastBand {
    pub(super) depth: usize,
    pub(super) members: BTreeSet<(String, String)>,
    pub(super) lowest_confidence: Option<f32>,
    pub(super) node_count: u32,
}

/// The complete result of an inbound walk: every band, unsampled, unbudgeted.
///
/// Separated from [`BlastRadius`] because two consumers want different things
/// from it. `affected_tests` needs everything the walk reached — deriving its
/// answer from a trimmed list would drop tests without any counter saying so.
/// `explore` needs something that fits a token budget. Presentation is
/// [`Self::into_radius`]; derivation reads the bands directly.
pub(super) struct BlastWalk {
    pub(super) seeds: Vec<(String, String)>,
    pub(super) unmatched: Vec<String>,
    pub(super) bands: Vec<BlastBand>,
    pub(super) total_impacted: u32,
    pub(super) stop: TraversalStop,
    pub(super) depth_cap: usize,
    /// True when no target resolved to a traversal start at all — an answer of
    /// "nothing is impacted" that nothing actually looked for.
    pub(super) unresolved_seeds: bool,
    pub(super) coverage_gap: Option<String>,
}

impl BlastWalk {
    /// Why the walk is a lower bound, or `None` when it ran to completion.
    pub(super) fn incomplete_reason(&self) -> Option<String> {
        devmap_analyze::combine_reasons(
            self.stop.reason(self.depth_cap, TRAVERSAL_MAX_NODES),
            self.coverage_gap.clone(),
        )
    }

    /// Sample each band and pack the bands into `token_budget`.
    ///
    /// The two trims are reported separately and neither touches a count:
    /// `nodes_omitted` per band, `hidden`/`truncated` for the band list.
    pub(super) fn into_radius(self, token_budget: u32) -> BlastRadius {
        let incomplete = self.incomplete_reason();
        let layers: Vec<BlastLayer> = self
            .bands
            .into_iter()
            .map(|band| {
                let nodes: Vec<String> = band
                    .members
                    .iter()
                    .take(BLAST_LAYER_NODE_SAMPLE)
                    .map(|(symbol, _)| symbol.clone())
                    .collect();
                BlastLayer {
                    depth: band.depth,
                    nodes_omitted: band
                        .node_count
                        .saturating_sub(u32::try_from(nodes.len()).unwrap_or(u32::MAX)),
                    nodes,
                    node_count: band.node_count,
                    lowest_confidence: band.lowest_confidence,
                }
            })
            .collect();
        let mut response = budget_take(layers, token_budget, blast_layer_tokens);
        if self.unresolved_seeds {
            response.resolution = ResolutionAvailability::Unavailable {
                reason: "no target matched an indexed traversal start".to_string(),
            };
        }
        response.walk_incomplete = incomplete;
        BlastRadius {
            seeds: self.seeds.into_iter().map(|(symbol, _)| symbol).collect(),
            unmatched_targets: self.unmatched,
            layers: response,
            total_impacted: self.total_impacted,
        }
    }
}

/// Band the endpoints of one traversal's edges by the hop that reached them.
///
/// The distance a traversal found a node at is not recoverable from an edge
/// list — an edge carries two endpoints and no hop count — which is why every
/// consumer that wanted bands had to guess, and why the guess in
/// `graph_cmd.py` labelled a three-hop dependent `depth: 1`. This recovers it
/// the only way it can be recovered honestly: by re-deriving shortest distance
/// over the edges the walk actually crossed.
///
/// **It is a partition of `edges`, not a second walk.** Everything it can name
/// is an endpoint of an edge already in this answer, so the bands and the edge
/// list cannot describe different graphs, cannot apply different direction
/// rules, and cannot come from different generations. That is the property
/// [`StoreQueryEngine::blast_walk`] cannot offer here: it walks the index
/// directly, without the traversal's reverse-direction exclusions, so it names
/// the *file* that contains the seed and everything importing it.
///
/// Breadth-first over the crossed edges reproduces the walk's own depths
/// exactly, because the walk is itself breadth-first and records the edge that
/// first reached each node — the one exception being a walk that hit the
/// recorded-edge cap, which is what `walk_incomplete` is carrying when it says
/// so.
pub(super) fn blast_radius_from_edges(
    seeds: &[String],
    edges: &[ResolvedEdge],
    reverse: bool,
    depth_cap: usize,
    token_budget: u32,
    walk_incomplete: Option<String>,
) -> BlastRadius {
    // Borrowed keys and a B-tree for the same reasons `AdjacencyIndex` uses
    // them: the edges outlive this call, and a deterministic iteration order is
    // what makes the sampled `nodes` list reproducible.
    let mut adjacency: BTreeMap<&str, Vec<(&str, f32)>> = BTreeMap::new();
    for edge in edges {
        let (from, to) = if reverse {
            (edge.target_symbol.as_str(), edge.source_symbol.as_str())
        } else {
            (edge.source_symbol.as_str(), edge.target_symbol.as_str())
        };
        adjacency
            .entry(from)
            .or_default()
            .push((to, edge.confidence.0));
    }

    let seed_set: BTreeSet<&str> = seeds.iter().map(String::as_str).collect();
    let mut visited: BTreeSet<&str> = seed_set.clone();
    let mut frontier: Vec<&str> = seed_set.iter().copied().collect();
    let mut layers: Vec<BlastLayer> = Vec::new();
    let mut total_impacted: u32 = 0;

    for depth in 1..=depth_cap {
        let mut members: BTreeSet<&str> = BTreeSet::new();
        let mut lowest: Option<f32> = None;
        for node in &frontier {
            for (next, confidence) in adjacency.get(node).map(Vec::as_slice).unwrap_or_default() {
                if visited.contains(next) {
                    continue;
                }
                members.insert(next);
                // The weakest edge that reached anything in this band. A radius
                // held together by name-only attribution must not read like one
                // built from resolved calls.
                lowest = Some(match lowest {
                    Some(current) => current.min(*confidence),
                    None => *confidence,
                });
            }
        }
        if members.is_empty() {
            break;
        }
        let node_count = u32::try_from(members.len()).unwrap_or(u32::MAX);
        total_impacted = total_impacted.saturating_add(node_count);
        let nodes: Vec<String> = members
            .iter()
            .take(BLAST_LAYER_NODE_SAMPLE)
            .map(|node| (*node).to_string())
            .collect();
        layers.push(BlastLayer {
            depth,
            nodes_omitted: node_count
                .saturating_sub(u32::try_from(nodes.len()).unwrap_or(u32::MAX)),
            nodes,
            node_count,
            lowest_confidence: lowest,
        });
        visited.extend(members.iter().copied());
        frontier = members.into_iter().collect();
    }

    let mut response = budget_take(layers, token_budget, blast_layer_tokens);
    response.walk_incomplete = walk_incomplete;
    BlastRadius {
        seeds: seed_set.into_iter().map(str::to_string).collect(),
        // Every seed here came from `indexed_traversal_starts` and therefore
        // matched. The unmatched case never reaches this function — it returns
        // an `Unavailable` radius one level up, where the caller's own text is
        // still in hand to name.
        unmatched_targets: Vec::new(),
        layers: response,
        total_impacted,
    }
}
