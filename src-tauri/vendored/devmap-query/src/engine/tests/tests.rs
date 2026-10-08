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
const ALL_EDGE_KINDS: [EdgeKind; 15] = [
    EdgeKind::Imports,
    EdgeKind::Calls,
    EdgeKind::Contains,
    EdgeKind::Defines,
    EdgeKind::Instantiates,
    EdgeKind::Extends,
    EdgeKind::Implements,
    EdgeKind::SubscribesTo,
    EdgeKind::HandlesRoute,
    EdgeKind::Registers,
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
    assert_eq!(ALL_EDGE_KINDS.len(), 15, "a variant was added or removed");
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
    let resolution = resolver.resolve_all(std::slice::from_ref(&ext)).unwrap();
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
    let resolution = resolver.resolve_all(std::slice::from_ref(&ext)).unwrap();
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
    let resolution = resolver.resolve_all(std::slice::from_ref(&ext)).unwrap();
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
