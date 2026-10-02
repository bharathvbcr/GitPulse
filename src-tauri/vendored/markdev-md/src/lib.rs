//! Markdown parsing into the flat model the TextKit 2 editor renders from.

pub mod incremental;
pub mod model;
pub mod obsidian;
pub mod parse;

pub use incremental::{Document, Reparse};
pub use model::{
    BlockDescriptor, BlockKind, CalloutKind, ParseResult, SpanKind, StyleSpan, SyntaxMarker,
    Utf16Mapper, CALLOUT_FOLD_COLLAPSED, CALLOUT_FOLD_EXPANDED, CALLOUT_FOLD_SHIFT,
    CALLOUT_KIND_MASK, MAX_DOCUMENT_BYTES, MAX_INTERNED_STRINGS, MAX_INTERNED_STRING_BYTES,
    MAX_PARSE_EVENTS, MAX_PARSE_NESTING, MAX_STRUCTURAL_RECORDS, MAX_TOTAL_STRING_BYTES, NO_INFO,
    TASK_STATUS_SHIFT,
};
pub use parse::{parse_checked, ParseError};
