pub mod clones;
pub mod clustering;
pub mod dead_clusters;
pub mod graph_intel;
pub mod liveness;
pub mod model;
pub mod pdg;
// The producer for `pdg`: source in, `FunctionPdgInput` out. Behind `parse`,
// because it needs a grammar — the whole module, not only its entry point:
// the sink table and its helpers have no reader without one, and left outside
// the gate they were four warnings the embedder shape carried for nobody.
#[cfg(feature = "parse")]
pub mod pdgsrc;
pub mod resolution_rate;
pub mod traversal;

pub use clones::{
    candidates_from_extractions, clone_coverage, group_clones, CloneCandidate, CloneCoverage,
    CloneGroup, CloneKind, CloneMember, CloneSummary,
};
pub use clustering::{detect_communities, CommunityDetection};
pub use dead_clusters::{
    dead_clusters, DeadClusterReport, DeadClusterScan, DEAD_CLUSTER_CAP, DEAD_CLUSTER_CONFIDENCE,
    DEAD_CLUSTER_MEMBER_CAP, DEAD_CLUSTER_QUALIFIED_CONFIDENCE,
};
pub use graph_intel::{
    graph_intel, FileChurn, GodNode, GraphIntel, Hotspot, ImportCycle, GOD_NODE_CAP, HOTSPOT_CAP,
    IMPORT_CYCLE_CAP,
};
pub use liveness::{
    analyze_liveness, analyze_liveness_with_coverage, exempt_symbol_names, extraction_coverage,
    extraction_gaps, DiscoveryCoverage, ExtractionCoverage, ExtractionGap, ExtractionGapEntry,
    LivenessOutcome, CALL_BLIND_REASON, COVERAGE_LOSS_CONFIDENCE_CAP, COVERAGE_LOSS_REASON,
    GO_BUILD_VARIANT_REASON, HIGHEST_DEGRADED_CONFIDENCE, UNRESOLVED_NAMESAKE_REASON,
};
pub use model::*;
pub use pdg::*;
pub use resolution_rate::{resolution_rate, LanguageResolution, Permille, ResolutionRate};
pub use traversal::*;

use devmap_extract::model::*;
use devmap_resolve::model::*;

/// Analyse a corpus that was handed to us directly, with no discovery step.
///
/// Correct for callers that build their own `extractions` — the single-file
/// preview path, and tests. A caller that walked a tree must use
/// [`analyze_with_discovery`] instead and pass what discovery refused, or the
/// summary will report full coverage of a corpus it never fully saw.
pub fn analyze(extractions: &[Extraction], resolution: &ResolutionResult) -> AnalysisSummary {
    analyze_with_discovery(extractions, resolution, DiscoveryCoverage::none())
}

/// Analyse a corpus, told what discovery refused before extraction saw it.
///
/// The refusal count cannot be recovered from `extractions`: a file discovery
/// turned away has no `Extraction` at all. That is why the gap survived the fix
/// which closed the parse-failure half of the same rule — every check was
/// computed from the slice, and the slice is exactly what the missing files are
/// missing from.
pub fn analyze_with_discovery(
    extractions: &[Extraction],
    resolution: &ResolutionResult,
    discovery: DiscoveryCoverage,
) -> AnalysisSummary {
    let liveness = analyze_liveness_with_coverage(extractions, resolution, discovery);
    let dead_symbols = liveness.reports;
    let detection = detect_communities(extractions, resolution);
    let clone_coverage = clone_coverage(extractions);

    let total_symbols = extractions.iter().map(|e| e.symbols.len()).sum();

    AnalysisSummary {
        total_files: extractions.len(),
        total_symbols,
        total_edges: resolution.edges.len(),
        dead_symbols,
        communities: detection.communities,
        // Computed, not asserted. This field was a literal `Ok` on every path,
        // which made `AnalysisStatus::Partial` and `Timeout` unconstructible
        // and the two arms rendering them unreachable — so N4's acceptance
        // ("a check that could not run never reports as one that passed") was
        // satisfied by a type that existed and a value that never varied.
        //
        // Two independent reasons now reach it, and both must. Clustering
        // convergence answers "did the partition settle"; extraction coverage
        // answers "was every file's calls looked for" — the second was the
        // half nothing folded in, so a generation missing whole files' call
        // edges reported `ok` and drove `graph_degraded = false`, which is what
        // let a maximum-confidence proposal to delete working code out of a
        // check that could not run. Concatenated rather than ranked because a
        // reader acting on `Partial` needs every reason it holds, not the
        // first one that happened to fire.
        status: match combine_reasons(detection.degraded, liveness.coverage.degraded_reason()) {
            None => AnalysisStatus::Ok,
            Some(reason) => AnalysisStatus::Partial { reason },
        },
        unresolved_calls: resolution.unresolved.len(),
        clone_coverage,
        // Recorded verbatim, unmeasured included. A caller that walked a tree
        // reports what it refused; one that supplied its own corpus reports
        // `None`, and the difference is what lets the daemon carry a real
        // measurement forward without inventing one.
        discovery_refused_files: discovery.refused_files(),
        // Computed here rather than in the CLI so every consumer of a summary
        // reads one number: the CLI renders it, the store persists it, and the
        // Python ratchet fences it. A rate computed at the point of printing is
        // a rate nothing else can ratchet.
        resolution_rate: crate::resolution_rate::resolution_rate(extractions, resolution),
        // Under a *stricter* coverage ceiling than every other verdict, because
        // the claim is stronger: a cluster finding is wrong outright if one
        // call edge into the component was missed, where the same missed edge
        // costs a single-symbol finding only itself. `cap_cluster` prices that
        // by compounding the ceiling over the membership, so a forty-symbol
        // cluster in a five-percent-blind corpus is no longer priced the same
        // as a two-symbol cluster in a corpus that is one file short.
        dead_clusters: {
            let mut scan = crate::dead_clusters::dead_clusters(extractions, resolution);
            let degraded = !liveness.coverage.is_complete();
            for cluster in &mut scan.clusters {
                cluster.confidence = liveness
                    .coverage
                    .cap_cluster(cluster.confidence, cluster.size);
                // The reason has to move with the number. It said "reached by
                // nothing outside the component" — an absolute claim — beside a
                // confidence the coverage hole had already demoted, so a reader
                // taking the prose at face value saw no caveat at all. The
                // single-symbol path swaps in `COVERAGE_LOSS_REASON` for exactly
                // this; a cluster is the *stronger* claim and had the weaker
                // disclosure.
                if degraded {
                    cluster.reason.push_str(
                        " — but call extraction did not cover every file, and one \
                         missed call edge into this component makes the whole \
                         finding wrong",
                    );
                }
            }
            scan
        },
    }
}

/// Join two "why this answer is qualified" reasons, keeping `None` when
/// neither fired.
///
/// `None` is load-bearing: here it is the only value that produces
/// `AnalysisStatus::Ok`, and therefore `graph_degraded: false` and
/// `analysis_status: "ok"`. A clean corpus whose partition converged must
/// still report exactly that, or the flag stops meaning anything.
///
/// Concatenated rather than ranked, and public for the same reason: a reader
/// acting on a degraded answer needs every reason it holds, not the first one
/// that happened to fire, and the query layer qualifies its dead-symbol
/// answers with exactly this pair-of-reasons shape.
pub fn combine_reasons(first: Option<String>, second: Option<String>) -> Option<String> {
    match (first, second) {
        (None, None) => None,
        (Some(reason), None) | (None, Some(reason)) => Some(reason),
        (Some(first), Some(second)) => Some(format!("{first}; {second}")),
    }
}
