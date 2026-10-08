use super::{edge_kind_name, edge_node_matches};
use crate::cancel::{Cancel, QueryCancelled};
use devmap_resolve::model::ResolvedEdge;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// One node reached by the scoped-trace walk, and the edge that reached it.
///
/// The walk carries a parent pointer rather than a copy of the path so far.
/// Cloning the whole path once per *edge considered* made the walk quadratic in
/// its own output on top of being quadratic in the graph.
pub(super) struct Reached {
    /// Index into the confidence-ordered edge list.
    edge: usize,
    /// The entry this one extends, or `None` for a first hop.
    parent: Option<usize>,
    node: (String, String),
    /// Number of edges from the origin, i.e. the length of the path to here.
    depth: usize,
}

/// Walk back from `entry` to the origin, producing the path in forward order.
pub(super) fn path_to(
    reached: &[Reached],
    ordered: &[&ResolvedEdge],
    entry: usize,
) -> Vec<ResolvedEdge> {
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
pub(super) fn shortest_path(
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
