//! Lua and Luau `require`.
//!
//! `require("app.util")` and `require "app.util"` are the same `function_call`
//! node with and without parentheses. The dotted specifier maps to
//! `app/util.lua` by the default `package.path`, which is a rule the runtime
//! fixes rather than a project convention.
//!
//! Luau shares Lua's grammar and its `require` semantics, so one module serves
//! both keys — the same reasoning `langcalls::lua` records for its own arm.

use tree_sitter::Node;

use super::{file_import, first_string_literal};
use crate::model::ExtractedImport;
use crate::treesitter::{get_node_text, node_span};

pub(crate) fn extract_require(node: Node, source: &str, imports: &mut Vec<ExtractedImport>) {
    if !matches!(node.kind(), "function_call" | "call_expression") {
        return;
    }
    let callee = node
        .child_by_field_name("name")
        .or_else(|| node.child_by_field_name("function"))
        .or_else(|| node.named_child(0));
    let Some(callee) = callee else {
        return;
    };
    if get_node_text(callee, source).trim() != "require" {
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
