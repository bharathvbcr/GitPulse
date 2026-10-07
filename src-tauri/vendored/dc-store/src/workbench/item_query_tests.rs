use super::{DELETED_SEARCH_HITS, item_query};
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
fn an_existing_profile_gains_the_member_index_when_opened() {
    let path = std::env::temp_dir().join(format!(
        "dc-store-member-index-{}.sqlite",
        std::process::id()
    ));
    remove_store_files(&path);
    let present = |store: &Store| -> bool {
        store
            .connection()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name='work_items_member')",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    let store = Store::open(&path).unwrap();
    store.workbench_request("items.list", "{}").unwrap();
    assert!(present(&store), "a new profile has it");
    // A profile written before the index existed.
    store
        .connection()
        .execute_batch("DROP INDEX work_items_member")
        .unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    store.workbench_request("items.list", "{}").unwrap();
    assert!(present(&store), "reopening an older profile adds it");
    drop(store);
    remove_store_files(&path);
}

#[test]
fn counting_deleted_search_hits_visits_only_deleted_tasks() {
    // The subtraction in an unscoped search's total must cost what the deleted
    // tasks cost, not what the profile does. With none deleted it should not
    // even build the hit set.
    let store = fixture(10_000);
    let mut statement = store.connection().prepare(DELETED_SEARCH_HITS).unwrap();
    let deleted: i64 = statement
        .query_row(["\"evidence\"*"], |r| r.get(0))
        .unwrap();
    assert_eq!(deleted, 0);
    let steps = statement.get_status(StatementStatus::VmStep);
    assert!(
        steps < 100,
        "{steps} operations to find no deleted task among 10,000"
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
    // Unscoped search counts index hits and subtracts deleted ones; a broad
    // query must lose the deleted task too.
    assert_eq!(count(r#"{"query":"evidence"}"#), 119);
    // A page past the end has no row to carry the scoped total.
    assert_eq!(
        count(r#"{"workspace_id":"workspace","cursor":"1:999999:zzz"}"#),
        14
    );
    assert_eq!(
        count(r#"{"repository_id":"common","query":"evidence","cursor":"1:999999:zzz"}"#),
        110
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

/// The Go host benchmark's 100,000-task profile (`BenchmarkWorkbenchProfileStress`
/// in `backend/go_orchestrator/dc/store`), built the same way: 100
/// repositories, ten workspaces of ten repositories each, and every task put
/// through the public API in its own transaction, homed in workspace `i % 10`
/// and linked to repository `i % 100`. The commits matter as much as the rows:
/// one transaction per task leaves the full-text index in many segments and
/// fills the revision, event and request tables, as a real profile does.
///
/// On disk, as the host's is: SQLite's page cache is a few MiB, so a read that
/// touches whole task rows misses it, and an in-memory store flatters it.
fn stress_fixture(tasks: i64) -> (Store, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "dc-store-stress-{}-{tasks}.sqlite",
        std::process::id()
    ));
    remove_store_files(&path);
    let store = Store::open(&path).unwrap();
    for i in 0..100 {
        store.workbench_request("repositories.put", &format!(r#"{{"request_id":"r{i}","id":"r{i}","expected_revision":0,"name":"Repository {i}","identity_key":"benchmark:r{i}"}}"#)).unwrap();
    }
    for i in 0..10 {
        let ids = (0..10)
            .map(|j| format!(r#""r{}""#, i * 10 + j))
            .collect::<Vec<_>>()
            .join(",");
        store.workbench_request("workspaces.put", &format!(r#"{{"request_id":"w{i}","id":"w{i}","expected_revision":0,"name":"Workspace {i}","repository_ids":[{ids}]}}"#)).unwrap();
    }
    let description = "Grounded task evidence. ".repeat(24);
    for i in 0..tasks {
        store.workbench_request("items.put", &format!(r#"{{"request_id":"t{i}-v0","id":"t{i}","expected_revision":0,"title":"Benchmark startup latency task {i}","description":"{description}","repository_ids":["r{r}"],"primary_repository_id":"r{r}","home_workspace_id":"w{w}","position":{i}}}"#, r = i % 100, w = i % 10)).unwrap();
    }
    (store, path)
}

fn remove_store_files(path: &std::path::Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

fn min_and_p95(
    mut samples: Vec<std::time::Duration>,
) -> (std::time::Duration, std::time::Duration) {
    samples.sort();
    (samples[0], samples[(samples.len() - 1) * 95 / 100])
}

/// Where stress-scale broad search spends its time. Run explicitly:
/// `cargo test -p dc-store --release --lib stress_search_profile -- --ignored --nocapture`
#[test]
#[ignore = "profiling harness: 100,000 tasks, prints timings, asserts shape only"]
fn stress_search_profile() {
    let (store, path) = stress_fixture(100_000);
    let conn = store.connection();
    for (name, input, total) in [
        (
            "search",
            r#"{"query":"startup latency","limit":200}"#,
            100_000,
        ),
        (
            "workspace_search",
            r#"{"workspace_id":"w0","query":"startup latency","limit":200}"#,
            19_000,
        ),
        (
            "repository_search",
            r#"{"repository_id":"r0","query":"startup latency","limit":200}"#,
            1_000,
        ),
        ("workspace", r#"{"workspace_id":"w0","limit":200}"#, 19_000),
        ("rare_search", r#"{"query":"99999","limit":200}"#, 1),
    ] {
        let response = store.workbench_request("items.list", input).unwrap();
        let got: i64 = conn
            .query_row("SELECT json_extract(?1,'$.total')", [&response], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(got, total, "{name}");
        let samples = (0..30)
            .map(|_| {
                let started = std::time::Instant::now();
                store.workbench_request("items.list", input).unwrap();
                started.elapsed()
            })
            .collect();
        let (min, p95) = min_and_p95(samples);
        eprintln!("{name:>26}: request min {min:?} p95 {p95:?}");
    }
    // The phases of broad search, each run alone.
    let fts = "\"startup\"* AND \"latency\"*";
    for (name, sql) in [
        ("fts hit set", format!("SELECT count(*) FROM work_items_fts WHERE work_items_fts MATCH '{fts}'")),
        ("fts + live rows", format!("SELECT count(*) FROM work_items_fts CROSS JOIN work_items t ON t.rowid=work_items_fts.rowid WHERE t.deleted=0 AND work_items_fts MATCH '{fts}'")),
        ("ordered scan, IN hit set", format!("SELECT t.rowid FROM work_items t WHERE t.deleted=0 AND t.rowid IN(SELECT rowid FROM work_items_fts WHERE work_items_fts MATCH '{fts}') ORDER BY t.position,t.id LIMIT 201")),
        ("workspace members", "SELECT count(*) FROM (SELECT id FROM work_items WHERE home_workspace_id='w0' AND deleted=0 UNION SELECT ir.item_id FROM work_workspace_repositories wr CROSS JOIN work_item_repositories ir ON ir.repository_id=wr.repository_id WHERE wr.workspace_id='w0')".into()),
        ("single-token fts", "SELECT count(*) FROM work_items_fts WHERE work_items_fts MATCH '\"startup\"*'".into()),
        ("exact-token fts", "SELECT count(*) FROM work_items_fts WHERE work_items_fts MATCH '\"startup\" AND \"latency\"'".into()),
        ("project 201 bodies", "SELECT json_remove(body,'$.description','$.acceptance_criteria','$.logs') FROM work_items ORDER BY rowid LIMIT 201".into()),
    ] {
        let mut statement = conn.prepare(&sql).unwrap();
        let samples = (0..15)
            .map(|_| {
                let started = std::time::Instant::now();
                let mut rows = statement.query([]).unwrap();
                while rows.next().unwrap().is_some() {}
                started.elapsed()
            })
            .collect();
        let (min, p95) = min_and_p95(samples);
        eprintln!("{name:>26}: min {min:?} p95 {p95:?}");
    }
    drop(store);
    remove_store_files(&path);
}
