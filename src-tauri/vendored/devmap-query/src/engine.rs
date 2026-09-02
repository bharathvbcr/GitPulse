use devmap_analyze::traversal::{traverse_graph, TraversalOptions};
use devmap_extract::model::*;
use devmap_resolve::model::*;
use devmap_store::{Store, StoredEdge};

use std::collections::{BTreeSet, VecDeque};

use crate::model::*;

pub struct QueryEngine<'a> {
    extractions: &'a [Extraction],
    resolution: &'a ResolutionResult,
}

/// Query facade over the latest durable SQLite generation. Unlike
/// `QueryEngine`, this type never extracts or resolves source files.
pub struct StoreQueryEngine<'a> {
    store: &'a Store,
}

impl<'a> StoreQueryEngine<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    pub fn search(&self, req: Request<String>) -> anyhow::Result<Response<SymbolHit>> {
        if self.store.latest_generation_id()?.is_none() {
            return Ok(unavailable_response(ResolutionAvailability::Unavailable {
                reason: "no persisted generation is available".to_string(),
            }));
        }
        if req.query.trim().is_empty() {
            return Ok(budget_take(Vec::new(), req.token_budget, |_| 0));
        }
        let total = self.store.count_search_symbols(&req.query)?;
        let page_size = (req.token_budget / 20).saturating_add(1).max(1) as usize;
        let rows = self.store.search_symbols(&req.query, page_size)?;
        let repo_root = self.store.latest_repo_root()?;
        let query = req.query.to_lowercase();
        let mut hits = Vec::with_capacity(rows.len());
        for row in rows {
            let source_result = std::fs::read_to_string(resolve_source_path(&repo_root, &row.path));
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
            let name = row.name.to_lowercase();
            let qualified = row.qualified_name.to_lowercase();
            let score = if name == query || qualified == query {
                1.0
            } else if name.starts_with(&query) {
                0.95
            } else {
                0.8
            };
            hits.push(SymbolHit {
                symbol_name: row.name,
                file_path: row.path,
                kind: row.kind,
                span,
                source_span,
                source_unavailable_reason,
                score,
            });
        }
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
        response.total = total;
        response.hidden = total.saturating_sub(response.shown);
        response.truncated = response.hidden > 0;
        Ok(response)
    }

    pub fn dependencies(&self, req: Request<String>) -> anyhow::Result<Response<ResolvedEdge>> {
        let Some(file) = self.store.latest_file(&req.query)? else {
            return Ok(unavailable_response(ResolutionAvailability::Unavailable {
                reason: format!("{} is not indexed", req.query),
            }));
        };
        if matches!(file.parse_outcome, ParseOutcome::Failed { .. }) {
            return Ok(unavailable_response(ResolutionAvailability::Unavailable {
                reason: format!("{} could not be parsed", req.query),
            }));
        }
        let rows = self
            .store
            .latest_edges_for_file(&req.query, req.min_confidence)?;
        let edges = rows
            .into_iter()
            .map(stored_edge_to_resolved)
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(budget_take(edges, req.token_budget, |_| 25))
    }

    pub fn impact(&self, req: Request<String>) -> anyhow::Result<Response<ResolvedEdge>> {
        self.traverse(req, true)
    }

    pub fn trace(&self, req: Request<String>) -> anyhow::Result<Response<ResolvedEdge>> {
        self.traverse(req, false)
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
        if self.store.latest_generation_id()?.is_none() {
            return Ok(unavailable_response(ResolutionAvailability::Unavailable {
                reason: "no persisted generation is available".to_string(),
            }));
        }
        let (from, to) = req.query;
        let from = from.trim();
        let to = to.trim();
        if from.is_empty() || to.is_empty() {
            return Ok(unavailable_response(ResolutionAvailability::Unavailable {
                reason: "scoped trace endpoints must not be empty".to_string(),
            }));
        }
        let edges = self
            .store
            .latest_edges(req.min_confidence)?
            .into_iter()
            .map(stored_edge_to_resolved)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let path = shortest_path(&edges, from, to, req.max_depth.min(64), 5_000);
        let Some(path) = path else {
            return Ok(unavailable_response(ResolutionAvailability::Unavailable {
                reason: format!("no indexed path from {from:?} to {to:?}"),
            }));
        };
        Ok(atomic_budget_take(path, req.token_budget, |_| 25))
    }

    fn traverse(
        &self,
        req: Request<String>,
        reverse: bool,
    ) -> anyhow::Result<Response<ResolvedEdge>> {
        let edges = self
            .store
            .latest_edges(req.min_confidence)?
            .into_iter()
            .map(stored_edge_to_resolved)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let target = req.query.trim();
        let start: Vec<String> = edges
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
                    edge.target_symbol.clone()
                } else {
                    edge.source_symbol.clone()
                }
            })
            .collect();
        if start.is_empty() {
            return Ok(unavailable_response(ResolutionAvailability::Unavailable {
                reason: format!("{target} has no indexed traversal start"),
            }));
        }
        let walk = traverse_graph(
            &start,
            &edges,
            &TraversalOptions {
                max_depth: req.max_depth.min(64),
                max_nodes: 5_000,
                reverse,
            },
        );
        let mut traversed = traversed_resolution_edges(&walk, &edges, req.min_confidence);
        traversed.sort_by(|a, b| {
            b.confidence
                .0
                .total_cmp(&a.confidence.0)
                .then_with(|| a.source_file.cmp(&b.source_file))
                .then_with(|| a.target_file.cmp(&b.target_file))
                .then_with(|| a.source_symbol.cmp(&b.source_symbol))
        });
        Ok(budget_take(traversed, req.token_budget, |_| 25))
    }

    pub fn dead_symbols(
        &self,
        token_budget: u32,
    ) -> anyhow::Result<Response<devmap_analyze::DeadSymbolReport>> {
        if self.store.latest_generation_id()?.is_none() {
            return Ok(unavailable_response(ResolutionAvailability::Unavailable {
                reason: "no persisted generation is available".to_string(),
            }));
        }
        let dead = self
            .store
            .latest_dead_symbols()?
            .into_iter()
            .filter(|row| !row.is_exempt)
            .collect();
        Ok(budget_take(dead, token_budget, |_| 30))
    }
}

fn edge_node_matches(file: &str, symbol: &str, query: &str) -> bool {
    crate::query_match::traversal_start_matches(query, symbol, file)
}

fn shortest_path(
    edges: &[ResolvedEdge],
    from: &str,
    to: &str,
    max_depth: usize,
    max_nodes: usize,
) -> Option<Vec<ResolvedEdge>> {
    if max_depth == 0 || max_nodes == 0 {
        return None;
    }
    let mut ordered: Vec<&ResolvedEdge> = edges.iter().collect();
    ordered.sort_by(|a, b| {
        b.confidence
            .0
            .total_cmp(&a.confidence.0)
            .then_with(|| a.source_file.cmp(&b.source_file))
            .then_with(|| a.source_symbol.cmp(&b.source_symbol))
            .then_with(|| a.target_file.cmp(&b.target_file))
            .then_with(|| a.target_symbol.cmp(&b.target_symbol))
            .then_with(|| format!("{:?}", a.edge_kind).cmp(&format!("{:?}", b.edge_kind)))
    });

    let mut queue = VecDeque::new();
    let mut visited = BTreeSet::new();
    for edge in ordered
        .iter()
        .filter(|edge| edge_node_matches(&edge.source_file, &edge.source_symbol, from))
    {
        let node = (edge.target_file.clone(), edge.target_symbol.clone());
        let path = vec![(*edge).clone()];
        if edge_node_matches(&node.0, &node.1, to) {
            return Some(path);
        }
        if visited.len() >= max_nodes {
            break;
        }
        if visited.insert(node.clone()) {
            queue.push_back((node, path));
        }
    }

    while let Some((node, path)) = queue.pop_front() {
        if path.len() >= max_depth {
            continue;
        }
        for edge in ordered
            .iter()
            .filter(|edge| edge.source_file == node.0 && edge.source_symbol == node.1)
        {
            let next = (edge.target_file.clone(), edge.target_symbol.clone());
            let mut candidate = path.clone();
            candidate.push((*edge).clone());
            if edge_node_matches(&next.0, &next.1, to) {
                return Some(candidate);
            }
            if visited.len() >= max_nodes {
                return None;
            }
            if visited.insert(next.clone()) {
                queue.push_back((next, candidate));
            }
        }
    }
    None
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
    let edge_kind = match edge.edge_kind.as_str() {
        "Imports" => EdgeKind::Imports,
        "Calls" => EdgeKind::Calls,
        "Contains" => EdgeKind::Contains,
        "Defines" => EdgeKind::Defines,
        "Instantiates" => EdgeKind::Instantiates,
        "Extends" => EdgeKind::Extends,
        "Implements" => EdgeKind::Implements,
        "SubscribesTo" => EdgeKind::SubscribesTo,
        "HandlesRoute" => EdgeKind::HandlesRoute,
        "WiredTo" => EdgeKind::WiredTo,
        "MemberOf" => EdgeKind::MemberOf,
        "DependsOn" => EdgeKind::DependsOn,
        "TaintFlow" => EdgeKind::TaintFlow,
        "References" => EdgeKind::References,
        other => anyhow::bail!("stored generation has unknown edge kind {other:?}"),
    };
    Ok(ResolvedEdge {
        source_file: edge.source_file,
        target_file: edge.target_file,
        source_symbol: edge.source_symbol,
        target_symbol: edge.target_symbol,
        edge_kind,
        confidence: Confidence(edge.confidence),
        resolution: None,
        details: None,
    })
}

impl<'a> QueryEngine<'a> {
    pub fn new(extractions: &'a [Extraction], resolution: &'a ResolutionResult) -> Self {
        Self {
            extractions,
            resolution,
        }
    }

    pub fn search(&self, req: Request<String>) -> Response<SymbolHit> {
        if req.query.trim().is_empty() {
            return Response {
                items: Vec::new(),
                shown: 0,
                hidden: 0,
                total: 0,
                truncated: false,
                tokens_used: 0,
                resolution: ResolutionAvailability::Available,
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

                hits.push(SymbolHit {
                    symbol_name: sym.name.clone(),
                    file_path: ext.file_path.clone(),
                    kind: format!("{:?}", sym.kind),
                    span: line_span,
                    source_span,
                    source_unavailable_reason,
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

        budget_take(hits, req.token_budget, |hit| {
            u32::try_from(hit.source_span.len() / 4)
                .unwrap_or(u32::MAX)
                .saturating_add(20)
        })
    }

    pub fn dependencies(&self, req: Request<String>) -> Response<ResolvedEdge> {
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

        budget_take(deps, req.token_budget, |_| 25)
    }

    /// Inbound blast radius (impact) with parametric depth (closes G8).
    pub fn impact(&self, req: Request<String>) -> Response<ResolvedEdge> {
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
            return unavailable_response(ResolutionAvailability::Unavailable {
                reason: format!("{target} has no indexed inbound target"),
            });
        }
        let opts = TraversalOptions {
            max_depth: req.max_depth,
            max_nodes: 5000,
            reverse: true,
        };
        let walk = traverse_graph(&start, &self.resolution.edges, &opts);
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
        budget_take(inbound, req.token_budget, |_| 25)
    }

    /// Outbound trace with parametric depth (closes G8).
    pub fn trace(&self, req: Request<String>) -> Response<ResolvedEdge> {
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
            return unavailable_response(ResolutionAvailability::Unavailable {
                reason: format!("{target} has no indexed outbound source"),
            });
        }
        let opts = TraversalOptions {
            max_depth: req.max_depth,
            max_nodes: 5000,
            reverse: false,
        };
        let walk = traverse_graph(&start, &self.resolution.edges, &opts);
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
        budget_take(outbound, req.token_budget, |_| 25)
    }
}

fn traversed_resolution_edges(
    traversal: &devmap_analyze::traversal::TraversalResult,
    edges: &[ResolvedEdge],
    min_confidence: f32,
) -> Vec<ResolvedEdge> {
    let traversed: std::collections::BTreeSet<(String, String, String)> = traversal
        .traversed_edges
        .iter()
        .map(|edge| {
            (
                edge.source.clone(),
                edge.target.clone(),
                edge.edge_kind.clone(),
            )
        })
        .collect();
    edges
        .iter()
        .filter(|edge| {
            edge.confidence.0 >= min_confidence
                && traversed.contains(&(
                    edge.source_symbol.clone(),
                    edge.target_symbol.clone(),
                    format!("{:?}", edge.edge_kind),
                ))
        })
        .cloned()
        .collect()
}

fn unavailable_response<T>(resolution: ResolutionAvailability) -> Response<T> {
    Response {
        items: Vec::new(),
        shown: 0,
        hidden: 0,
        total: 0,
        truncated: false,
        tokens_used: 0,
        resolution,
    }
}

pub(crate) fn byte_span_to_line_range(source: &str, span: &Span) -> (u32, u32) {
    let start = span.start_byte.min(source.len());
    let end = span.end_byte.min(source.len()).max(start);
    let start_line = source[..start]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count() as u32
        + 1;
    let end_line = source[..end].bytes().filter(|byte| *byte == b'\n').count() as u32 + 1;
    (start_line, end_line)
}

pub fn budget_take<T, F>(items: Vec<T>, token_budget: u32, cost_of: F) -> Response<T>
where
    F: Fn(&T) -> u32,
{
    let total = items.len() as u32;
    let mut out = Vec::new();
    let mut current_tokens = 0u32;
    let mut truncated = false;

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
        shown: out.len() as u32,
        hidden: total.saturating_sub(out.len() as u32),
        total,
        truncated,
        tokens_used: current_tokens,
        items: out,
        resolution: ResolutionAvailability::Available,
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
            items: Vec::new(),
            shown: 0,
            hidden: total,
            total,
            truncated: total > 0,
            tokens_used: 0,
            resolution: ResolutionAvailability::Available,
        };
    }
    Response {
        shown: total,
        hidden: 0,
        total,
        truncated: false,
        tokens_used: required,
        items,
        resolution: ResolutionAvailability::Available,
    }
}

#[cfg(all(test, feature = "parse"))]
mod tests {
    use super::*;
    use devmap_extract::extract_file;
    use devmap_resolve::Resolver;

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
        let path = shortest_path(&edges, "a", "d", 2, 5_000).expect("two-hop path");
        assert_eq!(
            path.iter()
                .map(|edge| edge.target_symbol.as_str())
                .collect::<Vec<_>>(),
            ["c", "d"]
        );
        assert!(shortest_path(&edges, "a", "d", 1, 5_000).is_none());

        let mut with_direct = edges;
        with_direct.push(path_edge("a", "d", 0.1));
        let path = shortest_path(&with_direct, "a", "d", 2, 5_000).expect("direct path");
        assert_eq!(
            path.len(),
            1,
            "edge count, not confidence, defines shortest"
        );
        assert_eq!(path[0].target_symbol, "d");
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
