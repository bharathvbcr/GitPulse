//! A namesake read that a prune races is the generation's own ledger, and a
//! read of a generation the store no longer holds is not an empty ledger.
//!
//! `unresolved_sites_naming` checks that the generation exists and then reads
//! its ledger. A prune committed by another connection between the two used to
//! leave the select looking at a store without that generation: a subset of the
//! ledger (rows still open into a retained generation survive, rows that had
//! ended are deleted) stored as `NamesakeRead::Read`, with `walk_incomplete`
//! unset. The read is now one snapshot: the pre-prune rows, or `Ok(None)` ->
//! `GenerationGone` when the generation was already gone at the check, which
//! the impact answer reports on `walk_incomplete`.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use super::{
    unavailable_response, NamesakeRead, ResolutionAvailability, Response, StoreQueryEngine,
};
use devmap_extract::extract_file;
use devmap_resolve::Resolver;

const TARGET: &str = "src/ledger.py::Ledger.settle";
const LEDGER: &str = "class Ledger:\n    def settle(self):\n        return 1\n";
const UNTYPED: &str = "def untyped(book):\n    return book.settle()\n";
const OTHER: &str = "def other(x):\n    return x.settle()\n";

thread_local! {
    static PAUSE_FIRES: Cell<u32> = const { Cell::new(0) };
    static PRUNE_TARGET: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

struct PauseInstall;

impl Drop for PauseInstall {
    fn drop(&mut self) {
        devmap_store::db::set_generation_read_pause(None);
        PRUNE_TARGET.with(|slot| *slot.borrow_mut() = None);
    }
}

/// Runs on the reading thread inside its snapshot. It opens a second `Store`;
/// the paused one's mutex is held and is not re-entrant.
fn prune_from_a_second_connection() {
    PAUSE_FIRES.with(|slot| slot.set(slot.get() + 1));
    let path = PRUNE_TARGET
        .with(|slot| slot.borrow().clone())
        .expect("the pause fired without a database to prune");
    let pruner = devmap_store::Store::open(&path).expect("a second connection opens the store");
    pruner
        .prune_generations_except_latest(1)
        .expect("the prune committed while the read was paused");
}

fn save(store: &devmap_store::Store, files: &[(&str, &str)]) -> u32 {
    let extractions: Vec<_> = files
        .iter()
        .map(|(path, body)| extract_file(path, body))
        .collect();
    let mut resolver = Resolver::new();
    resolver.index_extractions(&extractions);
    let resolution = resolver.resolve_all(&extractions).unwrap();
    let analysis = devmap_analyze::analyze(&extractions, &resolution);
    store
        .save_generation(&extractions, &resolution, &analysis)
        .unwrap()
}

fn read_sites(engine: &StoreQueryEngine<'_>, generation: u32) -> Option<String> {
    match engine
        .unresolved_namesakes(generation, TARGET, &[])
        .unwrap()
    {
        NamesakeRead::Read(found) => Some(format!("{:?}", found.sites)),
        NamesakeRead::GenerationGone => None,
        NamesakeRead::NotApplicable => panic!("the symbol query was treated as naming no callee"),
    }
}

#[test]
fn a_pruned_generation_mid_read_is_not_an_empty_namesake_read() {
    let dir = std::env::temp_dir().join(format!(
        "devmap-pruned-namesake-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("index.sqlite");
    let store = devmap_store::Store::open(&db_path).unwrap();
    let old = save(
        &store,
        &[
            ("src/ledger.py", LEDGER),
            ("src/a.py", UNTYPED),
            ("src/c.py", OTHER),
        ],
    );
    let latest = save(
        &store,
        &[
            ("src/ledger.py", LEDGER),
            ("src/a.py", UNTYPED),
            ("src/b.py", "def beta():\n    return 2\n"),
        ],
    );
    assert!(latest > old);
    let (generation, index) = store.generation_edges_with_id().unwrap().unwrap();
    assert_eq!(generation, latest);
    let engine = StoreQueryEngine::new(&store);

    // The baseline holds a row the prune deletes (src/c.py's range ended at
    // `latest`) beside one it keeps, so a partial read cannot equal it.
    let baseline = read_sites(&engine, old).expect("the fixture generation is live");
    assert!(
        baseline.contains("src/a.py") && baseline.contains("src/c.py"),
        "the fixture does not hold both a carried and an ended site: {baseline}"
    );

    PAUSE_FIRES.with(|slot| slot.set(0));
    PRUNE_TARGET.with(|slot| *slot.borrow_mut() = Some(db_path.clone()));
    devmap_store::db::set_generation_read_pause(Some(prune_from_a_second_connection));
    let _guard = PauseInstall;

    let raced = read_sites(&engine, old)
        .expect("the check had passed, so the read is the snapshot's, not GenerationGone");
    assert_eq!(PAUSE_FIRES.with(|slot| slot.get()), 1);
    assert!(
        store.list_generation_paths(old).is_err(),
        "the prune did not commit during the pause, so nothing raced"
    );
    assert_eq!(
        raced, baseline,
        "a prune landing inside the read changed the ledger it returned"
    );

    // A read that starts after the prune committed is the refusal.
    assert!(
        read_sites(&engine, old).is_none(),
        "a generation the store no longer holds was stored as a read"
    );
    let mut response: Response<()> = unavailable_response(ResolutionAvailability::Available);
    engine
        .attach_unresolved_namesakes(old, &index, TARGET, 0.0, 0, &mut response)
        .unwrap();
    assert!(
        response.unresolved_namesakes.is_none(),
        "a missing generation must not be stored as an empty ledger read: {:?}",
        response.unresolved_namesakes
    );
    let note = response.walk_incomplete.as_deref().unwrap_or("");
    assert!(
        note.contains("could not be read") && note.contains(&old.to_string()),
        "walk_incomplete does not say the ledger read was refused: {note:?}"
    );
    assert_eq!(PAUSE_FIRES.with(|slot| slot.get()), 1);
    drop(_guard);
    let _ = std::fs::remove_dir_all(&dir);
}
