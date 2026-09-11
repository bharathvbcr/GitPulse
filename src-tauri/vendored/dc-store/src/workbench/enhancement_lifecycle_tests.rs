use crate::Store;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

fn store_at(epoch: i64) -> (Store, Arc<AtomicI64>) {
    let clock = Arc::new(AtomicI64::new(epoch));
    let mut store = Store::open_in_memory().unwrap();
    let reader = Arc::clone(&clock);
    store.set_clock(move || reader.load(Ordering::SeqCst));
    (store, clock)
}

fn seed_task(store: &Store) {
    store
        .workbench_request(
            "repositories.put",
            r#"{"id":"r","request_id":"r","expected_revision":0,"name":"Repo","identity_key":"fixture:r"}"#,
        )
        .unwrap();
    store
        .workbench_request(
            "items.put",
            r#"{"id":"t","request_id":"t","expected_revision":0,"title":"Keep E42","description":"Exact notes","repository_ids":["r"],"primary_repository_id":"r"}"#,
        )
        .unwrap();
}

fn create(store: &Store, id: &str, automatic: bool) {
    let automatic = if automatic { "true" } else { "false" };
    store
        .workbench_request(
            "enhancements.create",
            &format!(
                r#"{{"id":"{id}","request_id":"{id}","expected_revision":0,"task_id":"t","source_revision":1,"fields":["title"],"provider":"local","model":"fixture-model","automatic":{automatic}}}"#
            ),
        )
        .unwrap();
}

fn state_of(store: &Store, id: &str) -> String {
    store
        .connection()
        .query_row(
            "SELECT json_extract(body,'$.state') FROM work_enhancements WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap()
}

fn err_code(store: &Store, method: &str, body: &str) -> String {
    match store.workbench_request(method, body) {
        Ok(ok) => panic!("expected {method} to fail, got {ok}"),
        Err(error) => error.code.to_string(),
    }
}

#[test]
fn claim_after_expiry_is_refused() {
    let (store, clock) = store_at(1_700_000_000);
    seed_task(&store);
    create(&store, "e1", false);
    assert_eq!(state_of(&store, "e1"), "pending");
    clock.store(1_700_000_000 + 181, Ordering::SeqCst);
    assert_eq!(
        err_code(
            &store,
            "enhancements.claim",
            r#"{"id":"e1","request_id":"claim","expected_revision":1,"worker_id":"w1"}"#
        ),
        "expired"
    );
    assert_eq!(state_of(&store, "e1"), "pending");
}

#[test]
fn recover_before_lease_is_busy_and_after_lease_interrupts() {
    let (store, clock) = store_at(1_700_000_000);
    seed_task(&store);
    create(&store, "e2", false);
    store
        .workbench_request(
            "enhancements.claim",
            r#"{"id":"e2","request_id":"claim","expected_revision":1,"worker_id":"w1"}"#,
        )
        .unwrap();
    assert_eq!(state_of(&store, "e2"), "running");
    assert_eq!(
        err_code(
            &store,
            "enhancements.recover",
            r#"{"id":"e2","request_id":"recover-early","expected_revision":2,"worker_id":"w1","acknowledge_uncertain":true}"#
        ),
        "busy"
    );
    clock.store(1_700_000_000 + 181, Ordering::SeqCst);
    store
        .workbench_request(
            "enhancements.recover",
            r#"{"id":"e2","request_id":"recover","expected_revision":2,"worker_id":"w1","acknowledge_uncertain":true}"#,
        )
        .unwrap();
    assert_eq!(state_of(&store, "e2"), "interrupted");
    let uncertain: i64 = store
        .connection()
        .query_row(
            "SELECT json_extract(body,'$.outcome_uncertain') FROM work_enhancements WHERE id='e2'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(uncertain, 1);
}

#[test]
fn complete_after_cancel_requested_requires_failure_acknowledgement() {
    let (store, _) = store_at(1_700_000_000);
    seed_task(&store);
    create(&store, "e3", false);
    store
        .workbench_request(
            "enhancements.claim",
            r#"{"id":"e3","request_id":"claim","expected_revision":1,"worker_id":"w1"}"#,
        )
        .unwrap();
    store
        .workbench_request(
            "enhancements.dismiss",
            r#"{"id":"e3","request_id":"cancel","expected_revision":2}"#,
        )
        .unwrap();
    assert_eq!(state_of(&store, "e3"), "cancel_requested");
    assert_eq!(
        err_code(
            &store,
            "enhancements.complete",
            r#"{"id":"e3","request_id":"publish","expected_revision":3,"worker_id":"w1","title":"New","description":null,"rationale":"ok","failure":null}"#
        ),
        "cancel_requested"
    );
    store
        .workbench_request(
            "enhancements.complete",
            r#"{"id":"e3","request_id":"ack","expected_revision":3,"worker_id":"w1","title":null,"description":null,"rationale":null,"failure":"generation cancelled"}"#,
        )
        .unwrap();
    assert_eq!(state_of(&store, "e3"), "cancelled");
}

#[test]
fn interrupted_rows_can_be_dismissed() {
    let (store, clock) = store_at(1_700_000_000);
    seed_task(&store);
    create(&store, "e4", false);
    store
        .workbench_request(
            "enhancements.claim",
            r#"{"id":"e4","request_id":"claim","expected_revision":1,"worker_id":"w1"}"#,
        )
        .unwrap();
    clock.store(1_700_000_000 + 181, Ordering::SeqCst);
    store
        .workbench_request(
            "enhancements.recover",
            r#"{"id":"e4","request_id":"recover","expected_revision":2,"worker_id":"w1","acknowledge_uncertain":true}"#,
        )
        .unwrap();
    assert_eq!(state_of(&store, "e4"), "interrupted");
    store
        .workbench_request(
            "enhancements.dismiss",
            r#"{"id":"e4","request_id":"dismiss","expected_revision":3}"#,
        )
        .unwrap();
    assert_eq!(state_of(&store, "e4"), "dismissed");
}

#[test]
fn automatic_quota_counts_rows_inside_the_hour_window() {
    let (store, clock) = store_at(1_700_000_000);
    seed_task(&store);
    store
        .workbench_request(
            "automation.put",
            r#"{"id":"profile","request_id":"enable","expected_revision":1,"enabled":true,"provider":"local","model":"fixture-model"}"#,
        )
        .unwrap();
    for index in 0..20 {
        let id = format!("auto-{index}");
        create(&store, &id, true);
        store
            .workbench_request(
                "enhancements.complete",
                &format!(
                    r#"{{"id":"{id}","request_id":"fail-{index}","expected_revision":1,"worker_id":null,"title":null,"description":null,"rationale":null,"failure":"fixture failure"}}"#
                ),
            )
            .unwrap();
        if index % 2 == 0 {
            store
                .workbench_request(
                    "enhancements.dismiss",
                    &format!(
                        r#"{{"id":"{id}","request_id":"dismiss-{index}","expected_revision":2}}"#
                    ),
                )
                .unwrap();
        }
    }
    assert_eq!(
        err_code(
            &store,
            "enhancements.create",
            r#"{"id":"auto-over","request_id":"over","expected_revision":0,"task_id":"t","source_revision":1,"fields":["title"],"provider":"local","model":"fixture-model","automatic":true}"#
        ),
        "enhancement_limit"
    );
    clock.store(1_700_000_000 + 3601, Ordering::SeqCst);
    create(&store, "auto-after-window", true);
    assert_eq!(state_of(&store, "auto-after-window"), "pending");
}
