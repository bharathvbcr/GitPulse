use super::tests::{git_repo, profile, registered, task};
use super::*;

/// Files `key` under `parent` (a board id) and returns its board id.
fn placed(store: &Store, repository: &Value, key: &str, parent: Option<&str>) -> String {
    let mut external = task(key, &format!("Task {key}"));
    external.parent = parent.map(str::to_owned);
    place(store, repository, &external, false, now_millis())
        .unwrap()
        .item_id
}

/// Moves a task to Done the way the store records it, bypassing the board's
/// verification gate (which is not what is under test here).
fn set_done(store: &Store, id: &str) {
    store
        .connection()
        .execute(
            "UPDATE work_items SET body=json_set(body,'$.status','done') WHERE id=?1",
            [id],
        )
        .unwrap();
}

fn lineage(host: &crate::workbench::WorkbenchState, ids: &[&str]) -> Result<Value, WorkbenchError> {
    host.request("items.lineage", &json!({ "ids": ids }).to_string())
}

#[test]
fn the_host_reads_each_cards_parent_and_how_many_of_its_subtasks_are_done() {
    let (dir, path) = profile();
    let host = crate::workbench::WorkbenchState::for_profile(&path);
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let repository = registered(&store, &repo);
    let parent = placed(&store, &repository, "gp-parent", None);
    let finished = placed(&store, &repository, "gp-finished", Some(&parent));
    let open = placed(&store, &repository, "gp-open", Some(&parent));
    let alone = placed(&store, &repository, "gp-alone", None);
    set_done(&store, &finished);

    let out = lineage(&host, &[&parent, &finished, &open, &alone]).unwrap();
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(
        out["items"][&parent],
        json!({"parent_item_id": null, "children": {"total": 2, "done": 1}}),
        "{out}"
    );
    assert_eq!(
        out["items"][&finished],
        json!({"parent_item_id": parent, "children": {"total": 0, "done": 0}}),
        "{out}"
    );
    assert_eq!(
        out["items"][&open]["parent_item_id"],
        json!(parent),
        "{out}"
    );
    assert_eq!(
        out["items"][&alone],
        json!({"parent_item_id": null, "children": {"total": 0, "done": 0}}),
        "a card in no tree is read as having none, not left out: {out}"
    );
    drop(dir);
}

#[test]
fn a_deleted_subtask_is_not_counted_and_a_deleted_parent_is_not_named() {
    let (dir, path) = profile();
    let host = crate::workbench::WorkbenchState::for_profile(&path);
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let repository = registered(&store, &repo);
    let parent = placed(&store, &repository, "gp-parent", None);
    let gone = placed(&store, &repository, "gp-gone", Some(&parent));
    let kept = placed(&store, &repository, "gp-kept", Some(&parent));
    store
        .connection()
        .execute("UPDATE work_items SET deleted=1 WHERE id=?1", [&gone])
        .unwrap();

    let out = lineage(&host, &[&parent]).unwrap();
    assert_eq!(out["items"][&parent]["children"]["total"], 1, "{out}");

    store
        .connection()
        .execute("UPDATE work_items SET deleted=1 WHERE id=?1", [&parent])
        .unwrap();
    let out = lineage(&host, &[&kept, &parent]).unwrap();
    assert_eq!(
        out["items"][&kept]["parent_item_id"],
        Value::Null,
        "a card cannot link to a task the board no longer has: {out}"
    );
    assert!(
        out["items"].get(&parent).is_none(),
        "a deleted card is not read, so the board shows it as unknown: {out}"
    );
    drop(dir);
}

#[test]
fn an_id_the_store_does_not_hold_is_left_out_not_read_as_empty() {
    let (dir, path) = profile();
    let host = crate::workbench::WorkbenchState::for_profile(&path);
    let store = Store::open(&path).unwrap();
    registered(&store, &git_repo());
    let out = lineage(&host, &["no-such-card"]).unwrap();
    assert_eq!(out["items"], json!({}), "{out}");
    drop(dir);
}

#[test]
fn the_read_is_bounded_and_refuses_what_it_cannot_answer_whole() {
    let (dir, path) = profile();
    let host = crate::workbench::WorkbenchState::for_profile(&path);
    let store = Store::open(&path).unwrap();
    registered(&store, &git_repo());
    let many: Vec<String> = (0..257).map(|n| format!("id-{n}")).collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    assert_eq!(lineage(&host, &refs).unwrap_err().code, "invalid_input");
    let long = "x".repeat(129);
    assert_eq!(lineage(&host, &[&long]).unwrap_err().code, "invalid_input");
    assert_eq!(lineage(&host, &[""]).unwrap_err().code, "invalid_input");
    for bad in [r#"{}"#, r#"{"ids":"a"}"#, r#"{"ids":[1]}"#, "not json"] {
        assert_eq!(
            host.request("items.lineage", bad).unwrap_err().code,
            "invalid_input",
            "{bad}"
        );
    }
    // Exactly the cap is accepted, and a repeated id is one id.
    let exactly: Vec<String> = (0..256).map(|n| format!("id-{n}")).collect();
    let refs: Vec<&str> = exactly.iter().map(String::as_str).collect();
    assert!(lineage(&host, &refs).is_ok());
    assert!(lineage(&host, &["a", "a"]).is_ok());
    drop(dir);
}

/// The cap is documented as at most 256 cards after repeats are folded, so a
/// request that names a card several times is judged by the cards it names.
#[test]
fn the_cap_counts_distinct_cards_not_names() {
    let (dir, path) = profile();
    let host = crate::workbench::WorkbenchState::for_profile(&path);
    let store = Store::open(&path).unwrap();
    registered(&store, &git_repo());
    // 300 names of 200 cards.
    let repeated: Vec<String> = (0..300).map(|n| format!("id-{}", n % 200)).collect();
    let refs: Vec<&str> = repeated.iter().map(String::as_str).collect();
    assert!(
        lineage(&host, &refs).is_ok(),
        "300 names of 200 distinct cards is within the 256-card cap"
    );
    // 257 distinct cards, however they repeat, are past it.
    let over: Vec<String> = (0..600).map(|n| format!("id-{}", n % 257)).collect();
    let refs: Vec<&str> = over.iter().map(String::as_str).collect();
    assert_eq!(lineage(&host, &refs).unwrap_err().code, "invalid_input");
    // A request too long to walk is refused unread.
    let flood: Vec<&str> = std::iter::repeat("a").take(4097).collect();
    assert_eq!(lineage(&host, &flood).unwrap_err().code, "invalid_input");
    drop(dir);
}

/// The board reads lineage on every refresh. That read commits nothing: a
/// commit there would itself be announced as a change and read again.
#[test]
fn the_lineage_read_leaves_the_profile_unchanged() {
    let (dir, path) = profile();
    let host = crate::workbench::WorkbenchState::for_profile(&path);
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let repository = registered(&store, &repo);
    let parent = placed(&store, &repository, "gp-parent", None);
    let observer = rusqlite::Connection::open(&path).unwrap();
    let version = || -> i64 {
        observer
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .unwrap()
    };
    let before = version();
    let out = lineage(&host, &[&parent]).unwrap();
    assert_eq!(out["items"][&parent]["children"]["total"], 0, "{out}");
    assert_eq!(
        version(),
        before,
        "the lineage read committed to the profile"
    );
    drop(dir);
}
