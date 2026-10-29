//! Java declaration identity.
//!
//! Java had no module of its own: `declaration_of` fell through to
//! [`super::generic`], a node-kind table and a `name` field. This module is the
//! seat for the Java rules that table cannot state (fields, enum constants,
//! `record`, `@interface`, anonymous classes), so a later change edits this file
//! and not the registry. For now it delegates to `generic` unchanged, which is
//! what keeps every Java symbol id exactly as it was.

use tree_sitter::Node;

use super::Declaration;

/// The declaration `node` is under Java's rules: the generic rule, until a Java
/// rule is stated here.
pub(crate) fn declaration(node: Node, source: &str) -> Option<Declaration> {
    super::generic(node, source)
}
