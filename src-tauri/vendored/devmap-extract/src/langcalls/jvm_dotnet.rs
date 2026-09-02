//! Shared emission for the four class-based languages this directory adds:
//! Java, C#, Kotlin and Dart.
//!
//! Caller attribution is **not** re-derived here. `scope::enclosing_emitted_symbol`
//! already mirrors the generic declaration arm, and mirroring it twice in one
//! directory is how the two copies come to disagree — which is the SC9/SC14
//! failure at one remove. What lives here is the part those four grammars need
//! and the other languages do not: the local binding an expression's value is
//! assigned to, and one place that builds the `ExtractedCall`/`ExtractedReference`
//! pair so the callee gate, the identity and the binding each have a single
//! site rather than four.
//!
//! The identity requirement is worth restating, because it is why the mirror is
//! mandatory rather than a nicety. Measured on these grammars before any
//! extraction existed:
//!
//! | shape | emitted symbol | `enclosing_callable_qualified` |
//! |---|---|---|
//! | Java method in an `interface` | `F::I.d` | `F::d` |
//! | Java method in an `enum` | `F::E.em` | `F::em` |
//! | C# method in a `struct` | `F::S.M` | `F::M` |
//! | C# method in a `record` inside a `namespace` | `F::N.M` | `F::M` |
//!
//! Every one of those is an orphaned edge, because the declaration side
//! qualifies through `generic_symbol_kind` — whose non-`Function` kinds include
//! `interface_declaration`, `enum_declaration`, `struct_declaration` and
//! `namespace_declaration` — while `enclosing_type_name`, written for
//! Python/JS/Rust, matches only `class_*`, `trait_item` and `impl_item`. Java
//! and C# put methods inside all of the former.

use super::scope::{enclosing_emitted_symbol, receiver_from};
use crate::model::{ExtractedCall, ExtractedReference, ReferenceKind};
use crate::treesitter::{get_node_text, node_span, split_call_target};
use tree_sitter::Node;

/// The single local name this expression's value is bound to, when the grammar
/// proves an assignment.
///
/// This is what lets `var w = new Widget(); w.run()` dispatch: the resolver
/// types a receiver from a reference carrying `assigned_to`, provided the
/// referenced name indexes to exactly one class or struct. That filter is the
/// safety, so recording a binding for an ordinary function call costs nothing —
/// `var n = helper(1)` binds `n` to a `Function`, which the filter drops.
///
/// The walk stops at an argument list deliberately: in `var x = foo(bar())` the
/// value bound to `x` is `foo`'s result, and letting `bar()` reach `x` would
/// bind a real name to the wrong type. Binding nothing is the fail-closed
/// direction (SC9).
pub(super) fn assigned_binding(mut node: Node, source: &str) -> Option<String> {
    for _ in 0..4 {
        node = node.parent()?;
        let target = match node.kind() {
            // Java, C#
            "variable_declarator" => node.child_by_field_name("name"),
            // Dart
            "initialized_variable_definition" | "initialized_identifier" => {
                node.child_by_field_name("name")
            }
            // Java, C#, Dart
            "assignment_expression" => node.child_by_field_name("left"),
            // Kotlin
            "assignment" => node.child_by_field_name("left"),
            // Kotlin `val w = Widget()`: the grammar gives the declaration no
            // fields and hangs the name off a `variable_declaration` child.
            "property_declaration" => node
                .named_child(0)
                .filter(|child| child.kind() == "variable_declaration")
                .and_then(|child| child.named_child(0)),
            _ => None,
        };
        if let Some(target) = target {
            return simple_name(target, source);
        }
        if matches!(
            node.kind(),
            "argument_list"
                | "arguments"
                | "value_arguments"
                | "block"
                | "expression_statement"
                | "return_statement"
        ) {
            break;
        }
    }
    None
}

/// One identifier, or nothing.
///
/// Dart wraps an assignment target in `assignable_expression`, so that one
/// wrapper is unwrapped. Anything else that is not a bare identifier names
/// either more than one thing or an expression, and binding it would be a
/// guess of exactly the kind that produces a confidently wrong edge.
fn simple_name(node: Node, source: &str) -> Option<String> {
    if node.kind() == "assignable_expression" {
        return node
            .named_child(0)
            .filter(|inner| inner.kind() == "identifier")
            .map(|inner| get_node_text(inner, source));
    }
    if node.kind() != "identifier" {
        return None;
    }
    let name = get_node_text(node, source);
    (!name.is_empty()).then_some(name)
}

/// The receiver a call is reached through.
///
/// `this` is kept rather than dropped. Dropping it would make `this.m()`
/// indistinguishable from a bare `m()`, and the two differ where it matters: an
/// inherited `this.m()` that resolves to nothing is a receiver we could not
/// type (`UninferredReceiver`), not a bare-name failure, and merging those is
/// what made the defect tier unreadable before SC30.
pub(super) fn this_receiver() -> Option<String> {
    Some("this".to_string())
}

/// A call this module is about to record.
pub(super) struct CallSite<'tree> {
    /// The whole call expression: owns the call's span and the assignment walk.
    pub call: Node<'tree>,
    /// The node whose text is the callee's *name*. Passed through
    /// `split_call_target`, which is the one place a callee name is built.
    pub name: Node<'tree>,
    pub receiver: Option<String>,
    pub kind: ReferenceKind,
}

/// Record one call and its reference, or nothing if the callee names nothing.
pub(super) fn record(
    site: CallSite,
    source: &str,
    lang: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    // `split_call_target` applies both rungs — structural
    // (`is_anonymous_callable`) then lexical (`is_callee_identity`) — and
    // returning `None` is how a target that names no symbol is refused. Reading
    // the node's text here instead is exactly the mechanism SC26 patched shape
    // by shape and SC32 removed.
    let Some((callee_name, inner_receiver)) = split_call_target(site.name, source) else {
        return;
    };
    let receiver_expr = site.receiver.or(inner_receiver);
    let caller_symbol = enclosing_emitted_symbol(site.call, source, lang, file_symbol_name);
    references.push(ExtractedReference {
        name: callee_name.clone(),
        kind: site.kind,
        span: node_span(site.name),
        enclosing_symbol: caller_symbol.clone(),
        assigned_to: assigned_binding(site.call, source),
        // The mirrored call already carries the receiver; repeating it here would be a second copy of one fact.
        receiver_expr: None,
    });
    calls.push(ExtractedCall {
        caller_symbol,
        callee_name,
        receiver_expr,
        span: node_span(site.call),
    });
}

/// A receiver taken from a node, bounded, with `this` normalised.
pub(super) fn receiver_of(node: Node, source: &str, this_kind: &str) -> Option<String> {
    if node.kind() == this_kind {
        return this_receiver();
    }
    receiver_from(node, source)
}
