//! What `latest_many` may and may not answer from memory.
//!
//! Every test counts work, never time: `seam::walks` is how many times the
//! evidence table of one store file was walked, and `seam::advance_clock` moves
//! the memo's clock instead of sleeping. A write is "recent" until the clock has
//! been advanced past the memo's racy window, so each test says which it means.

use super::tests::profile;
use crate::workbench::verification_gate::memo::{seam, FileIdentity};
use crate::workbench::verification_gate::{self as gate, Record, EVIDENCE_TYPE};
use dc_store::Store;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

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

fn outcome(store: &Store, task: &str) -> Option<String> {
    outcome_of(&gate::latest_many(store, &ids(&[task])).unwrap(), task)
}

/// Long enough for a write to stop being recent.
fn settle() {
    seam::advance_clock(Duration::from_secs(3));
}

/// A store with several records per task, a task with none, rows of another
/// kind newer than a task's verdicts, and a row with no task.
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

/// A store nobody has written to for longer than the memo's racy window.
fn quiet_store(path: &Path) -> Store {
    let store = Store::open(path).unwrap();
    fixture(&store);
    settle();
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

#[test]
fn the_single_pass_answers_what_the_old_query_answered() {
    let (_d, path) = profile();
    let store = Store::open(&path).unwrap();
    fixture(&store);
    for asked in [
        ids(&["a", "b", "c", "d"]),
        ids(&["a"]),
        ids(&["d"]),
        ids(&["c", "nobody", "a"]),
        ids(&["a", "a", "b"]),
        ids(&["nobody"]),
        ids(&[""]),
    ] {
        assert_eq!(
            gate::latest_many(&store, &asked).unwrap(),
            sql_oracle(&store, &asked),
            "{asked:?}"
        );
    }
    assert!(gate::latest_many(&store, &[]).unwrap().is_empty());
}

#[test]
fn the_oracle_itself_sees_the_newest_row_past_newer_rows_of_other_kinds() {
    let (_d, path) = profile();
    let store = Store::open(&path).unwrap();
    fixture(&store);
    let found = sql_oracle(&store, &ids(&["a", "b", "c", "d"]));
    assert_eq!(outcome_of(&found, "a").as_deref(), Some("passed"));
    assert_eq!(outcome_of(&found, "b").as_deref(), Some("passed"));
    assert!(!found.contains_key("d"));
}

#[test]
fn an_unchanged_store_is_walked_once_however_many_sweeps_ask() {
    let (_d, path) = profile();
    let store = quiet_store(&path);
    let wanted = ids(&["a", "b", "c", "d"]);
    for _ in 0..10 {
        let found = gate::latest_many(&store, &wanted).unwrap();
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
    drop(quiet_store(&path));
    // Closing the last connection checkpoints the log: a write, so settle again.
    settle();
    // A different task set on a different, read-only connection each time: the
    // way the board asks.
    for asked in [
        ids(&["a"]),
        ids(&["b", "c"]),
        ids(&["never"]),
        ids(&["a", "d"]),
    ] {
        let reader = Store::open_readonly(&path).unwrap();
        assert_eq!(
            gate::latest_many(&reader, &asked).unwrap(),
            sql_oracle(&reader, &asked)
        );
    }
    assert_eq!(seam::walks(&path), 1);
}

#[test]
fn a_task_never_asked_about_before_costs_no_walk() {
    let (_d, path) = profile();
    let store = quiet_store(&path);
    assert_eq!(outcome(&store, "a").as_deref(), Some("passed"));
    assert_eq!(outcome(&store, "c").as_deref(), Some("passed"));
    assert_eq!(outcome(&store, "unseen"), None);
    assert_eq!(seam::walks(&path), 1);
}

#[test]
fn an_empty_list_reads_nothing_even_on_a_cold_store() {
    let (_d, path) = profile();
    let store = quiet_store(&path);
    assert!(gate::latest_many(&store, &[]).unwrap().is_empty());
    assert_eq!(seam::walks(&path), 0);
}

#[test]
fn a_row_appended_by_a_second_connection_is_seen() {
    let (_d, path) = profile();
    let store = quiet_store(&path);
    let wanted = ids(&["a", "e"]);
    assert!(!gate::latest_many(&store, &wanted)
        .unwrap()
        .contains_key("e"));
    let other = Store::open(&path).unwrap();
    put(&other, "e", "passed");
    put(&other, "a", "failed");
    settle();
    let found = gate::latest_many(&store, &wanted).unwrap();
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
    let store = quiet_store(&path);
    let wanted = ids(&["a"]);
    assert_eq!(outcome(&store, "a").as_deref(), Some("passed"));
    let other = rusqlite::Connection::open(&path).unwrap();
    other
        .execute(
            "DELETE FROM evidence WHERE type = ?1 AND data_json LIKE '%\"passed\"%' AND task_id = 'a'",
            [EVIDENCE_TYPE],
        )
        .unwrap();
    settle();
    assert_eq!(
        outcome_of(&gate::latest_many(&store, &wanted).unwrap(), "a").as_deref(),
        Some("degraded")
    );
    other
        .execute("DELETE FROM evidence WHERE task_id = 'a'", [])
        .unwrap();
    settle();
    assert_eq!(
        outcome(&store, "a"),
        None,
        "every record of the task deleted"
    );
}

#[test]
fn a_row_rewritten_in_place_is_seen() {
    let (_d, path) = profile();
    let store = quiet_store(&path);
    assert_eq!(outcome(&store, "c").as_deref(), Some("passed"));
    let other = rusqlite::Connection::open(&path).unwrap();
    other
        .execute(
            "UPDATE evidence SET data_json = replace(data_json, 'passed', 'failed')
             WHERE type = ?1 AND task_id = 'c'",
            [EVIDENCE_TYPE],
        )
        .unwrap();
    settle();
    assert_eq!(outcome(&store, "c").as_deref(), Some("failed"));
}

#[test]
fn this_connections_own_write_is_seen() {
    let (_d, path) = profile();
    let store = quiet_store(&path);
    assert_eq!(outcome(&store, "c").as_deref(), Some("passed"));
    put(&store, "c", "failed");
    settle();
    assert_eq!(outcome(&store, "c").as_deref(), Some("failed"));
}

#[test]
fn a_write_in_the_last_two_seconds_is_never_served_from_memory() {
    let (_d, path) = profile();
    let store = Store::open(&path).unwrap();
    fixture(&store);
    // No settle: the files were modified a moment ago.
    for _ in 0..3 {
        assert_eq!(outcome(&store, "a").as_deref(), Some("passed"));
    }
    assert_eq!(seam::walks(&path), 3, "a store being written to was held");
    settle();
    assert_eq!(outcome(&store, "a").as_deref(), Some("passed"));
    assert_eq!(outcome(&store, "a").as_deref(), Some("passed"));
    assert_eq!(seam::walks(&path), 4, "held once the writes were old");
}

#[test]
fn a_held_answer_expires() {
    let (_d, path) = profile();
    let store = quiet_store(&path);
    assert_eq!(outcome(&store, "a").as_deref(), Some("passed"));
    seam::advance_clock(Duration::from_secs(59));
    assert_eq!(outcome(&store, "a").as_deref(), Some("passed"));
    assert_eq!(seam::walks(&path), 1, "still inside the bound");
    seam::advance_clock(Duration::from_secs(2));
    assert_eq!(outcome(&store, "a").as_deref(), Some("passed"));
    assert_eq!(seam::walks(&path), 2, "past the bound");
}

#[test]
fn a_failed_read_leaves_no_stale_answer_and_the_next_good_read_is_exact() {
    let (_d, path) = profile();
    let store = quiet_store(&path);
    assert_eq!(outcome(&store, "a").as_deref(), Some("passed"));
    let other = rusqlite::Connection::open(&path).unwrap();
    // A newer record nobody can parse: the verdict is unknown, not the older pass.
    other
        .execute(
            "INSERT INTO evidence (type, task_id, data_json) VALUES (?1, 'a', 'not json')",
            [EVIDENCE_TYPE],
        )
        .unwrap();
    settle();
    for _ in 0..2 {
        assert!(gate::latest_many(&store, &ids(&["a"])).is_err());
        assert!(gate::latest(&store, "a").is_err());
        assert_eq!(
            outcome(&store, "c").as_deref(),
            Some("passed"),
            "another task's record is its own"
        );
    }
    other
        .execute("DELETE FROM evidence WHERE data_json = 'not json'", [])
        .unwrap();
    settle();
    assert_eq!(outcome(&store, "a").as_deref(), Some("passed"));
    // A read the database itself refuses.
    other
        .execute_batch("ALTER TABLE evidence RENAME TO evidence_away")
        .unwrap();
    settle();
    assert!(gate::latest_many(&store, &ids(&["a"])).is_err());
    other
        .execute_batch("ALTER TABLE evidence_away RENAME TO evidence")
        .unwrap();
    other
        .execute(
            "INSERT INTO evidence (type, task_id, data_json) VALUES (?1, 'a', '{\"outcome\":\"failed\"}')",
            [EVIDENCE_TYPE],
        )
        .unwrap();
    settle();
    assert_eq!(outcome(&store, "a").as_deref(), Some("failed"));
}

#[test]
fn two_threads_asking_at_once_walk_once() {
    let (_d, path) = profile();
    drop(quiet_store(&path));
    // Closing the last connection checkpoints the log: a write, so settle again.
    settle();
    let skew = seam::skew();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                seam::set_skew(skew);
                let reader = Store::open_readonly(&path).unwrap();
                let found = gate::latest_many(&reader, &ids(&["a", "b"])).unwrap();
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
    let store = quiet_store(&path);
    assert_eq!(outcome(&store, "a").as_deref(), Some("passed"));
    drop(store);
    let before = FileIdentity::of(&path).unwrap();
    // The same shape of store with other content, moved over the first.
    let other_path = dir.path().join("replacement.sqlite");
    {
        let other = Store::open(&other_path).unwrap();
        put(&other, "a", "failed");
        put(&other, "z", "passed");
    }
    std::fs::rename(&other_path, &path).unwrap();
    assert_ne!(FileIdentity::of(&path).unwrap(), before);
    settle();
    let reopened = Store::open(&path).unwrap();
    let found = gate::latest_many(&reopened, &ids(&["a", "z", "c"])).unwrap();
    assert_eq!(outcome_of(&found, "a").as_deref(), Some("failed"));
    assert_eq!(outcome_of(&found, "z").as_deref(), Some("passed"));
    assert!(!found.contains_key("c"));
}

#[test]
fn a_store_with_no_file_is_read_every_time() {
    let store = Store::open_in_memory().unwrap();
    fixture(&store);
    settle();
    assert_eq!(outcome(&store, "a").as_deref(), Some("passed"));
    put(&store, "a", "failed");
    assert_eq!(outcome(&store, "a").as_deref(), Some("failed"));
}

#[test]
fn the_per_card_read_uses_a_current_answer_and_otherwise_asks_the_store() {
    let (_d, path) = profile();
    let store = quiet_store(&path);
    assert_eq!(outcome(&store, "a").as_deref(), Some("passed"));
    let walks = seam::walks(&path);
    // A card never verified: answered from memory, not by walking for it.
    assert!(gate::latest(&store, "never").unwrap().is_none());
    assert_eq!(
        gate::latest(&store, "a").unwrap().map(|r| r.outcome),
        Some("passed".to_owned())
    );
    assert_eq!(seam::walks(&path), walks);
    // Once the held answer is no longer current the store is asked, and it
    // answers for itself.
    put(&store, "a", "failed");
    assert_eq!(
        gate::latest(&store, "a").unwrap().map(|r| r.outcome),
        Some("failed".to_owned())
    );
}

#[test]
fn a_store_too_large_to_hold_is_read_in_full_and_still_exact() {
    let (_d, path) = profile();
    let store = quiet_store(&path);
    let wanted: HashSet<&str> = ["a", "c"].into_iter().collect();
    let (newest, complete) = seam::walk_with_cap(&store, &wanted, 1).unwrap();
    assert!(!complete, "a cap of one byte cannot hold everything");
    assert!(newest.contains_key("a") && newest.contains_key("c"));
    assert!(
        !newest.contains_key("b"),
        "unwanted tasks are dropped past the cap"
    );
    assert!(newest["a"].contains("passed"));
    let (all, complete) = seam::walk_with_cap(&store, &wanted, usize::MAX).unwrap();
    assert!(complete);
    assert!(all.contains_key("b"));
}
