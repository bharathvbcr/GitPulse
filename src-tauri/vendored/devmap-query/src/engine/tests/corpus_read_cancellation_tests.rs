//! A query that can read the whole generation observes cancellation before
//! the read, and between the reads it cannot avoid.
//!
//! Each case trips the flag at an observed checkpoint rather than on a timer,
//! so a warm query cannot finish before the flag is set. Where the check has to
//! come *before* a store read, the store is given a corrupted extraction row:
//! a read that ran anyway surfaces as that row's decode error, and only a
//! check that ran first can answer with `QueryCancelled` instead.

use super::*;
use crate::cancel::QueryCancelled;
use devmap_extract::extract_file;
use devmap_resolve::Resolver;
use devmap_store::GenerationWriteOpts;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

const SUM: &str = "def compute(rows, rate):\n    total = 0\n    for row in rows:\n        if row.active:\n            total += row.amount * rate\n        else:\n            total -= row.penalty\n    return total\n";
const SUM_AGAIN: &str = SUM;
const LAST: &str = "def zeta(value):\n    return value\n";

fn scratch(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let dir = std::env::temp_dir().join(format!(
        "devmap-corpus-cancel-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A file-backed store over three small files, so a second connection can
/// reach its rows.
fn fixture(tag: &str) -> (PathBuf, Store) {
    let root = scratch(tag);
    let sources = [("a.py", SUM), ("b.py", SUM_AGAIN), ("z.py", LAST)];
    let extractions: Vec<_> = sources
        .iter()
        .map(|(path, body)| extract_file(path, body))
        .collect();
    let mut resolver = Resolver::new();
    resolver.index_extractions(&extractions);
    let resolution = resolver.resolve_all(&extractions).unwrap();
    let analysis = devmap_analyze::analyze(&extractions, &resolution);
    let store = Store::open(root.join("devmap.sqlite")).unwrap();
    store
        .save_generation_with_opts(
            &extractions,
            &resolution,
            &analysis,
            GenerationWriteOpts::default(),
        )
        .unwrap();
    (root, store)
}

/// Overwrite one file's stored extraction with bytes that do not decode.
fn corrupt_extraction(root: &Path, path: &str) {
    let conn = rusqlite::Connection::open(root.join("devmap.sqlite")).unwrap();
    let changed = conn
        .execute(
            "UPDATE file_payloads SET extraction_json = '{not json'
             WHERE file_id = (SELECT id FROM paths WHERE path = ?1)",
            [path],
        )
        .unwrap();
    assert!(changed > 0, "the fixture holds a payload for {path}");
}

/// A flag that trips on its `trip_at`-th check (1-based), counting every check.
fn cancel_at(trip_at: usize) -> (Cancel, Arc<AtomicUsize>) {
    let cancel = Cancel::new();
    let trigger = cancel.clone();
    let checks = Arc::new(AtomicUsize::new(0));
    let observed = checks.clone();
    let cancel = cancel.with_check_probe(move || {
        if observed.fetch_add(1, Ordering::Relaxed) + 1 == trip_at {
            trigger.cancel();
        }
    });
    (cancel, checks)
}

fn assert_cancelled(outcome: anyhow::Result<impl std::fmt::Debug>, what: &str) {
    let error = outcome.expect_err(what);
    assert!(
        error.downcast_ref::<QueryCancelled>().is_some(),
        "{what}: expected QueryCancelled, got {error:#}"
    );
}

#[test]
fn a_cancelled_ask_stops_before_decoding_the_generation() {
    let (root, store) = fixture("ask");
    // Sanity: the corrupted row is what an uncancelled ask trips on, so the
    // assertion below can only pass if the decode never ran.
    corrupt_extraction(&root, "z.py");
    let uncancelled = StoreQueryEngine::new(&store).ask("total rows", 8_000, 0.0);
    let error = uncancelled.expect_err("the corrupted row is decoded by ask");
    assert!(
        error.downcast_ref::<QueryCancelled>().is_none(),
        "an uncancelled ask must not report cancellation: {error:#}"
    );

    // The entry check passes; the caller gives up on the next one.
    let (cancel, _checks) = cancel_at(2);
    let outcome = StoreQueryEngine::new(&store)
        .with_cancel(cancel)
        .ask("total rows", 8_000, 0.0);
    assert_cancelled(outcome, "a cancelled ask must stop before the decode");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn ask_evidence_shares_the_check_before_the_decode() {
    let (root, store) = fixture("ask-evidence");
    corrupt_extraction(&root, "z.py");
    let (cancel, _checks) = cancel_at(2);
    let outcome = StoreQueryEngine::new(&store)
        .with_cancel(cancel)
        .ask_evidence("total rows", 8_000, 0.0);
    assert_cancelled(
        outcome,
        "a cancelled ask_evidence must stop before the decode",
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_cancelled_graph_core_read_stops_before_and_during_the_decode() {
    let (root, store) = fixture("graph-core");
    corrupt_extraction(&root, "z.py");

    let cancel = Cancel::new();
    cancel.cancel();
    assert_cancelled(
        crate::graph_core_for_store_cancellable(&store, &cancel),
        "a cancelled graph read must stop before the decode",
    );

    // Tripped while the rows are being decoded: a.py decodes, the check before
    // b.py stops it, and z.py — the corrupt row — is never reached.
    let (cancel, checks) = cancel_at(3);
    assert_cancelled(
        crate::graph_core_for_store_cancellable(&store, &cancel),
        "a graph read cancelled mid-decode must stop at the next row",
    );
    assert_eq!(checks.load(Ordering::Relaxed), 3);

    // Uncancelled, the same store reaches the corrupt row: the decode really
    // does run over every row, so stopping early above was the check.
    let error = crate::graph_core_for_store(&store).expect_err("z.py is corrupt");
    assert!(
        error.downcast_ref::<QueryCancelled>().is_none(),
        "{error:#}"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_cancelled_clones_call_reads_no_candidates() {
    let (root, store) = fixture("clones");
    let engine = StoreQueryEngine::new(&store);
    let report = engine.clones(8_000, None, 0).unwrap();
    assert!(
        report.groups.total > 0,
        "the fixture holds a duplicate body: {:?}",
        report.groups
    );

    let cancel = Cancel::new();
    cancel.cancel();
    assert_cancelled(
        StoreQueryEngine::new(&store)
            .with_cancel(cancel)
            .clones(8_000, None, 0),
        "a cancelled clones call must stop before reading candidates",
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_cancelled_dead_symbols_call_reads_no_page() {
    let (root, store) = fixture("dead");
    StoreQueryEngine::new(&store).dead_symbols(8_000).unwrap();

    let cancel = Cancel::new();
    cancel.cancel();
    assert_cancelled(
        StoreQueryEngine::new(&store)
            .with_cancel(cancel)
            .dead_symbols(8_000),
        "a cancelled dead_symbols call must stop before the page",
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn locating_dead_evidence_stops_between_source_reads() {
    let (root, store) = fixture("dead-evidence");
    let row = |path: &str| devmap_analyze::DeadSymbolReport {
        symbol_name: format!("{path}::f"),
        file_path: path.to_string(),
        confidence: 0.4,
        is_exempt: false,
        exemption_reason: None,
        evidence: Some(devmap_analyze::model::DeadEvidence {
            sites: 1,
            first_path: Some(path.to_string()),
            first_byte: Some(0),
            first_line: None,
        }),
    };
    let mut rows = vec![row("a.py"), row("b.py"), row("z.py")];

    // One check before each distinct path: the second stops the walk, so the
    // third path is never read.
    let (cancel, checks) = cancel_at(2);
    let engine = StoreQueryEngine::new(&store).with_cancel(cancel);
    assert_cancelled(
        engine.locate_dead_evidence(&mut rows),
        "a cancelled evidence walk must stop between source reads",
    );
    assert_eq!(checks.load(Ordering::Relaxed), 2);
    std::fs::remove_dir_all(&root).ok();
}
