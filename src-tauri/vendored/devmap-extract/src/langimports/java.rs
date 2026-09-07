//! Java `import`.
//!
//! `import com.foo.Bar;` names `com/foo/Bar.java` by a rule the Java Language
//! Specification fixes — a public type must live in a file named for it, under
//! a directory path matching its package — which is what puts Java inside this
//! module's rule and C# outside it.

use tree_sitter::Node;

use super::{file_import, jvm_dotted_specifier};
use crate::model::ExtractedImport;
use crate::treesitter::{get_node_text, node_span};

pub(crate) fn extract_import(node: Node, source: &str, imports: &mut Vec<ExtractedImport>) {
    if node.kind() != "import_declaration" {
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
