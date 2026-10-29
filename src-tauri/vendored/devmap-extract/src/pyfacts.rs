//! Python facts (extract-v98): the one seam every Python-only fact is emitted
//! through.
//!
//! `treesitter::entry` makes exactly one call, [`collect`], for every file; it
//! answers for Python and returns an empty [`PyFacts`] for the rest. The first
//! fact to land here is the `getattr` dispatch site
//! ([`Extraction::dynamic_dispatch`]): `getattr(o, 'cmd_' + n)()` parses as a
//! `call` whose callee is itself a `call`, which no extractor code reads today.
//! It is a side table and not an `ExtractedCall` field, so a fake callee name
//! never reaches the resolver.
//!
//! The list is sorted and deduplicated before it is returned, every walk checks
//! the deadline, and a cap is recorded in the extraction's diagnostics, never
//! applied silently.
//!
//! Today [`collect`] returns an empty set for every language. That is the
//! scaffold: the field and the call exist, and nothing reads them yet.
//!
//! [`Extraction::dynamic_dispatch`]: crate::model::Extraction::dynamic_dispatch

use std::time::Instant;

use tree_sitter::Node;

use crate::model::{DynamicDispatch, ExtractedSymbol};

/// What [`collect`] hands back, one field per `Extraction` field it fills.
#[derive(Debug, Default)]
pub(crate) struct PyFacts {
    pub(crate) dynamic_dispatch: Vec<DynamicDispatch>,
}

/// The Python facts of one parsed file.
///
/// `symbols` are the file's emitted symbols, which a dispatch site's
/// `caller_symbol` must agree with; `deadline` is the one clock every other
/// stage of the file is checked against.
pub(crate) fn collect(
    _root: Node,
    _source: &str,
    _lang: &str,
    _symbols: &[ExtractedSymbol],
    _deadline: Instant,
) -> PyFacts {
    PyFacts::default()
}
