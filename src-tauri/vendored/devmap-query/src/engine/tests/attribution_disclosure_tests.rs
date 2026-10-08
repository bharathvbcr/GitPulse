use super::analysis_coverage_gap;
#[cfg(feature = "parse")]
use super::{qualify_response, Request, StoreQueryEngine};
use devmap_analyze::AnalysisDisclosure;
use serde_json::{json, Value};

#[cfg(feature = "parse")]
#[test]
fn a_composed_read_retries_once_and_keeps_all_existing_qualifications() {
    for moving_reads in [0, 1, 2, usize::MAX] {
        let store = devmap_store::Store::open_in_memory().unwrap();
        let resolution = devmap_resolve::Resolver::new().resolve_all(&[]).unwrap();
        let analysis = devmap_analyze::AnalysisSummary {
            status: devmap_analyze::AnalysisStatus::Partial {
                reason: "parse coverage gap".into(),
            },
            ..Default::default()
        };
        store.save_generation(&[], &resolution, &analysis).unwrap();
        let engine = StoreQueryEngine::new(&store);
        let mut reads = 0;
        let response = engine
            .read_composed(
                || {
                    reads += 1;
                    let answer = engine.impact(Request {
                        query: "missing".into(),
                        token_budget: 2000,
                        min_confidence: 0.0,
                        max_depth: 3,
                    })?;
                    if reads <= moving_reads {
                        store.save_generation(&[], &resolution, &analysis)?;
                    }
                    Ok(answer)
                },
                qualify_response,
            )
            .unwrap();
        assert_eq!(reads, if moving_reads == 0 { 1 } else { 2 });
        let reason = response.walk_incomplete.unwrap();
        assert!(reason.contains("parse coverage gap"), "{reason}");
        assert_eq!(
            reason.contains("index moved"),
            moving_reads >= 2,
            "{reason}"
        );
    }
}

fn gap(fields: Value) -> Option<String> {
    let mut summary =
        json!({"total_files": 1, "total_symbols": 2, "total_edges": 1, "status": "Ok"});
    summary
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    let disclosure: AnalysisDisclosure = serde_json::from_value(summary).unwrap();
    analysis_coverage_gap(Some(&disclosure))
}

#[test]
fn legacy_or_inconsistent_counters_cannot_claim_complete_coverage() {
    for fields in [
        json!({}),
        json!({"unresolved_calls": null}),
        json!({"unresolved_calls": 4}),
        json!({"unresolved_calls": 4, "resolution_rate": null}),
        json!({"unresolved_calls": 4, "resolution_rate": {"unresolved_sites": 0, "explained_sites": 0}}),
        json!({"unresolved_calls": 4, "resolution_rate": {"unresolved_sites": 4, "explained_sites": 5}}),
        json!({"unresolved_calls": 0, "resolution_rate": {"unresolved_sites": 2, "explained_sites": 2}}),
    ] {
        let reason =
            gap(fields.clone()).unwrap_or_else(|| panic!("must remain uncertain: {fields}"));
        assert!(reason.contains("unknown"), "{fields}: {reason}");
        assert!(!reason.contains("missing that many edges"), "{reason}");
    }
}

#[test]
fn recorded_zero_and_fully_explained_counts_do_not_invent_a_gap() {
    for fields in [
        json!({"unresolved_calls": 0}),
        json!({"unresolved_calls": 0, "resolution_rate": {"unresolved_sites": 0, "explained_sites": 0}}),
        json!({"unresolved_calls": 4, "resolution_rate": {"unresolved_sites": 4, "explained_sites": 4}}),
    ] {
        assert_eq!(gap(fields), None);
    }
}

#[test]
fn explained_calls_do_not_erase_parse_failures_or_timeouts() {
    for status in [
        json!({"Partial": {"reason": "file not parsed"}}),
        json!({"Timeout": {"reason": "analysis deadline"}}),
    ] {
        let reason = gap(json!({"status": status, "unresolved_calls": 4, "resolution_rate": {"unresolved_sites": 4, "explained_sites": 4}})).unwrap();
        assert!(
            reason.contains("file not parsed") || reason.contains("analysis deadline"),
            "{reason}"
        );
    }
}
