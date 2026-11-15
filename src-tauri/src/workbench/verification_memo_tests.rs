//! What the verdict memo may and may not answer from memory.
//!
//! Every test counts work, never time: `seam::walks` is how many times one
//! store file's evidence table was walked, `seam::rows` how many rows those
//! walks looked at and `seam::kept_rows` how many they copied out. Each test
//! builds its own [`Memo`], so what one holds is never another's to evict.

use super::tests::profile;
use crate::workbench::verification_gate::memo::{seam, WALK_SQL};
use crate::workbench::verification_gate::{Memo, Record, EVIDENCE_TYPE};
use dc_store::Store;
use std::collections::HashMap;
use std::path::Path;

fn put(store: &Store, task: &str, outcome: &str) {
    store
        .evidence_append(
            EVIDENCE_TYPE,
            Some(task),
            None,
            None,
            &format!(
                r#"{{"item_id":"{task}","request_id":"r-{outcome}","state":"settled","outcome":"{outcome}"}}"#
            ),
        )
        .unwrap();
}

fn ids(of: &[&str]) -> Vec<String> {
    of.iter().map(|id| (*id).to_owned()).collect()
}

fn outcome_of(found: &HashMap<String, Record>, task: &str) -> Option<String> {
    found.get(task).map(|r| r.outcome.clone())
}

fn outcome(memo: &Memo, store: &Store, task: &str) -> Option<String> {
    outcome_of(&memo.latest_many(store, &ids(&[task])).unwrap(), task)
}

/// Several records per task, a task with none, rows of another kind newer than
/// a task's verdicts, and a row with no task. Newest first it reads: b, c, a.
fn fixture(store: &Store) {
    put(store, "a", "failed");
    put(store, "b", "failed");
    put(store, "a", "degraded");
    store
        .evidence_append("other.kind", Some("a"), None, None, "{\"x\":1}")
        .unwrap();
    put(store, "a", "passed");
    put(store, "c", "passed");
    put(store, "b", "passed");
    store
        .evidence_append("other.kind", Some("b"), None, None, "{\"x\":2}")
        .unwrap();
    store
        .evidence_append(EVIDENCE_TYPE, None, None, None, "{\"item_id\":\"\"}")
        .unwrap();
    store
        .evidence_append("other.kind", Some("d"), None, None, "{\"x\":3}")
        .unwrap();
}

fn fixture_store(path: &Path) -> Store {
    let store = Store::open(path).unwrap();
    fixture(&store);
    store
}

/// The query `latest_many` used to be, kept as the oracle the single pass is
/// compared with.
fn sql_oracle(store: &Store, item_ids: &[String]) -> HashMap<String, Record> {
    let ids = serde_json::to_string(item_ids).unwrap();
    let mut statement = store
        .connection()
        .prepare(
            "SELECT e.task_id, e.data_json FROM evidence e
             JOIN (SELECT MAX(id) AS id FROM evidence
                   WHERE type = ?1 AND task_id IN (SELECT value FROM json_each(?2))
                   GROUP BY task_id) newest ON newest.id = e.id",
        )
        .unwrap();
    statement
        .query_map(rusqlite::params![EVIDENCE_TYPE, ids], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|row| {
            let (task, data) = row.unwrap();
            (task, serde_json::from_str(&data).unwrap())
        })
        .collect()
}

/// A checkpoint that has to wait for a reader reports busy. A memo that kept a
/// read transaction open between calls would pin the log.
fn log_is_free(path: &Path) -> bool {
    let other = rusqlite::Connection::open(path).unwrap();
    let busy: i64 = other
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
        .unwrap();
    busy == 0
}

#[test]
fn the_single_pass_answers_what_the_old_query_answered() {
    let (_d, path) = profile();
    let store = fixture_store(&path);
    for asked in [
        ids(&["a", "b", "c", "d"]),
        ids(&["a"]),
        ids(&["d"]),
        ids(&["c", "nobody", "a"]),
        ids(&["a", "a", "b"]),
        ids(&["nobody"]),
        ids(&[""]),
    ] {
        // A memo of its own each time, so every answer is a fresh read.
        let memo = Memo::new();
        assert_eq!(
            memo.latest_many(&store, &asked).unwrap(),
            sql_oracle(&store, &asked),
            "{asked:?}"
        );
    }
    assert!(Memo::new().latest_many(&store, &[]).unwrap().is_empty());
}

#[test]
fn the_oracle_itself_sees_the_newest_row_past_newer_rows_of_other_kinds() {
    let (_d, path) = profile();
    let store = fixture_store(&path);
    let found = sql_oracle(&store, &ids(&["a", "b", "c", "d"]));
    assert_eq!(outcome_of(&found, "a").as_deref(), Some("passed"));
    assert_eq!(outcome_of(&found, "b").as_deref(), Some("passed"));
    assert!(!found.contains_key("d"));
}

#[test]
fn the_walk_needs_no_sort() {
    let (_d, path) = profile();
    let store = fixture_store(&path);
    let mut statement = store
        .connection()
        .prepare(&format!("EXPLAIN QUERY PLAN {WALK_SQL}"))
        .unwrap();
    let plan: Vec<String> = statement
        .query_map([EVIDENCE_TYPE], |row| row.get::<_, String>(3))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        plan.iter().any(|step| step.contains("SCAN evidence")),
        "{plan:?}"
    );
    assert!(
        !plan.iter().any(|step| step.contains("TEMP B-TREE")),
        "the walk would sort through temporary files: {plan:?}"
    );
}

#[test]
fn a_full_walk_copies_each_tasks_record_once() {
    let (_d, path) = profile();
    let store = fixture_store(&path);
    let memo = Memo::new();
    assert_eq!(outcome(&memo, &store, "never").as_deref(), None);
    assert_eq!(seam::walks(&path), 1);
    assert_eq!(seam::rows(&path), 7, "every verification row was looked at");
    assert_eq!(
        seam::kept_rows(&path),
        3,
        "one copy per task (a, b, c), not one per row"
    );
}

#[test]
fn an_unchanged_store_is_walked_once_however_many_sweeps_ask() {
    let (_d, path) = profile();
    // Written a moment ago: there is no recency rule to wait out.
    let store = fixture_store(&path);
    let memo = Memo::new();
    let wanted = ids(&["a", "b", "c", "d"]);
    for _ in 0..10 {
        let found = memo.latest_many(&store, &wanted).unwrap();
        assert_eq!(outcome_of(&found, "a").as_deref(), Some("passed"));
        assert_eq!(outcome_of(&found, "b").as_deref(), Some("passed"));
        assert_eq!(outcome_of(&found, "c").as_deref(), Some("passed"));
        assert!(!found.contains_key("d"));
    }
    assert_eq!(
        seam::walks(&path),
        1,
        "ten reads of a store that did not change walked its evidence more than once"
    );
}

#[test]
fn a_connection_per_read_shares_the_one_walk() {
    let (_d, path) = profile();
    drop(fixture_store(&path));
    let memo = Memo::new();
    // A different task set on a different, read-only connection each time: the
    // way the board asks. The first asks for a task that was never verified, so
    // the whole table is read and every later answer is already known.
    for asked in [
        ids(&["never"]),
        ids(&["a"]),
        ids(&["b", "c"]),
        ids(&["a", "d"]),
    ] {
        let reader = Store::open_readonly(&path).unwrap();
        assert_eq!(
            memo.latest_many(&reader, &asked).unwrap(),
            sql_oracle(&reader, &asked)
        );
    }
    assert_eq!(seam::walks(&path), 1);
}

#[test]
fn a_walk_for_particular_tasks_stops_when_it_has_them() {
    let (_d, path) = profile();
    let store = Store::open(&path).unwrap();
    for _ in 0..50 {
        put(&store, "old", "failed");
    }
    put(&store, "target", "passed");
    let memo = Memo::new();
    assert_eq!(outcome(&memo, &store, "target").as_deref(), Some("passed"));
    assert_eq!(seam::walks(&path), 1);
    assert_eq!(
        seam::rows(&path),
        1,
        "the newest row is the card's; nothing after it was read"
    );
    assert_eq!(outcome(&memo, &store, "target").as_deref(), Some("passed"));
    assert_eq!(seam::walks(&path), 1, "and it is held");
}

#[test]
fn what_one_ask_learned_serves_the_next() {
    let (_d, path) = profile();
    let store = fixture_store(&path);
    let memo = Memo::new();
    assert_eq!(outcome(&memo, &store, "b").as_deref(), Some("passed"));
    assert_eq!(outcome(&memo, &store, "c").as_deref(), Some("passed"));
    assert_eq!(seam::walks(&path), 2, "a task not yet known is looked for");
    let both = memo.latest_many(&store, &ids(&["b", "c"])).unwrap();
    assert_eq!(both.len(), 2);
    assert_eq!(seam::walks(&path), 2, "the two merge");
    // A task never verified is proven absent only by the whole table.
    assert_eq!(outcome(&memo, &store, "never"), None);
    assert_eq!(outcome(&memo, &store, "never"), None);
    assert_eq!(outcome(&memo, &store, "a").as_deref(), Some("passed"));
    assert_eq!(seam::walks(&path), 3);
}

#[test]
fn an_empty_list_reads_nothing_even_on_a_cold_store() {
    let (_d, path) = profile();
    let store = fixture_store(&path);
    assert!(Memo::new().latest_many(&store, &[]).unwrap().is_empty());
    assert_eq!(seam::walks(&path), 0);
}

#[test]
fn a_row_appended_by_a_second_connection_is_seen() {
    let (_d, path) = profile();
    let store = fixture_store(&path);
    let memo = Memo::new();
    let wanted = ids(&["a", "e"]);
    assert!(!memo.latest_many(&store, &wanted).unwrap().contains_key("e"));
    let other = Store::open(&path).unwrap();
    put(&other, "e", "passed");
    put(&other, "a", "failed");
    let found = memo.latest_many(&store, &wanted).unwrap();
    assert_eq!(outcome_of(&found, "e").as_deref(), Some("passed"));
    assert_eq!(
        outcome_of(&found, "a").as_deref(),
        Some("failed"),
        "a newer record for an already held task"
    );
    assert_eq!(seam::walks(&path), 2);
}

#[test]
fn a_deleted_row_is_seen() {
    let (_d, path) = profile();
    let store = fixture_store(&path);
    let memo = Memo::new();
    assert_eq!(outcome(&memo, &store, "a").as_deref(), Some("passed"));
    let other = rusqlite::Connection::open(&path).unwrap();
    other
        .execute(
            "DELETE FROM evidence WHERE type = ?1 AND data_json LIKE '%\"passed\"%' AND task_id = 'a'",
            [EVIDENCE_TYPE],
        )
        .unwrap();
    assert_eq!(outcome(&memo, &store, "a").as_deref(), Some("degraded"));
    other
        .execute("DELETE FROM evidence WHERE task_id = 'a'", [])
        .unwrap();
    assert_eq!(
        outcome(&memo, &store, "a"),
        None,
        "every record of the task deleted"
    );
}

#[test]
fn a_row_rewritten_in_place_is_seen() {
    let (_d, path) = profile();
    let store = fixture_store(&path);
    let memo = Memo::new();
    assert_eq!(outcome(&memo, &store, "c").as_deref(), Some("passed"));
    let other = rusqlite::Connection::open(&path).unwrap();
    other
        .execute(
            "UPDATE evidence SET data_json = replace(data_json, 'passed', 'failed')
             WHERE type = ?1 AND task_id = 'c'",
            [EVIDENCE_TYPE],
        )
        .unwrap();
    assert_eq!(outcome(&memo, &store, "c").as_deref(), Some("failed"));
}

#[test]
fn this_connections_own_write_is_seen() {
    let (_d, path) = profile();
    let store = fixture_store(&path);
    let memo = Memo::new();
    assert_eq!(outcome(&memo, &store, "c").as_deref(), Some("passed"));
    put(&store, "c", "failed");
    assert_eq!(outcome(&memo, &store, "c").as_deref(), Some("failed"));
}

/// The case a stamp of the files' sizes and times cannot see: the log is
/// restarted from its head, a commit rewrites it in place so its length does
/// not move, and its modification time is put back. Only the counter knows.
#[test]
fn a_commit_that_leaves_the_files_looking_unchanged_is_seen() {
    let (_d, path) = profile();
    let store = fixture_store(&path);
    let other = rusqlite::Connection::open(&path).unwrap();
    other
        .query_row("PRAGMA wal_checkpoint(RESTART)", [], |_| Ok(()))
        .unwrap();
    let memo = Memo::new();
    assert_eq!(outcome(&memo, &store, "a").as_deref(), Some("passed"));
    let wal = {
        let mut name = path.clone().into_os_string();
        name.push("-wal");
        std::path::PathBuf::from(name)
    };
    let before = std::fs::metadata(&wal).unwrap();
    other
        .execute(
            "INSERT INTO evidence (type, task_id, data_json) VALUES (?1, 'a', ?2)",
            rusqlite::params![
                EVIDENCE_TYPE,
                r#"{"item_id":"a","state":"settled","outcome":"failed"}"#
            ],
        )
        .unwrap();
    let after = std::fs::metadata(&wal).unwrap();
    assert_eq!(
        before.len(),
        after.len(),
        "the restarted log was rewritten in place, so its length did not move"
    );
    std::fs::OpenOptions::new()
        .write(true)
        .open(&wal)
        .unwrap()
        .set_modified(before.modified().unwrap())
        .unwrap();
    assert_eq!(
        outcome(&memo, &store, "a").as_deref(),
        Some("failed"),
        "a newer record committed by another connection was not seen"
    );
}

#[test]
fn a_failed_read_leaves_no_stale_answer_and_the_next_good_read_is_exact() {
    let (_d, path) = profile();
    let store = fixture_store(&path);
    let memo = Memo::new();
    assert_eq!(outcome(&memo, &store, "a").as_deref(), Some("passed"));
    let other = rusqlite::Connection::open(&path).unwrap();
    // A newer record nobody can parse: the verdict is unknown, not the older pass.
    other
        .execute(
            "INSERT INTO evidence (type, task_id, data_json) VALUES (?1, 'a', 'not json')",
            [EVIDENCE_TYPE],
        )
        .unwrap();
    for _ in 0..2 {
        assert!(memo.latest_many(&store, &ids(&["a"])).is_err());
        assert!(memo.latest(&store, "a").is_err());
        assert_eq!(
            outcome(&memo, &store, "c").as_deref(),
            Some("passed"),
            "another task's record is its own"
        );
    }
    other
        .execute("DELETE FROM evidence WHERE data_json = 'not json'", [])
        .unwrap();
    assert_eq!(outcome(&memo, &store, "a").as_deref(), Some("passed"));
    // A read the database itself refuses.
    other
        .execute_batch("ALTER TABLE evidence RENAME TO evidence_away")
        .unwrap();
    assert!(memo.latest_many(&store, &ids(&["a"])).is_err());
    assert!(
        log_is_free(&path),
        "a failed read left a read transaction open"
    );
    other
        .execute_batch("ALTER TABLE evidence_away RENAME TO evidence")
        .unwrap();
    other
        .execute(
            "INSERT INTO evidence (type, task_id, data_json) VALUES (?1, 'a', '{\"outcome\":\"failed\"}')",
            [EVIDENCE_TYPE],
        )
        .unwrap();
    assert_eq!(outcome(&memo, &store, "a").as_deref(), Some("failed"));
}

#[test]
fn a_read_leaves_the_log_free_to_checkpoint() {
    let (_d, path) = profile();
    let store = fixture_store(&path);
    let memo = Memo::new();
    // An early-exited walk, a complete one and a held answer.
    assert_eq!(outcome(&memo, &store, "b").as_deref(), Some("passed"));
    assert!(log_is_free(&path), "after a walk that stopped early");
    assert_eq!(outcome(&memo, &store, "never"), None);
    assert!(log_is_free(&path), "after a walk of the whole table");
    assert_eq!(outcome(&memo, &store, "a").as_deref(), Some("passed"));
    assert!(log_is_free(&path), "after a held answer");
}

#[test]
fn two_threads_asking_at_once_walk_once() {
    let (_d, path) = profile();
    drop(fixture_store(&path));
    let memo = Memo::new();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                let reader = Store::open_readonly(&path).unwrap();
                let found = memo.latest_many(&reader, &ids(&["a", "b"])).unwrap();
                assert_eq!(outcome_of(&found, "a").as_deref(), Some("passed"));
                assert_eq!(outcome_of(&found, "b").as_deref(), Some("passed"));
            });
        }
    });
    assert_eq!(seam::walks(&path), 1);
}

#[test]
fn a_store_replaced_on_disk_is_seen() {
    let (dir, path) = profile();
    let store = fixture_store(&path);
    let memo = Memo::new();
    assert_eq!(outcome(&memo, &store, "a").as_deref(), Some("passed"));
    drop(store);
    // The old file's log and index belong to it, not to its replacement.
    for suffix in ["-wal", "-shm"] {
        let mut companion = path.clone().into_os_string();
        companion.push(suffix);
        let _ = std::fs::remove_file(companion);
    }
    // The same shape of store with other content, moved over the first.
    let other_path = dir.path().join("replacement.sqlite");
    {
        let other = Store::open(&other_path).unwrap();
        put(&other, "a", "failed");
        put(&other, "z", "passed");
    }
    std::fs::rename(&other_path, &path).unwrap();
    let reopened = Store::open(&path).unwrap();
    let found = memo.latest_many(&reopened, &ids(&["a", "z", "c"])).unwrap();
    assert_eq!(outcome_of(&found, "a").as_deref(), Some("failed"));
    assert_eq!(outcome_of(&found, "z").as_deref(), Some("passed"));
    assert!(!found.contains_key("c"));
}

#[test]
fn a_store_with_no_file_is_read_every_time() {
    let store = Store::open_in_memory().unwrap();
    fixture(&store);
    let memo = Memo::new();
    assert_eq!(outcome(&memo, &store, "a").as_deref(), Some("passed"));
    put(&store, "a", "failed");
    assert_eq!(outcome(&memo, &store, "a").as_deref(), Some("failed"));
    assert_eq!(memo.held_bytes(), 0);
}

#[test]
fn the_per_card_read_is_the_same_reader() {
    let (_d, path) = profile();
    let store = fixture_store(&path);
    let memo = Memo::new();
    assert_eq!(
        memo.latest(&store, "a").unwrap().map(|r| r.outcome),
        Some("passed".to_owned())
    );
    // A card never verified: one walk of the whole table, once.
    assert!(memo.latest(&store, "never").unwrap().is_none());
    assert!(memo.latest(&store, "never").unwrap().is_none());
    assert!(memo.latest(&store, "b").unwrap().is_some());
    assert_eq!(seam::walks(&path), 2);
    put(&store, "a", "failed");
    assert_eq!(
        memo.latest(&store, "a").unwrap().map(|r| r.outcome),
        Some("failed".to_owned())
    );
}

#[test]
fn a_walk_that_does_not_fit_keeps_only_what_was_asked_about() {
    let (_d, path) = profile();
    let store = fixture_store(&path);
    // What one task weighs, held.
    let probe = Memo::new();
    assert_eq!(outcome(&probe, &store, "b").as_deref(), Some("passed"));
    let one = probe.held_bytes();
    assert!(one > 0);
    // Room for two tasks, not for all three.
    let memo = Memo::with_budget(one * 2 + one / 2);
    let walks = seam::walks(&path);
    for _ in 0..3 {
        assert_eq!(outcome(&memo, &store, "never"), None);
    }
    assert!(
        memo.held_bytes() <= one * 2 + one / 2,
        "{} held",
        memo.held_bytes()
    );
    assert_eq!(
        seam::walks(&path) - walks,
        1,
        "the walk kept the one task asked about, and that fits"
    );
    // Asking about all three does not fit: it is read each time, and right.
    let before = seam::walks(&path);
    for _ in 0..3 {
        let found = memo.latest_many(&store, &ids(&["a", "b", "c"])).unwrap();
        assert_eq!(found.len(), 3);
    }
    assert!(memo.held_bytes() <= one * 2 + one / 2);
    assert_eq!(seam::walks(&path) - before, 3);
}

#[test]
fn one_budget_covers_every_store() {
    let (_d1, first) = profile();
    let (_d2, second) = profile();
    let one = fixture_store(&first);
    let two = fixture_store(&second);
    let probe = Memo::new();
    assert_eq!(outcome(&probe, &one, "never"), None);
    let whole = probe.held_bytes();
    // Room for one store's whole answer and a little more, not for two.
    let memo = Memo::with_budget(whole + whole / 2);
    assert_eq!(outcome(&memo, &one, "never"), None);
    assert_eq!(outcome(&memo, &two, "never"), None);
    assert_eq!(outcome(&memo, &one, "never"), None);
    assert_eq!(outcome(&memo, &two, "never"), None);
    assert!(memo.held_bytes() <= whole + whole / 2);
    let (a, b) = (seam::walks(&first), seam::walks(&second));
    // `probe` walked `first` once too.
    assert_eq!(a, 2, "the first store's answer stayed held");
    assert_eq!(b, 2, "the second did not fit and is read each time");
}

#[test]
fn a_memo_that_has_seen_many_stores_holds_within_its_budget() {
    let memo = Memo::new();
    let mut weights = Vec::new();
    for _ in 0..6 {
        let (_d, path) = profile();
        let store = fixture_store(&path);
        assert_eq!(outcome(&memo, &store, "never"), None);
        weights.push(memo.held_bytes());
        drop(store);
    }
    assert!(
        weights.iter().all(|held| *held <= weights[0] * 4),
        "a memo keeps at most four stores: {weights:?}"
    );
}

// ---------------------------------------------------------------------------
// Real-scale measurement. Not a check: it prints what reading verdicts costs on
// a copy or a live store, and it is ignored so no suite runs it.
// ---------------------------------------------------------------------------

/// What one file looked like: size and modification time, or that it is absent.
fn file_state(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let modified = meta
                .modified()
                .ok()
                .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_nanos());
            format!("{} bytes, modified {modified} ns", meta.len())
        }
        Err(_) => "absent".to_owned(),
    }
}

fn store_files(path: &Path) -> Vec<(String, String)> {
    ["", "-wal", "-shm"]
        .iter()
        .map(|suffix| {
            let mut name = path.as_os_str().to_owned();
            name.push(suffix);
            (
                format!("workbench.sqlite{suffix}"),
                file_state(Path::new(&name)),
            )
        })
        .collect()
}

/// The query `latest_many` was before the single pass, with the same decoding,
/// counting the newest records it could not parse instead of failing on them.
fn old_read(store: &Store, item_ids: &[String]) -> (usize, usize) {
    let ids = serde_json::to_string(item_ids).unwrap();
    let mut statement = store
        .connection()
        .prepare(
            "SELECT e.task_id, e.data_json FROM evidence e
             JOIN (SELECT MAX(id) AS id FROM evidence
                   WHERE type = ?1 AND task_id IN (SELECT value FROM json_each(?2))
                   GROUP BY task_id) newest ON newest.id = e.id",
        )
        .unwrap();
    let rows = statement
        .query_map(rusqlite::params![EVIDENCE_TYPE, ids], |row| {
            row.get::<_, String>(1)
        })
        .unwrap();
    let (mut found, mut unparseable) = (0, 0);
    for data in rows {
        match serde_json::from_str::<Record>(&data.unwrap()) {
            Ok(_) => found += 1,
            Err(_) => unparseable += 1,
        }
    }
    (found, unparseable)
}

/// ```text
/// GITPULSE_VERDICT_BENCH_STORE=<abs path of a workbench.sqlite> \
/// cargo test --manifest-path src-tauri/Cargo.toml --lib -- --ignored --exact \
///   workbench::intake::verification_memo_tests::bench_verdict_reads_on_a_real_store \
///   --test-threads=1 --nocapture
/// ```
///
/// The store is opened read-only and nothing in it is written, checkpointed or
/// vacuumed; the size and time of its files are printed before and after. Only
/// counts and timings are printed, never a row. Arms are interleaved
/// (ABCD, DCBA, ...) and the best of five is reported with every repetition.
#[test]
#[ignore = "reads a real store: set GITPULSE_VERDICT_BENCH_STORE to its absolute path"]
fn bench_verdict_reads_on_a_real_store() {
    use std::time::{Duration, Instant};
    let path = std::path::PathBuf::from(
        std::env::var_os("GITPULSE_VERDICT_BENCH_STORE")
            .expect("GITPULSE_VERDICT_BENCH_STORE names the store to measure"),
    );
    assert!(path.is_file(), "{} is not a file", path.display());
    let before = store_files(&path);
    // The host's load is the caller's to read and report (`sysctl -n vm.loadavg`).
    let store = Store::open_readonly(&path).unwrap();
    let (rows, tasks): (i64, i64) = store
        .connection()
        .query_row(
            "SELECT count(*), count(DISTINCT task_id) FROM evidence WHERE type = ?1",
            [EVIDENCE_TYPE],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    println!("verification rows: {rows}, tasks with one: {tasks}");
    let asked: Vec<String> = {
        let mut statement = store
            .connection()
            .prepare(
                "SELECT DISTINCT task_id FROM evidence
                 WHERE type = ?1 AND task_id IS NOT NULL LIMIT 176",
            )
            .unwrap();
        statement
            .query_map([EVIDENCE_TYPE], |row| row.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let mut with_unverified = asked.clone();
    with_unverified.push("bench-a-card-never-verified".to_owned());
    println!(
        "asked: {} tasks that have a record, and the same plus one that has none",
        asked.len()
    );
    let names = [
        "old GROUP BY query",
        "new, cold (fresh memo)",
        "new, warm (store unchanged)",
        "old GROUP BY query again (null pair)",
    ];
    for (label, wanted) in [
        ("every card verified", &asked),
        ("one card never verified", &with_unverified),
    ] {
        let warm = Memo::new();
        let first = warm.latest_many(&store, wanted);
        let mut runs: [Vec<Duration>; 4] = Default::default();
        let mut walks_by_arm = [0usize; 4];
        for rep in 0..5 {
            let order: [usize; 4] = if rep % 2 == 0 {
                [0, 1, 2, 3]
            } else {
                [3, 2, 1, 0]
            };
            for arm in order {
                let walks_before = seam::walks(&path);
                let started = Instant::now();
                match arm {
                    0 | 3 => {
                        std::hint::black_box(old_read(&store, wanted));
                    }
                    1 => {
                        let memo = Memo::new();
                        std::hint::black_box(memo.latest_many(&store, wanted).is_ok());
                    }
                    _ => {
                        std::hint::black_box(warm.latest_many(&store, wanted).is_ok());
                    }
                }
                runs[arm].push(started.elapsed());
                walks_by_arm[arm] += seam::walks(&path) - walks_before;
            }
        }
        println!("--- {label}");
        for (arm, name) in names.iter().enumerate() {
            let best = runs[arm].iter().min().unwrap();
            let each: Vec<String> = runs[arm]
                .iter()
                .map(|d| format!("{:.1}", d.as_secs_f64() * 1000.0))
                .collect();
            println!(
                "{name:<40} best of 5: {:>10.2} ms   all (ms): {}",
                best.as_secs_f64() * 1000.0,
                each.join(", ")
            );
        }
        let (found, unparseable) = old_read(&store, wanted);
        println!(
            "walks of the table, over 5 asks, by arm: cold memo {}, warm memo {}",
            walks_by_arm[1], walks_by_arm[2]
        );
        println!("records the old query found: {found}; newest records it could not parse: {unparseable}");
        println!(
            "new read answered: {}",
            match &first {
                Ok(found) => format!("{} records", found.len()),
                Err(_) => "an error: a newest record could not be parsed".to_owned(),
            }
        );
        println!("bytes held by the warm memo: {}", warm.held_bytes());
    }
    let after = store_files(&path);
    println!("store files before and after:");
    for ((name, was), (_, is)) in before.iter().zip(after.iter()) {
        println!(
            "  {name}: {} ({was} -> {is})",
            if was == is { "unchanged" } else { "DIFFERENT" }
        );
    }
}
