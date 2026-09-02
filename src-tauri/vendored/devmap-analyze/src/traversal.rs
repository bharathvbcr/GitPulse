//! Shared Graph Traversal Kernel.
//!
//! Closes:
//! - G8: Parametric depth limit.
//! - G21: Enqueued set tracked separately from Visited set to prevent queue re-addition deadlocks.
//! - CodeGraph #536/#774: Impact semantics (no upward contains, instantiates is caller, children fold at same depth).

use devmap_resolve::model::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EdgeIdentity {
    pub source: String,
    pub target: String,
    pub edge_kind: String,
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone)]
pub struct TraversalOptions {
    pub max_depth: usize,
    pub max_nodes: usize,
    pub reverse: bool, // true for impact/callers, false for callee trace
}

impl Default for TraversalOptions {
    fn default() -> Self {
        Self {
            max_depth: 10,
            max_nodes: 1000,
            reverse: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TraversalResult {
    pub visited_nodes: BTreeSet<String>,
    pub traversed_edges: Vec<EdgeIdentity>,
    pub max_depth_reached: usize,
}

pub fn traverse_graph(
    start_nodes: &[String],
    edges: &[ResolvedEdge],
    opts: &TraversalOptions,
) -> TraversalResult {
    let mut adj: BTreeMap<String, Vec<ResolvedEdge>> = BTreeMap::new();
    for edge in edges {
        let key = if opts.reverse {
            edge.target_symbol.clone()
        } else {
            edge.source_symbol.clone()
        };
        adj.entry(key).or_default().push(edge.clone());
    }

    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut enqueued: BTreeSet<String> = BTreeSet::new(); // G21: Separate enqueued tracking
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    let mut traversed_edges = Vec::new();
    let mut max_depth_reached = 0;

    for start in start_nodes.iter().take(opts.max_nodes) {
        if enqueued.insert(start.clone()) {
            queue.push_back((start.clone(), 0));
        }
    }

    while let Some((curr, depth)) = queue.pop_front() {
        if !visited.insert(curr.clone()) {
            continue;
        }
        max_depth_reached = max_depth_reached.max(depth);
        if depth >= opts.max_depth || visited.len() >= opts.max_nodes {
            continue;
        }

        if let Some(neighbors) = adj.get(&curr) {
            // Priority ordering: contains -> calls -> rest
            let mut sorted_neighbors = neighbors.clone();
            sorted_neighbors.sort_by(|a, b| {
                let priority = |edge: &ResolvedEdge| match edge.edge_kind {
                    devmap_extract::model::EdgeKind::Contains
                    | devmap_extract::model::EdgeKind::Defines => 0,
                    devmap_extract::model::EdgeKind::Calls => 1,
                    _ => 2,
                };
                priority(a)
                    .cmp(&priority(b))
                    .then_with(|| a.source_symbol.cmp(&b.source_symbol))
                    .then_with(|| a.target_symbol.cmp(&b.target_symbol))
                    .then_with(|| a.source_file.cmp(&b.source_file))
                    .then_with(|| a.target_file.cmp(&b.target_file))
            });

            for edge in sorted_neighbors {
                // Impact must not walk *upward* through containment. A symbol
                // is contained by its file, so following that edge in reverse
                // reaches the file and from there every sibling symbol in it —
                // turning "what depends on this" into "everything nearby".
                // Both structural kinds are excluded: `Contains` is the kind
                // actually emitted today, `Defines` is kept so a future
                // producer of it cannot silently reopen this hole.
                if opts.reverse
                    && matches!(
                        edge.edge_kind,
                        devmap_extract::model::EdgeKind::Contains
                            | devmap_extract::model::EdgeKind::Defines
                    )
                {
                    continue;
                }
                // File-level topology (package imports, Go package stars) is
                // impact for a *file* query. Following it from a symbol node
                // turns `impact Type.method` into "every importer of this
                // package" — the ScholarLM `segment` flood.
                if opts.reverse
                    && is_symbol_node(&curr)
                    && matches!(
                        edge.edge_kind,
                        devmap_extract::model::EdgeKind::Imports
                            | devmap_extract::model::EdgeKind::MemberOf
                    )
                {
                    continue;
                }
                let next_node = if opts.reverse {
                    edge.source_symbol.clone()
                } else {
                    edge.target_symbol.clone()
                };

                if !enqueued.contains(&next_node) && enqueued.len() >= opts.max_nodes {
                    continue;
                }

                let edge_id = EdgeIdentity {
                    source: edge.source_symbol.clone(),
                    target: edge.target_symbol.clone(),
                    edge_kind: format!("{:?}", edge.edge_kind),
                    line: 0,
                    col: 0,
                };
                if traversed_edges.len() < opts.max_nodes.saturating_sub(1) {
                    traversed_edges.push(edge_id);
                }

                if !enqueued.contains(&next_node) {
                    enqueued.insert(next_node.clone());
                    queue.push_back((next_node, depth + 1));
                }
            }
        }
    }

    TraversalResult {
        visited_nodes: visited,
        traversed_edges,
        max_depth_reached,
    }
}

fn is_symbol_node(id: &str) -> bool {
    id.contains("::")
        || (id.contains('.')
            && !id.contains('/')
            && !id.contains('\\')
            && !matches!(
                id.rsplit('.').next().unwrap_or(""),
                "go" | "py"
                    | "rs"
                    | "ts"
                    | "tsx"
                    | "js"
                    | "jsx"
                    | "c"
                    | "h"
                    | "cc"
                    | "cpp"
                    | "cs"
                    | "java"
                    | "kt"
                    | "swift"
                    | "rb"
                    | "php"
                    | "vue"
                    | "svelte"
            ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use devmap_extract::model::{Confidence, EdgeKind};

    /// `is_symbol_node` decides whether a node id names a symbol or a file, and
    /// every clause of it matters.
    ///
    /// Mutation testing replaced the whole function with `true` and with
    /// `false`, flipped each `&&`, and deleted each `!` — none of it noticed.
    /// This is the guard that stops `impact Type.method` from following
    /// file-level topology upward and returning every importer of the package:
    /// the documented `segment` flood. Always-true suppresses legitimate
    /// file-level impact; always-false reopens the flood.
    #[test]
    fn symbol_nodes_are_distinguished_from_file_nodes() {
        // Qualified identities are symbols.
        assert!(is_symbol_node("pkg/svc.go::Server.handle"));
        assert!(is_symbol_node("app.py::helper"));

        // `Type.method` with no path separator is a symbol.
        assert!(is_symbol_node("Server.handle"));

        // File paths are not symbols, however they are spelled.
        assert!(!is_symbol_node("pkg/svc.go"), "a path is not a symbol");
        assert!(!is_symbol_node("svc.go"), "a bare filename is not a symbol");
        assert!(!is_symbol_node("app.py"));
        assert!(!is_symbol_node("web\\app.tsx"), "Windows separators too");

        // A bare identifier is neither qualified nor dotted.
        assert!(!is_symbol_node("plain"));
    }

    fn edge(source: &str, target: &str, edge_kind: EdgeKind) -> ResolvedEdge {
        ResolvedEdge {
            source_file: "a.go".to_string(),
            target_file: "b.go".to_string(),
            source_symbol: source.to_string(),
            target_symbol: target.to_string(),
            edge_kind,
            confidence: Confidence::DETERMINISTIC,
            resolution: None,
            details: None,
        }
    }

    /// Reverse traversal from a *symbol* must not climb file-level topology.
    ///
    /// Following `Imports`/`MemberOf` upward from a symbol turns
    /// `impact Type.method` into "every importer of this package". The guard
    /// combines `reverse && is_symbol_node && matches!(kind)`; flipping its
    /// `&&` to `||` was undetected, and would suppress forward traversal or
    /// file-level impact entirely.
    #[test]
    fn reverse_impact_from_a_symbol_does_not_follow_package_topology() {
        let edges = vec![
            // A real caller: must be followed.
            edge("a.go::Caller", "b.go::Target.run", EdgeKind::Calls),
            // Package-level topology hanging off the same node: must not be.
            edge("importer.go", "b.go::Target.run", EdgeKind::Imports),
            edge("member.go", "b.go::Target.run", EdgeKind::MemberOf),
        ];
        let opts = TraversalOptions {
            max_depth: 5,
            max_nodes: 100,
            reverse: true,
        };
        let walk = traverse_graph(&["b.go::Target.run".to_string()], &edges, &opts);

        assert!(
            walk.visited_nodes.contains("a.go::Caller"),
            "a real caller must be reached: {:?}",
            walk.visited_nodes
        );
        assert!(
            !walk.visited_nodes.contains("importer.go"),
            "impact on a symbol must not climb to package importers: {:?}",
            walk.visited_nodes
        );
        assert!(
            !walk.visited_nodes.contains("member.go"),
            "impact on a symbol must not climb MemberOf topology: {:?}",
            walk.visited_nodes
        );

        // The same topology IS impact for a *file* query, so the guard must be
        // scoped to symbol starts rather than suppressing the edge kind wholesale.
        let file_walk = traverse_graph(
            &["b.go".to_string()],
            &[edge("importer.go", "b.go", EdgeKind::Imports)],
            &opts,
        );
        assert!(
            file_walk.visited_nodes.contains("importer.go"),
            "file-level impact must still follow imports: {:?}",
            file_walk.visited_nodes
        );
    }

    /// Neighbours are visited in priority order: containment, then calls, then
    /// everything else.
    ///
    /// Deleting either priority arm survived, because nothing observed the
    /// order. It is observable under a cap: with room for only a few recorded
    /// edges, the ones kept must be the high-priority ones, so a caller reading
    /// a truncated impact result still sees structure before incidental
    /// references.
    #[test]
    fn neighbours_are_recorded_in_containment_then_call_priority() {
        // Target names are chosen so alphabetical order DISAGREES with priority
        // order. With names that happen to sort the same way, deleting a
        // priority arm changes nothing and the arm looks untested when it is
        // merely unobservable.
        let edges = vec![
            edge("f.go::start", "f.go::a_ref", EdgeKind::References),
            edge("f.go::start", "f.go::z_call", EdgeKind::Calls),
            edge("f.go::start", "f.go::m_contains", EdgeKind::Contains),
        ];
        let walk = traverse_graph(
            &["f.go::start".to_string()],
            &edges,
            &TraversalOptions {
                max_depth: 1,
                max_nodes: 100,
                reverse: false,
            },
        );
        let order: Vec<&str> = walk
            .traversed_edges
            .iter()
            .map(|e| e.target.as_str())
            .collect();
        assert_eq!(
            order,
            vec!["f.go::m_contains", "f.go::z_call", "f.go::a_ref"],
            "containment must precede calls, which must precede other kinds, \
             regardless of how the target names sort"
        );
    }

    /// The recorded-edge budget is exclusive and leaves room for the node cap.
    ///
    /// `traversed_edges.len() < max_nodes - 1` was mutable to `<=`, recording
    /// one edge more than the budget allows — the kind of off-by-one that only
    /// shows up when a result is already truncated.
    #[test]
    fn recorded_edges_stay_within_their_budget() {
        // Several edges land on the SAME two targets, so edges outnumber nodes
        // and the edge budget actually binds. With one edge per node the node
        // cap stops the walk first and the budget is never reached — which is
        // why an off-by-one here can hide.
        let mut edges = Vec::new();
        for kind in [EdgeKind::Calls, EdgeKind::References, EdgeKind::MemberOf] {
            edges.push(edge("f.go::start", "f.go::a", kind));
            edges.push(edge("f.go::start", "f.go::b", kind));
        }
        let walk = traverse_graph(
            &["f.go::start".to_string()],
            &edges,
            &TraversalOptions {
                max_depth: 3,
                max_nodes: 3,
                reverse: false,
            },
        );
        assert_eq!(
            walk.traversed_edges.len(),
            2,
            "with max_nodes=3 exactly 2 edges may be recorded, found {:?}",
            walk.traversed_edges
        );
    }

    /// Depth and node caps are enforced, and the bound is exclusive.
    #[test]
    fn traversal_respects_its_depth_and_node_caps() {
        let edges: Vec<_> = (0..10)
            .map(|i| {
                edge(
                    &format!("f.go::n{i}"),
                    &format!("f.go::n{}", i + 1),
                    EdgeKind::Calls,
                )
            })
            .collect();

        let shallow = traverse_graph(
            &["f.go::n0".to_string()],
            &edges,
            &TraversalOptions {
                max_depth: 2,
                max_nodes: 100,
                reverse: false,
            },
        );
        assert!(
            shallow.max_depth_reached <= 2,
            "depth cap exceeded: {}",
            shallow.max_depth_reached
        );
        assert!(
            !shallow.visited_nodes.contains("f.go::n5"),
            "a node beyond the depth cap was reached: {:?}",
            shallow.visited_nodes
        );

        let capped = traverse_graph(
            &["f.go::n0".to_string()],
            &edges,
            &TraversalOptions {
                max_depth: 100,
                max_nodes: 3,
                reverse: false,
            },
        );
        assert!(
            capped.visited_nodes.len() <= 3,
            "node cap exceeded: {:?}",
            capped.visited_nodes
        );
    }
}
