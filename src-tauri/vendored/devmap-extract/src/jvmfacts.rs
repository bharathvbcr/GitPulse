//! The JVM-family declaration facts (extract-v98): the one seam every JVM fact
//! is emitted through.
//!
//! `treesitter::entry` makes exactly one call, [`collect`], for every file; the
//! function answers for the languages it knows and returns an empty
//! [`JvmFacts`] for the rest. Each later fact (Scala and C# package scopes,
//! type parameters, header and body spans, modifiers, local extents, declaration
//! gaps) is a leaf function called from here, so a lane that adds one edits this
//! file and not the extraction entry point.
//!
//! The fields it fills are [`Extraction::jvm_decls`], [`Extraction::package_scopes`],
//! [`Extraction::local_extents`] and [`Extraction::decl_gaps`]. Every list is
//! sorted and deduplicated before it is returned, because the determinism gate
//! digests the payload, and every walk checks the deadline and has a hard cap
//! that is recorded in the extraction's diagnostics: a capped fact is `None` or
//! flagged, never an empty list that reads as "none exist".
//!
//! Today [`collect`] returns an empty set for every language. That is the
//! scaffold: the fields and the call exist, and nothing reads them yet.
//!
//! [`Extraction::jvm_decls`]: crate::model::Extraction::jvm_decls
//! [`Extraction::package_scopes`]: crate::model::Extraction::package_scopes
//! [`Extraction::local_extents`]: crate::model::Extraction::local_extents
//! [`Extraction::decl_gaps`]: crate::model::Extraction::decl_gaps

use std::time::Instant;

use tree_sitter::Node;

use crate::model::{
    ExtractedSymbol, JvmDeclFact, LocalExtent, PackageScope, ParseOutcome, TextRange,
};

/// What [`collect`] hands back, one field per `Extraction` field it fills.
#[derive(Debug, Default)]
pub(crate) struct JvmFacts {
    pub(crate) jvm_decls: Vec<JvmDeclFact>,
    pub(crate) package_scopes: Vec<PackageScope>,
    pub(crate) local_extents: Vec<LocalExtent>,
    pub(crate) decl_gaps: Vec<TextRange>,
}

/// The JVM-family facts of one parsed file.
///
/// `symbols` are the file's emitted symbols, which a declaration fact joins to
/// by `(qualified_name, span.start_byte)`; `parse_outcome` is what the
/// declaration-gap fact reads its error ranges from; `deadline` is the one
/// clock every other stage of the file is checked against.
pub(crate) fn collect(
    _root: Node,
    _source: &str,
    _lang: &str,
    _symbols: &[ExtractedSymbol],
    _parse_outcome: &ParseOutcome,
    _deadline: Instant,
) -> JvmFacts {
    JvmFacts::default()
}
