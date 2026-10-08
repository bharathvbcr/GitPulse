use devmap_analyze::clones::group_clones;
use devmap_analyze::traversal::{traverse_graph_indexed, TraversalLimits, TraversalStop};
use devmap_extract::model::*;
use devmap_resolve::model::*;
use devmap_store::{GenerationEdges, Store, StoredEdge, StoredSymbol};

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::cancel::Cancel;
use crate::model::*;

// The engine's helpers, by area. Each item keeps the path it had when it was
// defined in this file: public ones are re-exported below, and private ones are
// `pub(super)` there and imported here, so `engine::` names are unchanged.
mod budget;
mod gaps;
mod hits;
mod in_memory;
mod trace;
mod traversal;
mod workspace;

use self::budget::{
    affected_test_tokens, atomic_budget_take, attach_source_freshness, blast_layer_tokens,
    budget_page_size, edges_per_direction, explore_budget, explore_definition_tokens,
    record_test_hit, search_page_size, search_rank_pool_size, unavailable_response,
};
pub use self::budget::{budget_take, is_test_path, BYTES_PER_TOKEN, SEARCH_PAGE_MAX};
pub(crate) use self::budget::{
    byte_span_to_line_range, byte_span_to_line_range_in, SEARCH_HIT_OVERHEAD_TOKENS,
};
use self::gaps::{
    analysis_coverage_gap, analysis_status_gap, attribution_coverage_gap, empty_ask_gap,
    empty_result_gap, empty_semantic_gap, file_edge_coverage_gap, qualify_response, query_file,
    rank_symbol_rows, ranking_coverage_gap, search_coverage_gap, CallBlindStarts, RadiusSide,
    MAX_CALL_BLIND_FILES_CHECKED,
};
pub(crate) use self::hits::search_hit_tokens;
use self::hits::{
    cap_source_span, hit_from_stored, literal_site_tokens, resolve_skeleton_path,
    scope_report_from_narrowing, skeleton_symbol_tokens, LITERAL_PAGE_CAP,
};
pub use self::hits::{EVIDENCE_TEST_BUDGET_SHARE, EVIDENCE_TEST_DEPTH};
use self::trace::shortest_path;
pub(crate) use self::trace::PathSearch;
use self::traversal::{
    bare_callee_name, blast_radius_from_edges, family_of_path, indexed_traversal_starts,
    indexed_traversed_edges, node_id_of, reached_by, BlastBand, BlastWalk, NamesakeRead,
    AFFECTED_SYMBOL_SAMPLE, DEAD_SYMBOL_TOKENS, EDGE_TOKENS, EXPLORE_DEFINITION_OVERHEAD_TOKENS,
    TRAVERSAL_MAX_NODES,
};
pub use self::traversal::{traversal_starts, traversed_resolution_edges};
pub use self::workspace::{link_candidates, workspace_search};
// Only the parse-gated test modules count source-span reads.
#[cfg(all(test, feature = "parse"))]
pub(crate) use self::hits::{SOURCE_SPAN_BYTES, SOURCE_SPAN_READS};

/// Most targets one composed `neighbors` request may ask about.
///
/// Sized above the five definitions the `graph_query` view measures, with room
/// for a caller that wants a few more, and far below anything that would let
/// one request monopolise the daemon: the worst case is
/// `2 * MAX_NEIGHBOR_TARGETS` sub-queries, which a client could already issue
/// as separate calls. It adds no reach the client did not have — it makes that
/// reach cost one round trip instead of thirty-two.
pub const MAX_NEIGHBOR_TARGETS: usize = 16;

/// Largest token budget any request may ask for.
///
/// A budget is how much answer the caller is willing to read, and every budget
/// buys work — rows scored, spans read, edges packed — so an unbounded one is
/// an unbounded request. 100,000 is far past what any consumer of this kernel
/// renders and small enough that one request cannot monopolise a daemon.
///
/// Owned here because the query layer is what *spends* a budget: the transport
/// validates against this and the CLI refuses past it, and separate private
/// copies of the number are separate places for one of them to drift into
/// permitting work the engine is not sized for.
pub const MAX_TOKEN_BUDGET: u32 = 100_000;

/// Deepest walk any traversal will perform, whatever `max_depth` asks for.
///
/// Depth multiplies with branching factor, so this is the difference between a
/// bounded question and one that visits the whole graph before `max_nodes`
/// stops it. Nothing in a real call graph needs 64 hops of transitive impact;
/// a walk that reaches it says so on `walk_incomplete` rather than presenting
/// a truncated radius as a complete one.
///
/// Owned here for the same reason as [`MAX_TOKEN_BUDGET`]: this is where the
/// clamp is actually applied, so this is where the number belongs.
pub const MAX_TRAVERSAL_DEPTH: usize = 64;

pub struct QueryEngine<'a> {
    extractions: &'a [Extraction],
    resolution: &'a ResolutionResult,
    /// The corpus half: files extraction could not fully read.
    coverage_gap: Option<String>,
    /// The repository-wide attribution sentence. Never attached to an answer
    /// whole — its presence is what says the per-walk check has anything to
    /// look for (see `radius_attribution_gap`).
    attribution_gap: Option<String>,
}

/// Query facade over the latest durable SQLite generation. Unlike
/// `QueryEngine`, this type never extracts or resolves source files.
pub struct StoreQueryEngine<'a> {
    store: &'a Store,
    /// Consulted inside the long loops. Default is a flag nobody sets, so a
    /// caller that has no way to give up (the CLI) behaves exactly as before.
    cancel: Cancel,
}

impl<'a> StoreQueryEngine<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self {
            store,
            cancel: Cancel::new(),
        }
    }

    /// Answer under a cancellation flag the caller can trip.
    ///
    /// The IPC layer bounds a query with a timeout that frees the connection
    /// but cannot abort the blocking task behind it, so without this the
    /// abandoned traversal or corpus scan runs to completion on a pool thread
    /// with nobody left to read it. See [`crate::cancel`].
    pub fn with_cancel(mut self, cancel: Cancel) -> Self {
        self.cancel = cancel;
        self
    }

    fn finish<T>(&self, response: Response<T>) -> Response<T> {
        attach_source_freshness(self.store, response)
    }

    fn unavailable<T>(&self, resolution: ResolutionAvailability) -> Response<T> {
        self.finish(unavailable_response(resolution))
    }

    pub fn search(&self, req: Request<String>) -> anyhow::Result<Response<SymbolHit>> {
        self.search_filtered(req, None)
    }

    /// [`Self::search`] over the files and kinds `filter` admits.
    ///
    /// `None` is the whole repository, and that path still reads the unfiltered
    /// full-text page. A set filter is applied in SQL before the page is cut,
    /// so `total` counts only matches that pass it and `scope` echoes the
    /// filter. A prefix, language or kind that names nothing indexed is refused.
    pub fn search_filtered(
        &self,
        req: Request<String>,
        filter: Option<&crate::scope::NameQueryFilter>,
    ) -> anyhow::Result<Response<SymbolHit>> {
        if req.query.trim().is_empty() && filter.is_none() {
            return Ok(self.finish(budget_take(Vec::new(), req.token_budget, |_| 0)));
        }
        let page = search_page_size(req.token_budget);
        let pool = search_rank_pool_size(req.token_budget);
        // One snapshot. The count, the rows and the root used to be three
        // independent reads, each resolving "the latest generation" for itself,
        // so a daemon commit landing between them produced an answer stitched
        // from two generations — `shown=40 hidden=0 total=1 truncated=false`
        // was measured. `shown + hidden == total` is the contract clients
        // enforce, and it cannot be honoured by numbers describing different
        // corpora.
        //
        // `neighbors` answers the same race by *detecting* a straddle and
        // disclosing it rather than locking, on the grounds that holding the
        // store lock across a whole fan-out blocks the writer for too long.
        // That trade is about fan-outs. This is a count, one limited select and
        // one row — so the exact answer is affordable here, and an exact answer
        // beats a disclosed approximation whenever it can be had.
        let Some(snapshot) = self.keyword_page(&req.query, pool, filter)? else {
            return Ok(self.unavailable(ResolutionAvailability::Unavailable {
                reason: "no persisted generation is available".to_string(),
            }));
        };
        let total = snapshot.total;
        let rows = snapshot.rows;
        let repo_root = snapshot.repo_root;
        // Taken off the same snapshot as the count and the rows, before either
        // is consumed, for the reason the comment above gives: a caveat
        // resolved separately could describe a different corpus from the one
        // that was searched.
        let coverage_gap = search_coverage_gap(analysis_status_gap(snapshot.analysis.as_ref()));
        let scope = snapshot
            .narrowing
            .as_ref()
            .map(|narrowing| scope_report_from_narrowing(narrowing, snapshot.analysis.as_ref()));
        let query = req.query.to_lowercase();
        // Rank, then truncate — R7, and the reason the pool above is wider than
        // the page below. The store cuts its page with `ORDER BY bm25(...)` and
        // this function then re-scores what survived with a different ordering
        // function, so the key that decided which rows *exist* was not the key
        // that decides which rows *rank*. A symbol named exactly `alpha` that
        // bm25 puts 150th among 200 prefix matches never entered the page, and
        // the answer led with 100 worse matches under honest counts.
        //
        // Scoring happens on the stored row, before any hit is materialised:
        // the score reads `name`/`qualified_name` and nothing else, while
        // building a hit reads the file off disk. So a ten-times wider pool
        // costs ten times the string comparisons and not one extra file read —
        // `page`, not `pool`, bounds what is materialised.
        let ranked = rank_symbol_rows(rows, &query, &self.cancel)?;
        let mut hits = Vec::with_capacity(ranked.len().min(page));
        for (score, row) in ranked.into_iter().take(page) {
            // Every iteration here opens a file. Checked per row rather than
            // per `CHECK_INTERVAL` because the unit of work is an I/O, not a
            // string comparison: a page of 200 abandoned reads is 200 reads
            // nobody is waiting for.
            self.cancel.check()?;
            hits.push(hit_from_stored(
                row,
                repo_root.as_deref(),
                req.token_budget,
                score,
            ));
        }
        let mut response = budget_take(hits, req.token_budget, search_hit_tokens);
        response.total = total;
        response.hidden = total.saturating_sub(response.shown);
        response.truncated = response.hidden > 0;
        // The pool is bounded, so on a query that matches more than it holds
        // the ranking really is over a bm25-ordered prefix. `truncated` says
        // the *list* was cut, which a caller expects; this says the *ordering*
        // was computed over a sample, which it cannot otherwise know. It is
        // `None` whenever every match was ranked, which on any ordinary query
        // is every time.
        let ranked_over_a_sample = ranking_coverage_gap(total, pool);
        // Three independent qualifications, composed rather than ranked — the
        // same shape `dependencies` and the traversals use. One is about the
        // *ordering* of what was found; one is about whether the corpus
        // searched was the whole repository; and the third is about what an
        // empty answer is entitled to mean. The second was documented here as
        // "the one that decides whether `total: 0` may be read as 'no such
        // symbol'" — but it is `None` on every healthy index, which left the
        // most confident-looking answer this API produces as the one carrying
        // the least justification. See [`empty_result_gap`].
        response.walk_incomplete = devmap_analyze::combine_reasons(
            ranked_over_a_sample,
            devmap_analyze::combine_reasons(coverage_gap, empty_result_gap(total, &req.query)),
        );
        response.scope = scope;
        Ok(self.finish(response))
    }

    /// One keyword page, pinned to the generation the filter was checked against.
    ///
    /// No filter calls [`Store::search_page`], whose SQL stays the unfiltered
    /// plan. A filter resolves path and language against that generation's
    /// files, then asks the store for a page whose count and rows both already
    /// satisfy it.
    fn keyword_page(
        &self,
        query: &str,
        pool: usize,
        filter: Option<&crate::scope::NameQueryFilter>,
    ) -> anyhow::Result<Option<devmap_store::SearchPage>> {
        let Some(filter) = filter else {
            return Ok(self.store.search_page(query, pool)?);
        };
        let Some((generation, root, files)) = self.store.latest_scope_inputs()? else {
            return Ok(None);
        };
        let (paths, languages) = match filter.symbol_scope()? {
            Some(scope) => {
                let resolved = scope.resolve(&files, root.as_deref())?;
                (resolved.report.paths, resolved.report.languages)
            }
            None => (Vec::new(), Vec::new()),
        };
        let narrowing = devmap_store::KeywordNarrowing {
            paths,
            languages,
            kinds: filter.kinds().to_vec(),
        };
        Ok(self
            .store
            .search_page_in(generation, query, pool, Some(&narrowing))?)
    }

    pub fn dependencies(&self, req: Request<String>) -> anyhow::Result<Response<ResolvedEdge>> {
        self.dependencies_at_rung(req, None)
    }

    /// [`Self::dependencies`], narrowed to a named rung on the resolution
    /// ladder.
    ///
    /// The floor is applied here rather than folded into `min_confidence` at
    /// the transport, because the store drops rows below its threshold before
    /// this function ever sees them — a histogram computed on what survived
    /// that could only ever report `filtered_out: 0`, which is the precise
    /// shape of "a structural absence read as an observed negative" this whole
    /// pass exists to remove. Filtering after the load and before the budget
    /// means the count is measured, and means the budget packs edges the caller
    /// actually asked for rather than spending itself on rows about to be cut.
    pub fn dependencies_at_rung(
        &self,
        req: Request<String>,
        min_rung: Option<crate::rung::Rung>,
    ) -> anyhow::Result<Response<ResolvedEdge>> {
        self.cancel.check()?;
        let Some(snapshot) = self.store.file_edges(&req.query, req.min_confidence)? else {
            return Ok(self.unavailable(ResolutionAvailability::Unavailable {
                reason: format!("{} is not indexed", req.query),
            }));
        };
        if matches!(snapshot.file.parse_outcome, ParseOutcome::Failed { .. }) {
            return Ok(self.unavailable(ResolutionAvailability::Unavailable {
                reason: format!("{} could not be parsed", req.query),
            }));
        }
        // The corpus half, then the sites written in this file that the
        // resolver could not bind — what this file's own list cannot show.
        let file_sites = self.radius_attribution_gap(
            Some(snapshot.generation),
            snapshot.analysis.as_ref(),
            &BTreeSet::from([(snapshot.file.path.clone(), snapshot.file.path.clone())]),
            RadiusSide::FileCallees,
        )?;
        let coverage_gap = devmap_analyze::combine_reasons(
            file_edge_coverage_gap(&snapshot.file.parse_outcome),
            devmap_analyze::combine_reasons(
                analysis_status_gap(snapshot.analysis.as_ref()),
                file_sites,
            ),
        );
        self.cancel.check()?;
        let edges = snapshot
            .edges
            .into_iter()
            .map(stored_edge_to_resolved)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let (edges, rungs) = crate::rung::narrow(edges, min_rung);
        let mut response = budget_take(edges, req.token_budget, |_| 25);
        response.rungs = Some(rungs);
        // Composed, not assigned: `budget_take` may already have set a reason
        // of its own, and a reader deciding whether to act on this list needs
        // every qualification the answer holds, not the last one written.
        response.walk_incomplete =
            devmap_analyze::combine_reasons(response.walk_incomplete.take(), coverage_gap);
        Ok(self.finish(response))
    }

    pub fn impact(&self, req: Request<String>) -> anyhow::Result<Response<ResolvedEdge>> {
        self.traverse(req, true, None)
    }

    /// [`Self::impact`], narrowed to a named rung. See
    /// [`Self::dependencies_at_rung`] for why the floor is not a confidence.
    pub fn impact_at_rung(
        &self,
        req: Request<String>,
        min_rung: Option<crate::rung::Rung>,
    ) -> anyhow::Result<Response<ResolvedEdge>> {
        self.traverse(req, true, min_rung)
    }

    /// [`Self::impact`], with the reached symbols banded by distance.
    ///
    /// The flat edge list answers *what*; the bands answer *how far*, and until
    /// this existed every consumer that needed the second derived it from the
    /// first. Deriving it is not possible — an edge list does not carry the hop
    /// at which the walk reached each endpoint — so the derivation was a guess,
    /// and the guess in this repository asserted `depth: 1` and
    /// `confidence: extracted` for every symbol a depth-3 walk returned.
    ///
    /// One generation, one index, two readings of it. Both halves come from the
    /// same [`GenerationEdges`] snapshot, so the edge list and the bands cannot
    /// describe different states of the repository the way a client issuing two
    /// calls could — the straddle [`Self::neighbors_at_rung`] has to detect and
    /// retry is not expressible here.
    ///
    /// **No rung floor.** [`Self::blast_walk`] filters on `min_confidence` and
    /// has no rung, so accepting one would narrow the edges and leave the bands
    /// wide — a composed answer whose two halves disagree about what the caller
    /// asked for, which is the defect `neighbors_at_rung` documents. A caller
    /// that wants a floor asks [`Self::impact_at_rung`] and gets an answer whose
    /// filter is uniform.
    ///
    /// The budget is split, not doubled: `token_budget / 2` to the bands and the
    /// remainder to the edges, as [`Self::affected_tests`] splits its own. A
    /// caller asking for 2,000 tokens is answered in 2,000.
    pub fn impact_layered(&self, req: Request<String>) -> anyhow::Result<LayeredImpact> {
        devmap_store::checked_min_confidence(req.min_confidence)?;
        let layer_budget = req.token_budget / 2;
        let edge_budget = req.token_budget.saturating_sub(layer_budget);
        let Some((generation, index)) = self.store.generation_edges_with_id()? else {
            let reason = "no persisted generation is available".to_string();
            return Ok(LayeredImpact {
                edges: unavailable_response(ResolutionAvailability::Unavailable {
                    reason: reason.clone(),
                }),
                blast_radius: BlastRadius {
                    seeds: Vec::new(),
                    unmatched_targets: vec![req.query],
                    layers: unavailable_response(ResolutionAvailability::Unavailable { reason }),
                    total_impacted: 0,
                },
            });
        };
        let direction = index.directed(true, req.min_confidence);
        let target = req.query.clone();
        let min_confidence = req.min_confidence;
        let (mut edges, bands) = self.traverse_walked(
            &index,
            &direction,
            Request {
                token_budget: edge_budget,
                ..req
            },
            None,
            Some(layer_budget),
        )?;
        let added = self.attach_unresolved_namesakes(
            generation,
            &index,
            &target,
            min_confidence,
            &mut edges,
        )?;
        // `Some` by construction: `band_budget` was `Some` on the call above,
        // and every return path of `traverse_walked` maps it.
        let mut blast_radius = bands.ok_or_else(|| {
            anyhow::anyhow!("a banded traversal returned no bands; this is a bug in the kernel")
        })?;
        // One answer, two halves: the bands are as incomplete as the edges.
        blast_radius.layers.walk_incomplete =
            devmap_analyze::combine_reasons(blast_radius.layers.walk_incomplete.take(), added);
        Ok(LayeredImpact {
            edges,
            blast_radius,
        })
    }

    /// Answer both call-graph directions for several targets in one pass.
    ///
    /// This is a composition, not new analysis: each target still gets exactly
    /// the [`Self::impact`] and [`Self::trace`] it would have got on its own,
    /// under the same budget and the same `min_confidence` — both directions,
    /// so the two halves of one answer cannot disagree about what the filter
    /// meant, nor about which file the target names. What it removes is the
    /// per-direction round trip — the caller pays one, not `2 * targets.len()`.
    ///
    /// The fan-out is bounded and the bound is *refused*, never silently
    /// applied: more than [`MAX_NEIGHBOR_TARGETS`] targets is an error, so
    /// nobody can read a truncated answer as a complete one. A caller that
    /// needs more must ask again, and know that it did.
    ///
    /// Cancellation is checked between targets, so a composed request cannot
    /// outlive its client by the whole fan-out.
    pub fn neighbors(
        &self,
        targets: &[String],
        token_budget: u32,
        min_confidence: f32,
        max_depth: usize,
    ) -> anyhow::Result<Vec<Neighbors>> {
        self.neighbors_at_rung(targets, token_budget, min_confidence, max_depth, None)
    }

    /// [`Self::neighbors`], narrowed to a named rung.
    ///
    /// A composition must accept every filter its parts accept. `min_rung` was
    /// pinned to `None` inside this fan-out while `impact` and `deps` — the two
    /// queries it *is* — each took a floor, so a caller could ask either half
    /// for deterministic-only edges and could not ask for both at once.
    ///
    /// That is the third parameter to be pinned here and the third to be a
    /// defect: `min_confidence` was hardcoded 0.0 on the inbound side, so one
    /// answer's two halves disagreed about the caller's filter; `max_depth` was
    /// pinned to 1, invisible to a composition test that compared at depth 1.
    /// Both notes are still in the body below, a few lines from where this
    /// argument now travels, because the pattern is the point: a fan-out that
    /// drops a filter answers the *broader* question, plausibly, and nothing in
    /// the response says so — the caller reads a wide list as the narrow one
    /// they asked for, which is the direction no reader guards against.
    pub fn neighbors_at_rung(
        &self,
        targets: &[String],
        token_budget: u32,
        min_confidence: f32,
        max_depth: usize,
        min_rung: Option<crate::rung::Rung>,
    ) -> anyhow::Result<Vec<Neighbors>> {
        if targets.len() > MAX_NEIGHBOR_TARGETS {
            anyhow::bail!(
                "neighbors accepts at most {} targets, got {}",
                MAX_NEIGHBOR_TARGETS,
                targets.len()
            );
        }
        // A composed answer must come from one generation.
        //
        // The fan-out used to be `2 * targets.len()` sub-queries, each taking
        // and releasing the store lock on its own. A build committing
        // mid-fan-out left one answer describing two different snapshots of the
        // repository — measured at 25 of 62 composed answers under contention —
        // and nothing in the response disclosed it. That is not a regression
        // against the separate `impact`/`deps` calls this replaced, which
        // straddled the same way; but those were visibly separate exchanges and
        // this is sold as one.
        //
        // `neighbors_once` now reads the edge table once for the whole
        // fan-out, so the parts of one answer can no longer disagree with each
        // other about the graph. The check below stays because that read is
        // still not atomic with the two generation probes around it: a commit
        // landing between the first probe and the read produces an answer from
        // a generation the caller was not told about. One read narrows the
        // window; it does not close it, and an undisclosed straddle is the
        // defect either way.
        //
        // Detected rather than locked out: holding the store lock across the
        // whole fan-out would block the writer for the duration of a composed
        // query, which is a worse trade. Commits are rare, so one retry
        // resolves nearly all of them; a second straddle is reported on every
        // direction instead of being smoothed over, because a caller that
        // cannot tell is the actual defect.
        self.read_composed(
            || self.neighbors_once(targets, token_budget, min_confidence, max_depth, min_rung),
            |answers, note| {
                for entry in answers {
                    qualify_response(&mut entry.callers, note);
                    qualify_response(&mut entry.callees, note);
                }
            },
        )
    }

    /// Bound retry work without holding the writer's lock across a fan-out.
    /// A second straddle qualifies every independently consumable response.
    fn read_composed<T>(
        &self,
        mut read: impl FnMut() -> anyhow::Result<T>,
        qualify: impl Fn(&mut T, &str),
    ) -> anyhow::Result<T> {
        for attempt in 0..2 {
            self.cancel.check()?;
            let before = self.store.latest_generation_id()?;
            let mut answer = read()?;
            let after = self.store.latest_generation_id()?;
            if before == after {
                return Ok(answer);
            }
            if attempt == 1 {
                qualify(
                    &mut answer,
                    &format!(
                        "the index moved from generation {before:?} to {after:?} while this \
                     composed answer was being assembled, twice in a row; its parts may \
                     describe different snapshots"
                    ),
                );
                return Ok(answer);
            }
        }
        unreachable!("the loop returns on both attempts")
    }

    /// One pass of the composition. See [`Self::neighbors`] for the retry that
    /// keeps a composed answer inside a single generation.
    fn neighbors_once(
        &self,
        targets: &[String],
        token_budget: u32,
        min_confidence: f32,
        max_depth: usize,
        min_rung: Option<crate::rung::Rung>,
    ) -> anyhow::Result<Vec<Neighbors>> {
        // Nothing asked, nothing read. Without this the hoisted load below
        // would pull the whole edge table to answer a request with no targets.
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        // One edge load for the whole fan-out.
        //
        // `impact` and `trace` each begin by reading and converting every edge
        // in the generation, so a composed answer over N targets paid for that
        // 2N times — 16 full reads for the eight-target fan-out the daemon
        // sends, of a table that does not change between them. On a
        // 660,000-edge store the read is 38 ms and the conversion 33 ms, so
        // fifteen of those sixteen reads were 1.07 s of a 2.56 s answer.
        //
        // This is the same hoist `explore` already performs, for the same
        // reason and through the same seam: `traverse_over` *is* the body of
        // `impact`/`trace` once the edges are in hand, so every direction below
        // gets exactly the traversal, budget and `walk_incomplete` reason it
        // got before — the ownership of the load moved, nothing else. The
        // filter is `min_confidence`, which is one value for the whole request,
        // so a single load can serve every target and both directions.
        //
        // It also makes the composition *more* coherent than it was: the parts
        // of one answer now share one edge snapshot instead of racing each
        // other. The generation straddle check in [`Self::neighbors`] still
        // wraps this, because the load is not the only store read here.
        //
        // What is hoisted is the *index*, and it is not built here at all: the
        // store keeps one per generation, so the fan-out pays a hash lookup per
        // target rather than a whole-generation scan — and so does a single
        // `impact`, which is the half a per-request index cannot reach. One
        // directed view per direction, three words each over the shared index,
        // because a walk must not be given a direction that disagrees with the
        // adjacency it reads.
        devmap_store::checked_min_confidence(min_confidence)?;
        let Some(index) = self.generation_edges()? else {
            return Ok(targets
                .iter()
                .map(|target| {
                    let unavailable = || {
                        unavailable_response(ResolutionAvailability::Unavailable {
                            reason: "no persisted generation is available".to_string(),
                        })
                    };
                    Neighbors {
                        target: target.clone(),
                        callers: unavailable(),
                        callees: unavailable(),
                    }
                })
                .collect());
        };
        let inbound = index.directed(true, min_confidence);
        let outbound = index.directed(false, min_confidence);
        let mut answers = Vec::with_capacity(targets.len());
        for target in targets {
            // Plain `check`, not `check_every`: the latter consults the flag
            // once per CHECK_INTERVAL (512) iterations, and this loop runs at
            // most MAX_NEIGHBOR_TARGETS (16) times, so it would fire on target
            // 0 and never again.
            //
            // In practice cancellation already lands inside `traverse_over`,
            // which checks the flag on either side of the walk, so this is not
            // what rescues a cancelled request — measurement confirms a
            // composition stops partway through either way. It closes the gap
            // *between* sub-queries, and costs one relaxed load per target.
            self.cancel.check()?;
            // `min_confidence` applies to *both* directions. It was hardcoded to
            // 0.0 here, which silently discarded the caller's filter on the
            // inbound side: one composed answer would report every caller while
            // reporting only the callees that cleared the threshold, so its two
            // halves disagreed about what the filter meant. It is now applied
            // once, in the shared `resolved_edges(min_confidence)` above, and
            // again inside `traverse_over` — so the two halves cannot diverge
            // by construction rather than by both remembering to pass it.
            let callers = self.traverse_over(
                &index,
                &inbound,
                Request {
                    query: target.clone(),
                    token_budget,
                    min_confidence,
                    max_depth,
                },
                min_rung,
            )?;
            // Outbound edges come from whichever query can actually answer
            // for this target's shape.
            //
            // `dependencies` resolves a *file* path, so for a symbol id it
            // returned `Unavailable: … is not indexed` — every time. The
            // composed `query` view therefore reported its callees as unknown
            // for every symbol-shaped query, which is honest but useless, and
            // the earlier attempt to fix it by asking about the containing file
            // was worse: a function's "callees" became the whole file's
            // outbound edges, so symbols appeared to call themselves.
            //
            // The forward traversal is symbol-scoped and answers exactly the
            // question. `latest_file` — the store's own notion of what is a
            // file — picks between them, rather than sniffing for `::`.
            // Outbound edges come from the forward traversal, for every target
            // shape — not from `dependencies`.
            //
            // `dependencies` resolves a *file* path, so for a symbol id it
            // always answered `Unavailable: … is not indexed` and `callees`
            // never carried anything for a symbol query. For a file it answered,
            // but wrongly for this field: its SQL matches
            // `sp.path = ?2 OR tp.path = ?2`, so "outbound edges" included
            // edges pointing *into* the file, and a file appeared in its own
            // callee list — the same "symbols appeared to call themselves"
            // shape, surviving on the file path.
            //
            // Worse, mixing the two resolvers made one answer incoherent:
            // `impact` matches by suffix (`path_matches`: `ends_with("/{query}")`)
            // while `dependencies` matches exactly, so with both `core.py` and
            // `pkg/core.py` indexed, the callers described one file and the
            // callees another. One resolver for both directions removes that by
            // construction.
            //
            // `deps` the command is unchanged; only this composition's `callees`
            // tightened to what the field has always claimed to be.
            //
            // `max_depth` is the caller's, on this side too. It was pinned at
            // 1 here — a leftover from when this field was answered by
            // `dependencies`, which is a one-hop query by construction — while
            // the inbound half above walked the depth the caller asked for. A
            // composed answer at depth 3 therefore reported a three-level
            // caller tree beside a single hop of callees, with nothing in it
            // saying which half had been cut short: exactly the defect the
            // `min_confidence` note above describes and closes, surviving in
            // the parameter beside it. `neighbors_composition.rs` compares
            // every field of both halves against the calls they replace and
            // could not see it, because it makes every comparison at depth 1 —
            // the one depth where the pinned value and the requested one agree.
            let callees = self.traverse_over(
                &index,
                &outbound,
                Request {
                    query: target.clone(),
                    token_budget,
                    min_confidence,
                    max_depth,
                },
                min_rung,
            )?;
            answers.push(Neighbors {
                target: target.clone(),
                callers,
                callees,
            });
        }
        Ok(answers)
    }

    pub fn trace(&self, req: Request<String>) -> anyhow::Result<Response<ResolvedEdge>> {
        self.traverse(req, false, None)
    }

    /// [`Self::trace`], narrowed to a named rung. See
    /// [`Self::dependencies_at_rung`] for why the floor is not a confidence.
    pub fn trace_at_rung(
        &self,
        req: Request<String>,
        min_rung: Option<crate::rung::Rung>,
    ) -> anyhow::Result<Response<ResolvedEdge>> {
        self.traverse(req, false, min_rung)
    }

    /// Return one deterministic shortest path from `from` to `to`.
    ///
    /// A scoped trace is atomic under token budgeting: a prefix that does not
    /// reach the requested destination is not a valid answer, so an
    /// insufficient budget returns zero items with `truncated = true`.
    pub fn trace_between(
        &self,
        req: Request<(String, String)>,
    ) -> anyhow::Result<Response<ResolvedEdge>> {
        self.cancel.check()?;
        devmap_store::checked_min_confidence(req.min_confidence)?;
        let Some(index) = self.generation_edges()? else {
            return Ok(self.unavailable(ResolutionAvailability::Unavailable {
                reason: "no persisted generation is available".to_string(),
            }));
        };
        let (from, to) = req.query;
        let from = from.trim();
        let to = to.trim();
        // "No indexed path" is a claim about the graph only where both
        // endpoints' calls were looked for (P2.7a). Bare-name endpoints name no
        // file and are not checked here; a qualified or path endpoint is.
        let coverage_gap = devmap_analyze::combine_reasons(
            analysis_coverage_gap(index.analysis()),
            self.call_blind_starts(query_file(from).into_iter().chain(query_file(to)))?
                .reason(),
        );
        let unavailable = |reason: String| {
            let mut response = unavailable_response(ResolutionAvailability::Unavailable { reason });
            response.walk_incomplete = coverage_gap.clone();
            response
        };
        if from.is_empty() || to.is_empty() {
            return Ok(self.unavailable(ResolutionAvailability::Unavailable {
                reason: "scoped trace endpoints must not be empty".to_string(),
            }));
        }
        // `trace X X` walked the graph from `X` looking for `X`, never counted
        // the start as reached, and reported the walk's budget — "stopped at
        // depth 3 after visiting 44 nodes; whether a path exists is unknown" —
        // for a question the walk cannot answer. Whether a cycle passes through
        // a symbol is `impact`'s question; a scoped trace needs two endpoints.
        if from == to {
            return Ok(self.unavailable(ResolutionAvailability::Unavailable {
                reason: format!(
                    "{from:?} and {to:?} are the same symbol; a scoped trace needs two \
                     different endpoints (whether a cycle passes through it is an \
                     `impact` question)"
                ),
            }));
        }
        let edges = self.resolved_edges(&index, req.min_confidence)?;
        let path = match shortest_path(
            &edges,
            from,
            to,
            req.max_depth.min(MAX_TRAVERSAL_DEPTH),
            5_000,
            &self.cancel,
        )? {
            PathSearch::Found(path) => path,
            // The only outcome that is a claim about the graph.
            PathSearch::NoPath => {
                return Ok(unavailable(format!(
                    "no indexed path from {from:?} to {to:?}"
                )))
            }
            // A limit stopped the walk, so the graph was never asked. Saying
            // "no path" here is how an agent concludes two symbols are
            // unrelated when the path is merely longer than `--depth`.
            PathSearch::Exhausted {
                depth_capped,
                node_capped,
                visited,
                max_depth,
                max_nodes,
            } => {
                let mut limits = Vec::new();
                if depth_capped {
                    limits.push(format!("depth {max_depth}"));
                }
                if node_capped {
                    limits.push(format!("{max_nodes} nodes"));
                }
                let limits = if limits.is_empty() {
                    "its budget".to_string()
                } else {
                    limits.join(" and ")
                };
                return Ok(unavailable(format!(
                    "search from {from:?} to {to:?} stopped at {limits} after visiting \
                         {visited} nodes without reaching the target; whether a path exists \
                         is unknown — retry with a larger --depth"
                )));
            }
        };
        let mut response = atomic_budget_take(path, req.token_budget, |_| 25);
        response.walk_incomplete = coverage_gap;
        Ok(self.finish(response))
    }

    /// Every edge in the latest generation at or above `min_confidence`, in
    /// engine form.
    ///
    /// One owner for the conversion, and the one place the whole edge set is
    /// walked before any traversal starts — so it is where an abandoned
    /// request stops earliest. It reads the store's shared per-generation rows
    /// rather than a private copy of them, so the whole-set materialisation is
    /// one allocation of `ResolvedEdge`s and not a `StoredEdge` clone before
    /// it. Only `trace_between` still needs the whole set; everything else
    /// asks [`Self::generation_edges`] for the adjacency and pays for the
    /// edges it reaches.
    fn resolved_edges(
        &self,
        index: &GenerationEdges,
        min_confidence: f32,
    ) -> anyhow::Result<Vec<ResolvedEdge>> {
        let min_confidence = devmap_store::checked_min_confidence(min_confidence)?;
        let mut edges = Vec::new();
        for id in 0..index.len() as u32 {
            self.cancel.check_every(id as usize)?;
            if !index.admits(id, min_confidence) {
                continue;
            }
            // The one whole-generation materialisation left, and the row is
            // built here rather than held for the life of the index: only
            // `trace_between` needs every edge as a `ResolvedEdge`, and it is
            // about to own all of them anyway.
            edges.push(stored_edge_to_resolved(index.stored_edge(id))?);
        }
        Ok(edges)
    }

    /// The latest generation's adjacency, or `None` when nothing is persisted.
    ///
    /// Built once per generation and memoised in the store, so a long-lived
    /// `devmap mcp` pays for it on the first graph question after a build and
    /// not on every question. The index is a snapshot of one generation: a
    /// build that lands mid-walk cannot mix two generations into one answer,
    /// and the next call after it gets the new one.
    fn generation_edges(&self) -> anyhow::Result<Option<std::sync::Arc<GenerationEdges>>> {
        Ok(self.store.generation_edges()?)
    }

    fn traverse(
        &self,
        req: Request<String>,
        reverse: bool,
        min_rung: Option<crate::rung::Rung>,
    ) -> anyhow::Result<Response<ResolvedEdge>> {
        // Before the generation lookup, not after: an unevaluable threshold is
        // a refusal whatever the store holds, and answering "no persisted
        // generation is available" to a caller who asked an unanswerable
        // question tells them about the store when the fault is in the request.
        devmap_store::checked_min_confidence(req.min_confidence)?;
        // The store lock is released before the index is returned — the store
        // locks, reads and drops — so nothing below this line holds it. An
        // abandoned traversal therefore cannot block the drain loop's writes
        // while it unwinds.
        let Some((generation, index)) = self.store.generation_edges_with_id()? else {
            return Ok(self.unavailable(ResolutionAvailability::Unavailable {
                reason: "no persisted generation is available".to_string(),
            }));
        };
        let direction = index.directed(reverse, req.min_confidence);
        if !reverse {
            return self.traverse_over(&index, &direction, req, min_rung);
        }
        let target = req.query.clone();
        let min_confidence = req.min_confidence;
        let mut response = self.traverse_over(&index, &direction, req, min_rung)?;
        self.attach_unresolved_namesakes(
            generation,
            &index,
            &target,
            min_confidence,
            &mut response,
        )?;
        Ok(response)
    }

    /// Attach the ledger's unresolved sites that name `target` to an `impact`
    /// answer, and say so in `walk_incomplete` when there are any.
    ///
    /// The names come from the walk's own starts when it has them, and from the
    /// query text when it has none — the second is not a corner case. A method
    /// whose every caller went unresolved has no inbound edge, so it is no
    /// traversal start, and that is precisely the method whose callers are all
    /// in the ledger.
    ///
    /// `generation` is the one `index` was built from. The ledger is read at
    /// that generation, so the edges and the candidates describe one state of
    /// the repository even when a daemon commits between the two reads.
    ///
    /// Returns the qualification it added, so a caller holding a second half
    /// of the same answer (`impact_layered`'s bands) can carry it too.
    fn attach_unresolved_namesakes<T>(
        &self,
        generation: u32,
        index: &GenerationEdges,
        target: &str,
        min_confidence: f32,
        response: &mut Response<T>,
    ) -> anyhow::Result<Option<String>> {
        // A refused start query (an ambiguous bare name) has already been
        // reported by the walk; the names below then come from the query text.
        let starts = indexed_traversal_starts(index, target, true, min_confidence, &self.cancel)
            .unwrap_or_default();
        let namesakes = match self.unresolved_namesakes(generation, target, &starts)? {
            NamesakeRead::NotApplicable => return Ok(None),
            NamesakeRead::GenerationGone => {
                // Checked, and could not be answered consistently: saying
                // nothing here would read as "the ledger holds no candidates".
                let note = format!(
                    "the unresolved ledger could not be read at generation {generation}, the one \
                     these edges came from (it was pruned mid-query); callers the resolver could \
                     not bind are not listed — ask again"
                );
                response.walk_incomplete = devmap_analyze::combine_reasons(
                    response.walk_incomplete.take(),
                    Some(note.clone()),
                );
                return Ok(Some(note));
            }
            NamesakeRead::Read(namesakes) => namesakes,
        };
        let mut added = None;
        if !namesakes.sites.is_empty() || namesakes.truncated {
            added = devmap_analyze::combine_reasons(
                added,
                Some(format!(
                    "{}{} unresolved call site(s) name {} and are not edges — an untyped \
                     receiver, a module loaded by path; they are listed in \
                     `unresolved_namesakes` as candidates to verify, not as callers",
                    if namesakes.truncated { "at least " } else { "" },
                    namesakes.sites.len(),
                    namesakes.names.join(", "),
                )),
            );
        }
        // A capped check must not read as a clean one: an empty `sites` with
        // names left unchecked is "not looked", not "none there".
        if namesakes.names_not_checked > 0 {
            added = devmap_analyze::combine_reasons(
                added,
                Some(format!(
                    "the unresolved ledger was checked for {} name(s) only; {} more were not \
                     looked up (`unresolved_namesakes.names_not_checked`)",
                    namesakes.names.len(),
                    namesakes.names_not_checked,
                )),
            );
        }
        response.walk_incomplete =
            devmap_analyze::combine_reasons(response.walk_incomplete.take(), added.clone());
        response.unresolved_namesakes = Some(namesakes);
        Ok(added)
    }

    /// The unresolved call sites at `generation` whose callee is the bare name
    /// of a target — one ledger read for every name.
    ///
    /// `starts` are `(qualified symbol, file)`. With none, the name is read off
    /// the query itself; a path query names no callee. Sites in another
    /// language family than the start that named them are counted and dropped.
    /// With no start there is no family to compare, and every site is kept.
    fn unresolved_namesakes(
        &self,
        generation: u32,
        target: &str,
        starts: &[(String, String)],
    ) -> anyhow::Result<NamesakeRead> {
        // name → the families of the starts that carry it; empty = unknown.
        let mut wanted: BTreeMap<String, BTreeSet<LangFamily>> = BTreeMap::new();
        for (symbol, file) in starts {
            if !symbol.contains("::") {
                continue; // a file node: it is not called by name
            }
            if let Some(name) = bare_callee_name(symbol) {
                wanted
                    .entry(name.to_string())
                    .or_default()
                    .insert(family_of_path(file));
            }
        }
        if wanted.is_empty() {
            let (name, families) = match crate::query_match::classify(target.trim()) {
                crate::query_match::StartQuery::Qualified { file, symbol } => (
                    bare_callee_name(symbol),
                    BTreeSet::from([family_of_path(file)]),
                ),
                crate::query_match::StartQuery::Symbol(name) => {
                    (bare_callee_name(name), BTreeSet::new())
                }
                crate::query_match::StartQuery::Path(_)
                | crate::query_match::StartQuery::Nothing => (None, BTreeSet::new()),
            };
            let Some(name) = name else {
                return Ok(NamesakeRead::NotApplicable);
            };
            wanted.insert(name.to_string(), families);
        }
        let names_not_checked = wanted.len().saturating_sub(MAX_NAMESAKE_NAMES);
        let wanted: Vec<(String, BTreeSet<LangFamily>)> =
            wanted.into_iter().take(MAX_NAMESAKE_NAMES).collect();
        let names: Vec<String> = wanted.iter().map(|(name, _)| name.clone()).collect();
        self.cancel.check()?;
        let Some(mut found) =
            self.store
                .unresolved_sites_naming(generation, &names, MAX_NAMESAKE_SITES_PER_NAME)?
        else {
            return Ok(NamesakeRead::GenerationGone);
        };
        let mut namesakes = UnresolvedNamesakes {
            generation_id: generation,
            names_not_checked,
            ..UnresolvedNamesakes::default()
        };
        for (name, families) in wanted {
            let (rows, truncated) = found.remove(&name).unwrap_or_default();
            namesakes.truncated |= truncated;
            for row in rows {
                if !families.is_empty() && !families.contains(&family_of_path(&row.source_file)) {
                    namesakes.other_language_sites += 1;
                    continue;
                }
                namesakes.sites.push(UnresolvedSite {
                    source_file: row.source_file,
                    source_symbol: row.source_symbol,
                    callee_name: name.clone(),
                    receiver: row.receiver,
                    classification: row.classification,
                });
            }
            namesakes.names.push(name);
        }
        Ok(NamesakeRead::Read(namesakes))
    }

    /// The traversal itself, over an index the caller already holds.
    ///
    /// Split out of [`Self::traverse`] because `explore` needs `2 * n + 1`
    /// walks for one answer and every one of them used to re-read and
    /// re-convert the whole generation's edge table — 271,543 rows on the
    /// ScholarLM corpus. The walk is unchanged; what moved is where the
    /// adjacency comes from. Every caller still gets exactly the traversal
    /// `impact`/`trace` performs, including the `walk_incomplete` reason,
    /// because there is only one implementation of it.
    fn traverse_over(
        &self,
        index: &GenerationEdges,
        direction: &devmap_store::DirectedEdges<'_>,
        req: Request<String>,
        min_rung: Option<crate::rung::Rung>,
    ) -> anyhow::Result<Response<ResolvedEdge>> {
        Ok(self
            .traverse_walked(index, direction, req, min_rung, None)?
            .0)
    }

    /// [`Self::traverse_over`], optionally banding the same walk by distance.
    ///
    /// One body, so `impact` and `impact --layers` cannot answer from two
    /// different walks. `band_budget` is `None` for every caller that wants only
    /// the edge list — which is the walk unchanged, byte for byte — and `Some`
    /// for [`Self::impact_layered`], which needs the *same* edges partitioned by
    /// the hop the walk reached them at.
    ///
    /// The bands are computed here rather than by a second walk over the store
    /// for one reason: they must describe *this* answer. [`Self::blast_walk`]
    /// walks the generation index directly and does not apply the reverse-
    /// direction exclusions the traversal applies — no upward containment, no
    /// file-level imports out of a symbol node — so a radius from there names
    /// nodes this answer's edge list does not contain. That is the right shape
    /// for `explore` and `affected`, which ask a wider question; it is the wrong
    /// shape for an answer whose other half is the edge list itself.
    fn traverse_walked(
        &self,
        index: &GenerationEdges,
        direction: &devmap_store::DirectedEdges<'_>,
        req: Request<String>,
        min_rung: Option<crate::rung::Rung>,
        band_budget: Option<u32>,
    ) -> anyhow::Result<(Response<ResolvedEdge>, Option<BlastRadius>)> {
        let min_confidence = devmap_store::checked_min_confidence(req.min_confidence)?;
        // The direction is the view's, not a second argument that could
        // disagree with it. A reversed walk over a forward index answers
        // plausibly and wrongly rather than failing, so the two are not
        // separable here.
        let reverse = devmap_analyze::traversal::GraphIndex::reverse(direction);
        let target = req.query.trim();
        // Read once, before either exit below can return without it, and from
        // the index rather than from the store: a walk over a corpus with a
        // hole in it is a lower bound whether it found a start or not, and
        // "no indexed traversal start" is a much weaker statement when the file
        // the symbol lives in was never read.
        //
        // The corpus half only. Whether unattributed calls touch this answer
        // is asked of the nodes the walk reached, below — see
        // `radius_attribution_gap`.
        let coverage_gap = analysis_status_gap(index.analysis());
        let starts =
            indexed_traversal_starts(index, target, reverse, min_confidence, &self.cancel)?;
        // P2.7a. The corpus-level marker above cannot see this: a `.tf` beside
        // Python hides no Python caller, so the analysis stays `Ok` — and then
        // `impact` on the `.tf` symbol itself answered `total: 0, Available`
        // for a file whose calls were never looked for. With no start, the file
        // the query names is the only one there is to ask about.
        let call_blind = if starts.is_empty() {
            self.call_blind_starts(query_file(target))?
        } else {
            self.call_blind_starts(starts.iter().map(|(_, file)| file.as_str()))?
        };
        let start: Vec<String> = starts.iter().map(|(symbol, _)| symbol.clone()).collect();
        if start.is_empty() {
            // A symbol whose every call went unbound has no outbound edge, so
            // it is no start for a walk toward callees — and its unbound calls
            // are exactly what this answer cannot show. Ask about the target
            // itself. (Toward callers, `attach_unresolved_namesakes` asks.)
            let coverage_gap = if reverse {
                coverage_gap
            } else {
                let named = match crate::query_match::classify(target) {
                    crate::query_match::StartQuery::Qualified { file, .. } => {
                        Some((RadiusSide::Callees, (target.to_string(), file.to_string())))
                    }
                    crate::query_match::StartQuery::Path(path) => Some((
                        RadiusSide::FileCallees,
                        (path.to_string(), path.to_string()),
                    )),
                    crate::query_match::StartQuery::Symbol(_)
                    | crate::query_match::StartQuery::Nothing => None,
                };
                match named {
                    Some((side, key)) => devmap_analyze::combine_reasons(
                        coverage_gap,
                        self.radius_attribution_gap(
                            index.generation(),
                            index.analysis(),
                            &BTreeSet::from([key]),
                            side,
                        )?,
                    ),
                    None => coverage_gap,
                }
            };
            let reason = match call_blind.reason() {
                Some(blind) => format!("{target} has no indexed traversal start: {blind}"),
                None => format!("{target} has no indexed traversal start"),
            };
            let mut response = unavailable_response(ResolutionAvailability::Unavailable {
                reason: reason.clone(),
            });
            response.walk_incomplete = coverage_gap.clone();
            // "Nothing looked" and "nothing was found" must not render alike on
            // either half: the bands go out `Unavailable` with the target named,
            // never as a radius of zero.
            let bands = band_budget.map(|_| BlastRadius {
                seeds: Vec::new(),
                unmatched_targets: vec![target.to_string()],
                layers: {
                    let mut layers =
                        unavailable_response(ResolutionAvailability::Unavailable { reason });
                    layers.walk_incomplete = coverage_gap.clone();
                    layers
                },
                total_impacted: 0,
            });
            return Ok((response, bands));
        }
        // `traverse_indexed` is bounded by `max_nodes`/`max_depth` and does not
        // itself consult the flag; checking on either side of it keeps an
        // abandoned request from paying for the sort and the budgeting that
        // follow.
        self.cancel.check()?;
        let max_depth = req.max_depth.min(MAX_TRAVERSAL_DEPTH);
        let max_nodes = TRAVERSAL_MAX_NODES;
        let walk = traverse_graph_indexed(
            &start,
            direction,
            TraversalLimits {
                max_depth,
                max_nodes,
            },
        );
        self.cancel.check()?;
        let mut traversed = indexed_traversed_edges(index, &walk, min_confidence, &self.cancel)?;
        traversed.sort_by(|a, b| {
            b.confidence
                .0
                .total_cmp(&a.confidence.0)
                .then_with(|| a.source_file.cmp(&b.source_file))
                .then_with(|| a.target_file.cmp(&b.target_file))
                .then_with(|| a.source_symbol.cmp(&b.source_symbol))
        });
        // Before the budget, deliberately: a floor applied to the packed slice
        // would report a distribution of whatever happened to fit, and would
        // spend the budget on edges it was about to discard.
        let (traversed, rungs) = crate::rung::narrow(traversed, min_rung);
        let coverage_gap = devmap_analyze::combine_reasons(
            coverage_gap,
            self.radius_attribution_gap(
                index.generation(),
                index.analysis(),
                &reached_by(starts.into_iter().collect(), &traversed),
                if reverse {
                    RadiusSide::Callers
                } else {
                    RadiusSide::Callees
                },
            )?,
        );
        // Also before the budget, and for the same reason: the bands describe
        // the population the walk reached, not the slice that fitted. They are
        // built from `traversed` rather than from `walk.traversed_edges` so that
        // every banded node is an endpoint of an edge this answer measured —
        // the two halves partition one set, and a node can appear in one and not
        // the other only if the budgeter trimmed it, which the budgeter counts.
        let incomplete = devmap_analyze::combine_reasons(
            devmap_analyze::combine_reasons(walk.stop.reason(max_depth, max_nodes), coverage_gap),
            call_blind.reason(),
        );
        // Refused rather than qualified only when nothing was measured: every
        // start is call-blind *and* the walk found nothing. A bare name that
        // also matched a Python symbol has a real half, and an answer with
        // edges in it is evidence, so both keep `Available` and carry the
        // reason instead.
        let refuse_empty = call_blind
            .reason()
            .filter(|_| call_blind.every_start_is_blind() && traversed.is_empty());
        let mut bands = band_budget.map(|budget| {
            blast_radius_from_edges(
                &start,
                &traversed,
                reverse,
                max_depth,
                budget,
                incomplete.clone(),
            )
        });
        let mut response = budget_take(traversed, req.token_budget, |_| EDGE_TOKENS);
        response.rungs = Some(rungs);
        // Two independent qualifications, composed rather than ranked.
        //
        // The budgeter counts what it received. When the walk itself stopped
        // early, `total` is the size of a partial answer and `truncated: false`
        // is a claim the walk never earned — this is where `impact` said "here
        // is the blast radius" after visiting three levels of a deeper graph.
        //
        // The second is about the graph rather than the walk: a traversal that
        // ran to completion over a corpus whose call extraction did not cover
        // every file has searched everything *it has*, which is not the same as
        // everything there is. `impact` returning an empty list is the reading
        // that gets a live symbol deleted, and it read identically in both
        // cases. The disclosure rides on the index so it describes the same
        // generation the edges came from.
        //
        // The third is about the start itself (P2.7a): see `refuse_empty`.
        response.walk_incomplete = incomplete;
        if let Some(reason) = refuse_empty {
            response.resolution = ResolutionAvailability::Unavailable {
                reason: reason.clone(),
            };
            if let Some(bands) = bands.as_mut() {
                bands.layers.resolution = ResolutionAvailability::Unavailable { reason };
            }
        }
        Ok((response, bands))
    }

    /// Which of `files` a grammar read in a language with no call extractor,
    /// by [`devmap_extract::model::is_call_blind`] — the predicate `dead`
    /// prices the same files with.
    ///
    /// Read through [`Store::latest_file`], as `explore` reads a definition's
    /// language, so a daemon commit between the edge load and this read can
    /// describe the path one generation later. The language of a path cannot
    /// change between generations; its engine can only if the file was edited,
    /// which a re-ask observes.
    ///
    /// Bounded: at most [`MAX_CALL_BLIND_FILES_CHECKED`] distinct files are
    /// read, and the rest are counted as unchecked rather than as clean — a
    /// capped check must not read as one that found nothing.
    fn call_blind_starts<'f>(
        &self,
        files: impl IntoIterator<Item = &'f str>,
    ) -> anyhow::Result<CallBlindStarts> {
        let distinct: BTreeSet<&str> = files.into_iter().collect();
        let mut starts = CallBlindStarts {
            unchecked: distinct.len().saturating_sub(MAX_CALL_BLIND_FILES_CHECKED),
            ..CallBlindStarts::default()
        };
        for path in distinct.into_iter().take(MAX_CALL_BLIND_FILES_CHECKED) {
            self.cancel.check()?;
            starts.checked += 1;
            let Some(file) = self.store.latest_file(path)? else {
                continue;
            };
            if devmap_extract::model::is_call_blind(
                &file.language,
                &file.engine,
                &file.parse_outcome,
            ) {
                starts.blind.push((file.path, file.language));
            }
        }
        Ok(starts)
    }

    /// Definitions matching `query`, each with its source, both call-graph
    /// directions, and one layered blast radius over all of them.
    ///
    /// Replaces the Python `CodeIntelQueryEngine.explore`, which loaded the
    /// whole graph into process memory and walked it there. Three contract
    /// repairs came with the move, all of them in the direction of not
    /// overclaiming:
    ///
    /// * **Ranked before truncated (R7).** Python concatenated exact and
    ///   partial name matches and sliced `[:limit]`, so which definitions
    ///   survived depended on node order in the store rather than on relevance.
    ///   Here the FTS hits are scored and sorted first, and the cut is the
    ///   budgeter's.
    /// * **A snippet that could not be read is not an empty snippet (Class A).**
    ///   Python returned `""` for a file it could not open and `""` for a
    ///   zero-length span. `source_unavailable_reason` separates them.
    /// * **One edge load, not `2n + 1`.** See [`Self::traverse_over`].
    ///
    /// `limit` caps the definitions considered; the budget decides how many of
    /// those are actually packed. Both are reported, and `definitions.total` is
    /// the measured index-wide match count either way.
    pub fn explore(
        &self,
        query: &str,
        limit: usize,
        token_budget: u32,
        min_confidence: f32,
        max_depth: usize,
    ) -> anyhow::Result<ExploreReport> {
        self.explore_filtered(query, limit, token_budget, min_confidence, max_depth, None)
    }

    /// [`Self::explore`] over the files and kinds `filter` admits.
    ///
    /// `definitions.total` is the filtered match count. `scope` echoes the
    /// filter, and is absent when the caller narrowed nothing.
    pub fn explore_filtered(
        &self,
        query: &str,
        limit: usize,
        token_budget: u32,
        min_confidence: f32,
        max_depth: usize,
        filter: Option<&crate::scope::NameQueryFilter>,
    ) -> anyhow::Result<ExploreReport> {
        self.read_composed(
            || {
                self.explore_once(
                    query,
                    limit,
                    token_budget,
                    min_confidence,
                    max_depth,
                    filter,
                )
            },
            |report, note| {
                qualify_response(&mut report.definitions, note);
                qualify_response(&mut report.blast_radius.layers, note);
                for definition in &mut report.definitions.items {
                    qualify_response(&mut definition.callers, note);
                    qualify_response(&mut definition.callees, note);
                }
            },
        )
    }

    fn explore_once(
        &self,
        query: &str,
        limit: usize,
        token_budget: u32,
        min_confidence: f32,
        max_depth: usize,
        filter: Option<&crate::scope::NameQueryFilter>,
    ) -> anyhow::Result<ExploreReport> {
        // Refused here, before the empty-report shapes below can absorb it: a
        // threshold no comparison can evaluate is a bad request, not a
        // repository with nothing in it.
        devmap_store::checked_min_confidence(min_confidence)?;
        let budget = explore_budget(token_budget);
        let empty = |reason: String| ExploreReport {
            query: query.to_string(),
            definitions: unavailable_response(ResolutionAvailability::Unavailable {
                reason: reason.clone(),
            }),
            limit: u32::try_from(limit).unwrap_or(u32::MAX),
            blast_radius: BlastRadius {
                seeds: Vec::new(),
                unmatched_targets: Vec::new(),
                layers: unavailable_response(ResolutionAvailability::Unavailable { reason }),
                total_impacted: 0,
            },
            budget,
            scope: None,
        };
        if query.trim().is_empty() {
            return Ok(empty("explore requires a non-empty query".to_string()));
        }

        let page = budget_page_size(budget.definitions);
        let pool = search_rank_pool_size(budget.definitions);
        let Some(snapshot) = self.keyword_page(query, pool, filter)? else {
            return Ok(empty("no persisted generation is available".to_string()));
        };
        let total = snapshot.total;
        let scope = snapshot
            .narrowing
            .as_ref()
            .map(|narrowing| scope_report_from_narrowing(narrowing, snapshot.analysis.as_ref()));
        let coverage_gap = devmap_analyze::combine_reasons(
            search_coverage_gap(analysis_status_gap(snapshot.analysis.as_ref())),
            ranking_coverage_gap(total, pool),
        );
        let rows = snapshot.rows;
        let repo_root = snapshot.repo_root;
        let lowered = query.to_lowercase();
        // Rank the *rows*, then read files for the survivors only.
        //
        // Scoring needs `name` and `qualified_name`, both already in the row;
        // `hit_from_stored` is what opens a file. Materialising first and
        // cutting afterwards would open one file per candidate — 401 of them at
        // the default budget — in order to keep `limit` of them, which is the
        // amplification `search_semantic` was repaired for.
        let mut scored = rank_symbol_rows(rows, &lowered, &self.cancel)?;
        scored.truncate(limit.min(page));
        // `qualified_name` is carried out of the row before `hit_from_stored`
        // consumes it: the hit keeps only the bare name, and a definition that
        // reported no qualified name would be indistinguishable from one whose
        // language has none.
        let ranked: Vec<(String, SymbolHit)> = scored
            .into_iter()
            .map(|(score, row)| {
                let qualified = row.qualified_name.clone();
                (
                    qualified,
                    hit_from_stored(row, repo_root.as_deref(), budget.definitions, score),
                )
            })
            .collect();

        // Pack the definitions before any edge work: a definition the budget
        // cannot admit must not cost two traversals.
        let shells: Vec<ExploreDefinition> = ranked
            .into_iter()
            .map(|(qualified_name, hit)| ExploreDefinition {
                // `file::name` — the identity every devmap traversal surface
                // already resolves, and the one `graph_query` sends today.
                id: if qualified_name.is_empty() {
                    node_id_of(&hit.file_path, &hit.symbol_name)
                } else {
                    qualified_name.clone()
                },
                qualified_name,
                symbol_name: hit.symbol_name,
                file_path: hit.file_path,
                kind: hit.kind,
                language: None,
                span: hit.span,
                source: hit.source_span,
                source_unavailable_reason: hit.source_unavailable_reason,
                source_omitted_bytes: hit.source_span_omitted_bytes,
                score: hit.score,
                callers: budget_take(Vec::new(), 0, |_| EDGE_TOKENS),
                callees: budget_take(Vec::new(), 0, |_| EDGE_TOKENS),
            })
            .collect();
        let mut definitions = budget_take(shells, budget.definitions, explore_definition_tokens);
        // `budget_take` counts the page it was handed; the index-wide count is
        // the honest denominator, exactly as `search` reports it.
        definitions.total = total;
        definitions.hidden = definitions.total.saturating_sub(definitions.shown);
        definitions.truncated = definitions.hidden > 0;
        definitions.walk_incomplete = coverage_gap;
        // Looked up only for the definitions that survived the budget, and left
        // `None` when the generation holds no row for the file — a definition
        // whose language was never recorded must not be labelled with a guess.
        for definition in &mut definitions.items {
            definition.language = self
                .store
                .latest_file(&definition.file_path)?
                .map(|file| file.language);
        }

        self.cancel.check()?;
        let Some(index) = self.generation_edges()? else {
            return Ok(empty("no persisted generation is available".to_string()));
        };
        // One directed view per direction for the whole fan-out. There are only
        // ever two directions, so there are only ever two views, and each is a
        // borrow of the generation index the store already holds.
        let inbound = index.directed(true, min_confidence);
        let outbound = index.directed(false, min_confidence);
        let per_direction = edges_per_direction(&budget, definitions.shown);
        let mut budget = budget;
        budget.edges_per_direction = per_direction;
        for definition in &mut definitions.items {
            self.cancel.check()?;
            definition.callers = self.traverse_over(
                &index,
                &inbound,
                Request {
                    query: definition.id.clone(),
                    token_budget: per_direction,
                    min_confidence,
                    max_depth: 1,
                },
                None,
            )?;
            definition.callees = self.traverse_over(
                &index,
                &outbound,
                Request {
                    query: definition.id.clone(),
                    token_budget: per_direction,
                    min_confidence,
                    max_depth: 1,
                },
                None,
            )?;
        }

        let seeds: Vec<String> = definitions
            .items
            .iter()
            .map(|definition| definition.id.clone())
            .collect();
        let mut blast_radius = self
            .blast_walk(&index, &seeds, max_depth, min_confidence)?
            .into_radius(budget.blast_radius);
        if definitions.hidden > 0 {
            qualify_response(&mut blast_radius.layers, &format!(
                "this radius uses {} shown definition(s) of {} matches; omitted definitions may have additional callers",
                definitions.shown, definitions.total,
            ));
        }
        Ok(ExploreReport {
            query: query.to_string(),
            definitions,
            limit: u32::try_from(limit).unwrap_or(u32::MAX),
            blast_radius,
            budget,
            scope,
        })
    }

    /// Test files reachable through the inbound blast radius of `targets`.
    ///
    /// Replaces the Python `CodeIntelQueryEngine.affected_tests`. Two things
    /// changed with the move. Each test file carries the **distance** at which
    /// the walk first reached it, and the list is ranked nearest-first before
    /// truncation, so a budget-trimmed answer keeps the tests most likely to
    /// break; Python sorted alphabetically and had no cap at all. And a target
    /// that matched nothing is named in `blast_radius.unmatched_targets`
    /// instead of silently contributing no seeds — a typo used to come back as
    /// "no affected tests", which is the flattering reading of "we did not
    /// look".
    pub fn affected_tests(
        &self,
        targets: &[String],
        token_budget: u32,
        min_confidence: f32,
        max_depth: usize,
    ) -> anyhow::Result<AffectedTestsReport> {
        devmap_store::checked_min_confidence(min_confidence)?;
        let layer_budget = token_budget / 2;
        let list_budget = token_budget.saturating_sub(layer_budget);
        let empty_report = |reason: String| AffectedTestsReport {
            targets: targets.to_vec(),
            tests: unavailable_response(ResolutionAvailability::Unavailable {
                reason: reason.clone(),
            }),
            blast_radius: BlastRadius {
                seeds: Vec::new(),
                unmatched_targets: targets.to_vec(),
                layers: unavailable_response(ResolutionAvailability::Unavailable { reason }),
                total_impacted: 0,
            },
        };
        if self.store.latest_generation_id()?.is_none() {
            return Ok(empty_report(
                "no persisted generation is available".to_string(),
            ));
        }
        if targets.is_empty() {
            return Ok(empty_report(
                "affected_tests requires at least one target".to_string(),
            ));
        }
        if targets.len() > MAX_NEIGHBOR_TARGETS {
            anyhow::bail!(
                "affected accepts at most {} targets, got {}",
                MAX_NEIGHBOR_TARGETS,
                targets.len()
            );
        }

        let Some(index) = self.generation_edges()? else {
            return Ok(empty_report(
                "no persisted generation is available".to_string(),
            ));
        };
        let walk = self.blast_walk(&index, targets, max_depth, min_confidence)?;
        // A test is a symbol in a test file or one a test runner invokes, so a
        // `#[test] fn` beside the code it tests is named too; its file is the
        // entry's `path`, like any other test's.
        let test_symbols = self.store.latest_test_entry_symbols()?;

        // Derived from the *complete* walk, never from the budgeted layers.
        // Reading the presentation back would drop every test whose band the
        // token budget trimmed, and the shortfall would be invisible: the test
        // list's own counters would report a complete answer over a set that
        // had already been cut. The bands are also sampled for display at
        // `BLAST_LAYER_NODE_SAMPLE`, which would hide the 51st caller in a band
        // for the same reason.
        //
        // Depth 0 is the seed band: a target that is itself in a test file
        // counts as an affected test.
        let mut nearest: BTreeMap<String, (usize, BTreeSet<String>)> = BTreeMap::new();
        for (symbol, file) in &walk.seeds {
            record_test_hit(&mut nearest, &test_symbols, symbol, file, 0);
        }
        for band in &walk.bands {
            for (symbol, file) in &band.members {
                record_test_hit(&mut nearest, &test_symbols, symbol, file, band.depth);
            }
        }

        let mut tests: Vec<AffectedTest> = nearest
            .into_iter()
            .map(|(path, (depth, symbols))| AffectedTest {
                path,
                depth,
                reached_symbols: u32::try_from(symbols.len()).unwrap_or(u32::MAX),
                symbols: symbols.into_iter().take(AFFECTED_SYMBOL_SAMPLE).collect(),
            })
            .collect();
        // Nearest first, then alphabetically — ranked before the budgeter cuts.
        tests.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.path.cmp(&b.path)));
        let mut response = budget_take(tests, list_budget, affected_test_tokens);
        // A test list derived from a walk that stopped early is a lower bound,
        // and the counters above cannot say so — they describe the budget.
        response.walk_incomplete = walk.incomplete_reason();
        Ok(AffectedTestsReport {
            targets: targets.to_vec(),
            tests: response,
            blast_radius: walk.into_radius(layer_budget),
        })
    }

    /// Inbound reachability from `targets`, banded by distance.
    ///
    /// [`traverse_graph`] answers *what* is reachable and [`Response`] carries
    /// how much of that fit; neither carries *how far*, and `TraversalResult`
    /// does not expose per-node depth. So the banding is done here, over the
    /// same edge set, under the same `max_nodes` bound, and it reports the same
    /// kind of incompleteness reason — a blast radius that stopped at the cap
    /// must not read like one that ran out of graph.
    ///
    /// Returns the **complete** walk. Sampling and token budgeting happen in
    /// [`BlastWalk::into_radius`], at the presentation boundary, so anything
    /// derived from the walk — the affected-test list — sees everything the
    /// walk reached rather than what a budget left of it.
    fn blast_walk(
        &self,
        index: &GenerationEdges,
        targets: &[String],
        max_depth: usize,
        min_confidence: f32,
    ) -> anyhow::Result<BlastWalk> {
        let min_confidence = devmap_store::checked_min_confidence(min_confidence)?;
        let depth_cap = max_depth.min(MAX_TRAVERSAL_DEPTH);
        let mut seed_set: BTreeSet<(String, String)> = BTreeSet::new();
        let mut unmatched: Vec<String> = Vec::new();
        for target in targets {
            let matched =
                indexed_traversal_starts(index, target.trim(), true, min_confidence, &self.cancel)?;
            if matched.is_empty() {
                unmatched.push(target.clone());
                continue;
            }
            seed_set.extend(matched);
        }
        let starts_dropped = seed_set.len().saturating_sub(TRAVERSAL_MAX_NODES);
        let seeds: Vec<(String, String)> = seed_set.into_iter().take(TRAVERSAL_MAX_NODES).collect();
        let mut walk = BlastWalk {
            seeds,
            unmatched,
            bands: Vec::new(),
            total_impacted: 0,
            stop: TraversalStop {
                starts_dropped,
                ..TraversalStop::default()
            },
            depth_cap,
            unresolved_seeds: false,
            // The corpus half; the reached radius adds its own below.
            coverage_gap: analysis_status_gap(index.analysis()),
        };
        if walk.seeds.is_empty() {
            walk.unresolved_seeds = true;
            return Ok(walk);
        }

        // The inbound edges of one node, in the generation's own order, at or
        // above the floor. Two thresholds for the same reason
        // `indexed_traversed_edges` applies two: `admits` is the store's
        // rounded comparison for what exists, the plain compare is the one this
        // walk applied before an index existed, and an edge can pass one and
        // fail the other. The whole-generation `BTreeMap` this replaced was
        // rebuilt per call, which is the cost the index exists to remove.
        let admits =
            |id| index.admits(id, min_confidence) && index.confidence(id) >= min_confidence;

        let mut visited: BTreeSet<String> = walk
            .seeds
            .iter()
            .map(|(symbol, _)| symbol.clone())
            .collect();
        let mut frontier: Vec<String> = visited.iter().cloned().collect();
        let mut checked = 0usize;
        for depth in 1..=depth_cap {
            self.cancel.check()?;
            let mut members: BTreeSet<(String, String)> = BTreeSet::new();
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let mut lowest: Option<f32> = None;
            for node in &frontier {
                for id in index.into_target_symbol(node).iter().copied() {
                    self.cancel.check_every(checked)?;
                    checked = checked.wrapping_add(1);
                    if !admits(id) {
                        continue;
                    }
                    let source_symbol = index.source_symbol(id);
                    if visited.contains(source_symbol) || seen.contains(source_symbol) {
                        continue;
                    }
                    // The cap is a *withholding*, recorded as one. A radius
                    // that stopped at 5,000 nodes must not be readable as one
                    // that ran out of graph.
                    if visited.len() + seen.len() >= TRAVERSAL_MAX_NODES {
                        walk.stop.node_capped = true;
                        continue;
                    }
                    seen.insert(source_symbol.to_string());
                    // The file comes from the edge that actually reached this
                    // node, not from a global symbol-to-file guess: the same
                    // qualified name can appear in two files, and attributing a
                    // reached symbol to the wrong one puts the wrong test in
                    // the answer.
                    members.insert((source_symbol.to_string(), index.source_file(id).to_string()));
                    let confidence = index.confidence(id);
                    lowest = Some(match lowest {
                        Some(current) => current.min(confidence),
                        None => confidence,
                    });
                }
            }
            if seen.is_empty() {
                break;
            }
            walk.total_impacted = walk
                .total_impacted
                .saturating_add(u32::try_from(seen.len()).unwrap_or(u32::MAX));
            walk.bands.push(BlastBand {
                depth,
                members,
                lowest_confidence: lowest,
                node_count: u32::try_from(seen.len()).unwrap_or(u32::MAX),
            });
            visited.extend(seen.iter().cloned());
            frontier = seen.into_iter().collect();
        }
        // Also inspect the seed frontier at depth zero. No hops were requested,
        // but a caller must still know whether further reachability was withheld.
        'probe: for node in &frontier {
            self.cancel.check()?;
            for id in index.into_target_symbol(node).iter().copied() {
                self.cancel.check_every(checked)?;
                checked = checked.wrapping_add(1);
                if admits(id) && !visited.contains(index.source_symbol(id)) {
                    walk.stop.depth_capped = walk.bands.len() == depth_cap;
                    break 'probe;
                }
            }
        }
        let reached: BTreeSet<(String, String)> = walk
            .seeds
            .iter()
            .cloned()
            .chain(
                walk.bands
                    .iter()
                    .flat_map(|band| band.members.iter().cloned()),
            )
            .collect();
        let radius = self.radius_attribution_gap(
            index.generation(),
            index.analysis(),
            &reached,
            RadiusSide::Callers,
        )?;
        walk.coverage_gap = devmap_analyze::combine_reasons(walk.coverage_gap.take(), radius);
        Ok(walk)
    }

    /// Symbols the latest generation found nothing calling.
    ///
    /// The answer carries the coverage it was computed over. This list is read
    /// as "delete these", and without a denominator a generation with 4,242
    /// unattributed calls answered in exactly the shape of one with none —
    /// `resolution: Available`, `truncated: false`, and not a word about
    /// either `AnalysisSummary::status` or `unresolved_calls`, the field whose
    /// own documentation says it exists so a reader can tell "nothing calls
    /// this" from "we could not work out what this calls".
    ///
    /// It rides on `walk_incomplete` rather than a wrapper struct because that
    /// is the field this crate already has for "the producer of these items
    /// did not see everything", it is already rendered by the CLI and already
    /// read by `DevMapClient._budgeted`, and a second shape for the same
    /// statement is a second thing for a consumer to miss.
    pub fn dead_symbols(
        &self,
        token_budget: u32,
    ) -> anyhow::Result<Response<devmap_analyze::DeadSymbolReport>> {
        // One snapshot. This resolved the generation three times — an existence
        // check, the analysis, then the rows — so the coverage disclosure could
        // describe a different generation than the findings it was attached to.
        // That combination is what promotes a row from "look at this" to "safe
        // to delete": a disclosure saying the corpus was fully covered, over
        // rows from a generation where it was not.
        //
        // Bounded by the answer, not by the corpus. The exempt filter and the
        // cut both run in SQL now; this used to materialise every dead row of
        // the generation and drop almost all of them here — 80,000 read to show
        // 66 on the benchmark corpus. One more row than the budget can seat is
        // read on purpose, so `budget_take` still sees something it cannot fit
        // and reports `truncated` for the right reason.
        let limit = (token_budget / DEAD_SYMBOL_TOKENS) as usize + 1;
        let Some(page) = self.store.dead_page(limit)? else {
            return Ok(self.unavailable(ResolutionAvailability::Unavailable {
                reason: "no persisted generation is available".to_string(),
            }));
        };
        let mut response = budget_take(page.rows, token_budget, |_| DEAD_SYMBOL_TOKENS);
        // `budget_take` counts the page it was handed, and the page is now a
        // bounded read — so the generation-wide count has to be restored as the
        // denominator, exactly as `explore` does for definitions. Without this
        // a capped list would report itself as the whole truth.
        response.total = u32::try_from(page.total_non_exempt)
            .unwrap_or(u32::MAX)
            .max(response.shown);
        response.hidden = response.total.saturating_sub(response.shown);
        response.truncated = response.hidden > 0;
        response.walk_incomplete = analysis_coverage_gap(page.analysis.as_ref());
        // The other half of the answer, and until now the half nothing could
        // see. A one-hop inbound-edge join structurally cannot report a
        // subsystem whose functions call each other, so a `dead` answer without
        // the component pass is not a shorter list — it is a list missing an
        // entire class of finding. `DeadClusterScan::clusters` is capped at
        // `DEAD_CLUSTER_CAP` by the producer, so this needs no budget of its
        // own; what the cap dropped travels beside it.
        if let Some(scan) = page.dead_clusters {
            if scan.refused_oversized_graph {
                // The one outcome an `Option<Vec<_>>` cannot hold. Leaving the
                // empty list here would say the walk ran and found nothing.
                response.dead_clusters_incomplete = Some(format!(
                    "the call graph exceeded {} distinct symbols, so no \
                     component scan ran for this generation",
                    devmap_analyze::dead_clusters::DEAD_CLUSTER_MAX_NODES
                ));
            } else {
                response.dead_clusters_truncated = scan.truncated_clusters;
                response.dead_clusters = Some(scan.clusters);
            }
        }
        Ok(self.finish(response))
    }

    /// Duplicate bodies in the latest generation.
    ///
    /// Grouping runs here rather than at build time. The signatures are stored
    /// per symbol, so the groups are derivable on demand and never go stale
    /// against the rows they came from — and a build does not pay for a report
    /// most builds have no reader for.
    /// `kind` and `min_nodes` narrow the report; `None` and `0` mean no filter.
    ///
    /// The filters are applied *before* the budget, and that ordering is the
    /// whole contract. Filtering afterwards would narrow a list the budget had
    /// already cut, so `--min-nodes` could never reach past the first page —
    /// and, because re-budgeting the survivors leaves `hidden` at zero, the
    /// subset would be returned as a complete answer. Measured on this
    /// repository: `--kind exact --min-nodes 100` under a 900-token budget
    /// reported "2 groups, not truncated" where the true answer was 29.
    pub fn clones(
        &self,
        token_budget: u32,
        kind: Option<devmap_analyze::CloneKind>,
        min_nodes: u32,
    ) -> anyhow::Result<CloneReport> {
        if self.store.latest_generation_id()?.is_none() {
            return Ok(CloneReport {
                groups: unavailable_response(ResolutionAvailability::Unavailable {
                    reason: "no persisted generation is available".to_string(),
                }),
                signed_symbols: 0,
                unsigned_symbols: 0,
            });
        }
        let (candidates, unsigned) = self.store.latest_clone_candidates()?;
        let summary = group_clones(&candidates, unsigned);
        let matching: Vec<_> = summary
            .groups
            .into_iter()
            .filter(|group| kind.is_none_or(|wanted| group.kind == wanted))
            .filter(|group| group.min_nodes >= min_nodes)
            .collect();
        Ok(CloneReport {
            groups: budget_take(matching, token_budget, clone_group_tokens),
            signed_symbols: summary.signed_symbols,
            unsigned_symbols: summary.unsigned_symbols,
        })
    }

    /// Definitions in one file as signature plus span — never the body.
    ///
    /// An empty file and a path the index does not contain are different
    /// envelopes ([`SkeletonPresence`]). When `signature` is absent the span
    /// is still returned and `signature_note` says so explicitly.
    pub fn skeleton(&self, path: &str, token_budget: u32) -> anyhow::Result<SkeletonReport> {
        self.cancel.check()?;
        let path = resolve_skeleton_path(self.store, path)?;
        let freshness = SourceFreshness::from_store(self.store.query_source_freshness());
        let Some(extraction) = self.store.latest_extraction_for_path(&path)? else {
            // Distinguish "no generation at all" from "this path is absent".
            // Both look like an empty list; only the latter is a finding about
            // the path the caller named.
            if self.store.latest_generation_id()?.is_none() {
                return Ok(SkeletonReport {
                    file: path,
                    presence: SkeletonPresence::NotInIndex,
                    items: Vec::new(),
                    shown: 0,
                    total: 0,
                    truncated: false,
                    source_freshness: freshness,
                    resolution: ResolutionAvailability::Unavailable {
                        reason: "no persisted generation is available".to_string(),
                    },
                });
            }
            return Ok(SkeletonReport {
                file: path,
                presence: SkeletonPresence::NotInIndex,
                items: Vec::new(),
                shown: 0,
                total: 0,
                truncated: false,
                source_freshness: freshness,
                resolution: ResolutionAvailability::Available,
            });
        };

        if extraction.symbols.is_empty() {
            return Ok(SkeletonReport {
                file: path,
                presence: SkeletonPresence::Empty,
                items: Vec::new(),
                shown: 0,
                total: 0,
                truncated: false,
                source_freshness: freshness,
                resolution: ResolutionAvailability::Available,
            });
        }

        // Prefer the on-disk file for line conversion so the numbers match what
        // an editor shows. Spans were recorded against the indexed content; a
        // dirty buffer can shift them, which is disclosed via lines_from_bytes
        // only when the file cannot be read at all.
        let source_root = self.store.latest_repo_root()?;
        let on_disk = devmap_extract::safe_fs::read_repo_source(
            source_root.as_deref().map(Path::new),
            &path,
            devmap_extract::MAX_SOURCE_BYTES,
        )
        .ok();
        let line_index = on_disk.as_deref().map(LineIndex::new);
        let lines_from_bytes = line_index.is_none();

        let mut items: Vec<SkeletonSymbol> = extraction
            .symbols
            .iter()
            .map(|sym| {
                let (start_line, end_line) = match &line_index {
                    Some(index) => byte_span_to_line_range_in(index, &sym.span),
                    None => (
                        (sym.span.start_byte as u32).saturating_add(1),
                        (sym.span.end_byte as u32).saturating_add(1).max(1),
                    ),
                };
                let (signature, signature_note) = match &sym.signature {
                    Some(text) if !text.is_empty() => (Some(text.clone()), None),
                    _ => (None, Some("not extracted".to_string())),
                };
                SkeletonSymbol {
                    qualified_name: sym.qualified_name.clone(),
                    kind: sym.kind.as_str().to_string(),
                    start_line,
                    end_line,
                    start_byte: sym.span.start_byte,
                    end_byte: sym.span.end_byte,
                    lines_from_bytes,
                    signature,
                    signature_note,
                }
            })
            .collect();
        // Stable order: declaration order in the file (by start byte), then name.
        items.sort_by(|a, b| {
            a.start_byte
                .cmp(&b.start_byte)
                .then(a.qualified_name.cmp(&b.qualified_name))
        });

        let total = u32::try_from(items.len()).unwrap_or(u32::MAX);
        let mut packed = budget_take(items, token_budget, skeleton_symbol_tokens);
        packed.total = total;
        packed.hidden = total.saturating_sub(packed.shown);
        packed.truncated = packed.hidden > 0;

        Ok(SkeletonReport {
            file: path,
            presence: SkeletonPresence::Indexed,
            items: packed.items,
            shown: packed.shown,
            total: packed.total,
            truncated: packed.truncated,
            source_freshness: freshness,
            resolution: ResolutionAvailability::Available,
        })
    }

    /// Rank symbols by TF-IDF similarity of their names to `query`.
    ///
    /// Complements `search`, which is FTS5 prefix matching: that finds symbols
    /// whose names *contain* the query, this finds symbols whose names are
    /// *about* it. `LLMCache` for "llm cache", `compute_freshness` for
    /// "freshness computation".
    ///
    /// Scores nothing when no symbol shares a term with the query, rather than
    /// returning the whole corpus ordered by a zero. "Nothing matched" is an
    /// answer.
    pub fn search_semantic(
        &self,
        query: &str,
        token_budget: u32,
    ) -> anyhow::Result<Response<SymbolHit>> {
        self.search_semantic_scoped(query, token_budget, None)
    }

    /// [`Self::search_semantic`] over the symbols `scope` admits, with IDF
    /// computed over that corpus alone. `None` is the whole repository. A
    /// scope that names no indexed file is refused; see [`crate::scope`].
    pub fn search_semantic_scoped(
        &self,
        query: &str,
        token_budget: u32,
        scope: Option<&crate::scope::SymbolScope>,
    ) -> anyhow::Result<Response<SymbolHit>> {
        self.search_semantic_filtered(query, token_budget, scope, &[])
    }

    /// [`Self::search_semantic_scoped`] further restricted to `kinds`.
    ///
    /// Kinds are retained before scoring, so `total` counts only matches of
    /// those kinds. A kind that labels nothing in the already path- and
    /// language-scoped rows is refused. Kind-only, with no [`SymbolScope`],
    /// still sets `scope`.
    pub fn search_semantic_filtered(
        &self,
        query: &str,
        token_budget: u32,
        scope: Option<&crate::scope::SymbolScope>,
        kinds: &[String],
    ) -> anyhow::Result<Response<SymbolHit>> {
        self.cancel.check()?;
        let Some((mut snapshot, resolved)) = self.ranking_corpus(scope)? else {
            return Ok(self.unavailable(ResolutionAvailability::Unavailable {
                reason: "no persisted generation is available".to_string(),
            }));
        };
        let mut report = resolved.map(|resolved| resolved.report);
        if !kinds.is_empty() {
            let present: BTreeSet<String> =
                snapshot.rows.iter().map(|row| row.kind.clone()).collect();
            let missing: Vec<String> = kinds
                .iter()
                .filter(|kind| !present.contains(kind.as_str()))
                .cloned()
                .collect();
            if !missing.is_empty() {
                anyhow::bail!(crate::scope::missing_kind_message(
                    &missing,
                    &present,
                    scope.is_some()
                ));
            }
            let loaded = snapshot.rows.len();
            let loaded_paths = snapshot
                .rows
                .iter()
                .map(|row| row.path.as_str())
                .collect::<BTreeSet<_>>()
                .len();
            snapshot
                .rows
                .retain(|row| kinds.iter().any(|kind| kind == &row.kind));
            let kind_symbols = u32::try_from(snapshot.rows.len()).unwrap_or(u32::MAX);
            snapshot.total = kind_symbols;
            let kind_files = snapshot
                .rows
                .iter()
                .map(|row| row.path.as_str())
                .collect::<BTreeSet<_>>()
                .len();
            let mut built = report.unwrap_or_else(|| crate::scope::ScopeReport {
                paths: Vec::new(),
                languages: Vec::new(),
                kinds: Vec::new(),
                files: 0,
                symbols: 0,
                corpus_files: 0,
                corpus_symbols: 0,
                related_tests_outside_scope: 0,
            });
            built.kinds = kinds.to_vec();
            built.symbols = kind_symbols;
            if scope.is_none() {
                built.files = u32::try_from(kind_files).unwrap_or(u32::MAX);
                built.corpus_files = match snapshot.analysis.as_ref() {
                    Some(disclosure) if disclosure.total_files > 0 => {
                        u32::try_from(disclosure.total_files).unwrap_or(u32::MAX)
                    }
                    _ => u32::try_from(loaded_paths).unwrap_or(u32::MAX),
                };
                built.corpus_symbols = match snapshot.analysis.as_ref() {
                    Some(disclosure) if disclosure.total_symbols > 0 => {
                        u32::try_from(disclosure.total_symbols).unwrap_or(u32::MAX)
                    }
                    _ => u32::try_from(loaded).unwrap_or(u32::MAX),
                };
            } else if let Some(disclosure) = snapshot.analysis.as_ref() {
                if disclosure.total_files > 0 {
                    built.corpus_files = u32::try_from(disclosure.total_files).unwrap_or(u32::MAX);
                }
                if disclosure.total_symbols > 0 {
                    built.corpus_symbols =
                        u32::try_from(disclosure.total_symbols).unwrap_or(u32::MAX);
                }
            }
            report = Some(built);
        }
        let mut response = self.search_semantic_over(query, token_budget, snapshot)?;
        response.scope = report;
        Ok(response)
    }

    /// Sites where a string value is written.
    ///
    /// `exact` is equality. Otherwise `query` is a case-sensitive prefix, so
    /// `session.` matches `session.spawn`. A Rust module-level const's value
    /// is also reported at each use of that const. `shown + hidden == total`.
    /// `walk_incomplete` is set when no generation exists, or when the page
    /// cap cut the read before the budget did.
    pub fn literals(
        &self,
        query: &str,
        exact: bool,
        token_budget: u32,
    ) -> anyhow::Result<LiteralReport> {
        if query.trim().is_empty() {
            anyhow::bail!("literals requires a non-empty query");
        }
        let Some(page) = self.store.search_literals(query, exact, LITERAL_PAGE_CAP)? else {
            return Ok(LiteralReport {
                query: query.to_string(),
                exact,
                items: Vec::new(),
                shown: 0,
                hidden: 0,
                total: 0,
                truncated: false,
                tokens_used: 0,
                walk_incomplete: Some("no persisted generation is available".to_string()),
            });
        };
        let fetched = page.rows.len();
        let mut items = Vec::new();
        let mut tokens_used = 0u32;
        for row in page.rows {
            let site = LiteralSite {
                file_path: row.file_path,
                line: row.line,
                value: row.value,
                qualified_name: row.qualified_name,
                symbol_name: row.symbol_name,
            };
            let cost = literal_site_tokens(&site);
            if cost > token_budget.saturating_sub(tokens_used) {
                break;
            }
            tokens_used += cost;
            items.push(site);
        }
        let shown = u32::try_from(items.len()).unwrap_or(u32::MAX);
        let total = page.total;
        let hidden = total.saturating_sub(shown);
        let walk_incomplete = (shown == u32::try_from(fetched).unwrap_or(u32::MAX) && total > shown)
            .then(|| {
                format!(
                    "literal page cap of {LITERAL_PAGE_CAP} cut the read; {total} sites match and {fetched} were loaded"
                )
            });
        Ok(LiteralReport {
            query: query.to_string(),
            exact,
            items,
            shown,
            hidden,
            total,
            truncated: hidden > 0,
            tokens_used,
            walk_incomplete,
        })
    }

    /// The symbol rows a ranking runs over: the whole latest generation, or
    /// the part of it `scope` admits, with the scope checked against the files
    /// that same generation indexed.
    ///
    /// Filtered here, before any index is built, so the term weights and the
    /// call subgraph a scoped ranking uses are the scope's own. `None` when
    /// the store holds no generation.
    fn ranking_corpus(
        &self,
        scope: Option<&crate::scope::SymbolScope>,
    ) -> anyhow::Result<
        Option<(
            devmap_store::SearchPage,
            Option<crate::scope::ResolvedScope>,
        )>,
    > {
        let Some(scope) = scope else {
            return Ok(self.store.all_symbols_page()?.map(|page| (page, None)));
        };
        let Some((mut page, files)) = self.store.all_symbols_page_with_files()? else {
            return Ok(None);
        };
        let mut resolved = scope.resolve(&files, page.repo_root.as_deref())?;
        let corpus_symbols = page.rows.len();
        page.rows.retain(|row| resolved.contains(&row.path));
        resolved.report.symbols = u32::try_from(page.rows.len()).unwrap_or(u32::MAX);
        resolved.report.corpus_symbols = u32::try_from(corpus_symbols).unwrap_or(u32::MAX);
        page.total = resolved.report.symbols;
        Ok(Some((page, Some(resolved))))
    }

    fn search_semantic_over(
        &self,
        query: &str,
        token_budget: u32,
        snapshot: devmap_store::SearchPage,
    ) -> anyhow::Result<Response<SymbolHit>> {
        let coverage_gap = search_coverage_gap(analysis_status_gap(snapshot.analysis.as_ref()));
        let symbols = snapshot.rows;
        if symbols.is_empty() || query.trim().is_empty() {
            let mut response = budget_take(Vec::new(), token_budget, |_| 0);
            response.walk_incomplete = coverage_gap;
            return Ok(self.finish(response));
        }
        // Both names, so a query can match either the bare symbol or the path
        // and type it sits under.
        let texts: Vec<String> = symbols
            .iter()
            .map(|s| format!("{} {}", s.name, s.qualified_name))
            .collect();
        let index = crate::semantic::SemanticIndex::build(&texts, &self.cancel)?;

        let repo_root = snapshot.repo_root;
        let scored = index.score(query, &self.cancel)?;
        let total = u32::try_from(scored.len()).unwrap_or(u32::MAX);
        let head: Vec<usize> = scored
            .iter()
            .take(crate::ask::ASK_COVERAGE_HEAD)
            .map(|(position, _)| *position)
            .collect();
        // Materialise only as far down the ranking as the budget could reach.
        // Every scored symbol used to be turned into a `SymbolHit` first — one
        // `read_to_string` each — and budgeted afterwards, so a query matching
        // a common term opened every file it matched in order to discard almost
        // all of them. The ranking is already sorted, so the page bound is the
        // same one keyword search uses.
        let mut hits = Vec::new();
        for (position, score) in scored.into_iter().take(search_page_size(token_budget)) {
            self.cancel.check()?;
            hits.push(hit_from_stored(
                symbols[position].clone(),
                repo_root.as_deref(),
                token_budget,
                score,
            ));
        }
        // `total` is the whole ranked corpus, not the page: budgeting a page
        // and reporting its length as the total is how a capped sample comes
        // back labelled complete.
        let mut response = budget_take(hits, token_budget, search_hit_tokens);
        response.total = total;
        response.hidden = total.saturating_sub(response.shown);
        response.truncated = response.hidden > 0;
        // The same disclosure the keyword surface makes, for the same reason.
        // This function's own doc says "'Nothing matched' is an answer" — it
        // is, and it is also an answer whose scope the caller cannot see, since
        // the texts scored here are `name` and `qualified_name` and nothing
        // else. See [`SEARCH_SCOPE_NOTE`].
        response.walk_incomplete = devmap_analyze::combine_reasons(
            devmap_analyze::combine_reasons(
                coverage_gap,
                empty_semantic_gap(response.total, query),
            ),
            (!head.is_empty())
                .then(|| index.coverage_note(query, &head))
                .flatten(),
        );
        Ok(self.finish(response))
    }

    /// Plain-language find: name+docstring TF-IDF seeds, re-ranked by
    /// personalized PageRank over stored call edges.
    ///
    /// Distinct from [`Self::search_semantic`], which ranks names only and
    /// never walks the graph. Default `min_confidence` is the deterministic
    /// rung ([`crate::ASK_DEFAULT_MIN_CONFIDENCE`]); edges below it are
    /// excluded unless the caller lowers the floor. A seed set whose only
    /// call edges sit below that floor returns empty with an explicit
    /// withheld line — matches were not absent.
    pub fn ask(
        &self,
        query: &str,
        token_budget: u32,
        min_confidence: f32,
    ) -> anyhow::Result<Response<SymbolHit>> {
        self.ask_scoped(query, token_budget, min_confidence, None)
    }

    /// [`Self::ask`] over the symbols `scope` admits: seeds are scored with
    /// IDF over the scoped corpus, and PageRank walks only call edges whose
    /// two ends are both in scope. `None` is the whole repository. A scope
    /// that names no indexed file is refused; see [`crate::scope`].
    pub fn ask_scoped(
        &self,
        query: &str,
        token_budget: u32,
        min_confidence: f32,
        scope: Option<&crate::scope::SymbolScope>,
    ) -> anyhow::Result<Response<SymbolHit>> {
        Ok(self
            .ask_ranked(query, token_budget, min_confidence, false, scope)?
            .0)
    }

    /// [`Self::ask`], answered as an evidence pack: hits grouped by file in
    /// rank order, each file marked test or implementation, the call edges
    /// that connect the hits, a nested hit's source folded into the hit that
    /// already shows it, and the test files that reach the implementation
    /// hits. See [`crate::evidence`].
    ///
    /// The budget is split: a [`EVIDENCE_TEST_BUDGET_SHARE`]th of it is held
    /// for the related-test list and the rest goes to the hits, so the pack as
    /// a whole never exceeds `token_budget`.
    pub fn ask_evidence(
        &self,
        query: &str,
        token_budget: u32,
        min_confidence: f32,
    ) -> anyhow::Result<crate::evidence::EvidencePack> {
        self.ask_evidence_scoped(query, token_budget, min_confidence, None)
    }

    /// [`Self::ask_evidence`] within `scope`, ranked as [`Self::ask_scoped`]
    /// ranks. `related_tests` lists only test files in scope; the pack's
    /// `scope.related_tests_outside_scope` counts the ones the walk reached
    /// and left out.
    pub fn ask_evidence_scoped(
        &self,
        query: &str,
        token_budget: u32,
        min_confidence: f32,
        scope: Option<&crate::scope::SymbolScope>,
    ) -> anyhow::Result<crate::evidence::EvidencePack> {
        let test_budget = token_budget / EVIDENCE_TEST_BUDGET_SHARE;
        let (response, qualified, resolved) = self.ask_ranked(
            query,
            token_budget - test_budget,
            min_confidence,
            true,
            scope,
        )?;
        let (edges, test_symbols) = if response.items.is_empty() {
            (None, std::collections::HashSet::new())
        } else {
            (
                self.generation_edges()?,
                self.store.latest_test_entry_symbols()?,
            )
        };
        let (related_tests, coverage_gap, outside_scope) = match edges.as_deref() {
            Some(edges) => self.evidence_related_tests(
                edges,
                &response,
                &qualified,
                &test_symbols,
                test_budget,
                min_confidence,
                resolved.as_ref(),
            )?,
            None => (
                self.finish(budget_take(Vec::new(), test_budget, |_| 0)),
                None,
                0,
            ),
        };
        // The pack states both directions of every hit — `calls` and
        // `called_by` — so its gap does too: the walk above asked about the
        // callers it could not follow, and this asks about the calls inside the
        // hits themselves.
        let callee_gap = match edges.as_deref() {
            Some(edges) => {
                let hits: BTreeSet<(String, String)> = response
                    .items
                    .iter()
                    .zip(&qualified)
                    .map(|(hit, name)| (name.clone(), hit.file_path.clone()))
                    .collect();
                self.radius_attribution_gap(
                    edges.generation(),
                    edges.analysis(),
                    &hits,
                    RadiusSide::Callees,
                )?
            }
            None => None,
        };
        let mut pack = crate::evidence::assemble(
            response,
            &qualified,
            edges.as_deref(),
            min_confidence,
            &test_symbols,
            related_tests,
            &self.cancel,
        )?;
        pack.coverage_gap = devmap_analyze::combine_reasons(coverage_gap, callee_gap);
        if let Some(scope) = pack.scope.as_mut() {
            scope.related_tests_outside_scope = outside_scope;
        }
        Ok(pack)
    }

    /// Test files reaching the pack's implementation hits, nearest first.
    ///
    /// The same inbound walk as [`Self::affected_tests`], seeded with the
    /// implementation hits' qualified names (at most [`MAX_NEIGHBOR_TARGETS`],
    /// best-ranked first). A reached symbol is a test when its file is a test
    /// path — `affected_tests`' rule — *or* a test runner invokes it, so a
    /// `#[test] fn` beside the code it tests is found too. Tests that are
    /// already hits are dropped *before* budgeting, so the counters describe
    /// the list as returned.
    ///
    /// Returns the walk's coverage gap separately from the list's own
    /// `walk_incomplete`. It belongs to the pack — the hits' `called_by` share
    /// it — and folded into the list it buried the one clause about *this*
    /// walk — where it stopped.
    ///
    /// Under a `scope`, a reached test file outside it is left out and
    /// counted: the third value is how many distinct such files there were.
    /// The walk itself is not scoped — a test in scope that reaches a hit
    /// through code outside it is still found.
    #[allow(clippy::too_many_arguments)]
    fn evidence_related_tests(
        &self,
        edges: &GenerationEdges,
        response: &Response<SymbolHit>,
        qualified: &[String],
        test_symbols: &std::collections::HashSet<String>,
        token_budget: u32,
        min_confidence: f32,
        scope: Option<&crate::scope::ResolvedScope>,
    ) -> anyhow::Result<(Response<AffectedTest>, Option<String>, u32)> {
        let mut targets: Vec<String> = Vec::new();
        let mut hits: BTreeSet<(&str, &str)> = BTreeSet::new();
        for (hit, name) in response.items.iter().zip(qualified) {
            hits.insert((hit.file_path.as_str(), name.as_str()));
            let is_test = is_test_path(&hit.file_path) || test_symbols.contains(name);
            if !is_test && targets.len() < MAX_NEIGHBOR_TARGETS && !targets.contains(name) {
                targets.push(name.clone());
            }
        }
        if targets.is_empty() {
            return Ok((
                self.finish(budget_take(Vec::new(), token_budget, |_| 0)),
                None,
                0,
            ));
        }
        let walk = self.blast_walk(edges, &targets, EVIDENCE_TEST_DEPTH, min_confidence)?;
        // Seeds are implementation hits by construction, so only the bands can
        // hold a test.
        let mut nearest: BTreeMap<String, (usize, BTreeSet<String>)> = BTreeMap::new();
        let mut outside_scope: BTreeSet<&str> = BTreeSet::new();
        for band in &walk.bands {
            for (symbol, file) in &band.members {
                let is_test = is_test_path(file) || test_symbols.contains(symbol);
                if !is_test || hits.contains(&(file.as_str(), symbol.as_str())) {
                    continue;
                }
                if scope.is_some_and(|scope| !scope.contains(file)) {
                    outside_scope.insert(file.as_str());
                    continue;
                }
                let entry = nearest
                    .entry(file.clone())
                    .or_insert((band.depth, BTreeSet::new()));
                entry.0 = entry.0.min(band.depth);
                entry.1.insert(symbol.clone());
            }
        }
        let mut tests: Vec<AffectedTest> = nearest
            .into_iter()
            .map(|(path, (depth, symbols))| AffectedTest {
                path,
                depth,
                reached_symbols: u32::try_from(symbols.len()).unwrap_or(u32::MAX),
                symbols: symbols.into_iter().take(AFFECTED_SYMBOL_SAMPLE).collect(),
            })
            .collect();
        tests.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.path.cmp(&b.path)));
        let mut related = budget_take(tests, token_budget, affected_test_tokens);
        related.walk_incomplete = walk.stop.reason(walk.depth_cap, TRAVERSAL_MAX_NODES);
        let outside_scope = u32::try_from(outside_scope.len()).unwrap_or(u32::MAX);
        Ok((self.finish(related), walk.coverage_gap, outside_scope))
    }

    /// The ask walk, with each shown hit's qualified name alongside it.
    ///
    /// `SymbolHit` carries the bare name, and a bare name cannot be joined
    /// against call edges — two files can each define `run`. The second vector
    /// is index-aligned with `response.items` and exists for that join.
    ///
    /// `fold_aware` budgets the page as the evidence pack will print it: a hit
    /// inside an earlier hit's whole source costs only its lead
    /// ([`crate::evidence::fold_aware_take`]). Plain `ask` prints every hit's
    /// source and budgets it so.
    ///
    /// The third value is the checked scope, `None` when unscoped or when no
    /// generation exists.
    #[allow(clippy::type_complexity)]
    fn ask_ranked(
        &self,
        query: &str,
        token_budget: u32,
        min_confidence: f32,
        fold_aware: bool,
        scope: Option<&crate::scope::SymbolScope>,
    ) -> anyhow::Result<(
        Response<SymbolHit>,
        Vec<String>,
        Option<crate::scope::ResolvedScope>,
    )> {
        self.cancel.check()?;
        let min_confidence = devmap_store::checked_min_confidence(min_confidence)?;
        let Some((snapshot, resolved)) = self.ranking_corpus(scope)? else {
            return Ok((
                self.unavailable(ResolutionAvailability::Unavailable {
                    reason: "no persisted generation is available".to_string(),
                }),
                Vec::new(),
                None,
            ));
        };
        let (mut response, qualified) = self.ask_ranked_over(
            query,
            token_budget,
            min_confidence,
            fold_aware,
            snapshot,
            resolved.as_ref(),
        )?;
        response.scope = resolved.as_ref().map(|resolved| resolved.report.clone());
        Ok((response, qualified, resolved))
    }

    /// [`Self::ask_ranked`] over an already loaded — and, under a scope,
    /// already narrowed — corpus.
    fn ask_ranked_over(
        &self,
        query: &str,
        token_budget: u32,
        min_confidence: f32,
        fold_aware: bool,
        snapshot: devmap_store::SearchPage,
        scope: Option<&crate::scope::ResolvedScope>,
    ) -> anyhow::Result<(Response<SymbolHit>, Vec<String>)> {
        let coverage_gap = search_coverage_gap(analysis_status_gap(snapshot.analysis.as_ref()));
        let symbols = snapshot.rows;
        if symbols.is_empty() || query.trim().is_empty() {
            let mut response = budget_take(Vec::new(), token_budget, |_| 0);
            response.walk_incomplete = coverage_gap;
            return Ok((self.finish(response), Vec::new()));
        }

        let docstrings = crate::ask::docstring_by_qualified_name(&self.store.latest_extractions()?);
        let texts: Vec<String> = symbols
            .iter()
            .map(|symbol| {
                crate::ask::seed_text(
                    &symbol.name,
                    &symbol.qualified_name,
                    docstrings.get(&symbol.qualified_name).map(String::as_str),
                )
            })
            .collect();
        let index = crate::semantic::SemanticIndex::build(&texts, &self.cancel)?;
        let scored = index.score_terms(&crate::ask::question_terms(query), &self.cancel)?;
        if scored.is_empty() {
            let mut response = budget_take(Vec::new(), token_budget, |_| 0);
            response.walk_incomplete =
                devmap_analyze::combine_reasons(coverage_gap, empty_ask_gap(query));
            return Ok((self.finish(response), Vec::new()));
        }

        // Judged on the TF-IDF head, before the graph re-rank can move it: the
        // note is about whether any name answers the question, which the call
        // graph has no say in.
        let coverage_note = index.coverage_note(
            query,
            &scored
                .iter()
                .take(crate::ask::ASK_COVERAGE_HEAD)
                .map(|(position, _)| *position)
                .collect::<Vec<_>>(),
        );
        let seed_positions: Vec<usize> = scored.iter().map(|(position, _)| *position).collect();
        let seed_names: std::collections::HashSet<&str> = seed_positions
            .iter()
            .map(|&position| symbols[position].qualified_name.as_str())
            .collect();

        let Some(edges) = self.generation_edges()? else {
            // Seeds matched, but there is no edge index to re-rank over. Surface
            // the TF-IDF order rather than inventing a graph walk.
            return self.ask_hits_from_seeds(
                &symbols,
                &scored,
                snapshot.repo_root.as_deref(),
                token_budget,
                coverage_gap,
                coverage_note,
                fold_aware,
            );
        };

        // Node set: every seed, plus every endpoint of an admitted Calls edge.
        // Seeds that never appear on an edge still keep their personalization
        // mass; endpoints outside the seed set exist so mass can flow along
        // the graph before we re-rank the seed set alone.
        //
        // Under a scope, only edges with both ends in scope add nodes, so mass
        // cannot leave the scope and come back through code the caller
        // excluded. `call_adjacency` joins by node, so those edges are then
        // absent from the walk too.
        let mut nodes: Vec<String> = seed_positions
            .iter()
            .map(|&position| symbols[position].qualified_name.clone())
            .collect();
        // Membership through a set: the linear scan this replaced was
        // O(nodes × edges) — every admitted edge searched the whole list twice.
        let mut known: std::collections::HashSet<String> = nodes.iter().cloned().collect();
        for id in 0..edges.len() as u32 {
            self.cancel.check_every(id as usize)?;
            if edges.kind(id) != EdgeKind::Calls || !edges.admits(id, min_confidence) {
                continue;
            }
            if scope.is_some_and(|scope| {
                !scope.contains(edges.source_file(id)) || !scope.contains(edges.target_file(id))
            }) {
                continue;
            }
            for name in [edges.source_symbol(id), edges.target_symbol(id)] {
                if !known.contains(name) {
                    known.insert(name.to_string());
                    nodes.push(name.to_string());
                }
            }
        }

        let (outbound, any_call_touched_seed, admitted_call_touched_seed) =
            crate::ask::call_adjacency(&edges, &nodes, &seed_names, min_confidence);

        if any_call_touched_seed && !admitted_call_touched_seed {
            let mut response = budget_take(Vec::new(), token_budget, |_| 0);
            response.walk_incomplete = devmap_analyze::combine_reasons(
                coverage_gap,
                Some(crate::ask::confidence_withheld_reason()),
            );
            return Ok((self.finish(response), Vec::new()));
        }

        let mut personalization = vec![0.0; nodes.len()];
        let mut node_rank: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::with_capacity(nodes.len());
        for (i, name) in nodes.iter().enumerate() {
            node_rank.insert(name.as_str(), i);
        }
        for &(position, score) in &scored {
            let name = symbols[position].qualified_name.as_str();
            if let Some(&i) = node_rank.get(name) {
                personalization[i] = score;
            }
        }

        let ranks = crate::ask::personalized_pagerank(&outbound, &personalization, &self.cancel)?;
        // Each side normalised to the best seed, then blended — see
        // `ASK_LEXICAL_WEIGHT`. A zero maximum means that side separates
        // nothing, and it contributes nothing rather than dividing by zero.
        let graph: Vec<f32> = seed_positions
            .iter()
            .map(|&position| {
                let name = symbols[position].qualified_name.as_str();
                node_rank.get(name).map(|&i| ranks[i]).unwrap_or(0.0)
            })
            .collect();
        let normalise = |value: f32, max: f32| {
            if max > 0.0 && value.is_finite() {
                value / max
            } else {
                0.0
            }
        };
        let max_lexical = scored
            .iter()
            .map(|(_, score)| *score)
            .fold(0.0f32, f32::max);
        let max_graph = graph.iter().copied().fold(0.0f32, f32::max);
        let mut ordered: Vec<(usize, f32)> = scored
            .iter()
            .zip(&graph)
            .map(|(&(position, lexical), &rank)| {
                let blended = crate::ask::ASK_LEXICAL_WEIGHT * normalise(lexical, max_lexical)
                    + (1.0 - crate::ask::ASK_LEXICAL_WEIGHT) * normalise(rank, max_graph);
                (position, blended)
            })
            .collect();
        ordered.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });

        self.ask_hits_from_seeds(
            &symbols,
            &ordered,
            snapshot.repo_root.as_deref(),
            token_budget,
            coverage_gap,
            coverage_note,
            fold_aware,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn ask_hits_from_seeds(
        &self,
        symbols: &[StoredSymbol],
        ordered: &[(usize, f32)],
        repo_root: Option<&str>,
        token_budget: u32,
        coverage_gap: Option<String>,
        extra_gap: Option<String>,
        fold_aware: bool,
    ) -> anyhow::Result<(Response<SymbolHit>, Vec<String>)> {
        let total = u32::try_from(ordered.len()).unwrap_or(u32::MAX);
        let mut hits = Vec::new();
        let mut qualified = Vec::new();
        for &(position, score) in ordered.iter().take(search_page_size(token_budget)) {
            self.cancel.check()?;
            qualified.push(symbols[position].qualified_name.clone());
            hits.push(hit_from_stored(
                symbols[position].clone(),
                repo_root,
                token_budget,
                score,
            ));
        }
        let mut response = if fold_aware {
            crate::evidence::fold_aware_take(hits, token_budget)
        } else {
            budget_take(hits, token_budget, search_hit_tokens)
        };
        response.total = total;
        response.hidden = total.saturating_sub(response.shown);
        response.truncated = response.hidden > 0;
        response.walk_incomplete = devmap_analyze::combine_reasons(coverage_gap, extra_gap);
        // Both takes keep a prefix, so the names stay aligned by truncation.
        qualified.truncate(response.items.len());
        Ok((self.finish(response), qualified))
    }

    /// What the map cost against what reading files would have.
    ///
    /// Sizes come from the files on disk, resolved against the generation's
    /// `repo_root`. Files that cannot be read are counted separately rather
    /// than treated as zero bytes: a corpus figure that silently omits what it
    /// could not open understates the alternative and flatters the map.
    pub fn savings(&self, query: Option<&str>, token_budget: u32) -> anyhow::Result<SavingsReport> {
        let repo_root = self.store.latest_repo_root()?;
        let resolve = |path: &str| -> PathBuf {
            match &repo_root {
                Some(root) if !Path::new(path).is_absolute() => Path::new(root).join(path),
                _ => PathBuf::from(path),
            }
        };
        let size_of =
            |path: &str| -> Option<u64> { std::fs::metadata(resolve(path)).ok().map(|m| m.len()) };

        let indexed_paths = match self.store.latest_generation_id()? {
            Some(gen) => self.store.list_generation_paths(gen)?,
            None => Vec::new(),
        };
        let mut corpus_bytes = 0u64;
        let mut corpus_files_unreadable = 0usize;
        for path in &indexed_paths {
            match size_of(path) {
                Some(bytes) => corpus_bytes = corpus_bytes.saturating_add(bytes),
                None => corpus_files_unreadable += 1,
            }
        }

        let repo_map_bytes = repo_root
            .as_ref()
            .map(devmap_extract::paths::repo_map_path)
            .and_then(|path| std::fs::metadata(path).ok())
            .map(|m| m.len());

        let query_savings = match query {
            None => None,
            Some(text) => {
                let response = self.search(Request {
                    query: text.to_string(),
                    token_budget,
                    min_confidence: 0.0,
                    max_depth: 1,
                })?;
                let mut named: BTreeSet<&str> = BTreeSet::new();
                for hit in &response.items {
                    named.insert(hit.file_path.as_str());
                }
                let mut files_bytes = 0u64;
                let mut files_unreadable = 0usize;
                for path in &named {
                    match size_of(path) {
                        Some(bytes) => files_bytes = files_bytes.saturating_add(bytes),
                        None => files_unreadable += 1,
                    }
                }
                Some(QuerySavings {
                    query: text.to_string(),
                    hits: response.items.len(),
                    answer_tokens: response.tokens_used,
                    files_named: named.len(),
                    files_bytes,
                    files_unreadable,
                })
            }
        };

        Ok(SavingsReport {
            basis: format!("estimated as bytes / {BYTES_PER_TOKEN}; not a tokenizer count"),
            indexed_files: indexed_paths.len(),
            corpus_bytes,
            corpus_files_unreadable,
            repo_map_bytes,
            query: query_savings,
        })
    }

    /// What `new_source` would do to the graph if it were written to `path`.
    ///
    /// Nothing is written and no generation is created. The buffer is extracted
    /// in memory and diffed against the stored extraction for the same path.
    ///
    /// A buffer that fails to parse produces no symbols, and diffing that
    /// against a real file reports every symbol as removed and every caller as
    /// breaking. That output is indistinguishable from a genuine mass deletion
    /// and is the single most likely thing to be produced by a half-typed edit,
    /// which is exactly when an agent would be asking. So a failed parse
    /// returns `delta_available: false` and no symbols at all.
    ///
    /// Requires the `parse` feature. Every other query on this engine answers
    /// from a persisted map; this one has to parse a buffer that was never
    /// indexed, so it is the one surface that genuinely needs the grammars.
    #[cfg(feature = "parse")]
    pub fn preview(
        &self,
        path: &str,
        new_source: &str,
        token_budget: u32,
        min_confidence: f32,
    ) -> anyhow::Result<PreviewReport> {
        if new_source.len() as u64 > devmap_extract::MAX_SOURCE_BYTES {
            anyhow::bail!("preview source exceeds the 1 MiB source ceiling");
        }
        let candidate = devmap_extract::extract_file(path, new_source);
        let parse_status = parse_status_name(&candidate.parse_outcome).to_string();

        let empty_callers = || Response {
            source_freshness: crate::model::SourceFreshness::unverified(
                "whole-tree source freshness was not checked for this answer",
            ),
            items: Vec::new(),
            shown: 0,
            hidden: 0,
            total: 0,
            truncated: false,
            tokens_used: 0,
            resolution: ResolutionAvailability::Available,
            walk_incomplete: None,
            rungs: None,
            dead_clusters: None,
            dead_clusters_truncated: 0,
            dead_clusters_incomplete: None,
            unresolved_namesakes: None,
            scope: None,
        };

        // `Skipped` is here for the same reason `Failed` is, and the reason is
        // the sentence below rather than the variant: a file that yielded no
        // symbols cannot be diffed against one that did without the diff
        // reading as a deletion of every symbol in it. Which of the two
        // produced the empty symbol list does not change that.
        if let ParseOutcome::Failed { reason } | ParseOutcome::Skipped { reason } =
            &candidate.parse_outcome
        {
            return Ok(PreviewReport {
                file_path: path.to_string(),
                parse_status,
                delta_available: false,
                file_is_indexed: self.store.latest_extraction_for_path(path)?.is_some(),
                compared_against: "nothing".to_string(),
                degraded_reason: Some(format!(
                    "the buffer was not parsed ({reason}); no delta is reported, \
                     because an unparsed file yields no symbols and would read \
                     as a deletion of every symbol in it"
                )),
                symbols: Vec::new(),
                bodies_not_compared: 0,
                ambiguous_callers: 0,
                broken_callers: empty_callers(),
            });
        }

        // The "before" side is extracted from the file on disk, not read back
        // from the store. Two reasons: the stored extraction has been through
        // `for_durable_store`, and more importantly the user is editing the
        // file that is on disk — diffing against a generation built from an
        // older commit would report their own already-saved work as part of the
        // candidate change. The store is still consulted, but only for the
        // caller graph, where being a generation behind is a known and stated
        // property rather than a wrong diff.
        let file_is_indexed = self.store.latest_extraction_for_path(path)?.is_some();
        // Resolved against the generation's own root, not the process's working
        // directory. Indexed paths are repository-relative, and the two callers
        // of this have different working directories: the CLI runs wherever the
        // user is, the daemon wherever it was spawned. Reading `path` directly
        // works for one of them and silently finds nothing for the other —
        // which reads as "no such file", i.e. every symbol added.
        //
        // Containment is enforced *before* the read: `path` arrives from an IPC
        // caller, and this is the only query that reads a file the caller
        // names. See `contained_repo_path`.
        let source_root = self.store.latest_repo_root()?;
        contained_repo_path(source_root.as_deref(), path)?;
        // `.ok()` here used to collapse two different facts into one. "There is
        // no such file" and "the file is there and I could not read it"
        // (non-UTF-8, EACCES, EISDIR) both became `None`, and `None` means
        // `compared_against: "nothing"` — documented as *no such file, so every
        // symbol is an addition*.
        //
        // The consequence is the worst shape this repository has: a genuine
        // removal **disappears**. Same edit, same file — readable, the report
        // says `symbols: ["beta:Removed"]`; unreadable, it says
        // `["mod.py:Added", "alpha:Added"]` with `degraded_reason: null` and
        // `delta_available: true`. The caller is handed a clean bill of health
        // by a comparison that never ran.
        let (on_disk, read_failure) = match devmap_extract::safe_fs::read_repo_source(
            source_root.as_deref().map(Path::new),
            path,
            devmap_extract::MAX_SOURCE_BYTES,
        ) {
            Ok(source) => (Some(source), None),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => (None, None),
            Err(err) => (None, Some(err)),
        };
        let compared_against = match (&on_disk, &read_failure) {
            (Some(_), _) => "disk",
            // A third value, never a reuse of `nothing`. A consumer keying off
            // `nothing` to mean "new file" must not be handed this case.
            (None, Some(_)) => "unreadable",
            (None, None) => "nothing",
        };
        let previous = on_disk
            .as_deref()
            .map(|source| devmap_extract::extract_file(path, source).symbols)
            .unwrap_or_default();

        let mut old_by_name: BTreeMap<&str, &ExtractedSymbol> = BTreeMap::new();
        for symbol in &previous {
            old_by_name.insert(symbol.qualified_name.as_str(), symbol);
        }
        let mut new_by_name: BTreeMap<&str, &ExtractedSymbol> = BTreeMap::new();
        for symbol in &candidate.symbols {
            new_by_name.insert(symbol.qualified_name.as_str(), symbol);
        }

        let mut symbols: Vec<PreviewSymbol> = Vec::new();
        let mut bodies_not_compared = 0usize;
        for (qualified, now) in &new_by_name {
            match old_by_name.get(qualified) {
                None => symbols.push(PreviewSymbol {
                    symbol_name: now.name.clone(),
                    qualified_name: now.qualified_name.clone(),
                    kind: now.kind.as_str().to_string(),
                    change: PreviewChange::Added,
                    was: None,
                    now: now.signature.clone(),
                }),
                Some(was) => {
                    // Declaration first: that is what a caller binds to, and a
                    // changed declaration outranks whatever the body did.
                    //
                    // `ExtractedSymbol::signature` is not used for this. It is
                    // populated by only one grammar in this workspace — 80 of
                    // ~2,300 sampled symbols, all Go — so comparing it makes
                    // every Python, Rust and TypeScript signature change look
                    // like a body change. The declaration hash is computed from
                    // the tree and works wherever the grammar names a body.
                    let declaration_moved = match (was.declaration_hash, now.declaration_hash) {
                        (Some(a), Some(b)) => Some(a != b),
                        _ => None,
                    };
                    let change = match declaration_moved {
                        Some(true) => PreviewChange::SignatureChanged,
                        Some(false) | None => match (was.body_signature, now.body_signature) {
                            (Some(a), Some(b)) if a.exact != b.exact => {
                                if declaration_moved.is_none() {
                                    // The body moved and nothing could tell
                                    // whether the declaration did. Reported as
                                    // the caller-affecting case, because
                                    // under-reporting a break is the costlier
                                    // error of the two.
                                    PreviewChange::Changed
                                } else {
                                    PreviewChange::BodyChanged
                                }
                            }
                            (Some(_), Some(_)) => continue,
                            // Bodies not comparable. With an unchanged
                            // declaration there is nothing to report; without
                            // one, nothing was compared at all.
                            _ => {
                                bodies_not_compared += 1;
                                continue;
                            }
                        },
                    };
                    symbols.push(PreviewSymbol {
                        symbol_name: now.name.clone(),
                        qualified_name: now.qualified_name.clone(),
                        kind: now.kind.as_str().to_string(),
                        change,
                        was: was.signature.clone(),
                        now: now.signature.clone(),
                    });
                }
            }
        }
        for (qualified, was) in &old_by_name {
            if new_by_name.contains_key(qualified) {
                continue;
            }
            symbols.push(PreviewSymbol {
                symbol_name: was.name.clone(),
                qualified_name: was.qualified_name.clone(),
                kind: was.kind.as_str().to_string(),
                change: PreviewChange::Removed,
                was: was.signature.clone(),
                now: None,
            });
        }
        symbols.sort_by(|a, b| {
            (a.change as u8, &a.qualified_name).cmp(&(b.change as u8, &b.qualified_name))
        });

        // Only removals and re-declarations can break a caller. A body change
        // moves behaviour without touching the call site, and listing its
        // callers would bury the cases that actually stop compiling.
        let at_risk: Vec<String> = symbols
            .iter()
            .filter(|s| {
                matches!(
                    s.change,
                    PreviewChange::Removed
                        | PreviewChange::SignatureChanged
                        | PreviewChange::Changed
                )
            })
            // Qualified, not bare. Every `target_symbol` in `generation_edges`
            // is `path::Name` — matching on the bare name finds nothing at all,
            // and "no calls are affected" is a perfectly plausible-looking way
            // for this feature to do nothing.
            .map(|s| s.qualified_name.clone())
            .collect();
        // One snapshot for the list and the denominator it is measured
        // against. Read separately they could describe two generations, and the
        // `saturating_sub` below turns that into silence: when the newer
        // generation holds fewer callers the difference clamps to zero and this
        // reports that the confidence floor hid nothing. Drawn from one
        // generation the floored set is a subset of the unfiltered one, so the
        // subtraction cannot underflow at all.
        let page = self.store.callers_page(&at_risk, path, min_confidence)?;
        let (caller_edges, total_unfiltered) = match page {
            Some(page) => (page.callers, page.total_unfiltered),
            None => (Vec::new(), 0),
        };
        let callers: Vec<PreviewCaller> = caller_edges
            .into_iter()
            .map(|edge| PreviewCaller {
                target_symbol: edge.target_symbol,
                caller_file: edge.source_file,
                caller_symbol: edge.source_symbol,
                confidence: edge.confidence,
            })
            .collect();
        // What the floor excluded. Reported rather than dropped: a symbol with
        // no confident callers and 900 ambiguous ones is not the same situation
        // as one nothing references, and the difference decides whether a
        // reader should go and look.
        // Counted, not built. This asked for every caller edge at floor 0.0 —
        // six `String` allocations per row — solely to take `.len()`. On this
        // repository the busiest symbol has 918 callers, so previewing a file
        // that declares one materialised ~1,836 rows and kept none of them.
        let ambiguous_callers = total_unfiltered.saturating_sub(callers.len());

        let degraded_reason = match &candidate.parse_outcome {
            ParseOutcome::Partial { .. } => Some(
                "the buffer parsed with errors; a symbol inside an error region \
                 is invisible to extraction and will appear here as removed"
                    .to_string(),
            ),
            ParseOutcome::Fallback { .. } => Some(
                "the buffer's language has no linked grammar, so declarations \
                 were recovered by pattern and carry no bodies; body changes \
                 cannot be detected"
                    .to_string(),
            ),
            _ => None,
        };

        // A read that failed outranks a parse note: without the previous
        // content there is no comparison at all, so the delta is not merely
        // "to be read with care", it is absent. `delta_available: false` is the
        // gate the model documents for exactly this, and stating the errno is
        // what lets a caller tell a permissions problem from a binary file.
        let (delta_available, degraded_reason) = match read_failure {
            Some(err) => (
                false,
                Some(format!(
                    "the file on disk could not be read ({err}), so the buffer was compared \
against nothing and no symbol can be reported as removed; this is not a clean delta"
                )),
            ),
            None => (true, degraded_reason),
        };

        Ok(PreviewReport {
            file_path: path.to_string(),
            parse_status,
            delta_available,
            file_is_indexed,
            compared_against: compared_against.to_string(),
            degraded_reason,
            symbols,
            bodies_not_compared,
            ambiguous_callers,
            broken_callers: budget_take(callers, token_budget, |_| PREVIEW_CALLER_TOKENS),
        })
    }
}

#[cfg(test)]
#[path = "engine/tests/specifier_tests.rs"]
mod specifier_tests;

#[cfg(test)]
#[path = "engine/tests/span_line_range_tests.rs"]
mod span_line_range_tests;

/// Token cost of one caller line: two paths and a symbol name.
#[cfg(feature = "parse")]
const PREVIEW_CALLER_TOKENS: u32 = 25;

/// Confidence a call edge needs before `preview` will call it a caller.
///
/// The resolver publishes three tiers on this workspace's 50,533 call edges:
/// 1.0 (19,134 edges), 0.9 (4,800) and 0.2 (26,599). The 0.2 tier is
/// name-only attribution, and on a common method name it is not close to
/// right: every `dict.get(...)` in the tree resolves to
/// `LLMCache.get`, giving that one method 921 edges — all of them at 0.2, none
/// of them real. Listing those under "this change breaks" would send a reader
/// to `benchmarks/map_bench.py` to fix a call it does not contain.
///
/// 0.5 sits in the empty band between 0.2 and 0.9, so it separates the tiers
/// rather than cutting through one. Edges below it are counted, not discarded —
/// see `PreviewReport::ambiguous_callers`.
pub const PREVIEW_CALLER_MIN_CONFIDENCE: f32 = 0.5;

/// Describe a parse outcome in one word, for a report a human reads.
#[cfg(feature = "parse")]
fn parse_status_name(outcome: &ParseOutcome) -> &'static str {
    match outcome {
        ParseOutcome::Clean => "clean",
        ParseOutcome::Partial { .. } => "partial",
        ParseOutcome::Fallback { .. } => "fallback",
        ParseOutcome::Failed { .. } => "failed",
        // Not "failed". This word is what a human reads next to the file, and
        // the whole point of the outcome is that nothing went wrong here.
        ParseOutcome::Skipped { .. } => "skipped",
    }
}

/// Parse a clone kind from a caller-supplied string.
///
/// `None` for an unrecognised name rather than a default, so a caller can
/// reject a typo instead of answering it with an unfiltered report.
pub fn parse_clone_kind(name: &str) -> Option<devmap_analyze::CloneKind> {
    match name {
        "exact" => Some(devmap_analyze::CloneKind::Exact),
        "structural" => Some(devmap_analyze::CloneKind::Structural),
        _ => None,
    }
}

/// Token cost of one clone group: a header line plus one line per member.
///
/// Public because a caller that filters groups has to re-take the budget over
/// what survives, and it must charge the same rate this engine did — two copies
/// of the arithmetic is how a response comes to report a token count it did not
/// spend.
pub fn clone_group_tokens(group: &devmap_analyze::CloneGroup) -> u32 {
    /// A group's header line.
    const HEADER_TOKENS: u32 = 12;
    /// One member line: a path, a symbol name, and a span.
    const MEMBER_TOKENS: u32 = 25;
    HEADER_TOKENS + group.members.len() as u32 * MEMBER_TOKENS
}

fn edge_node_matches(file: &str, symbol: &str, query: &str) -> bool {
    crate::query_match::traversal_start_matches(query, symbol, file)
}

/// Stable name of an edge kind, for tie-breaking.
///
/// The comparator used `format!("{:?}", kind)`, which allocated two `String`s
/// per comparison inside a sort over every edge in the generation. These are
/// the same names `Debug` derives, so the ordering is unchanged and no
/// allocation happens.
fn edge_kind_name(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Imports => "Imports",
        EdgeKind::Calls => "Calls",
        EdgeKind::Contains => "Contains",
        EdgeKind::Defines => "Defines",
        EdgeKind::Instantiates => "Instantiates",
        EdgeKind::Extends => "Extends",
        EdgeKind::Implements => "Implements",
        EdgeKind::SubscribesTo => "SubscribesTo",
        EdgeKind::HandlesRoute => "HandlesRoute",
        EdgeKind::Registers => "Registers",
        EdgeKind::WiredTo => "WiredTo",
        EdgeKind::MemberOf => "MemberOf",
        EdgeKind::DependsOn => "DependsOn",
        EdgeKind::TaintFlow => "TaintFlow",
        EdgeKind::References => "References",
    }
}

/// A caller-supplied path that does not resolve inside the indexed repository.
///
/// A distinct type rather than a bare `anyhow!` so the IPC layer can answer it
/// as a rejected *parameter* instead of an internal failure: the caller asked
/// for something it is not allowed to ask for, and "the request was invalid" is
/// a different fact from "the query broke".
#[derive(Debug)]
pub struct PathOutsideRepoRoot {
    /// The path exactly as the caller supplied it.
    pub requested: String,
    /// Why it was refused.
    pub reason: String,
}

impl std::fmt::Display for PathOutsideRepoRoot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:?} {}", self.requested, self.reason)
    }
}

impl std::error::Error for PathOutsideRepoRoot {}

/// Resolve a caller-supplied path against the indexed repository root, or
/// refuse it.
///
/// `preview` is the one query that reads a file the *caller* names, and the
/// name arrives over IPC from any process that can reach the socket. Joined
/// onto the root unchecked, `../../etc/passwd` reads outside the repository;
/// used verbatim when absolute, any path at all does. What came back was not a
/// diff but a listing of every symbol and signature in the target file.
///
/// The rule is `daemon.rs::collect_pending_path`'s, which already guards
/// watcher paths, applied to the same question:
///
/// 1. A `..` component is refused outright — a repository-relative path never
///    needs one, and normalising it away would silently accept the escape.
/// 2. An absolute path is accepted only when it lies under the root, and is
///    then treated as the relative path it denotes. This is the daemon's own
///    convention for watcher paths, so refusing it outright would split the
///    two surfaces.
/// 3. The resolved candidate must be lexically under the root, and — when it
///    exists — its *canonical* form must be too, which is what catches a
///    symlink whose every component sits inside the repository.
///
/// With no recorded root (a pre-v7 generation, or an in-memory store) there is
/// nothing to contain against, so an absolute path cannot be shown to be
/// inside the repository and is refused. A relative path is resolved against
/// the process's working directory exactly as before, which rule 1 keeps from
/// climbing out of it.
#[cfg(feature = "parse")]
pub(crate) fn contained_repo_path(
    repo_root: Option<&str>,
    path: &str,
) -> Result<PathBuf, PathOutsideRepoRoot> {
    let refuse = |reason: &str| PathOutsideRepoRoot {
        requested: path.to_string(),
        reason: reason.to_string(),
    };
    if path.is_empty() {
        return Err(refuse("is empty"));
    }
    let raw = Path::new(path);
    if raw
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(refuse("contains a parent traversal component"));
    }

    let Some(root) = repo_root else {
        if raw.is_absolute() {
            return Err(refuse(
                "is absolute, and this generation records no repository root to \
                 contain it within",
            ));
        }
        return Ok(PathBuf::from(path));
    };
    let root = Path::new(root);

    let candidate = if raw.is_absolute() {
        match raw.strip_prefix(root) {
            Ok(relative) => root.join(relative),
            Err(_) => return Err(refuse("is outside the indexed repository root")),
        }
    } else {
        root.join(raw)
    };
    if !candidate.starts_with(root) {
        return Err(refuse("is outside the indexed repository root"));
    }

    // Only a path that exists can be canonicalized, and a preview of a file
    // that does not exist yet is a legitimate request — it is how a new file is
    // previewed. Rules 1 and 3 already bound where a non-existent path could
    // point.
    if candidate.exists() {
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        match candidate.canonicalize() {
            Ok(canonical) if !canonical.starts_with(&canonical_root) => {
                return Err(refuse("resolves outside the indexed repository root"));
            }
            Ok(_) => {}
            Err(_) => return Err(refuse("could not be resolved for containment checking")),
        }
    }
    Ok(candidate)
}

pub fn resolved_edge_from_stored(edge: StoredEdge) -> anyhow::Result<ResolvedEdge> {
    stored_edge_to_resolved(edge)
}

/// The latest generation's graph core — nodes, edges and route nodes — read
/// from the store, for the views that walk the whole graph: `routes`,
/// `shape-check`, `api-impact` and `cypher`.
///
/// The artifact's `nodes` and `edges` and none of its panels — no `git log`,
/// no intel, no dead-code list, no freshness; `build_graph_core_value` says
/// what building the whole artifact cost these views. One owner, so the CLI
/// and the MCP server answer those questions from the same graph. Every edge
/// is read (`min_confidence` 0): the views report each edge's own confidence
/// rather than pre-filtering it away.
pub fn graph_core_for_store(store: &Store) -> anyhow::Result<serde_json::Value> {
    store
        .latest_generation_id()?
        .ok_or_else(|| anyhow::anyhow!("no committed generation: run `devmap build` first"))?;
    let extractions = store.latest_extractions()?;
    let analysis = store
        .latest_analysis()?
        .ok_or_else(|| anyhow::anyhow!("no committed generation: run `devmap build` first"))?;
    let edges = store
        .latest_edges(0.0)?
        .into_iter()
        .map(resolved_edge_from_stored)
        .collect::<anyhow::Result<Vec<_>>>()?;
    let repo_root = store.latest_repo_root()?;
    Ok(crate::build_graph_core_value(
        &extractions,
        &analysis,
        &edges,
        repo_root.as_deref(),
    ))
}

fn stored_edge_to_resolved(edge: StoredEdge) -> anyhow::Result<ResolvedEdge> {
    // The kind table lives with the rows it decodes, in `devmap-store`: the
    // stored spelling is that crate's `format!("{kind:?}")` on the way in, and
    // a second table here could disagree with the one the edge index uses.
    let edge_kind = devmap_store::edge_kind_from_stored(&edge.edge_kind)?;
    // The evidence tier the row carries, decoded by the same owner the edge
    // index uses. A row written before the column existed comes back as a
    // reconstruction and says so; nothing here rounds it up to a reading.
    let evidence = devmap_store::edge_resolution(&edge)?;
    Ok(ResolvedEdge {
        source_file: edge.source_file,
        target_file: edge.target_file,
        source_symbol: edge.source_symbol,
        target_symbol: edge.target_symbol,
        edge_kind,
        confidence: Confidence(edge.confidence),
        // The payload is not persisted — an `ImportScoped` row does not carry
        // `imported_from` — so the variant is not rebuilt: a `Resolution`
        // invented to fill it would be a guess wearing the resolver's type.
        // The kind and its provenance travel in `evidence` instead.
        resolution: None,
        evidence: Some(evidence),
        details: None,
    })
}

#[cfg(test)]
#[path = "engine/tests/attribution_disclosure_tests.rs"]
mod attribution_disclosure_tests;

#[cfg(all(test, feature = "parse"))]
#[path = "engine/tests/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "engine/tests/indexed_start_equivalence_tests.rs"]
mod indexed_start_equivalence_tests;

// Needs the parsing frontend: every case here builds a real generation from
// source. Without `parse` the crate answers questions about a persisted map and
// cannot make one, so these are compiled out rather than left to break the
// `--no-default-features` build.
#[cfg(all(test, feature = "parse"))]
#[path = "engine/tests/search_bounds_tests.rs"]
mod search_bounds_tests;

#[cfg(all(test, feature = "parse"))]
#[path = "engine/tests/composition_cancellation_tests.rs"]
mod composition_cancellation_tests;
