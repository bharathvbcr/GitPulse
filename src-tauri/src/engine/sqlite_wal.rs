//! The one place a GitPulse-owned SQLite file is switched to WAL.
//!
//! `PRAGMA journal_mode=WAL` can answer `SQLITE_BUSY` immediately, without
//! consulting the connection's busy handler, while another connection is
//! switching the same file. That is contention that clears in milliseconds,
//! so every store goes through [`enter_wal`], which retries it for a bound its
//! caller chooses and returns contention that outlasts the bound as the
//! original `rusqlite::Error`, for the caller to type as it sees fit.
//!
//! Who races depends on the store. The ledger is opened by many processes (the
//! app, gitpulse-mcp, hooks), so two of them can create it at once: that lost a
//! writer 2 in 60 under load. The churn cache is written by the app alone, so
//! the race there is two app processes on one cache directory (a relaunch while
//! the previous process is still exiting) or two threads of one process opening
//! it; the churn tests race threads, the only form one test process can make.

use rusqlite::{Connection, ErrorCode};
use std::time::{Duration, Instant};

/// How long the ledger keeps trying while another connection holds its file.
pub(crate) const LEDGER_WAL_PATIENCE: Duration = Duration::from_secs(10);

/// Pause between attempts. Short: the contention being waited out is another
/// connection's one-statement switch.
const RETRY_PAUSE: Duration = Duration::from_millis(5);

/// Puts the database behind `conn` in WAL mode, retrying `SQLITE_BUSY` for at
/// most `patience` in total.
///
/// The connection's own busy handler sleeps inside every attempt, so it would
/// stack on top of `patience`: it is set to zero for the switch and put back,
/// whatever the outcome. An already-WAL file needs no switch, so it is only
/// read, never written. Any error other than `SQLITE_BUSY` is returned at once;
/// so is `SQLITE_BUSY` once `patience` has elapsed (one attempt is always made).
pub(crate) fn enter_wal(conn: &Connection, patience: Duration) -> rusqlite::Result<()> {
    let handler_ms: i64 = conn.pragma_query_value(None, "busy_timeout", |row| row.get(0))?;
    conn.busy_timeout(Duration::ZERO)?;
    let switched = switch_within(conn, patience);
    let restored = conn.busy_timeout(Duration::from_millis(
        u64::try_from(handler_ms).unwrap_or(0),
    ));
    switched.and(restored)
}

fn switch_within(conn: &Connection, patience: Duration) -> rusqlite::Result<()> {
    let deadline = Instant::now() + patience;
    loop {
        let switched = conn
            .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
            .and_then(|mode| {
                if mode.eq_ignore_ascii_case("wal") {
                    Ok(())
                } else {
                    conn.pragma_update(None, "journal_mode", "WAL")
                }
            });
        match switched {
            Err(error)
                if error.sqlite_error_code() == Some(ErrorCode::DatabaseBusy)
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(RETRY_PAUSE);
            }
            other => return other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rollback_file() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE t(v INTEGER);").unwrap();
        (dir, path)
    }

    fn impatient(path: &std::path::Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.busy_timeout(Duration::ZERO).unwrap();
        conn
    }

    fn mode(conn: &Connection) -> String {
        conn.pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn a_rollback_file_is_switched_and_a_wal_file_is_left_alone() {
        let (_dir, path) = rollback_file();
        let conn = Connection::open(&path).unwrap();
        enter_wal(&conn, Duration::ZERO).unwrap();
        assert_eq!(mode(&conn), "wal");
        // Already WAL: read-only, so a holder of the write lock cannot block it.
        let holder = Connection::open(&path).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE;").unwrap();
        enter_wal(&impatient(&path), Duration::ZERO).unwrap();
    }

    #[test]
    fn contention_that_clears_within_the_bound_is_waited_out() {
        let (_dir, path) = rollback_file();
        let holder = Connection::open(&path).unwrap();
        holder.execute_batch("BEGIN EXCLUSIVE;").unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            drop(holder);
        });
        let conn = impatient(&path);
        enter_wal(&conn, Duration::from_secs(30)).expect("the switch waits for the holder");
        release.join().unwrap();
        assert_eq!(mode(&conn), "wal");
    }

    #[test]
    fn contention_that_outlasts_the_bound_is_the_busy_error() {
        let (_dir, path) = rollback_file();
        let holder = Connection::open(&path).unwrap();
        holder.execute_batch("BEGIN EXCLUSIVE;").unwrap();
        let started = Instant::now();
        let error = enter_wal(&impatient(&path), Duration::from_millis(100))
            .expect_err("the holder never lets go");
        assert_eq!(error.sqlite_error_code(), Some(ErrorCode::DatabaseBusy));
        assert!(started.elapsed() >= Duration::from_millis(100));
    }

    /// ADVERSARIAL W6-L3, FAILS BY DESIGN: a connection with the ordinary
    /// busy handler (rusqlite's default is 5 s; the ledger never lowers it)
    /// sleeps inside every attempt, so `patience` is not the bound: the
    /// busy timeout stacks on top of it.
    #[test]
    fn a_sleeping_busy_handler_does_not_stretch_the_patience_bound() {
        let (_dir, path) = rollback_file();
        let holder = Connection::open(&path).unwrap();
        holder.execute_batch("BEGIN EXCLUSIVE;").unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.busy_timeout(Duration::from_millis(1500)).unwrap();
        let started = Instant::now();
        let error = enter_wal(&conn, Duration::from_millis(100)).expect_err("never released");
        assert_eq!(error.sqlite_error_code(), Some(ErrorCode::DatabaseBusy));
        let took = started.elapsed();
        assert!(
            took < Duration::from_millis(100) + Duration::from_millis(500),
            "patience 100 ms, busy_timeout 1.5 s: took {took:?}"
        );
    }

    /// The connection's busy handler is only suspended for the switch: it is
    /// back as it was after a switch, and after a refusal.
    #[test]
    fn the_connections_busy_handler_is_restored_on_every_path() {
        let busy_ms = |conn: &Connection| -> i64 {
            conn.pragma_query_value(None, "busy_timeout", |row| row.get(0))
                .unwrap()
        };
        let (_dir, path) = rollback_file();
        let conn = Connection::open(&path).unwrap();
        conn.busy_timeout(Duration::from_millis(1234)).unwrap();
        enter_wal(&conn, Duration::from_secs(5)).unwrap();
        assert_eq!(busy_ms(&conn), 1234, "restored after a switch");

        let (_dir2, path2) = rollback_file();
        let holder = Connection::open(&path2).unwrap();
        holder.execute_batch("BEGIN EXCLUSIVE;").unwrap();
        let refused = Connection::open(&path2).unwrap();
        refused.busy_timeout(Duration::from_millis(777)).unwrap();
        enter_wal(&refused, Duration::from_millis(50)).expect_err("held");
        assert_eq!(busy_ms(&refused), 777, "restored after a refusal");
    }

    /// ADVERSARIAL W6-L3 (held): every extended BUSY code maps to the primary
    /// `DatabaseBusy` that `enter_wal` retries; LOCKED is a different primary
    /// code and is returned at once.
    #[test]
    fn extended_busy_codes_are_retried_and_locked_is_not() {
        for extended in [
            rusqlite::ffi::SQLITE_BUSY,
            rusqlite::ffi::SQLITE_BUSY_RECOVERY,
            rusqlite::ffi::SQLITE_BUSY_SNAPSHOT,
            rusqlite::ffi::SQLITE_BUSY_TIMEOUT,
        ] {
            let error = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(extended), None);
            assert_eq!(error.sqlite_error_code(), Some(ErrorCode::DatabaseBusy));
        }
        let locked = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_LOCKED_SHAREDCACHE),
            None,
        );
        assert_eq!(locked.sqlite_error_code(), Some(ErrorCode::DatabaseLocked));
    }

    /// ADVERSARIAL W6-L3 (held): a reader holding a SHARED lock on a
    /// rollback-journal file blocks the switch; it is the busy error after the
    /// bound, never a hang or a deadlock.
    #[test]
    fn a_rollback_mode_reader_is_waited_on_and_then_reported_busy() {
        let (_dir, path) = rollback_file();
        let reader = Connection::open(&path).unwrap();
        reader
            .execute_batch("BEGIN; SELECT count(*) FROM t;")
            .unwrap();
        let error = enter_wal(&impatient(&path), Duration::from_millis(150))
            .expect_err("a live reader prevents the switch");
        assert_eq!(error.sqlite_error_code(), Some(ErrorCode::DatabaseBusy));
        assert_eq!(mode(&reader), "delete");
    }
}
