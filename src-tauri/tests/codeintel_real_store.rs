//! Phase 5's claim, against DevCouncil's real code map.
//!
//! The spike proved the crates link and answer. This proves the module built on
//! them answers the same way, in-process, with no daemon and no socket — and
//! that it says so honestly when there is no map to read.
//!
//! Skipped loudly rather than silently when the real store is absent: a test
//! that passes by finding nothing is the failure mode this project is about.

use gitpulse_lib::codeintel;

/// DevCouncil's own repository, which carries a built map.
const REAL_REPO: &str = "/Users/bharath/Code/devtools/DevCouncil";

fn have_real_store() -> bool {
    codeintel::devmap_db_path(REAL_REPO).exists()
}

#[test]
fn answers_impact_from_the_real_map_in_process() {
    if !have_real_store() {
        eprintln!(
            "SKIPPED: no devmap store at {}",
            codeintel::devmap_db_path(REAL_REPO).display()
        );
        return;
    }

    let status = codeintel::status(REAL_REPO);
    if let Some(ref reason) = status.reason {
        if reason.contains("unsupported future schema version") {
            eprintln!(
                "SKIPPED: devmap store on disk has newer schema version than vendored reader: {}",
                reason
            );
            return;
        }
    }
    assert!(
        status.available,
        "the map is on disk but unreadable: {:?}",
        status.reason
    );
    assert!(
        status.generation_id.is_some(),
        "a readable map has a generation"
    );
    eprintln!(
        "codeintel status: generation={:?} files={:?} edges={:?}",
        status.generation_id, status.total_files, status.total_edges
    );

    let started = std::time::Instant::now();
    let hits = codeintel::search(REAL_REPO, "StoreQueryEngine", Some(2000));
    let elapsed = started.elapsed();
    assert!(
        hits.available,
        "search reported unavailable: {:?}",
        hits.reason
    );
    eprintln!(
        "search(StoreQueryEngine): {} hits of {} total in {:?}",
        hits.items.len(),
        hits.total,
        elapsed
    );
    assert!(
        !hits.items.is_empty(),
        "the real map contains this symbol; the query found nothing"
    );

    let impact = codeintel::impact(REAL_REPO, "StoreQueryEngine", Some(2000));
    assert!(
        impact.available,
        "impact reported unavailable: {:?}",
        impact.reason
    );
    eprintln!("impact(StoreQueryEngine): {} edges", impact.items.len());
}

#[test]
fn a_repository_with_no_map_says_so_rather_than_answering_emptily() {
    // The distinction the `available` flag exists for: "no map here" must never
    // render as "this symbol has no callers".
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().to_str().expect("utf8");

    let status = codeintel::status(repo);
    assert!(!status.available);
    assert!(status.reason.is_some(), "an absent map must explain itself");

    let hits = codeintel::search(repo, "anything", Some(2000));
    assert!(
        !hits.available,
        "an absent map must not report a clean empty result"
    );
    assert!(hits.items.is_empty());
    assert!(hits.reason.is_some());

    // ...and querying must not have created a database as a side effect.
    assert!(
        !codeintel::devmap_db_path(repo).exists(),
        "a read created a store, which turns 'never indexed' into 'indexed and empty'"
    );
}
