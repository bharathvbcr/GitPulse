//! Kotlin `import`.
//!
//! Same rule as Java and the same dotted-name shape, with two differences that
//! the shared helper already covers: the grammar names the node `import` rather
//! than `import_declaration`, and Kotlin has no `static` form (a top-level
//! function is imported by its own dotted name).
//!
//! Kotlin's file-naming rule is weaker than Java's — a file may hold several
//! top-level declarations and need not be named for any of them — so a dotted
//! name resolves to a file only when the conventional name matches. That is a
//! resolver concern; extracting the specifier is correct either way, and a
//! specifier that matches no file produces no edge rather than a wrong one.

use tree_sitter::Node;

use super::{file_import, jvm_dotted_specifier};
use crate::model::ExtractedImport;
use crate::treesitter::{get_node_text, node_span};

pub(crate) fn extract_import(node: Node, source: &str, imports: &mut Vec<ExtractedImport>) {
    // `import_header` is the kind in the tree-sitter-kotlin releases that wrap
    // the directive in a header node; `import` is the kind in the ones that do
    // not. Both are accepted rather than pinning the grammar version, because a
    // silent miss here is exactly the failure this module exists to end.
    if !matches!(node.kind(), "import" | "import_header") {
        return;
    }
    let Some(specifier) = jvm_dotted_specifier(node, source) else {
        return;
    };
    imports.push(file_import(
        &get_node_text(node, source),
        specifier,
        node_span(node),
    ));
}
