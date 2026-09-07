//! Erlang `-include` and `-include_lib`.
//!
//! Both are preprocessor directives carrying a quoted path:
//! `-include("records.hrl").` names a file relative to the including module,
//! and `-include_lib("kernel/include/file.hrl").` names one relative to an
//! application's directory. The second form's first path segment is an
//! application name rather than a directory in this repository, which the
//! resolver's suffix rung handles; extraction keeps the path whole.
//!
//! `-import(lists, [map/2]).` is **not** extracted. It names a module for
//! unqualified calls, and the module-to-file mapping it implies is already
//! carried by the call graph — emitting it as an import would double-count the
//! same relation under a second edge kind.

use tree_sitter::Node;

use super::{file_import, first_string_literal};
use crate::model::ExtractedImport;
use crate::treesitter::{get_node_text, node_span};

pub(crate) fn extract_include(node: Node, source: &str, imports: &mut Vec<ExtractedImport>) {
    if !matches!(
        node.kind(),
        "pp_include" | "pp_include_lib" | "include_attribute" | "include_lib_attribute"
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
