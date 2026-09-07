//! Nix `import ./path`.
//!
//! Nix has no import statement; `import` is a builtin applied to a path, so the
//! specifier lives in an `apply_expression` whose function is the identifier
//! `import` and whose argument is a `path_expression`. `import ./lib` names a
//! directory, which Nix resolves to `lib/default.nix` — a rule the language
//! fixes, and the one the resolver's candidate ladder applies.
//!
//! `import <nixpkgs>` names a search-path entry, never a file in this
//! repository. It reaches the resolver, matches nothing and produces no edge.

use tree_sitter::Node;

use super::{file_import, first_string_literal};
use crate::model::ExtractedImport;
use crate::treesitter::{get_node_text, node_span};

pub(crate) fn extract_import(node: Node, source: &str, imports: &mut Vec<ExtractedImport>) {
    if node.kind() != "apply_expression" {
        return;
    }
    let Some(function) = node
        .child_by_field_name("function")
        .or_else(|| node.named_child(0))
    else {
        return;
    };
    if get_node_text(function, source).trim() != "import" {
        return;
    }
    let Some(argument) = node
        .child_by_field_name("argument")
        .or_else(|| node.named_child(1))
    else {
        return;
    };
    // A path is unquoted in Nix (`./lib/foo.nix`), so the shared string finder
    // matches it through its `path_expression` / `path_fragment` kinds; a
    // quoted `"./lib/foo.nix"` is matched through the string kinds. Reading the
    // argument's own text is the fallback for a grammar release that names
    // neither, and it is exact here because the argument *is* the path.
    let specifier = first_string_literal(argument, source)
        .unwrap_or_else(|| super::unquote(&get_node_text(argument, source)));
    if specifier.is_empty() {
        return;
    }
    imports.push(file_import(
        &get_node_text(node, source),
        specifier,
        node_span(node),
    ));
}
