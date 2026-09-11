//! Shell `source` and `.`.
//!
//! A shell script has no import keyword. Loading another file is the builtin
//! `source` (and its POSIX spelling `.`), which names a path outright when the
//! argument is a literal word. `source "$HELPER"` computes its path at runtime
//! and is refused: emitting the expansion's text would invent a specifier the
//! author never wrote.

use tree_sitter::Node;

use super::file_import;
use crate::model::ExtractedImport;
use crate::treesitter::{get_node_text, node_span};

pub(crate) fn extract_source(node: Node, source: &str, imports: &mut Vec<ExtractedImport>) {
    if node.kind() != "command" {
        return;
    }
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    if !matches!(get_node_text(name_node, source).as_str(), "source" | ".") {
        return;
    }
    let Some(argument) = node.child_by_field_name("argument") else {
        return;
    };
    // Only a `word` is a static path. A `string` here is `"$HELPER"` or
    // `"$(find_helper)"` — computed, and the same refusal `first_string_literal`
    // applies to a nested invocation in the other languages.
    if argument.kind() != "word" {
        return;
    }
    let specifier = get_node_text(argument, source);
    let specifier = specifier.trim();
    if specifier.is_empty() || specifier.contains('$') {
        return;
    }
    imports.push(file_import(
        &get_node_text(node, source),
        specifier.to_string(),
        node_span(node),
    ));
}
