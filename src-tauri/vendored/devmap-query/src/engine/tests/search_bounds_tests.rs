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
    let resolution = resolver.resolve_all(std::slice::from_ref(&ext)).unwrap();
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
    let freshness = wire
        .get("source_freshness")
        .expect("query envelopes must carry source_freshness");
    assert!(
        freshness.get("fresh").is_some_and(|v| v.is_null()),
        "a query must disclose that whole-tree freshness was not verified: {freshness}"
    );
    assert!(
        freshness
            .get("reason")
            .and_then(|v| v.as_str())
            .is_some_and(|reason| !reason.is_empty()),
        "unverified freshness must name why: {freshness}"
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
/// K-B1 again, for the other two surfaces that materialise a page of hits:
/// `ask` (and the evidence pack built on it) and `search_semantic`. Each
/// hit is a verified whole-file read, so the same page ceiling applies.
#[test]
fn ask_and_semantic_search_share_the_page_ceiling() {
    const SYMBOLS: usize = 4_000;
    let mut source = String::new();
    for index in 0..SYMBOLS {
        source.push_str(&format!("def widget_{index:05}():\n    return {index}\n"));
    }
    let store = store_of("things.py", &source);
    let engine = StoreQueryEngine::new(&store);

    // A surface under test: runs one query, reports its `total` and `truncated`.
    type Probe<'a> = dyn Fn() -> (u32, bool) + 'a;
    let reads_for = |run: &Probe| {
        SOURCE_SPAN_READS.with(|reads| reads.set(0));
        let (total, truncated) = run();
        (
            SOURCE_SPAN_READS.with(|reads| reads.get()),
            total,
            truncated,
        )
    };
    let cases: [(&str, &Probe); 3] = [
        ("ask", &|| {
            let r = engine.ask("widget", 100_000, 0.0).expect("ask");
            (r.total, r.truncated)
        }),
        ("ask_evidence", &|| {
            let r = engine
                .ask_evidence("widget", 100_000, 0.0)
                .expect("ask_evidence");
            (r.total, r.truncated)
        }),
        ("search_semantic", &|| {
            let r = engine
                .search_semantic("widget", 100_000)
                .expect("search_semantic");
            (r.total, r.truncated)
        }),
    ];
    for (name, run) in cases {
        let (reads, total, truncated) = reads_for(run);
        assert!(
            reads <= SEARCH_PAGE_MAX,
            "{name}: a 100,000-token budget opened {reads} files; the page \
                 ceiling is {SEARCH_PAGE_MAX}"
        );
        assert!(
            total as usize >= SYMBOLS && truncated,
            "{name}: the count stays index-wide and a capped page says so: \
                 total={total} truncated={truncated}"
        );
    }
}
