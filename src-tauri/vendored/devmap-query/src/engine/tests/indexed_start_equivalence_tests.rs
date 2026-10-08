use super::*;
use devmap_analyze::traversal::TraversalOptions;
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
                    indexed_traversal_starts(&index, query.trim(), reverse, floor, &cancel);
                match actual {
                    Ok(actual) => assert_eq!(
                        actual, expected,
                        "query {query:?} reverse={reverse} floor={floor}"
                    ),
                    Err(err)
                        if matches!(
                            crate::query_match::classify(query.trim()),
                            crate::query_match::StartQuery::Symbol(_)
                        ) && err.to_string().contains("ambiguous") =>
                    {
                        // Bare-name multi-match is refused by the indexed
                        // path; the scan still unions. Confirm the scan
                        // would have produced more than one definition.
                        let unique: BTreeSet<_> = expected.into_iter().collect();
                        assert!(
                            unique.len() > 1,
                            "indexed refused {query:?} as ambiguous, but \
                                 the scan had {} unique start(s): {unique:?}",
                            unique.len()
                        );
                    }
                    Err(err) => panic!(
                        "indexed starts failed for {query:?} reverse={reverse} \
                             floor={floor}: {err}"
                    ),
                }
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
                    let scanned = devmap_analyze::traversal::traverse_graph(&start, &edges, &opts);
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
