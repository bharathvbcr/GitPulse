//! Plants a task in a dc-store the way schema 10 makes one: a workbench item.
//!
//! The legacy `tasks` table was retired at dc-store schema 10, so a fixture can
//! no longer `INSERT INTO tasks`. `items.put` is the only door a task comes
//! through now, and it validates `scope` where it enters; this goes through it
//! rather than writing `work_items` rows by hand, so a fixture exercises the
//! real schema and can never plant a scope the store would have refused.
//!
//! Included by path (`#[path = ".../plant_task.rs"]`) from the crate's tests and
//! from integration tests, which cannot see `#[cfg(test)]` items.

use dc_store::Store;

/// Plants task `id` with `status` and the `scope` JSON object `scope`
/// (`{}` for a task with no authority declared).
pub fn plant(store: &Store, id: &str, status: &str, scope: &str) {
    store
        .ensure_local_repository()
        .expect("the local repository registers");
    let request = format!(
        r#"{{"request_id":"plant-{id}","id":"{id}","expected_revision":0,"title":"planted {id}","status":"{status}","repository_ids":["{repo}"],"primary_repository_id":"{repo}","scope":{scope}}}"#,
        repo = dc_store::workbench::LOCAL_REPOSITORY_ID,
    );
    store
        .workbench_request("items.put", &request)
        .unwrap_or_else(|e| panic!("planting {id}: {e}"));
}
