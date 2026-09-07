//! `#include` and `#import` — the C family's entire import surface.
//!
//! This is the gap that made W0.3 necessary. `unwired_candidates` asks whether
//! anything imports a file, and for C, C++, Objective-C and CUDA the extractor
//! had **no `imports.push` site at all** — not a partial one, not a wrong one,
//! none. Every header and every translation unit in a C-family repository was
//! structurally unwired, which is why the honest gate excludes them and counts
//! the exclusion rather than reporting them.
//!
//! One module serves all four keys because `#include` is one grammar node in
//! every linked C-family grammar, and `tree-sitter-objc` gives `#import` the
//! same `preproc_include` node rather than a kind of its own.

use tree_sitter::Node;

use super::{file_import, unquote};
use crate::model::ExtractedImport;
use crate::treesitter::{get_node_text, node_span};

pub(crate) fn extract_include(node: Node, source: &str, imports: &mut Vec<ExtractedImport>) {
    if node.kind() != "preproc_include" {
        return;
    }
    let Some(path) = node.child_by_field_name("path") else {
        return;
    };
    // `#include MACRO` and `#include HDR(x)` are legal, and the grammar puts an
    // `identifier` or a `call_expression` in the same field. They name no file
    // until the preprocessor has run, which this extractor does not do. Refused
    // rather than turned into a specifier that can only ever mis-resolve — the
    // repository's own rule that a check which could not run must not report
    // the same result as one that ran and passed.
    if !matches!(path.kind(), "string_literal" | "system_lib_string") {
        return;
    }
    let raw = get_node_text(path, source);
    let specifier = if path.kind() == "system_lib_string" {
        // `#include <vector>`: the angle brackets are part of the token. The
        // header may still belong to this repository — a target built with
        // `-Iinclude` includes its own headers in exactly this form, which is
        // ordinary in every CMake project — so it is emitted. When it really is
        // a system header no repository file matches it and the resolver
        // creates no edge, which costs an unresolved lookup and claims nothing.
        raw.trim()
            .trim_start_matches('<')
            .trim_end_matches('>')
            .trim()
            .to_string()
    } else {
        // `#include "util.h"`: `string_literal` wrapping a `string_content`.
        // Trimming the quote characters serves both that shape and grammars
        // that keep the quotes inside one token.
        unquote(&raw)
    };
    if specifier.is_empty() {
        return;
    }
    imports.push(file_import(
        &get_node_text(node, source),
        specifier,
        node_span(node),
    ));
}
