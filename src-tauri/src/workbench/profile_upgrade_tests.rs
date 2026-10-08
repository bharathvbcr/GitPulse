//! Upgrading a real profile to dc-store schema 11, end to end, through the
//! host GitPulse runs.
//!
//! Ignored by default: it needs a profile that was written by real use, which
//! a fixture cannot stand in for. Point `GITPULSE_PROFILE_COPY` at a COPY of
//! one (`sqlite3 -readonly <live> ".backup '<copy>'"`) and run
//!
//! ```text
//! GITPULSE_PROFILE_COPY=/path/to/copy.sqlite \
//!   cargo test --lib profile_upgrade -- --ignored --nocapture
//! ```
//!
//! The test copies that file again into a temporary directory and works only
//! there, and it refuses the live profile's own path: the upgrade is one-way,
//! so it must never be the thing that migrates a profile other hosts still
//! read at schema 10.
//!
//! Each expectation comes from the pre-upgrade file read directly, not from
//! the store being tested. The completion time in particular is recomputed
//! here from the revision history rather than by re-running the migration's
//! SQL, so a mistake in that SQL cannot agree with itself.
use crate::workbench::{Inner, WorkbenchState};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

const COPY_ENV: &str = "GITPULSE_PROFILE_COPY";
const PAGE: u64 = 100;

struct Before {
    version: i64,
    /// Live (not deleted) tasks: id → (status, revision).
    live: BTreeMap<String, (String, i64)>,
    deleted: BTreeMap<String, i64>,
    /// For each live Done task, when its current Done streak began.
    completed: BTreeMap<String, i64>,
    runs: Vec<String>,
}

fn read_before(path: &Path) -> Before {
    let db = Connection::open(path).unwrap();
    let version = db
        .query_row("SELECT version FROM work_meta WHERE id=1", [], |r| r.get(0))
        .unwrap();
    let mut live = BTreeMap::new();
    let mut deleted = BTreeMap::new();
    let mut rows = db
        .prepare("SELECT id, deleted, body FROM work_items")
        .unwrap();
    for row in rows
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
    {
        let (id, gone, body) = row.unwrap();
        let body: Value = serde_json::from_str(&body).unwrap();
        let revision = body["revision"].as_i64().unwrap();
        if gone == 1 {
            deleted.insert(id, revision);
        } else {
            live.insert(id, (body["status"].as_str().unwrap().to_owned(), revision));
        }
    }
    // Every recorded revision of every task, oldest first.
    let mut history: BTreeMap<String, Vec<(i64, String, i64)>> = BTreeMap::new();
    let mut revs = db
        .prepare("SELECT entity_id, revision, body FROM work_revisions WHERE entity_type='item'")
        .unwrap();
    for row in revs
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
    {
        let (id, revision, body) = row.unwrap();
        let body: Value = serde_json::from_str(&body).unwrap();
        history.entry(id).or_default().push((
            revision,
            body["status"].as_str().unwrap_or_default().to_owned(),
            body["updated_at"].as_i64().unwrap_or_default(),
        ));
    }
    let mut completed = BTreeMap::new();
    for (id, (status, _)) in &live {
        if status != "done" {
            continue;
        }
        let body: String = db
            .query_row("SELECT body FROM work_items WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        let updated_at = serde_json::from_str::<Value>(&body).unwrap()["updated_at"]
            .as_i64()
            .unwrap();
        // Walk back from the newest revision while it is Done; the streak
        // began at the oldest revision reached.
        let mut revisions = history.get(id).cloned().unwrap_or_default();
        revisions.sort_by_key(|(revision, _, _)| *revision);
        let streak = revisions
            .iter()
            .rev()
            .take_while(|(_, status, _)| status == "done")
            .map(|(_, _, at)| *at)
            .min();
        completed.insert(id.clone(), streak.unwrap_or(updated_at));
    }
    let runs = db
        .prepare("SELECT id FROM work_runs")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    Before {
        version,
        live,
        deleted,
        completed,
        runs,
    }
}

/// Every card a listing returns, following its cursor to the end.
fn list_all(host: &WorkbenchState, filter: Value) -> (Vec<Value>, u64) {
    let mut items = Vec::new();
    let mut cursor: Option<String> = None;
    let mut total;
    loop {
        let mut input = filter.clone();
        input["limit"] = json!(PAGE);
        input["query"] = json!("");
        if let Some(cursor) = &cursor {
            input["cursor"] = json!(cursor);
        }
        let page = host.request("items.list", &input.to_string()).unwrap();
        total = page["total"].as_u64().unwrap();
        items.extend(page["items"].as_array().unwrap().iter().cloned());
        match page["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
        assert!(items.len() <= 100_000, "a listing that never ends");
    }
    (items, total)
}

#[test]
#[ignore = "needs GITPULSE_PROFILE_COPY: a copy of a real profile"]
fn a_real_profile_upgrades_to_schema_11_and_keeps_everything_it_held() {
    let source =
        std::env::var_os(COPY_ENV).expect("set GITPULSE_PROFILE_COPY to a copy of a profile");
    let source = Path::new(&source)
        .canonicalize()
        .expect("the profile copy exists");
    if let Ok(live) = crate::workbench::intake::default_profile_path() {
        if let Ok(live) = live.canonicalize() {
            assert_ne!(
                source, live,
                "refusing the live profile: point this at a copy"
            );
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workbench.sqlite");
    std::fs::copy(&source, &path).unwrap();
    let before = read_before(&path);
    assert_eq!(
        before.version, 10,
        "this checks the 10 → 11 upgrade; the copy is at {}",
        before.version
    );

    let host = WorkbenchState(Arc::new(Inner {
        path: Some(path.clone()),
        ..Inner::default()
    }));
    // The first request opens the store, which runs the upgrade.
    let (archived, archived_total) =
        list_all(&host, json!({"archived": true, "order": "completed"}));
    let version: i64 = Connection::open(&path)
        .unwrap()
        .query_row("SELECT version FROM work_meta WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 11);

    // Every Done task, and only those, is in the archive, most recently
    // completed first, with the completion its own history records.
    let done: Vec<&String> = before.completed.keys().collect();
    assert_eq!(archived_total as usize, done.len());
    assert_eq!(archived.len(), done.len());
    let mut last = i64::MAX;
    for card in &archived {
        let id = card["id"].as_str().unwrap();
        let at = card["completed_at"]
            .as_i64()
            .unwrap_or_else(|| panic!("{id} archived with no completion time"));
        assert_eq!(Some(&at), before.completed.get(id), "{id}: completion time");
        assert!(at <= last, "{id}: archive out of completion order");
        last = at;
        assert_eq!(card["status"], "done");
        // Restating what the row meant spends no revision, so a host holding
        // the pre-upgrade revision can still save.
        assert_eq!(
            card["revision"].as_i64(),
            before.live.get(id).map(|(_, r)| *r),
            "{id}: revision moved"
        );
    }

    // The board holds everything else, each task once, none of it archived.
    let mut board = BTreeMap::new();
    for status in ["inbox", "backlog", "ready", "in_progress", "review", "done"] {
        let (cards, total) = list_all(&host, json!({"status": status, "archived": false}));
        assert_eq!(
            total as usize,
            cards.len(),
            "{status}: total and pages disagree"
        );
        for card in cards {
            assert_eq!(card["archived"], false);
            assert_eq!(card["status"], status);
            assert!(
                board
                    .insert(card["id"].as_str().unwrap().to_owned(), ())
                    .is_none(),
                "a card listed twice"
            );
        }
    }
    let open: Vec<&String> = before
        .live
        .iter()
        .filter(|(_, (s, _))| s != "done")
        .map(|(id, _)| id)
        .collect();
    assert_eq!(board.len(), open.len(), "board lost or gained a task");
    assert!(open.iter().all(|id| board.contains_key(*id)));

    // Deleted tasks are listed apart, most recently changed first.
    let (gone, gone_total) = list_all(&host, json!({"deleted": true, "order": "updated"}));
    assert_eq!(gone_total as usize, before.deleted.len());
    assert!(gone
        .windows(2)
        .all(|w| w[0]["updated_at"].as_i64() >= w[1]["updated_at"].as_i64()));

    // Every run recorded before schema 11 still reads, with no model recorded.
    for id in &before.runs {
        let run = host
            .request("runs.get", &json!({"id": id}).to_string())
            .unwrap();
        assert!(
            run["item"].get("model_choice").is_none_or(Value::is_null),
            "{id}"
        );
    }

    // A brief renders for a task on each side.
    for id in [open.first().copied(), done.first().copied()]
        .into_iter()
        .flatten()
    {
        let revision = before.live[id].1;
        host.request(
            "items.brief.get",
            &json!({"id": id, "expected_revision": revision}).to_string(),
        )
        .unwrap_or_else(|error| panic!("{id}: brief: {error:?}"));
    }

    // A write that does not name the new fields keeps them: an archived Done
    // task stays archived with its completion time, an open one stays open.
    for id in [open.first().copied(), done.first().copied()]
        .into_iter()
        .flatten()
    {
        let task = host
            .request("items.get", &json!({"id": id}).to_string())
            .unwrap()["item"]
            .clone();
        let mut write = serde_json::Map::new();
        for key in [
            "title",
            "description",
            "kind",
            "status",
            "priority",
            "labels",
            "acceptance_criteria",
            "repository_ids",
            "primary_repository_id",
            "position",
        ] {
            if !task[key].is_null() {
                write.insert(key.into(), task[key].clone());
            }
        }
        write.insert("id".into(), json!(id));
        write.insert("request_id".into(), json!(format!("upgrade-check-{id}")));
        write.insert("expected_revision".into(), task["revision"].clone());
        let saved = host
            .request("items.put", &Value::Object(write).to_string())
            .unwrap()["item"]
            .clone();
        assert_eq!(
            saved["archived"], task["archived"],
            "{id}: archived changed by a write that did not name it"
        );
        assert_eq!(
            saved["completed_at"], task["completed_at"],
            "{id}: completion time moved"
        );
    }

    // A deleted task comes back with its id.
    if let Some((id, revision)) = before.deleted.iter().next() {
        host.request(
            "items.restore",
            &json!({"id": id, "request_id": "upgrade-restore", "expected_revision": revision})
                .to_string(),
        )
        .unwrap_or_else(|error| panic!("{id}: restore: {error:?}"));
        let back = host
            .request("items.get", &json!({"id": id}).to_string())
            .unwrap();
        assert_eq!(back["item"]["id"], json!(id));
    }

    // Opening the upgraded file again is a no-op, not a second migration.
    drop(host);
    let again = WorkbenchState(Arc::new(Inner {
        path: Some(path.clone()),
        ..Inner::default()
    }));
    let (_, total) = list_all(&again, json!({"archived": true}));
    assert!(total as usize >= done.len());

    eprintln!(
        "upgraded a real profile: {} live tasks ({} Done → archived, {} on the board), {} deleted, {} runs",
        before.live.len(),
        done.len(),
        open.len(),
        before.deleted.len(),
        before.runs.len()
    );
}
