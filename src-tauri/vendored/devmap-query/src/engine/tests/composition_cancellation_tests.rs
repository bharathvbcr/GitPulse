use super::*;
use devmap_extract::extract_file;
use devmap_resolve::Resolver;
use devmap_store::GenerationWriteOpts;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

// Cancel at an observed checkpoint, after work has started. A timer based
// on a cold baseline could fire after a warm query had already completed.
#[test]
fn a_cancelled_composition_stops_partway_through_the_fan_out() {
    let store = wide_fixture(16);
    let targets: Vec<_> = (0..MAX_NEIGHBOR_TARGETS)
        .map(|i| format!("mod{i}.py"))
        .collect();
    assert_eq!(
        StoreQueryEngine::new(&store)
            .neighbors(&targets, 200_000, 0.0, 6)
            .unwrap()
            .len(),
        targets.len()
    );
    let cancel = Cancel::new();
    let trigger = cancel.clone();
    let checks = Arc::new(AtomicUsize::new(0));
    let observed = checks.clone();
    let cancel = cancel.with_check_probe(move || {
        if observed.fetch_add(1, Ordering::Relaxed) == 2 {
            trigger.cancel();
        }
    });
    let outcome = StoreQueryEngine::new(&store)
        .with_cancel(cancel)
        .neighbors(&targets, 200_000, 0.0, 6);
    assert!(
        outcome.is_err(),
        "an in-flight cancellation must stop the composition"
    );
    assert_eq!(
        checks.load(Ordering::Relaxed),
        3,
        "work must stop at the checkpoint that observes cancellation"
    );
}
fn wide_fixture(fan_per_module: usize) -> Store {
    let mut sources: Vec<(String, String)> = Vec::new();
    let mut hub = String::new();
    for index in 0..MAX_NEIGHBOR_TARGETS {
        hub.push_str(&format!(
            "def hub{index}(rows):\n    return sum(rows)\n\n\n"
        ));
    }
    sources.push(("hub.py".to_string(), hub));
    for module in 0..MAX_NEIGHBOR_TARGETS {
        let mut body = String::from("import hub\n\n\n");
        for func in 0..fan_per_module {
            body.push_str(&format!(
                "def f{module}_{func}(rows):\n    return hub.hub{}(rows)\n\n\n",
                func % MAX_NEIGHBOR_TARGETS
            ));
        }
        sources.push((format!("mod{module}.py"), body));
    }
    let extractions: Vec<_> = sources
        .iter()
        .map(|(path, body)| extract_file(path, body))
        .collect();
    let mut resolver = Resolver::new();
    resolver.index_extractions(&extractions);
    let resolution = resolver.resolve_all(&extractions).unwrap();
    let analysis = devmap_analyze::analyze(&extractions, &resolution);
    let store = Store::open_in_memory().unwrap();
    store
        .save_generation_with_opts(
            &extractions,
            &resolution,
            &analysis,
            GenerationWriteOpts::default(),
        )
        .unwrap();
    store
}
