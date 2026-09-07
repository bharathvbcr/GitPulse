//! Pascal `uses`.
//!
//! `uses SysUtils, App.Helpers;` names units, and a unit is one file named for
//! it — a rule every Pascal compiler fixes, which is what puts Pascal inside
//! this module's rule. One clause names several units, so each is emitted
//! separately rather than collapsed into the clause text.

use tree_sitter::Node;

use super::file_import;
use crate::model::ExtractedImport;
use crate::treesitter::{get_node_text, node_span};

pub(crate) fn extract_uses(node: Node, source: &str, imports: &mut Vec<ExtractedImport>) {
    if !matches!(node.kind(), "declUses" | "uses_clause") {
        return;
    }
    let raw = get_node_text(node, source);
    let mut cursor = node.walk();
    let mut emitted = false;
    for child in node.named_children(&mut cursor) {
        if !matches!(child.kind(), "moduleName" | "identifier" | "unit_name") {
            continue;
        }
        // `uses App.Helpers in 'src/helpers.pas';` gives the file outright; the
        // `in` clause is a separate child and the unit name still names the
        // unit, so the unit name is what is emitted and the string is left to
        // the generic string rung below.
        let specifier = get_node_text(child, source).trim().to_string();
        if specifier.is_empty() {
            continue;
        }
        imports.push(file_import(&raw, specifier, node_span(node)));
        emitted = true;
    }
    if !emitted {
        if let Some(specifier) = super::first_string_literal(node, source) {
            imports.push(file_import(&raw, specifier, node_span(node)));
        }
    }
}
