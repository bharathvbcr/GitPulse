//! Terraform / OpenTofu `module { source = … }`.
//!
//! HCL is the one language here whose import names a **directory**:
//! `source = "./modules/vpc"` pulls in every `.tf` file under that path. That
//! is still a rule the language fixes rather than a convention, so it meets
//! this module's bar, and the resolver expands the directory the same way it
//! already expands a Go package.
//!
//! Only `source` inside a `module` block is read. `required_providers` also
//! carries a `source`, and it names a registry address — `hashicorp/aws` — that
//! is not a path at all. Both would resolve to nothing, so the narrowing does
//! not change any answer today; it is there so that a future rung which maps
//! registry addresses somewhere cannot silently start treating a provider as a
//! local module.

use tree_sitter::Node;

use super::{file_import, unquote};
use crate::model::ExtractedImport;
use crate::treesitter::{get_node_text, node_span};

pub(crate) fn extract_module_source(node: Node, source: &str, imports: &mut Vec<ExtractedImport>) {
    if node.kind() != "block" {
        return;
    }
    let mut cursor = node.walk();
    let block_kind = node
        .named_children(&mut cursor)
        .find(|child| child.kind() == "identifier")
        .map(|child| get_node_text(child, source).trim().to_string());
    if block_kind.as_deref() != Some("module") {
        return;
    }
    let Some(body) = block_body(node) else {
        return;
    };
    let mut body_cursor = body.walk();
    for attribute in body.named_children(&mut body_cursor) {
        if attribute.kind() != "attribute" {
            continue;
        }
        let mut attr_cursor = attribute.walk();
        let named = attribute.named_children(&mut attr_cursor).next();
        if named.map(|n| get_node_text(n, source).trim().to_string()) != Some("source".to_string())
        {
            continue;
        }
        // `string_lit` wraps `template_literal` between two quote tokens, so
        // the node text carries the quotes and `unquote` removes them. An
        // interpolated source — `"${path.module}/x"` — keeps its `${…}` and
        // matches no indexed file, which is the honest outcome for a path this
        // extractor cannot evaluate.
        let Some(literal) = first_string_lit(attribute) else {
            continue;
        };
        let specifier = unquote(&get_node_text(literal, source));
        if specifier.is_empty() {
            continue;
        }
        imports.push(file_import(
            &get_node_text(node, source),
            specifier,
            node_span(attribute),
        ));
    }
}

fn block_body(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    let body = node
        .named_children(&mut cursor)
        .find(|child| child.kind() == "body");
    body
}

/// The first `string_lit` under `node`, within the two levels HCL nests an
/// attribute value through (`expression` → `literal_value` → `string_lit`).
fn first_string_lit(node: Node) -> Option<Node> {
    fn within(node: Node, depth: u8) -> Option<Node> {
        if depth == 0 {
            return None;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "string_lit" {
                return Some(child);
            }
            if let Some(found) = within(child, depth - 1) {
                return Some(found);
            }
        }
        None
    }
    within(node, 4)
}
