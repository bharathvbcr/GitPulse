//! devmap in-process link test, originally validation spike S1.
//!
//! Proves that `devmap-query` and `devmap-store` compile cleanly alongside
//! Tauri's dependency tree and can execute in-process queries without
//! collisions — the question the spike existed to answer before any of the
//! control plane was built.
//!
//! It earns its place after the fact: it now exercises the *vendored* copies
//! under `src-tauri/vendored/`, from a separate test crate, with no
//! dev-dependency of its own. `vendor-contract` checks that no path dependency
//! leaves the repository; this checks that what those paths point at still
//! links and runs.

use devmap_query::{Request, StoreQueryEngine};
use devmap_store::Store;

#[test]
fn spike_s1_devmap_in_process_link_and_query() {
    // 1. Instantiate in-memory store
    let store = Store::open_in_memory().expect("Store::open_in_memory failed");

    // 2. Initialize StoreQueryEngine
    let engine = StoreQueryEngine::new(&store);

    // 3. Execute impact request on empty/mock store
    let req = Request {
        query: "src/main.rs".to_string(),
        token_budget: 1000,
        min_confidence: 0.0,
        max_depth: 3,
    };

    let resp = engine.impact(req).expect("impact query failed");

    // When no generation exists, devmap-query returns an unavailable or empty response safely
    println!(
        "Spike S1 Link Success: impact query completed with resolution: {:?}",
        resp.resolution
    );
    assert_eq!(resp.items.len(), 0);
}
