//! The backup's exists-check shares the write lock with the copy.
//!
//! A host that waited for the lock must look for the backup *after* getting it:
//! the host that held the lock may have written the backup and changed the
//! profile since. These tests hold the lock on one connection, let a second
//! host block on it (its busy handler signals), and only then play the first
//! host: write the backup, change the profile, commit. No sleeps.

use super::{Error, backup_profile};
use rusqlite::Connection;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

thread_local! {
    static BLOCKED: RefCell<Option<mpsc::Sender<()>>> = const { RefCell::new(None) };
}

/// Tells the test, once, that this connection is waiting for a lock.
fn on_busy(_attempt: i32) -> bool {
    BLOCKED.with(|slot| {
        if let Some(tx) = slot.borrow_mut().take() {
            let _ = tx.send(());
        }
    });
    std::thread::yield_now();
    true
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "dc-store-backup-lock-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Outcome {
    blocked: bool,
    created: Option<bool>,
    error: Option<Error>,
    before: Vec<u8>,
    after: Vec<u8>,
}

/// Host B calls `backup_profile` while `holder` owns the write lock. When B is
/// seen waiting, the holder writes `<profile>.<stamp>.bak` (a copy of the
/// profile as it is), runs `change`, and commits.
fn race(
    profile: &Path,
    stamp: &str,
    expected_version: Option<i64>,
    must_hold_table: Option<&'static str>,
    change: &str,
) -> Outcome {
    let holder = Connection::open(profile).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let path = profile.to_str().unwrap().to_owned();
    let stamp_b = stamp.to_owned();
    let (tx, rx) = mpsc::channel();
    let host_b = std::thread::spawn(move || {
        let mut conn = Connection::open(&path).unwrap();
        conn.busy_handler(Some(on_busy)).unwrap();
        BLOCKED.with(|slot| *slot.borrow_mut() = Some(tx));
        let result = backup_profile(
            &conn,
            &stamp_b,
            expected_version,
            must_hold_table,
            |_, detail| detail,
        );
        // Drops the sender when the handler never ran.
        BLOCKED.with(|slot| slot.borrow_mut().take());
        result
    });
    let blocked = rx.recv().is_ok();
    let backup = PathBuf::from(format!("{}.{stamp}.bak", profile.display()));
    let before = if blocked {
        // The first host: its copy exists before it changes the profile.
        Connection::open(profile)
            .unwrap()
            .execute("VACUUM INTO ?1", [backup.to_str().unwrap()])
            .unwrap();
        std::fs::read(&backup).unwrap()
    } else {
        Vec::new()
    };
    holder.execute_batch(change).unwrap();
    holder.execute_batch("COMMIT").unwrap();
    let result = host_b.join().unwrap();
    let after = std::fs::read(&backup).unwrap_or_default();
    let (created, error) = match result {
        Ok(Some(made)) => (Some(made.created), None),
        Ok(None) => (None, None),
        Err(error) => (None, Some(error)),
    };
    Outcome {
        blocked,
        created,
        error,
        before,
        after,
    }
}

fn assert_waiter_kept_the_copy(outcome: &Outcome) {
    assert!(outcome.blocked, "host B never waited for the lock");
    assert!(
        outcome.error.is_none(),
        "host B failed: {:?}",
        outcome.error.as_ref().map(|e| e.message.clone())
    );
    assert_eq!(
        outcome.created,
        Some(false),
        "host B wrote a copy instead of finding the first host's"
    );
    assert_eq!(
        outcome.before, outcome.after,
        "host B replaced the first host's backup"
    );
}

/// Once `tasks` is gone the retire path no longer requires the table of a copy,
/// so a host that checked before waiting would rename a copy of the retired
/// store over the only copy that holds the legacy rows.
#[test]
fn a_host_that_waited_keeps_the_retire_backup_the_first_host_made() {
    let dir = Scratch::new("retire");
    let profile = dir.0.join("profile.sqlite");
    let conn = Connection::open(&profile).unwrap();
    conn.execute_batch("CREATE TABLE tasks(id TEXT); INSERT INTO tasks VALUES ('TASK-1');")
        .unwrap();
    drop(conn);
    let outcome = race(
        &profile,
        "pre-tasks-retire",
        None,
        Some("tasks"),
        "DROP TABLE tasks",
    );
    assert_waiter_kept_the_copy(&outcome);
    let backup = dir.0.join("profile.sqlite.pre-tasks-retire.bak");
    let rows: i64 = Connection::open(&backup)
        .unwrap()
        .query_row("SELECT count(*) FROM tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 1, "the copy no longer holds the legacy row");
}

/// The upgrade path: the first host migrated while B waited, so a copy B made
/// after the wait would hold the new schema.
#[test]
fn a_host_that_waited_keeps_the_pre_schema_backup_the_first_host_made() {
    let dir = Scratch::new("schema");
    let profile = dir.0.join("profile.sqlite");
    let conn = Connection::open(&profile).unwrap();
    conn.execute_batch(
        "CREATE TABLE work_meta(id INTEGER PRIMARY KEY, version INTEGER);
         INSERT INTO work_meta VALUES (1, 11);",
    )
    .unwrap();
    drop(conn);
    let outcome = race(
        &profile,
        "pre-schema-11",
        Some(11),
        None,
        "UPDATE work_meta SET version=17 WHERE id=1",
    );
    assert_waiter_kept_the_copy(&outcome);
    let backup = dir.0.join("profile.sqlite.pre-schema-11.bak");
    let held: i64 = Connection::open(&backup)
        .unwrap()
        .query_row("SELECT version FROM work_meta WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(held, 11);
}
