//! The ledger's connection lifecycle and write paths, attacked with real
//! SQLite files, several connections and several processes.

use super::*;
use crate::procguard::LockedSpawn as _;
use crate::test_support::isolated_libtest_command;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;

const CHILD: &str = "ledger::reliability_tests::concurrent_append_child";
const ENV_REPO: &str = "GITPULSE_LEDGER_RELIABILITY_REPO";
const ENV_TAG: &str = "GITPULSE_LEDGER_RELIABILITY_TAG";
const ENV_COUNT: &str = "GITPULSE_LEDGER_RELIABILITY_COUNT";

fn draft(repo: &str, action: &str) -> Draft {
    Draft {
        repo_path: repo.to_string(),
        action: action.to_string(),
        actor_kind: Some(ActorKind::Human),
        outcome: Some(Outcome::Ok),
        ..Default::default()
    }
}

fn repo_dir() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().to_str().unwrap().to_string();
    (dir, repo)
}

fn event_count(repo: &str) -> i64 {
    let db = ledger_path(repo);
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    conn.query_row("SELECT count(*) FROM events", [], |r| r.get(0))
        .unwrap()
}

/// A failed open is returned to its caller and forgotten: the next call
/// retries. Here `.devcouncil` is briefly a regular file.
#[test]
fn a_failed_open_is_not_cached_for_the_process_lifetime() {
    let (dir, repo) = repo_dir();
    let blocker = dir.path().join(".devcouncil");
    std::fs::write(&blocker, b"not a directory").unwrap();
    assert!(append(draft(&repo, "git.commit")).is_err());
    std::fs::remove_file(&blocker).unwrap();
    let second = append(draft(&repo, "git.commit"));
    assert!(second.is_ok(), "{:?}", second.err());
}

/// A connection to a ledger whose `.devcouncil/` was deleted would append to
/// an unlinked inode and report `recording`. It must reopen instead.
#[cfg(unix)]
#[test]
fn a_deleted_state_directory_is_reopened_not_written_into_the_void() {
    let (dir, repo) = repo_dir();
    append(draft(&repo, "git.commit")).unwrap();
    std::fs::remove_dir_all(dir.path().join(".devcouncil")).unwrap();
    append(draft(&repo, "git.push")).unwrap();
    assert!(ledger_path(&repo).exists(), "the ledger was recreated");
    assert_eq!(event_count(&repo), 1, "the new row is in the file on disk");
}

/// One repository's slot lock, held across its IO, must not stall another's.
#[test]
fn a_busy_repository_does_not_stall_another_repository() {
    let (_a_dir, a) = repo_dir();
    let (_b_dir, b) = repo_dir();
    append(draft(&a, "git.commit")).unwrap();
    append(draft(&b, "git.commit")).unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let a2 = a.clone();
    let holder = std::thread::spawn(move || {
        with_conn(&a2, |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(20)).ok();
            Ok(())
        })
    });
    entered_rx.recv_timeout(Duration::from_secs(20)).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let b2 = b.clone();
    std::thread::spawn(move || done_tx.send(append(draft(&b2, "git.push"))).unwrap());
    let outcome = done_rx.recv_timeout(Duration::from_secs(5));
    release_tx.send(()).unwrap();
    holder.join().unwrap().unwrap();
    assert!(
        outcome.is_ok_and(|r| r.is_ok()),
        "repository B waited on repository A's connection"
    );
}

#[cfg(unix)]
#[test]
fn status_readonly_opens_read_only_and_leaves_the_ledger_untouched() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, repo) = repo_dir();
    append(draft(&repo, "git.commit")).unwrap();
    let state = dir.path().join(".devcouncil");
    let db = state.join("ledger.sqlite");
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
    let version = |conn: &Connection| -> i64 {
        conn.query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap()
    };
    let probe = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let before = version(&probe);
    let status = status_readonly(&repo);
    assert!(status.recording, "{}", status.error);
    assert_eq!(version(&probe), before, "data_version moved");

    // An existing database with no schema is reported, never conjured.
    let (bare_dir, bare) = repo_dir();
    let bare_state = bare_dir.path().join(".devcouncil");
    std::fs::create_dir(&bare_state).unwrap();
    std::fs::set_permissions(&bare_state, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bare_db = bare_state.join("ledger.sqlite");
    std::fs::write(&bare_db, b"").unwrap();
    std::fs::set_permissions(&bare_db, std::fs::Permissions::from_mode(0o644)).unwrap();
    let status = status_readonly(&bare);
    assert!(!status.recording);
    assert_eq!(status.error_code, "not_initialised");
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!((mode(&bare_state), mode(&bare_db)), (0o755, 0o644));
    let ro = Connection::open_with_flags(&bare_db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let tables: i64 = ro
        .query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))
        .unwrap();
    assert_eq!(tables, 0, "status_readonly ran DDL");
    assert!(tail_readonly(&bare, 0, 10).is_err());
}

#[test]
fn tail_readonly_pages_without_a_writable_connection() {
    let (_dir, repo) = repo_dir();
    for _ in 0..3 {
        append(draft(&repo, "git.commit")).unwrap();
    }
    let events = tail_readonly(&repo, 1, 10).unwrap().unwrap();
    assert_eq!(events.len(), 2);
}

#[test]
fn an_action_outside_the_schema_pattern_is_refused() {
    let (_dir, repo) = repo_dir();
    for bad in [
        "commit",
        "Git.commit",
        "git.",
        ".commit",
        "git commit",
        "git.com-mit",
        "1git.x",
        "git..x",
    ] {
        let err = append(draft(&repo, bad)).unwrap_err();
        assert_eq!(err.code, "invalid_action", "{bad}");
    }
    assert!(append(draft(&repo, "worktree.bind")).is_ok());
    assert!(append(draft(&repo, "ci.step_2.done")).is_ok());
}

/// Not a test on its own: the child half of
/// [`two_processes_appending_concurrently_drop_nothing`].
#[test]
fn concurrent_append_child() {
    let (Ok(repo), Ok(tag), Ok(count)) = (
        std::env::var(ENV_REPO),
        std::env::var(ENV_TAG),
        std::env::var(ENV_COUNT),
    ) else {
        return;
    };
    for n in 0..count.parse::<u32>().unwrap() {
        let mut d = draft(&repo, "git.commit");
        d.object = Some(format!("{tag}-{n}"));
        append(d).expect("append under contention");
        // The multi-statement writers contend with the appends.
        forget_transcripts(&repo, &repo, &[format!("{tag}-{n}")]).expect("forget");
    }
}

#[test]
fn two_processes_appending_concurrently_drop_nothing() {
    const EACH: u32 = 150;
    let (_dir, repo) = repo_dir();
    append(draft(&repo, "git.commit")).unwrap();
    let (command, _harness) = isolated_libtest_command(CHILD);
    let program = command.get_program().to_owned();
    let mut children: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|tag| {
            std::process::Command::new(&program)
                .args(["--exact", CHILD, "--nocapture"])
                .env(ENV_REPO, &repo)
                .env(ENV_TAG, tag)
                .env(ENV_COUNT, EACH.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn_locked()
                .expect("spawn child")
        })
        .collect();
    for child in &mut children {
        let status = child.wait().expect("wait");
        assert!(status.success(), "a writer process failed under contention");
    }
    assert_eq!(event_count(&repo), 1 + 2 * i64::from(EACH));
}

#[test]
fn fleet_metrics_languages_and_history_commit_together() {
    let (_dir, repo) = repo_dir();
    let stat = |language: &str| FleetLanguageStat {
        language: language.to_string(),
        color_hex: "#000000".to_string(),
        category: "programming".to_string(),
        code_lines: 10,
        file_count: 1,
        percentage: 100.0,
    };
    // A language row the second scan cannot write (the table is dropped
    // under it) must roll the scalar row and the history back with it.
    save_fleet_metrics(
        &repo,
        &FleetMetricsInput {
            loc: Some(5),
            languages: Some(vec![stat("Rust")]),
            ..Default::default()
        },
    )
    .unwrap();
    with_conn(&repo, |conn| {
        conn.execute_batch("DROP TABLE fleet_languages;")
            .map_err(|e| LedgerError::new("t", e.to_string()))
    })
    .unwrap();
    let failed = save_fleet_metrics(
        &repo,
        &FleetMetricsInput {
            loc: Some(999),
            languages: Some(vec![stat("Go")]),
            ..Default::default()
        },
    );
    assert!(failed.is_err());
    let loc: i64 = with_conn(&repo, |conn| {
        conn.query_row("SELECT loc FROM fleet_metrics", [], |r| r.get(0))
            .map_err(|e| LedgerError::new("t", e.to_string()))
    })
    .unwrap();
    assert_eq!(loc, 5, "the scalar row committed without its languages");
}

#[test]
fn the_reflog_identity_query_uses_its_partial_index() {
    let (_dir, repo) = repo_dir();
    append(draft(&repo, "git.commit")).unwrap();
    let plan: Vec<String> = with_conn(&repo, |conn| {
        let mut stmt = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {RECORDED_REFLOG_SQL}"))
            .map_err(|e| LedgerError::new("t", e.to_string()))?;
        let rows = stmt
            .query_map(params!["a", "a", true], |row| row.get::<_, String>(3))
            .map_err(|e| LedgerError::new("t", e.to_string()))?;
        rows.collect::<Result<_, _>>()
            .map_err(|e| LedgerError::new("t", e.to_string()))
    })
    .unwrap();
    assert!(
        plan.iter().all(|step| step.contains("idx_events_reflog")),
        "{plan:?}"
    );
}

/// The per-pass legacy read is bounded by append time: it walks the time index,
/// not the checkout's whole reflog history.
#[test]
fn the_legacy_reflog_read_since_a_time_uses_the_time_index() {
    let (_dir, repo) = repo_dir();
    append(draft(&repo, "git.commit")).unwrap();
    let plan: Vec<String> = with_conn(&repo, |conn| {
        let mut stmt = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {RECORDED_REFLOG_SINCE_SQL}"))
            .map_err(|e| LedgerError::new("t", e.to_string()))?;
        let rows = stmt
            .query_map(params!["a", "a", true, "2026-01-01T00:00:00.000Z"], |row| {
                row.get::<_, String>(3)
            })
            .map_err(|e| LedgerError::new("t", e.to_string()))?;
        rows.collect::<Result<_, _>>()
            .map_err(|e| LedgerError::new("t", e.to_string()))
    })
    .unwrap();
    assert!(
        plan.iter().all(|step| step.contains("idx_events_ts")),
        "{plan:?}"
    );
}

#[test]
fn recorded_reflog_objects_is_scoped_to_the_checkout() {
    let (_dir, repo) = repo_dir();
    let mut main = draft(&repo, "reflog.commit");
    main.object = Some("a1 main".into());
    append(main).unwrap();
    let mut linked = draft(&repo, "reflog.commit");
    linked.object = Some("b2 linked".into());
    linked.worktree_path = Some("/elsewhere/linked".into());
    append(linked).unwrap();
    let mut other = draft(&repo, "git.commit");
    other.object = Some("c3 not a reflog row".into());
    append(other).unwrap();

    let anchor = recorded_reflog_objects(&repo, &repo, true).unwrap();
    assert_eq!(anchor, ["a1 main".to_string()].into());
    let linked = recorded_reflog_objects(&repo, "/elsewhere/linked", false).unwrap();
    assert_eq!(linked, ["b2 linked".to_string()].into());
}

#[test]
fn pruning_keeps_the_newest_events_and_every_binding() {
    let (_dir, repo) = repo_dir();
    for _ in 0..12 {
        append(draft(&repo, "git.commit")).unwrap();
    }
    append(draft(&repo, "worktree.bind")).unwrap();
    // Age everything: the policy deletes by age, so push the stored times back.
    with_conn(&repo, |conn| {
        conn.execute("UPDATE events SET ts_utc = '2001-01-01T00:00:00.000Z'", [])
            .map_err(|e| LedgerError::new("t", e.to_string()))
    })
    .unwrap();
    let before = latest_cursor(&repo).unwrap();

    // Nothing is older than a day only if it is new: these are all old.
    let deleted = prune_events(&repo, 1, 5).unwrap();
    assert_eq!(
        deleted, 8,
        "13 rows less the newest 5 are 8 candidates, none of them the bind"
    );
    assert_eq!(
        prune_events(&repo, 1, 5).unwrap(),
        0,
        "a second pass has nothing left"
    );
    let rest = tail(&repo, 0, 100).unwrap();
    assert_eq!(rest.len(), 5);
    assert!(rest.iter().any(|e| e.action == "worktree.bind"));
    assert_eq!(
        latest_cursor(&repo).unwrap(),
        before,
        "the highest cursor survives"
    );
    // A row appended after a prune takes a fresh id, never a reused one.
    let next = append(draft(&repo, "git.push")).unwrap();
    assert_eq!(next, before + 1);
    // Age decides, not just position: the fresh row survives even a keep of
    // one, and the old non-binding rows go.
    assert_eq!(prune_events(&repo, 1, 1).unwrap(), 4);
    let left: Vec<String> = tail(&repo, 0, 100)
        .unwrap()
        .into_iter()
        .map(|e| e.action)
        .collect();
    assert_eq!(left, ["worktree.bind", "git.push"]);
}

/// Inserts `count` events stamped `ts` with one statement, so a policy sized
/// in thousands of rows is not tested by thousands of appends.
fn insert_events(repo: &str, count: i64, ts: &str, tag: &str) {
    with_conn(repo, |conn| {
        conn.execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
             INSERT INTO events (ulid, ts_utc, repo_path, actor_kind, action, outcome)
             SELECT ?3 || i, ?2, 'r', 'system', 'git.commit', 'ok' FROM n",
            params![count, ts, tag],
        )
        .map_err(|e| LedgerError::new("t", e.to_string()))
    })
    .unwrap();
}

/// Catch-up with the transcript root pointed at an empty directory, so the
/// pass touches nothing of the machine's real history.
fn hermetic_catch_up(repo: &str) -> crate::ingest::CatchUp {
    let root = tempfile::tempdir().unwrap();
    let serial = crate::test_support::env::env_serial();
    let _env =
        crate::test_support::env::bind_env(&serial).set("GITPULSE_TRANSCRIPT_ROOT", root.path());
    crate::ingest::catch_up_into_bounded(repo, repo, None)
}

/// `prune_events` had no caller, so the events table only ever grew. Catch-up
/// is the pass every host runs; it applies the retention policy.
#[test]
fn catch_up_prunes_events_past_the_retention_policy() {
    let (_dir, repo) = repo_dir();
    insert_events(
        &repo,
        RETAIN_NEWEST_EVENTS + 50,
        "2001-01-01T00:00:00.000Z",
        "old",
    );
    let report = hermetic_catch_up(&repo);
    assert_eq!(report.pruned, 50, "{}", report.error);
    assert_eq!(event_count(&repo), RETAIN_NEWEST_EVENTS);
}

/// The window is the contract: a row younger than the retention age survives
/// even when it is not among the newest rows.
#[test]
fn catch_up_never_prunes_inside_the_retention_window() {
    let (_dir, repo) = repo_dir();
    let recent = ids::iso8601_utc(ids::now_millis());
    insert_events(&repo, RETAIN_NEWEST_EVENTS + 50, &recent, "new");
    let report = hermetic_catch_up(&repo);
    assert_eq!(report.pruned, 0, "{}", report.error);
    assert_eq!(event_count(&repo), RETAIN_NEWEST_EVENTS + 50);
}

/// A prune that fails is reported on the pass, not folded into "nothing to
/// prune".
#[test]
fn a_failing_prune_is_surfaced_on_the_catch_up_report() {
    let (_dir, repo) = repo_dir();
    insert_events(
        &repo,
        RETAIN_NEWEST_EVENTS + 5,
        "2001-01-01T00:00:00.000Z",
        "old",
    );
    with_conn(&repo, |conn| {
        conn.execute_batch(
            "CREATE TRIGGER no_delete BEFORE DELETE ON events
             BEGIN SELECT RAISE(ABORT, 'delete refused'); END;",
        )
        .map_err(|e| LedgerError::new("t", e.to_string()))
    })
    .unwrap();
    let report = hermetic_catch_up(&repo);
    assert!(
        report.error.contains("ledger retention failed"),
        "error was {:?}",
        report.error
    );
    assert_eq!(report.pruned, 0);
    assert_eq!(event_count(&repo), RETAIN_NEWEST_EVENTS + 5);
}

#[test]
fn a_pass_past_its_deadline_starts_no_batch() {
    let (_dir, repo) = repo_dir();
    insert_events(&repo, 20, "2001-01-01T00:00:00.000Z", "old");
    let expired = std::time::Instant::now() - Duration::from_secs(1);
    let report = prune_with_policy(&repo, 1, 5, Some(expired));
    assert_eq!((report.deleted, report.error.as_str()), (0, ""));
    assert_eq!(event_count(&repo), 20);
    let report = prune_with_policy(&repo, 1, 5, None);
    assert_eq!(report.deleted, 15);
}

fn age_all_events(repo: &str) {
    with_conn(repo, |conn| {
        conn.execute("UPDATE events SET ts_utc = '2001-01-01T00:00:00.000Z'", [])
            .map_err(|e| LedgerError::new("t", e.to_string()))
    })
    .unwrap();
}

/// Finding the candidates scans every row older than the newest
/// `keep_newest` inside the write transaction. On a year of real events that
/// is close to a second of write lock per catch-up when nothing has expired,
/// so a pass that reached "nothing left" is not repeated for a day.
#[test]
fn a_clean_prune_pass_is_not_repeated_within_the_interval() {
    let (_dir, repo) = repo_dir();
    let recent = ids::iso8601_utc(ids::now_millis());
    insert_events(&repo, 20, &recent, "new");
    let first = prune_with_policy(&repo, 1, 5, None);
    assert_eq!((first.deleted, first.error.as_str()), (0, ""));
    age_all_events(&repo);
    let second = prune_with_policy(&repo, 1, 5, None);
    assert_eq!(
        second.deleted, 0,
        "a clean ledger was scanned again at once"
    );
    assert_eq!(event_count(&repo), 20);
}

/// Only a pass that reached the end is remembered: one stopped by its
/// deadline or its batch cap leaves the backlog to the very next pass.
#[test]
fn an_unfinished_prune_pass_does_not_defer_the_next() {
    let (_dir, repo) = repo_dir();
    insert_events(&repo, 20, "2001-01-01T00:00:00.000Z", "old");
    let expired = std::time::Instant::now() - Duration::from_secs(1);
    assert_eq!(prune_with_policy(&repo, 1, 5, Some(expired)).deleted, 0);
    assert_eq!(prune_with_policy(&repo, 1, 5, None).deleted, 15);
}

/// A row exactly at the cutoff is not older than it, and the floor is
/// counted in ids from the newest: an empty ledger, a ledger no larger than
/// the floor, and a floor of zero (clamped to one) all leave the newest row.
#[test]
fn prune_boundaries_hold_on_empty_floor_and_clamped_inputs() {
    let (_dir, repo) = repo_dir();
    assert_eq!(prune_events(&repo, 1, 5).unwrap(), 0, "empty ledger");
    insert_events(&repo, 5, "2001-01-01T00:00:00.000Z", "old");
    assert_eq!(prune_events(&repo, 1, 5).unwrap(), 0, "at the floor");
    assert_eq!(prune_events(&repo, 1, 0).unwrap(), 4, "floor clamps to one");
    assert_eq!(event_count(&repo), 1);
}

/// Rows stamped in the future (a clock that ran ahead) are never older than
/// the cutoff, so they are kept however far past the floor they sit.
#[test]
fn future_stamped_events_are_never_pruned() {
    let (_dir, repo) = repo_dir();
    insert_events(&repo, 30, "2999-01-01T00:00:00.000Z", "future");
    assert_eq!(prune_events(&repo, 1, 5).unwrap(), 0);
    assert_eq!(event_count(&repo), 30);
}

/// Several writers meeting a ledger that does not exist yet all open it. The
/// journal-mode switch to WAL can answer `database is locked` at once, without
/// consulting the busy handler, when another connection is switching the same
/// file; a writer that gave up there left its caller (a replay child, in the
/// two-process reflog test) with an error and no work done.
#[test]
fn writers_racing_to_create_one_ledger_all_open_it() {
    const WRITERS: usize = 8;
    for round in 0..25 {
        let (_dir, repo) = repo_dir();
        let gate = std::sync::Arc::new(std::sync::Barrier::new(WRITERS));
        let opened: Vec<_> = (0..WRITERS)
            .map(|_| {
                let (gate, repo) = (gate.clone(), repo.clone());
                std::thread::spawn(move || {
                    gate.wait();
                    open(&ledger_path(&repo))
                        .map(drop)
                        .map_err(|e| e.to_string())
                })
            })
            .collect();
        for result in opened {
            assert_eq!(result.join().unwrap(), Ok(()), "round {round}");
        }
    }
}
