//! R `source()`.
//!
//! `source("helpers.R")` is R's only static file-level dependency: it names a
//! path outright and evaluates it in the current environment. `library(dplyr)`
//! deliberately does **not** belong here — it names an installed package, never
//! a file in the repository, and emitting it would produce a specifier that can
//! only ever fail to resolve while inflating the import count.

use tree_sitter::Node;

use super::{file_import, first_string_literal};
use crate::model::ExtractedImport;
use crate::treesitter::{get_node_text, node_span};

pub(crate) fn extract_source(node: Node, source: &str, imports: &mut Vec<ExtractedImport>) {
    if node.kind() != "call" {
        return;
    }
    let callee = node
        .child_by_field_name("function")
        .or_else(|| node.named_child(0));
    let Some(callee) = callee else {
        return;
    };
    if !matches!(
        get_node_text(callee, source).trim(),
        "source" | "sys.source"
    ) {
        return;
    }
    let Some(specifier) = first_string_literal(node, source) else {
        return;
    };
    imports.push(file_import(
        &get_node_text(node, source),
        specifier,
        node_span(node),
    ));
}
