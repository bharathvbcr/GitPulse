//! Ruby `require`, `require_relative` and `load`.
//!
//! Ruby has no import keyword; loading another file is an ordinary method call,
//! which is why the specifier lives inside a `call` node rather than a
//! directive of its own. `require_relative 'helper'` names a sibling file
//! outright. `require 'app/helper'` names one relative to the load path, which
//! for a repository's own code means a `lib/` or root-relative path.
//!
//! `require 'json'` names a gem and matches no indexed file, so it produces no
//! edge — the same shape as a C system header, and the same cost.

use tree_sitter::Node;

use super::{file_import, first_string_literal};
use crate::model::ExtractedImport;
use crate::treesitter::{get_node_text, node_span};

pub(crate) fn extract_require(node: Node, source: &str, imports: &mut Vec<ExtractedImport>) {
    if node.kind() != "call" {
        return;
    }
    let Some(method) = node.child_by_field_name("method") else {
        return;
    };
    if !matches!(
        get_node_text(method, source).as_str(),
        "require" | "require_relative" | "load"
    ) {
        return;
    }
    // A receiver means this is not the Kernel method: `loader.require 'x'` is
    // somebody else's API and says nothing about this file's dependencies.
    if node.child_by_field_name("receiver").is_some() {
        return;
    }
    let Some(arguments) = node.child_by_field_name("arguments") else {
        return;
    };
    // `require File.join(dir, 'x')` and `require CONST` compute their path at
    // runtime. Only a literal argument names a file statically, and
    // `first_string_literal` returning nothing is exactly that case.
    let Some(specifier) = first_string_literal(arguments, source) else {
        return;
    };
    imports.push(file_import(
        &get_node_text(node, source),
        specifier,
        node_span(node),
    ));
}
