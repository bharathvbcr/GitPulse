use devmap_analyze::clones::group_clones;
use devmap_analyze::traversal::{
    traverse_graph_indexed, AdjacencyIndex, TraversalLimits, TraversalOptions, TraversalStop,
};
use devmap_extract::model::*;
use devmap_resolve::model::*;
use devmap_store::{GenerationEdges, Store, StoredEdge, StoredSymbol};

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::cancel::{Cancel, QueryCancelled};
use crate::model::*;

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
    coverage_gap: Option<String>,
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

    pub fn search(&self, req: Request<String>) -> anyhow::Result<Response<SymbolHit>> {
        if req.query.trim().is_empty() {
            return Ok(budget_take(Vec::new(), req.token_budget, |_| 0));
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
        let Some(snapshot) = self.store.search_page(&req.query, pool)? else {
            return Ok(unavailable_response(ResolutionAvailability::Unavailable {
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
        // Two independent qualifications, composed rather than ranked — the
        // same shape `dependencies` and the traversals use. One is about the
        // *ordering* of what was found; the other is about whether the corpus
        // searched was the whole repository, and it is the one that decides
        // whether `total: 0` may be read as "no such symbol".
        response.walk_incomplete =
            devmap_analyze::combine_reasons(ranked_over_a_sample, coverage_gap);
        Ok(response)
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
            return Ok(unavailable_response(ResolutionAvailability::Unavailable {
                reason: format!("{} is not indexed", req.query),
            }));
        };
        if matches!(snapshot.file.parse_outcome, ParseOutcome::Failed { .. }) {
            return Ok(unavailable_response(ResolutionAvailability::Unavailable {
                reason: format!("{} could not be parsed", req.query),
            }));
        }
        let coverage_gap = devmap_analyze::combine_reasons(
            file_edge_coverage_gap(&snapshot.file.parse_outcome),
            analysis_coverage_gap(snapshot.analysis.as_ref()),
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
        Ok(response)
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
        let Some(index) = self.generation_edges()? else {
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
        let (edges, bands) = self.traverse_walked(
            &index,
            &direction,
            Request {
                token_budget: edge_budget,
                ..req
            },
            None,
            Some(layer_budget),
        )?;
        Ok(LayeredImpact {
            edges,
            // `Some` by construction: `band_budget` was `Some` on the call
            // above, and every return path of `traverse_walked` maps it.
            blast_radius: bands.ok_or_else(|| {
                anyhow::anyhow!("a banded traversal returned no bands; this is a bug in the kernel")
            })?,
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
            return Ok(unavailable_response(ResolutionAvailability::Unavailable {
                reason: "no persisted generation is available".to_string(),
            }));
        };
        let coverage_gap = analysis_coverage_gap(index.analysis());
        let unavailable = |reason: String| {
            let mut response = unavailable_response(ResolutionAvailability::Unavailable { reason });
            response.walk_incomplete = coverage_gap.clone();
            response
        };
        let (from, to) = req.query;
        let from = from.trim();
        let to = to.trim();
        if from.is_empty() || to.is_empty() {
            return Ok(unavailable_response(ResolutionAvailability::Unavailable {
                reason: "scoped trace endpoints must not be empty".to_string(),
            }));
        }
        // `trace X X` walked the graph from `X` looking for `X`, never counted
        // the start as reached, and reported the walk's budget — "stopped at
        // depth 3 after visiting 44 nodes; whether a path exists is unknown" —
        // for a question the walk cannot answer. Whether a cycle passes through
        // a symbol is `impact`'s question; a scoped trace needs two endpoints.
        if from == to {
            return Ok(unavailable_response(ResolutionAvailability::Unavailable {
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
        Ok(response)
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
        let Some(index) = self.generation_edges()? else {
            return Ok(unavailable_response(ResolutionAvailability::Unavailable {
                reason: "no persisted generation is available".to_string(),
            }));
        };
        let direction = index.directed(reverse, req.min_confidence);
        self.traverse_over(&index, &direction, req, min_rung)
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
        let coverage_gap = analysis_coverage_gap(index.analysis());
        let start: Vec<String> =
            indexed_traversal_starts(index, target, reverse, min_confidence, &self.cancel)?
                .into_iter()
                .map(|(symbol, _)| symbol)
                .collect();
        if start.is_empty() {
            let reason = format!("{target} has no indexed traversal start");
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
        // Also before the budget, and for the same reason: the bands describe
        // the population the walk reached, not the slice that fitted. They are
        // built from `traversed` rather than from `walk.traversed_edges` so that
        // every banded node is an endpoint of an edge this answer measured —
        // the two halves partition one set, and a node can appear in one and not
        // the other only if the budgeter trimmed it, which the budgeter counts.
        let incomplete =
            devmap_analyze::combine_reasons(walk.stop.reason(max_depth, max_nodes), coverage_gap);
        let bands = band_budget.map(|budget| {
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
        response.walk_incomplete = incomplete;
        Ok((response, bands))
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
        self.read_composed(
            || self.explore_once(query, limit, token_budget, min_confidence, max_depth),
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
        };
        if query.trim().is_empty() {
            return Ok(empty("explore requires a non-empty query".to_string()));
        }

        let page = budget_page_size(budget.definitions);
        let pool = search_rank_pool_size(budget.definitions);
        let Some(snapshot) = self.store.search_page(query, pool)? else {
            return Ok(empty("no persisted generation is available".to_string()));
        };
        let total = snapshot.total;
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
            record_test_hit(&mut nearest, symbol, file, 0);
        }
        for band in &walk.bands {
            for (symbol, file) in &band.members {
                record_test_hit(&mut nearest, symbol, file, band.depth);
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
            coverage_gap: analysis_coverage_gap(index.analysis()),
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
            return Ok(unavailable_response(ResolutionAvailability::Unavailable {
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
        Ok(response)
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
        self.cancel.check()?;
        let Some(snapshot) = self.store.all_symbols_page()? else {
            return Ok(unavailable_response(ResolutionAvailability::Unavailable {
                reason: "no persisted generation is available".to_string(),
            }));
        };
        let coverage_gap = search_coverage_gap(analysis_status_gap(snapshot.analysis.as_ref()));
        let symbols = snapshot.rows;
        if symbols.is_empty() || query.trim().is_empty() {
            let mut response = budget_take(Vec::new(), token_budget, |_| 0);
            response.walk_incomplete = coverage_gap;
            return Ok(response);
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
        // Materialise only as far down the ranking as the budget could reach.
        // Every scored symbol used to be turned into a `SymbolHit` first — one
        // `read_to_string` each — and budgeted afterwards, so a query matching
        // a common term opened every file it matched in order to discard almost
        // all of them. The ranking is already sorted, so the page bound is the
        // same one keyword search uses.
        let mut hits = Vec::new();
        for (position, score) in scored.into_iter().take(budget_page_size(token_budget)) {
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
        response.walk_incomplete = coverage_gap;
        Ok(response)
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
            source_freshness: None,
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
        let resolved = contained_repo_path(self.store.latest_repo_root()?.as_deref(), path)?;
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
        let (on_disk, read_failure) = match devmap_extract::read_source(&resolved) {
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

/// Search every repository in a workspace, labelling each hit with its origin.
///
/// Repositories are queried in registry order and the budget is spent across
/// the union, so a large first repository can exhaust it before a later one is
/// reached. That is reported — `truncated` and `hidden` cover the whole
/// workspace, not one repository — rather than papered over by giving each
/// repository an equal slice, which would silently drop the best matches in a
/// large repository to make room for weak ones in a small one.
pub fn workspace_search(
    workspace: &crate::workspace::Workspace,
    query: &str,
    token_budget: u32,
    semantic: bool,
) -> anyhow::Result<crate::workspace::FederatedSearch> {
    use crate::workspace::{FederatedHit, FederatedSearch, RepoUnavailable};

    let mut all: Vec<FederatedHit> = Vec::new();
    let mut unavailable: Vec<RepoUnavailable> = Vec::new();
    let mut queried = 0usize;
    // Matches across the workspace *before* any budget was applied. Counting
    // the union of the returned items instead — which this did — counts what
    // each repository could afford to send, and every repository has already
    // spent its budget by then. A repository with 500 matches that fitted four
    // contributed four, and the federated answer called that the total and set
    // `truncated: false`.
    let mut matched_total = 0u32;

    for repo in &workspace.repos {
        let db = repo.db_path();
        let store = match devmap_store::Store::open_existing(&db) {
            Ok(Some(store)) => store,
            Ok(None) => {
                unavailable.push(RepoUnavailable {
                    repo: repo.name.clone(),
                    reason: format!("no store at {}", db.display()),
                });
                continue;
            }
            Err(error) => {
                unavailable.push(RepoUnavailable {
                    repo: repo.name.clone(),
                    reason: format!("store at {} could not be opened: {error}", db.display()),
                });
                continue;
            }
        };
        let engine = StoreQueryEngine::new(&store);
        // Each repository is asked for the *whole* budget's worth of hits; the
        // union is trimmed once at the end. Asking each for a slice would rank
        // within repositories instead of across them.
        let response = if semantic {
            engine.search_semantic(query, token_budget)?
        } else {
            engine.search(Request {
                query: query.to_string(),
                token_budget,
                min_confidence: 0.0,
                max_depth: 1,
            })?
        };
        if let ResolutionAvailability::Unavailable { reason } = &response.resolution {
            unavailable.push(RepoUnavailable {
                repo: repo.name.clone(),
                reason: reason.clone(),
            });
            continue;
        }
        queried += 1;
        matched_total = matched_total.saturating_add(response.total);
        for hit in response.items {
            all.push(FederatedHit {
                repo: repo.name.clone(),
                hit,
            });
        }
    }

    // One ranking across the workspace. Ties break on repository then path so
    // the order is total and identical on every run.
    all.sort_by(|a, b| {
        b.hit
            .score
            .total_cmp(&a.hit.score)
            .then_with(|| a.repo.cmp(&b.repo))
            .then_with(|| a.hit.file_path.cmp(&b.hit.file_path))
            .then_with(|| a.hit.symbol_name.cmp(&b.hit.symbol_name))
    });

    let budgeted = budget_take(all, token_budget, |entry| search_hit_tokens(&entry.hit));
    // `matched_total` counts every match each repository found, so it is never
    // below what was shown; the saturating subtraction is belt-and-braces
    // against a store that miscounts rather than a state this can reach.
    let hidden = matched_total.saturating_sub(budgeted.shown);
    Ok(FederatedSearch {
        items: budgeted.items,
        repos_queried: queried,
        unavailable,
        total: matched_total,
        shown: budgeted.shown,
        hidden,
        truncated: hidden > 0,
    })
}

/// Modules each repository *provides*, as `(specifier prefix, evidence)`.
///
/// Two sources, both declarations rather than inferences:
///
/// - Go: every `module` line in every `go.mod`. `import "manvi/dc/store"`
///   resolving to the repository whose `go.mod` says `module manvi` is not a
///   guess, it is how the toolchain resolves it.
/// - Python and JavaScript: top-level package directories — a directory
///   directly under the root containing `__init__.py`, or a `package.json`
///   `name`. Weaker than Go's, and labelled as the directory it came from.
///
/// Deliberately not included: matching on symbol names. Two repositories both
/// declaring `Client` is not a link, and asserting one would produce edges at a
/// rate that buries the real ones.
fn provided_modules(root: &std::path::Path) -> Vec<(String, String)> {
    let mut provided: Vec<(String, String)> = Vec::new();

    if let Ok(modules) = devmap_extract::collect_go_modules(root) {
        for module in modules {
            if module.prefix.is_empty() {
                continue;
            }
            let where_from = if module.dir.is_empty() {
                "go.mod".to_string()
            } else {
                format!("{}/go.mod", module.dir)
            };
            provided.push((
                module.prefix.clone(),
                format!("{where_from} declares `module {}`", module.prefix),
            ));
        }
    }

    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }
            if entry.path().join("__init__.py").is_file() {
                provided.push((
                    name.clone(),
                    format!("{name}/__init__.py declares a Python package"),
                ));
            }
        }
    }
    // A `src/` layout puts the package one level down, which is where this
    // repository's own `devcouncil` package lives.
    if let Ok(entries) = std::fs::read_dir(root.join("src")) {
        for entry in entries.flatten() {
            if entry.path().join("__init__.py").is_file() {
                let name = entry.file_name().to_string_lossy().into_owned();
                provided.push((
                    name.clone(),
                    format!("src/{name}/__init__.py declares a Python package"),
                ));
            }
        }
    }

    provided.sort();
    provided.dedup();
    provided
}

/// Whether `specifier` is satisfied by a module named `prefix`.
///
/// Exact, or a path segment beneath it. `manvi/dc/store` is provided by
/// `manvi`; `manvibench` is not, and matching on a bare `starts_with` would
/// claim it is.
fn specifier_matches(specifier: &str, prefix: &str) -> bool {
    if specifier == prefix {
        return true;
    }
    specifier
        .strip_prefix(prefix)
        .is_some_and(|rest| rest.starts_with('/') || rest.starts_with('.'))
}

/// Imports in one repository that another repository declares the module for.
///
/// Reported as *candidates*. A matching module path is strong evidence — for Go
/// it is how the compiler resolves the import — but this does not verify that
/// the imported symbol exists in the target, and it cannot tell a local
/// checkout from a published copy at a different version. Calling these
/// resolved edges would put an unverified claim in the graph beside verified
/// ones.
pub fn link_candidates(
    workspace: &crate::workspace::Workspace,
) -> anyhow::Result<Vec<crate::workspace::LinkCandidate>> {
    use crate::workspace::LinkCandidate;

    // What each repository provides.
    let mut providers: Vec<(&str, Vec<(String, String)>)> = Vec::new();
    for repo in &workspace.repos {
        providers.push((repo.name.as_str(), provided_modules(&repo.root)));
    }

    let mut candidates: Vec<LinkCandidate> = Vec::new();
    for repo in &workspace.repos {
        let Ok(Some(store)) = devmap_store::Store::open_existing(repo.db_path()) else {
            continue;
        };
        let extractions = store.latest_extractions()?;
        for extraction in &extractions {
            for import in &extraction.imports {
                let specifier = import.module_specifier.trim();
                if specifier.is_empty() || specifier.starts_with('.') {
                    continue;
                }
                for (provider_name, provided) in &providers {
                    // A repository importing its own module is not a
                    // cross-repository link.
                    if *provider_name == repo.name {
                        continue;
                    }
                    for (prefix, evidence) in provided {
                        if specifier_matches(specifier, prefix) {
                            candidates.push(LinkCandidate {
                                from_repo: repo.name.clone(),
                                from_file: extraction.file_path.clone(),
                                module_specifier: specifier.to_string(),
                                to_repo: (*provider_name).to_string(),
                                evidence: evidence.clone(),
                            });
                        }
                    }
                }
            }
        }
    }
    candidates.sort_by(|a, b| {
        (&a.from_repo, &a.from_file, &a.module_specifier, &a.to_repo).cmp(&(
            &b.from_repo,
            &b.from_file,
            &b.module_specifier,
            &b.to_repo,
        ))
    });
    candidates.dedup_by(|a, b| {
        a.from_repo == b.from_repo
            && a.from_file == b.from_file
            && a.module_specifier == b.module_specifier
            && a.to_repo == b.to_repo
    });
    Ok(candidates)
}

#[cfg(test)]
mod specifier_tests {
    use super::specifier_matches;

    /// A module prefix matches its own path and anything beneath it — and
    /// nothing that merely starts with the same letters. `manvibench` sharing a
    /// prefix with `manvi` is not an import of it, and a bare `starts_with`
    /// would claim it is.
    #[test]
    fn a_prefix_matches_only_on_a_segment_boundary() {
        assert!(specifier_matches("example.com/libb", "example.com/libb"));
        assert!(specifier_matches(
            "example.com/libb/store",
            "example.com/libb"
        ));
        assert!(specifier_matches("devcouncil.app.config", "devcouncil"));

        assert!(!specifier_matches(
            "example.com/libbeta",
            "example.com/libb"
        ));
        assert!(!specifier_matches("manvibench", "manvi"));
        assert!(!specifier_matches("libb", "example.com/libb"));
    }
}

#[cfg(test)]
mod span_line_range_tests {
    use super::byte_span_to_line_range;
    use devmap_extract::model::Span;

    /// Multi-byte source must not abort the process.
    ///
    /// This function counted newlines with `source[..start]` — slicing a `&str`
    /// at an index that is not a character boundary, which panics. Spans are
    /// byte offsets recorded at extraction time while the source is re-read
    /// from disk when the graph is exported, so an offset lands mid-character
    /// whenever a multi-byte character was inserted before it. One emoji added
    /// to a file aborted `dev map manifest`, and the release profile is
    /// `panic = "abort"`, so nothing recovered.
    ///
    /// Exhaustive over every offset pair rather than sampled: the failure is
    /// per-offset, and testing only the boundaries would pass against exactly
    /// the code that panicked, because boundaries were always the safe case.
    #[test]
    fn every_offset_into_multibyte_source_is_answered_rather_than_panicked_on() {
        let source = "fn a() {}\n// \u{1F980} ferris r\u{e9}\nfn b() {}\n";
        for start in 0..=source.len() {
            for end in 0..=source.len() {
                let span = Span {
                    start_byte: start,
                    end_byte: end,
                };
                let (first, last) = byte_span_to_line_range(source, &span);
                assert!(first >= 1, "lines are one-based, got {first}");
                assert!(
                    last >= first,
                    "end line {last} precedes start line {first} for {start}..{end}"
                );
            }
        }
    }

    /// The line numbers must be right, not merely non-panicking.
    ///
    /// A fix that clamped every offset to zero would satisfy the test above.
    #[test]
    fn line_numbers_are_correct_across_a_multibyte_character() {
        let source = "alpha\n\u{1F980}beta\ngamma\n";
        let crab = source
            .find('\u{1F980}')
            .expect("fixture contains the emoji");

        let at_emoji = Span {
            start_byte: crab,
            end_byte: crab,
        };
        assert_eq!(byte_span_to_line_range(source, &at_emoji), (2, 2));

        // Strictly inside the four-byte emoji — the exact index that panicked.
        let inside = Span {
            start_byte: crab + 1,
            end_byte: crab + 2,
        };
        assert_eq!(byte_span_to_line_range(source, &inside), (2, 2));

        let whole = Span {
            start_byte: 0,
            end_byte: source.len(),
        };
        assert_eq!(byte_span_to_line_range(source, &whole), (1, 4));
    }

    /// A stored span outliving the file it points into is the everyday case
    /// after an edit, not a hostile one.
    #[test]
    fn offsets_beyond_the_source_are_clamped() {
        let source = "one\ntwo\n";
        let span = Span {
            start_byte: 10_000,
            end_byte: 20_000,
        };
        assert_eq!(byte_span_to_line_range(source, &span), (3, 3));
    }
}

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
        EdgeKind::WiredTo => "WiredTo",
        EdgeKind::MemberOf => "MemberOf",
        EdgeKind::DependsOn => "DependsOn",
        EdgeKind::TaintFlow => "TaintFlow",
        EdgeKind::References => "References",
    }
}

/// One node reached by the scoped-trace walk, and the edge that reached it.
///
/// The walk carries a parent pointer rather than a copy of the path so far.
/// Cloning the whole path once per *edge considered* made the walk quadratic in
/// its own output on top of being quadratic in the graph.
struct Reached {
    /// Index into the confidence-ordered edge list.
    edge: usize,
    /// The entry this one extends, or `None` for a first hop.
    parent: Option<usize>,
    node: (String, String),
    /// Number of edges from the origin, i.e. the length of the path to here.
    depth: usize,
}

/// Walk back from `entry` to the origin, producing the path in forward order.
fn path_to(reached: &[Reached], ordered: &[&ResolvedEdge], entry: usize) -> Vec<ResolvedEdge> {
    let mut path = Vec::with_capacity(reached[entry].depth);
    let mut cursor = Some(entry);
    while let Some(index) = cursor {
        path.push(ordered[reached[index].edge].clone());
        cursor = reached[index].parent;
    }
    path.reverse();
    path
}

/// One deterministic shortest path from `from` to `to`, breadth-first.
///
/// The graph is indexed by source node once, up front. The previous
/// implementation filtered the *entire* edge list for every node it dequeued,
/// so a trace across a repository-sized generation cost `frontier × edges` —
/// with the frontier bounded at 5,000 and generations running to tens of
/// thousands of edges, that is ~10^8 string comparisons per request, each one
/// also cloning the path built so far. Indexed, the walk touches each edge at
/// most once and the whole call is `O(E log E)`, dominated by the ordering
/// sort.
///
/// Why a scoped trace ended, so a caller can tell an answer from a decline.
///
/// A bare `Option<Vec<_>>` made four outcomes one value: a zero budget, a
/// frontier pruned at `max_depth`, the node cap stopping the walk, and the
/// reachable set genuinely not containing the target. Only the last of those
/// licenses the sentence `trace_between` was printing — "no indexed path from
/// X to Y" — and an agent that reads it concludes two symbols are unrelated.
#[derive(Debug)]
pub(crate) enum PathSearch {
    /// The target was reached; these are the edges, source-first.
    Found(Vec<ResolvedEdge>),
    /// The reachable set from `from` was explored to exhaustion without
    /// reaching `to`. This is the only outcome that is a fact about the graph.
    NoPath,
    /// A limit stopped the walk before it could answer. `depth_capped` means a
    /// node with unexplored successors sat at `max_depth`; `node_capped` means
    /// the frontier hit `max_nodes`. Both can be true.
    Exhausted {
        depth_capped: bool,
        node_capped: bool,
        visited: usize,
        max_depth: usize,
        max_nodes: usize,
    },
}

/// Ordering, and therefore *which* shortest path is returned, is unchanged:
/// edges are considered in descending confidence with a total tie-break, and
/// the frontier is explored in the same first-in-first-out order.
fn shortest_path(
    edges: &[ResolvedEdge],
    from: &str,
    to: &str,
    max_depth: usize,
    max_nodes: usize,
    cancel: &Cancel,
) -> Result<PathSearch, QueryCancelled> {
    if max_depth == 0 || max_nodes == 0 {
        return Ok(PathSearch::Exhausted {
            depth_capped: max_depth == 0,
            node_capped: max_nodes == 0,
            visited: 0,
            max_depth,
            max_nodes,
        });
    }
    // Set the instant a limit actually costs the walk a successor it would
    // otherwise have expanded. Reaching a cap with nothing left to explore is
    // not a decline, so neither flag is raised for it.
    let mut depth_capped = false;
    let mut node_capped = false;
    let mut ordered: Vec<&ResolvedEdge> = edges.iter().collect();
    ordered.sort_by(|a, b| {
        b.confidence
            .0
            .total_cmp(&a.confidence.0)
            .then_with(|| a.source_file.cmp(&b.source_file))
            .then_with(|| a.source_symbol.cmp(&b.source_symbol))
            .then_with(|| a.target_file.cmp(&b.target_file))
            .then_with(|| a.target_symbol.cmp(&b.target_symbol))
            .then_with(|| edge_kind_name(a.edge_kind).cmp(edge_kind_name(b.edge_kind)))
    });

    // Source node -> its outgoing edges, in the order above. Built once; every
    // expansion below is a map lookup instead of a scan of the whole graph.
    let mut outgoing: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for (index, edge) in ordered.iter().enumerate() {
        cancel.check_every(index)?;
        outgoing
            .entry((edge.source_file.clone(), edge.source_symbol.clone()))
            .or_default()
            .push(index);
    }

    let mut reached: Vec<Reached> = Vec::new();
    let mut queue: VecDeque<usize> = VecDeque::new();
    let mut visited: BTreeSet<(String, String)> = BTreeSet::new();

    for (index, edge) in ordered.iter().enumerate() {
        cancel.check_every(index)?;
        if !edge_node_matches(&edge.source_file, &edge.source_symbol, from) {
            continue;
        }
        let node = (edge.target_file.clone(), edge.target_symbol.clone());
        if edge_node_matches(&node.0, &node.1, to) {
            return Ok(PathSearch::Found(vec![(*edge).clone()]));
        }
        if visited.len() >= max_nodes {
            node_capped = true;
            break;
        }
        if visited.insert(node.clone()) {
            reached.push(Reached {
                edge: index,
                parent: None,
                node,
                depth: 1,
            });
            queue.push_back(reached.len() - 1);
        }
    }

    let mut dequeued = 0usize;
    while let Some(entry) = queue.pop_front() {
        cancel.check_every(dequeued)?;
        dequeued += 1;
        let depth = reached[entry].depth;
        if depth >= max_depth {
            // Only a pruned node that *had* somewhere to go cost us anything.
            // A leaf at `max_depth` is fully explored, and counting it would
            // make every trace on a bounded graph report itself uncertain.
            if outgoing.contains_key(&reached[entry].node) {
                depth_capped = true;
            }
            continue;
        }
        let Some(candidates) = outgoing.get(&reached[entry].node) else {
            continue;
        };
        // Copied so the borrow of `reached` ends before it is extended below;
        // an adjacency list is a handful of entries, not the whole graph.
        let candidates = candidates.clone();
        for index in candidates {
            let edge = ordered[index];
            let next = (edge.target_file.clone(), edge.target_symbol.clone());
            if edge_node_matches(&next.0, &next.1, to) {
                reached.push(Reached {
                    edge: index,
                    parent: Some(entry),
                    node: next,
                    depth: depth + 1,
                });
                return Ok(PathSearch::Found(path_to(
                    &reached,
                    &ordered,
                    reached.len() - 1,
                )));
            }
            if visited.len() >= max_nodes {
                return Ok(PathSearch::Exhausted {
                    depth_capped,
                    node_capped: true,
                    visited: visited.len(),
                    max_depth,
                    max_nodes,
                });
            }
            if visited.insert(next.clone()) {
                reached.push(Reached {
                    edge: index,
                    parent: Some(entry),
                    node: next,
                    depth: depth + 1,
                });
                queue.push_back(reached.len() - 1);
            }
        }
    }
    if depth_capped || node_capped {
        return Ok(PathSearch::Exhausted {
            depth_capped,
            node_capped,
            visited: visited.len(),
            max_depth,
            max_nodes,
        });
    }
    Ok(PathSearch::NoPath)
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

/// Stored node paths are repo-relative. Resolving them against the recorded
/// build root — rather than the query process's working directory — is what
/// lets `devmap search` return real source spans from anywhere on the machine.
/// With no recorded root the relative path is used unchanged, which keeps the
/// pre-v7 behaviour for generations built before the root was captured.
pub(crate) fn resolve_source_path(repo_root: &Option<String>, path: &str) -> std::path::PathBuf {
    match repo_root {
        Some(root) => std::path::Path::new(root).join(path),
        None => std::path::PathBuf::from(path),
    }
}

pub fn resolved_edge_from_stored(edge: StoredEdge) -> anyhow::Result<ResolvedEdge> {
    stored_edge_to_resolved(edge)
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

impl<'a> QueryEngine<'a> {
    pub fn new(extractions: &'a [Extraction], resolution: &'a ResolutionResult) -> Self {
        let rate = devmap_analyze::resolution_rate(extractions, resolution);
        let attribution = devmap_analyze::AttributionCoverage {
            unresolved_sites: rate.unresolved_sites,
            explained_sites: rate.explained_sites,
        };
        let coverage_gap = devmap_analyze::combine_reasons(
            devmap_analyze::extraction_coverage(extractions).degraded_reason(),
            attribution_coverage_gap(Some(resolution.unresolved.len()), Some(&attribution)),
        );
        Self {
            extractions,
            resolution,
            coverage_gap,
        }
    }

    pub fn search(&self, req: Request<String>) -> Response<SymbolHit> {
        if req.query.trim().is_empty() {
            return Response {
                source_freshness: None,
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
            };
        }
        let q_lower = req.query.to_lowercase();
        let mut hits = Vec::new();

        for ext in self.extractions {
            for sym in &ext.symbols {
                let name_l = sym.name.to_lowercase();
                let qn_l = sym.qualified_name.to_lowercase();
                if !(name_l.contains(&q_lower) || qn_l.contains(&q_lower)) {
                    continue;
                }
                let score = if name_l == q_lower || qn_l == q_lower {
                    1.0
                } else if name_l.starts_with(&q_lower) {
                    0.95
                } else {
                    0.8
                };
                let disk_content = if ext.source_code.is_none() {
                    // In-memory engine: extractions are supplied by the caller,
                    // who is already running at the repo root, so the stored
                    // relative path is correct here.
                    std::fs::read_to_string(&ext.file_path).ok()
                } else {
                    None
                };
                let source_unavailable_reason = (ext.source_code.is_none()
                    && disk_content.is_none())
                .then(|| format!("source unavailable at query time for {:?}", ext.file_path));
                let code_str = ext
                    .source_code
                    .as_deref()
                    .or(disk_content.as_deref())
                    .unwrap_or("");
                let source_span = code_str
                    .get(sym.span.start_byte..sym.span.end_byte)
                    .unwrap_or("")
                    .to_string();
                let line_span = byte_span_to_line_range(code_str, &sym.span);
                // Capped for the same reason as the store-backed search above:
                // this engine shares the cost function, so it shares the bug.
                let (source_span, source_span_omitted_bytes) =
                    cap_source_span(source_span, req.token_budget);

                hits.push(SymbolHit {
                    symbol_name: sym.name.clone(),
                    file_path: ext.file_path.clone(),
                    kind: format!("{:?}", sym.kind),
                    span: line_span,
                    source_span,
                    source_unavailable_reason,
                    source_span_omitted_bytes,
                    score,
                });
            }
        }

        // Rank before truncating (T1–T4).
        hits.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.file_path.cmp(&b.file_path))
                .then_with(|| a.symbol_name.cmp(&b.symbol_name))
                .then_with(|| a.span.cmp(&b.span))
        });

        let mut response = budget_take(hits, req.token_budget, |hit| {
            u32::try_from(hit.source_span.len() / 4)
                .unwrap_or(u32::MAX)
                .saturating_add(20)
        });
        // The same caveat the store-backed search carries, from the same owner
        // asked of what this engine actually has. A refused extraction in the
        // slice hides its symbols from this loop exactly as a refused row hides
        // them from the index, so one engine disclosing and the other not would
        // let the same corpus answer differently depending on which was asked.
        //
        // Composed, not assigned: `budget_take` sets its own reason when the
        // page was cut.
        response.walk_incomplete = devmap_analyze::combine_reasons(
            response.walk_incomplete.take(),
            search_coverage_gap(
                devmap_analyze::extraction_coverage(self.extractions).degraded_reason(),
            ),
        );
        response
    }

    pub fn dependencies(&self, req: Request<String>) -> Response<ResolvedEdge> {
        if let Err(error) = devmap_store::checked_min_confidence(req.min_confidence) {
            return unavailable_response(ResolutionAvailability::Unavailable {
                reason: error.to_string(),
            });
        }
        let file_path = &req.query;
        let availability = match self
            .extractions
            .iter()
            .find(|extraction| &extraction.file_path == file_path)
        {
            None => ResolutionAvailability::Unavailable {
                reason: format!("{file_path} is not indexed"),
            },
            Some(extraction) if matches!(extraction.parse_outcome, ParseOutcome::Failed { .. }) => {
                ResolutionAvailability::Unavailable {
                    reason: format!("{file_path} could not be parsed"),
                }
            }
            Some(_) => ResolutionAvailability::Available,
        };
        if !matches!(availability, ResolutionAvailability::Available) {
            return unavailable_response(availability);
        }
        // The same statement the store-backed engine makes, from the same
        // owner: a file whose calls and imports were never extracted answers
        // here in the exact shape of one that genuinely has none.
        let file_gap = self
            .extractions
            .iter()
            .find(|extraction| &extraction.file_path == file_path)
            .and_then(|extraction| file_edge_coverage_gap(&extraction.parse_outcome));
        let coverage_gap = devmap_analyze::combine_reasons(file_gap, self.coverage_gap.clone());
        let mut deps = Vec::new();

        for edge in &self.resolution.edges {
            if (&edge.source_file == file_path || &edge.target_file == file_path)
                && edge.confidence.0 >= req.min_confidence
            {
                deps.push(edge.clone());
            }
        }

        deps.sort_by(|a, b| {
            b.confidence
                .0
                .total_cmp(&a.confidence.0)
                .then_with(|| a.source_file.cmp(&b.source_file))
                .then_with(|| a.target_file.cmp(&b.target_file))
                .then_with(|| a.target_symbol.cmp(&b.target_symbol))
        });

        let mut response = budget_take(deps, req.token_budget, |_| 25);
        response.walk_incomplete =
            devmap_analyze::combine_reasons(response.walk_incomplete.take(), coverage_gap);
        response
    }

    /// Inbound blast radius (impact) with parametric depth (closes G8).
    pub fn impact(&self, req: Request<String>) -> Response<ResolvedEdge> {
        if let Err(error) = devmap_store::checked_min_confidence(req.min_confidence) {
            return unavailable_response(ResolutionAvailability::Unavailable {
                reason: error.to_string(),
            });
        }
        let target = req.query.trim();
        let start: Vec<String> = self
            .resolution
            .edges
            .iter()
            .filter(|edge| {
                crate::query_match::traversal_start_matches(
                    target,
                    &edge.target_symbol,
                    &edge.target_file,
                )
            })
            .map(|edge| edge.target_symbol.clone())
            .collect();
        if start.is_empty() {
            let mut response = unavailable_response(ResolutionAvailability::Unavailable {
                reason: format!("{target} has no indexed inbound target"),
            });
            response.walk_incomplete = self.coverage_gap.clone();
            return response;
        }
        let opts = TraversalOptions {
            max_depth: req.max_depth.min(MAX_TRAVERSAL_DEPTH),
            max_nodes: 5000,
            reverse: true,
        };
        let index = AdjacencyIndex::build(&self.resolution.edges, opts.reverse)
            .with_min_confidence(req.min_confidence);
        let walk = traverse_graph_indexed(&start, &index, opts.limits());
        let mut inbound =
            traversed_resolution_edges(&walk, &self.resolution.edges, req.min_confidence);
        inbound.sort_by(|a, b| {
            b.confidence
                .0
                .total_cmp(&a.confidence.0)
                .then_with(|| a.source_file.cmp(&b.source_file))
                .then_with(|| a.target_file.cmp(&b.target_file))
                .then_with(|| a.source_symbol.cmp(&b.source_symbol))
        });
        let mut response = budget_take(inbound, req.token_budget, |_| 25);
        // The walk's own "I stopped looking" signal, carried the way
        // `StoreQueryEngine::traverse` carries it (engine.rs, `traverse`).
        // Discarding it published a depth-capped walk as a complete answer:
        // over a four-hop chain at depth 2 the traversal computes *"stopped at
        // depth 2; the result is a lower bound, not the full blast radius"* and
        // the response said `truncated: false, walk_incomplete: None`. For
        // `impact` in particular that is the reading that gets a live symbol
        // deleted — an incomplete blast radius is indistinguishable from a small
        // one.
        response.walk_incomplete = devmap_analyze::combine_reasons(
            walk.stop.reason(opts.max_depth, opts.max_nodes),
            self.coverage_gap.clone(),
        );
        response
    }

    /// Outbound trace with parametric depth (closes G8).
    pub fn trace(&self, req: Request<String>) -> Response<ResolvedEdge> {
        if let Err(error) = devmap_store::checked_min_confidence(req.min_confidence) {
            return unavailable_response(ResolutionAvailability::Unavailable {
                reason: error.to_string(),
            });
        }
        let target = req.query.trim();
        let start: Vec<String> = self
            .resolution
            .edges
            .iter()
            .filter(|edge| {
                crate::query_match::traversal_start_matches(
                    target,
                    &edge.source_symbol,
                    &edge.source_file,
                )
            })
            .map(|edge| edge.source_symbol.clone())
            .collect();
        if start.is_empty() {
            let mut response = unavailable_response(ResolutionAvailability::Unavailable {
                reason: format!("{target} has no indexed outbound source"),
            });
            response.walk_incomplete = self.coverage_gap.clone();
            return response;
        }
        let opts = TraversalOptions {
            max_depth: req.max_depth.min(MAX_TRAVERSAL_DEPTH),
            max_nodes: 5000,
            reverse: false,
        };
        let index = AdjacencyIndex::build(&self.resolution.edges, opts.reverse)
            .with_min_confidence(req.min_confidence);
        let walk = traverse_graph_indexed(&start, &index, opts.limits());
        let mut outbound =
            traversed_resolution_edges(&walk, &self.resolution.edges, req.min_confidence);
        outbound.sort_by(|a, b| {
            b.confidence
                .0
                .total_cmp(&a.confidence.0)
                .then_with(|| a.source_file.cmp(&b.source_file))
                .then_with(|| a.target_file.cmp(&b.target_file))
                .then_with(|| a.source_symbol.cmp(&b.source_symbol))
        });
        let mut response = budget_take(outbound, req.token_budget, |_| 25);
        // Same signal, same reason as `impact` above.
        response.walk_incomplete = devmap_analyze::combine_reasons(
            walk.stop.reason(opts.max_depth, opts.max_nodes),
            self.coverage_gap.clone(),
        );
        response
    }
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
const TRAVERSAL_MAX_NODES: usize = 5_000;

/// Token cost the budgeter charges for one graph edge, everywhere.
const EDGE_TOKENS: u32 = 25;
/// Token cost of one dead-symbol row, and so the divisor that turns a budget
/// into how many rows are worth reading.
const DEAD_SYMBOL_TOKENS: u32 = 30;

/// Node ids listed per blast-radius band. The band's exact size travels in
/// `node_count` regardless, so this trims the listing, never the count.
const BLAST_LAYER_NODE_SAMPLE: usize = 50;

/// Reached symbols listed per affected test file; `reached_symbols` stays exact.
const AFFECTED_SYMBOL_SAMPLE: usize = 8;

/// Fixed per-definition cost in `explore`'s packer: identity, kind, span, score
/// and the two edge-response envelopes, before any source text.
const EXPLORE_DEFINITION_OVERHEAD_TOKENS: u32 = 40;

/// The `file::symbol` identity every traversal surface resolves.
fn node_id_of(file_path: &str, symbol_name: &str) -> String {
    if file_path.is_empty() {
        return symbol_name.to_string();
    }
    if symbol_name.is_empty() {
        return file_path.to_string();
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
fn indexed_traversed_edges(
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
/// Identical to the [`traversal_starts`] scan it replaced — same pairs, same
/// order and duplicates. The traversal admits distinct seeds before applying
/// its node cap, so repeated edge endpoints cannot consume that capacity.
/// What changes is the cost: a symbol query tests the distinct
/// symbols (41,276 on the ScholarLM corpus) and a path query the distinct
/// files (4,499), instead of testing every one of 271,543 edges.
///
/// Lifted out of `traverse` so the blast radius resolves its seeds through the
/// same matcher the traversal does. Resolving them two ways is how a radius
/// ends up seeded from a symbol the trace never visits.
fn indexed_traversal_starts(
    index: &GenerationEdges,
    target: &str,
    reverse: bool,
    min_confidence: f32,
    cancel: &Cancel,
) -> anyhow::Result<Vec<(String, String)>> {
    let mut ids: Vec<u32> = Vec::new();
    match crate::query_match::classify(target) {
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
    Ok(ids
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
        .collect())
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
struct BlastBand {
    depth: usize,
    members: BTreeSet<(String, String)>,
    lowest_confidence: Option<f32>,
    node_count: u32,
}

/// The complete result of an inbound walk: every band, unsampled, unbudgeted.
///
/// Separated from [`BlastRadius`] because two consumers want different things
/// from it. `affected_tests` needs everything the walk reached — deriving its
/// answer from a trimmed list would drop tests without any counter saying so.
/// `explore` needs something that fits a token budget. Presentation is
/// [`Self::into_radius`]; derivation reads the bands directly.
struct BlastWalk {
    seeds: Vec<(String, String)>,
    unmatched: Vec<String>,
    bands: Vec<BlastBand>,
    total_impacted: u32,
    stop: TraversalStop,
    depth_cap: usize,
    /// True when no target resolved to a traversal start at all — an answer of
    /// "nothing is impacted" that nothing actually looked for.
    unresolved_seeds: bool,
    coverage_gap: Option<String>,
}

impl BlastWalk {
    /// Why the walk is a lower bound, or `None` when it ran to completion.
    fn incomplete_reason(&self) -> Option<String> {
        devmap_analyze::combine_reasons(
            self.stop.reason(self.depth_cap, TRAVERSAL_MAX_NODES),
            self.coverage_gap.clone(),
        )
    }

    /// Sample each band and pack the bands into `token_budget`.
    ///
    /// The two trims are reported separately and neither touches a count:
    /// `nodes_omitted` per band, `hidden`/`truncated` for the band list.
    fn into_radius(self, token_budget: u32) -> BlastRadius {
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
fn blast_radius_from_edges(
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

/// Divide one caller-supplied budget across `explore`'s four parts.
///
/// Halves and quarters, computed before any work, so the split is deterministic
/// and reportable. `edges_per_direction` is filled in later — it cannot be
/// known until the packer has decided how many definitions there are to divide
/// the edge pool between.
fn explore_budget(total: u32) -> ExploreBudget {
    let definitions = total / 2;
    let blast_radius = total / 4;
    ExploreBudget {
        total,
        definitions,
        edges_per_direction: 0,
        blast_radius,
    }
}

/// Split the edge pool evenly across every direction of every definition shown.
///
/// Deliberately not floored at one edge's worth: a floor would let a large
/// answer exceed the budget the caller set, and `DevMapClient._budgeted` treats
/// the budget as a hard contract. A direction that gets nothing still reports
/// `total` — "0 shown of 42 callers" is a complete answer to "how many", which
/// is the question the count exists for.
fn edges_per_direction(budget: &ExploreBudget, shown: u32) -> u32 {
    let pool = budget
        .total
        .saturating_sub(budget.definitions)
        .saturating_sub(budget.blast_radius);
    let directions = shown.saturating_mul(2);
    if directions == 0 {
        return 0;
    }
    pool / directions
}

/// Token cost of one packed definition: its source span plus a fixed overhead,
/// charged at the same [`BYTES_PER_TOKEN`] every other surface uses.
fn explore_definition_tokens(definition: &ExploreDefinition) -> u32 {
    u32::try_from(definition.source.len() / BYTES_PER_TOKEN as usize)
        .unwrap_or(u32::MAX)
        .saturating_add(EXPLORE_DEFINITION_OVERHEAD_TOKENS)
}

/// Token cost of one blast-radius band: its listed node ids plus a small header.
fn blast_layer_tokens(layer: &BlastLayer) -> u32 {
    let bytes: usize = layer.nodes.iter().map(|node| node.len() + 1).sum();
    u32::try_from(bytes / BYTES_PER_TOKEN as usize)
        .unwrap_or(u32::MAX)
        .saturating_add(10)
}

/// Token cost of one affected-test row: path, listed symbols, and a header.
fn affected_test_tokens(test: &AffectedTest) -> u32 {
    let bytes: usize = test.path.len()
        + test
            .symbols
            .iter()
            .map(|symbol| symbol.len() + 1)
            .sum::<usize>();
    u32::try_from(bytes / BYTES_PER_TOKEN as usize)
        .unwrap_or(u32::MAX)
        .saturating_add(10)
}

/// Whether `path` names a test file.
///
/// Deliberately stricter than the Python predicate it replaces, which asked
/// `"/test" in "/" + path` and so counted `src/testing_utils.py`,
/// `lib/latest/mod.rs` and any path containing the substring anywhere as tests.
/// Here a directory must be *exactly* a test directory, and a file must carry a
/// recognised test affix. Recorded in `DIVERGENCES.md`: the affected-test list
/// gets shorter and the entries that leave were never tests.
pub fn is_test_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let Some((directories, file_name)) = normalized.rsplit_once('/') else {
        return is_test_file_name(&normalized);
    };
    let in_test_directory = directories.split('/').any(|segment| {
        matches!(
            segment.to_lowercase().as_str(),
            "test" | "tests" | "spec" | "specs" | "__tests__" | "testing" | "e2e"
        )
    });
    in_test_directory || is_test_file_name(file_name)
}

/// Whether a bare file name carries a test affix.
///
/// Affixes only, never bare substrings, and the boundary is the point: `test`
/// as a suffix of a *word* (`contest`, `attestation`, `latest`) is not a test
/// affix, and `testing_utils.py` is not `test_utils.py`. The camel-case arm
/// reads the original casing, so `FooTest.java` is recognised while `contest`
/// is not — which is why the name is not lower-cased wholesale first.
fn is_test_file_name(file_name: &str) -> bool {
    let lowered = file_name.to_lowercase();
    let stem = file_name.split('.').next().unwrap_or(file_name);
    let lowered_stem = lowered.split('.').next().unwrap_or(&lowered);
    lowered.starts_with("test_")
        || lowered.starts_with("spec_")
        || lowered.contains(".test.")
        || lowered.contains(".spec.")
        || lowered.contains("_test.")
        || lowered.contains("_spec.")
        || lowered.contains("-test.")
        || lowered.contains("-spec.")
        || matches!(lowered_stem, "test" | "tests" | "spec" | "specs")
        || stem.ends_with("Test")
        || stem.ends_with("Tests")
        || stem.ends_with("Spec")
        || stem.ends_with("Specs")
}

/// Fold one reached symbol into the nearest-test table.
///
/// A free function rather than a closure so the table can be read back in the
/// same scope it is built in.
fn record_test_hit(
    nearest: &mut BTreeMap<String, (usize, BTreeSet<String>)>,
    symbol: &str,
    file: &str,
    depth: usize,
) {
    if !is_test_path(file) {
        return;
    }
    let entry = nearest
        .entry(file.to_string())
        .or_insert((depth, BTreeSet::new()));
    entry.0 = entry.0.min(depth);
    entry.1.insert(symbol.to_string());
}

fn unavailable_response<T>(resolution: ResolutionAvailability) -> Response<T> {
    Response {
        source_freshness: None,
        items: Vec::new(),
        shown: 0,
        hidden: 0,
        total: 0,
        truncated: false,
        tokens_used: 0,
        resolution,
        walk_incomplete: None,
        rungs: None,
        dead_clusters: None,
        dead_clusters_truncated: 0,
        dead_clusters_incomplete: None,
    }
}

pub(crate) fn byte_span_to_line_range(source: &str, span: &Span) -> (u32, u32) {
    byte_span_to_line_range_in(&devmap_extract::model::LineIndex::new(source), span)
}

/// [`byte_span_to_line_range`] over a table built once per file, for a loop
/// that converts every span in the file — the string form scans from the top
/// of the file on every call, which the artifact's node loop paid twice per
/// symbol.
pub(crate) fn byte_span_to_line_range_in(
    lines: &devmap_extract::model::LineIndex,
    span: &Span,
) -> (u32, u32) {
    // Delegates the counting to `Span::line_range`, which is the canonical
    // owner and is already UTF-8-safe.
    //
    // This function used to count newlines itself with `source[..start]` —
    // slicing a `&str`, which **panics** on an index that is not a character
    // boundary. Spans are byte offsets recorded at extraction time while
    // `source` is re-read from disk when the graph is exported, so any
    // multi-byte character inserted before an indexed symbol's end offset put
    // the offset mid-character: one emoji added to a file aborted
    // `dev map manifest` outright, and the release profile is `panic = "abort"`,
    // so there was no recovery.
    //
    // Two copies of one computation existed and only one was safe. The wrapper
    // survives for the single thing it adds beyond the canonical version — the
    // `max(start)` below — and no longer restates the arithmetic.
    let len = lines.len();
    let clamped = Span {
        start_byte: span.start_byte.min(len),
        // A stored span whose end precedes its start would otherwise report an
        // end line above its start line. Clamping keeps the range orderable for
        // the consumers that render it as `line..end_line`.
        end_byte: span.end_byte.min(len).max(span.start_byte.min(len)),
    };
    lines.line_range(&clamped)
}

/// Per-hit token overhead in [`StoreQueryEngine::search`]'s cost function.
/// Kept next to [`cap_source_span`] because the cap must invert the same
/// arithmetic the packer uses, and a drift between the two reintroduces the
/// oversized-hit bug in a form no test names.
const SEARCH_HIT_OVERHEAD_TOKENS: u32 = 20;

/// Bytes of source per token, matching the `len / 4` estimate in the search
/// cost function.
pub const BYTES_PER_TOKEN: u32 = 4;

/// How many ranked candidates a budget could conceivably show.
///
/// A hit costs at least [`SEARCH_HIT_OVERHEAD_TOKENS`], so `budget / overhead`
/// bounds how many can fit; the `+ 1` keeps the page from cutting a hit the
/// packer would still have admitted, and the floor of 1 keeps a zero budget
/// from asking for an empty page and reporting "nothing matched".
///
/// Both search paths use this as the ceiling on *materialised* hits — the ones
/// whose source is read off disk. Keyword search draws its candidates from a
/// wider page ([`search_rank_pool_size`]) and cuts to this after ranking;
/// semantic search bounds how far down its ranking it materialises hits before
/// budgeting them. Two copies of the arithmetic would let the same budget mean
/// different page sizes depending on which command asked.
fn budget_page_size(token_budget: u32) -> usize {
    (token_budget / SEARCH_HIT_OVERHEAD_TOKENS)
        .saturating_add(1)
        .max(1) as usize
}

/// Hard ceiling on hits one `search` may materialise, whatever the budget asks.
///
/// K-B1: every hit on a search page is a file opened and read, and the page was
/// `token_budget / 20 + 1` — 5,001 rows at a 100,000-token budget. The token
/// budget is a *presentation* limit the caller chooses; letting it set the
/// number of files opened turns "give me a generous budget" into "open five
/// thousand files". 200 is far past any page a reader consumes and far below
/// anything that reads a corpus.
///
/// It applies to `search` and not to `explore`, and the difference is where the
/// I/O is: `explore` scores stored rows and opens a file only for the
/// definitions that survive its own `limit` —
/// `explore_reads_one_file_per_definition_it_returns_not_per_candidate` pins
/// that — so capping its candidate page would cost ranking quality on a
/// high-match query and buy no bounded-ness at all.
///
/// The cap trims the page, never the count: `total` is still measured over the
/// whole index and a trimmed page still reports `truncated` and `hidden`.
pub const SEARCH_PAGE_MAX: usize = 200;

/// Hits `search` will materialise for this budget: what it can show, capped at
/// what it is allowed to open. See [`SEARCH_PAGE_MAX`].
fn search_page_size(token_budget: u32) -> usize {
    budget_page_size(token_budget).min(SEARCH_PAGE_MAX)
}

/// How many candidates keyword search pulls from the store before ranking them.
///
/// The store orders its page by bm25 and this crate ranks by exact/prefix/other
/// match, so the page has to be wider than the answer or the second ranking
/// only ever sees what the first one liked. Ten times is comfortably past the
/// gap the audit measured (an exact match 100 rows below the cut on a 200-match
/// query) without being a licence to walk the corpus: the pool is a hard
/// ceiling, and when the match set outruns it `search` says so on
/// `walk_incomplete` rather than presenting a sample's best as the corpus's.
const SEARCH_RANK_OVERSAMPLE: usize = 10;

/// Hard ceiling on that pool, whatever the budget asks for.
///
/// Every pooled row costs a `String` comparison and no file read, so 2,000 is
/// cheap; it is here so one query can never scan an unbounded number of FTS
/// rows on a corpus where the query matches everything.
const SEARCH_RANK_POOL_MAX: usize = 2_000;

fn search_rank_pool_size(token_budget: u32) -> usize {
    // The *capped* page: the floor below exists so the pool can never be
    // narrower than what will be shown, and taking it from the uncapped page
    // would let a large budget reopen the ceiling the cap just closed.
    let page = search_page_size(token_budget);
    // Never below the page: a pool smaller than what the budget could show
    // would drop results the caller has already paid for.
    page.saturating_mul(SEARCH_RANK_OVERSAMPLE)
        .min(SEARCH_RANK_POOL_MAX)
        .max(page)
}

/// Why one file's edge list is a lower bound, or `None` when it is not.
///
/// `Failed` is not here: a file that contributed nothing is refused outright
/// with `resolution: Unavailable`, which is a stronger statement than this one
/// and is made by the callers. The two states below did contribute — they are
/// the ones that answer in the shape of a complete extraction while holding
/// less than one.
///
/// `Clean` returns `None`, and that is the load-bearing case: a caveat that
/// rides on every answer tells a reader nothing, which is the failure mode
/// [`analysis_coverage_gap`] documents for its own marker.
///
/// One owner for both engines. `StoreQueryEngine::dependencies` reads the
/// outcome off a stored row and `QueryEngine::dependencies` off an in-memory
/// `Extraction`, and two near-copies of this sentence would let the same file
/// be described differently depending on which one was asked.
fn file_edge_coverage_gap(outcome: &ParseOutcome) -> Option<String> {
    match outcome {
        ParseOutcome::Clean | ParseOutcome::Failed { .. } => None,
        ParseOutcome::Fallback { reason } => Some(format!(
            "this file's declarations were recovered by pattern rather than parsed ({reason}); \
             the pattern scanner extracts no calls and no imports at all, so an empty or short \
             list here is not evidence the file has no dependencies"
        )),
        ParseOutcome::Partial { error_ranges } => Some(format!(
            "this file parsed with {} error range(s); a call or import inside an error region \
             is invisible to extraction, so this list is a lower bound",
            error_ranges.len()
        )),
        // Unlike `Failed`, the callers do *not* refuse this file — it is
        // indexed and its `File` node is a real node — so if the caveat were
        // `None` here the answer would be an empty dependency list presented as
        // a fact about the file. It is a fact about the extractor.
        ParseOutcome::Skipped { reason } => Some(format!(
            "this file was not parsed ({reason}); no calls or imports were extracted from it \
             at all, so an empty list here is not evidence the file has no dependencies"
        )),
    }
}

fn qualify_response<T>(response: &mut Response<T>, reason: &str) {
    response.walk_incomplete =
        devmap_analyze::combine_reasons(response.walk_incomplete.take(), Some(reason.to_string()));
}

/// Why an answer derived from one generation's graph is a lower bound.
///
/// Two independent reasons, joined rather than ranked — a reader deciding
/// whether to act on "nothing calls this" needs every qualification the run
/// holds, not the first one that fired. `None` on a converged analysis with
/// every call attributed is the load-bearing case: a marker that appears on
/// every answer leaves a caller exactly where it started.
///
/// Shared by `dead_symbols` and by every traversal, because they are the same
/// claim about the same graph. `dead_symbols` had it and `impact` did not,
/// which is backwards: the dead list is explicitly a *candidate* list and
/// already exempts symbols in unread files, while `impact` is what a reader
/// consults immediately before deleting a symbol, and it answered `items: [],
/// resolution: Available, walk_incomplete: None` over a corpus whose only
/// calling file had never been parsed.
///
/// The wording is direction-neutral for that reason: it describes the holes in
/// the graph, and leaves what those holes mean to the surface that names
/// itself.
fn analysis_coverage_gap(
    analysis: Option<&devmap_analyze::model::AnalysisDisclosure>,
) -> Option<String> {
    let unresolved = analysis.and_then(|analysis| {
        attribution_coverage_gap(analysis.unresolved_calls, analysis.resolution_rate.as_ref())
    });
    devmap_analyze::combine_reasons(analysis_status_gap(analysis), unresolved)
}

/// Unresolved sites include known external targets and are not an edge count.
/// Use the persisted rate's existing classification, with conservative fallback
/// for older or inconsistent summaries. These are repository-wide measurements;
/// no attribution data establishes how many missing links affect this target.
fn attribution_coverage_gap(
    total: Option<usize>,
    coverage: Option<&devmap_analyze::AttributionCoverage>,
) -> Option<String> {
    let Some(total) = total else {
        return Some("the unresolved attribution count was not recorded for this generation; call-graph coverage is unknown".to_string());
    };
    match coverage {
        Some(coverage) if coverage.unresolved_sites == total && coverage.explained_sites <= total => {
            let remaining = total - coverage.explained_sites;
            (remaining > 0).then(|| format!(
                "{remaining} of {total} unresolved attribution site(s) have no indexed target after excluding {} known builtin, runtime-global, and external-import site(s); these repository-wide counts are not specific to this target, so this answer may omit callers or dependencies",
                coverage.explained_sites,
            ))
        }
        None if total == 0 => None,
        _ => Some(format!(
            "this generation records {total} unresolved attribution site(s), but their classification breakdown is unavailable or inconsistent; these repository-wide counts are not specific to this target, so call-graph coverage is unknown"
        )),
    }
}

#[cfg(test)]
mod attribution_disclosure_tests {
    use super::analysis_coverage_gap;
    #[cfg(feature = "parse")]
    use super::{qualify_response, Request, StoreQueryEngine};
    use devmap_analyze::AnalysisDisclosure;
    use serde_json::{json, Value};

    #[cfg(feature = "parse")]
    #[test]
    fn a_composed_read_retries_once_and_keeps_all_existing_qualifications() {
        for moving_reads in [0, 1, 2, usize::MAX] {
            let store = devmap_store::Store::open_in_memory().unwrap();
            let resolution = devmap_resolve::Resolver::new().resolve_all(&[]);
            let analysis = devmap_analyze::AnalysisSummary {
                status: devmap_analyze::AnalysisStatus::Partial {
                    reason: "parse coverage gap".into(),
                },
                ..Default::default()
            };
            store.save_generation(&[], &resolution, &analysis).unwrap();
            let engine = StoreQueryEngine::new(&store);
            let mut reads = 0;
            let response = engine
                .read_composed(
                    || {
                        reads += 1;
                        let answer = engine.impact(Request {
                            query: "missing".into(),
                            token_budget: 2000,
                            min_confidence: 0.0,
                            max_depth: 3,
                        })?;
                        if reads <= moving_reads {
                            store.save_generation(&[], &resolution, &analysis)?;
                        }
                        Ok(answer)
                    },
                    qualify_response,
                )
                .unwrap();
            assert_eq!(reads, if moving_reads == 0 { 1 } else { 2 });
            let reason = response.walk_incomplete.unwrap();
            assert!(reason.contains("parse coverage gap"), "{reason}");
            assert_eq!(
                reason.contains("index moved"),
                moving_reads >= 2,
                "{reason}"
            );
        }
    }

    fn gap(fields: Value) -> Option<String> {
        let mut summary =
            json!({"total_files": 1, "total_symbols": 2, "total_edges": 1, "status": "Ok"});
        summary
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let disclosure: AnalysisDisclosure = serde_json::from_value(summary).unwrap();
        analysis_coverage_gap(Some(&disclosure))
    }

    #[test]
    fn legacy_or_inconsistent_counters_cannot_claim_complete_coverage() {
        for fields in [
            json!({}),
            json!({"unresolved_calls": null}),
            json!({"unresolved_calls": 4}),
            json!({"unresolved_calls": 4, "resolution_rate": null}),
            json!({"unresolved_calls": 4, "resolution_rate": {"unresolved_sites": 0, "explained_sites": 0}}),
            json!({"unresolved_calls": 4, "resolution_rate": {"unresolved_sites": 4, "explained_sites": 5}}),
            json!({"unresolved_calls": 0, "resolution_rate": {"unresolved_sites": 2, "explained_sites": 2}}),
        ] {
            let reason =
                gap(fields.clone()).unwrap_or_else(|| panic!("must remain uncertain: {fields}"));
            assert!(reason.contains("unknown"), "{fields}: {reason}");
            assert!(!reason.contains("missing that many edges"), "{reason}");
        }
    }

    #[test]
    fn recorded_zero_and_fully_explained_counts_do_not_invent_a_gap() {
        for fields in [
            json!({"unresolved_calls": 0}),
            json!({"unresolved_calls": 0, "resolution_rate": {"unresolved_sites": 0, "explained_sites": 0}}),
            json!({"unresolved_calls": 4, "resolution_rate": {"unresolved_sites": 4, "explained_sites": 4}}),
        ] {
            assert_eq!(gap(fields), None);
        }
    }

    #[test]
    fn explained_calls_do_not_erase_parse_failures_or_timeouts() {
        for status in [
            json!({"Partial": {"reason": "file not parsed"}}),
            json!({"Timeout": {"reason": "analysis deadline"}}),
        ] {
            let reason = gap(json!({"status": status, "unresolved_calls": 4, "resolution_rate": {"unresolved_sites": 4, "explained_sites": 4}})).unwrap();
            assert!(
                reason.contains("file not parsed") || reason.contains("analysis deadline"),
                "{reason}"
            );
        }
    }
}

/// The corpus half of [`analysis_coverage_gap`], on its own.
///
/// Split out because the two halves answer different questions and not every
/// surface is entitled to both. `unresolved_calls` is about *edges* — how much
/// of the call graph was attributed — and it is non-zero on essentially every
/// real repository. A surface that does not answer from the call graph must not
/// carry it, or the marker rides on every answer and tells a reader nothing,
/// which is the failure mode [`analysis_coverage_gap`] documents.
///
/// What this half says is about the *corpus*: whether the generation is a
/// complete read of the repository at all. `AnalysisStatus::Partial` is where
/// `ExtractionCoverage::degraded_reason` lands, so the counts it carries —
/// files that failed to parse, were recovered by pattern, or were refused by
/// discovery — come through verbatim from their one owner.
fn analysis_status_gap(
    analysis: Option<&devmap_analyze::model::AnalysisDisclosure>,
) -> Option<String> {
    use devmap_analyze::model::AnalysisStatus;
    // A generation exists but its analysis blob does not read back. That is a
    // check that could not run, and it must not answer like one that ran.
    let Some(analysis) = analysis else {
        return Some(
            "the analysis summary for this generation could not be read, so the coverage \
             behind this answer is unknown"
                .to_string(),
        );
    };
    match &analysis.status {
        AnalysisStatus::Ok => None,
        AnalysisStatus::Partial { reason } => Some(format!("the analysis is partial: {reason}")),
        AnalysisStatus::Timeout { reason } => Some(format!("the analysis timed out: {reason}")),
    }
}

/// What a corpus-level gap means for a *search*, or `None` when there is none.
///
/// `search` answers "does a symbol by this name exist here", and a miss is the
/// answer callers act on hardest: `total: 0, truncated: false, resolution:
/// Available, walk_incomplete: None` reads as *"there are zero matches in this
/// corpus"* stated as a completed check. Every symbol of a file that was
/// refused — a parse over its budget, a grammar that would not load, a NUL byte
/// caught at the boundary — is absent from the index, so a name that lives only
/// there answered identically to a name that exists nowhere. A reader deciding
/// "this symbol does not exist, so I may take the name" and one deciding "this
/// symbol may exist in a file nothing read" were given the same sentence.
///
/// One owner for both engines and one wording, fed from the two places the same
/// fact lives: the persisted disclosure for [`StoreQueryEngine`], and
/// [`devmap_analyze::extraction_coverage`] over the slice for [`QueryEngine`].
/// `None` in, `None` out is the load-bearing case — a fully read corpus must
/// keep answering without a caveat.
fn search_coverage_gap(corpus_gap: Option<String>) -> Option<String> {
    corpus_gap.map(|gap| {
        format!(
            "the corpus behind this answer is not a complete read of the repository, so a \
             name that matches nothing here may still be declared in a file that was never \
             indexed: {gap}"
        )
    })
}

fn ranking_coverage_gap(total: u32, pool: usize) -> Option<String> {
    (total as usize > pool).then(|| format!(
        "ranked the first {pool} of {total} matches, in the store's relevance order; a closer match may sit outside that page"
    ))
}

fn rank_symbol_rows(
    rows: Vec<StoredSymbol>,
    query_lower: &str,
    cancel: &Cancel,
) -> anyhow::Result<Vec<(f32, StoredSymbol)>> {
    let mut ranked = Vec::with_capacity(rows.len());
    for (index, row) in rows.into_iter().enumerate() {
        cancel.check_every(index)?;
        ranked.push((name_match_score(&row, query_lower), row));
    }
    ranked.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .total_cmp(left_score)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.span_start.cmp(&right.span_start))
            .then_with(|| left.span_end.cmp(&right.span_end))
    });
    Ok(ranked)
}

/// Rank of one stored symbol against an already-lowercased query.
///
/// The single owner of the ordering, for `search` and for `explore` alike: two
/// copies would let the same query return a different "best match" depending on
/// which command asked. It runs on the stored row rather than on a built
/// [`SymbolHit`] precisely so that ranking can happen before the file reads do
/// — which is what lets the candidate pool be wider than the answer without
/// costing the caller anything.
fn name_match_score(row: &devmap_store::StoredSymbol, query_lower: &str) -> f32 {
    let name = row.name.to_lowercase();
    if name == query_lower || row.qualified_name.to_lowercase() == query_lower {
        1.0
    } else if name.starts_with(query_lower) {
        0.95
    } else {
        0.8
    }
}

/// Token cost of one search hit: its source span plus a fixed per-row overhead.
///
/// Shared by keyword and semantic search so the two spend the budget at the
/// same rate; two copies of this arithmetic would let the same result cost
/// different amounts depending on which command asked for it.
fn search_hit_tokens(hit: &SymbolHit) -> u32 {
    u32::try_from(hit.source_span.len() / BYTES_PER_TOKEN as usize)
        .unwrap_or(u32::MAX)
        .saturating_add(SEARCH_HIT_OVERHEAD_TOKENS)
}

#[cfg(test)]
thread_local! {
    /// Source-span reads attempted on this thread.
    ///
    /// Reading a file per scored symbol is the cost `search_semantic` used to
    /// pay for the entire corpus before the budget was applied, and "how many
    /// files did this query open" is not observable from the response. Counted
    /// per thread rather than globally so tests running in parallel in one
    /// binary cannot contaminate each other's count.
    pub(crate) static SOURCE_SPAN_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };

    /// Bytes those reads pulled off disk on this thread.
    ///
    /// The count above says how many files were opened; it says nothing about
    /// how much of each was read, and `read_to_string` read all of it. A 50 MB
    /// vendored bundle answered a one-line span with 50 MB of I/O and 50 MB of
    /// resident string, per hit, and neither the response nor the read counter
    /// showed it.
    pub(crate) static SOURCE_SPAN_BYTES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Verify the complete source identity before applying stored byte coordinates.
/// A bounded prefix alone can belong to a different file revision. Reads stay
/// within discovery's source ceiling and only cover files selected for hits.
fn read_verified_source(
    path: &std::path::Path,
    span: std::ops::Range<usize>,
    expected_hash: u64,
) -> std::io::Result<String> {
    let source = devmap_extract::read_source(path)?;
    #[cfg(test)]
    SOURCE_SPAN_BYTES.with(|bytes| bytes.set(bytes.get().saturating_add(source.len() as u64)));
    if devmap_extract::content_hash(&source) != expected_hash {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "source changed since this symbol was indexed",
        ));
    }
    if source.get(span).is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "stored span is invalid for the verified source",
        ));
    }
    Ok(source)
}

/// Build a hit from a stored symbol row, reading its source span from disk.
///
/// One owner for the read, the line-range conversion, the span cap and the
/// unavailability reason. When the file cannot be read the reason is recorded
/// on the hit rather than dropped, so an empty `source_span` is never mistaken
/// for a symbol with no body.
fn hit_from_stored(
    row: devmap_store::StoredSymbol,
    repo_root: Option<&str>,
    token_budget: u32,
    score: f32,
) -> SymbolHit {
    let owned_root = repo_root.map(str::to_string);
    #[cfg(test)]
    SOURCE_SPAN_READS.with(|reads| reads.set(reads.get().saturating_add(1)));
    let source_result = read_verified_source(
        &resolve_source_path(&owned_root, &row.path),
        row.span_start..row.span_end,
        row.content_hash,
    );
    let source_unavailable_reason = source_result.as_ref().err().map(|error| {
        format!(
            "source unavailable at query time for {:?}: {error}",
            row.path
        )
    });
    let source = source_result.ok();
    let source_span = source
        .as_deref()
        .and_then(|text| text.get(row.span_start..row.span_end))
        .unwrap_or("")
        .to_string();
    let span = source
        .as_deref()
        .map(|text| {
            Span {
                start_byte: row.span_start,
                end_byte: row.span_end,
            }
            .line_range(text)
        })
        .unwrap_or((0, 0));
    let (source_span, source_span_omitted_bytes) = cap_source_span(source_span, token_budget);
    SymbolHit {
        symbol_name: row.name,
        file_path: row.path,
        kind: row.kind,
        span,
        source_span,
        source_unavailable_reason,
        source_span_omitted_bytes,
        score,
    }
}

/// Cap a hit's source span so one hit can never exceed the whole token budget.
///
/// A search hit costs `source_span.len() / 4 + 20` tokens, and `source_span` is
/// the symbol's entire body. One 8 KB function therefore outweighed the 2,000
/// token default on its own, and the caller enforces the budget as a hard
/// contract — `DevMapClient._budgeted` raises on an over-budget response — so
/// an uncapped hit is not merely large, it is unreturnable. `devmap search
/// "resolve calls"` on this repository matched exactly one symbol,
/// `resolve_calls`, and answered with nothing.
///
/// Returns the (possibly capped) span and the number of bytes dropped, which
/// the caller records in `source_span_omitted_bytes`. A capped span is never
/// passed off as the verbatim body R2 promises.
/// Largest share of a request's budget one hit's source span may take.
///
/// Capping at the *whole* budget — which this did — is enough to keep a single
/// oversized item from being withheld, but it lets that item crowd out every
/// other result. A `File` symbol's span is its entire file, so a search whose
/// best matches are files returned two hits against a 4,000-token budget and
/// reported 512 more withheld. A quarter guarantees at least three results
/// survive alongside any one of them.
const MAX_HIT_BUDGET_SHARE: u32 = 4;

fn cap_source_span(source_span: String, token_budget: u32) -> (String, Option<u32>) {
    let max_bytes = (token_budget / MAX_HIT_BUDGET_SHARE)
        .saturating_sub(SEARCH_HIT_OVERHEAD_TOKENS)
        .saturating_mul(BYTES_PER_TOKEN) as usize;
    if source_span.len() <= max_bytes {
        return (source_span, None);
    }
    // Truncate on a char boundary: `String` slicing panics mid-codepoint, and
    // source files contain non-ASCII in strings, comments and identifiers.
    let mut end = max_bytes;
    while end > 0 && !source_span.is_char_boundary(end) {
        end -= 1;
    }
    let omitted = u32::try_from(source_span.len() - end).unwrap_or(u32::MAX);
    let mut capped = source_span;
    capped.truncate(end);
    (capped, Some(omitted))
}

pub fn budget_take<T, F>(items: Vec<T>, token_budget: u32, cost_of: F) -> Response<T>
where
    F: Fn(&T) -> u32,
{
    let total = items.len() as u32;
    let mut out = Vec::new();
    let mut current_tokens = 0u32;
    let mut truncated = false;

    // The budget is hard: an item that does not fit is withheld, never emitted
    // over budget. `test_search_never_exceeds_hard_token_budget` pins this, and
    // `DevMapClient._budgeted` raises on any response that breaks it, so a
    // packer that "made progress" by exceeding the budget would turn a thin
    // result into a client-side error.
    //
    // Which is why an oversized *item* is bounded where it is built rather than
    // waved through here — see `cap_source_span`.
    for item in items {
        let cost = cost_of(&item);
        if cost > token_budget.saturating_sub(current_tokens) {
            truncated = true;
            break;
        }
        current_tokens += cost;
        out.push(item);
    }

    Response {
        source_freshness: None,
        shown: out.len() as u32,
        hidden: total.saturating_sub(out.len() as u32),
        total,
        truncated,
        tokens_used: current_tokens,
        items: out,
        resolution: ResolutionAvailability::Available,
        walk_incomplete: None,
        rungs: None,
        dead_clusters: None,
        dead_clusters_truncated: 0,
        dead_clusters_incomplete: None,
    }
}

fn atomic_budget_take<T, F>(items: Vec<T>, token_budget: u32, cost_of: F) -> Response<T>
where
    F: Fn(&T) -> u32,
{
    let total = u32::try_from(items.len()).unwrap_or(u32::MAX);
    let required = items
        .iter()
        .fold(0u32, |sum, item| sum.saturating_add(cost_of(item)));
    if required > token_budget {
        return Response {
            source_freshness: None,
            items: Vec::new(),
            shown: 0,
            hidden: total,
            total,
            truncated: total > 0,
            tokens_used: 0,
            resolution: ResolutionAvailability::Available,
            walk_incomplete: None,
            rungs: None,
            dead_clusters: None,
            dead_clusters_truncated: 0,
            dead_clusters_incomplete: None,
        };
    }
    Response {
        source_freshness: None,
        shown: total,
        hidden: 0,
        total,
        truncated: false,
        tokens_used: required,
        items,
        resolution: ResolutionAvailability::Available,
        walk_incomplete: None,
        rungs: None,
        dead_clusters: None,
        dead_clusters_truncated: 0,
        dead_clusters_incomplete: None,
    }
}

#[cfg(all(test, feature = "parse"))]
mod tests {
    use super::*;
    use devmap_extract::extract_file;
    use devmap_resolve::Resolver;

    /// Every `EdgeKind`, so a variant added to the enum cannot slip past the
    /// two spellings below without failing here.
    ///
    /// `edge_kind_name`'s own `match` has no wildcard arm, so a new variant is
    /// a compile error there; this array is what stops a new variant from being
    /// *added to the enum and to that match* while going untested. The length
    /// assertion below is what stops the array itself from silently shrinking.
    const ALL_EDGE_KINDS: [EdgeKind; 14] = [
        EdgeKind::Imports,
        EdgeKind::Calls,
        EdgeKind::Contains,
        EdgeKind::Defines,
        EdgeKind::Instantiates,
        EdgeKind::Extends,
        EdgeKind::Implements,
        EdgeKind::SubscribesTo,
        EdgeKind::HandlesRoute,
        EdgeKind::WiredTo,
        EdgeKind::MemberOf,
        EdgeKind::DependsOn,
        EdgeKind::TaintFlow,
        EdgeKind::References,
    ];

    /// `edge_kind_name` must spell a kind exactly as `traverse_graph` records
    /// it, and exactly as `stored_edge_to_resolved` parses it back.
    ///
    /// This is load-bearing, not cosmetic. `traversed_resolution_edges` selects
    /// the walk's edges out of the generation by comparing
    /// `edge_kind_name(kind)` against the `format!("{:?}", kind)` string
    /// `traverse_graph` put in `EdgeIdentity::edge_kind`. If those two ever
    /// disagree for one variant, every edge of that kind silently fails the
    /// membership test and `impact` reports "no callers" — a positive claim
    /// produced by a comparison that never matched, which is exactly the
    /// failure mode this codebase treats as worse than an empty answer. There
    /// is no output to observe it in: the answer is well-formed and wrong.
    ///
    /// Three spellings are tied together here — the `Debug` derive, the
    /// `edge_kind_name` table, and the store's string parser — so no two of
    /// them can drift apart unnoticed.
    #[test]
    fn edge_kind_name_is_the_spelling_traverse_graph_records() {
        let mut seen = std::collections::BTreeSet::new();
        for kind in ALL_EDGE_KINDS {
            let name = edge_kind_name(kind);
            assert_eq!(
                name,
                format!("{kind:?}"),
                "edge_kind_name disagrees with the Debug spelling traverse_graph \
                 records in EdgeIdentity::edge_kind; traversed_resolution_edges \
                 would drop every {kind:?} edge and report an empty blast radius"
            );
            let round_tripped = stored_edge_to_resolved(StoredEdge {
                source_file: "a.py".to_string(),
                target_file: "b.py".to_string(),
                source_symbol: "a.py::from".to_string(),
                target_symbol: "b.py::to".to_string(),
                edge_kind: name.to_string(),
                confidence: 1.0,
                resolution: None,
            })
            .expect("the store must parse back the name this table emits");
            assert_eq!(
                round_tripped.edge_kind, kind,
                "the store parses {name:?} as a different kind than it names"
            );
            assert!(seen.insert(name), "two kinds share the name {name:?}");
        }
        assert_eq!(
            seen.len(),
            ALL_EDGE_KINDS.len(),
            "the kind table must hold one distinct name per variant"
        );
        assert_eq!(ALL_EDGE_KINDS.len(), 14, "a variant was added or removed");
    }

    /// A symbol too large for the budget is returned capped, and says so.
    ///
    /// The whole point: a hit costs `len / 4 + 20` tokens against a 2,000-token
    /// default, so one 8 KB function was unreturnable — the packer dropped it
    /// and `DevMapClient._budgeted` would reject it even if the packer had not.
    /// The cap must leave the hit inside the budget *and* record what it
    /// dropped, because `source_span` is contractually the verbatim body.
    #[test]
    fn an_oversized_source_span_is_capped_within_budget_and_reports_the_omission() {
        let budget = 2_000u32;
        let huge = "x".repeat(40_000);
        let (capped, omitted) = cap_source_span(huge.clone(), budget);

        let cost = (capped.len() as u32) / BYTES_PER_TOKEN + SEARCH_HIT_OVERHEAD_TOKENS;
        assert!(
            cost <= budget,
            "capped hit still costs {cost} tokens against a {budget} budget"
        );
        let omitted = omitted.expect("a capped span must report what it dropped");
        assert_eq!(
            capped.len() as u32 + omitted,
            huge.len() as u32,
            "kept + omitted must account for every byte of the original"
        );
    }

    /// A span that already fits is returned untouched and unmarked.
    ///
    /// `source_span_omitted_bytes` must mean "this was capped" and nothing
    /// else; a `Some(0)` on every hit would make the signal useless.
    #[test]
    fn a_span_within_budget_is_left_verbatim() {
        let small = "def f():\n    return 1\n".to_string();
        let (kept, omitted) = cap_source_span(small.clone(), 2_000);
        assert_eq!(kept, small);
        assert_eq!(omitted, None);
    }

    /// Capping never splits a UTF-8 codepoint.
    ///
    /// `String::truncate` panics on a non-boundary index, so a source file with
    /// non-ASCII in a comment or string literal would crash the query rather
    /// than answer it.
    #[test]
    fn capping_a_span_full_of_multibyte_characters_does_not_panic() {
        // 4-byte codepoints, so most byte offsets are not char boundaries.
        let emoji_source = "🦀".repeat(4_000);
        let (capped, omitted) = cap_source_span(emoji_source.clone(), 200);
        assert!(capped.len() < emoji_source.len());
        assert!(omitted.is_some());
        // Round-trips as valid UTF-8 precisely because it stopped on a boundary.
        assert!(capped.chars().all(|c| c == '🦀'));
    }

    #[test]
    fn search_reports_shown_and_total_when_truncated() {
        let mut src = String::new();
        for i in 0..50 {
            src.push_str(&format!("def fn_{i}():\n    return {i}\n"));
        }
        let ext = extract_file("mod.py", &src);
        let mut resolver = Resolver::new();
        resolver.index_extractions(std::slice::from_ref(&ext));
        let resolution = resolver.resolve_all(std::slice::from_ref(&ext));
        let exts = [ext];
        let engine = QueryEngine::new(&exts, &resolution);
        let resp = engine.search(Request {
            query: "fn_".into(),
            token_budget: 80,
            min_confidence: 0.0,
            max_depth: 1,
        });
        assert!(resp.total > resp.shown);
        assert!(resp.truncated);
        assert_eq!(resp.shown, resp.items.len() as u32);
    }

    #[test]
    fn persisted_engine_answers_without_reextracting_sources() {
        use devmap_analyze::analyze;
        use devmap_store::Store;

        let target = extract_file("missing/target.py", "def target():\n    return 1\n");
        let caller = extract_file(
            "missing/caller.py",
            "from target import target\n\ndef caller():\n    return target()\n",
        );
        let mut resolver = Resolver::new();
        resolver.index_extractions(&[target.clone(), caller.clone()]);
        let resolution = resolver.resolve_all(&[target.clone(), caller.clone()]);
        let analysis = analyze(&[target.clone(), caller.clone()], &resolution);
        let store = Store::open_in_memory().unwrap();
        store
            .save_generation(&[target, caller], &resolution, &analysis)
            .unwrap();

        let engine = StoreQueryEngine::new(&store);
        let search = engine
            .search(Request {
                query: "target".into(),
                token_budget: 2_000,
                min_confidence: 0.0,
                max_depth: 1,
            })
            .unwrap();
        assert!(search
            .items
            .iter()
            .any(|item| item.file_path == "missing/target.py"));
        let target_hit = search
            .items
            .iter()
            .find(|item| item.file_path == "missing/target.py")
            .expect("persisted target hit");
        assert!(target_hit.source_span.is_empty());
        assert!(target_hit.source_unavailable_reason.is_some());

        let deps = engine
            .dependencies(Request {
                query: "missing/caller.py".into(),
                token_budget: 2_000,
                min_confidence: 0.0,
                max_depth: 1,
            })
            .unwrap();
        assert!(matches!(deps.resolution, ResolutionAvailability::Available));
        assert!(deps.items.iter().any(|edge| {
            edge.source_file == "missing/caller.py" && edge.target_file == "missing/target.py"
        }));
    }

    #[test]
    fn persisted_engine_preserves_unavailable_and_dead_states() {
        use devmap_analyze::analyze;
        use devmap_store::Store;

        let ext = extract_file("dead.py", "def abandoned():\n    return 1\n");
        let mut resolver = Resolver::new();
        resolver.index_extractions(std::slice::from_ref(&ext));
        let resolution = resolver.resolve_all(std::slice::from_ref(&ext));
        let analysis = analyze(std::slice::from_ref(&ext), &resolution);
        let store = Store::open_in_memory().unwrap();
        store
            .save_generation(std::slice::from_ref(&ext), &resolution, &analysis)
            .unwrap();

        let engine = StoreQueryEngine::new(&store);
        let missing = engine
            .dependencies(Request {
                query: "absent.py".into(),
                token_budget: 2_000,
                min_confidence: 0.0,
                max_depth: 1,
            })
            .unwrap();
        assert!(matches!(
            missing.resolution,
            ResolutionAvailability::Unavailable { .. }
        ));
        let dead = engine.dead_symbols(2_000).unwrap();
        assert!(dead
            .items
            .iter()
            .any(|item| item.symbol_name == "abandoned"));
    }

    fn path_edge(source: &str, target: &str, confidence: f32) -> ResolvedEdge {
        ResolvedEdge {
            source_file: format!("{source}.py"),
            target_file: format!("{target}.py"),
            source_symbol: source.to_string(),
            target_symbol: target.to_string(),
            edge_kind: EdgeKind::Calls,
            confidence: Confidence(confidence),
            resolution: None,
            details: None,
            evidence: None,
        }
    }

    /// A depth-capped walk must not be reported as proof that no path exists.
    ///
    /// `shortest_path` returned a bare `Option`, so "the frontier was pruned at
    /// `max_depth`", "the node cap stopped the walk", "the budget was zero" and
    /// "the reachable set really does not contain the target" were one value.
    /// `trace_between` then stated the strongest of those as fact —
    /// `no indexed path from {from} to {to}` — and an agent concluded two
    /// symbols were unrelated when the path was simply longer than `--depth`.
    #[test]
    fn a_depth_capped_trace_is_not_reported_as_proof_that_no_path_exists() {
        // a -> b -> c -> d is three hops; ask for two.
        let chain = vec![
            path_edge("a", "b", 0.9),
            path_edge("b", "c", 0.9),
            path_edge("c", "d", 0.9),
        ];
        match shortest_path(&chain, "a", "d", 2, 5_000, &Cancel::new()).expect("uncancelled") {
            PathSearch::Exhausted { depth_capped, .. } => {
                assert!(depth_capped, "the depth cap is what stopped this walk");
            }
            other => panic!("a depth-capped walk must report Exhausted, got {other:?}"),
        }

        // With the same graph and enough depth, the answer is the path.
        match shortest_path(&chain, "a", "d", 3, 5_000, &Cancel::new()).expect("uncancelled") {
            PathSearch::Found(path) => assert_eq!(path.len(), 3),
            other => panic!("the path is reachable at depth 3, got {other:?}"),
        }

        // A target that genuinely is not in the reachable set is NoPath, and
        // must stay distinguishable from the capped case above.
        let disjoint = vec![path_edge("a", "b", 0.9), path_edge("y", "z", 0.9)];
        match shortest_path(&disjoint, "a", "z", 64, 5_000, &Cancel::new()).expect("uncancelled") {
            PathSearch::NoPath => {}
            other => panic!("an exhausted reachable set is NoPath, got {other:?}"),
        }

        // The node cap is its own reason, not the depth cap's.
        match shortest_path(&chain, "a", "d", 64, 1, &Cancel::new()).expect("uncancelled") {
            PathSearch::Exhausted { node_capped, .. } => {
                assert!(node_capped, "the node cap is what stopped this walk");
            }
            other => panic!("a node-capped walk must report Exhausted, got {other:?}"),
        }
    }

    #[test]
    fn scoped_trace_is_shortest_bounded_and_deterministic() {
        let edges = vec![
            path_edge("a", "b", 0.7),
            path_edge("b", "d", 0.7),
            path_edge("a", "c", 0.9),
            path_edge("c", "d", 0.9),
            path_edge("d", "a", 1.0),
        ];
        let PathSearch::Found(path) =
            shortest_path(&edges, "a", "d", 2, 5_000, &Cancel::new()).expect("uncancelled")
        else {
            panic!("two-hop path");
        };
        assert_eq!(
            path.iter()
                .map(|edge| edge.target_symbol.as_str())
                .collect::<Vec<_>>(),
            ["c", "d"]
        );
        assert!(
            matches!(
                shortest_path(&edges, "a", "d", 1, 5_000, &Cancel::new()).expect("uncancelled"),
                PathSearch::Exhausted {
                    depth_capped: true,
                    ..
                }
            ),
            "depth 1 cannot reach a two-hop target, and that is a cap, not a fact \
             about the graph"
        );

        let mut with_direct = edges;
        with_direct.push(path_edge("a", "d", 0.1));
        let PathSearch::Found(path) =
            shortest_path(&with_direct, "a", "d", 2, 5_000, &Cancel::new()).expect("uncancelled")
        else {
            panic!("direct path");
        };
        assert_eq!(
            path.len(),
            1,
            "edge count, not confidence, defines shortest"
        );
        assert_eq!(path[0].target_symbol, "d");
    }

    /// A scoped trace over a real-sized graph must finish in a moment.
    ///
    /// The search rescanned *every* edge for every node it dequeued and cloned
    /// the whole path vector once per edge, so the cost was
    /// `frontier × edges`. This fixture is shaped to reach both production
    /// bounds at once — 64 levels deep (`max_depth`) and just under the
    /// 5,000-node frontier (`max_nodes`) — across ~20,000 edges, which is
    /// 4,900 × 19,656 ≈ 96 million string comparisons. Measured pre-fix: 5.9 s.
    /// An adjacency index built once, plus parent pointers instead of path
    /// clones, makes the walk linear in the edges: ~40 ms.
    ///
    /// One second is deliberately loose — it must not flake on a loaded machine
    /// — and still an order of magnitude below the behaviour it guards against.
    #[test]
    fn a_scoped_trace_over_twenty_thousand_edges_finishes_promptly() {
        const LEVELS: usize = 64;
        const WIDTH: usize = 78;
        const FANOUT: usize = 4;

        // A layered DAG: every node points at four nodes one level down. Wide
        // enough to fill the frontier, shallow enough that the depth bound
        // never cuts the walk short, and acyclic so the node count is exact.
        let mut edges = Vec::with_capacity((LEVELS - 1) * WIDTH * FANOUT);
        for level in 0..LEVELS - 1 {
            for column in 0..WIDTH {
                for step in 0..FANOUT {
                    let next = (column * FANOUT + step) % WIDTH;
                    edges.push(path_edge(
                        &format!("L{level:02}W{column:03}"),
                        &format!("L{:02}W{next:03}", level + 1),
                        0.9,
                    ));
                }
            }
        }
        assert_eq!(edges.len(), (LEVELS - 1) * WIDTH * FANOUT);

        // A destination no node carries, so the search must exhaust the
        // frontier instead of returning on an early hit.
        let started = std::time::Instant::now();
        let found = shortest_path(
            &edges,
            "L00W000",
            "absent_destination",
            LEVELS,
            5_000,
            &Cancel::new(),
        )
        .expect("uncancelled");
        let elapsed = started.elapsed();

        assert!(
            !matches!(found, PathSearch::Found(_)),
            "the destination is not in the graph, got {found:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "exhausting a {}-edge graph took {elapsed:?}; the search is rescanning \
             every edge per dequeued node",
            edges.len()
        );
    }

    /// The index must not change which path is returned. Same fixture as
    /// `scoped_trace_is_shortest_bounded_and_deterministic`, asserted across
    /// repeated runs so a map iteration order could not make it wobble.
    #[test]
    fn the_scoped_trace_path_is_identical_across_runs() {
        let edges = vec![
            path_edge("a", "b", 0.7),
            path_edge("b", "d", 0.7),
            path_edge("a", "c", 0.9),
            path_edge("c", "d", 0.9),
            path_edge("d", "a", 1.0),
        ];
        let PathSearch::Found(first) =
            shortest_path(&edges, "a", "d", 2, 5_000, &Cancel::new()).expect("uncancelled")
        else {
            panic!("two-hop path");
        };
        for _ in 0..8 {
            let PathSearch::Found(again) =
                shortest_path(&edges, "a", "d", 2, 5_000, &Cancel::new()).expect("uncancelled")
            else {
                panic!("two-hop path");
            };
            assert_eq!(
                again
                    .iter()
                    .map(|edge| (edge.source_symbol.clone(), edge.target_symbol.clone()))
                    .collect::<Vec<_>>(),
                first
                    .iter()
                    .map(|edge| (edge.source_symbol.clone(), edge.target_symbol.clone()))
                    .collect::<Vec<_>>(),
                "the scoped trace answered differently on a repeat run"
            );
        }
    }

    /// Semantic search must not read the whole corpus to answer with a page of
    /// it.
    ///
    /// `explore` opens one file per definition it returns, not per candidate.
    ///
    /// The candidate pool is `budget_page_size(definition_budget)` rows — 100
    /// at the default 8,000-token budget — and a `SymbolHit` is what reads a
    /// file. Materialising the pool and cutting it to `limit` afterwards would
    /// open a hundred files to keep three, which is the amplification
    /// `search_semantic` was repaired for. The ranking runs on the stored rows,
    /// which already carry both names it scores.
    #[test]
    fn explore_reads_one_file_per_definition_it_returns_not_per_candidate() {
        use devmap_analyze::analyze;
        use devmap_store::Store;

        const SYMBOLS: usize = 200;
        const LIMIT: usize = 3;

        let mut source = String::new();
        for index in 0..SYMBOLS {
            source.push_str(&format!("def widget_{index:04}():\n    return {index}\n"));
        }
        let ext = extract_file("things.py", &source);
        let mut resolver = Resolver::new();
        resolver.index_extractions(std::slice::from_ref(&ext));
        let resolution = resolver.resolve_all(std::slice::from_ref(&ext));
        let analysis = analyze(std::slice::from_ref(&ext), &resolution);
        let store = Store::open_in_memory().unwrap();
        store
            .save_generation(std::slice::from_ref(&ext), &resolution, &analysis)
            .unwrap();

        SOURCE_SPAN_READS.with(|reads| reads.set(0));
        let report = StoreQueryEngine::new(&store)
            .explore("widget", LIMIT, 8_000, 0.0, 1)
            .expect("explore");
        let reads = SOURCE_SPAN_READS.with(|reads| reads.get());

        assert_eq!(
            report.definitions.total, SYMBOLS as u32,
            "every match must still be counted in `total`"
        );
        assert!(
            report.definitions.shown <= LIMIT as u32,
            "the limit must bound what is returned, got {}",
            report.definitions.shown
        );
        assert!(
            reads <= LIMIT,
            "explore opened {reads} files to return {} definitions",
            report.definitions.shown
        );
    }

    /// Every scored symbol was materialised into a `SymbolHit` — one
    /// `read_to_string` each — and only then handed to the budget, so a query
    /// matching a common term opened every file it matched in order to throw
    /// almost all of them away. Scoring already yields a ranked list, so the
    /// bound is the same page size keyword search uses: a hit costs at least
    /// `SEARCH_HIT_OVERHEAD_TOKENS`, so no more than `budget / overhead` of them
    /// can ever be shown.
    #[test]
    fn semantic_search_reads_only_as_many_files_as_the_budget_could_show() {
        use devmap_analyze::analyze;
        use devmap_store::Store;

        const SYMBOLS: usize = 500;
        const BUDGET: u32 = 200;

        let mut source = String::new();
        for index in 0..SYMBOLS {
            // `widget_NNNN` tokenizes to `widget` + `NNNN`, so every symbol
            // shares the query's single term and the whole corpus scores.
            source.push_str(&format!("def widget_{index:04}():\n    return {index}\n"));
        }
        let ext = extract_file("things.py", &source);
        let mut resolver = Resolver::new();
        resolver.index_extractions(std::slice::from_ref(&ext));
        let resolution = resolver.resolve_all(std::slice::from_ref(&ext));
        let analysis = analyze(std::slice::from_ref(&ext), &resolution);
        let store = Store::open_in_memory().unwrap();
        store
            .save_generation(std::slice::from_ref(&ext), &resolution, &analysis)
            .unwrap();

        SOURCE_SPAN_READS.with(|reads| reads.set(0));
        let response = StoreQueryEngine::new(&store)
            .search_semantic("widget", BUDGET)
            .expect("semantic search");
        let reads = SOURCE_SPAN_READS.with(|reads| reads.get());

        assert_eq!(
            response.total, SYMBOLS as u32,
            "every scored symbol must still be counted in `total`"
        );
        assert!(
            response.shown > 0,
            "a {BUDGET}-token budget must show something"
        );
        assert!(
            response.truncated && response.hidden == response.total - response.shown,
            "the withheld matches must be reported: shown={} hidden={} total={}",
            response.shown,
            response.hidden,
            response.total
        );

        let ceiling = (BUDGET / SEARCH_HIT_OVERHEAD_TOKENS) as usize + 1;
        assert!(
            reads <= ceiling,
            "scored {SYMBOLS} symbols and read {reads} files for a budget that can \
             show at most {ceiling}"
        );
        assert!(
            reads >= response.shown as usize,
            "every shown hit needs its source read: reads={reads} shown={}",
            response.shown
        );
    }

    /// Widening the candidate pool must cost string comparisons, not file
    /// reads.
    ///
    /// Keyword search now draws `SEARCH_RANK_OVERSAMPLE` times the page from
    /// the store so its ranking is not confined to what bm25 liked. That is
    /// only free because ranking runs on the stored row and the cut to
    /// `budget_page_size` happens *before* anything is materialised. Score the
    /// pool after building the hits instead — the obvious refactor — and this
    /// query opens ten times as many files to discard nine tenths of them.
    #[test]
    fn keyword_search_reads_only_as_many_files_as_the_budget_could_show() {
        use devmap_analyze::analyze;
        use devmap_store::Store;

        const SYMBOLS: usize = 500;
        const BUDGET: u32 = 200;

        let mut source = String::new();
        for index in 0..SYMBOLS {
            source.push_str(&format!("def widget_{index:04}():\n    return {index}\n"));
        }
        let ext = extract_file("things.py", &source);
        let mut resolver = Resolver::new();
        resolver.index_extractions(std::slice::from_ref(&ext));
        let resolution = resolver.resolve_all(std::slice::from_ref(&ext));
        let analysis = analyze(std::slice::from_ref(&ext), &resolution);
        let store = Store::open_in_memory().unwrap();
        store
            .save_generation(std::slice::from_ref(&ext), &resolution, &analysis)
            .unwrap();

        let pool = search_rank_pool_size(BUDGET);
        assert!(
            pool > budget_page_size(BUDGET),
            "the point of the pool is that it is wider than the page"
        );

        SOURCE_SPAN_READS.with(|reads| reads.set(0));
        let response = StoreQueryEngine::new(&store)
            .search(Request {
                query: "widget".to_string(),
                token_budget: BUDGET,
                min_confidence: 0.0,
                max_depth: 1,
            })
            .expect("keyword search");
        let reads = SOURCE_SPAN_READS.with(|reads| reads.get());

        assert_eq!(response.total, SYMBOLS as u32);
        assert!(response.shown > 0);
        assert!(
            reads <= budget_page_size(BUDGET),
            "ranked a pool of {pool} and read {reads} files for a page of {}",
            budget_page_size(BUDGET)
        );
        assert!(
            reads >= response.shown as usize,
            "every shown hit needs its source read: reads={reads} shown={}",
            response.shown
        );
    }

    #[test]
    fn scoped_trace_budget_never_returns_a_misleading_prefix() {
        let path = vec![path_edge("a", "b", 1.0), path_edge("b", "c", 1.0)];
        let response = atomic_budget_take(path, 25, |_| 25);
        assert!(response.items.is_empty());
        assert_eq!(response.total, 2);
        assert_eq!(response.hidden, 2);
        assert!(response.truncated);
        assert_eq!(response.tokens_used, 0);
    }
}

#[cfg(test)]
mod indexed_start_equivalence_tests {
    use super::*;
    use devmap_extract::model::EdgeKind;
    use devmap_store::{GenerationEdges, StoredEdge};

    fn stored(
        source_symbol: &str,
        source_file: &str,
        target_symbol: &str,
        target_file: &str,
        kind: EdgeKind,
        confidence: f32,
    ) -> StoredEdge {
        StoredEdge {
            source_file: source_file.to_string(),
            target_file: target_file.to_string(),
            source_symbol: source_symbol.to_string(),
            target_symbol: target_symbol.to_string(),
            edge_kind: edge_kind_name(kind).to_string(),
            confidence,
            resolution: None,
        }
    }

    /// The generation used by every case below.
    ///
    /// Deliberately awkward: duplicate edges, a self-edge, two files that share
    /// a basename, a symbol whose tail collides with another's method, a
    /// file-shaped node id, and confidences that straddle the store's rounding
    /// boundary. Sorted the way the store sorts a generation, because the order
    /// is part of what the index must reproduce.
    fn rows() -> Vec<StoredEdge> {
        let mut rows = vec![
            stored(
                "a/b/c.go::Run",
                "a/b/c.go",
                "hub",
                "hub.go",
                EdgeKind::Calls,
                1.0,
            ),
            stored(
                "a/b/c.go::Run",
                "a/b/c.go",
                "hub",
                "hub.go",
                EdgeKind::Calls,
                1.0,
            ),
            stored(
                "x/c.go::Run",
                "x/c.go",
                "hub",
                "hub.go",
                EdgeKind::Calls,
                0.9,
            ),
            stored("hub", "hub.go", "hub", "hub.go", EdgeKind::Calls, 0.8),
            stored(
                "p.py::T.run",
                "p.py",
                "hub",
                "hub.go",
                EdgeKind::Calls,
                0.7495,
            ),
            stored("p.py::run", "p.py", "hub", "hub.go", EdgeKind::Calls, 0.75),
            stored(
                "a/b/c.go",
                "a/b/c.go",
                "a/b/c.go::Run",
                "a/b/c.go",
                EdgeKind::Contains,
                1.0,
            ),
            stored(
                "q.py::only",
                "q.py",
                "q.py::only",
                "q.py",
                EdgeKind::Calls,
                0.2,
            ),
        ];
        rows.sort_by(|left, right| {
            right
                .confidence
                .total_cmp(&left.confidence)
                .then_with(|| left.source_file.cmp(&right.source_file))
                .then_with(|| left.target_file.cmp(&right.target_file))
                .then_with(|| left.source_symbol.cmp(&right.source_symbol))
                .then_with(|| left.target_symbol.cmp(&right.target_symbol))
                .then_with(|| left.edge_kind.cmp(&right.edge_kind))
        });
        rows
    }

    fn resolved(rows: &[StoredEdge], min_confidence: f32) -> Vec<ResolvedEdge> {
        rows.iter()
            .filter(|row| {
                (row.confidence * 1000.0).round() as i64 >= (min_confidence * 1000.0).round() as i64
            })
            .map(|row| stored_edge_to_resolved(row.clone()).expect("kind"))
            .collect()
    }

    /// Every query shape, both directions, several floors: the index must
    /// return exactly what the scan returned, in the same order, duplicates
    /// included.
    ///
    /// Duplicates are the case worth stating: `traverse_indexed` derives
    /// `starts_dropped` from the *raw* start list, so an index that helpfully
    /// deduplicated would change `walk_incomplete` on a capped walk without
    /// changing anything a smaller test would look at.
    #[test]
    fn the_indexed_starts_are_the_scan_s_starts() {
        let rows = rows();
        let index = GenerationEdges::build(std::sync::Arc::new(rows.clone()), None).expect("index");
        let cancel = Cancel::new();
        let queries = [
            "hub",
            "Run",
            "c.go",
            "a/b/c.go",
            "x/c.go",
            "a/b/c.go::Run",
            "c.go::Run",
            "p.py::run",
            "run",
            "T.run",
            "only",
            "q.py",
            "",
            "   ",
            "::",
            "nothing_here",
            "no/such.go",
        ];
        for query in queries {
            for reverse in [false, true] {
                for floor in [0.0f32, 0.2, 0.75, 0.9, 1.0, -1.0, 2.0] {
                    let edges = resolved(&rows, floor);
                    let expected = traversal_starts(&edges, query.trim(), reverse);
                    let actual =
                        indexed_traversal_starts(&index, query.trim(), reverse, floor, &cancel)
                            .expect("indexed starts");
                    assert_eq!(
                        actual, expected,
                        "query {query:?} reverse={reverse} floor={floor}"
                    );
                }
            }
        }
    }

    /// The same for the edges a walk reports: same set, same order.
    #[test]
    fn the_indexed_traversed_edges_are_the_scan_s_traversed_edges() {
        let rows = rows();
        let index = GenerationEdges::build(std::sync::Arc::new(rows.clone()), None).expect("index");
        let cancel = Cancel::new();
        for query in ["hub", "Run", "a/b/c.go", "only"] {
            for reverse in [false, true] {
                for floor in [0.0f32, 0.75, 0.9] {
                    for max_depth in [1usize, 3, 64] {
                        let edges = resolved(&rows, floor);
                        let start: Vec<String> = traversal_starts(&edges, query, reverse)
                            .into_iter()
                            .map(|(symbol, _)| symbol)
                            .collect();
                        if start.is_empty() {
                            continue;
                        }
                        let opts = TraversalOptions {
                            max_depth,
                            max_nodes: TRAVERSAL_MAX_NODES,
                            reverse,
                        };
                        let scanned =
                            devmap_analyze::traversal::traverse_graph(&start, &edges, &opts);
                        let walked = traverse_graph_indexed(
                            &start,
                            &index.directed(reverse, floor),
                            opts.limits(),
                        );
                        assert_eq!(
                            scanned.visited_nodes, walked.visited_nodes,
                            "{query:?} reverse={reverse} floor={floor} depth={max_depth}"
                        );
                        assert_eq!(scanned.traversed_edges, walked.traversed_edges);
                        assert_eq!(scanned.stop, walked.stop);
                        assert_eq!(scanned.max_depth_reached, walked.max_depth_reached);

                        let expected = traversed_resolution_edges(&scanned, &edges, floor);
                        let actual =
                            indexed_traversed_edges(&index, &walked, floor, &cancel).expect("ok");
                        assert_eq!(
                            actual.len(),
                            expected.len(),
                            "{query:?} reverse={reverse} floor={floor} depth={max_depth}"
                        );
                        for (left, right) in actual.iter().zip(expected.iter()) {
                            assert_eq!(left.source_symbol, right.source_symbol);
                            assert_eq!(left.target_symbol, right.target_symbol);
                            assert_eq!(left.source_file, right.source_file);
                            assert_eq!(left.target_file, right.target_file);
                            assert_eq!(left.edge_kind, right.edge_kind);
                            assert_eq!(left.confidence.0, right.confidence.0);
                        }
                    }
                }
            }
        }
    }

    /// The two confidence tests are not one test.
    ///
    /// `admits` rounds and decides what the walk may cross; the plain compare
    /// decides what the answer may contain. 0.7495 passes the first at a floor
    /// of 0.75 and fails the second, and that asymmetry was in the code this
    /// replaced — an index that applied only one of them would answer with an
    /// edge the scan withheld.
    #[test]
    fn an_edge_on_the_rounding_boundary_is_crossed_but_not_reported() {
        let rows = rows();
        let index = GenerationEdges::build(std::sync::Arc::new(rows.clone()), None).expect("index");
        let boundary = (0..index.len() as u32)
            .find(|id| index.confidence(*id) == 0.7495)
            .expect("fixture holds the boundary edge");
        assert!(
            index.admits(boundary, 0.75),
            "0.7495 rounds to 750 and must be admitted, as the store admits it"
        );
        assert!(
            index.confidence(boundary) < 0.75,
            "and must still fail the plain compare the answer applies"
        );
    }

    /// A confidence no comparison can evaluate is refused, not answered.
    ///
    /// Over an *empty* store deliberately: the refusal has to come before the
    /// generation lookup, or a caller who asked an unanswerable question is
    /// told about the store instead. That ordering was free while every
    /// traversal went through `latest_edges`, which validated first; the index
    /// path has to state it.
    #[test]
    fn a_nan_floor_is_refused_by_every_indexed_surface() {
        let store = Store::open_in_memory().expect("store");
        let engine = StoreQueryEngine::new(&store);
        for request in [
            Request {
                query: "hub".to_string(),
                token_budget: 2_000,
                min_confidence: f32::NAN,
                max_depth: 3,
            },
            Request {
                query: "hub".to_string(),
                token_budget: 2_000,
                min_confidence: f32::NAN,
                max_depth: 1,
            },
        ] {
            assert!(
                engine.impact(request.clone()).is_err(),
                "NaN was answered rather than refused"
            );
        }
        assert!(engine
            .affected_tests(&["hub".to_string()], 2_000, f32::NAN, 3)
            .is_err());
        assert!(engine.explore("hub", 5, 2_000, f32::NAN, 3).is_err());
    }

    /// A self-edge and a duplicate must not turn a bounded walk into a loop.
    #[test]
    fn cycles_self_edges_and_duplicates_terminate() {
        let rows = vec![
            stored("a", "a.py", "b", "b.py", EdgeKind::Calls, 1.0),
            stored("b", "b.py", "c", "c.py", EdgeKind::Calls, 1.0),
            stored("c", "c.py", "a", "a.py", EdgeKind::Calls, 1.0),
            stored("a", "a.py", "a", "a.py", EdgeKind::Calls, 1.0),
            stored("a", "a.py", "b", "b.py", EdgeKind::Calls, 1.0),
        ];
        let index = GenerationEdges::build(std::sync::Arc::new(rows), None).expect("index");
        for reverse in [false, true] {
            let walk = traverse_graph_indexed(
                &["a".to_string()],
                &index.directed(reverse, 0.0),
                TraversalLimits {
                    max_depth: 64,
                    max_nodes: TRAVERSAL_MAX_NODES,
                },
            );
            assert_eq!(walk.visited_nodes.len(), 3, "reverse={reverse}");
            assert!(!walk.stop.is_incomplete(), "a cycle is not a cap");
        }
    }

    /// A walk that stops exactly at its node cap says so.
    #[test]
    fn a_walk_at_the_node_cap_reports_the_cap_rather_than_a_small_graph() {
        let rows: Vec<StoredEdge> = (0..TRAVERSAL_MAX_NODES + 200)
            .map(|i| {
                stored(
                    &format!("n{i}"),
                    "chain.py",
                    &format!("n{}", i + 1),
                    "chain.py",
                    EdgeKind::Calls,
                    1.0,
                )
            })
            .collect();
        let index = GenerationEdges::build(std::sync::Arc::new(rows), None).expect("index");
        let walk = traverse_graph_indexed(
            &["n0".to_string()],
            &index.directed(false, 0.0),
            TraversalLimits {
                max_depth: 64,
                max_nodes: TRAVERSAL_MAX_NODES,
            },
        );
        assert!(
            walk.stop.is_incomplete(),
            "a walk stopped by depth 64 over a 5,200-long chain is a lower bound"
        );
        let reason = walk
            .stop
            .reason(64, TRAVERSAL_MAX_NODES)
            .expect("an incomplete walk needs a reason");
        assert!(reason.contains("lower bound"), "{reason}");
    }

    #[test]
    fn affected_seed_expansion_obeys_the_same_node_bound_as_its_frontier() {
        let rows = (0..TRAVERSAL_MAX_NODES + 10)
            .map(|i| {
                stored(
                    "caller",
                    "caller.py",
                    &format!("wide.py::seed{i}"),
                    "wide.py",
                    EdgeKind::Calls,
                    1.0,
                )
            })
            .collect();
        let index = GenerationEdges::build(std::sync::Arc::new(rows), None).unwrap();
        let store = Store::open_in_memory().unwrap();
        let walk = StoreQueryEngine::new(&store)
            .blast_walk(&index, &["wide.py".into()], 0, 0.0)
            .unwrap();
        assert_eq!(walk.seeds.len(), TRAVERSAL_MAX_NODES);
        assert_eq!(walk.stop.starts_dropped, 10);
        assert!(walk
            .incomplete_reason()
            .unwrap()
            .contains("start nodes dropped"));
    }

    #[test]
    fn affected_cancellation_is_checked_inside_a_single_large_band() {
        let rows = (0..10_000)
            .map(|i| {
                stored(
                    &format!("caller{i}"),
                    "caller.py",
                    "hub",
                    "hub.py",
                    EdgeKind::Calls,
                    1.0,
                )
            })
            .collect();
        let index = GenerationEdges::build(std::sync::Arc::new(rows), None).unwrap();
        let store = Store::open_in_memory().unwrap();
        let cancel = Cancel::new();
        let flipper = cancel.clone();
        let checks = std::sync::atomic::AtomicUsize::new(0);
        let cancel = cancel.with_check_probe(move || {
            if checks.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 2 {
                flipper.cancel();
            }
        });
        let outcome = StoreQueryEngine::new(&store)
            .with_cancel(cancel)
            .blast_walk(&index, &["hub".into()], 1, 0.0);
        assert!(
            outcome.is_err(),
            "a 10,000-edge band must consult cancellation more than once"
        );
    }

    /// A cancelled walk stops rather than finishing the fan-out.
    #[test]
    fn a_cancelled_start_lookup_stops_instead_of_scanning_every_symbol() {
        let rows: Vec<StoredEdge> = (0..20_000)
            .map(|i| {
                stored(
                    &format!("s{i}"),
                    "wide.py",
                    "hub",
                    "hub.py",
                    EdgeKind::Calls,
                    1.0,
                )
            })
            .collect();
        let index = GenerationEdges::build(std::sync::Arc::new(rows), None).expect("index");
        let cancel = Cancel::new();
        cancel.cancel();
        assert!(
            indexed_traversal_starts(&index, "s1", false, 0.0, &cancel).is_err(),
            "a cancelled start lookup ran to completion"
        );
    }
}

// Needs the parsing frontend: every case here builds a real generation from
// source. Without `parse` the crate answers questions about a persisted map and
// cannot make one, so these are compiled out rather than left to break the
// `--no-default-features` build.
#[cfg(all(test, feature = "parse"))]
mod search_bounds_tests {
    use super::*;
    use devmap_analyze::analyze;
    use devmap_extract::extract_file;
    use devmap_resolve::Resolver;
    use devmap_store::Store;

    /// A store whose only file is `path`, holding `source`.
    fn store_of(path: &str, source: &str) -> Store {
        store_rooted_at(path, source, None)
    }

    /// The same, recording `repo_root` as the directory the engine resolves a
    /// hit's path against when it reads that hit's span off disk.
    fn store_rooted_at(path: &str, source: &str, repo_root: Option<&str>) -> Store {
        let ext = extract_file(path, source);
        // The fixture's own precondition, checked rather than assumed.
        //
        // `extract_file` is bounded by `DEFAULT_PARSE_BUDGET` — five *wall
        // clock* seconds — and a refusal returns an extraction holding only the
        // File node. Saved unchecked, that is a store with no symbols in it,
        // and every assertion below then reads as a defect in the query engine.
        // This module's huge-file case failed at `shown == 1` on a loaded
        // machine for precisely that reason: its 50 MiB fixture ran 4 s of
        // extraction on an idle machine, went over the budget under load, and
        // the test reported zero hits while measuring nothing at all. A fixture
        // that could not be built must never look like one that was.
        assert!(
            matches!(ext.parse_outcome, ParseOutcome::Clean),
            "the fixture for {path} was not extracted cleanly, so nothing below \
             is a statement about the query engine: {:?}",
            ext.parse_outcome
        );
        let mut resolver = Resolver::new();
        resolver.index_extractions(std::slice::from_ref(&ext));
        let resolution = resolver.resolve_all(std::slice::from_ref(&ext));
        let analysis = analyze(std::slice::from_ref(&ext), &resolution);
        let store = Store::open_in_memory().expect("store");
        store
            .save_generation_with_opts(
                std::slice::from_ref(&ext),
                &resolution,
                &analysis,
                devmap_store::GenerationWriteOpts {
                    repo_root: repo_root.map(str::to_string),
                    ..Default::default()
                },
            )
            .expect("generation");
        store
    }

    #[test]
    fn ra3_a_stored_name_cannot_be_combined_with_edited_source() {
        let dir = std::env::temp_dir().join(format!(
            "devmap-source-coherence-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let old = "def original_name(): return 1\n";
        std::fs::write(dir.join("a.py"), old).unwrap();
        let store = store_rooted_at("a.py", old, Some(&dir.to_string_lossy()));
        let request = || Request {
            query: "original_name".into(),
            token_budget: 2000,
            min_confidence: 0.0,
            max_depth: 1,
        };
        let before = StoreQueryEngine::new(&store).search(request()).unwrap();
        let wire = serde_json::to_value(&before).unwrap();
        assert_eq!(
            wire.get("source_freshness"),
            Some(&serde_json::Value::Null),
            "a query must explicitly disclose that whole-tree freshness was not checked"
        );
        assert!(before.items[0].source_span.contains("original_name"));
        std::fs::write(dir.join("a.py"), "def modified_name(): return 2\n").unwrap();
        let after = StoreQueryEngine::new(&store).search(request()).unwrap();
        assert_eq!(after.shown, 1);
        assert!(
            after.items[0].source_span.is_empty(),
            "stale name paired with current bytes: {:?}",
            after.items
        );
        assert!(
            after.items[0]
                .source_unavailable_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("changed")),
            "{:?}",
            after.items
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A hit near the top of a huge file must not read the whole file.
    ///
    /// K-B1: `hit_from_stored` called `fs::read_to_string`, so the cost of one
    /// search hit was the size of the file it lives in, however far the span
    /// was from the end. On a repository carrying a generated or vendored
    /// bundle that is tens of megabytes of I/O and tens of megabytes resident,
    /// per hit, per query — bounded by nothing.
    ///
    /// What is indexed here is the two-line function; what sits on disk is that
    /// function followed by 50 MiB of filler. The two differ on purpose, and
    /// the difference is the only production shape there is:
    /// `devmap_extract::MAX_SOURCE_BYTES` refuses anything over 1 MiB at
    /// discovery, so a 50 MiB file can carry a stored span *only* from a
    /// generation written while it was smaller — which is exactly the case
    /// `read_verified_source` refuses, the working tree having moved on.
    ///
    /// Handing the filler to the extractor as well, which this fixture used to
    /// do twice over, bought no coverage of the read under test and cost the
    /// test its determinism: 4 s of `extract_file` against a five-second wall
    /// clock budget. Under load from other builds the extraction was refused,
    /// the generation held no `findable_symbol` row, and this test failed at
    /// `shown == 1` — reporting a query defect while asserting nothing about
    /// the bound it exists to guard.
    #[test]
    fn a_hit_near_the_top_of_a_huge_file_reads_a_bounded_prefix() {
        let dir = std::env::temp_dir().join(format!(
            "devmap-hugefile-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("fixture dir");

        // The symbol is in the first 100 bytes; the rest is filler the answer
        // never names, and no part of the index describes.
        const INDEXED: &str = "def findable_symbol():\n    return 1\n";
        const FILLER: usize = 50 * 1024 * 1024;
        let head = INDEXED.len();
        let mut on_disk = String::with_capacity(FILLER + head + 8);
        on_disk.push_str(INDEXED);
        on_disk.push_str("# ");
        while on_disk.len() < FILLER {
            on_disk.push('x');
        }
        on_disk.push('\n');
        std::fs::write(dir.join("huge.py"), &on_disk).expect("write fixture");
        assert!(
            on_disk.len() as u64 > devmap_extract::MAX_SOURCE_BYTES,
            "the file on disk must dwarf the span, or this bounds nothing"
        );

        // The engine resolves spans against the generation's recorded root.
        let store = store_rooted_at("huge.py", INDEXED, Some(&dir.to_string_lossy()));

        SOURCE_SPAN_READS.with(|reads| reads.set(0));
        SOURCE_SPAN_BYTES.with(|bytes| bytes.set(0));
        let response = StoreQueryEngine::new(&store)
            .search(Request {
                query: "findable_symbol".to_string(),
                token_budget: 2_000,
                min_confidence: 0.0,
                max_depth: 1,
            })
            .expect("search");
        let bytes = SOURCE_SPAN_BYTES.with(|bytes| bytes.get());

        assert_eq!(
            response.shown, 1,
            "the fixture must produce exactly one hit"
        );
        assert!(
            response.items[0].source_span.is_empty()
                && response.items[0].source_unavailable_reason.is_some(),
            "changed oversized source must be refused while preserving the hit: {:?}",
            response.items[0]
        );
        assert!(
            bytes < (head as u64) * 8 + 4096,
            "one hit whose span ends at byte {head} read {bytes} bytes of a \
             {FILLER}-byte file"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A cancelled search stops instead of opening every file on its page.
    #[test]
    fn a_cancelled_search_stops_before_materialising_its_page() {
        const SYMBOLS: usize = 4_000;
        let mut source = String::new();
        for index in 0..SYMBOLS {
            source.push_str(&format!("def widget_{index:05}():\n    return {index}\n"));
        }
        let store = store_of("things.py", &source);

        let cancel = Cancel::new();
        cancel.cancel();
        SOURCE_SPAN_READS.with(|reads| reads.set(0));
        let outcome = StoreQueryEngine::new(&store)
            .with_cancel(cancel)
            .search(Request {
                query: "widget".to_string(),
                token_budget: 100_000,
                min_confidence: 0.0,
                max_depth: 1,
            });
        let reads = SOURCE_SPAN_READS.with(|reads| reads.get());

        assert!(
            outcome.is_err(),
            "a cancelled search answered instead of stopping"
        );
        assert!(
            reads < 64,
            "a cancelled search opened {reads} files before noticing"
        );
    }

    /// The number of files one search may open is capped by the page ceiling,
    /// not by whatever token budget the caller asked for.
    ///
    /// K-B1: `budget_page_size(100_000)` is 5,001, and every row on the page is
    /// a file read. The token budget is a *presentation* limit chosen by the
    /// caller; letting it set the number of files opened makes a large budget a
    /// request for thousands of file reads.
    #[test]
    fn a_huge_token_budget_does_not_buy_thousands_of_file_reads() {
        const SYMBOLS: usize = 4_000;
        let mut source = String::new();
        for index in 0..SYMBOLS {
            source.push_str(&format!("def widget_{index:05}():\n    return {index}\n"));
        }
        let store = store_of("things.py", &source);

        SOURCE_SPAN_READS.with(|reads| reads.set(0));
        let response = StoreQueryEngine::new(&store)
            .search(Request {
                query: "widget".to_string(),
                token_budget: 100_000,
                min_confidence: 0.0,
                max_depth: 1,
            })
            .expect("search");
        let reads = SOURCE_SPAN_READS.with(|reads| reads.get());

        assert_eq!(
            response.total, SYMBOLS as u32,
            "the index-wide count must stay honest whatever the page cap"
        );
        assert!(
            reads <= SEARCH_PAGE_MAX,
            "a 100,000-token budget opened {reads} files; the page ceiling is \
             {SEARCH_PAGE_MAX}"
        );
        assert!(
            response.truncated && response.hidden > 0,
            "a capped page must say it was capped: shown={} total={} hidden={}",
            response.shown,
            response.total,
            response.hidden
        );
    }
}

#[cfg(all(test, feature = "parse"))]
mod composition_cancellation_tests {
    use super::*;
    use devmap_extract::extract_file;
    use devmap_resolve::Resolver;
    use devmap_store::GenerationWriteOpts;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    // Cancel at an observed checkpoint, after work has started. A timer based
    // on a cold baseline could fire after a warm query had already completed.
    #[test]
    fn a_cancelled_composition_stops_partway_through_the_fan_out() {
        let store = wide_fixture(16);
        let targets: Vec<_> = (0..MAX_NEIGHBOR_TARGETS)
            .map(|i| format!("mod{i}.py"))
            .collect();
        assert_eq!(
            StoreQueryEngine::new(&store)
                .neighbors(&targets, 200_000, 0.0, 6)
                .unwrap()
                .len(),
            targets.len()
        );
        let cancel = Cancel::new();
        let trigger = cancel.clone();
        let checks = Arc::new(AtomicUsize::new(0));
        let observed = checks.clone();
        let cancel = cancel.with_check_probe(move || {
            if observed.fetch_add(1, Ordering::Relaxed) == 2 {
                trigger.cancel();
            }
        });
        let outcome = StoreQueryEngine::new(&store)
            .with_cancel(cancel)
            .neighbors(&targets, 200_000, 0.0, 6);
        assert!(
            outcome.is_err(),
            "an in-flight cancellation must stop the composition"
        );
        assert_eq!(
            checks.load(Ordering::Relaxed),
            3,
            "work must stop at the checkpoint that observes cancellation"
        );
    }
    fn wide_fixture(fan_per_module: usize) -> Store {
        let mut sources: Vec<(String, String)> = Vec::new();
        let mut hub = String::new();
        for index in 0..MAX_NEIGHBOR_TARGETS {
            hub.push_str(&format!(
                "def hub{index}(rows):\n    return sum(rows)\n\n\n"
            ));
        }
        sources.push(("hub.py".to_string(), hub));
        for module in 0..MAX_NEIGHBOR_TARGETS {
            let mut body = String::from("import hub\n\n\n");
            for func in 0..fan_per_module {
                body.push_str(&format!(
                    "def f{module}_{func}(rows):\n    return hub.hub{}(rows)\n\n\n",
                    func % MAX_NEIGHBOR_TARGETS
                ));
            }
            sources.push((format!("mod{module}.py"), body));
        }
        let extractions: Vec<_> = sources
            .iter()
            .map(|(path, body)| extract_file(path, body))
            .collect();
        let mut resolver = Resolver::new();
        resolver.index_extractions(&extractions);
        let resolution = resolver.resolve_all(&extractions);
        let analysis = devmap_analyze::analyze(&extractions, &resolution);
        let store = Store::open_in_memory().unwrap();
        store
            .save_generation_with_opts(
                &extractions,
                &resolution,
                &analysis,
                GenerationWriteOpts::default(),
            )
            .unwrap();
        store
    }
}
