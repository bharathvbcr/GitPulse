pub mod coverage;
pub mod db;
pub mod edge_index;
// The extraction cache belongs to the *build* path: it needs the parsing
// frontend to produce what it caches. A read-only consumer never touches it.
#[cfg(feature = "parse")]
pub mod extract_cache;
pub mod schema;

pub use coverage::{
    discovery_refusals, CoverageGapRow, CoverageGapSample, CoverageGaps, DiscoveryRefusal,
    COVERAGE_GAP_SAMPLE, GAP_DISCOVERY_REFUSED,
};
pub use db::{
    checked_min_confidence, current_git_head, BuildHistoryRow, CallersPage, DeadPage,
    GenerationWriteOpts, PageSizeConversion, PendingClaim, PendingEnqueueReport, PendingReconcile,
    PendingSupersede, SearchPage, Store, StoreStatus, StoredEdge, StoredFile, StoredSymbol,
    VacuumAction, VacuumOutcome, WalCheckpointMode, WalCheckpointResult, WriteBreakdown,
    WriterLock, MAX_PENDING_ATTEMPTS,
};
pub use edge_index::{
    edge_kind_from_stored, edge_resolution, resolution_kind_from_stored, resolution_kind_label,
    DirectedEdges, EdgeResolution, GenerationEdges, ResolutionSource, StoredResolutionKind,
    UnknownEdgeKind, UnknownResolutionKind,
};
#[cfg(feature = "parse")]
pub use extract_cache::{
    extract_scanned_cached, extract_tree_cached, extract_tree_cached_with_report,
};
pub use schema::*;
