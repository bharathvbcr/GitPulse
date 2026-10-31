//! Java declaration identity.
//!
//! Java had no module of its own: `declaration_of` fell through to
//! [`super::generic`], a node-kind table and a `name` field. This module is the
//! seat for the Java rules that table cannot state. Kinds the table already
//! answers keep the generic rule and so keep every symbol id exactly as it was;
//! what is stated here is the part of the language the table has no entry for.
//!
//! * **`record Point(int x, int y) {}`** is `record_declaration`, a node kind the
//!   table does not list, so a record emitted *no symbol* and every member of it
//!   — an explicit method, a compact constructor — was owned by whatever type
//!   enclosed it, or by the file. Measured on the Orbit `java/records` fixture:
//!   `Range.java` produced `Range` and `length` as bare file-level functions and
//!   `Point.java` produced nothing, so each call into a record had no target.
//!   A record is a final class, so it is a `Class`; no new kind, because a
//!   reader older than this change must still be able to read the symbol.
//! * **The components of a record are its accessors.** `Point(int x, int y)`
//!   declares `x()` and `y()` unless the body writes its own, and a call `p.x()`
//!   names one of them. Each component is therefore a method of the record, and
//!   its span is the component's own, so it anchors to the record header. An
//!   explicit zero-argument method of the same name replaces the implicit
//!   accessor, and the component then declares nothing: two declarations of
//!   `Wrapped.raw` would be a duplicate qualified name, a broken join key.
//!   Not synthesised: `equals`, `hashCode`, `toString` and the implicit canonical
//!   constructor. None names a source span of its own, and a `new Point(…)` with
//!   no declared constructor already binds to the type.
//! * **A compact constructor** (`public Bounds { … }`) is a constructor, and a
//!   record is the type that owns it.
//! * **An enum constant** (`EARTH(…)` in `enum Planet`) is a field of its enum,
//!   which is what `Planet.EARTH` names. Kotlin's `enum_entry` is the precedent.

use tree_sitter::Node;

use crate::model::SymbolKind;
use crate::treesitter::{bounded_parent, get_child_text, get_node_text};

use super::Declaration;

/// The declaration `node` is under Java's rules.
pub(crate) fn declaration(node: Node, source: &str) -> Option<Declaration> {
    let (declared_kind, name) = match node.kind() {
        "record_declaration" => (
            SymbolKind::Class,
            get_child_text(node, "name", source).filter(|name| !name.is_empty())?,
        ),
        "compact_constructor_declaration" => (
            SymbolKind::Function,
            get_child_text(node, "name", source).filter(|name| !name.is_empty())?,
        ),
        "enum_constant" => (
            SymbolKind::Field,
            get_child_text(node, "name", source).filter(|name| !name.is_empty())?,
        ),
        "formal_parameter" | "spread_parameter" => {
            let record = record_of_component(node)?;
            let name = component_name(node, source)?;
            if body_declares_accessor(record, &name, source) {
                return None;
            }
            return Declaration::new(
                SymbolKind::Function,
                enclosing_type_name(node, source),
                name,
            );
        }
        _ => crate::treesitter::generic_declaration(node, source)?,
    };
    Declaration::new(declared_kind, enclosing_type_name(node, source), name)
}

/// The record whose header declares `node` as a component, or `None` for any
/// other parameter.
///
/// The parent must be the record's own `parameters` list: a method inside the
/// record carries a `formal_parameters` too, and its parameters are not
/// components.
fn record_of_component(node: Node) -> Option<Node> {
    let list = bounded_parent(node)?;
    let record = bounded_parent(list)?;
    (record.kind() == "record_declaration"
        && record
            .child_by_field_name("parameters")
            .is_some_and(|parameters| parameters.id() == list.id()))
    .then_some(record)
}

/// A component's name: the `name` field of a `formal_parameter`, or the declarator
/// a varargs `spread_parameter` wraps (`int... xs`).
fn component_name(node: Node, source: &str) -> Option<String> {
    let named = if node.kind() == "spread_parameter" {
        let mut cursor = node.walk();
        let declarator = node
            .named_children(&mut cursor)
            .find(|child| child.kind() == "variable_declarator")?;
        declarator.child_by_field_name("name")?
    } else {
        node.child_by_field_name("name")?
    };
    (named.kind() == "identifier")
        .then(|| get_node_text(named, source))
        .filter(|name| !name.is_empty())
}

/// Whether the record body declares a zero-argument method called `name`: the
/// explicit accessor that replaces the implicit one.
fn body_declares_accessor(record: Node, name: &str, source: &str) -> bool {
    let Some(body) = record.child_by_field_name("body") else {
        return false;
    };
    let mut cursor = body.walk();
    let found = body.named_children(&mut cursor).any(|member| {
        member.kind() == "method_declaration"
            && get_child_text(member, "name", source).is_some_and(|method| method == name)
            && member
                .child_by_field_name("parameters")
                .is_some_and(|parameters| parameters.named_child_count() == 0)
    });
    found
}

/// The nearest enclosing type, as the generic rule reads it — the table's
/// type kinds, and a callable ends the walk — with `record_declaration` a type
/// and a compact constructor a callable.
fn enclosing_type_name(node: Node, source: &str) -> Option<String> {
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        let kind = parent.kind();
        if kind == "record_declaration" {
            return get_child_text(parent, "name", source).filter(|name| !name.is_empty());
        }
        if kind == "compact_constructor_declaration" {
            return None;
        }
        if let Some(declared) = crate::treesitter::generic_symbol_kind(kind) {
            if !matches!(declared, SymbolKind::Function) {
                return get_child_text(parent, "name", source).filter(|name| !name.is_empty());
            }
            return None;
        }
        ancestor = bounded_parent(parent);
    }
    None
}
