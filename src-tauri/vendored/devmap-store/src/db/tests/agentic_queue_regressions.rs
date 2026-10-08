use super::*;

fn enqueue_at(store: &Store, path: &str, time: f64) {
    let conn = lock_conn(&store.conn).unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    Store::upsert_pending(&tx, std::iter::once(path), time).unwrap();
    tx.commit().unwrap();
}

#[test]
fn equal_clock_ticks_cannot_acknowledge_a_new_edit() {
    let store = Store::open_in_memory().unwrap();
    enqueue_at(&store, "main.py", 100.0);
    let old = store.claim_pending_batch(1).unwrap();
    enqueue_at(&store, "main.py", 100.0);
    assert_eq!(store.clear_claimed_pending_paths(&old).unwrap(), 0);
    assert_eq!(store.get_pending_paths().unwrap(), vec!["main.py"]);
}

#[test]
fn a_build_cannot_discard_a_newer_head_event() {
    let store = Store::open_in_memory().unwrap();
    let path = "\0devmap:git-head-changed";
    let through = store.pending_watermark().unwrap();
    enqueue_at(&store, path, 101.0);
    let cleared = store
        .clear_pending_superseded(PendingSupersede::WholeTreeThrough(&through))
        .unwrap();
    assert!(
        cleared.is_empty(),
        "a later checkout was never read: {cleared:?}"
    );
    assert_eq!(store.get_pending_paths().unwrap(), vec![path]);
}

#[test]
fn repair_cannot_delete_an_event_that_arrived_during_inspection() {
    let root = std::env::temp_dir();
    let store = Store::open_in_memory().unwrap();
    store
        .enqueue_pending_paths(&["agentic-missing-file.py".into()])
        .unwrap();
    let result = store
        .reconcile_pending_paths_with(&root, &|| {
            store
                .enqueue_pending_paths(&["agentic-missing-file.py".into()])
                .unwrap();
        })
        .unwrap();
    assert_eq!(
        store.get_pending_paths().unwrap(),
        vec!["agentic-missing-file.py"]
    );
    assert!(result.dropped.is_empty());
}

#[test]
fn backwards_clock_and_delete_reinsert_do_not_reuse_a_claim() {
    let store = Store::open_in_memory().unwrap();
    enqueue_at(&store, "a.py", 100.0);
    let old = store.claim_pending_batch(1).unwrap();
    store.clear_claimed_pending_paths(&old).unwrap();
    enqueue_at(&store, "a.py", 10.0);
    assert_eq!(store.clear_claimed_pending_paths(&old).unwrap(), 0);
    store.bump_pending_attempts(&old).unwrap();
    assert_eq!(store.pending_attempts("a.py").unwrap(), Some(0));
}

#[test]
fn a_failed_old_attempt_cannot_quarantine_a_repaired_edit() {
    let store = Store::open_in_memory().unwrap();
    enqueue_at(&store, "a.py", 100.0);
    let old = store.claim_pending_batch(1).unwrap();
    enqueue_at(&store, "a.py", 100.0);
    for _ in 0..MAX_PENDING_ATTEMPTS {
        store.bump_pending_attempts(&old).unwrap();
    }
    assert_eq!(store.pending_attempts("a.py").unwrap(), Some(0));
}

#[test]
fn acknowledgements_cannot_cross_store_boundaries() {
    let a = Store::open_in_memory().unwrap();
    let b = Store::open_in_memory().unwrap();
    for store in [&a, &b] {
        enqueue_at(store, "a.py", 100.0);
    }
    let claims = a.claim_pending_batch(1).unwrap();
    let through = a.pending_watermark().unwrap();
    assert!(b.clear_claimed_pending_paths(&claims).is_err());
    assert!(b.bump_pending_attempts(&claims).is_err());
    assert!(b
        .clear_pending_superseded(PendingSupersede::WholeTreeThrough(&through))
        .is_err());
    assert_eq!(b.pending_attempts("a.py").unwrap(), Some(0));
}

#[test]
fn pending_admission_restores_the_query_timeout_and_rolls_back_errors() {
    let store = Store::open_in_memory().unwrap();
    let before = store.pending_watermark().unwrap();
    let error: Result<()> =
        store.with_pending_transaction(std::time::Duration::from_millis(25), |tx| {
            Store::upsert_pending(tx, std::iter::once("refused.py"), 1.0)?;
            Err(refusal("injected write failure"))
        });
    assert!(error
        .unwrap_err()
        .to_string()
        .contains("injected write failure"));
    assert!(store.get_pending_paths().unwrap().is_empty());
    assert_eq!(store.pending_watermark().unwrap(), before);
    let timeout: i64 = lock_conn(&store.conn)
        .unwrap()
        .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
        .unwrap();
    assert_eq!(timeout, 5_000);
    store
        .enqueue_pending_paths(&["accepted.py".into()])
        .unwrap();
    let timeout: i64 = lock_conn(&store.conn)
        .unwrap()
        .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
        .unwrap();
    assert_eq!(timeout, 5_000);
}

#[test]
fn revision_exhaustion_rolls_back_the_entire_enqueue() {
    let store = Store::open_in_memory().unwrap();
    lock_conn(&store.conn)
        .unwrap()
        .execute(
            "UPDATE pending_state SET revision = 9223372036854775806",
            [],
        )
        .unwrap();
    assert!(store
        .enqueue_pending_paths(&["a.py".into(), "b.py".into()])
        .is_err());
    assert!(store.get_pending_paths().unwrap().is_empty());
    assert_eq!(store.pending_watermark().unwrap().revision, i64::MAX - 1);
}

#[test]
fn canonical_repair_invalidates_claims_for_both_spellings() {
    let root = std::env::temp_dir();
    let store = Store::open_in_memory().unwrap();
    // Directories are real work even without indexable files.
    enqueue_at(&store, ".", 100.0);
    enqueue_at(&store, root.to_str().unwrap(), 100.0);
    let claims = store.claim_pending_batch(2).unwrap();
    let result = store.reconcile_pending_paths(&root).unwrap();
    assert_eq!(result.rewritten.len(), 1);
    assert_eq!(store.clear_claimed_pending_paths(&claims).unwrap(), 0);
    assert_eq!(store.get_pending_paths().unwrap(), vec!["."]);
}

#[test]
fn a_narrow_build_cannot_retire_an_unread_quarantined_path() {
    let store = Store::open_in_memory().unwrap();
    enqueue_at(&store, "unread.py", 100.0);
    for _ in 0..MAX_PENDING_ATTEMPTS {
        store
            .bump_pending_attempts(&store.claim_pending_batch(1).unwrap())
            .unwrap();
    }
    let cleared = store
        .clear_pending_superseded(PendingSupersede::IndexedPathsThrough(
            &["other.py".into()],
            &store.pending_watermark().unwrap(),
        ))
        .unwrap();
    assert!(cleared.is_empty(), "unread work disappeared: {cleared:?}");
    assert_eq!(
        store.pending_attempts("unread.py").unwrap(),
        Some(MAX_PENDING_ATTEMPTS)
    );
}
