//! Freshness, recording honesty and drop scope of read-only ledger answers.

use crate::ledger::*;
use crate::procguard::LockedSpawn as _;
use crate::test_support::isolated_libtest_command;
use std::process::Stdio;

const DROP_CHILD: &str = "ledger::freshness::tests::dropped_in_another_process_child";
const ENV_REPO: &str = "GITPULSE_LEDGER_FRESHNESS_REPO";

fn draft(repo: &str) -> Draft {
    Draft {
        repo_path: repo.to_string(),
        action: "git.commit".to_string(),
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

fn minutes(n: u64) -> u64 {
    n * 60 * 1000
}

/// The `ts_utc` of the newest row, as epoch milliseconds.
fn newest_ms(repo: &str) -> u64 {
    let ts = freshness_readonly_at(repo, 0).as_of.expect("a row");
    ids::parse_event_time(&ts).expect("a ledger timestamp")
}

#[test]
fn an_empty_ledger_has_no_as_of_and_is_stale_with_a_reason() {
    let (_dir, repo) = repo_dir();
    with_conn(&repo, |_| Ok(())).expect("create the schema, no rows");

    let status = status_readonly_at(&repo, ids::now_millis());

    assert_eq!(status.freshness.as_of, None, "no row, so no time to invent");
    assert_eq!(status.freshness.last_catch_up, None);
    assert_eq!(status.freshness.last_catch_up_basis, "none");
    assert!(status.freshness.stale);
    assert!(
        status.freshness.stale_reason.contains("no events"),
        "{}",
        status.freshness.stale_reason
    );
    assert!(status.freshness.freshness_ok, "the ledger read fine");
}

#[test]
fn recent_rows_are_fresh_and_old_ones_are_stale_by_the_named_window() {
    let (_dir, repo) = repo_dir();
    append(draft(&repo)).unwrap();
    let at = newest_ms(&repo);

    let fresh = freshness_readonly_at(&repo, at + minutes(1));
    assert!(!fresh.stale, "{fresh:?}");
    assert!(fresh.stale_reason.is_empty());
    assert_eq!(fresh.last_catch_up_basis, "newest_row");

    let window = STALE_AFTER_SECS * 1000;
    let edge = freshness_readonly_at(&repo, at + window);
    assert!(!edge.stale, "exactly at the window is still inside it");
    let old = freshness_readonly_at(&repo, at + window + 1000);
    assert!(old.stale);
    assert!(old.stale_reason.contains("600s"), "{}", old.stale_reason);
    assert_eq!(old.as_of, fresh.as_of, "as_of is the data, not the clock");
}

#[test]
fn a_clock_set_backwards_is_never_called_fresh() {
    let (_dir, repo) = repo_dir();
    append(draft(&repo)).unwrap();
    let at = newest_ms(&repo);

    let skewed = freshness_readonly_at(&repo, at - minutes(60));

    assert!(skewed.stale, "evidence from the future has no age");
    assert!(
        skewed.stale_reason.contains("in the future"),
        "{}",
        skewed.stale_reason
    );
    let small_step = freshness_readonly_at(&repo, at - 5_000);
    assert!(!small_step.stale, "a few seconds of skew is not an alarm");
}

#[test]
fn a_persisted_catch_up_is_the_last_catch_up_and_carries_what_it_covered() {
    let (_dir, repo) = repo_dir();
    append(draft(&repo)).unwrap();
    let row_at = newest_ms(&repo);
    let pass_at = row_at + minutes(30);
    record_catch_up(
        &repo,
        &repo,
        &CatchUpRecord {
            completed: true,
            truncated: true,
            recorded: 4,
            transcripts: 2,
            transcripts_listed: 5000,
            transcripts_unlisted: 1,
            skipped_lines: 3,
            failed_records: 0,
            error: "catch-up truncated: the transcript walk left 1 file(s) unlisted".into(),
            ..CatchUpRecord::default()
        },
        pass_at,
    )
    .unwrap();

    let read = freshness_readonly_at(&repo, pass_at + minutes(1));

    assert_eq!(read.last_catch_up_basis, "catch_up_state");
    assert_eq!(
        read.last_catch_up.as_deref(),
        Some(ids::iso8601_utc(pass_at).as_str())
    );
    assert!(
        !read.stale,
        "a pass a minute ago is evidence, whatever the rows say"
    );
    let covered = read.catch_up.expect("the pass was persisted");
    assert!(covered.truncated);
    assert_eq!(covered.skipped_lines, 3, "skipped lines must be visible");
    assert_eq!(covered.transcripts_unlisted, 1);
    assert_eq!(covered.transcripts_listed, 5000);
    assert!(covered.error.contains("unlisted"));
}

#[test]
fn a_pass_that_did_not_run_does_not_move_the_last_catch_up() {
    let (_dir, repo) = repo_dir();
    append(draft(&repo)).unwrap();
    let t0 = newest_ms(&repo) + minutes(1);
    let ok = CatchUpRecord {
        completed: true,
        ..CatchUpRecord::default()
    };
    record_catch_up(&repo, &repo, &ok, t0).unwrap();
    let failed = CatchUpRecord {
        completed: false,
        error: "the worktree family could not be resolved".into(),
        ..CatchUpRecord::default()
    };
    record_catch_up(&repo, &repo, &failed, t0 + minutes(60)).unwrap();

    let read = freshness_readonly_at(&repo, t0 + minutes(61));

    assert_eq!(
        read.last_catch_up.as_deref(),
        Some(ids::iso8601_utc(t0).as_str())
    );
    assert!(read.stale, "the only completion is an hour old");
    assert!(read.catch_up.expect("row").error.contains("family"));
}

#[test]
fn recording_a_pass_never_creates_a_ledger() {
    let (_dir, repo) = repo_dir();
    record_catch_up(&repo, &repo, &CatchUpRecord::default(), 1).unwrap();
    assert!(!is_initialised(&repo));
}

#[test]
fn a_read_only_open_is_readable_never_recording() {
    let (_dir, repo) = repo_dir();
    append(draft(&repo)).unwrap();

    let readonly = status_readonly(&repo);

    assert!(
        !readonly.recording,
        "an open that cannot write is not recording"
    );
    assert_eq!(readonly.recording_state, "readable");
    assert_eq!(readonly.dropped_scope, "this_process");
    let json = serde_json::to_value(&readonly).unwrap();
    assert_eq!(json["recording"], false);
    assert_eq!(json["recording_state"], "readable");
    assert_eq!(json["dropped_scope"], "this_process");
    assert!(
        json["as_of"].is_string(),
        "flattened beside recording: {json}"
    );
    assert!(json["stale"].is_boolean());
    // The creating path in the writing process still says so.
    let writing = status(&repo);
    assert!(writing.recording);
    assert_eq!(writing.recording_state, "recording");
}

#[test]
fn no_ledger_is_unknown_and_stale_with_the_reason() {
    let (_dir, repo) = repo_dir();
    let status = status_readonly(&repo);
    assert!(!status.recording);
    assert_eq!(status.recording_state, "unknown");
    assert!(status.freshness.stale);
    assert!(!status.freshness.freshness_ok);
    assert!(status.freshness.freshness_error.contains("no ledger"));
}

#[test]
fn an_unreadable_database_is_never_reported_fresh() {
    let (_dir, repo) = repo_dir();
    append(draft(&repo)).unwrap();
    let db = ledger_path(&repo);
    // A file that is not a database: the open succeeds, every query fails.
    let garbage = [0x5a_u8; 4096];
    let moved = db.with_extension("sqlite.orig");
    std::fs::rename(&db, &moved).unwrap();
    // The write-ahead log and its index would otherwise be replayed over the
    // garbage and make it read as the old database.
    for suffix in ["sqlite-wal", "sqlite-shm"] {
        let _ = std::fs::remove_file(db.with_extension(suffix));
    }
    std::fs::write(&db, garbage).unwrap();

    let status = status_readonly_at(&repo, ids::now_millis());

    assert_eq!(status.recording_state, "unknown", "{status:?}");
    assert!(!status.recording);
    assert!(status.freshness.stale, "{status:?}");
    assert!(!status.freshness.freshness_ok);
    assert!(!status.freshness.freshness_error.is_empty());
    assert!(!status.error.is_empty());
    assert_eq!(status.freshness.as_of, None);
}

#[test]
fn a_ledger_older_than_the_catch_up_table_still_answers_from_its_newest_row() {
    let (_dir, repo) = repo_dir();
    let db = crate::ledger::schema_bump_tests::old_ledger(&repo);
    let conn = Connection::open(&db).unwrap();
    conn.execute("DROP TABLE IF EXISTS catch_up_state", [])
        .unwrap();
    crate::ledger::schema_bump_tests::old_insert(
        &conn,
        "01HZZZZZZZZZZZZZZZZZZZZZZZ",
        None,
        "git.commit",
        None,
        None,
    );
    drop(conn);

    let read = freshness_readonly_at(&repo, ids::now_millis());

    assert!(read.freshness_ok, "{read:?}");
    assert_eq!(read.last_catch_up_basis, "newest_row");
    assert!(read.catch_up.is_none());
}

#[test]
fn freshness_that_was_never_read_serialises_as_stale() {
    let unread = Freshness::default();
    assert!(unread.stale);
    assert!(!unread.freshness_ok);
}

/// Not a test on its own: the child half of
/// [`a_drop_in_another_process_is_not_claimed_as_zero_everywhere`]. It drops
/// one append through the production counter, in its own process.
#[test]
fn dropped_in_another_process_child() {
    let Ok(repo) = std::env::var(ENV_REPO) else {
        return;
    };
    record_dropped_append(&repo);
    assert_eq!(dropped_appends(&repo), 1);
}

#[test]
fn a_drop_in_another_process_is_not_claimed_as_zero_everywhere() {
    let (_dir, repo) = repo_dir();
    append(draft(&repo)).unwrap();
    let (command, _harness) = isolated_libtest_command(DROP_CHILD);
    let program = command.get_program().to_owned();
    let status = std::process::Command::new(&program)
        .args(["--exact", DROP_CHILD, "--nocapture"])
        .env(ENV_REPO, &repo)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn_locked()
        .expect("spawn child")
        .wait()
        .expect("wait");
    assert!(status.success(), "process A could not drop an append");

    // Process B (this one) cannot see A's count, and must not pass its own 0
    // off as the repository's.
    let reader = status_readonly(&repo);

    assert_eq!(reader.dropped, 0, "B's own count");
    assert_eq!(reader.dropped_scope, "this_process");
    assert_eq!(
        serde_json::to_value(&reader).unwrap()["dropped_scope"],
        "this_process",
        "the label travels with the number over the wire"
    );
}
