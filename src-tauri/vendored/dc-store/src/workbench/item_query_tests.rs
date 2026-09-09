use super::item_query;
use crate::Store;
use rusqlite::{StatementStatus, params, params_from_iter};

fn fixture(size: i64) -> Store {
    let store = Store::open_in_memory().unwrap();
    for id in ["common", "rare"] {
        store.workbench_request("repositories.put", &format!(r#"{{"id":"{id}","request_id":"{id}","expected_revision":0,"name":"{id}","identity_key":"fixture:{id}"}}"#)).unwrap();
    }
    store.workbench_request("workspaces.put", r#"{"id":"workspace","request_id":"workspace","expected_revision":0,"name":"Sparse workspace","repository_ids":["rare"]}"#).unwrap();
    store.workbench_request("items.put", r#"{"id":"template","request_id":"template","expected_revision":0,"title":"Common task","description":"Saved text","status":"backlog","repository_ids":["common"],"primary_repository_id":"common","position":0}"#).unwrap();
    // This is a query-complexity fixture, not a mutation benchmark. Use the
    // actual schema, indexes, FTS triggers and a canonical saved record shape.
    let body: String = store
        .connection()
        .query_row(
            "SELECT body FROM work_items WHERE id='template'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let tx = store.connection().unchecked_transaction().unwrap();
    store.connection().execute(
        "WITH RECURSIVE ids(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM ids WHERE n<?1)
         INSERT INTO work_items(id,revision,body,home_workspace_id,primary_repository_id)
         SELECT printf('t%06d',n),1,json_set(?2,'$.id',printf('t%06d',n),'$.title',CASE WHEN n<=10 THEN 'Rareword evidence' ELSE 'Common evidence' END,'$.status',CASE WHEN n<=10 THEN 'review' ELSE 'backlog' END,'$.position',n,'$.home_workspace_id',CASE WHEN n<=15 THEN 'workspace' ELSE NULL END,'$.repository_ids',json_array(CASE WHEN n<=10 THEN 'rare' ELSE 'common' END),'$.primary_repository_id',CASE WHEN n<=10 THEN 'rare' ELSE 'common' END),CASE WHEN n<=15 THEN 'workspace' ELSE NULL END,CASE WHEN n<=10 THEN 'rare' ELSE 'common' END FROM ids",
        params![size,body],
    ).unwrap();
    store.connection().execute("INSERT INTO work_item_repositories(item_id,repository_id,position) SELECT id,primary_repository_id,0 FROM work_items WHERE id<>'template'", []).unwrap();
    tx.commit().unwrap();
    store
}

#[test]
fn sparse_queries_do_not_visit_unrelated_profile_rows() {
    let store = fixture(10_000);
    let mut excessive = Vec::new();
    for (name, workspace, repo, status, search, expected) in [
        ("status", None, None, Some("review"), None, 10),
        ("repository", None, Some("rare"), None, None, 10),
        ("workspace", Some("workspace"), None, None, None, 15),
        ("search", None, None, None, Some("\"Rareword\"*"), 10),
        (
            "scoped common search",
            None,
            Some("rare"),
            None,
            Some("\"evidence\"*"),
            10,
        ),
        (
            "workspace common search",
            Some("workspace"),
            None,
            None,
            Some("\"evidence\"*"),
            15,
        ),
    ] {
        let (query, values) = item_query(workspace, repo, status, search, false);
        let mut statement = store
            .connection()
            .prepare(&format!("SELECT count(*) {query}"))
            .unwrap();
        let actual: i64 = statement
            .query_row(params_from_iter(values.iter()), |row| row.get(0))
            .unwrap();
        assert_eq!(actual, expected, "{name} count changed");
        let steps = statement.get_status(StatementStatus::VmStep);
        println!("{name}: {steps} SQLite operations for {expected} matches");
        // Scoped broad search builds one FTS hit set. Those index entries are
        // relevant work even when only a few repository members match. The
        // previous 5,000-step limit here preferred repeated virtual-table
        // callbacks whose substantial cost this counter cannot observe.
        let budget = if search.is_some() && (workspace.is_some() || repo.is_some()) {
            5_000 + 10 * 10_000
        } else {
            5_000
        };
        if steps >= budget {
            excessive.push(format!("{name}: {steps} operations for {expected} matches"));
        }
    }
    assert!(
        excessive.is_empty(),
        "unrelated profile rows dominate the queries: {excessive:?}"
    );
}

#[test]
fn broad_scoped_search_does_not_repeat_expensive_full_text_evaluation() {
    let store = fixture(10_000);
    let started = std::time::Instant::now();
    let response = store
        .workbench_request(
            "items.list",
            r#"{"repository_id":"common","query":"evidence","limit":7}"#,
        )
        .unwrap();
    let elapsed = started.elapsed();
    let count: i64 = store
        .connection()
        .query_row("SELECT json_extract(?1,'$.total')", [response], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 9_990);
    // VM steps omit work inside FTS callbacks. This generous gross-regression
    // bound catches repeated MATCH evaluation; the explicit host benchmark
    // measures p95 against the much tighter product latency target.
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "scoped search took {elapsed:?}"
    );
}

#[test]
fn all_scope_status_and_search_combinations_match_an_independent_reference() {
    // Exercise both small indexed hit sets and global search sets larger than
    // the maximum page, including status and cursor filtering on either path.
    let store = fixture(240);
    let scopes = [
        None,
        Some(("workspace_id", "workspace")),
        Some(("repository_id", "rare")),
        Some(("repository_id", "common")),
    ];
    for scope in scopes {
        for status in [None, Some("review"), Some("backlog"), Some("done")] {
            for search in [None, Some("Rareword"), Some("evidence"), Some("absent")] {
                let selected: Vec<String> = (1..=240)
                    .filter(|n| {
                        let in_scope = match scope {
                            Some(("workspace_id", _)) => *n <= 15,
                            Some((_, "rare")) => *n <= 10,
                            Some((_, "common")) => *n > 10,
                            _ => true,
                        };
                        let in_status = match status {
                            Some("review") => *n <= 10,
                            Some("backlog") => *n > 10,
                            Some(_) => false,
                            None => true,
                        };
                        let in_search = match search {
                            Some("Rareword") => *n <= 10,
                            Some("absent") => false,
                            _ => true,
                        };
                        in_scope && in_status && in_search
                    })
                    .map(|n| format!("t{n:06}"))
                    .collect();
                // The template is outside this oracle; every page starts after
                // its position, while total still includes it when applicable.
                let template_matches = scope.is_none_or(|(_, id)| id == "common")
                    && status.is_none_or(|s| s == "backlog")
                    && search.is_none();
                let mut after = "1:0:template".to_string();
                let mut found = Vec::new();
                loop {
                    let mut fields = format!(r#""limit":7,"cursor":"{after}""#);
                    if let Some((key, value)) = scope {
                        fields.push_str(&format!(r#", "{key}":"{value}""#));
                    }
                    if let Some(value) = status {
                        fields.push_str(&format!(r#", "status":"{value}""#));
                    }
                    if let Some(value) = search {
                        fields.push_str(&format!(r#", "query":"{value}""#));
                    }
                    let response = store
                        .workbench_request("items.list", &format!("{{{fields}}}"))
                        .unwrap();
                    let total: i64 = store
                        .connection()
                        .query_row("SELECT json_extract(?1,'$.total')", [&response], |r| {
                            r.get(0)
                        })
                        .unwrap();
                    assert_eq!(
                        total,
                        i64::try_from(selected.len() + usize::from(template_matches)).unwrap(),
                        "{fields}"
                    );
                    let mut rows = store
                        .connection()
                        .prepare("SELECT json_extract(value,'$.id') FROM json_each(?1,'$.items')")
                        .unwrap();
                    found.extend(
                        rows.query_map([&response], |r| r.get::<_, String>(0))
                            .unwrap()
                            .map(|r| r.unwrap()),
                    );
                    let next: Option<String> = store
                        .connection()
                        .query_row(
                            "SELECT json_extract(?1,'$.next_cursor')",
                            [&response],
                            |r| r.get(0),
                        )
                        .unwrap();
                    match next {
                        Some(next) => {
                            assert_ne!(next, after);
                            after = next;
                        }
                        None => break,
                    }
                }
                assert_eq!(found, selected, "{scope:?} {status:?} {search:?}");
            }
        }
    }
}

#[test]
fn scoped_search_tracks_link_changes_group_membership_deletion_and_renamed_text() {
    let store = fixture(120);
    let count = |input: &str| -> i64 {
        let response = store.workbench_request("items.list", input).unwrap();
        store
            .connection()
            .query_row("SELECT json_extract(?1,'$.total')", [response], |row| {
                row.get(0)
            })
            .unwrap()
    };
    let mutate = |method: &str, input: &str| {
        store.workbench_request(method, input).unwrap();
    };
    assert_eq!(count(r#"{"workspace_id":"workspace"}"#), 15);
    mutate(
        "items.put",
        r#"{"id":"t000020","request_id":"link-change","expected_revision":1,"title":"Renamed raretoken","description":"Changed evidence","status":"review","repository_ids":["rare","common"],"primary_repository_id":"rare","position":1}"#,
    );
    assert_eq!(count(r#"{"workspace_id":"workspace"}"#), 16);
    assert_eq!(count(r#"{"repository_id":"rare"}"#), 11);
    assert_eq!(count(r#"{"repository_id":"common"}"#), 111);
    assert_eq!(
        count(r#"{"workspace_id":"workspace","query":"raretoken","status":"review"}"#),
        1
    );
    assert_eq!(
        count(r#"{"repository_id":"common","query":"raretoken"}"#),
        1
    );
    mutate(
        "workspaces.put",
        r#"{"id":"workspace","request_id":"include-both","expected_revision":1,"name":"Both repositories","repository_ids":["rare","common"]}"#,
    );
    assert_eq!(
        count(r#"{"workspace_id":"workspace"}"#),
        121,
        "multiple repo links duplicated a task"
    );
    mutate(
        "workspaces.put",
        r#"{"id":"workspace","request_id":"home-only","expected_revision":2,"name":"Home tasks","repository_ids":[]}"#,
    );
    assert_eq!(
        count(r#"{"workspace_id":"workspace"}"#),
        15,
        "removing membership changed home tasks"
    );
    assert_eq!(
        count(r#"{"workspace_id":"workspace","query":"raretoken"}"#),
        0
    );
    mutate(
        "items.delete",
        r#"{"id":"t000005","request_id":"delete-task","expected_revision":1}"#,
    );
    assert_eq!(
        count(r#"{"query":"Rareword"}"#),
        9,
        "FTS included a deleted task"
    );
    assert_eq!(
        count(r#"{"workspace_id":"workspace","query":"Rareword"}"#),
        9
    );
    assert_eq!(count(r#"{"repository_id":"rare","query":"Rareword"}"#), 9);
    mutate(
        "workspaces.delete",
        r#"{"id":"workspace","request_id":"delete-group","expected_revision":3}"#,
    );
    assert_eq!(
        store
            .workbench_request("items.list", r#"{"workspace_id":"workspace"}"#)
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(count("{}"), 120, "deleting a group removed its tasks");
}

#[test]
fn search_only_projects_bodies_in_the_selected_page() {
    let store = fixture(120);
    // Reverse task positions so early FTS candidates are later discarded by
    // the top-N sorter. A body projection tripwire detects unnecessary work
    // without a timing assertion or a second query implementation. The view
    // preserves the real table's stored filter/order columns and indexes.
    store.connection().execute(
        "UPDATE work_items SET body=json_set(body,'$.position',121-position) WHERE id<>'template'",
        [],
    ).unwrap();
    store
        .connection()
        .execute_batch(
            "CREATE TEMP VIEW work_items AS SELECT rowid,id,revision,
         CASE WHEN id='t000001' THEN json('outside-page body was read') ELSE body END AS body,
         deleted,home_workspace_id,primary_repository_id,title,description,status,position
         FROM main.work_items;",
        )
        .unwrap();
    assert!(
        store
            .workbench_request("items.list", r#"{"query":"evidence","limit":200}"#)
            .is_err(),
        "the projection tripwire was not exercised when its row was selected"
    );
    let response = store
        .workbench_request("items.list", r#"{"query":"evidence","limit":7}"#)
        .expect("search projected a body outside its page and lookahead row");
    let (total, rows, first, last): (i64, i64, String, String) = store.connection().query_row(
        "SELECT json_extract(?1,'$.total'),json_array_length(?1,'$.items'),json_extract(?1,'$.items[0].id'),json_extract(?1,'$.items[6].id')",
        [response], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
    ).unwrap();
    assert_eq!(
        (total, rows, first.as_str(), last.as_str()),
        (120, 7, "t000120", "t000114")
    );
}
