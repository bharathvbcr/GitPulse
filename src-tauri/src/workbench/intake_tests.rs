use super::*;

fn profile() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workbench.sqlite");
    (dir, path)
}

fn untrusted_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    // `git_in` trusts a repository it `init`s; a leading global option keeps
    // this one untrusted, which is the state under test.
    crate::test_support::git_in(dir.path(), &["--no-pager", "init", "-q"]);
    dir
}

fn git_repo() -> tempfile::TempDir {
    crate::test_support::git_repo()
}

fn registered(store: &Store, repo: &tempfile::TempDir) -> Value {
    let local = resolve(repo.path().to_str().unwrap()).unwrap();
    register(store, &local, &fresh_id("repo"), &fresh_id("intake")).unwrap()["repository"].clone()
}

fn task(key: &str, title: &str) -> ExternalTask {
    ExternalTask {
        key: key.into(),
        title: title.into(),
        ..ExternalTask::default()
    }
}

fn items(store: &Store, repository: &Value) -> Vec<Value> {
    let page = query(
        store,
        "items.list",
        &json!({"repository_id": repository["id"], "limit": 200}).to_string(),
    )
    .unwrap();
    page["items"].as_array().unwrap().clone()
}

#[test]
fn item_ids_are_stable_distinct_and_valid_store_ids() {
    let a = item_id("repo-1", "gp-x");
    assert_eq!(a, item_id("repo-1", "gp-x"));
    assert_ne!(
        a,
        item_id("repo-2", "gp-x"),
        "the same key in another repository is another task"
    );
    assert_ne!(a, item_id("repo-1", "gp-y"));
    // The separator keeps (ab, c) and (a, bc) apart.
    assert_ne!(item_id("ab", "c"), item_id("a", "bc"));
    assert!(a.len() <= 128 && a.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
}

#[test]
fn due_dates_are_calendar_checked() {
    let noon = |days: i64| Some(days * 86_400 + 43_200);
    assert_eq!(parse_due(Some("1970-01-01")).unwrap(), noon(0));
    assert_eq!(parse_due(Some("2000-03-01")).unwrap(), noon(11_017));
    assert_eq!(parse_due(Some("2024-02-29")).unwrap(), noon(19_782));
    assert_eq!(parse_due(Some("1775000000")).unwrap(), Some(1_775_000_000));
    for absent in [None, Some(""), Some("none"), Some("None"), Some("  ")] {
        assert_eq!(parse_due(absent).unwrap(), None);
    }
    for bad in [
        "2025-02-29",
        "2026-13-01",
        "2026-00-10",
        "2026-04-31",
        "1969-12-31",
        "next week",
        "2026-4-1",
        "99999999999999999999",
        "-5",
    ] {
        assert!(parse_due(Some(bad)).is_err(), "{bad}");
    }
}

#[test]
fn placement_is_idempotent_and_replace_keeps_board_placement() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let repository = registered(&store, &repo);

    let first = place(&store, &repository, &task("gp-a", "Alpha"), false, 10).unwrap();
    assert_eq!(first.outcome, Outcome::Created);
    let again = place(
        &store,
        &repository,
        &task("gp-a", "Alpha renamed"),
        false,
        20,
    )
    .unwrap();
    assert_eq!(again.outcome, Outcome::AlreadyPresent);
    assert_eq!(items(&store, &repository).len(), 1);

    // Someone drags it to another column position on the board.
    let mut moved = again.item.unwrap();
    let mut write = moved.as_object().unwrap().clone();
    for key in ["revision", "updated_at", "locked_fields"] {
        write.remove(key);
    }
    write.insert("position".into(), json!(999));
    write.insert("expected_revision".into(), moved["revision"].clone());
    write.insert("request_id".into(), json!("move-1"));
    if write["due_at"].is_null() {
        write.remove("due_at");
    }
    moved = query(&store, "items.put", &Value::Object(write).to_string()).unwrap()["item"].clone();
    assert_eq!(moved["position"], 999);

    let replaced = place(
        &store,
        &repository,
        &task("gp-a", "Alpha renamed"),
        true,
        20,
    )
    .unwrap();
    assert_eq!(replaced.outcome, Outcome::Updated);
    let item = replaced.item.unwrap();
    assert_eq!(item["title"], "Alpha renamed");
    assert_eq!(
        item["position"], 999,
        "replace changes content, never where the card sits"
    );
    let same = place(
        &store,
        &repository,
        &task("gp-a", "Alpha renamed"),
        true,
        20,
    )
    .unwrap();
    assert_eq!(
        same.outcome,
        Outcome::Unchanged,
        "an identical replace writes nothing"
    );
}

#[test]
fn a_task_deleted_on_the_board_stays_deleted() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let repository = registered(&store, &repo);
    let placed = place(&store, &repository, &task("gp-gone", "Gone"), false, 1).unwrap();
    let revision = placed.item.unwrap()["revision"].as_i64().unwrap();
    query(
        &store,
        "items.delete",
        &json!({"id": placed.item_id, "expected_revision": revision, "request_id": "del"})
            .to_string(),
    )
    .unwrap();
    // This pins the store's wording that `place` relies on.
    let again = place(&store, &repository, &task("gp-gone", "Gone"), false, 1).unwrap();
    assert_eq!(again.outcome, Outcome::DeletedOnBoard);
    assert!(items(&store, &repository).is_empty());
}

#[test]
fn an_exported_board_brief_imported_back_finds_its_original() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let repository = registered(&store, &repo);
    let original = place(&store, &repository, &task("gp-orig", "Original"), false, 1).unwrap();
    // Copy saved brief writes `Task: <item id>`; the key is the board's own id.
    let back = place(
        &store,
        &repository,
        &task(&original.item_id, "Original"),
        false,
        2,
    )
    .unwrap();
    assert_eq!(back.outcome, Outcome::AlreadyPresent);
    assert_eq!(back.item_id, original.item_id);
    assert_eq!(items(&store, &repository).len(), 1);

    // With replace, the board id is the task to update — never a twin of it.
    let replaced = place(
        &store,
        &repository,
        &task(&original.item_id, "Original, with the merged work"),
        true,
        3,
    )
    .unwrap();
    assert_eq!(replaced.outcome, Outcome::Updated);
    assert_eq!(replaced.item_id, original.item_id);
    assert_eq!(
        replaced.item.unwrap()["title"],
        "Original, with the merged work"
    );
    assert_eq!(items(&store, &repository).len(), 1);
}

#[test]
fn title_words_keep_what_a_title_is_about() {
    let words = super::title_words("Fix: the DevMap resolver drops calls into other crates");
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    assert_eq!(
        words,
        vec!["call", "crate", "devmap", "drop", "other", "resolver"]
    );
    // Too short, generic, or only digits-and-noise: nothing to match on.
    assert!(super::title_words("Task 12").is_empty());
    assert!(super::title_words("Add support for it").is_empty());
    // "status" and "class" keep their s; "process" is not "proces".
    let kept = super::title_words("status class process");
    assert!(kept.contains("status") && kept.contains("class") && kept.contains("process"));
}

#[test]
fn invalid_fields_are_named_and_bounds_include_folded_sections() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let repository = registered(&store, &repo);
    let cases: Vec<(ExternalTask, &str)> = vec![
        (
            ExternalTask {
                status: Some("someday".into()),
                ..task("k", "T")
            },
            "unknown status",
        ),
        (
            ExternalTask {
                severity: Some("apocalyptic".into()),
                ..task("k", "T")
            },
            "unknown severity",
        ),
        (
            ExternalTask {
                priority: Some(7),
                ..task("k", "T")
            },
            "out of range",
        ),
        (
            ExternalTask {
                due: Some("soon".into()),
                ..task("k", "T")
            },
            "YYYY-MM-DD",
        ),
        (task("k", "two\nlines"), "control character"),
        (task("../k", "T"), "task key"),
        (task("k", &"x".repeat(301)), "301 characters"),
        (
            ExternalTask {
                labels: vec!["l".repeat(129)],
                ..task("k", "T")
            },
            "labels",
        ),
        (
            ExternalTask {
                description: "d".repeat(file_tasks::MAX_TASK_DESCRIPTION - 10),
                planned_files: vec!["src/lib.rs".into()],
                ..task("k", "T")
            },
            "description with planned files",
        ),
    ];
    for (bad, needle) in cases {
        let error = place(&store, &repository, &bad, false, 1)
            .err()
            .expect(needle);
        assert_eq!(error.code, "invalid_input");
        assert!(
            error.message.contains(needle),
            "{needle}: {}",
            error.message
        );
    }
    assert!(items(&store, &repository).is_empty());
}

#[test]
fn add_refuses_an_untrusted_repository_and_a_bad_request_registers_nothing() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let untrusted = untrusted_repo();
    let error = add_task(
        &store,
        untrusted.path().to_str().unwrap(),
        task("gp-a", "A"),
        false,
        &[],
    )
    .unwrap_err();
    assert_eq!(error.code, "untrusted_repository");

    let trusted = git_repo();
    let error = add_task(
        &store,
        trusted.path().to_str().unwrap(),
        task("gp-a", ""),
        false,
        &[],
    )
    .unwrap_err();
    assert_eq!(error.code, "invalid_input");
    let repos: Value =
        serde_json::from_str(&store.workbench_request("repositories.list", "{}").unwrap()).unwrap();
    assert_eq!(
        repos["total"], 0,
        "neither refusal left a repository behind"
    );
}

#[test]
fn a_linked_worktree_files_under_the_same_repository() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = untrusted_repo();
    crate::test_support::git_in(
        repo.path(),
        &["commit", "-q", "--allow-empty", "-m", "init"],
    );
    crate::test_support::git_in(repo.path(), &["worktree", "add", "-q", "wt"]);
    crate::test_support::trust_repo(repo.path());
    let worktree = repo.path().join("wt");
    let from_worktree = add_task(
        &store,
        worktree.to_str().unwrap(),
        task("gp-wt", "From a worktree"),
        false,
        &[],
    )
    .unwrap();
    let main = registered(&store, &repo);
    assert_eq!(from_worktree["repository"]["id"], main["id"]);
    assert_eq!(items(&store, &main).len(), 1);
}

/// Many writers on one profile, each with its own connection — the shape of
/// several agents filing at once while the board is open.
#[test]
fn concurrent_writers_never_duplicate_or_lose_a_task() {
    let (_dir, path) = profile();
    let repo = git_repo();
    let repo_path = repo.path().to_str().unwrap().to_string();
    let threads = 16;
    let keys = 25;
    let handles: Vec<_> = (0..threads)
        .map(|worker| {
            let path = path.clone();
            let repo_path = repo_path.clone();
            std::thread::spawn(move || {
                let store = Store::open(&path).unwrap();
                let mut outcomes = Vec::new();
                for i in 0..keys {
                    // Every worker races on every key, in a different order.
                    let k = (i * 7 + worker) % keys;
                    match add_task(
                        &store,
                        &repo_path,
                        task(&format!("gp-{k}"), &format!("Task {k}")),
                        false,
                        &[],
                    ) {
                        Ok(v) => outcomes.push(v["outcome"].as_str().unwrap().to_string()),
                        Err(e) => outcomes.push(e.code),
                    }
                }
                outcomes
            })
        })
        .collect();
    let outcomes: Vec<String> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();
    let created = outcomes.iter().filter(|o| *o == "created").count();
    let refused = outcomes.iter().filter(|o| *o == "already_exists").count();
    assert_eq!(created, keys, "{outcomes:?}");
    assert_eq!(
        refused,
        threads * keys - keys,
        "every other attempt is a refusal, never an error: {outcomes:?}"
    );
    let store = Store::open(&path).unwrap();
    let repos: Value =
        serde_json::from_str(&store.workbench_request("repositories.list", "{}").unwrap()).unwrap();
    assert_eq!(
        repos["total"], 1,
        "racing first registrations converge on one repository"
    );
    assert_eq!(items(&store, &repos["items"][0]).len(), keys);
}

#[test]
fn import_places_briefs_in_priority_order_and_accounts_for_every_file() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let tasks = repo.path().join("tasks");
    std::fs::create_dir(&tasks).unwrap();
    for (name, priority) in [("c-low", 3), ("a-urgent", 0), ("b-high", 1)] {
        std::fs::write(
            tasks.join(format!("{name}.md")),
            format!("---\ntitle: {name}\npriority: {priority}\n---\n"),
        )
        .unwrap();
    }
    std::fs::write(tasks.join("z-bad.md"), "no title").unwrap();
    let report = import_briefs(&store, repo.path().to_str().unwrap(), None, false).unwrap();
    assert!(!report.ok);
    assert_eq!(
        (report.found, report.read, report.created, report.invalid),
        (4, 4, 3, 1)
    );
    let repository = registered(&store, &repo);
    let mut board = items(&store, &repository);
    board.sort_by_key(|i| i["position"].as_i64().unwrap());
    let order: Vec<&str> = board.iter().map(|i| i["title"].as_str().unwrap()).collect();
    assert_eq!(order, ["a-urgent", "b-high", "c-low"]);

    // A file edit is not applied without replace, and is with it.
    std::fs::write(
        tasks.join("b-high.md"),
        "---\ntitle: b-high edited\npriority: 1\n---\n",
    )
    .unwrap();
    std::fs::remove_file(tasks.join("z-bad.md")).unwrap();
    let report = import_briefs(&store, repo.path().to_str().unwrap(), None, false).unwrap();
    assert!(report.ok);
    assert_eq!((report.created, report.already_present), (0, 3));
    let report = import_briefs(&store, repo.path().to_str().unwrap(), None, true).unwrap();
    assert_eq!((report.updated, report.unchanged), (1, 2));
}

#[test]
fn importing_an_empty_or_missing_folder_registers_nothing() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let report = import_briefs(&store, repo.path().to_str().unwrap(), None, false).unwrap();
    assert!(!report.directory_exists && report.ok && report.repository.is_null());
    let repos: Value =
        serde_json::from_str(&store.workbench_request("repositories.list", "{}").unwrap()).unwrap();
    assert_eq!(repos["total"], 0);
}

/// The board opening a repository while an agent files into it: both see no
/// record and both register. Released together by a barrier so the window
/// between the read and the write is actually hit, round after round.
#[test]
fn racing_first_registrations_converge_on_one_record() {
    let (_dir, path) = profile();
    Store::open(&path).unwrap();
    for _round in 0..12 {
        let repo = git_repo();
        let repo_path = repo.path().to_str().unwrap().to_string();
        let threads = 8;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(threads));
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let (path, repo_path, barrier) = (path.clone(), repo_path.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let store = Store::open(&path).unwrap();
                    let local = resolve(&repo_path).unwrap();
                    barrier.wait();
                    register(&store, &local, &fresh_id("repo"), &fresh_id("intake"))
                        .map(|v| v["repository"]["id"].clone())
                })
            })
            .collect();
        let ids: Vec<Value> = handles
            .into_iter()
            .map(|h| h.join().unwrap().expect("every racer gets the record"))
            .collect();
        assert!(ids.windows(2).all(|w| w[0] == w[1]), "{ids:?}");
    }
}

/// An item already sitting at this repository's derived id but linked to
/// another repository is not this task, whatever made it collide.
#[test]
fn an_item_at_the_derived_id_that_belongs_elsewhere_is_never_replaced() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let (mine, theirs) = (git_repo(), git_repo());
    let (mine, theirs) = (registered(&store, &mine), registered(&store, &theirs));
    let squatted = item_id(mine["id"].as_str().unwrap(), "gp-k");
    let input = json!({
        "id": squatted, "request_id": "squat", "expected_revision": 0, "title": "Not yours",
        "repository_ids": [theirs["id"]], "primary_repository_id": theirs["id"], "position": 0,
    });
    query(&store, "items.put", &input.to_string()).unwrap();
    for replace in [false, true] {
        let error = place(&store, &mine, &task("gp-k", "Mine"), replace, 1)
            .err()
            .expect("refused");
        assert_eq!(error.code, "key_collision");
    }
    let kept = query(&store, "items.get", &json!({"id": squatted}).to_string()).unwrap();
    assert_eq!(kept["item"]["title"], "Not yours");
}
