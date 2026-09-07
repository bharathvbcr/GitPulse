//! Rust `mod foo;` — the declaration that names a file.
//!
//! Found by measurement, not by the capability audit, and that is the point:
//! Rust already declared `Capability::Imports` because the extractor reads
//! `use_declaration`, so every check that asks "does this language extract
//! imports" answered yes. It does — for the wrong statement.
//!
//! `use` names a path in the *module tree*. `mod foo;` names a **file**:
//! `foo.rs` beside the declaring file, or `foo/mod.rs` under it, by a rule the
//! Rust reference fixes. It is the strongest case in this whole module, and it
//! had no handler. Measured on this repository before the fix: every
//! `langdecl/*.rs` and `langcalls/*.rs` module — thirty-odd files, each one
//! declared by a `mod` line in its own parent and used everywhere — was
//! reported as an unwired candidate, which is a delete-this suggestion for a
//! file the compiler will not build without.
//!
//! The `use_declaration` arm stays in `treesitter.rs` where it is. The split is
//! not arbitrary: this module's rule is "the specifier names a file", `mod` is
//! that statement in Rust and `use` is not, so they belong on opposite sides of
//! it.
//!
//! The specifier is emitted as `self::<path>` rather than as a bare name, so it
//! resolves through the resolver's existing `self::` rung — the one that
//! already tries `<dir>/x.rs` and `<dir>/x/mod.rs` — instead of needing a rung
//! of its own. It is also what the declaration *means*: `mod foo;` inside a
//! module declares `self::foo`.

use tree_sitter::Node;

use super::{file_import, unquote};
use crate::model::ExtractedImport;
use crate::treesitter::{get_node_text, node_span};

pub(crate) fn extract_mod(node: Node, source: &str, imports: &mut Vec<ExtractedImport>) {
    if node.kind() != "mod_item" {
        return;
    }
    // An inline module — `mod tests { … }` — has a body and names no file. Its
    // contents are in this same file and already extracted; emitting an import
    // for it would be an edge from a file to itself.
    if node.child_by_field_name("body").is_some() {
        return;
    }
    let Some(name) = node
        .child_by_field_name("name")
        .map(|child| get_node_text(child, source))
        .filter(|name| !name.is_empty())
    else {
        return;
    };

    // `#[path = "generated/foo.rs"] mod foo;` overrides the file the name would
    // pick. Honouring it matters: the attribute exists precisely because the
    // conventional path is wrong for that module, so a resolver that ignored it
    // would resolve to nothing and report the real file unwired.
    let raw = get_node_text(node, source);
    if let Some(path) = path_attribute(node, source) {
        imports.push(file_import(&raw, format!("self::{path}"), node_span(node)));
        return;
    }

    // A `mod` nested inside an inline module lives one directory deeper:
    // `mod a { mod b; }` declares `a/b.rs`. Walking the inline ancestors keeps
    // that case right rather than silently resolving it beside the file.
    let mut segments = vec![name];
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "mod_item" && parent.child_by_field_name("body").is_some() {
            if let Some(outer) = parent
                .child_by_field_name("name")
                .map(|child| get_node_text(child, source))
                .filter(|outer| !outer.is_empty())
            {
                segments.push(outer);
            }
        }
        current = parent.parent();
    }
    segments.reverse();
    imports.push(file_import(
        &raw,
        format!("self::{}", segments.join("::")),
        node_span(node),
    ));
}

/// The value of a `#[path = "…"]` attribute on this `mod_item`, if it has one.
///
/// The attribute is a **preceding sibling**, not a child: `tree-sitter-rust`
/// parses `#[path = "x.rs"] mod tables;` as an `attribute_item` followed by a
/// `mod_item`, both directly under the file. Reading children found nothing and
/// silently resolved the module to its conventional path instead — which is the
/// one case the attribute exists to say is wrong.
///
/// Read through nodes rather than by string-matching the attribute's text:
/// the grammar gives `attribute` an `identifier` and a `string_literal` as
/// separate children, so `#[ path  =  "x.rs" ]` and `#[path="x.rs"]` are the
/// same tree and only one of them survives text surgery.
fn path_attribute(node: Node, source: &str) -> Option<String> {
    let mut sibling = node.prev_sibling();
    while let Some(current) = sibling {
        match current.kind() {
            "attribute_item" => {
                if let Some(path) = path_from_attribute_item(current, source) {
                    return Some(path);
                }
            }
            // Doc comments and ordinary comments sit between an item and its
            // attributes as extras; walking past them keeps
            // `#[path = "x.rs"]\n// why\nmod m;` working.
            "line_comment" | "block_comment" => {}
            // Anything else ends the attribute run — the previous item.
            _ => return None,
        }
        sibling = current.prev_sibling();
    }
    None
}

fn path_from_attribute_item(item: Node, source: &str) -> Option<String> {
    let mut item_cursor = item.walk();
    let attribute = item
        .named_children(&mut item_cursor)
        .find(|child| child.kind() == "attribute")?;
    let mut attr_cursor = attribute.walk();
    let children: Vec<Node> = attribute.named_children(&mut attr_cursor).collect();
    let names_path = children
        .iter()
        .any(|child| child.kind() == "identifier" && get_node_text(*child, source) == "path");
    if !names_path {
        return None;
    }
    let literal = children
        .iter()
        .find(|child| child.kind().contains("string"))?;
    let path = unquote(&get_node_text(*literal, source));
    (!path.is_empty()).then_some(path)
}
