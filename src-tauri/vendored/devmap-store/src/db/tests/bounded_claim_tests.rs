use super::*;
use rusqlite::params;

/// S-6: a lock check that could not run is not another writer.
///
/// `lock_writer_at` matched `Err(_)` from `File::try_lock`, which collapses
/// `TryLockError::WouldBlock` (contention — wait and retry) with
/// `TryLockError::Error` (the check itself failed). On a filesystem that
/// does not implement `flock`, the second is what *every* attempt returns:
/// each build polled the full 60 s and then failed with "another devmap
/// writer holds … (pid unknown)" — a definite claim about a process that
/// does not exist, made by a check that never completed, after a minute
/// spent waiting for it.
#[test]
fn s6_a_writer_lock_check_that_failed_is_not_reported_as_another_writer() {
    let lock_path = std::path::Path::new("/nonexistent/devmap.sqlite.writer.lock");
    let mut attempts = 0usize;
    let started = std::time::Instant::now();
    let error = Store::poll_writer_lock(
        || {
            attempts += 1;
            Err(std::fs::TryLockError::Error(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied,
            )))
        },
        std::time::Duration::from_secs(60),
        std::time::Duration::from_millis(10),
        lock_path,
    )
    .expect_err("a failed lock check must not be reported as a taken lock");

    assert_eq!(
        attempts, 1,
        "a check that cannot run must not be retried until the deadline"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "must fail immediately, not after the full wait: {:?}",
        started.elapsed()
    );
    let text = error.to_string();
    assert!(
        !text.contains("another devmap writer holds"),
        "must not claim another writer exists: {text}"
    );
    assert!(
        text.contains("could not be taken") && text.contains("permission denied"),
        "must name the failure that actually happened: {text}"
    );
}

/// S-6 control: real contention must still wait and still name the holder.
///
/// A guard that propagated every error would trade the false claim for a
/// build that refuses to wait out a peer, which is the failure the bounded
/// poll exists to prevent.
#[test]
fn s6_writer_lock_contention_still_polls_to_the_deadline_and_names_the_holder() {
    let dir = std::env::temp_dir().join(format!(
        "devmap-s6-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let lock_path = dir.join("index.sqlite.writer.lock");
    std::fs::write(&lock_path, "4242\n").unwrap();
    #[cfg(windows)]
    std::fs::write(lock_path.with_extension("lock.owner"), "4242\n").unwrap();

    let mut attempts = 0usize;
    let started = std::time::Instant::now();
    let error = Store::poll_writer_lock(
        || {
            attempts += 1;
            Err(std::fs::TryLockError::WouldBlock)
        },
        std::time::Duration::from_millis(60),
        std::time::Duration::from_millis(10),
        &lock_path,
    )
    .expect_err("a permanently contended lock must time out");

    assert!(attempts >= 2, "contention must be retried, got {attempts}");
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(60),
        "must wait out the deadline: {:?}",
        started.elapsed()
    );
    let text = error.to_string();
    assert!(
        text.contains("another devmap writer holds") && text.contains("pid 4242"),
        "contention must name the recorded holder: {text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// S-6 control: a lock that is granted returns at once.
#[test]
fn s6_writer_lock_returns_as_soon_as_the_lock_is_granted() {
    let mut attempts = 0usize;
    Store::poll_writer_lock(
        || {
            attempts += 1;
            if attempts >= 3 {
                Ok(())
            } else {
                Err(std::fs::TryLockError::WouldBlock)
            }
        },
        std::time::Duration::from_secs(5),
        std::time::Duration::from_millis(1),
        std::path::Path::new("/nonexistent/lock"),
    )
    .expect("a lock granted before the deadline must succeed");
    assert_eq!(attempts, 3);
}

/// S-9: SQLite reads a negative `LIMIT` as *unbounded*.
///
/// `latest_unresolved` bound `params![limit as i64]`. `usize::MAX as i64`
/// is `-1`, and every `usize` at or above `2^63` casts to a negative
/// `i64`, so a caller asking for a very large cap silently got no cap at
/// all — the opposite of the request. Four other bounded readers already
/// clamped inline; this makes that clamp the one owner of the rule so a
/// fifth reader cannot be written without it.
///
/// What this gate can and cannot prove: the *row-count* difference between
/// an unbounded query and one capped at `i64::MAX` is only observable in a
/// table of more than `2^63` rows, so no fixture can exhibit it. The gate
/// is therefore on the binding rule itself, plus the SQLite behaviour it
/// exists for, asserted in the test below.
#[test]
fn s9_a_sqlite_limit_is_never_the_negative_that_means_unbounded() {
    assert_eq!(
        sqlite_limit(usize::MAX),
        i64::MAX,
        "usize::MAX must clamp to the largest cap SQLite can express, \
             not wrap to -1"
    );
    for limit in [
        usize::MAX,
        usize::MAX - 1,
        i64::MAX as usize,
        i64::MAX as usize + 1,
    ] {
        assert!(
            sqlite_limit(limit) > 0,
            "{limit} must not bind a non-positive LIMIT, got {}",
            sqlite_limit(limit)
        );
    }
    // A clamp that flattened everything would be a different silent
    // wrong answer, so the ordinary range must pass through untouched.
    for limit in [0usize, 1, 2, 64, 100_000] {
        assert_eq!(sqlite_limit(limit), limit as i64);
    }
}

/// S-9, the behaviour the clamp protects: `LIMIT -1` really is unbounded
/// in this SQLite build, so the raw cast was not a cosmetic defect.
#[test]
fn s9_negative_limits_are_unbounded_and_the_clamped_one_is_a_cap() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE t (n INTEGER);
             INSERT INTO t (n) VALUES (1), (2), (3);",
    )
    .unwrap();
    let count = |bound: i64| -> usize {
        conn.prepare("SELECT n FROM t LIMIT ?1")
            .unwrap()
            .query_map(params![bound], |row| row.get::<_, i64>(0))
            .unwrap()
            .count()
    };
    assert_eq!(
        count(usize::MAX as i64),
        3,
        "the unclamped cast asks SQLite for every row"
    );
    assert_eq!(count(sqlite_limit(2)), 2, "a real cap still truncates");
}

#[test]
fn fts_match_query_splits_double_colon_qualifiers_and_preserves_safety() {
    assert_eq!(
        fts_match_query("webcontent::snapshot").unwrap(),
        "\"webcontent\"* AND \"snapshot\"*"
    );
    assert_eq!(
        fts_match_query("sysload::webcontent::Ring::dump").unwrap(),
        "\"sysload\"* AND \"webcontent\"* AND \"Ring\"* AND \"dump\"*"
    );
    // Single colons stay inside the quoted FTS literal so column syntax cannot leak.
    assert_eq!(fts_match_query("path:foo").unwrap(), "\"path:foo\"*");
    // Degenerate or empty inputs stay valid quoted FTS literals, and NUL is refused.
    assert_eq!(fts_match_query("::").unwrap(), "\"::\"*");
    assert_eq!(fts_match_query("   ").unwrap(), "\"   \"*");
    assert!(fts_match_query("a\0b").is_err());
}

#[cfg(feature = "parse")]
#[test]
fn partial_parse_gaps_order_charged_rows_before_uncharged_rows_within_cap() {
    use devmap_analyze::analyze;
    use devmap_extract::extract_file;
    use devmap_resolve::Resolver;

    let store = Store::open_in_memory().unwrap();
    let extractions = vec![
        extract_file("a_vendored/a.py", "def a():\n    return 1\n"),
        extract_file("b_vendored/b.py", "def b():\n    return 2\n"),
        extract_file("z_app/view.py", "def view():\n    return 3\n"),
    ];
    let mut resolver = Resolver::new();
    resolver.index_extractions(&extractions);
    let resolution = resolver.resolve_all(&extractions).unwrap();
    let analysis = analyze(&extractions, &resolution);
    let generation = store
        .save_generation_with_opts(
            &extractions,
            &resolution,
            &analysis,
            GenerationWriteOpts::default(),
        )
        .unwrap();

    // Seed coverage-gap rows with alphabetically-earlier uncharged/exempt entries
    // and an alphabetically-later charged entry.
    let conn = store.conn.lock().unwrap();
    let label = devmap_analyze::ExtractionGap::PartialParse.label();
    for (path, reason) in [
        (
            "a_vendored/a.py",
            "1 error region; not charged: would be charged 1 region(s) outside every surviving declaration (0..10)",
        ),
        (
            "b_vendored/b.py",
            "1 error region; not charged: would be charged 1 region(s) outside every surviving declaration (0..10)",
        ),
        (
            "z_app/view.py",
            "1 error region; charged: 1 region(s) outside every surviving declaration (0..10)",
        ),
    ] {
        conn.execute(
            "INSERT INTO generation_coverage_gaps (generation_id, gap, path, reason)
             VALUES (?1, ?2, ?3, ?4)",
            params![generation, label, path, reason],
        )
        .unwrap();
    }
    drop(conn);

    // With limit=1, the charged row must come first despite sorting alphabetically last.
    let (rows, total) = store
        .latest_partial_parse_gaps("surviving declaration (", None, 1)
        .unwrap();
    assert_eq!(total, 3);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].path, "z_app/view.py");
}
