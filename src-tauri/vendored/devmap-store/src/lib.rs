pub mod db;
// The extraction cache belongs to the *build* path: it needs the parsing
// frontend to produce what it caches. A read-only consumer never touches it.
#[cfg(feature = "parse")]
pub mod extract_cache;
pub mod schema;

pub use db::{
    current_git_head, BuildHistoryRow, GenerationWriteOpts, Store, StoreStatus, StoredEdge,
    StoredFile, StoredSymbol, WalCheckpointMode, WalCheckpointResult,
};
#[cfg(feature = "parse")]
pub use extract_cache::{extract_tree_cached, extract_tree_cached_with_report};
pub use schema::*;
