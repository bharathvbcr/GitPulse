use super::{
    attribution_coverage_gap, budget_take, byte_span_to_line_range, cap_source_span,
    empty_result_gap, file_edge_coverage_gap, reached_by, search_coverage_gap,
    traversed_resolution_edges, unavailable_response, QueryEngine, RadiusSide, MAX_TRAVERSAL_DEPTH,
};
use crate::model::{Request, ResolutionAvailability, Response, SymbolHit};
use devmap_analyze::traversal::{traverse_graph_indexed, AdjacencyIndex, TraversalOptions};
use devmap_extract::model::{Extraction, ParseOutcome};
use devmap_resolve::model::{ResolutionResult, ResolvedEdge};
use std::collections::BTreeSet;

impl<'a> QueryEngine<'a> {
    pub fn new(extractions: &'a [Extraction], resolution: &'a ResolutionResult) -> Self {
        let rate = devmap_analyze::resolution_rate(extractions, resolution);
        let attribution = devmap_analyze::AttributionCoverage {
            unresolved_sites: rate.unresolved_sites,
            explained_sites: rate.explained_sites,
        };
        Self {
            extractions,
            resolution,
            coverage_gap: devmap_analyze::extraction_coverage(extractions).degraded_reason(),
            attribution_gap: attribution_coverage_gap(
                Some(resolution.unresolved.len()),
                Some(&attribution),
            ),
        }
    }

    pub fn search(&self, req: Request<String>) -> Response<SymbolHit> {
        if req.query.trim().is_empty() {
            return Response {
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
                    source_indent: None,
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
            devmap_analyze::combine_reasons(
                search_coverage_gap(
                    devmap_analyze::extraction_coverage(self.extractions).degraded_reason(),
                ),
                // Same owner, same sentence: an empty answer from this engine
                // means exactly what an empty answer from the store-backed one
                // means, and must say so identically. This engine matches by
                // substring over names rather than through FTS, so its zero has
                // a different *cause* — but the same thing is true of it, which
                // is that it never read a file body.
                empty_result_gap(response.total, &req.query),
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
        let file_sites = self.radius_attribution_gap(
            &BTreeSet::from([(file_path.clone(), file_path.clone())]),
            RadiusSide::FileCallees,
        );
        let coverage_gap = devmap_analyze::combine_reasons(
            file_gap,
            devmap_analyze::combine_reasons(self.coverage_gap.clone(), file_sites),
        );
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
        let starts: BTreeSet<(String, String)> = self
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
            .map(|edge| (edge.target_symbol.clone(), edge.target_file.clone()))
            .collect();
        let start: Vec<String> = starts.iter().map(|(symbol, _)| symbol.clone()).collect();
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
        let radius =
            self.radius_attribution_gap(&reached_by(starts, &inbound), RadiusSide::Callers);
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
            devmap_analyze::combine_reasons(self.coverage_gap.clone(), radius),
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
        let starts: BTreeSet<(String, String)> = self
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
            .map(|edge| (edge.source_symbol.clone(), edge.source_file.clone()))
            .collect();
        let start: Vec<String> = starts.iter().map(|(symbol, _)| symbol.clone()).collect();
        if start.is_empty() {
            let mut response = unavailable_response(ResolutionAvailability::Unavailable {
                reason: format!("{target} has no indexed outbound source"),
            });
            // As the store engine asks: a symbol whose every call went unbound
            // is no outbound start, and those calls are what is missing.
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
            let radius = named
                .and_then(|(side, key)| self.radius_attribution_gap(&BTreeSet::from([key]), side));
            response.walk_incomplete =
                devmap_analyze::combine_reasons(self.coverage_gap.clone(), radius);
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
        let radius =
            self.radius_attribution_gap(&reached_by(starts, &outbound), RadiusSide::Callees);
        let mut response = budget_take(outbound, req.token_budget, |_| 25);
        // Same signal, same reason as `impact` above.
        response.walk_incomplete = devmap_analyze::combine_reasons(
            walk.stop.reason(opts.max_depth, opts.max_nodes),
            devmap_analyze::combine_reasons(self.coverage_gap.clone(), radius),
        );
        response
    }
}
