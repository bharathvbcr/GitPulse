use super::*;
use crate::schema::declared_index_names;
use rusqlite::params;

fn scratch(label: &str) -> std::path::PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("devmap-{label}-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// The reclaim cap is a byte budget, so it means the same at any page size.
///
/// It was `65_536` pages, calibrated when every store had 4 KiB pages. The
/// doc on the constant says the bound exists because incremental vacuum
/// costs time proportional to the pages it moves — and pages are not a
/// fixed amount of work once two page sizes are in play. Left in pages, the
/// same constant licensed 256 MiB of moving on a 4 KiB store and 1 GiB on a
/// 16 KiB one: a time bound that quietly quadrupled.
///
/// 4 KiB reproducing the original 65,536 is the part that pins the budget
/// was carried over rather than re-guessed.
#[test]
fn the_reclaim_cap_is_the_same_budget_at_every_page_size() {
    assert_eq!(
        Store::incremental_vacuum_max_pages(4096),
        65_536,
        "the original calibration, restated in bytes, must come back unchanged"
    );
    assert_eq!(Store::incremental_vacuum_max_pages(16384), 16_384);
    assert_eq!(Store::incremental_vacuum_max_pages(8192), 32_768);
    for page in [4096, 8192, 16384, 32768, 65536] {
        assert_eq!(
            Store::incremental_vacuum_max_pages(page) * page,
            Store::INCREMENTAL_VACUUM_MAX_BYTES,
            "every page size must reclaim the same number of bytes per pass"
        );
    }
}

/// A new store is created with 16 KiB pages.
///
/// The pragma is silently ignored on a database that already has tables, so
/// "it is in `configure_connection`" is not evidence that it took. This
/// reads the page size back off a store the code actually created.
#[test]
fn a_new_store_is_created_with_the_configured_page_size() {
    let dir = scratch("pagesize");
    let store = Store::open(dir.join("devmap.sqlite")).expect("store");
    let size: i64 = lock_conn(&store.conn)
        .expect("connection")
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .expect("page_size");
    assert_eq!(
        size, 16384,
        "a store created by this code should use the configured page size"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `repair --page-size` converts a store the daemon never will.
///
/// The three things that make it safe are asserted together, because any
/// one of them alone would pass on a broken conversion: the page size
/// actually moved, the database came back to WAL (left in DELETE it still
/// works, but every reader blocks behind every writer — a cliff nobody
/// would attribute to a repair), and the rows survived the rewrite.
///
/// The second call pins idempotence and that "already correct" is reported
/// as `converted: false` rather than inferred from `before == after`.
#[test]
fn converting_an_existing_store_moves_it_to_the_current_page_size() {
    let dir = scratch("pagesize-convert");
    let path = dir.join("devmap.sqlite");
    {
        let conn = Connection::open(&path).expect("seed connection");
        conn.pragma_update(None, "page_size", 4096).expect("4 KiB");
        conn.execute_batch("CREATE TABLE seed (x INTEGER); DROP TABLE seed;")
            .expect("fix the page size into the file header");
    }
    let store = Store::open(&path).expect("store");
    {
        let conn = lock_conn(&store.conn).expect("connection");
        conn.execute("INSERT INTO paths (path) VALUES ('survives.py')", [])
            .expect("a row to carry across the rewrite");
    }

    let outcome = store.convert_page_size().expect("conversion");
    assert_eq!(outcome.before, 4096);
    assert_eq!(outcome.after, Store::PAGE_SIZE);
    assert!(outcome.converted, "a 4 KiB store must report as converted");

    let conn = lock_conn(&store.conn).expect("connection");
    let size: i64 = conn
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .expect("page_size");
    assert_eq!(size, Store::PAGE_SIZE);
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("journal_mode");
    assert_eq!(
        mode.to_lowercase(),
        "wal",
        "the rewrite must leave the database back in WAL"
    );
    let kept: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM paths WHERE path = 'survives.py'",
            [],
            |row| row.get(0),
        )
        .expect("row");
    assert_eq!(kept, 1, "the rewrite must not lose rows");
    drop(conn);

    let again = store.convert_page_size().expect("second conversion");
    assert_eq!(again.after, Store::PAGE_SIZE);
    assert!(
        !again.converted,
        "a store already at the target reports converted=false, not a second rewrite"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A store written with the old 4 KiB pages still opens, and stays 4 KiB.
///
/// Page size is fixed when a database first gets content, so every store
/// already on disk is 4 KiB and cannot be changed by a pragma. Raising the
/// default is only safe if those stores keep working untouched — the same
/// conversion path `auto_vacuum` already depends on. This creates a 4 KiB
/// file, opens it with the current code, and writes through it.
#[test]
fn an_existing_small_page_store_opens_and_reads() {
    let dir = scratch("pagesize-legacy");
    let path = dir.join("devmap.sqlite");
    {
        let conn = Connection::open(&path).expect("seed connection");
        conn.pragma_update(None, "page_size", 4096).expect("4 KiB");
        conn.execute_batch("CREATE TABLE seed (x INTEGER); DROP TABLE seed;")
            .expect("fix the page size into the file header");
    }

    let store = Store::open(&path).expect("an existing 4 KiB store must still open");
    let size: i64 = lock_conn(&store.conn)
        .expect("connection")
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .expect("page_size");
    assert_eq!(
        size, 4096,
        "an existing store keeps its page size; the pragma is accepted and ignored"
    );
    // Not merely openable: usable. A schema that migrated onto the smaller
    // page is what the next build writes into.
    assert!(
        store.latest_generation_id().expect("query").is_none(),
        "a freshly migrated store has no generation yet"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// S-8: the gate must not drift behind the schema it asserts.
///
/// `REQUIRED_SCHEMA` is hand-written and the schema is not, so the only
/// thing keeping them in step is this test. It fails the moment a
/// migration adds a column the gate does not name — which is exactly how
/// `grammar_version`, `analyzer_version`, `classification` and `receiver`
/// came to be missing, leaving a store that opened clean and failed at its
/// first write.
#[test]
fn the_schema_gate_names_every_column_the_current_schema_creates() {
    let store = Store::open_in_memory().expect("store");
    let conn = lock_conn(&store.conn).expect("connection");
    for (table, required) in REQUIRED_SCHEMA {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info(\"{table}\")"))
            .expect("table_info");
        let actual: Vec<String> = stmt
            .query_map([], |row| row.get(1))
            .expect("columns")
            .collect::<Result<_>>()
            .expect("columns");
        assert!(
            !actual.is_empty(),
            "{table} is required by the gate but a freshly created store does not have it"
        );
        for column in actual {
            assert!(
                required.contains(&column.as_str()),
                "{table}.{column} exists in the current schema but the gate does not \
                     require it; a store missing that column would open clean and fail at \
                     the first write instead of at the gate"
            );
        }
    }
}

/// S-8, the other direction: the gate must name every relation, not just
/// every column of the relations it happens to name.
///
/// `the_schema_gate_names_every_column_the_current_schema_creates` iterates
/// `REQUIRED_SCHEMA` and checks that every *actual* column of each listed
/// table is required — so a table missing from the list is invisible to it
/// by construction, and `file_payloads` and `generation_file_rows` were
/// both missing from v17 onward. They passed only transitively, through the
/// `generation_files` view that joins them.
#[test]
fn the_schema_gate_names_every_relation_the_current_schema_creates() {
    let store = Store::open_in_memory().expect("store");
    let conn = lock_conn(&store.conn).expect("connection");
    let required: std::collections::BTreeSet<&str> =
        REQUIRED_SCHEMA.iter().map(|(table, _)| *table).collect();
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master
                  WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%'
                  ORDER BY name",
        )
        .expect("sqlite_master");
    let actual: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .expect("relations")
        .collect::<Result<_>>()
        .expect("relations");
    let missing: Vec<&String> = actual
        .iter()
        // FTS5 owns four shadow tables beneath `nodes_fts`; their existence
        // follows from the virtual table and is not this schema's to declare.
        .filter(|name| !name.starts_with("nodes_fts_") || *name == "nodes_fts_map")
        .filter(|name| !required.contains(name.as_str()))
        // Named, with the reason, beside REQUIRED_SCHEMA — never silently.
        .filter(|name| !crate::db::OPTIONAL_RELATIONS.contains(&name.as_str()))
        .collect();
    for optional in crate::db::OPTIONAL_RELATIONS {
        assert!(
            actual.iter().any(|name| name == optional),
            "{optional} is listed optional but the current schema does not create it; \
             a stale exemption hides nothing until it hides something"
        );
        assert!(
            !required.contains(optional),
            "{optional} cannot be both required and optional"
        );
    }
    assert!(
        missing.is_empty(),
        "{missing:?} exist in the current schema but the gate does not require them; \
             a store missing one would open clean and fail at the first write"
    );
}

/// A scanner that silently found nothing would make
/// `validate_schema`'s index check vacuous — the same "passes for the wrong
/// reason" failure the whole review turns on.
#[test]
fn the_index_gate_reads_every_index_the_schema_creates() {
    let store = Store::open_in_memory().expect("store");
    let conn = lock_conn(&store.conn).expect("connection");
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master
                  WHERE type = 'index' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .expect("indexes");
    let built: std::collections::BTreeSet<String> = stmt
        .query_map([], |row| row.get(0))
        .expect("index names")
        .collect::<Result<_>>()
        .expect("index names");
    let declared: std::collections::BTreeSet<String> = declared_index_names().into_iter().collect();
    assert!(
        !declared.is_empty(),
        "the DDL scan found no indexes at all; the gate would pass vacuously"
    );
    assert_eq!(
        declared, built,
        "the index names parsed out of the schema DDL disagree with the indexes \
             a fresh store actually has"
    );
}

#[test]
fn store_connections_enable_integrity_and_contention_pragmas() {
    let store = Store::open_in_memory().expect("store");
    let conn = lock_conn(&store.conn).expect("connection");
    let foreign_keys: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .expect("foreign_keys pragma");
    let busy_timeout: i64 = conn
        .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
        .expect("busy_timeout pragma");
    assert_eq!(foreign_keys, 1);
    assert!(busy_timeout >= 5_000, "busy timeout was {busy_timeout} ms");
}

/// The write-path pragmas are a contract, not an incidental default.
///
/// Each of these was measured: leaving `synchronous` at `FULL` and
/// `cache_size` at SQLite's 2 MiB default made `save_generation` the single
/// most expensive phase of a build. A later edit that drops one of them
/// would restore that cost silently — nothing fails, builds just get slower
/// — so the settings are asserted rather than trusted.
///
/// `synchronous` is asserted as exactly 1 (NORMAL). Not `<= 1`: 0 is OFF,
/// which trades corruption-on-crash for speed, and this store must never
/// drift into it.
#[test]
fn write_connections_use_the_tuned_durability_and_cache_pragmas() {
    let store = Store::open_in_memory().expect("store");
    let conn = lock_conn(&store.conn).expect("connection");

    let synchronous: i64 = conn
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .expect("synchronous pragma");
    assert_eq!(
        synchronous, 1,
        "expected synchronous=NORMAL (1), found {synchronous} \
             (0=OFF risks corruption, 2=FULL fsyncs every commit)"
    );

    let cache_size: i64 = conn
        .query_row("PRAGMA cache_size", [], |row| row.get(0))
        .expect("cache_size pragma");
    assert_eq!(
        cache_size,
        Store::CACHE_SIZE_KIB as i64,
        "cache_size should be the tuned {} KiB",
        -Store::CACHE_SIZE_KIB
    );

    let temp_store: i64 = conn
        .query_row("PRAGMA temp_store", [], |row| row.get(0))
        .expect("temp_store pragma");
    assert_eq!(temp_store, 2, "expected temp_store=MEMORY (2)");
}

/// K12: every read path fails closed on a poisoned mutex.
///
/// `lock_conn` exists precisely so a poisoned store mutex becomes an error
/// the caller can report, and five readers bypassed it with
/// `.expect("store mutex poisoned")`. Under the former release profile's
/// `panic = "abort"` those are not recoverable panics — they end the
/// process. A daemon serving IPC would vanish mid-request because one
/// earlier query panicked while holding the lock; the CLI would die with no
/// message a caller could act on.
///
/// The lock is poisoned deliberately here rather than by provoking a real
/// panic: what is under test is the failure *mode* of these five readers,
/// not the cause of the poison.
#[test]
fn poisoned_store_mutex_is_an_error_on_every_reader() {
    let store = Store::open_in_memory().expect("store");

    // Poison the mutex: panic while holding it, catching the unwind so the
    // test process survives. Tests build with the default unwind profile.
    let poisoner = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = store.conn.lock().expect("first lock");
        panic!("deliberate poison");
    }));
    assert!(poisoner.is_err(), "the poisoning panic must have unwound");
    assert!(store.conn.is_poisoned(), "the mutex must now be poisoned");

    // Each of these used `.expect("store mutex poisoned")` and therefore
    // aborted rather than returning. Naming them individually so a
    // regression says which reader regressed.
    assert!(
        store.latest_unresolved(10).is_err(),
        "latest_unresolved must fail closed on a poisoned mutex"
    );
    assert!(
        store.count_unresolved_rows().is_err(),
        "count_unresolved_rows must fail closed on a poisoned mutex"
    );
    assert!(
        store.latest_file_hashes().is_err(),
        "latest_file_hashes must fail closed on a poisoned mutex"
    );
    assert!(
        store.latest_symbol_names_by_file().is_err(),
        "latest_symbol_names_by_file must fail closed on a poisoned mutex"
    );
    assert!(
        store.latest_edges_for_test().is_err(),
        "latest_edges_for_test must fail closed on a poisoned mutex"
    );
}

/// The deterministic guard for [`Store::latest_snapshot`].
///
/// The defect it exists for reproduces only probabilistically — a second
/// process has to commit *and* prune inside the microseconds between a
/// reader's generation lookup and its row read, which took 182,781 reads
/// against a continuously rebuilding writer to hit 31 times. A test that
/// fires at that rate is not a guard: a green run from it says nothing.
///
/// So the guard is stated over the mechanism instead, with the schedule
/// forced rather than raced. A second connection — the same thing a second
/// process is, as far as SQLite's snapshots are concerned — deletes the
/// generation's rows strictly between the pin and the read. Both readers
/// are run over that schedule, and they must disagree:
///
/// * without a snapshot the pinned generation reads back **empty**, which
///   is the defect, verbatim;
/// * with one it reads back its rows.
///
/// Asserting both directions is what keeps this honest. A test that only
/// checked the snapshot would still pass if the delete silently stopped
/// landing, and would then be proving nothing at all.
#[test]
fn a_pinned_generation_keeps_its_rows_when_another_connection_prunes_it() {
    let dir = std::env::temp_dir().join(format!(
        "devmap-snapshot-guard-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let db_path = dir.join("index.sqlite");
    let store = Store::open(&db_path).expect("store");
    {
        let conn = lock_conn(&store.conn).expect("connection");
        conn.execute(
            "INSERT INTO generations (id, created_at, head_sha, analysis_json)
                 VALUES (7, 1.0, 'seed', '{}')",
            [],
        )
        .expect("generation");
        conn.execute("INSERT INTO paths (id, path) VALUES (1, 'src/a.py')", [])
            .expect("path");
        conn.execute(
            "INSERT INTO generation_nodes
                 (generation_id, ordinal, file_id, name, qualified_name, kind,
                  span_start, span_end, is_exported)
                 VALUES (7, 0, 1, 'alpha', 'src/a.py::alpha', 'Function', 0, 1, 0)",
            [],
        )
        .expect("node");
    }

    // What the pruning writer does, from a connection of its own.
    let prune = || {
        let other = Connection::open(&db_path).expect("second connection");
        other
            .busy_timeout(std::time::Duration::from_secs(5))
            .expect("busy timeout");
        other
            .execute("DELETE FROM generation_nodes WHERE generation_id = 7", [])
            .expect("prune");
        other
            .execute("DELETE FROM generations WHERE id = 7", [])
            .expect("prune");
    };
    let count_rows = |conn: &Connection, generation: u32| -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM generation_nodes WHERE generation_id = ?1",
            params![generation],
            |row| row.get(0),
        )
        .expect("count")
    };

    // Reader A: the pre-fix shape — pin, then read, with no snapshot
    // between them. This is the control, and it must observe the deletion.
    let unpinned = {
        let conn = lock_conn(&store.conn).expect("connection");
        let generation = Store::latest_generation_id_locked(&conn)
            .expect("pin")
            .expect("a generation");
        assert_eq!(generation, 7);
        prune();
        count_rows(&conn, generation)
    };
    assert_eq!(
        unpinned, 0,
        "the control did not actually race: without a snapshot the pinned \
             generation must read back empty, or this test proves nothing"
    );

    // Put the generation back and run the same schedule through the
    // snapshot the fix installs.
    {
        let conn = lock_conn(&store.conn).expect("connection");
        conn.execute(
            "INSERT INTO generations (id, created_at, head_sha, analysis_json)
                 VALUES (7, 1.0, 'seed', '{}')",
            [],
        )
        .expect("generation");
        conn.execute(
            "INSERT INTO generation_nodes
                 (generation_id, ordinal, file_id, name, qualified_name, kind,
                  span_start, span_end, is_exported)
                 VALUES (7, 0, 1, 'alpha', 'src/a.py::alpha', 'Function', 0, 1, 0)",
            [],
        )
        .expect("node");
    }
    let pinned = {
        let conn = lock_conn(&store.conn).expect("connection");
        let (snapshot, generation) = Store::latest_snapshot(&conn)
            .expect("pin")
            .expect("a generation");
        assert_eq!(generation, 7);
        prune();
        count_rows(&snapshot, generation)
    };
    assert_eq!(
        pinned, 1,
        "a generation pinned inside a read snapshot lost its rows to a \
             concurrent prune; the reader would answer `[]` for a store that \
             holds data, which is the failure `latest_snapshot` exists to stop"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
