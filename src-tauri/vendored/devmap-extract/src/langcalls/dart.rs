//! Dart call extraction.
//!
//! Dart reached `extract_node`'s generic arm and produced declarations only —
//! 0 `Calls` edges under this port and under the Python implementation it
//! replaces. New capability, not a restored regression.
//!
//! # This module is correct and currently inert, and that is reported, not hidden
//!
//! Dart emits **no function or method symbols at all**, which is a
//! declaration-side defect this module cannot fix and must not disguise.
//! `generic_declaration_name` reads a declaration's `name` field, and
//! `tree-sitter-dart` puts the name one level down: a `function_declaration`
//! carries `signature=function_signature`, and the `name` field lives on the
//! *signature*. Measured on a file declaring a top-level function, a class with
//! a constructor, two methods, a mixin and an extension, the only symbols
//! emitted were the class and the enum.
//!
//! The consequence is stated rather than worked around. Because no Dart
//! callable is a symbol, `enclosing_declared_symbol` correctly returns `None`
//! for every Dart call, so each edge is attributed to its file — coarse but
//! joinable, never orphaned — and no Dart call can resolve to a Dart target
//! until the declaration side names these functions. Extracting the calls now
//! is still the right half to own: the shapes below are read from the grammar's
//! own parse tree and become live the moment declarations land.

use super::jvm_dotnet::{receiver_of, record, CallSite};
use super::scope::receiver_from;
use crate::model::{ExtractedCall, ExtractedReference, ReferenceKind};
use tree_sitter::Node;

pub(crate) fn extract_dart_call(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    if let Some(site) = call_site(node, source) {
        record(site, source, "dart", file_symbol_name, calls, references);
    }
}

fn call_site<'tree>(node: Node<'tree>, source: &str) -> Option<CallSite<'tree>> {
    match node.kind() {
        "call_expression" => {
            let (name, receiver) = callee_target(node.child_by_field_name("function")?, source)?;
            Some(CallSite {
                call: node,
                name,
                receiver,
                kind: ReferenceKind::Call,
            })
        }
        // `new C()` and `const C()`. Dart also allows `C()` with no keyword,
        // which is an ordinary `call_expression` handled above — the grammar
        // draws no distinction there and neither can this, so such a call is
        // recorded as a `Call`. Nothing is lost: the resolver types a receiver
        // from any reference whose name indexes to exactly one class, and a
        // reference kind is not consulted for that.
        "new_expression" | "const_object_expression" => {
            let (name, scope) = type_target(node.child_by_field_name("type")?, source)?;
            Some(CallSite {
                call: node,
                name,
                receiver: scope,
                kind: ReferenceKind::Constructor,
            })
        }
        // `b..add(1)..add(2)`. Each section is its own node with the method in
        // a `property` field; the target is the expression the sections hang
        // off, which is their common parent's first child.
        "cascade_call_expression" => {
            let name = node.child_by_field_name("property")?;
            Some(CallSite {
                call: node,
                name,
                receiver: cascade_receiver(node, source),
                kind: ReferenceKind::Call,
            })
        }
        _ => None,
    }
}

fn callee_target<'tree>(
    function: Node<'tree>,
    source: &str,
) -> Option<(Node<'tree>, Option<String>)> {
    match function.kind() {
        "identifier" => Some((function, None)),
        // `o.m()`, `a?.b()`, `C.named(…)` — a named constructor is a member of
        // its class, so it splits the same way and lands with `C` as receiver.
        "member_expression" | "null_aware_member_expression" => {
            let object = function.child_by_field_name("object")?;
            // `super.m()`: the superclass is not named at the call site, and
            // recording callee `m` would let same-file resolution bind it to
            // this class's own override at full confidence.
            if object.kind() == "super" {
                return None;
            }
            let name = function.child_by_field_name("property")?;
            let receiver = receiver_of(object, source, "this");
            Some((name, receiver))
        }
        // An inline `(x) => …` has no callee identity.
        _ => None,
    }
}

/// `(name node, qualifier)` for a `new`/`const` expression's type.
fn type_target<'tree>(
    type_node: Node<'tree>,
    source: &str,
) -> Option<(Node<'tree>, Option<String>)> {
    if type_node.kind() != "type" {
        return None;
    }
    let count = type_node.named_child_count();
    let name = type_node
        .named_child(count.checked_sub(1)?)
        .filter(|last| last.kind() == "type_identifier")?;
    let scope = count
        .checked_sub(2)
        .and_then(|index| type_node.named_child(index))
        .and_then(|scope| receiver_from(scope, source));
    Some((name, scope))
}

/// The expression a cascade's sections are applied to.
///
/// `b..add(1)..add(2)` places `b` and both `cascade_section`s under one parent,
/// so the target is that parent's first named child — refused unless it is a
/// bare identifier, since a receiver that is an expression tells the resolver
/// nothing it can key on here.
fn cascade_receiver(node: Node, source: &str) -> Option<String> {
    let section = node.parent().filter(|p| p.kind() == "cascade_section")?;
    let target = section.parent()?.named_child(0)?;
    if target.kind() != "identifier" {
        return None;
    }
    receiver_from(target, source)
}
