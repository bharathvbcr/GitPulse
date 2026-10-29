//! What a generation write stores for each freshly extracted file, and in what
//! order.
//!
//! The payload JSON of a fresh row is produced before the row is inserted, and
//! how it is produced -- one extraction at a time, or a chunk at a time on
//! several threads -- is an implementation choice. These tests pin what must
//! not depend on that choice:
//!
//! * **Characterization.** Every `file_payloads` row holds exactly the three
//!   documents the contract names, the payload ids follow extraction order, and
//!   a carried file does not get a new payload. They were written against the
//!   serial loop and pass on it.
//! * **Determinism.** Two stores written from the same input, under any thread
//!   count and any chunk size, hold identical rows in every table, ids
//!   included. Row ids are assigned by insertion order, so identical tables are
//!   the proof that the insert order did not move.

use super::write::{
    serialize_payload, PayloadDone, PayloadFeed, PayloadJob, PayloadJson, PayloadTuning,
    PayloadWindow,
};
use super::*;
use devmap_analyze::analyze;
use devmap_extract::extract_file;
use devmap_extract::model::Extraction;
use devmap_resolve::Resolver;
use rayon::prelude::*;
use rusqlite::params;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

/// One generation's input: the extractions to write and what is declared
/// about them.
struct Step {
    extractions: Vec<Extraction>,
    opts: GenerationWriteOpts,
}

/// `modules` Python files, one of them syntactically broken so that a
/// `parse_outcome` is more than the default, plus a hub that imports two of
/// them. `revision` changes only the hub and the files named in `edited`.
fn sources(modules: usize, revision: usize, edited: &[usize]) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = (0..modules)
        .map(|index| {
            let bump = if edited.contains(&index) { revision } else { 0 };
            (
                format!("pkg/mod_{index}.py"),
                format!(
                    "VALUE_{index} = 'lit-{index}-{bump}'\n\n\ndef leaf_{index}():\n    return {bump}\n\n\ndef caller_{index}():\n    return leaf_{index}() + nowhere_{index}()\n"
                ),
            )
        })
        .collect();
    files.push((
        "pkg/broken.py".to_string(),
        format!("def broken(:\n    return {revision}\n"),
    ));
    files
}

fn extract_all(files: &[(String, String)]) -> Vec<Extraction> {
    files
        .iter()
        .map(|(path, body)| extract_file(path, body))
        .collect()
}

fn resolve_and_analyze(
    extractions: &[Extraction],
) -> (devmap_resolve::model::ResolutionResult, AnalysisSummary) {
    let mut resolver = Resolver::new();
    resolver.index_extractions(extractions);
    let resolution = resolver.resolve_all(extractions).unwrap();
    let analysis = analyze(extractions, &resolution);
    (resolution, analysis)
}

/// Three generations over `modules` files: a cold write, an edit to one file,
/// and an edit to another together with the deletion of a third.
fn scenario(modules: usize) -> Vec<Step> {
    let cold = extract_all(&sources(modules, 0, &[]));
    let edit = extract_all(&sources(modules, 1, &[0]));
    let mut last_files = sources(modules, 2, &[0, 2.min(modules.saturating_sub(1))]);
    let deleted = "pkg/mod_1.py".to_string();
    last_files.retain(|(path, _)| *path != deleted);
    let last = extract_all(&last_files);
    let mut steps = vec![Step {
        extractions: cold,
        opts: GenerationWriteOpts::default(),
    }];
    if modules > 0 {
        steps.push(Step {
            extractions: edit,
            opts: GenerationWriteOpts {
                affected_paths: vec!["pkg/mod_0.py".to_string(), "pkg/broken.py".to_string()],
                ..GenerationWriteOpts::default()
            },
        });
    }
    if modules > 2 {
        steps.push(Step {
            extractions: last,
            opts: GenerationWriteOpts {
                affected_paths: vec![
                    "pkg/mod_0.py".to_string(),
                    "pkg/mod_2.py".to_string(),
                    "pkg/broken.py".to_string(),
                ],
                deleted_paths: vec![deleted],
                ..GenerationWriteOpts::default()
            },
        });
    }
    steps
}

fn write_step(store: &Store, step: &Step) -> Result<u32> {
    let (resolution, analysis) = resolve_and_analyze(&step.extractions);
    store.save_generation_with_opts(&step.extractions, &resolution, &analysis, step.opts.clone())
}

/// Every row of every table the write path fills, as one sorted list of
/// quoted rows per table.
///
/// Wall-clock columns and the history table are left out: they differ between
/// two runs of the same input by construction. Everything else is compared,
/// the autoincrement ids included, which is the point.
fn dump(store: &Store) -> Vec<(String, Vec<String>)> {
    const SKIPPED_COLUMNS: &[&str] = &["created_at"];
    let conn = lock_conn(&store.conn).expect("connection");
    let tables: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master
              WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
                AND name NOT IN ('build_history', 'extraction_cache', 'extraction_retry',
                                 'extraction_cache_v4', 'pending_state', 'pending_paths')
                AND name NOT LIKE 'nodes_fts\\_%' ESCAPE '\\' OR name = 'nodes_fts_map'
              ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<_>>()
        .unwrap();
    let mut out = Vec::new();
    for table in tables {
        let columns: Vec<String> = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        let columns: Vec<String> = columns
            .into_iter()
            .filter(|column| !SKIPPED_COLUMNS.contains(&column.as_str()))
            .collect();
        let expr = columns
            .iter()
            .map(|column| format!("quote({column})"))
            .collect::<Vec<_>>()
            .join(" || '|' || ");
        let mut rows: Vec<String> = conn
            .prepare(&format!("SELECT {expr} FROM {table}"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        rows.sort();
        out.push((table, rows));
    }
    out
}

fn rows_of<'a>(dump: &'a [(String, Vec<String>)], table: &str) -> &'a [String] {
    dump.iter()
        .find(|(name, _)| name == table)
        .map(|(_, rows)| rows.as_slice())
        .unwrap_or_else(|| panic!("no table {table} in the dump"))
}

/// The stored payload documents of one path, newest first.
fn payloads_of(store: &Store, path: &str) -> Vec<(i64, String, String, String)> {
    let conn = lock_conn(&store.conn).expect("connection");
    let rows: Vec<(i64, String, String, String)> = conn
        .prepare(
            "SELECT fp.payload_id, fp.parse_outcome_json, fp.engine_json, fp.extraction_json
           FROM file_payloads fp JOIN paths p ON p.id = fp.file_id
          WHERE p.path = ?1 ORDER BY fp.payload_id DESC",
        )
        .unwrap()
        .query_map(params![path], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_>>()
        .unwrap();
    rows
}

fn payload_order(store: &Store) -> Vec<String> {
    let conn = lock_conn(&store.conn).expect("connection");
    let rows: Vec<String> = conn
        .prepare(
            "SELECT p.path FROM file_payloads fp JOIN paths p ON p.id = fp.file_id
          ORDER BY fp.payload_id",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<_>>()
        .unwrap();
    rows
}

/// The three documents a fresh row is contracted to hold, built the way the
/// contract states them rather than by calling the code under test.
fn expected_documents(extraction: &Extraction) -> (String, String, String) {
    let mut durable = extraction.for_durable_store();
    durable.source_code = None;
    (
        serde_json::to_string(&extraction.parse_outcome).unwrap(),
        serde_json::to_string(&extraction.engine).unwrap(),
        serde_json::to_string(&durable).unwrap(),
    )
}

/// A fresh row holds the parse outcome, the engine and the durable extraction,
/// each as the JSON `serde_json` produces for it, and nothing else.
#[test]
fn a_fresh_payload_row_holds_the_three_documents_the_contract_names() {
    let store = Store::open_in_memory().expect("store");
    let steps = scenario(6);
    write_step(&store, &steps[0]).expect("cold write");
    for extraction in &steps[0].extractions {
        let rows = payloads_of(&store, &extraction.file_path);
        assert_eq!(rows.len(), 1, "{}: one payload", extraction.file_path);
        let (_, parse, engine, body) = &rows[0];
        let (want_parse, want_engine, want_body) = expected_documents(extraction);
        assert_eq!(
            parse, &want_parse,
            "{}: parse outcome",
            extraction.file_path
        );
        assert_eq!(engine, &want_engine, "{}: engine", extraction.file_path);
        assert_eq!(body, &want_body, "{}: extraction", extraction.file_path);
    }
}

/// Payload ids are handed out in extraction order, because the insert order is
/// extraction order. A write that serialized in parallel and inserted in
/// completion order would reorder these and still store every document.
#[test]
fn payload_ids_follow_extraction_order() {
    let store = Store::open_in_memory().expect("store");
    let steps = scenario(40);
    write_step(&store, &steps[0]).expect("cold write");
    let want: Vec<String> = steps[0]
        .extractions
        .iter()
        .map(|extraction| extraction.file_path.clone())
        .collect();
    assert_eq!(payload_order(&store), want);
}

/// A carried file keeps its payload; only the files the write names get new
/// ones, and a deleted file gets none.
#[test]
fn an_incremental_write_adds_payloads_only_for_the_files_it_rewrites() {
    let store = Store::open_in_memory().expect("store");
    let steps = scenario(6);
    assert_eq!(steps.len(), 3);
    write_step(&store, &steps[0]).expect("cold write");
    let cold_order = payload_order(&store);
    assert_eq!(cold_order.len(), 7, "six modules and the broken file");

    write_step(&store, &steps[1]).expect("edit write");
    let mut order = payload_order(&store);
    assert_eq!(
        &order[cold_order.len()..],
        &["pkg/mod_0.py".to_string(), "pkg/broken.py".to_string()],
        "the two affected files, in extraction order, after the cold payloads"
    );

    write_step(&store, &steps[2]).expect("edit and delete write");
    order = payload_order(&store);
    assert_eq!(
        &order[cold_order.len() + 2..],
        &[
            "pkg/mod_0.py".to_string(),
            "pkg/mod_2.py".to_string(),
            "pkg/broken.py".to_string()
        ],
        "the three affected files; the deleted file gets none"
    );
    assert_eq!(
        payloads_of(&store, "pkg/mod_1.py").len(),
        1,
        "the deleted file keeps only the payload of the generation that had it"
    );
    let live: i64 = lock_conn(&store.conn)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM generation_file_rows WHERE generation_id = 3",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(live, 6, "seven files less the deleted one");
}

/// The dump is not vacuous: the tables the comparison is about hold rows.
#[test]
fn the_table_dump_covers_the_relations_the_write_fills() {
    let store = Store::open_in_memory().expect("store");
    for step in scenario(6) {
        write_step(&store, &step).expect("write");
    }
    let dump = dump(&store);
    for table in [
        "paths",
        "file_payloads",
        "generation_file_rows",
        "generation_nodes",
        "generation_literals",
        "edge_rows",
        "generation_file_digests",
        "nodes_fts_map",
    ] {
        assert!(
            !rows_of(&dump, table).is_empty(),
            "{table} is empty, so comparing it proves nothing"
        );
    }
}

// --- What must not depend on how the documents are produced -----------------

fn pool(threads: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .expect("thread pool")
}

fn write_step_tuned(store: &Store, step: &Step, tuning: &PayloadTuning) -> Result<u32> {
    let (resolution, analysis) = resolve_and_analyze(&step.extractions);
    store
        .save_generation_tuned(
            &step.extractions,
            &resolution,
            &analysis,
            step.opts.clone(),
            "unknown",
            tuning,
        )
        .map(|(generation, _)| generation)
}

fn tuned(threads: usize) -> PayloadTuning {
    PayloadTuning {
        threads,
        ..PayloadTuning::default()
    }
}

/// `threads` threads and a window that stops handing out work once `bytes` of
/// documents are waiting to be inserted.
fn windowed(threads: usize, bytes: usize) -> PayloadTuning {
    PayloadTuning {
        window_bytes: bytes,
        ..tuned(threads)
    }
}

fn dump_after(steps: &[Step], tuning: &PayloadTuning) -> Vec<(String, Vec<String>)> {
    let store = Store::open_in_memory().expect("store");
    for step in steps {
        write_step_tuned(&store, step, tuning).expect("write");
    }
    dump(&store)
}

/// An empty generation, which has no fresh extraction at all.
fn empty_scenario() -> Vec<Step> {
    vec![Step {
        extractions: Vec::new(),
        opts: GenerationWriteOpts::default(),
    }]
}

/// Two writes of one input hold identical rows in every table, ids included,
/// whatever the thread count and whatever the byte budget -- on both sides of
/// every window boundary.
///
/// The reference is one thread, which is the serial loop in all but name. The
/// window is twice the threads, so the fresh counts are chosen against the
/// windows under test: 0 (nothing to serialize), 1, one short of a window,
/// exactly a window, and a window plus one, for windows of 4 (two threads) and
/// of 16 (eight).
#[test]
fn the_tables_do_not_depend_on_thread_count_or_window() {
    let serial = tuned(1);
    let mut cases: Vec<(String, Vec<Step>)> = vec![("empty".to_string(), empty_scenario())];
    for fresh in [1, 2, 3, 4, 5, 8, 9, 15, 16, 17, 40] {
        // A cold write holds `modules + 1` files: the modules and the broken one.
        cases.push((format!("{fresh} fresh"), scenario(fresh - 1)));
    }

    for (label, steps) in &cases {
        let want = dump_after(steps, &serial);
        for (threads, bytes) in [
            (2, usize::MAX),
            (3, usize::MAX),
            (8, usize::MAX),
            (8, 0),
            (3, 1),
            (16, 1),
        ] {
            assert_eq!(
                dump_after(steps, &windowed(threads, bytes)),
                want,
                "{label}: {threads} threads with a {bytes} B budget wrote different rows than the serial loop"
            );
        }
        assert_eq!(
            dump_after(steps, &PayloadTuning::default()),
            want,
            "{label}: the production tuning wrote different rows than the serial loop"
        );
    }
}

/// A serializer that fails for two files, so that which one the write names
/// shows whether the first in iteration order wins.
fn fails_on_mod_3_and_mod_5(extraction: &Extraction) -> Result<PayloadJson> {
    if extraction.file_path.ends_with("/mod_3.py") || extraction.file_path.ends_with("/mod_5.py") {
        return Err(refusal(format!(
            "injected serialization failure for {}",
            extraction.file_path
        )));
    }
    serialize_payload(extraction)
}

fn panics_on_mod_2_and_mod_4(extraction: &Extraction) -> Result<PayloadJson> {
    if extraction.file_path.ends_with("/mod_2.py") || extraction.file_path.ends_with("/mod_4.py") {
        panic!("injected panic for {}", extraction.file_path);
    }
    serialize_payload(extraction)
}

fn nothing_was_written(store: &Store) {
    let conn = lock_conn(&store.conn).expect("connection: the store must not be poisoned");
    for table in [
        "generations",
        "paths",
        "file_payloads",
        "generation_file_rows",
        "generation_nodes",
        "generation_literals",
    ] {
        let rows: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 0, "{table} kept rows of a write that failed");
    }
}

/// The write fails at the first failing extraction in iteration order, with
/// that extraction's message, whatever the window and
/// however many threads reach the failures first; and the failed write
/// leaves nothing behind.
#[test]
fn a_serialization_failure_names_the_first_failing_extraction_and_rolls_back() {
    let steps = scenario(8);
    let mut seen = Vec::new();
    for (threads, bytes) in [
        (1, usize::MAX),
        (2, usize::MAX),
        (8, usize::MAX),
        (8, 0),
        (3, 1),
    ] {
        let store = Store::open_in_memory().expect("store");
        let tuning = PayloadTuning {
            serialize: fails_on_mod_3_and_mod_5,
            ..windowed(threads, bytes)
        };
        let error = write_step_tuned(&store, &steps[0], &tuning)
            .expect_err("a failing serializer must fail the write")
            .to_string();
        assert!(
            error.contains("injected serialization failure for pkg/mod_3.py"),
            "{threads} threads, {bytes} B: the first failing extraction in iteration order is mod_3: {error}"
        );
        assert!(
            !error.contains("mod_5.py"),
            "{threads} threads, {bytes} B: a later failure pre-empted the first: {error}"
        );
        nothing_was_written(&store);
        // The store is usable after the failed write.
        write_step_tuned(&store, &steps[0], &windowed(threads, bytes))
            .expect("a good write after a failed one");
        seen.push(error);
    }
    assert!(
        seen.windows(2).all(|pair| pair[0] == pair[1]),
        "the refusal text must not depend on the threads or the window: {seen:?}"
    );
}

/// A serializer that panics does not take the process's store with it: the
/// write fails naming the file, rolls back, and the connection mutex is not
/// poisoned.
#[test]
fn a_panic_in_the_serializer_is_a_failed_write_not_a_poisoned_store() {
    let steps = scenario(8);
    for threads in [1, 3, 8] {
        let store = Store::open_in_memory().expect("store");
        let tuning = PayloadTuning {
            serialize: panics_on_mod_2_and_mod_4,
            ..tuned(threads)
        };
        let error = write_step_tuned(&store, &steps[0], &tuning)
            .expect_err("a panicking serializer must fail the write")
            .to_string();
        assert!(
            error.contains("panicked for pkg/mod_2.py") && error.contains("injected panic"),
            "{threads} threads: the first panicking extraction is named with its reason: {error}"
        );
        nothing_was_written(&store);
        write_step_tuned(&store, &steps[0], &tuned(threads))
            .expect("the store is not poisoned by the caught panic");
        assert_eq!(payload_order(&store).len(), 9);
    }
}

/// An insert that fails before a later extraction's serialization failure is
/// the error the write reports, as in the serial loop, where the later
/// extraction was never reached. A window that serialized the later one first
/// must not change that.
#[test]
fn an_earlier_insert_failure_is_not_pre_empted_by_a_later_serialization_failure() {
    let steps = scenario(8);
    let mut seen = Vec::new();
    for threads in [1, 3, 8] {
        let store = Store::open_in_memory().expect("store");
        lock_conn(&store.conn)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER refuse_mod_2 BEFORE INSERT ON file_payloads
                 WHEN NEW.file_id = (SELECT id FROM paths WHERE path = 'pkg/mod_2.py')
                 BEGIN SELECT RAISE(ABORT, 'injected insert failure'); END;",
            )
            .unwrap();
        let tuning = PayloadTuning {
            serialize: fails_on_mod_3_and_mod_5,
            ..tuned(threads)
        };
        let error = write_step_tuned(&store, &steps[0], &tuning)
            .expect_err("both failures are armed")
            .to_string();
        assert!(
            error.contains("injected insert failure"),
            "{threads} threads: mod_2's insert fails before mod_3 is reached: {error}"
        );
        seen.push(error);
    }
    assert!(seen.windows(2).all(|pair| pair[0] == pair[1]), "{seen:?}");
}

/// No thread to serialize on is not a tuning; the write refuses it before it
/// touches the store.
#[test]
fn zero_threads_are_refused_not_a_panic() {
    let store = Store::open_in_memory().expect("store");
    let steps = scenario(3);
    let error = write_step_tuned(&store, &steps[0], &tuned(0))
        .expect_err("no thread to serialize on")
        .to_string();
    assert!(error.contains("at least one thread"), "{error}");
    nothing_was_written(&store);
}

// --- Deadlock: the write holds the connection mutex while it waits ---------

/// Runs `work` on its own thread and fails, rather than hangs, when it has not
/// returned within `limit`. A timed-out thread is leaked on purpose: it is the
/// deadlock. The only clock in these tests; every assertion is on an outcome.
fn bounded<T: Send + 'static>(
    what: &str,
    limit: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(work());
    });
    match receiver.recv_timeout(limit) {
        Ok(value) => value,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("DEADLOCK: {what} did not return within {limit:?}")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("{what}: the worker thread died without a result")
        }
    }
}

/// The write holds the store's connection mutex while it waits for its chunk.
/// If the chunk were serialized on a rayon pool, a job already running on that
/// pool that wants the same mutex -- the extraction pass's cache lookups are
/// exactly that, on rayon's global pool -- would block behind a holder that is
/// waiting for the pool, and nothing could make progress.
///
/// Built deterministically on a rayon pool of two: two blockers occupy both
/// workers and wait for a flag; the flag is raised only once the writer is seen
/// holding the mutex; then the blockers go for the mutex. The write must finish
/// anyway, because it does not use that pool.
#[test]
fn a_pool_job_that_needs_the_store_mutex_cannot_deadlock_the_write_that_holds_it() {
    assert_eq!(pool_blocked_write(true), Ok(1));
}

/// Control for the scaffolding above: the same blockers and the same pool, but
/// they never ask for the mutex, so a hang above comes from the mutex and not
/// from the construction.
#[test]
fn the_same_scaffolding_without_the_mutex_completes() {
    assert_eq!(pool_blocked_write(false), Ok(1));
}

fn pool_blocked_write(blockers_take_mutex: bool) -> std::result::Result<u32, String> {
    let store = Arc::new(Store::open_in_memory().expect("store"));
    let steps = scenario(7);
    let extractions = Arc::new(steps.into_iter().next().expect("a cold step").extractions);
    let (resolution, analysis) = resolve_and_analyze(&extractions);
    let the_pool = Arc::new(pool(2));
    let go = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicUsize::new(0));
    for _ in 0..2 {
        let (store, go, started) = (Arc::clone(&store), Arc::clone(&go), Arc::clone(&started));
        the_pool.spawn(move || {
            started.fetch_add(1, Ordering::SeqCst);
            while !go.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            if blockers_take_mutex {
                let _guard = lock_conn(&store.conn);
            }
        });
    }
    while started.load(Ordering::SeqCst) < 2 {
        std::thread::sleep(Duration::from_millis(1));
    }
    {
        let (store, go) = (Arc::clone(&store), Arc::clone(&go));
        std::thread::spawn(move || {
            // Raised only once somebody holds the mutex, which can only be the
            // writer: both pool workers are still spinning on the flag.
            loop {
                match store.conn.try_lock() {
                    Err(std::sync::TryLockError::WouldBlock) => break,
                    _ => std::thread::sleep(Duration::from_millis(1)),
                }
            }
            go.store(true, Ordering::SeqCst);
        });
    }
    let writer_store = Arc::clone(&store);
    bounded(
        "a generation write whose rayon pool is busy waiting for the store mutex",
        Duration::from_secs(20),
        move || {
            let outcome = writer_store
                .save_generation_tuned(
                    &extractions,
                    &resolution,
                    &analysis,
                    GenerationWriteOpts::default(),
                    "unknown",
                    &tuned(4),
                )
                .map(|(generation, _)| generation)
                .map_err(|error| error.to_string());
            drop(the_pool);
            outcome
        },
    )
}

/// The same hazard without a second thread: the write is issued from inside a
/// rayon worker. The sibling writes queued on that pool are jobs that want the
/// same mutex; a write that waited for its chunk by stealing work would run one
/// of them on top of its own stack and block on a mutex it already holds.
#[test]
fn writes_issued_from_rayon_workers_do_not_steal_each_other_and_self_deadlock() {
    fn slow(extraction: &Extraction) -> Result<PayloadJson> {
        std::thread::sleep(Duration::from_millis(3));
        serialize_payload(extraction)
    }
    let store = Arc::new(Store::open_in_memory().expect("store"));
    let steps = scenario(7);
    let extractions = Arc::new(steps.into_iter().next().expect("a cold step").extractions);
    let (resolution, analysis) = resolve_and_analyze(&extractions);
    let resolution = Arc::new(resolution);
    let analysis = Arc::new(analysis);
    let finished = bounded(
        "sibling store writes issued from rayon workers",
        Duration::from_secs(60),
        move || {
            let writes = AtomicUsize::new(0);
            pool(2).install(|| {
                (0..16usize).into_par_iter().for_each(|_| {
                    store
                        .save_generation_tuned(
                            &extractions,
                            &resolution,
                            &analysis,
                            GenerationWriteOpts::default(),
                            "unknown",
                            &PayloadTuning {
                                serialize: slow,
                                ..tuned(4)
                            },
                        )
                        .expect("write");
                    writes.fetch_add(1, Ordering::SeqCst);
                });
            });
            writes.load(Ordering::SeqCst)
        },
    );
    assert_eq!(finished, 16);
}

static MOD_1_RUNNING: AtomicBool = AtomicBool::new(false);
static MOD_1_RELEASE: AtomicBool = AtomicBool::new(false);

/// `mod_1` holds its thread until released (or for a second), and says so;
/// `mod_0` waits for it, so the two are in flight on two threads at once.
fn holds_mod_1(extraction: &Extraction) -> Result<PayloadJson> {
    if extraction.file_path.ends_with("/mod_0.py") {
        let waited = std::time::Instant::now();
        while !MOD_1_RUNNING.load(Ordering::SeqCst) && waited.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    if extraction.file_path.ends_with("/mod_1.py") {
        MOD_1_RUNNING.store(true, Ordering::SeqCst);
        let waited = std::time::Instant::now();
        while !MOD_1_RELEASE.load(Ordering::SeqCst) && waited.elapsed() < Duration::from_secs(1) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    serialize_payload(extraction)
}

/// Deterministic form of the above. The write runs on a rayon worker, with two
/// of its extractions in flight and an unrelated job queued on the same pool
/// that only wants the store mutex, which the write is holding. A write whose
/// waiting thread stole work would run that job on top of its own stack and
/// never get the lock. Here the pool's other worker runs it, and it waits its
/// turn behind the write.
#[test]
fn a_write_on_a_pool_worker_runs_no_queued_work_on_top_of_its_held_mutex() {
    assert_eq!(
        queued_job_beside_a_pool_worker_write(false),
        Ok(1),
        "control"
    );
    assert_eq!(queued_job_beside_a_pool_worker_write(true), Ok(1));
}

fn queued_job_beside_a_pool_worker_write(
    queued_job_takes_the_mutex: bool,
) -> std::result::Result<u32, String> {
    MOD_1_RUNNING.store(false, Ordering::SeqCst);
    MOD_1_RELEASE.store(false, Ordering::SeqCst);
    let store = Arc::new(Store::open_in_memory().expect("store"));
    let two = Arc::new(extract_all(&[
        (
            "pkg/mod_0.py".to_string(),
            "def a():\n    return 0\n".to_string(),
        ),
        (
            "pkg/mod_1.py".to_string(),
            "def b():\n    return 1\n".to_string(),
        ),
    ]));
    let (resolution, analysis) = resolve_and_analyze(&two);
    let the_pool = Arc::new(pool(2));
    {
        let (store, the_pool) = (Arc::clone(&store), Arc::clone(&the_pool));
        std::thread::spawn(move || {
            while !MOD_1_RUNNING.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            // Queued from outside the pool, so it lands in the injector.
            the_pool.spawn(move || {
                if queued_job_takes_the_mutex {
                    let _guard = lock_conn(&store.conn);
                }
                MOD_1_RELEASE.store(true, Ordering::SeqCst);
            });
        });
    }
    let (writer_store, writer_pool) = (Arc::clone(&store), Arc::clone(&the_pool));
    bounded(
        &format!(
            "a write issued on a pool worker beside a queued job (that takes the mutex: {queued_job_takes_the_mutex})"
        ),
        Duration::from_secs(20),
        move || {
            writer_pool.install(|| {
                writer_store
                    .save_generation_tuned(
                        &two,
                        &resolution,
                        &analysis,
                        GenerationWriteOpts::default(),
                        "unknown",
                        &PayloadTuning {
                            serialize: holds_mod_1,
                            ..tuned(2)
                        },
                    )
                    .map(|(generation, _)| generation)
                    .map_err(|error| error.to_string())
            })
        },
    )
}

// --- Threads: once per write, never the inserting thread -------------------

static SEEN: Mutex<Vec<(String, std::thread::ThreadId)>> = Mutex::new(Vec::new());

/// Held for the whole of any in-process test that reads `SEEN`: they all write
/// paths named `pkg/mod_*.py`, and one test's threads must not count as
/// another's.
static SEEN_GUARD: Mutex<()> = Mutex::new(());

fn records_its_thread(extraction: &Extraction) -> Result<PayloadJson> {
    SEEN.lock()
        .unwrap()
        .push((extraction.file_path.clone(), std::thread::current().id()));
    serialize_payload(extraction)
}

fn threads_that_serialized(paths: &[String]) -> std::collections::BTreeSet<String> {
    SEEN.lock()
        .unwrap()
        .iter()
        .filter(|(path, _)| paths.contains(path))
        .map(|(_, thread)| format!("{thread:?}"))
        .collect()
}

/// A write starts its serializer threads once, not once per chunk, and the
/// thread that inserts never serializes while they exist. Nine fresh
/// extractions in chunks of two are five chunks: a thread per chunk would show
/// up as up to twenty distinct serializing threads, not four.
#[test]
fn a_write_starts_its_serializer_threads_once_and_the_inserting_thread_runs_none_of_them() {
    let _only_test_using_seen = SEEN_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let me = format!("{:?}", std::thread::current().id());
    let tuning = PayloadTuning {
        serialize: records_its_thread,
        ..tuned(4)
    };

    let steps = scenario(8);
    let nine: Vec<String> = steps[0]
        .extractions
        .iter()
        .map(|extraction| extraction.file_path.clone())
        .collect();
    assert_eq!(nine.len(), 9);
    let store = Store::open_in_memory().expect("store");
    write_step_tuned(&store, &steps[0], &tuning).expect("write");
    let used = threads_that_serialized(&nine);
    assert!(
        !used.is_empty() && used.len() <= 4,
        "five chunks on four threads started {} serializing threads: {used:?}",
        used.len()
    );
    assert!(!used.contains(&me), "the inserting thread serialized");

    // One thread asked for: no thread started, the writer serializes inline.
    let inline_steps = scenario(5);
    let inline_paths: Vec<String> = inline_steps[0]
        .extractions
        .iter()
        .map(|extraction| extraction.file_path.clone())
        .collect();
    SEEN.lock().unwrap().clear();
    let store = Store::open_in_memory().expect("store");
    let one_thread = PayloadTuning {
        serialize: records_its_thread,
        ..tuned(1)
    };
    write_step_tuned(&store, &inline_steps[0], &one_thread).expect("write");
    assert_eq!(
        threads_that_serialized(&inline_paths),
        std::collections::BTreeSet::from([me.clone()]),
        "one thread means the writing thread"
    );

    // One extraction and many threads asked for: nothing to spread, so no
    // thread is started for it either -- the shape of a one-file edit.
    let single = vec![inline_steps[0].extractions[0].clone()];
    let single_paths = vec![single[0].file_path.clone()];
    SEEN.lock().unwrap().clear();
    let store = Store::open_in_memory().expect("store");
    let many = PayloadTuning {
        serialize: records_its_thread,
        ..tuned(16)
    };
    let step = Step {
        extractions: single,
        opts: GenerationWriteOpts::default(),
    };
    write_step_tuned(&store, &step, &many).expect("write");
    assert_eq!(
        threads_that_serialized(&single_paths),
        std::collections::BTreeSet::from([me]),
        "a lone extraction is serialized inline"
    );
}

// --- Panic payloads --------------------------------------------------------

/// A panic payload whose own `Drop` panics. The catch is `catch_unwind` around
/// the serializer only; a payload dropped afterwards, outside it, would unwind
/// through the held connection guard.
struct DropBomb;
impl Drop for DropBomb {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            panic!("the payload's drop panicked");
        }
    }
}

fn panics_with_a_drop_bomb(extraction: &Extraction) -> Result<PayloadJson> {
    if extraction.file_path.ends_with("/mod_2.py") {
        std::panic::panic_any(DropBomb);
    }
    serialize_payload(extraction)
}

fn write_with_a_drop_bomb(
    store: &Store,
    threads: usize,
) -> std::thread::Result<std::result::Result<u32, String>> {
    let steps = scenario(5);
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        write_step_tuned(
            store,
            &steps[0],
            &PayloadTuning {
                serialize: panics_with_a_drop_bomb,
                ..tuned(threads)
            },
        )
        .map_err(|error| error.to_string())
    }))
}

/// "A panic in the serializer is a refusal naming the file" holds for a payload
/// whose drop panics too, on a worker thread and inline.
#[test]
fn a_panic_payload_that_panics_on_drop_is_still_a_refusal() {
    for threads in [1, 4] {
        let store = Store::open_in_memory().unwrap();
        let outcome = write_with_a_drop_bomb(&store, threads);
        let refusal = outcome.expect("the write unwound instead of returning a refusal");
        assert!(
            refusal.is_err_and(|text| text.contains("pkg/mod_2.py")),
            "{threads} threads: a refusal naming the file"
        );
    }
}

/// The consequence the catch exists to prevent: the unwind crosses the held
/// connection guard and poisons the store for every later write.
#[test]
fn a_panic_payload_that_panics_on_drop_does_not_poison_the_store() {
    for threads in [1, 4] {
        let store = Store::open_in_memory().unwrap();
        let _ = write_with_a_drop_bomb(&store, threads);
        let steps = scenario(5);
        let second = write_step_tuned(&store, &steps[0], &tuned(threads));
        assert_eq!(
            second.map_err(|error| error.to_string()),
            Ok(1),
            "{threads} threads: the store must still accept a write"
        );
    }
}

// --- The byte budget -------------------------------------------------------

/// The budget closes the window while finished documents wait to be inserted
/// and opens it again as they are. The test plays the serializer threads
/// itself, so every step is deterministic: a window of four, a budget of 25
/// bytes, documents of 10 bytes.
#[test]
fn the_budget_stops_handing_out_work_while_finished_documents_wait() {
    let extractions = extract_all(&[
        ("pkg/a0.py".to_string(), "x = 0\n".to_string()),
        ("pkg/a1.py".to_string(), "x = 1\n".to_string()),
        ("pkg/a2.py".to_string(), "x = 2\n".to_string()),
        ("pkg/a3.py".to_string(), "x = 3\n".to_string()),
        ("pkg/a4.py".to_string(), "x = 4\n".to_string()),
        ("pkg/a5.py".to_string(), "x = 5\n".to_string()),
        ("pkg/a6.py".to_string(), "x = 6\n".to_string()),
    ]);
    let fresh: Vec<&Extraction> = extractions.iter().collect();
    let (job_tx, job_rx) = mpsc::channel::<PayloadJob<'_>>();
    let (done_tx, done_rx) = mpsc::channel::<PayloadDone>();
    let mut feed = PayloadFeed::Windowed(PayloadWindow {
        jobs: &job_tx,
        done: &done_rx,
        fresh: &fresh,
        window: 4,
        budget: 25,
        next_dispatch: 0,
        held_bytes: 0,
        arrived: std::collections::BTreeMap::new(),
    });
    let mut handed_out = 0usize;
    let mut handed = |job_rx: &mpsc::Receiver<PayloadJob<'_>>| {
        handed_out += job_rx.try_iter().count();
        handed_out
    };

    // The head is the only thing back when the write asks for it: the window of
    // four is handed out.
    done_tx.send((0, Ok(PayloadJson::sized(10)))).unwrap();
    assert!(feed.take(0).unwrap().is_ok());
    assert_eq!(handed(&job_rx), 4, "an empty window is filled");

    // Three more come back, out of order, while the head is inserted: 30 bytes
    // wait, over the budget, so nothing more is handed out.
    for index in [3, 2, 1] {
        done_tx.send((index, Ok(PayloadJson::sized(10)))).unwrap();
    }
    assert!(feed.take(1).unwrap().is_ok());
    assert_eq!(handed(&job_rx), 4, "30 B waiting is over a 25 B budget");

    // One is inserted: 20 B wait, inside the budget, and the window has room for
    // two more behind the head it now has (4 outstanding -> indexes 4 and 5).
    assert!(feed.take(2).unwrap().is_ok());
    assert_eq!(handed(&job_rx), 6, "20 B waiting is inside the budget");
}

#[cfg(test)]
#[path = "attack2_persist_serialize_tests.rs"]
mod attack2;
