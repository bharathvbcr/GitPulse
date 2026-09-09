//! Shell out to the installed `devmap` CLI for operations GitPulse cannot do
//! in-process, and read the consumer `repo_map.json` navigator artifact.
//!
//! GitPulse links `devmap-query` / `devmap-store` with `default-features =
//! false`, so it can *read* a map but cannot build one or run `preview` (both
//! need the parse frontend). The installed CLI always matches the schema on
//! disk; this module is the seam that keeps that true without pulling
//! tree-sitter into the app binary.

pub mod cli;
mod diagnostics;
pub mod live;
pub mod repo_map;
pub mod viz;

pub use cli::{
    build, is_build_in_flight, preview, preview_many, refresh, resolve_binary,
    status as cli_status, BuildOutcome, CliStatus, DevmapLookup, PreviewFileResult, PreviewOutcome,
};
pub use live::{
    decide_live_refresh, freshness_from_cli_status, maybe_refresh, LiveRefreshDecision,
    LiveRefreshFacts, LiveRefreshFactsDto, LiveRefreshOutcome,
};
pub use repo_map::{
    load_repo_map, parse_repo_map, repo_map_path, CapMeta, LivenessMeta, PreferredDeadLists,
    RepoMapDocument, RepoMapLoad, RepoMapSubsystem,
};
pub use viz::{
    code_graph_path, load_code_graph_viz, load_map_preview, GraphVizKind, GraphVizLoad,
    DEFAULT_VIZ_MAX_NODES,
};
