//! Round-2 attack on `lane/persist-serialize` @ 5fa8ed9d: the `std::thread::scope`
//! serializer. Brought onto the branch from `attack2/persist-serialize` and adapted
//! to the in-order window that replaced the chunks and the JSON estimate.
//!
//! Every assertion is on an outcome. The only clocks are the hard timeouts of
//! `bounded`, which turn a hang into a failure; no test compares timings and
//! none burns CPU to create load.
//!
//! Tests named `breaks_*` were written to FAIL on 5fa8ed9d; every other test
//! passed there too and is kept as a guard.

use super::*;
use devmap_extract::extract_file_with_budget;

// --- helpers ----------------------------------------------------------------

fn explicit(threads: usize, window_bytes: usize) -> PayloadTuning {
    // Built field by field, never from `PayloadTuning::default()`.
    PayloadTuning {
        threads,
        window_bytes,
        serialize: serialize_payload,
    }
}

fn paths_of(step: &Step) -> Vec<String> {
    step.extractions
        .iter()
        .map(|extraction| extraction.file_path.clone())
        .collect()
}

/// The module number of `pkg/mod_<k>.py`, and the file count for `broken.py`.
fn module_number(extraction: &Extraction, broken_as: usize) -> usize {
    extraction
        .file_path
        .strip_prefix("pkg/mod_")
        .and_then(|rest| rest.strip_suffix(".py"))
        .and_then(|number| number.parse().ok())
        .unwrap_or(broken_as)
}

fn count_rows(store: &Store, table: &str) -> i64 {
    lock_conn(&store.conn)
        .expect("connection: the store must not be poisoned")
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

// --- 1. early exits inside the scope ----------------------------------------

/// An insert that fails in the middle of the window, while every serializer thread
/// is idle on the job channel: the scope must still join, the write must
/// report the insert, the transaction must roll back, and the next write on
/// the same store must produce exactly what a fresh store would.
#[test]
fn a1_an_insert_error_mid_window_joins_the_scope_and_leaves_the_store_whole() {
    let steps = scenario(10);
    for threads in [8, 2, 3] {
        let store = Store::open_in_memory().unwrap();
        lock_conn(&store.conn)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER refuse_mod_4 BEFORE INSERT ON file_payloads
                 WHEN NEW.file_id = (SELECT id FROM paths WHERE path = 'pkg/mod_4.py')
                 BEGIN SELECT RAISE(ABORT, 'injected insert failure'); END;",
            )
            .unwrap();
        let step = Step {
            extractions: steps[0].extractions.clone(),
            opts: GenerationWriteOpts::default(),
        };
        let store = Arc::new(store);
        let outcome = {
            let store = Arc::clone(&store);
            bounded(
                "a write whose insert fails mid-window",
                Duration::from_secs(60),
                move || {
                    write_step_tuned(&store, &step, &tuned(threads))
                        .map_err(|error| error.to_string())
                },
            )
        };
        let error = outcome.expect_err("the insert is refused");
        assert!(error.contains("injected insert failure"), "{error}");
        for table in [
            "generations",
            "paths",
            "file_payloads",
            "generation_file_rows",
            "generation_nodes",
        ] {
            assert_eq!(count_rows(&store, table), 0, "{table} kept rows");
        }
        lock_conn(&store.conn)
            .unwrap()
            .execute_batch("DROP TRIGGER refuse_mod_4")
            .unwrap();
        let again = Step {
            extractions: steps[0].extractions.clone(),
            opts: GenerationWriteOpts::default(),
        };
        let generation = bounded("the write after the failed one", Duration::from_secs(60), {
            let store = Arc::clone(&store);
            move || write_step_tuned(&store, &again, &tuned(threads)).unwrap()
        });
        assert_eq!(generation, 1, "{threads} threads");
        assert_eq!(
            dump(&store),
            {
                let reference = Store::open_in_memory().unwrap();
                write_step_tuned(&reference, &steps[0], &tuned(1)).unwrap();
                dump(&reference)
            },
            "{threads} threads: the write after a failed one differs from a fresh store's"
        );
    }
}

static CALLS_A: AtomicUsize = AtomicUsize::new(0);

fn counts_and_fails_on_the_first(extraction: &Extraction) -> Result<PayloadJson> {
    CALLS_A.fetch_add(1, Ordering::SeqCst);
    if extraction.file_path.ends_with("/mod_0.py") {
        return Err(refusal("injected failure on the first extraction"));
    }
    serialize_payload(extraction)
}

/// When a write that failed returns, no serializer call is still running or
/// queued: the count is the same a moment later. The head of the window failed,
/// so exactly the window -- twice the six threads -- had been handed out.
#[test]
fn a1_no_serializer_call_outlives_a_failed_write() {
    let steps = scenario(20);
    CALLS_A.store(0, Ordering::SeqCst);
    let store = Store::open_in_memory().unwrap();
    let tuning = PayloadTuning {
        serialize: counts_and_fails_on_the_first,
        ..explicit(6, usize::MAX)
    };
    let error = write_step_tuned(&store, &steps[0], &tuning).expect_err("first fails");
    assert!(error.to_string().contains("first extraction"));
    let at_return = CALLS_A.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(CALLS_A.load(Ordering::SeqCst), at_return, "a straggler ran");
    assert_eq!(
        at_return, 12,
        "the window of six threads is twelve extractions"
    );
}

// --- 2. thread starvation and partial spawns ----------------------------------

/// Thread counts from one to far more than there are extractions write the
/// same rows, and the threads started never exceed the extractions.
#[test]
fn a2_any_thread_count_writes_the_same_rows_and_starts_no_more_threads_than_extractions() {
    let _only_test_using_seen = SEEN_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let steps = scenario(2); // three fresh extractions
    let want = dump_after(&steps[..1], &tuned(1));
    for threads in [1usize, 2, 3, 4, 10_000] {
        SEEN.lock().unwrap().clear();
        let store = Store::open_in_memory().unwrap();
        let tuning = PayloadTuning {
            serialize: records_its_thread,
            ..explicit(threads, usize::MAX)
        };
        let paths = paths_of(&steps[0]);
        let ok = bounded("a write with many threads", Duration::from_secs(60), {
            let step = Step {
                extractions: steps[0].extractions.clone(),
                opts: GenerationWriteOpts::default(),
            };
            move || {
                let store = store;
                write_step_tuned(&store, &step, &tuning).unwrap();
                dump(&store)
            }
        });
        assert_eq!(ok, want, "{threads} threads wrote different rows");
        let used = threads_that_serialized(&paths);
        assert!(
            used.len() <= threads.min(3),
            "{threads} threads asked for, 3 extractions, {} threads serialized",
            used.len()
        );
        if threads == 1 {
            assert_eq!(used.len(), 1, "one thread: inline on the writing thread");
        }
    }
}

const SPAWN_CHILD: &str = "ATTACK2_SPAWN_REFUSAL_CHILD";

/// The OS refuses every thread the write asks for (the child's stack size is
/// larger than the address space, so `spawn_scoped` returns `Err`): the write
/// goes on inline and writes the same rows. Run in a child process because
/// the stack size is read once per process. The fixtures are resolved here and
/// handed over as JSON, because the child cannot start rayon's pool either.
#[test]
fn a2_a_write_whose_threads_the_os_refuses_still_finishes_with_the_same_rows() {
    if std::env::var_os(SPAWN_CHILD).is_some() {
        return;
    }
    let steps = scenario(5);
    let (resolution, analysis) = resolve_and_analyze(&steps[0].extractions);
    let fixture = std::env::temp_dir().join(format!("attack2-spawn-{}.json", std::process::id()));
    std::fs::write(
        &fixture,
        serde_json::to_vec(&(&steps[0].extractions, &resolution, &analysis)).unwrap(),
    )
    .unwrap();
    let exe = std::env::current_exe().unwrap();
    let output = std::process::Command::new(exe)
        .args([
            "--exact",
            "db::payload_serialization_tests::attack2::a2_spawn_refusal_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(SPAWN_CHILD, &fixture)
        .env("RUST_MIN_STACK", "70368744177664")
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&fixture);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("CHILD-OK"),
        "child failed: status {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
}

#[test]
fn a2_spawn_refusal_child() {
    let Some(fixture) = std::env::var_os(SPAWN_CHILD) else {
        return;
    };
    // Proof the premise holds in this process: a thread cannot be started.
    let refused = std::thread::Builder::new().spawn(|| ()).is_err();
    assert!(
        refused,
        "the OS did not refuse a thread; the child proves nothing"
    );
    let (extractions, resolution, analysis): (
        Vec<Extraction>,
        devmap_resolve::model::ResolutionResult,
        AnalysisSummary,
    ) = serde_json::from_slice(&std::fs::read(fixture).unwrap()).unwrap();
    let write = |tuning: &PayloadTuning| {
        let store = Store::open_in_memory().unwrap();
        store
            .save_generation_tuned(
                &extractions,
                &resolution,
                &analysis,
                GenerationWriteOpts::default(),
                "unknown",
                tuning,
            )
            .unwrap();
        dump(&store)
    };
    let want = write(&explicit(1, usize::MAX));
    let me = format!("{:?}", std::thread::current().id());
    SEEN.lock().unwrap().clear();
    let got = write(&PayloadTuning {
        serialize: records_its_thread,
        ..explicit(8, usize::MAX)
    });
    assert_eq!(got, want);
    let paths: Vec<String> = extractions.iter().map(|e| e.file_path.clone()).collect();
    assert_eq!(
        threads_that_serialized(&paths),
        std::collections::BTreeSet::from([me]),
        "no thread could start, so the writer serialized inline"
    );
    println!("CHILD-OK");
}

// --- 3. adversarial completion order ------------------------------------------

fn inverse_sleep(extraction: &Extraction) -> Result<PayloadJson> {
    let k = module_number(extraction, 9);
    std::thread::sleep(Duration::from_millis((10 - k.min(10)) as u64 * 8));
    serialize_payload(extraction)
}

static LATER_DONE: AtomicUsize = AtomicUsize::new(0);

/// Job `k` does not finish until every job after it has: the reverse of the
/// order the write needs them in.
fn later_first(extraction: &Extraction) -> Result<PayloadJson> {
    let k = module_number(extraction, 8);
    let later: usize = ((k + 1)..=8).map(|j| 1usize << j).sum();
    let waited = std::time::Instant::now();
    while LATER_DONE.load(Ordering::SeqCst) & later != later {
        if waited.elapsed() > Duration::from_secs(20) {
            return Err(refusal(format!("later_first gate timed out at {k}")));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let outcome = if k == 3 || k == 5 {
        Err(refusal(format!(
            "injected failure for {}",
            extraction.file_path
        )))
    } else {
        serialize_payload(extraction)
    };
    LATER_DONE.fetch_or(1 << k, Ordering::SeqCst);
    outcome
}

fn later_first_all_ok(extraction: &Extraction) -> Result<PayloadJson> {
    let k = module_number(extraction, 8);
    let later: usize = ((k + 1)..=8).map(|j| 1usize << j).sum();
    let waited = std::time::Instant::now();
    while LATER_DONE.load(Ordering::SeqCst) & later != later {
        if waited.elapsed() > Duration::from_secs(20) {
            return Err(refusal(format!("later_first gate timed out at {k}")));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let outcome = serialize_payload(extraction);
    LATER_DONE.fetch_or(1 << k, Ordering::SeqCst);
    outcome
}

/// Serial order of insertion survives a serializer that finishes the extractions
/// in exactly reverse order, and one that merely sleeps inversely to the index.
/// (One test, because the two later-first serializers share a static.)
#[test]
fn a3_reverse_completion_order_writes_the_serial_rows_and_names_the_first_failure() {
    let steps = scenario(8); // mod_0..mod_7, broken.py = nine fresh
    let want = dump_after(&steps, &tuned(1));

    let slow = PayloadTuning {
        serialize: inverse_sleep,
        ..explicit(9, usize::MAX)
    };
    assert_eq!(dump_after(&steps, &slow), want, "inverse sleep");

    LATER_DONE.store(0, Ordering::SeqCst);
    let reversed = PayloadTuning {
        serialize: later_first_all_ok,
        ..explicit(9, usize::MAX)
    };
    let got = bounded("reverse completion", Duration::from_secs(60), {
        let steps = scenario(8);
        move || dump_after(&steps, &reversed)
    });
    assert_eq!(got, want, "reverse completion order");

    // mod_5 finishes (and fails) before mod_3 does, yet mod_3 is named.
    LATER_DONE.store(0, Ordering::SeqCst);
    let failing = PayloadTuning {
        serialize: later_first,
        ..explicit(9, usize::MAX)
    };
    let error = bounded(
        "reverse completion with failures",
        Duration::from_secs(60),
        {
            let steps = scenario(8);
            move || {
                let store = Store::open_in_memory().unwrap();
                write_step_tuned(&store, &steps[0], &failing)
                    .expect_err("two failures")
                    .to_string()
            }
        },
    );
    assert!(
        error.contains("pkg/mod_3.py") && !error.contains("mod_5"),
        "the first failing extraction in order must be named: {error}"
    );
}

// --- 4. reentrancy -------------------------------------------------------------

/// The production entry (`save_generation_timed`, default tuning) issued from
/// jobs on rayon's *global* pool while other global-pool jobs queue for the
/// store mutex.
#[test]
fn a4_production_writes_on_rayon_global_pool_beside_jobs_that_take_the_mutex() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let steps = scenario(7);
    let extractions = Arc::new(steps.into_iter().next().unwrap().extractions);
    let (resolution, analysis) = resolve_and_analyze(&extractions);
    let (resolution, analysis) = (Arc::new(resolution), Arc::new(analysis));
    let finished = bounded(
        "production writes issued from the global rayon pool",
        Duration::from_secs(120),
        {
            let store = Arc::clone(&store);
            move || {
                let done = Arc::new(AtomicUsize::new(0));
                for _ in 0..16 {
                    let (store, done) = (Arc::clone(&store), Arc::clone(&done));
                    rayon::spawn(move || {
                        for _ in 0..20 {
                            let _guard = lock_conn(&store.conn);
                        }
                        done.fetch_add(1, Ordering::SeqCst);
                    });
                }
                (0..24usize).into_par_iter().for_each(|_| {
                    store
                        .save_generation_timed(
                            &extractions,
                            &resolution,
                            &analysis,
                            GenerationWriteOpts::default(),
                            "unknown",
                        )
                        .map(|(generation, _)| generation)
                        .expect("write");
                });
                while done.load(Ordering::SeqCst) < 16 {
                    std::thread::sleep(Duration::from_millis(1));
                }
                24usize
            }
        },
    );
    assert_eq!(finished, 24);
}

/// Writes issued from inside sibling `std` scoped threads, each starting its
/// own scope of serializer threads, on one store.
#[test]
fn a4_writes_from_inside_scoped_threads_nest_without_deadlock() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let steps = scenario(9);
    let extractions = Arc::new(steps.into_iter().next().unwrap().extractions);
    let (resolution, analysis) = resolve_and_analyze(&extractions);
    let (resolution, analysis) = (Arc::new(resolution), Arc::new(analysis));
    let written = bounded("writes from scoped threads", Duration::from_secs(120), {
        let store = Arc::clone(&store);
        move || {
            let count = AtomicUsize::new(0);
            std::thread::scope(|scope| {
                for _ in 0..4 {
                    scope.spawn(|| {
                        for _ in 0..3 {
                            store
                                .save_generation_tuned(
                                    &extractions,
                                    &resolution,
                                    &analysis,
                                    GenerationWriteOpts::default(),
                                    "unknown",
                                    &explicit(4, usize::MAX),
                                )
                                .expect("write");
                            count.fetch_add(1, Ordering::SeqCst);
                        }
                    });
                }
            });
            count.load(Ordering::SeqCst)
        }
    });
    assert_eq!(written, 12);
}
/// The real extractor under a budget no loaded machine reaches. With the
/// default 5 s budget a 1 MiB file can come back `Failed` (a budget refusal,
/// which is tiny) instead of `Partial`, and a test that then passed would have
/// passed because the extraction did not run. `partial` asserts the premise.
fn hostile_extract(path: &str, source: &str, partial: bool) -> Extraction {
    let extraction = extract_file_with_budget(path, source, std::time::Duration::from_secs(600));
    assert!(
        !partial
            || matches!(
                &extraction.parse_outcome,
                devmap_extract::model::ParseOutcome::Partial { error_ranges, .. }
                    if !error_ranges.is_empty()
            ),
        "{path}: the premise (a Partial parse with error ranges) did not hold: {:?}",
        std::mem::discriminant(&extraction.parse_outcome)
    );
    extraction
}

fn empty_block_python(bytes: usize) -> String {
    "if 1:\n".repeat(bytes / 6)
}

static PRODUCED: AtomicUsize = AtomicUsize::new(0);

fn counts_what_it_produces(extraction: &Extraction) -> Result<PayloadJson> {
    let documents = serialize_payload(extraction)?;
    // `PayloadJson`'s fields are private to `write`; the contract's own
    // statement of the three documents has the same length by definition.
    let (parse, engine, body) = expected_documents(extraction);
    PRODUCED.fetch_add(parse.len() + engine.len() + body.len(), Ordering::SeqCst);
    Ok(documents)
}

// --- 8. panics outside the serializer's own catch ----------------------------------

/// An error whose `Drop` panics. The serializer's *return value* is foreign
/// code too: an `Err(rusqlite::Error::ToSqlConversionFailure(Box<dyn Error>))`
/// carries any boxed error.
#[derive(Debug)]
struct BombError;
impl std::fmt::Display for BombError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bomb")
    }
}
impl std::error::Error for BombError {}
impl Drop for BombError {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            panic!("the returned error's drop panicked");
        }
    }
}

fn plain_error_then_a_bomb(extraction: &Extraction) -> Result<PayloadJson> {
    if extraction.file_path.ends_with("/mod_1.py") {
        return Err(refusal("ordinary failure on mod_1"));
    }
    if extraction.file_path.ends_with("/mod_3.py") {
        return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(BombError)));
    }
    serialize_payload(extraction)
}

/// The first failure in order is an ordinary error and a later extraction's
/// *unconsumed* result is dropped when the write returns early. It is dropped
/// on the writing thread while the connection guard is held. The write must
/// still return the first error and leave the store usable.
///
/// Reachable only with an injected serializer: the production serializer's
/// errors are plain strings.
#[test]
fn breaks_an_unconsumed_result_whose_drop_panics_does_not_poison_the_store() {
    let mut report = Vec::new();
    for threads in [1usize, 4] {
        let steps = scenario(5);
        let store = Store::open_in_memory().unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            write_step_tuned(
                &store,
                &steps[0],
                &PayloadTuning {
                    serialize: plain_error_then_a_bomb,
                    ..explicit(threads, usize::MAX)
                },
            )
            .map_err(|error| error.to_string())
        }));
        let unwound = outcome.is_err();
        let poisoned = store.conn.is_poisoned();
        let named = matches!(&outcome, Ok(Err(text)) if text.contains("mod_1"));
        if unwound || poisoned || !named {
            report.push(format!(
                "{threads} threads: write unwound = {unwound}, store mutex poisoned = {poisoned}, first failure named = {named}"
            ));
        }
    }
    assert!(report.is_empty(), "{report:#?}");
}

// --- the production entry when the OS refuses threads ----------------------------

const PRODUCTION_CHILD: &str = "ATTACK2_PRODUCTION_REFUSAL_CHILD";

/// `save_generation_with_opts` (default tuning) in a process where no thread
/// can be started and rayon's global pool has not been initialized yet. The
/// documented behaviour is that a thread the OS will not give is not an
/// error. The default tuning asks rayon for its thread count, and rayon
/// builds its global pool to answer.
#[test]
fn breaks_the_production_entry_writes_inline_when_the_os_refuses_threads() {
    if std::env::var_os(PRODUCTION_CHILD).is_some() {
        return;
    }
    let steps = scenario(5);
    let (resolution, analysis) = resolve_and_analyze(&steps[0].extractions);
    let fixture =
        std::env::temp_dir().join(format!("attack2-production-{}.json", std::process::id()));
    std::fs::write(
        &fixture,
        serde_json::to_vec(&(&steps[0].extractions, &resolution, &analysis)).unwrap(),
    )
    .unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "db::payload_serialization_tests::attack2::a2_production_refusal_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PRODUCTION_CHILD, &fixture)
        .env("RUST_MIN_STACK", "70368744177664")
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&fixture);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("CHILD-OK"),
        "the production entry did not write: status {:?}\nstderr:\n{}",
        output.status,
        stderr.lines().take(6).collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn a2_production_refusal_child() {
    let Some(fixture) = std::env::var_os(PRODUCTION_CHILD) else {
        return;
    };
    assert!(std::thread::Builder::new().spawn(|| ()).is_err());
    let (extractions, resolution, analysis): (
        Vec<Extraction>,
        devmap_resolve::model::ResolutionResult,
        AnalysisSummary,
    ) = serde_json::from_slice(&std::fs::read(fixture).unwrap()).unwrap();
    let store = Store::open_in_memory().unwrap();
    store
        .save_generation_with_opts(
            &extractions,
            &resolution,
            &analysis,
            GenerationWriteOpts::default(),
        )
        .unwrap();
    println!("CHILD-OK");
}

// --- the in-flight bound, in actual bytes -----------------------------------------

fn json_bytes(extraction: &Extraction) -> usize {
    let (parse, engine, body) = expected_documents(extraction);
    parse.len() + engine.len() + body.len()
}

fn hostile_kib() -> usize {
    std::env::var("ATTACK2_HOSTILE_KIB")
        .ok()
        .and_then(|kib| kib.parse().ok())
        .unwrap_or(192)
}

/// What a write of `count` copies of a hostile extraction produced before its
/// first insert was refused: exactly the JSON the write held in flight, the
/// head and everything behind it that the window had handed out. Returns it
/// with the size of one payload.
fn hostile_in_flight(threads: usize, window_bytes: usize, count: usize) -> (usize, usize) {
    let template = hostile_extract(
        "hostile/suites.py",
        &empty_block_python(hostile_kib() * 1024),
        true,
    );
    let extractions: Vec<Extraction> = (0..count)
        .map(|index| {
            let mut copy = template.clone();
            copy.file_path = format!("hostile/suites_{index:03}.py");
            copy
        })
        .collect();
    // The largest payload is the one whose path is longest: the path is in it.
    let one = extractions.iter().map(json_bytes).max().unwrap_or(0);
    let step = Step {
        extractions,
        opts: GenerationWriteOpts::default(),
    };
    let store = Store::open_in_memory().unwrap();
    lock_conn(&store.conn)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER stop_at_first BEFORE INSERT ON file_payloads
             BEGIN SELECT RAISE(ABORT, 'stop at the first insert'); END;",
        )
        .unwrap();
    PRODUCED.store(0, Ordering::SeqCst);
    let tuning = PayloadTuning {
        serialize: counts_what_it_produces,
        ..explicit(threads, window_bytes)
    };
    let error = write_step_tuned(&store, &step, &tuning).expect_err("first insert refused");
    assert!(error.to_string().contains("stop at the first insert"));
    (PRODUCED.load(Ordering::SeqCst), one)
}

/// The in-flight JSON of a real write of 64 hostile files is bounded by what the
/// documentation states, in actual bytes: the budget, plus the window's worth of
/// payloads handed out before the budget closed it. (Round 2's 64 files of
/// 1 MiB held 892 MB against a 64 MiB budget when the budget was an estimate.)
/// `ATTACK2_HOSTILE_KIB=1024` makes the files 1 MiB.
#[test]
fn a_real_write_holds_no_more_json_in_flight_than_the_budget_plus_the_window() {
    let production = PayloadTuning::default();
    for (threads, window_bytes) in [
        (4usize, production.window_bytes),
        (4, 4 * 1024 * 1024),
        (4, 0),
        (8, production.window_bytes),
    ] {
        let (in_flight, one) = hostile_in_flight(threads, window_bytes, 64);
        let window = 2 * threads;
        let bound = window_bytes + window * one;
        eprintln!(
            "{threads} threads, budget {window_bytes} B, payload {one} B: {in_flight} B in flight before the first insert, bound {bound} B"
        );
        assert!(
            in_flight <= bound,
            "{in_flight} B held in flight; budget {window_bytes} + {window} x {one} = {bound}"
        );
        assert!(in_flight >= one, "the head itself was produced");
    }
}

static STARTED: AtomicUsize = AtomicUsize::new(0);
static STARTED_WHEN_HEAD_RELEASED: AtomicUsize = AtomicUsize::new(0);

/// `mod_0` is the head and does not finish until three more extractions have
/// started and a moment has passed; whatever has started by then is exactly what
/// the window had room for.
fn slow_head(extraction: &Extraction) -> Result<PayloadJson> {
    STARTED.fetch_add(1, Ordering::SeqCst);
    if extraction.file_path.ends_with("/mod_0.py") {
        let waited = std::time::Instant::now();
        while STARTED.load(Ordering::SeqCst) < 4 && waited.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(1));
        }
        std::thread::sleep(Duration::from_millis(150));
        STARTED_WHEN_HEAD_RELEASED.store(STARTED.load(Ordering::SeqCst), Ordering::SeqCst);
    }
    serialize_payload(extraction)
}

/// The window is a window and not a barrier: while the head is still being
/// serialized the extractions behind it are, up to the window and no further.
/// Two threads make a window of four; twelve extractions are fresh.
#[test]
fn the_window_serializes_behind_a_slow_head_up_to_its_size_and_no_further() {
    STARTED.store(0, Ordering::SeqCst);
    STARTED_WHEN_HEAD_RELEASED.store(0, Ordering::SeqCst);
    let steps = scenario(11);
    let store = Store::open_in_memory().unwrap();
    write_step_tuned(
        &store,
        &steps[0],
        &PayloadTuning {
            serialize: slow_head,
            ..explicit(2, usize::MAX)
        },
    )
    .unwrap();
    assert_eq!(
        STARTED_WHEN_HEAD_RELEASED.load(Ordering::SeqCst),
        4,
        "while the head was pending the window of four extractions, and no more, had started"
    );
    assert_eq!(
        STARTED.load(Ordering::SeqCst),
        12,
        "every extraction was serialized"
    );
    let want = dump_after(&steps, &tuned(1));
    let again = Store::open_in_memory().unwrap();
    for step in &steps {
        write_step_tuned(&again, step, &explicit(2, usize::MAX)).unwrap();
    }
    assert_eq!(dump(&again), want);
}

/// Every join happens before an error is returned: a thread that panicked on
/// its own does not keep the rest from being joined. (A panic outside the
/// serializer's catch cannot be injected from here; this pins that the scope
/// returns, with every serializer call finished, when the first insert fails
/// while many are outstanding.)
#[test]
fn every_outstanding_job_has_finished_when_a_failed_write_returns() {
    static FINISHED: AtomicUsize = AtomicUsize::new(0);
    static BEGUN: AtomicUsize = AtomicUsize::new(0);
    fn slow_then_counted(extraction: &Extraction) -> Result<PayloadJson> {
        BEGUN.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(40));
        let outcome = serialize_payload(extraction);
        FINISHED.fetch_add(1, Ordering::SeqCst);
        outcome
    }
    let steps = scenario(20);
    let store = Store::open_in_memory().unwrap();
    lock_conn(&store.conn)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER stop_at_first BEFORE INSERT ON file_payloads
             BEGIN SELECT RAISE(ABORT, 'stop at the first insert'); END;",
        )
        .unwrap();
    let error = write_step_tuned(
        &store,
        &steps[0],
        &PayloadTuning {
            serialize: slow_then_counted,
            ..explicit(4, usize::MAX)
        },
    )
    .expect_err("first insert refused");
    assert!(error.to_string().contains("stop at the first insert"));
    assert_eq!(
        FINISHED.load(Ordering::SeqCst),
        BEGUN.load(Ordering::SeqCst),
        "a serializer call was still running when the write returned"
    );
    assert!(
        BEGUN.load(Ordering::SeqCst) <= 8,
        "more than the window was handed out"
    );
}

// --- the default thread count -----------------------------------------------------------

const DEFAULTS_CHILD: &str = "ATTACK2_DEFAULTS_CHILD";

/// `PayloadTuning::default()` reads `RAYON_NUM_THREADS` when it is a positive
/// number and otherwise the machine's parallelism. (That it touches no rayon API
/// is pinned by the thread-refusal test below, in a process where rayon cannot
/// start a pool.)
/// Run in a child process: the variable is read per call, and the parent's
/// other tests run concurrently.
#[test]
fn the_default_thread_count_honours_rayon_num_threads() {
    if std::env::var_os(DEFAULTS_CHILD).is_some() {
        return;
    }
    let ask = |value: Option<&str>| -> String {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "db::payload_serialization_tests::attack2::a2_defaults_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(DEFAULTS_CHILD, "1");
        match value {
            Some(value) => command.env("RAYON_NUM_THREADS", value),
            None => command.env_remove("RAYON_NUM_THREADS"),
        };
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(output.status.success(), "child failed: {stdout}");
        stdout
            .lines()
            .find_map(|line| line.split_once("DEFAULTS ").map(|(_, rest)| rest))
            .unwrap_or_default()
            .to_string()
    };
    let machine = std::thread::available_parallelism().map_or(1, |n| n.get());
    assert_eq!(ask(Some("3")), "threads=3");
    assert_eq!(ask(Some("0")), format!("threads={machine}"));
    assert_eq!(ask(Some("not a number")), format!("threads={machine}"));
    assert_eq!(ask(None), format!("threads={machine}"));
}

#[test]
fn a2_defaults_child() {
    if std::env::var_os(DEFAULTS_CHILD).is_none() {
        return;
    }
    println!("DEFAULTS threads={}", PayloadTuning::default().threads);
}
