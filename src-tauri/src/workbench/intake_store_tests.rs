//! What the host does with the store's newer semantics: a caller's request id
//! reaches the store, a delete that was told to override a running attempt
//! reaches the store with that, and the related-task scan and the creating
//! write are one decision.

use super::tests::{filed, git_repo, items, live, prepare_run, profile, registered, task};
use super::*;

/// The method the store recorded a request id under, if it recorded it.
fn recorded_method(store: &Store, request_id: &str) -> Option<String> {
    store
        .connection()
        .query_row(
            "SELECT method FROM work_requests WHERE id = ?1",
            [request_id],
            |row| row.get(0),
        )
        .ok()
}

#[test]
fn a_malformed_request_id_is_refused_before_anything_is_written() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let repo_path = repo.path().to_str().unwrap();
    let id = filed(&store, &repo, "gp-rid", "Card for request ids");
    let before = live(&store, &id).unwrap();
    for bad in ["", "has space", "semi;colon", &"x".repeat(121)] {
        let error = complete_task(&store, repo_path, &id, "review", None, Some("s"), Some(bad))
            .expect_err(bad);
        assert_eq!(error.code, "invalid_input", "{bad:?}: {}", error.message);
        let error =
            delete_task(&store, repo_path, &id, None, "r", false, Some(bad)).expect_err(bad);
        assert_eq!(error.code, "invalid_input", "{bad:?}: {}", error.message);
    }
    assert_eq!(live(&store, &id).unwrap(), before, "nothing was written");
}

#[test]
fn a_callers_request_id_is_the_stores_request_id() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let repo_path = repo.path().to_str().unwrap();
    let id = filed(&store, &repo, "gp-rid-ok", "Card moved with a request id");

    let moved = complete_task(
        &store,
        repo_path,
        &id,
        "review",
        None,
        Some("ready for a look"),
        Some("agent-retry-key-1"),
    )
    .unwrap();
    assert_eq!(moved["outcome"], "updated", "{moved}");
    assert_eq!(
        recorded_method(&store, "agent-retry-key-1").as_deref(),
        Some("items.put")
    );
    // The store keys its receipts by it: the same key for different work is
    // refused rather than answered with the first call's result.
    let other = filed(&store, &repo, "gp-rid-other", "A different card");
    let reused = complete_task(
        &store,
        repo_path,
        &other,
        "review",
        None,
        None,
        Some("agent-retry-key-1"),
    )
    .expect_err("a request id belongs to one request");
    assert_eq!(reused.code, "idempotency_conflict", "{}", reused.message);

    let doomed = filed(
        &store,
        &repo,
        "gp-rid-del",
        "Card deleted with a request id",
    );
    let deleted = delete_task(
        &store,
        repo_path,
        &doomed,
        None,
        "not needed",
        false,
        Some("agent-delete-key-1"),
    )
    .unwrap();
    assert_eq!(deleted["outcome"], "deleted", "{deleted}");
    assert_eq!(
        recorded_method(&store, "agent-delete-key-1").as_deref(),
        Some("items.delete")
    );
    // The reason's own write is keyed too, so a repeat of the whole call
    // replays both rather than recording the reason twice.
    assert_eq!(
        recorded_method(&store, "agent-delete-key-1-log").as_deref(),
        Some("items.put")
    );
}

/// The store now refuses to delete a task a run may still be working on, inside
/// the delete's own transaction. A caller that said `even_if_running` must be
/// the one that gets past it: its flag has to reach the store.
#[test]
fn even_if_running_reaches_the_store_and_its_absence_is_refused_there_too() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let repo_path = repo.path().to_str().unwrap();
    let id = filed(&store, &repo, "gp-live", "Card an agent is working on");
    prepare_run(&store, &repo, &id, "run-live");

    // The store's refusal, reached directly: no host check stands in front.
    let item = live(&store, &id).unwrap();
    let revision = item["revision"].as_i64().unwrap();
    let raw = json!({"id": id, "request_id": "raw-delete", "expected_revision": revision});
    let refused =
        query(&store, "items.delete", &raw.to_string()).expect_err("refused in the store");
    assert_eq!(refused.code, "task_in_use", "{}", refused.message);

    let forced = delete_task(&store, repo_path, &id, None, "the person asked", true, None).unwrap();
    assert_eq!(forced["outcome"], "deleted", "{forced}");
    assert!(live(&store, &id).is_none());
}

/// Another writer files a task between the scan and the write. The creating
/// write carries the sequence the scan started from, the store refuses it, and
/// one rescan sees the new card and answers with it instead of filing a twin.
#[test]
fn a_board_that_changes_between_the_scan_and_the_write_is_scanned_again() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let repo_path = repo.path().to_str().unwrap();
    let repository = registered(&store, &repo);
    filed(
        &store,
        &repo,
        "gp-seed",
        "Seed card so the repository has a board",
    );

    let mut attempts = 0;
    let mut racer = || {
        attempts += 1;
        if attempts == 1 {
            // The race: a person (or another agent) files the same work.
            let twin = task("gp-twin", "Rebuild the exporter pipeline stages");
            place(&store, &repository, &twin, false, 1).unwrap();
        }
    };
    let error = add_task_racing(
        &store,
        repo_path,
        &task("gp-new", "Rebuild the exporter pipeline stages"),
        false,
        &[],
        &mut racer,
    )
    .expect_err("the rescan finds the card that landed in between");
    assert_eq!(error.code, "related_tasks_exist", "{}", error.message);
    // The rescan refused before it reached the write, so the seam ran once.
    assert_eq!(
        attempts, 1,
        "the second scan stopped at the card that landed"
    );
    assert!(
        items(&store, &repository).iter().all(|item| item["title"]
            != "Rebuild the exporter pipeline stages"
            || item["id"] == item_id(repository["id"].as_str().unwrap(), "gp-twin")),
        "the new card was not written beside its twin"
    );
}

#[test]
fn a_board_that_keeps_changing_is_reported_after_one_retry() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let repo_path = repo.path().to_str().unwrap();
    let repository = registered(&store, &repo);
    filed(
        &store,
        &repo,
        "gp-seed",
        "Seed card so the repository has a board",
    );

    let mut attempts = 0;
    let mut racer = || {
        attempts += 1;
        // Unrelated work, so the scan finds nothing to refuse on.
        let key = format!("gp-noise-{attempts}");
        place(
            &store,
            &repository,
            &task(&key, &format!("Zzz{attempts} qqq{attempts}")),
            false,
            1,
        )
        .unwrap();
    };
    let error = add_task_racing(
        &store,
        repo_path,
        &task("gp-new", "Wholly unrelated card"),
        false,
        &[],
        &mut racer,
    )
    .expect_err("the board moved under both attempts");
    assert_eq!(error.code, "board_changed", "{}", error.message);
    assert_eq!(attempts, 2);
    assert!(items(&store, &repository)
        .iter()
        .all(|item| item["title"] != "Wholly unrelated card"));
}

#[test]
fn a_quiet_board_files_the_task_with_the_scan_it_made() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let repo_path = repo.path().to_str().unwrap();
    let repository = registered(&store, &repo);
    filed(
        &store,
        &repo,
        "gp-seed",
        "Seed card so the repository has a board",
    );
    let created = add_task(
        &store,
        repo_path,
        task("gp-quiet", "Wholly unrelated card"),
        false,
        &[],
    )
    .unwrap();
    assert_eq!(created["outcome"], "created", "{created}");
    assert_eq!(created["related_check"]["scan_complete"], true);
    assert_eq!(items(&store, &repository).len(), 2);
}

/// `place` on its own (an import, a proposal) names no scan, so it is never
/// held to one.
#[test]
fn a_placement_with_no_scan_is_not_held_to_one() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let repository = registered(&store, &repo);
    place(&store, &repository, &task("gp-a", "Alpha"), false, 1).unwrap();
    let Err(stale) = place_scanned(
        &store,
        &repository,
        &task("gp-b", "Beta"),
        false,
        2,
        Some(0),
    ) else {
        panic!("a scan from before the first write must be stale");
    };
    assert_eq!(stale.code, "board_changed", "{}", stale.message);
    assert_eq!(
        place(&store, &repository, &task("gp-b", "Beta"), false, 2)
            .unwrap()
            .outcome,
        Outcome::Created
    );
}

/// Registers `count` synthetic repositories straight into the table, so a
/// registry larger than any page walk the host once made exists without a
/// minute of writes.
fn bulk_repositories(store: &Store, count: i64) {
    store
        .connection()
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM n WHERE i+1<?1)
             INSERT INTO work_repositories(id,revision,body)
             SELECT 'bulk-'||i,1,json_object('id','bulk-'||i,'revision',1,'name','Bulk '||i,'identity_key','local:/bulk-'||i||'/.git','remote_url',NULL,'updated_at',1) FROM n",
            [count],
        )
        .unwrap();
}

#[test]
fn a_repository_is_found_by_identity_in_a_registry_past_the_old_page_walk() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let first = registered(&store, &repo);
    bulk_repositories(&store, 10_050);
    let found = find(&store, "local:/bulk-10049/.git")
        .unwrap()
        .expect("registered");
    assert_eq!(found["id"], "bulk-10049");
    assert!(find(&store, "local:/never-registered/.git")
        .unwrap()
        .is_none());
    // A real registration against a registry this size still finds before it creates.
    assert_eq!(
        registered(&store, &repo)["id"],
        first["id"],
        "one record, not two"
    );
}

#[test]
fn the_last_revision_of_a_task_with_a_very_long_history_is_one_read() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let id = filed(&store, &repo, "gp-long-history", "Card with a long history");
    store
        .connection()
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 1000 UNION ALL SELECT i+1 FROM n WHERE i<11000)
             INSERT INTO work_revisions(entity_type,entity_id,revision,body)
             SELECT 'item',?1,i,json_object('id',?1,'revision',i,'title','revision '||i) FROM n",
            [&id],
        )
        .unwrap();
    let last = last_revision(&store, &id).unwrap().expect("history");
    assert_eq!(last["revision"], 11_000);
    assert!(last_revision(&store, "never-filed").unwrap().is_none());
}
