//! Solidity `import`.
//!
//! `import "./Ownable.sol";`, `import {A} from "./A.sol";` and
//! `import * as Lib from "./Lib.sol";` all carry the path as a quoted string in
//! one `import_directive` node — the same claim JavaScript's `from` clause
//! makes, and resolved the same way.

use tree_sitter::Node;

use super::{file_import, first_string_literal};
use crate::model::ExtractedImport;
use crate::treesitter::{get_node_text, node_span};

pub(crate) fn extract_import(node: Node, source: &str, imports: &mut Vec<ExtractedImport>) {
    if node.kind() != "import_directive" {
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
