//! OD 1: the one ledger schema bump (writer generation 1 to 2).
//!
//! Every test builds its "old" ledger from `fixtures/ledger_writer1_schema.sql`,
//! the schema exactly as writer generation 1 created it, so what is migrated is
//! a real old ledger rather than the current schema with a column removed.

use super::*;

const WRITER1_SCHEMA: &str = include_str!("fixtures/ledger_writer1_schema.sql");

const DEDUPED_INDEX: &str = "idx_events_reflog_identity";
const NEW_TABLES: [&str; 2] = ["reflog_watermarks", "catch_up_state"];

fn repo_dir() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().to_str().unwrap().to_string();
    (dir, repo)
}

/// Creates the ledger file for `repo` as a writer-generation-1 build left it:
/// the old schema, `user_version = 1`, and no migration run.
pub(super) fn old_ledger(repo: &str) -> PathBuf {
    let db = ledger_path(repo);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = Connection::open(&db).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.execute_batch(WRITER1_SCHEMA).unwrap();
    conn.pragma_update(None, "user_version", 1).unwrap();
    db
}

pub(super) fn old_insert(
    conn: &Connection,
    ulid: &str,
    worktree: Option<&str>,
    action: &str,
    object: Option<&str>,
    detail: Option<&str>,
) {
    conn.execute(
        "INSERT INTO events (ulid, ts_utc, repo_path, worktree_path, actor_kind,
                             action, object, outcome, detail_json)
         VALUES (?1, '2026-01-01T00:00:00.000Z', '/r', ?2, 'system', ?3, ?4, 'ok', ?5)",
        params![ulid, worktree, action, object, detail],
    )
    .unwrap();
}

fn stamp_detail(at: &str) -> String {
    format!(
        r#"{{"source":"reflog","occurred_at":"{at}","occurred_at_basis":"reflog","task_basis":"unbound"}}"#
    )
}

fn columns(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
        .unwrap();
    stmt.query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn has_object(conn: &Connection, kind: &str, name: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = ?1 AND name = ?2",
        params![kind, name],
        |row| row.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

fn version(db: &Path) -> i64 {
    Connection::open(db)
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap()
}

fn ulids(conn: &Connection) -> Vec<String> {
    let mut stmt = conn.prepare("SELECT ulid FROM events ORDER BY id").unwrap();
    stmt.query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// `(ulid, reflog_ordinal)` of every reflog row that has an object, by id.
fn ordinals(conn: &Connection) -> Vec<(String, Option<i64>)> {
    conn.prepare("SELECT ulid, reflog_ordinal FROM events WHERE action LIKE 'reflog.%' ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn assert_new_shape(conn: &Connection) {
    assert!(columns(conn, "events").contains(&"occurred_at".to_string()));
    assert!(columns(conn, "events").contains(&"reflog_ordinal".to_string()));
    assert!(columns(conn, "transcript_cursors").contains(&"fingerprint".to_string()));
    for table in NEW_TABLES {
        assert!(has_object(conn, "table", table), "missing table {table}");
    }
    assert!(
        has_object(conn, "index", DEDUPED_INDEX),
        "missing the unique reflog index"
    );
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = ?1",
            params![DEDUPED_INDEX],
            |row| row.get(0),
        )
        .unwrap();
    assert!(sql.contains("UNIQUE"), "{sql}");
}

#[test]
fn a_fresh_ledger_has_every_new_column_table_and_index() {
    let (_dir, repo) = repo_dir();
    let db = ledger_path(&repo);
    let conn = open(&db).expect("a fresh ledger opens");
    assert_new_shape(&conn);
    assert_eq!(version(&db), WRITER_VERSION);
    assert_eq!(WRITER_VERSION, 2, "exactly one bump for the five changes");
}

#[test]
fn an_old_ledger_is_migrated_with_every_row_kept_and_identical_reflog_rows_numbered() {
    let (_dir, repo) = repo_dir();
    let db = old_ledger(&repo);
    let old = Connection::open(&db).unwrap();
    let at1 = "2026-01-01T10:00:00.000Z";
    let at2 = "2026-01-02T10:00:00.000Z";
    // Pre-interim rows: no event time. The same identity three times.
    old_insert(
        &old,
        "A1",
        Some("/w/1"),
        "reflog.checkout",
        Some("abc msg"),
        Some("{}"),
    );
    old_insert(
        &old,
        "A2",
        Some("/w/1"),
        "reflog.checkout",
        Some("abc msg"),
        Some("{}"),
    );
    old_insert(
        &old,
        "A3",
        Some("/w/1"),
        "reflog.checkout",
        Some("abc msg"),
        None,
    );
    // Interim rows: the same identity at two different times is two entries.
    old_insert(
        &old,
        "B1",
        Some("/w/1"),
        "reflog.commit",
        Some("def msg"),
        Some(&stamp_detail(at1)),
    );
    old_insert(
        &old,
        "B2",
        Some("/w/1"),
        "reflog.commit",
        Some("def msg"),
        Some(&stamp_detail(at1)),
    );
    old_insert(
        &old,
        "B3",
        Some("/w/1"),
        "reflog.commit",
        Some("def msg"),
        Some(&stamp_detail(at2)),
    );
    // The anchor checkout stores a NULL worktree: NULLs compare equal here.
    old_insert(&old, "C1", None, "reflog.commit", Some("anchor msg"), None);
    old_insert(&old, "C2", None, "reflog.commit", Some("anchor msg"), None);
    // Two identities cut to the same text differ by the recorded hash.
    old_insert(
        &old,
        "D1",
        Some("/w/1"),
        "reflog.commit",
        Some("long"),
        Some(r#"{"object_truncated":{"hash":"h1"}}"#),
    );
    old_insert(
        &old,
        "D2",
        Some("/w/1"),
        "reflog.commit",
        Some("long"),
        Some(r#"{"object_truncated":{"hash":"h2"}}"#),
    );
    // Another worktree with the same identity is another row.
    old_insert(
        &old,
        "E1",
        Some("/w/2"),
        "reflog.checkout",
        Some("abc msg"),
        None,
    );
    // Rows that are not reflog rows are never deduped, whatever they repeat.
    old_insert(
        &old,
        "F1",
        Some("/w/1"),
        "session.tool_use",
        Some("abc msg"),
        None,
    );
    old_insert(
        &old,
        "F2",
        Some("/w/1"),
        "session.tool_use",
        Some("abc msg"),
        None,
    );
    old_insert(&old, "G1", Some("/w/1"), "reflog.commit", None, None);
    old_insert(&old, "G2", Some("/w/1"), "reflog.commit", None, None);
    old.execute(
        "INSERT INTO transcript_cursors (worktree_key, file_key, byte_offset) VALUES ('w', 'f', 77)",
        [],
    )
    .unwrap();
    drop(old);

    let conn = open(&db).expect("the migration runs on open");
    assert_new_shape(&conn);
    assert_eq!(version(&db), WRITER_VERSION);
    assert_eq!(
        ulids(&conn),
        [
            "A1", "A2", "A3", "B1", "B2", "B3", "C1", "C2", "D1", "D2", "E1", "F1", "F2", "G1",
            "G2"
        ],
        "the migration deletes nothing"
    );
    let numbered: Vec<(String, Option<i64>)> = ordinals(&conn);
    assert_eq!(
        numbered,
        [
            // A group of three identical rows is numbered 0, 1, 2 by id.
            ("A1", Some(0)),
            ("A2", Some(1)),
            ("A3", Some(2)),
            ("B1", Some(0)),
            ("B2", Some(1)),
            ("B3", Some(0)),
            ("C1", Some(0)),
            ("C2", Some(1)),
            ("D1", Some(0)),
            ("D2", Some(0)),
            ("E1", Some(0)),
            // An `object`-less reflog row is outside the index and unnumbered.
            ("G1", None),
            ("G2", None),
        ]
        .map(|(ulid, n)| (ulid.to_string(), n)),
        "ordinals count up within an identity group; groups never share a count"
    );
    let non_reflog: Vec<Option<i64>> = conn
        .prepare("SELECT reflog_ordinal FROM events WHERE ulid IN ('F1','F2')")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(non_reflog, [None, None], "only reflog rows are numbered");
    let offset: (i64, Option<String>) = conn
        .query_row(
            "SELECT byte_offset, fingerprint FROM transcript_cursors WHERE worktree_key = 'w'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        offset,
        (77, None),
        "cursors survive and the fingerprint is null"
    );
    let stamped: Vec<(String, Option<String>)> = conn
        .prepare(
            "SELECT ulid, occurred_at FROM events WHERE ulid IN ('A1','B1','B3','F1') ORDER BY id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        stamped,
        [
            ("A1".to_string(), None),
            ("B1".to_string(), Some(at1.to_string())),
            ("B3".to_string(), Some(at2.to_string())),
            ("F1".to_string(), None),
        ],
        "reflog rows keep the interim time in the typed column; nothing else is invented"
    );
}

#[test]
fn the_migration_is_idempotent_across_two_opens() {
    let (_dir, repo) = repo_dir();
    let db = old_ledger(&repo);
    let old = Connection::open(&db).unwrap();
    old_insert(&old, "A1", Some("/w/1"), "reflog.commit", Some("x"), None);
    old_insert(&old, "A2", Some("/w/1"), "reflog.commit", Some("x"), None);
    drop(old);

    let first = {
        let conn = open(&db).unwrap();
        (ulids(&conn), version(&db))
    };
    let second = {
        let conn = open(&db).unwrap();
        (ulids(&conn), version(&db))
    };
    assert_eq!(first, second);
    assert_eq!(first.0, ["A1", "A2"]);

    // Run again with the stamp forced back, as a crash after the commit's
    // data but before a stamp the next open can see would look.
    Connection::open(&db)
        .unwrap()
        .pragma_update(None, "user_version", 1)
        .unwrap();
    let conn = open(&db).unwrap();
    assert_eq!(ulids(&conn), ["A1", "A2"]);
    assert_eq!(
        ordinals(&conn),
        [("A1".to_string(), Some(0)), ("A2".to_string(), Some(1))],
        "a re-run renumbers to the same ordinals"
    );
    assert_new_shape(&conn);
    assert_eq!(version(&db), WRITER_VERSION);
}

#[test]
fn a_migration_interrupted_midway_leaves_the_old_ledger_and_the_next_open_finishes() {
    let (_dir, repo) = repo_dir();
    let db = old_ledger(&repo);
    let old = Connection::open(&db).unwrap();
    old_insert(&old, "A1", Some("/w/1"), "reflog.commit", Some("x"), None);
    old_insert(&old, "A2", Some("/w/1"), "reflog.commit", Some("x"), None);
    // Numbering the ordinals is the last data step; making it fail interrupts
    // the migration after the columns and tables were added inside the same
    // transaction.
    old.execute_batch(
        "CREATE TRIGGER interrupt BEFORE UPDATE OF reflog_ordinal ON events
         BEGIN SELECT RAISE(ABORT, 'interrupted'); END;",
    )
    .unwrap();
    drop(old);

    let error = open(&db).expect_err("the interrupted migration must fail loudly");
    assert_ne!(error.code, "writer_too_old", "{error:?}");
    let check = Connection::open(&db).unwrap();
    assert_eq!(version(&db), 1, "an unfinished migration must not stamp");
    assert!(
        !columns(&check, "events").contains(&"occurred_at".to_string()),
        "the columns added before the interruption must roll back with it"
    );
    for table in NEW_TABLES {
        assert!(
            !has_object(&check, "table", table),
            "{table} survived a rollback"
        );
    }
    assert!(!has_object(&check, "index", DEDUPED_INDEX));
    assert_eq!(ulids(&check), ["A1", "A2"], "no row was lost");
    check.execute_batch("DROP TRIGGER interrupt").unwrap();
    drop(check);

    let conn = open(&db).expect("the next open re-runs the migration cleanly");
    assert_new_shape(&conn);
    assert_eq!(ulids(&conn), ["A1", "A2"]);
    assert_eq!(
        ordinals(&conn),
        [("A1".to_string(), Some(0)), ("A2".to_string(), Some(1))]
    );
    assert_eq!(version(&db), WRITER_VERSION);
}

#[test]
fn a_half_applied_old_ledger_is_completed_not_failed() {
    // A ledger some earlier attempt left with the column already added.
    let (_dir, repo) = repo_dir();
    let db = old_ledger(&repo);
    let old = Connection::open(&db).unwrap();
    old.execute_batch("ALTER TABLE events ADD COLUMN occurred_at TEXT")
        .unwrap();
    old_insert(&old, "A1", Some("/w/1"), "reflog.commit", Some("x"), None);
    drop(old);
    let conn = open(&db).expect("a column that already exists is not an error");
    assert_new_shape(&conn);
}

#[test]
fn a_migrated_ledger_still_accepts_the_old_binarys_inserts() {
    let (_dir, repo) = repo_dir();
    let db = old_ledger(&repo);
    drop(Connection::open(&db).unwrap());
    drop(open(&db).unwrap());

    // What a writer-generation-1 binary runs, verbatim from main: named-column
    // inserts, explicit-column reads, the cursor upsert, the reflog key read.
    let conn = Connection::open(&db).unwrap();
    conn.execute(
        "INSERT INTO events (ulid, ts_utc, schema_version, repo_path, worktree_path,
             actor_kind, actor_id, session_id, task_id, action, object,
             argv_json, outcome, verdict_json, before_ref, after_ref,
             duration_ms, detail_json
         ) VALUES ('O1','2026-01-01T00:00:00.000Z',1,'/r',NULL,'human',NULL,NULL,NULL,
                   'git.commit',NULL,NULL,'ok',NULL,NULL,NULL,NULL,NULL)",
        [],
    )
    .expect("an old-shape insert must still be accepted");
    conn.execute(
        "INSERT INTO transcript_cursors (worktree_key, file_key, byte_offset)
         VALUES ('w', 'f', 1)
         ON CONFLICT(worktree_key, file_key) DO UPDATE SET byte_offset = excluded.byte_offset",
        [],
    )
    .expect("the old cursor upsert must still be accepted");
    let row: (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT occurred_at, (SELECT fingerprint FROM transcript_cursors LIMIT 1)
             FROM events WHERE ulid = 'O1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(row, (None, None), "columns an old writer ignores stay null");

    // The old build's own gate refuses a newer stamp: it cannot write, and it
    // cannot corrupt, because it never reaches an insert.
    assert!(
        version(&db) > 1,
        "an old build must see a stamp newer than its own"
    );
}

#[test]
fn a_newer_ledger_is_refused_for_writes_and_left_unaltered() {
    let (_dir, repo) = repo_dir();
    let db = old_ledger(&repo);
    Connection::open(&db)
        .unwrap()
        .pragma_update(None, "user_version", WRITER_VERSION + 1)
        .unwrap();
    let error = open(&db).expect_err("a newer ledger must be refused");
    assert_eq!(error.code, "writer_too_old");
    let check = Connection::open(&db).unwrap();
    assert!(!columns(&check, "events").contains(&"occurred_at".to_string()));
    assert_eq!(version(&db), WRITER_VERSION + 1);
    assert!(append(Draft {
        repo_path: repo.clone(),
        action: "git.commit".into(),
        ..Default::default()
    })
    .is_err());
}

/// Append cost of the OD 1 index: 10 000 real appends per arm into a fresh
/// ledger, arms interleaved every round, minimum of 7 rounds, for an ordinary
/// workload and for a reflog workload (the only rows the index covers). Run
/// alone: `GITPULSE_TEST_WRITE_BUDGET=0 cargo test --manifest-path
/// src-tauri/Cargo.toml --lib -- --ignored --nocapture
/// ledger::schema_bump_tests::bench_append_cost_of_the_reflog_identity_index`
#[test]
#[ignore = "benchmark: 280 000 real appends; run alone with GITPULSE_TEST_WRITE_BUDGET=0"]
fn bench_append_cost_of_the_reflog_identity_index() {
    use std::time::{Duration, Instant};
    let rows = 10_000usize;
    for workload in ["session", "reflog"] {
        let mut best = [Duration::MAX; 2];
        for round in 0..7 {
            for step in 0..2 {
                let with_index = (round + step) % 2 == 1;
                let (_dir, repo) = repo_dir();
                with_conn(&repo, |conn| {
                    if !with_index {
                        conn.execute_batch(&format!("DROP INDEX {DEDUPED_INDEX}"))
                            .map_err(|e| LedgerError::new("t", e.to_string()))?;
                    }
                    Ok(())
                })
                .unwrap();
                let started = Instant::now();
                for n in 0..rows {
                    let mut draft = if workload == "reflog" {
                        reflog_draft(
                            &repo,
                            Some(&ids::iso8601_utc(1_700_000_000_000 + n as u64 * 1000)),
                        )
                    } else {
                        Draft {
                            repo_path: repo.clone(),
                            action: "session.command".into(),
                            actor_kind: Some(ActorKind::Agent),
                            outcome: Some(Outcome::Ok),
                            ..Default::default()
                        }
                    };
                    draft.object = Some(format!("{n:040x} echo {n}"));
                    append(draft).unwrap();
                }
                let slot = usize::from(with_index);
                best[slot] = best[slot].min(started.elapsed());
            }
        }
        println!(
            "APPENDBENCH {workload}: without index {:?} ({:?}/append), with index {:?} ({:?}/append), ratio {:.3}",
            best[0],
            best[0] / rows as u32,
            best[1],
            best[1] / rows as u32,
            best[1].as_secs_f64() / best[0].as_secs_f64()
        );
    }
}

fn reflog_draft(repo: &str, at: Option<&str>) -> Draft {
    Draft {
        repo_path: repo.to_string(),
        worktree_path: Some("/w/1".into()),
        action: "reflog.commit".into(),
        object: Some("abc msg".into()),
        actor_kind: Some(ActorKind::System),
        outcome: Some(Outcome::Ok),
        event_stamp: at.map(|at| EventStamp {
            occurred_at: at.into(),
            occurred_at_basis: "reflog".into(),
            task_basis: "unbound".into(),
        }),
        ..Default::default()
    }
}

#[test]
fn the_writer_fills_the_typed_column_only_from_a_well_formed_stamp() {
    let (_dir, repo) = repo_dir();
    let at = "2026-03-04T05:06:07.000Z";
    append(reflog_draft(&repo, Some(at))).unwrap();
    let mut bad = reflog_draft(&repo, Some("not a time"));
    bad.object = Some("other".into());
    append(bad).unwrap();
    let mut none = reflog_draft(&repo, None);
    none.object = Some("third".into());
    append(none).unwrap();
    with_conn(&repo, |conn| {
        let values: Vec<Option<String>> = conn
            .prepare("SELECT occurred_at FROM events ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(values, [Some(at.to_string()), None, None]);
        assert_eq!(
            ordinals(conn).iter().map(|(_, n)| *n).collect::<Vec<_>>(),
            [Some(0), Some(0), Some(0)],
            "this lane writes every reflog row with ordinal 0"
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn a_racing_duplicate_reflog_row_is_refused_by_the_index_and_is_not_a_dropped_append() {
    let (_dir, repo) = repo_dir();
    let at = "2026-03-04T05:06:07.000Z";
    append(reflog_draft(&repo, Some(at))).unwrap();
    let error = append(reflog_draft(&repo, Some(at))).expect_err("the same entry twice");
    assert_eq!(error.code, "duplicate_reflog");
    assert_eq!(dropped_appends(&repo), 0, "a duplicate is not data loss");
    // The same identity at another time is another entry.
    append(reflog_draft(&repo, Some("2026-03-04T05:06:08.000Z"))).unwrap();
    with_conn(&repo, |conn| {
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
        Ok(())
    })
    .unwrap();
}
