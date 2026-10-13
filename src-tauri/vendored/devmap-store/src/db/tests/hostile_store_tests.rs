//! A store that arrives with the repository is not this process's own.
//!
//! `.devcouncil/codeintel/devmap.sqlite` can be committed or copied between
//! worktrees. SQLite lets a database carry triggers and views whose SQL runs
//! with this process's authority on the next write or read: a persistence
//! trigger survives every `devmap build` and rewrites what it stores, and a
//! recursive one makes the build spin. The open must refuse such a store before
//! it reads or migrates it, and must not let schema SQL call functions that
//! have side effects.
use super::*;

fn scratch(label: &str) -> std::path::PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("devmap-{label}-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A valid current-schema store with `sql` executed against it by a plain
/// SQLite connection, the way whoever shipped the file could have.
fn tampered_store(label: &str, sql: &str) -> std::path::PathBuf {
    let dir = scratch(label);
    let path = dir.join("devmap.sqlite");
    drop(Store::open(&path).expect("fresh store"));
    let conn = rusqlite::Connection::open(&path).expect("plain connection");
    conn.execute_batch(sql).expect("tamper");
    drop(conn);
    path
}

const PERSISTENCE_TRIGGER: &str = "CREATE TRIGGER inject AFTER INSERT ON generation_literals
     BEGIN UPDATE generation_literals SET value = 'INJECTED-BY-SHIPPED-STORE' WHERE rowid = NEW.rowid; END;";

const SPINNING_TRIGGER: &str = "CREATE TRIGGER spin AFTER INSERT ON generations BEGIN
     WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r) SELECT count(*) FROM r; END;";

fn assert_refused(result: Result<Store>, what: &str) {
    match result {
        Ok(_) => panic!("{what}: a store carrying a trigger was opened"),
        Err(error) => assert!(
            error.to_string().contains("trigger"),
            "{what}: refused, but not for the trigger: {error}"
        ),
    }
}

#[test]
fn a_store_carrying_a_persistence_trigger_is_refused_by_every_open() {
    let path = tampered_store("trigger-persist", PERSISTENCE_TRIGGER);
    assert_refused(Store::open(&path), "open");
    assert_refused(Store::open_read_only(&path), "open_read_only");
    assert!(Store::open_existing(&path).is_err(), "open_existing");
}

#[test]
fn a_store_carrying_a_recursive_trigger_is_refused_before_any_write() {
    let path = tampered_store("trigger-spin", SPINNING_TRIGGER);
    let started = std::time::Instant::now();
    assert_refused(Store::open(&path), "open");
    assert_refused(Store::open_read_only(&path), "open_read_only");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

#[test]
fn a_trigger_on_a_temp_or_unrelated_table_is_refused_too() {
    let path = tampered_store(
        "trigger-other",
        "CREATE TABLE unrelated(x); CREATE TRIGGER t AFTER INSERT ON unrelated BEGIN SELECT 1; END;",
    );
    assert_refused(Store::open(&path), "open");
}

#[test]
fn a_clean_store_still_opens_and_schema_sql_is_untrusted() {
    let dir = scratch("clean");
    let path = dir.join("devmap.sqlite");
    for store in [
        Store::open(&path).expect("writable"),
        Store::open_read_only(&path).expect("read-only"),
        Store::open_in_memory().expect("in memory"),
    ] {
        let conn = store.conn.lock().unwrap();
        let trusted: i64 = conn
            .query_row("PRAGMA trusted_schema", [], |row| row.get(0))
            .unwrap();
        assert_eq!(trusted, 0, "schema SQL may call side-effecting functions");
    }
}

/// The core views, as the store a fresh open builds defines them.
fn core_views() -> Vec<(String, String)> {
    let dir = scratch("views-source");
    let path = dir.join("devmap.sqlite");
    drop(Store::open(&path).expect("fresh store"));
    let conn = rusqlite::Connection::open(&path).expect("plain connection");
    let mut stmt = conn
        .prepare("SELECT name, sql FROM sqlite_schema WHERE type = 'view' ORDER BY name")
        .unwrap();
    let views = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert!(!views.is_empty(), "the schema declares no views to guard");
    views
}

/// Replaces a view's SQL in a store that otherwise opens: same name, same
/// columns, and so the column gate reads it as the view it expects.
fn store_with_view(label: &str, name: &str, sql: &str) -> std::path::PathBuf {
    tampered_store(label, &format!("DROP VIEW \"{name}\"; {sql}"))
}

fn assert_view_refused(result: Result<Store>, what: &str) {
    match result {
        Ok(_) => panic!("{what}: a store that redefines a core view was opened"),
        Err(error) => assert!(
            error.to_string().contains("view"),
            "{what}: refused, but not for the view: {error}"
        ),
    }
}

#[test]
fn a_store_that_redefines_a_core_view_is_refused_by_every_open() {
    for (name, sql) in core_views() {
        // Same columns, no rows: the answer a reader gets is silently empty.
        let path = store_with_view(&format!("view-{name}"), &name, &format!("{sql} LIMIT 0"));
        assert_view_refused(Store::open(&path), &format!("{name}: open"));
        assert_view_refused(
            Store::open_read_only(&path),
            &format!("{name}: open_read_only"),
        );
        assert!(
            Store::open_existing(&path).is_err(),
            "{name}: open_existing"
        );
    }
}

#[test]
fn only_whitespace_differences_in_a_view_are_tolerated() {
    for (name, sql) in core_views() {
        let reflowed = sql.split_whitespace().collect::<Vec<_>>().join("\n  ");
        let path = store_with_view(&format!("ws-{name}"), &name, &reflowed);
        Store::open(&path)
            .unwrap_or_else(|e| panic!("{name}: whitespace-only change refused: {e}"));
    }
}
