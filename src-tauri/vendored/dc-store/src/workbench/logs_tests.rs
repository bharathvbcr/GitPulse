use crate::Store;

fn seed(store: &Store) {
    store
        .workbench_request(
            "repositories.put",
            r#"{"id":"r","request_id":"r","expected_revision":0,"name":"Repo","identity_key":"fixture:r"}"#,
        )
        .unwrap();
}

fn extract(store: &Store, json: &str, path: &str) -> Option<String> {
    store
        .connection()
        .query_row("SELECT json_extract(?1, ?2)", [json, path], |row| {
            row.get::<_, Option<String>>(0)
        })
        .unwrap()
}

fn err_code(store: &Store, method: &str, body: &str) -> String {
    match store.workbench_request(method, body) {
        Ok(ok) => panic!("expected {method} to fail, got {ok}"),
        Err(error) => error.code.to_string(),
    }
}

#[test]
fn logs_round_trip_on_put_and_brief_and_are_stripped_from_cards() {
    let store = Store::open_in_memory().unwrap();
    seed(&store);
    store
        .workbench_request(
            "items.put",
            r#"{"id":"t","request_id":"t","expected_revision":0,"title":"Keep E42","description":"Exact notes","repository_ids":["r"],"primary_repository_id":"r","logs":"error: E42\n    at src/main.rs:12\n```` inner"}"#,
        )
        .unwrap();
    let got = store
        .workbench_request("items.get", r#"{"id":"t"}"#)
        .unwrap();
    assert_eq!(
        extract(&store, &got, "$.item.logs").as_deref(),
        Some("error: E42\n    at src/main.rs:12\n```` inner")
    );
    let listed = store
        .workbench_request("items.list", r#"{"status":"inbox","limit":30}"#)
        .unwrap();
    assert!(extract(&store, &listed, "$.items[0].logs").is_none());
    assert!(extract(&store, &listed, "$.items[0].description").is_none());
    let brief = store
        .workbench_request("items.brief.get", r#"{"id":"t","expected_revision":1}"#)
        .unwrap();
    let markdown = extract(&store, &brief, "$.item.markdown").unwrap();
    assert!(markdown.contains("## Raw logs"), "{markdown}");
    assert!(markdown.contains("error: E42"), "{markdown}");
    assert!(markdown.contains("src/main.rs:12"), "{markdown}");
    assert!(markdown.contains("`````\n"), "{markdown}");
}

#[test]
fn omitted_logs_are_preserved_and_empty_logs_clear() {
    let store = Store::open_in_memory().unwrap();
    seed(&store);
    store
        .workbench_request(
            "items.put",
            r#"{"id":"t","request_id":"t","expected_revision":0,"title":"Keep E42","repository_ids":["r"],"primary_repository_id":"r","logs":"error: E42"}"#,
        )
        .unwrap();
    store
        .workbench_request(
            "items.put",
            r#"{"id":"t","request_id":"edit","expected_revision":1,"title":"Keep E42 revised","repository_ids":["r"],"primary_repository_id":"r"}"#,
        )
        .unwrap();
    let after_omit = store
        .workbench_request("items.get", r#"{"id":"t"}"#)
        .unwrap();
    assert_eq!(
        extract(&store, &after_omit, "$.item.logs").as_deref(),
        Some("error: E42")
    );
    store
        .workbench_request(
            "items.put",
            r#"{"id":"t","request_id":"clear","expected_revision":2,"title":"Keep E42 revised","repository_ids":["r"],"primary_repository_id":"r","logs":""}"#,
        )
        .unwrap();
    let after_clear = store
        .workbench_request("items.get", r#"{"id":"t"}"#)
        .unwrap();
    assert_eq!(
        extract(&store, &after_clear, "$.item.logs").as_deref(),
        Some("")
    );
}

#[test]
fn logs_refuse_nul_unknown_type_and_oversize() {
    let store = Store::open_in_memory().unwrap();
    seed(&store);
    assert_eq!(
        err_code(
            &store,
            "items.put",
            "{\"id\":\"t\",\"request_id\":\"t\",\"expected_revision\":0,\"title\":\"Keep\",\"repository_ids\":[\"r\"],\"primary_repository_id\":\"r\",\"logs\":\"a\\u0000b\"}",
        ),
        "invalid_input"
    );
    assert_eq!(
        err_code(
            &store,
            "items.put",
            r#"{"id":"t","request_id":"t","expected_revision":0,"title":"Keep","repository_ids":["r"],"primary_repository_id":"r","logs":12}"#,
        ),
        "invalid_input"
    );
    let huge = "x".repeat(256 * 1024 + 1);
    assert_eq!(
        err_code(
            &store,
            "items.put",
            &format!(
                r#"{{"id":"t","request_id":"t","expected_revision":0,"title":"Keep","repository_ids":["r"],"primary_repository_id":"r","logs":"{huge}"}}"#
            ),
        ),
        "invalid_input"
    );
}

#[test]
fn enhancement_source_does_not_carry_logs() {
    let store = Store::open_in_memory().unwrap();
    seed(&store);
    store
        .workbench_request(
            "items.put",
            r#"{"id":"t","request_id":"t","expected_revision":0,"title":"Keep E42","description":"Exact notes","repository_ids":["r"],"primary_repository_id":"r","logs":"error: E42\n    at src/main.rs:12"}"#,
        )
        .unwrap();
    store
        .workbench_request(
            "enhancements.create",
            r#"{"id":"e","request_id":"e","expected_revision":0,"task_id":"t","source_revision":1,"fields":["title"],"provider":"local","model":"fixture-model","automatic":false}"#,
        )
        .unwrap();
    let source: String = store
        .connection()
        .query_row(
            "SELECT json_extract(body,'$.source') FROM work_enhancements WHERE id='e'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!source.contains("error: E42"), "{source}");
    assert!(source.contains("Keep E42"), "{source}");
    let got = store
        .workbench_request("items.get", r#"{"id":"t"}"#)
        .unwrap();
    assert_eq!(
        extract(&store, &got, "$.item.logs").as_deref(),
        Some("error: E42\n    at src/main.rs:12")
    );
}
