//! The scope a charged-hole lookup resolves and the partial-parse rows it then
//! reads describe one generation.
//!
//! `charged_hole_check` (reached through `outside_index_reason`, the miss path of
//! a scoped lookup) resolves a path scope from the latest generation's files
//! and then asked the store for the latest partial-parse rows in a second read.
//! A build that committed between the two paired generation N's files with
//! generation N+1's gaps: here the hole in `consts.rs` (generation 1) vanished
//! from the disclosure because generation 2, built in between, read the whole
//! file, and a name that may sit in the hole read as a clean miss.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use super::StoreQueryEngine;
use crate::scope::NameQueryFilter;
use devmap_extract::extract_file;
use devmap_extract::model::{Extraction, ParseOutcome, TextRange};
use devmap_resolve::Resolver;
use devmap_store::{GenerationWriteOpts, Store};

const SOURCE: &str = "pub const KEPT: u32 = 1;\npub const LOST_IN_HOLE: u32 = 2;\n";
const HOLE: &str = "pub const LOST_IN_HOLE: u32 = 2;";

thread_local! {
    static PRUNE_THE_SCOPE_GENERATION: Cell<bool> = const { Cell::new(false) };
    static PAUSE_FIRES: Cell<u32> = const { Cell::new(0) };
    static STORE_PATH: RefCell<Option<(PathBuf, PathBuf)>> = const { RefCell::new(None) };
}

struct PauseInstall;

impl Drop for PauseInstall {
    fn drop(&mut self) {
        devmap_store::db::set_generation_read_pause(None);
        STORE_PATH.with(|slot| *slot.borrow_mut() = None);
        PRUNE_THE_SCOPE_GENERATION.with(|prune| prune.set(false));
    }
}

fn save(store: &Store, root: &std::path::Path, partial: bool) -> u32 {
    let mut consts: Extraction = extract_file("consts.rs", SOURCE);
    if partial {
        let start = SOURCE.find(HOLE).unwrap();
        consts
            .symbols
            .retain(|symbol| symbol.name != "LOST_IN_HOLE");
        consts.parse_outcome = ParseOutcome::Partial {
            error_ranges: vec![TextRange {
                start_byte: start,
                end_byte: start + HOLE.len(),
            }],
            nameless_ranges: Vec::new(),
        };
    }
    let extractions = vec![consts];
    let mut resolver = Resolver::new();
    resolver.index_extractions(&extractions);
    let resolution = resolver.resolve_all(&extractions).unwrap();
    let analysis = devmap_analyze::analyze(&extractions, &resolution);
    store
        .save_generation_with_opts(
            &extractions,
            &resolution,
            &analysis,
            GenerationWriteOpts {
                repo_root: Some(root.to_string_lossy().into_owned()),
                ..Default::default()
            },
        )
        .unwrap()
}

/// Fires inside the snapshot that reads the scope's files, on the reading
/// thread, once: a second connection commits generation 2, which read the whole
/// file. Later fires (the pinned gap read's own snapshot) do nothing.
fn commit_a_clean_generation_from_a_second_connection() {
    let fire = PAUSE_FIRES.with(|fires| {
        fires.set(fires.get() + 1);
        fires.get()
    });
    if fire != 1 {
        return;
    }
    let (db, root) = STORE_PATH
        .with(|slot| slot.borrow().clone())
        .expect("the pause fired without a database");
    let writer = Store::open(&db).expect("a second connection opens the store");
    save(&writer, &root, false);
    if PRUNE_THE_SCOPE_GENERATION.with(Cell::get) {
        writer
            .prune_generations_except_latest(1)
            .expect("the prune committed while the read was paused");
    }
}

#[test]
fn a_build_between_the_scope_and_the_gap_read_does_not_hide_the_hole() {
    let dir = std::env::temp_dir().join(format!(
        "devmap-charged-pairing-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let root = dir.canonicalize().unwrap();
    std::fs::write(root.join("consts.rs"), SOURCE).unwrap();
    let db = root.join("index.sqlite");
    let store = Store::open(&db).unwrap();
    let first = save(&store, &root, true);
    STORE_PATH.with(|slot| *slot.borrow_mut() = Some((db, root.clone())));
    PAUSE_FIRES.with(|fires| fires.set(0));
    devmap_store::db::set_generation_read_pause(Some(
        commit_a_clean_generation_from_a_second_connection,
    ));
    let _guard = PauseInstall;

    let filter = NameQueryFilter::new(&["consts.rs".to_string()], &[], &[])
        .unwrap()
        .expect("a non-empty filter");
    // The miss path of a scoped lookup: its first generation-pinned read is the
    // scope's own, which is the one the pause races.
    let outside = StoreQueryEngine::new(&store)
        .outside_index_reason(Some(&filter), "LOST_IN_HOLE")
        .unwrap();

    assert!(
        PAUSE_FIRES.with(Cell::get) >= 1,
        "the pause never fired, so nothing raced"
    );
    assert!(
        store.latest_generation_id().unwrap().unwrap() > first,
        "the second connection did not commit a newer generation"
    );
    let reason = outside.reason.unwrap_or_else(|| {
        panic!("the hole read as a clean miss: the gap rows came from a different generation")
    });
    assert!(
        reason.contains("consts.rs"),
        "the hole of the generation the scope was resolved against is named: {reason}"
    );
    drop(_guard);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A scope whose generation was pruned before its gap rows were read could not
/// be checked, and a scoped lookup that could not check its scope is
/// unavailable (the reason), not a clean miss and not a background note.
#[test]
fn a_pruned_scope_generation_makes_the_scoped_lookup_unavailable() {
    let dir = std::env::temp_dir().join(format!(
        "devmap-charged-pruned-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let root = dir.canonicalize().unwrap();
    std::fs::write(root.join("consts.rs"), SOURCE).unwrap();
    let db = root.join("index.sqlite");
    let store = Store::open(&db).unwrap();
    save(&store, &root, true);
    STORE_PATH.with(|slot| *slot.borrow_mut() = Some((db, root.clone())));
    PAUSE_FIRES.with(|fires| fires.set(0));
    PRUNE_THE_SCOPE_GENERATION.with(|prune| prune.set(true));
    devmap_store::db::set_generation_read_pause(Some(
        commit_a_clean_generation_from_a_second_connection,
    ));
    let _guard = PauseInstall;

    let filter = NameQueryFilter::new(&["consts.rs".to_string()], &[], &[])
        .unwrap()
        .expect("a non-empty filter");
    let outside = StoreQueryEngine::new(&store)
        .outside_index_reason(Some(&filter), "LOST_IN_HOLE")
        .unwrap();

    let reason = outside.reason.unwrap_or_else(|| {
        panic!(
            "a scope whose generation was pruned read as a clean miss; note: {:?}",
            outside.note
        )
    });
    assert!(reason.contains("no longer in the store"), "{reason}");
    assert!(
        outside.note.is_none(),
        "the pruned scope is the reason, not background"
    );
    drop(_guard);
    let _ = std::fs::remove_dir_all(&dir);
}
