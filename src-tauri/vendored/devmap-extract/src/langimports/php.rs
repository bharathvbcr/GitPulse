//! PHP `use`, `require` and `include`.
//!
//! Two syntaxes with different standing, both emitted and both documented as
//! what they are:
//!
//! * `require 'lib/util.php'` names a file outright. It is the same claim
//!   `#include` makes and belongs here without qualification.
//! * `use App\Foo\Bar;` names a class in a namespace. PSR-4 maps that to
//!   `src/Foo/Bar.php`, and PSR-4 is a *convention* enforced by Composer's
//!   autoloader rather than a rule of the language — a weaker footing than
//!   Java's, and it is stated rather than glossed. It is still emitted, because
//!   the failure mode is bounded in the safe direction: a specifier that maps
//!   to no indexed file produces no edge at all, so a project that does not
//!   follow PSR-4 loses nothing it had, and one that does gains a real edge.

use tree_sitter::Node;

use super::{file_import, first_string_literal};
use crate::model::ExtractedImport;
use crate::treesitter::{get_node_text, node_span};

pub(crate) fn extract_use_and_require(
    node: Node,
    source: &str,
    imports: &mut Vec<ExtractedImport>,
) {
    match node.kind() {
        "namespace_use_declaration" => extract_namespace_use(node, source, imports),
        "require_expression"
        | "require_once_expression"
        | "include_expression"
        | "include_once_expression" => {
            // `require __DIR__ . '/util.php'` is the idiomatic form and the
            // concatenation is part of the expression, so taking the first
            // string yields `/util.php` — a path relative to the *file*, which
            // is what `__DIR__` means. The leading slash is left on; the
            // resolver normalises it against the importing file's directory.
            let Some(specifier) = first_string_literal(node, source) else {
                return;
            };
            imports.push(file_import(
                &get_node_text(node, source),
                specifier,
                node_span(node),
            ));
        }
        _ => {}
    }
}

/// `use A\B\C;`, `use A\B\{C, D};` and `use function A\B\c;` all reach here.
///
/// Each `namespace_use_clause` names one target, so a grouped declaration emits
/// one import per clause rather than one for the group — the group is sugar,
/// and collapsing it would under-report the file's actual dependencies.
fn extract_namespace_use(node: Node, source: &str, imports: &mut Vec<ExtractedImport>) {
    let raw = get_node_text(node, source);
    let mut cursor = node.walk();
    let mut emitted = false;
    for clause in node.named_children(&mut cursor) {
        if clause.kind() != "namespace_use_clause" && clause.kind() != "namespace_use_group" {
            continue;
        }
        emitted |= push_qualified_names(clause, source, &raw, node, imports);
    }
    if !emitted {
        // A grammar release that flattens the clause puts the qualified name
        // directly under the declaration. Falling back keeps this arm from
        // silently extracting nothing, which is the failure mode the whole
        // module exists to remove.
        push_qualified_names(node, source, &raw, node, imports);
    }
}

fn push_qualified_names(
    scope: Node,
    source: &str,
    raw: &str,
    span_node: Node,
    imports: &mut Vec<ExtractedImport>,
) -> bool {
    let mut cursor = scope.walk();
    let mut emitted = false;
    for child in scope.named_children(&mut cursor) {
        if !matches!(child.kind(), "qualified_name" | "namespace_name" | "name") {
            continue;
        }
        let specifier = get_node_text(child, source).trim().to_string();
        if specifier.is_empty() {
            continue;
        }
        imports.push(file_import(raw, specifier, node_span(span_node)));
        emitted = true;
    }
    emitted
}
