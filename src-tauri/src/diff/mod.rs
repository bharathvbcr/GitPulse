pub mod conflict;
mod conflict_fs;
pub mod conflict_session;
pub mod patch_builder;
pub mod word_diff;

pub use conflict::{
    ConflictChunk, ConflictDocument, ConflictResolutionChoice, ConflictResolver, FileSegment,
};
pub use patch_builder::{DiffLineType, FilePatch, PatchBuilder, UnifiedDiffHunk, UnifiedDiffLine};
pub use word_diff::{compute_word_diff, DiffChunkKind, DiffSegment, IntraLineDiff};
