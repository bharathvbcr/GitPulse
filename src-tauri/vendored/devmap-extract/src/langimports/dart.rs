//! Dart `import` / `export` / `part`.
//!
//! Every one of them carries a quoted URI naming a file: `import 'util.dart'`,
//! `import 'package:app/util.dart'`, `part 'widget.g.dart'`. The `package:`
//! form maps to `lib/` by a rule the Dart tooling fixes, which the resolver
//! applies; extraction keeps the URI whole so that mapping has something to
//! work from.

use tree_sitter::Node;

use super::{file_import, first_string_literal};
use crate::model::ExtractedImport;
use crate::treesitter::{get_node_text, node_span};

pub(crate) fn extract_import(node: Node, source: &str, imports: &mut Vec<ExtractedImport>) {
    if !matches!(
        node.kind(),
        "import_or_export" | "library_import" | "part_directive" | "part_of_directive"
    ) {
        return;
    }
    // `import_or_export` wraps a `library_import`, which wraps an
    // `import_specification`. Both outer kinds are accepted, so the walk
    // reaching either one extracts the URI once — and the duplicate that would
    // otherwise produce is refused: only the outermost node of a nesting is
    // handled, by declining when an accepted ancestor exists.
    if has_accepted_ancestor(node) {
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

fn has_accepted_ancestor(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if matches!(
            parent.kind(),
            "import_or_export" | "library_import" | "part_directive" | "part_of_directive"
        ) {
            return true;
        }
        current = parent.parent();
    }
    false
}
