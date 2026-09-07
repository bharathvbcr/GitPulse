//! MarkDev core — Markdown parsing, vault indexing, and search.
//!
//! This crate holds everything that is not AppKit. It knows nothing about
//! views, fonts, or layout; it turns Markdown text into flat, ordered data
//! that the Swift side renders. Keeping the split that strict is what lets
//! the parser be tested exhaustively without a UI.
//!
//! Feature flags:
//! - `ffi` (default): C ABI + cbindgen header generation for MarkDev.app.
//! - `highlight` (default): tree-sitter syntax highlighting.
//!
//! Library consumers that only need the parse model (e.g. GitPulse) depend
//! with `default-features = false`, and opt into `highlight` when they want
//! tree-sitter spans without pulling cbindgen into their build.

#[cfg(feature = "ffi")]
pub mod ffi;
#[cfg(feature = "highlight")]
pub mod highlight;
pub mod html;
pub mod md;
pub mod vault;

pub use md::{parse_checked, BlockKind, ParseError, ParseResult, SpanKind};
