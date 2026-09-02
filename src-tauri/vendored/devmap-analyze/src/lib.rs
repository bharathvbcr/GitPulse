pub mod clustering;
pub mod liveness;
pub mod model;
pub mod pdg;
pub mod traversal;

pub use clustering::detect_communities;
pub use liveness::{analyze_liveness, GO_BUILD_VARIANT_REASON};
pub use model::*;
pub use pdg::*;
pub use traversal::*;

use devmap_extract::model::*;
use devmap_resolve::model::*;

pub fn analyze(extractions: &[Extraction], resolution: &ResolutionResult) -> AnalysisSummary {
    let dead_symbols = analyze_liveness(extractions, resolution);
    let communities = detect_communities(extractions, resolution);

    let total_symbols = extractions.iter().map(|e| e.symbols.len()).sum();

    AnalysisSummary {
        total_files: extractions.len(),
        total_symbols,
        total_edges: resolution.edges.len(),
        dead_symbols,
        communities,
        status: AnalysisStatus::Ok,
        unresolved_calls: resolution.unresolved.len(),
    }
}
