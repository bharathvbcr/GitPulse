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

const PERSISTENCE_TRIGGER: &str = "CREATE TRIGGER inject AFTER INSERT ON payload_literals
     BEGIN UPDATE payload_literals SET value = 'INJECTED-BY-SHIPPED-STORE'
      WHERE payload_id = NEW.payload_id AND span_start = NEW.span_start; END;";

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

/// Every refusal that tells the operator to remove a store names a command that
/// exists. `dev map doctor --fix` was named here and does not exist
/// (`docs/cli-reference.md` lists it under "Unknown"), and the auto-quarantine
/// it promised is parked (`docs/devmap/design/auto-quarantine-parked.md`): the
/// remedy is to move or delete the store and run `devmap build`.
fn assert_names_the_remedy_that_exists(error: &rusqlite::Error, what: &str) {
    let text = error.to_string();
    assert!(
        text.contains("move or delete the store and run `devmap build`"),
        "{what}: the refusal does not say what to do: {text}"
    );
    assert!(
        !text.contains("doctor") && !text.contains("quarantines"),
        "{what}: the refusal names a command that does not exist: {text}"
    );
}

#[test]
fn the_trigger_refusal_names_a_remedy_that_exists() {
    let path = tampered_store("remedy-trigger", PERSISTENCE_TRIGGER);
    let error = Store::open(&path).err().expect("a trigger is refused");
    assert_names_the_remedy_that_exists(&error, "trigger");
}

#[test]
fn the_redefined_view_refusal_names_a_remedy_that_exists() {
    let (name, sql) = core_views().into_iter().next().expect("a core view");
    let path = store_with_view("remedy-view", &name, &format!("{sql} LIMIT 0"));
    let error = Store::open(&path)
        .err()
        .expect("a redefined view is refused");
    assert_names_the_remedy_that_exists(&error, &name);
}

#[test]
fn the_missing_index_refusal_names_a_remedy_that_exists() {
    // A writer open recreates a missing index, so the refusal is the read-only
    // open's: it validates the schema and cannot repair it. Every declared index
    // is dropped in turn and each refusal is read.
    let mut refused = 0;
    for index in crate::schema::declared_index_names() {
        let path = tampered_store(
            &format!("remedy-index-{index}"),
            &format!("DROP INDEX \"{index}\";"),
        );
        if let Err(error) = Store::open_read_only(&path) {
            assert!(
                error.to_string().contains("was not recreated"),
                "{index}: refused, but not for the index: {error}"
            );
            assert_names_the_remedy_that_exists(&error, &index);
            refused += 1;
        }
    }
    assert!(
        refused > 0,
        "no dropped index was refused: the test proves nothing"
    );
}

/// The ownership read `devmap build` makes before it opens a store reads SQL that
/// belongs to the file. A view standing where `pending_state` should be, and
/// spinning, must not be run by it.
#[test]
fn the_ownership_read_before_open_does_not_run_a_view_standing_for_the_owner_table() {
    let path = tampered_store(
        "owner-view",
        "DROP TABLE pending_state;
         CREATE VIEW pending_state AS
         WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 400000000)
         SELECT 1 AS singleton, max(n) AS repo_root FROM r;",
    );
    let root = scratch("owner-view-root");
    let started = std::time::Instant::now();
    // At the current schema an unreadable owner passes (the open reports the
    // damage itself); what is pinned is that nothing of the file's was run.
    let _ = Store::refuse_a_live_foreign_owner_before_open(&path, &root);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "the ownership read ran a view for {:?}",
        started.elapsed()
    );
}

/// A store owned by a live worktree, at `path`: built, then bound to `owner`.
fn store_owned_by(label: &str, owner: &std::path::Path) -> std::path::PathBuf {
    let dir = scratch(label);
    let path = dir.join("devmap.sqlite");
    let store = Store::open(&path).expect("fresh store");
    store.bind_repo_root(owner).expect("bind owner");
    drop(store);
    path
}

fn set_owner_row(path: &std::path::Path, owner: &str) {
    let conn = rusqlite::Connection::open(path).expect("plain connection");
    conn.execute(
        "UPDATE pending_state SET repo_root = ?1 WHERE singleton = 1",
        [owner],
    )
    .expect("set owner");
}

/// A relative owner is read against the working directory of whoever asks, so
/// it named nobody (a vanished directory the store then took over) or the
/// requester itself. It is refused by the read made before the open, which is
/// the one `serve`, `build` and `repair` make, and by the opened store.
#[test]
fn a_store_whose_owner_row_is_a_relative_path_is_refused() {
    let owner = scratch("relative-owner-a");
    let root = scratch("relative-owner-b");
    for relative in ["somewhere/else", ".", "../x"] {
        let path = store_owned_by("relative-owner", &owner);
        set_owner_row(&path, relative);
        let error = Store::refuse_a_live_foreign_owner_before_open(&path, &root)
            .expect_err(&format!("relative owner {relative:?} was accepted"));
        assert!(
            error.to_string().contains("relative path"),
            "{relative}: {error}"
        );
        let store = Store::open(&path).expect("the open does not judge ownership");
        let error = store
            .validate_repo_root(&root)
            .expect_err("opened store accepted it");
        assert!(
            error.to_string().contains("relative path"),
            "{relative}: {error}"
        );
        assert!(
            store.bind_repo_root(&root).is_err(),
            "{relative}: bound over it"
        );
    }
}

/// Stamp 0 reads as "no schema yet", but the open heals a stamp-0 file in
/// place, so one that already records an owner is not provably fresh.
#[test]
fn a_stamp_zero_store_does_not_skip_the_ownership_check() {
    let owner = scratch("stamp-zero-owner");
    let root = scratch("stamp-zero-root");
    let path = store_owned_by("stamp-zero", &owner);
    {
        let conn = rusqlite::Connection::open(&path).expect("plain connection");
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .expect("checkpoint");
        conn.execute_batch("PRAGMA user_version = 0;")
            .expect("reset stamp");
    }
    let error = Store::refuse_a_live_foreign_owner_before_open(&path, &root)
        .expect_err("a stamp-0 store owned by a live worktree was passed");
    assert!(
        error
            .to_string()
            .contains("one store cannot serve two live worktrees"),
        "{error}"
    );
    // Positive controls: the owner's own request, and a file with no schema at
    // all, are not refused.
    Store::refuse_a_live_foreign_owner_before_open(&path, &owner).expect("owner's own");
    let empty = scratch("stamp-zero-empty").join("devmap.sqlite");
    drop(rusqlite::Connection::open(&empty).expect("empty file"));
    Store::refuse_a_live_foreign_owner_before_open(&empty, &root).expect("no schema, no owner");
}

/// The ownership read and the open are separate connections with no lock
/// between them. This pins the failure that allows, so the documented window
/// cannot widen unnoticed: the peek passes an unbound store, another worktree
/// binds it, and the open still succeeds; only the bind refuses.
#[test]
fn the_ownership_peek_is_not_atomic_with_the_open() {
    let a = scratch("window-a");
    let b = scratch("window-b");
    let dir = scratch("window-store");
    let path = dir.join("devmap.sqlite");
    drop(Store::open(&path).expect("fresh store"));
    Store::refuse_a_live_foreign_owner_before_open(&path, &b).expect("unbound at the peek");
    Store::open(&path)
        .expect("a")
        .bind_repo_root(&a)
        .expect("a binds between B's peek and B's open");
    let opened = Store::open(&path).expect("B's open does not re-check ownership: the window");
    assert!(
        opened.bind_repo_root(&b).is_err(),
        "the bind is the backstop: B still cannot take A's rows"
    );
}

/// Adversarial owner rows: an empty string, `./`, a home-relative spelling and a
/// non-ASCII relative name are all relative, and none may read as a vanished
/// owner the store is then taken over from.
#[test]
fn every_spelling_of_a_relative_owner_is_refused_and_none_is_taken_over() {
    let owner = scratch("relative-spellings-a");
    let root = scratch("relative-spellings-b");
    for relative in [
        "",
        "./",
        "./x",
        "../",
        "~",
        "~/x",
        "名前/ディレクトリ",
        " ",
        "x\u{0}y",
    ] {
        let path = store_owned_by("relative-spellings", &owner);
        set_owner_row(&path, relative);
        let error = Store::refuse_a_live_foreign_owner_before_open(&path, &root).expect_err(
            &format!("relative owner {relative:?} was accepted by the peek"),
        );
        assert!(
            error.to_string().contains("relative path"),
            "{relative:?}: {error}"
        );
        let store = Store::open(&path).expect("the open does not judge ownership");
        assert!(
            store.bind_repo_root(&root).is_err(),
            "{relative:?}: bound over it"
        );
        assert_eq!(
            rusqlite::Connection::open(&path)
                .unwrap()
                .query_row(
                    "SELECT repo_root FROM pending_state WHERE singleton = 1",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            relative,
            "{relative:?}: the owner row was rewritten by a refused bind"
        );
    }
}

/// An owner row that is not text at all (a BLOB, an integer) is not a path.
#[test]
fn a_non_text_owner_row_is_not_read_as_an_owner_and_is_not_taken_over() {
    let owner = scratch("non-text-owner-a");
    let root = scratch("non-text-owner-b");
    for literal in ["x'2f746d70'", "42", "1.5"] {
        let path = store_owned_by("non-text-owner", &owner);
        let conn = rusqlite::Connection::open(&path).expect("plain connection");
        conn.execute_batch(&format!(
            "UPDATE pending_state SET repo_root = {literal} WHERE singleton = 1;"
        ))
        .expect("set owner");
        drop(conn);
        let peek = Store::refuse_a_live_foreign_owner_before_open(&path, &root);
        // The open itself refusing the row is a refusal too.
        let Ok(store) = Store::open(&path) else {
            continue;
        };
        let bind = store.bind_repo_root(&root);
        assert!(
            peek.is_err() || bind.is_err(),
            "{literal}: neither the peek nor the bind refused a non-text owner"
        );
    }
}

/// A non-ASCII live owner is refused like any other.
#[test]
fn a_live_owner_with_a_non_ascii_path_is_refused() {
    let owner = scratch("名前-owner");
    let root = scratch("名前-requester");
    let path = store_owned_by("unicode-owner", &owner);
    let error = Store::refuse_a_live_foreign_owner_before_open(&path, &root)
        .expect_err("a live unicode owner was passed");
    assert!(
        error
            .to_string()
            .contains("one store cannot serve two live worktrees"),
        "{error}"
    );
    Store::refuse_a_live_foreign_owner_before_open(&path, &owner).expect("the owner itself");
}

/// A stamp-0 file that records its owner only on the pre-20 `generations`
/// column is still asked.
#[test]
fn a_stamp_zero_store_with_only_a_generations_owner_column_is_still_checked() {
    let owner = scratch("stamp-zero-gen-owner");
    let root = scratch("stamp-zero-gen-root");
    let dir = scratch("stamp-zero-gen");
    let path = dir.join("devmap.sqlite");
    {
        let conn = rusqlite::Connection::open(&path).expect("plain connection");
        conn.execute_batch(&format!(
            "CREATE TABLE generations (id INTEGER PRIMARY KEY, created_at REAL, head_sha TEXT, analysis_json TEXT, repo_root TEXT);
             INSERT INTO generations (id, created_at, head_sha, analysis_json, repo_root) VALUES (1, 1.0, 'x', '{{}}', '{}');
             PRAGMA user_version = 0;",
            owner.to_str().unwrap()
        ))
        .expect("old-shape file");
    }
    let error = Store::refuse_a_live_foreign_owner_before_open(&path, &root)
        .expect_err("a stamp-0 file owned through generations.repo_root was passed");
    assert!(
        error
            .to_string()
            .contains("one store cannot serve two live worktrees"),
        "{error}"
    );
}

/// APFS is case-insensitive: the owner's own root spelled in another case is
/// the same worktree, not a live foreign one. Skipped where the filesystem
/// distinguishes case.
#[test]
fn the_owners_own_root_in_another_case_is_not_a_foreign_owner() {
    let owner = scratch("CaseOwnerRoot");
    let swapped = owner.with_file_name(owner.file_name().unwrap().to_string_lossy().to_uppercase());
    if !swapped.exists() {
        return;
    }
    let path = store_owned_by("case-variant", &owner);
    Store::refuse_a_live_foreign_owner_before_open(&path, &swapped)
        .expect("the same directory in another case was refused as a foreign live owner");
    let store = Store::open(&path).expect("open");
    store
        .bind_repo_root(&swapped)
        .expect("the same directory in another case could not bind");
}
