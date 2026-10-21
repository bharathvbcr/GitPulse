//! Adversarial review of OD 1 (writer generation 1 -> 2). Tests that PASS are
//! attacks that held. Tests marked `#[ignore = "adv: ..."]` are real breaks:
//! each fails against this branch and names why. Measurement tests read
//! `ADV_ROWS` and print their numbers.

use super::schema_bump_tests::{old_insert, old_ledger};
use super::*;
use crate::procguard::LockedSpawn as _;
use rusqlite::types::Value;
use std::collections::HashSet;
use std::io::Write as _;
use std::time::{Duration, Instant};

const ORIGINAL_COLUMNS: [&str; 19] = [
    "id",
    "ulid",
    "ts_utc",
    "schema_version",
    "repo_path",
    "worktree_path",
    "actor_kind",
    "actor_id",
    "session_id",
    "task_id",
    "action",
    "object",
    "argv_json",
    "outcome",
    "verdict_json",
    "before_ref",
    "after_ref",
    "duration_ms",
    "detail_json",
];

fn repo_dir() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().to_str().unwrap().to_string();
    (dir, repo)
}

/// Type and exact bytes of every original column of every row.
fn snapshot(conn: &Connection) -> Vec<Vec<String>> {
    let select = ORIGINAL_COLUMNS
        .iter()
        .map(|c| format!("typeof({c}) || ':' || hex(CAST({c} AS BLOB))"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = conn
        .prepare(&format!("SELECT {select} FROM events ORDER BY id"))
        .unwrap();
    stmt.query_map([], |row| {
        (0..ORIGINAL_COLUMNS.len())
            .map(|i| row.get::<_, String>(i))
            .collect::<Result<Vec<_>, _>>()
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

fn stamp_json(at: &str) -> String {
    format!(
        r#"{{"source":"reflog","occurred_at":"{at}","occurred_at_basis":"reflog","task_basis":"unbound"}}"#
    )
}

fn file_hash(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    Some(format!("{:016x} len={}", hasher.finish(), bytes.len()))
}

// ---------------------------------------------------------------------------
// 1. Data loss: fuzz generation-1 ledgers with a FIXED seed.
// ---------------------------------------------------------------------------

fn fuzz_actions() -> Vec<&'static str> {
    vec![
        "reflog.checkout",
        "reflog.commit",
        "REFLOG.Move",
        "session.start",
        "tool.use",
        "reflogx.y",
    ]
}

fn fuzz_worktrees() -> Vec<Value> {
    vec![
        Value::Null,
        Value::Text(String::new()),
        Value::Text("/w1".into()),
        Value::Text("/w2".into()),
        Value::Blob(b"/w1".to_vec()),
    ]
}

fn fuzz_objects() -> Vec<Value> {
    vec![
        Value::Null,
        Value::Text(String::new()),
        Value::Text("a".into()),
        Value::Text("b".into()),
        Value::Text("x".repeat(20_000)),
        Value::Blob(b"a".to_vec()),
    ]
}

fn fuzz_details() -> Vec<Value> {
    let at1 = "2026-01-01T00:00:00.000Z";
    let at2 = "2026-01-01T00:00:01.000Z";
    let text = |s: &str| Value::Text(s.to_string());
    vec![
        Value::Null,
        text(""),
        text("not json"),
        text(&stamp_json(at1)),
        text(&stamp_json(at2)),
        text(r#"{"occurred_at":5}"#),
        text(r#"{"occurred_at":"5"}"#),
        text(r#"{"occurred_at":1e999}"#),
        text(r#"{"occurred_at":NaN}"#),
        text(r#"{"occurred_at":"2026-01-01T00:00:00.000Zjunk"}"#),
        text(r#"{"occurred_at":null}"#),
        text(r#"{"occurred_at":"9999-99-99T99:99:99.999Z"}"#),
        text(r#"{"object_truncated":{"hash":"h1"}}"#),
        text(r#"{"object_truncated":{"hash":1}}"#),
        text(r#"{"object_truncated":{"hash":1.0}}"#),
        text(r#"{"object_truncated":{"hash":[1,2]}}"#),
        text(&format!(r#"{{"occurred_at":"{}"}}"#, "9".repeat(200_000))),
        Value::Blob(b"{\"occurred_at\":1}".to_vec()),
    ]
}

fn insert_fuzz_row(conn: &Connection, i: usize, spec: (usize, usize, usize, usize)) {
    let actions = fuzz_actions();
    let worktrees = fuzz_worktrees();
    let objects = fuzz_objects();
    let details = fuzz_details();
    conn.execute(
        "INSERT INTO events (ulid, ts_utc, repo_path, worktree_path, actor_kind,
                             action, object, outcome, detail_json)
         VALUES (?1, '2026-01-01T00:00:00.000Z', '/r', ?2, 'system', ?3, ?4, 'ok', ?5)",
        params![
            format!("F{i}"),
            worktrees[spec.1],
            actions[spec.0],
            objects[spec.2],
            details[spec.3]
        ],
    )
    .unwrap();
}

#[test]
fn fuzzed_generation_1_ledgers_migrate_without_changing_any_original_column() {
    use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
    let groups_seen = std::cell::Cell::new(0usize);
    let reflog_rows = std::cell::Cell::new(0usize);
    let mut runner = TestRunner::new_with_rng(
        Config {
            cases: 64,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[0x0d; 32]),
    );
    let strategy = proptest::collection::vec(
        (
            0usize..fuzz_actions().len(),
            0usize..fuzz_worktrees().len(),
            0usize..fuzz_objects().len(),
            0usize..fuzz_details().len(),
        ),
        0..40,
    );
    runner
        .run(&strategy, |rows| {
            let (_dir, repo) = repo_dir();
            let db = old_ledger(&repo);
            {
                let old = Connection::open(&db).unwrap();
                for (i, spec) in rows.iter().enumerate() {
                    insert_fuzz_row(&old, i, *spec);
                }
            }
            let before = {
                let old = Connection::open(&db).unwrap();
                snapshot(&old)
            };
            let conn = open(&db).expect("the migration must not fail on any generation-1 ledger");
            let after = snapshot(&conn);
            assert_eq!(before, after, "a migrated row's original columns changed");
            let version: i64 = conn
                .pragma_query_value(None, "user_version", |r| r.get(0))
                .unwrap();
            assert_eq!(version, WRITER_VERSION);
            let integrity: String = conn
                .pragma_query_value(None, "integrity_check", |r| r.get(0))
                .unwrap();
            assert_eq!(integrity, "ok");
            let reflog: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM events WHERE {REFLOG_ROWS}"),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let distinct: i64 = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM (SELECT DISTINCT {} FROM events WHERE {REFLOG_ROWS})",
                        IdentityParts::COLUMNS.key()
                    ),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            reflog_rows.set(reflog_rows.get() + reflog as usize);
            if distinct < reflog {
                groups_seen.set(groups_seen.get() + 1);
            }
            drop(conn);
            // A second open is a no-op on the stamp and on the rows.
            let again = open(&db).expect("reopen");
            assert_eq!(snapshot(&again), after);
            Ok(())
        })
        .unwrap();
    assert!(
        groups_seen.get() > 5,
        "the generator must hit duplicate groups, hit {}",
        groups_seen.get()
    );
    assert!(reflog_rows.get() > 100);
}

// ---------------------------------------------------------------------------
// 2. Index build failure and its atomicity.
// ---------------------------------------------------------------------------

#[test]
fn a_failed_migration_leaves_stamp_1_and_the_old_shape_and_both_generations_can_write() {
    let (_dir, repo) = repo_dir();
    let db = old_ledger(&repo);
    {
        let old = Connection::open(&db).unwrap();
        old_insert(&old, "A", Some("/w"), "reflog.checkout", Some("o"), None);
        old_insert(&old, "B", Some("/w"), "reflog.checkout", Some("o"), None);
        // A trigger the migration's UPDATE trips: the backfill fails after the
        // ALTERs have run.
        old.execute_batch(
            "CREATE TRIGGER adv_boom BEFORE UPDATE ON events
             BEGIN SELECT RAISE(ABORT, 'adv boom'); END;",
        )
        .unwrap();
    }
    let before = {
        let c = Connection::open(&db).unwrap();
        snapshot(&c)
    };
    let error = open(&db).expect_err("the migration is made to fail");
    assert_eq!(error.code, "migration_failed", "{error:?}");
    assert!(error.message.contains("adv boom"), "{error:?}");
    let c = Connection::open(&db).unwrap();
    let version: i64 = c
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1, "a failed migration must not stamp");
    let cols: Vec<String> = c
        .prepare("SELECT name FROM pragma_table_info('events')")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        !cols.contains(&"occurred_at".to_string()),
        "ALTER leaked out of the rolled-back transaction"
    );
    assert_eq!(snapshot(&c), before);
    // Generation 1's own insert (named columns) still works on it.
    old_insert(&c, "C", Some("/w"), "reflog.checkout", Some("o"), None);
    // Repeated attempts fail the same clear way, never hang.
    let started = Instant::now();
    for _ in 0..3 {
        assert_eq!(
            open(&db).expect_err("still failing").code,
            "migration_failed"
        );
    }
    assert!(started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(5)));
}

#[test]
fn the_index_cannot_fail_to_build_over_coalesce_collisions() {
    let (_dir, repo) = repo_dir();
    let db = old_ledger(&repo);
    {
        let old = Connection::open(&db).unwrap();
        // NULL vs '' worktree, NULL vs '' time (no detail vs {}), cut and
        // un-cut with the same text.
        for (i, wt) in [None, Some(""), None, Some("")].iter().enumerate() {
            old_insert(&old, &format!("W{i}"), *wt, "reflog.c", Some("o"), None);
        }
        old_insert(&old, "H1", Some("/w"), "reflog.c", Some("o"), Some("{}"));
        old_insert(&old, "H2", Some("/w"), "reflog.c", Some("o"), None);
        old_insert(
            &old,
            "H3",
            Some("/w"),
            "reflog.c",
            Some("o"),
            Some(r#"{"object_truncated":{"hash":""}}"#),
        );
        old_insert(
            &old,
            "H4",
            Some("/w"),
            "reflog.c",
            Some("o"),
            Some(r#"{"object_truncated":{"hash":null}}"#),
        );
    }
    let conn = open(&db).expect("migrates");
    // The first row of a group keeps a NULL ordinal, which the index reads as 0.
    let ordinals: Vec<i64> = conn
        .prepare("SELECT COALESCE(reflog_ordinal, 0) FROM events ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    // W0..W3 are one group; H1..H4 are one group (NULL/''/{} all coalesce to '').
    assert_eq!(ordinals, [0, 1, 2, 3, 0, 1, 2, 3]);
}

#[test]
fn a_stale_index_of_the_same_name_is_replaced_not_trusted() {
    let (_dir, repo) = repo_dir();
    let db = old_ledger(&repo);
    {
        let old = Connection::open(&db).unwrap();
        old_insert(&old, "A", Some("/w"), "reflog.c", Some("o"), None);
        // An earlier build's unique-by-object index under this name.
        old.execute_batch(
            "CREATE UNIQUE INDEX idx_events_reflog_identity ON events(object)
               WHERE action LIKE 'reflog.%';",
        )
        .unwrap();
    }
    let conn = open(&db).expect("opens");
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'idx_events_reflog_identity'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        sql.contains("reflog_ordinal"),
        "the stamped-2 ledger holds a stale identity index: {sql}"
    );
}

// ---------------------------------------------------------------------------
// 3. Concurrency, and 4. crash + timings.
// ---------------------------------------------------------------------------

const OPEN_CHILD_ENV: &str = "ADV_OPEN_DB";

/// Not a test on its own: the child half of the concurrency and crash tests.
#[test]
fn adv_open_child() {
    let Ok(path) = std::env::var(OPEN_CHILD_ENV) else {
        return;
    };
    let started = Instant::now();
    let result = open(Path::new(&path));
    let ms = started.elapsed().as_millis();
    match result {
        Ok(_) => println!("ADV_RESULT ok {ms}"),
        Err(e) => println!("ADV_RESULT err {} {} {ms}", e.code, e.message),
    }
    let _ = std::io::stdout().flush();
}

fn rows_from_env(default: usize) -> usize {
    std::env::var("ADV_ROWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// A realistic old ledger: 60% reflog rows (a duplicate of the previous
/// identity every 20th row), the rest tool events.
fn big_old_ledger(repo: &str, rows: usize) -> PathBuf {
    let db = old_ledger(repo);
    let mut conn = Connection::open(&db).unwrap();
    let tx = conn.transaction().unwrap();
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO events (ulid, ts_utc, repo_path, worktree_path, actor_kind,
                                     action, object, outcome, detail_json)
                 VALUES (?1, '2026-01-01T00:00:00.000Z', '/r', ?2, 'system', ?3, ?4, 'ok', ?5)",
            )
            .unwrap();
        for i in 0..rows {
            if i % 10 < 6 {
                let g = if i % 20 == 1 { i - 1 } else { i };
                let at = format!(
                    "2026-{:02}-{:02}T{:02}:{:02}:{:02}.000Z",
                    1 + (g / 2_000_000) % 12,
                    1 + (g / 86_400) % 28,
                    (g / 3_600) % 24,
                    (g / 60) % 60,
                    g % 60
                );
                stmt.execute(params![
                    format!("U{i}"),
                    format!("/w/{}", g % 3),
                    "reflog.checkout",
                    format!("{g:040x} checkout: moving from a to b{}", g % 50),
                    stamp_json(&at)
                ])
                .unwrap();
            } else {
                stmt.execute(params![
                    format!("U{i}"),
                    Value::Null,
                    "tool.use",
                    Value::Null,
                    "{}"
                ])
                .unwrap();
            }
        }
    }
    tx.commit().unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    db
}

fn run_child(db: &Path) -> std::process::Child {
    std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "ledger::schema_bump_adv_tests::adv_open_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(OPEN_CHILD_ENV, db)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn_locked()
        .expect("spawn child")
}

fn child_result(child: std::process::Child) -> String {
    let out = child.wait_with_output().expect("child output");
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find_map(|l| l.split("ADV_RESULT ").nth(1).map(str::to_string))
        .unwrap_or_else(|| format!("no result: {text} {}", String::from_utf8_lossy(&out.stderr)))
}

#[test]
fn adv_migration_time_by_rows() {
    let rows = rows_from_env(100_000);
    let (_dir, repo) = repo_dir();
    let built = Instant::now();
    let db = big_old_ledger(&repo, rows);
    println!(
        "ADV built {rows} rows in {} ms",
        built.elapsed().as_millis()
    );
    let started = Instant::now();
    let conn = open(&db).expect("migrates");
    println!(
        "ADV migrate {rows} rows: {} ms (busy_timeout default is 5000 ms)",
        started.elapsed().as_millis()
    );
    let (n, groups): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), COUNT(DISTINCT reflog_ordinal) FROM events",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    println!("ADV rows={n} distinct_ordinals={groups}");
    let started = Instant::now();
    drop(conn);
    let _ = open(&db).expect("reopen");
    println!(
        "ADV reopen of a stamp-2 ledger: {} ms",
        started.elapsed().as_millis()
    );
}

#[test]
fn two_processes_opening_one_old_ledger_migrate_it_once_and_both_succeed() {
    let rows = rows_from_env(30_000);
    let (_dir, repo) = repo_dir();
    let db = big_old_ledger(&repo, rows);
    let a = run_child(&db);
    std::thread::sleep(Duration::from_millis(30));
    let b = run_child(&db);
    let (ra, rb) = (child_result(a), child_result(b));
    println!("ADV concurrent rows={rows}: first={ra} second={rb}");
    assert!(ra.starts_with("ok"), "first: {ra}");
    assert!(rb.starts_with("ok"), "second: {rb}");
    let conn = Connection::open(&db).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, WRITER_VERSION);
}

#[test]
fn a_second_opener_survives_a_migration_longer_than_the_busy_timeout() {
    let (_dir, repo) = repo_dir();
    let db = old_ledger(&repo);
    let holder = Connection::open(&db).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let child = run_child(&db);
    std::thread::sleep(Duration::from_secs(8));
    holder.execute_batch("ROLLBACK").unwrap();
    let result = child_result(child);
    assert!(
        result.starts_with("ok"),
        "the second opener fails startup instead of waiting out the migration: {result}"
    );
}

/// Characterization, renamed from
/// `a_live_generation_1_connection_keeps_working_exactly_as_before_the_migration`:
/// the stamp is read at open, so a generation-1 process already running keeps
/// writing after a generation-2 build migrated the ledger (documented on
/// [`WRITER_VERSION`]; the fix is to restart it). Its rows carry no typed event
/// time, and a duplicate reflog insert from it hits the unique index.
#[test]
fn a_live_generation_1_connection_is_not_stopped_by_the_stamp_and_its_duplicate_reflog_insert_hits_the_unique_index(
) {
    let (_dir, repo) = repo_dir();
    let db = old_ledger(&repo);
    let live_old = Connection::open(&db).unwrap();
    old_insert(&live_old, "A0", Some("/w"), "reflog.c", Some("seed"), None);
    drop(open(&db).unwrap());
    let at = stamp_json("2026-01-01T00:00:00.000Z");
    // The generation-1 writer's own INSERT (named columns), twice, same pass.
    old_insert(
        &live_old,
        "A1",
        Some("/w"),
        "reflog.c",
        Some("o"),
        Some(&at),
    );
    let second = live_old.execute(
        "INSERT INTO events (ulid, ts_utc, repo_path, worktree_path, actor_kind,
                             action, object, outcome, detail_json)
         VALUES ('A2', '2026-01-01T00:00:00.000Z', '/r', '/w', 'system',
                 'reflog.c', 'o', 'ok', ?1)",
        params![at],
    );
    let refused = second.expect_err("the unique index refuses the identical second insert");
    assert!(
        refused.to_string().contains(REFLOG_IDENTITY_INDEX),
        "refused by the identity index: {refused}"
    );
    let untyped: i64 = live_old
        .query_row(
            "SELECT COUNT(*) FROM events WHERE ulid = 'A1' AND occurred_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        untyped, 1,
        "the live old writer's row has no typed event time"
    );
}

#[test]
fn a_reader_holding_a_read_transaction_does_not_block_or_fail_the_migration() {
    let (_dir, repo) = repo_dir();
    let db = big_old_ledger(&repo, 5_000);
    let reader = Connection::open(&db).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let before: i64 = reader
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .unwrap();
    let started = Instant::now();
    let conn = open(&db).expect("migration with a reader parked");
    assert!(started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(4)));
    drop(conn);
    // The parked reader still sees its snapshot, then the new schema.
    let during: i64 = reader
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, during);
    reader.execute_batch("COMMIT").unwrap();
    let c: i64 = reader
        .query_row("SELECT COUNT(reflog_ordinal) FROM events", [], |r| r.get(0))
        .unwrap();
    assert!(c > 0);
}

#[test]
fn a_child_killed_mid_migration_leaves_a_ledger_the_next_open_finishes() {
    let rows = rows_from_env(200_000);
    let (_dir, repo) = repo_dir();
    let db = big_old_ledger(&repo, rows);
    let before = {
        let c = Connection::open(&db).unwrap();
        snapshot(&c)
    };
    let mut child = run_child(&db);
    std::thread::sleep(Duration::from_millis(400));
    child.kill().expect("SIGKILL our own child");
    let _ = child.wait();
    let stamp = {
        let c = Connection::open(&db).unwrap();
        c.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
            .unwrap()
    };
    println!("ADV kill -9 at 400ms: stamp afterwards = {stamp}");
    let started = Instant::now();
    let conn = open(&db).expect("the next open finishes the migration");
    println!(
        "ADV recovery open after kill: {} ms",
        started.elapsed().as_millis()
    );
    assert_eq!(snapshot(&conn), before);
    let integrity: String = conn
        .pragma_query_value(None, "integrity_check", |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, WRITER_VERSION);
    // Only duplicates are numbered, so a NULL ordinal is normal; what must
    // hold is that no two reflog rows share an identity once the ordinal (NULL
    // read as 0) is part of it.
    let key = IdentityParts::COLUMNS.key();
    let (rows, distinct): (i64, i64) = conn
        .query_row(
            &format!(
                "SELECT (SELECT COUNT(*) FROM events WHERE {REFLOG_ROWS}),
                        (SELECT COUNT(*) FROM (SELECT DISTINCT {key}, COALESCE(reflog_ordinal, 0)
                                               FROM events WHERE {REFLOG_ROWS}))"
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(rows, distinct);
}

// ---------------------------------------------------------------------------
// 5. Writer semantics.
// ---------------------------------------------------------------------------

fn reflog_draft(repo: &str, at: &str) -> Draft {
    Draft {
        repo_path: repo.to_string(),
        worktree_path: Some("/w/1".into()),
        action: "reflog.reset".into(),
        object: Some("abc reset: moving to HEAD".into()),
        actor_kind: Some(ActorKind::System),
        outcome: Some(Outcome::Ok),
        event_stamp: Some(EventStamp {
            occurred_at: at.into(),
            occurred_at_basis: "reflog".into(),
            task_basis: "unbound".into(),
        }),
        ..Default::default()
    }
}

/// Renamed from `a_second_identical_entry_in_the_same_second_is_still_recorded`:
/// the replay numbers identical entries of a pass 0, 1, 2, so the second is
/// recorded under the next ordinal; the same ordinal twice is the duplicate.
#[test]
fn a_second_identical_entry_in_the_same_second_is_recorded_under_the_next_ordinal() {
    let (_dir, repo) = repo_dir();
    let at = "2026-03-04T05:06:07.000Z";
    append(reflog_draft(&repo, at)).expect("first");
    append_numbered(reflog_draft(&repo, at), 1).expect("the second identical entry has ordinal 1");
    assert_eq!(
        append(reflog_draft(&repo, at))
            .expect_err("ordinal 0 again is the same entry")
            .code,
        "duplicate_reflog"
    );
}

/// Renamed from `a_refused_duplicate_reflog_row_is_counted_somewhere_a_reader_can_see`:
/// the ledger reports the refusal as `Recorded::Duplicate` (the replay counts it
/// in `CatchUp::duplicates_refused`, see `ingest::od1_adv_tests`) and never as a
/// dropped append.
#[test]
fn a_refused_duplicate_reflog_row_is_reported_as_a_duplicate_and_never_as_dropped() {
    let (_dir, repo) = repo_dir();
    let at = "2026-03-04T05:06:07.000Z";
    assert!(matches!(
        record_outcome(reflog_draft(&repo, at), 0),
        Recorded::Appended(_)
    ));
    assert_eq!(
        record_outcome(reflog_draft(&repo, at), 0),
        Recorded::Duplicate
    );
    assert_eq!(status(&repo).dropped, 0, "a duplicate is not a drop");
}

#[test]
fn legacy_import_keeps_two_identical_reflog_rows_like_the_migration_does() {
    let (_dir, anchor) = repo_dir();
    let event = |id: i64, ulid: &str| LedgerEvent {
        id,
        ulid: ulid.into(),
        ts_utc: "2026-01-01T00:00:00.000Z".into(),
        schema_version: 1,
        repo_path: anchor.clone(),
        worktree_path: Some("/w/1".into()),
        actor_kind: "system".into(),
        actor_id: None,
        session_id: None,
        task_id: None,
        action: "reflog.reset".into(),
        object: Some("abc reset: moving to HEAD".into()),
        argv_json: None,
        outcome: "ok".into(),
        verdict_json: None,
        before_ref: None,
        after_ref: None,
        duration_ms: None,
        detail_json: None,
    };
    let page = [
        event(1, "01AAAAAAAAAAAAAAAAAAAAAAAA"),
        event(2, "01BBBBBBBBBBBBBBBBBBBBBBBB"),
    ];
    let (imported, _) =
        import_legacy_page(&anchor, &"a".repeat(40), "sibling", &page, 2).expect("import");
    assert_eq!(
        imported, 2,
        "one of two distinct-ULID rows was silently skipped"
    );
    let numbered = |anchor: &str| -> Vec<(String, Option<i64>)> {
        with_conn(anchor, |conn| {
            Ok(conn
                .prepare("SELECT ulid, reflog_ordinal FROM events ORDER BY id")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap())
        })
        .unwrap()
    };
    let rows = numbered(&anchor);
    assert_eq!(
        rows.iter().map(|(_, n)| n.unwrap_or(0)).collect::<Vec<_>>(),
        [0, 1],
        "two identical source rows import as ordinals 0 and 1, as the migration numbers them"
    );
    // A re-import is stopped by the ULID: nothing new, nothing renumbered.
    let (again, _) =
        import_legacy_page(&anchor, &"a".repeat(40), "sibling", &page, 2).expect("re-import");
    assert_eq!(again, 0, "a re-import must be idempotent");
    assert_eq!(numbered(&anchor), rows);
}

// ---------------------------------------------------------------------------
// 6/7. Old-binary interop and downgrade.
// ---------------------------------------------------------------------------

#[test]
fn a_stamp_3_ledger_is_refused_for_writes_clearly_and_not_modified() {
    let (_dir, repo) = repo_dir();
    let db = old_ledger(&repo);
    {
        let c = Connection::open(&db).unwrap();
        old_insert(&c, "A", Some("/w"), "reflog.c", Some("o"), None);
        c.pragma_update(None, "user_version", 3).unwrap();
        c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    }
    let wal = PathBuf::from(format!("{}-wal", db.display()));
    let before = (file_hash(&db), file_hash(&wal));
    let error = open(&db).expect_err("refused");
    assert_eq!(error.code, "writer_too_old");
    assert!(error.message.contains("generation 3"), "{error:?}");
    assert!(error.message.contains("Update GitPulse"), "{error:?}");
    let after = (file_hash(&db), file_hash(&wal));
    assert_eq!(before.0, after.0, "the main database file changed");
    // And through the public append path.
    let error =
        append(reflog_draft(&repo, "2026-03-04T05:06:07.000Z")).expect_err("append is refused too");
    assert_eq!(error.code, "writer_too_old");
    assert_eq!(file_hash(&db), before.0);
}

#[test]
fn a_refused_stamp_3_ledger_in_delete_mode_is_byte_identical() {
    let (_dir, repo) = repo_dir();
    let db = ledger_path(&repo);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    {
        let c = Connection::open(&db).unwrap();
        c.pragma_update(None, "journal_mode", "DELETE").unwrap();
        c.execute_batch(include_str!("fixtures/ledger_writer1_schema.sql"))
            .unwrap();
        c.pragma_update(None, "user_version", 3).unwrap();
    }
    let before = file_hash(&db);
    let _ = open(&db).expect_err("refused");
    assert_eq!(file_hash(&db), before);
}

#[test]
fn a_migrated_ledger_is_still_readable_read_only_by_named_column_queries() {
    let (_dir, repo) = repo_dir();
    let db = old_ledger(&repo);
    {
        let c = Connection::open(&db).unwrap();
        old_insert(&c, "A", Some("/w"), "reflog.c", Some("o"), None);
    }
    drop(open(&db).unwrap());
    // The generation-1 reader path: read-only open, the tail query by name.
    let conn = open_readonly(&db).unwrap().unwrap();
    let rows = tail_rows_for_adv(&conn);
    assert_eq!(rows, 1);
    // Every `SELECT *` in this crate is test code (rg `SELECT \*` ledger/ ingest/ bin/).
}

fn tail_rows_for_adv(conn: &Connection) -> usize {
    let mut stmt = conn
        .prepare(
            "SELECT id, ulid, ts_utc, schema_version, repo_path, worktree_path,
                    actor_kind, actor_id, session_id, task_id, action, object,
                    argv_json, outcome, verdict_json, before_ref, after_ref,
                    duration_ms, detail_json
             FROM events WHERE id > ?1 ORDER BY id LIMIT 10",
        )
        .unwrap();
    stmt.query_map([0], event_from_row).unwrap().count()
}

// ---------------------------------------------------------------------------
// 8. Plans: write a generation-1 and a migrated copy of the same data for the
// external EXPLAIN QUERY PLAN diff (ADV_OUT=<dir>).
// ---------------------------------------------------------------------------

#[test]
fn adv_emit_plan_databases() {
    let Ok(out) = std::env::var("ADV_OUT") else {
        return;
    };
    let out = PathBuf::from(out);
    let mut seen = HashSet::new();
    for (name, migrate) in [("gen1.sqlite", false), ("gen2.sqlite", true)] {
        let (_dir, repo) = repo_dir();
        let db = big_old_ledger(&repo, 40_000);
        if migrate {
            let conn = open(&db).unwrap();
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
                .unwrap();
        }
        let target = out.join(name);
        std::fs::copy(&db, &target).unwrap();
        assert!(seen.insert(target));
    }
}
