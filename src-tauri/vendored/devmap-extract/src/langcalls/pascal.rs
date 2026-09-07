//! Calls made by Pascal, Delphi and Free Pascal code.
//!
//! Pascal reached `extract_node`'s generic arm and recovered nothing from it:
//! measured on a unit with `function Helper` and `procedure Run` calling it,
//! the extractor produced **1 symbol** (the `File` node), **0 calls** and eight
//! references, all attributed to the file because no Pascal declaration existed
//! to attribute them to. `langdecl::pascal` supplies the symbols; this module
//! supplies the edges.
//!
//! # A call needs no parentheses
//!
//! `P;` and `P()` are the same call in Pascal, and `tree-sitter-pascal` parses
//! them into two different shapes: `P()` is an `exprCall`, while `P;` is a bare
//! `(statement (identifier))` with no call node anywhere in it. Handling only
//! `exprCall` would drop every parameterless call in the language, which is not
//! a corner case — it is how Pascal is written, and `inherited Create;`,
//! `Free;` and `Result := …; Cleanup;` are all of that shape.
//!
//! A statement whose entire content is one identifier can be nothing else. Bare
//! identifiers appear in expressions constantly, so the rule is keyed on the
//! `statement` node and requires the identifier to be its only child; anything
//! richer reaches the `exprCall` arm or is refused.
//!
//! # What is deliberately not recorded
//!
//! `inherited Create;` names the *parent* type's routine, and the call site
//! never says which type that is. Recording callee `Create` would let same-file
//! resolution bind it to the very method the statement appears in, at full
//! confidence — a self-loop that is not merely unresolved but wrong, and the
//! same refusal Java's `super.` and C#'s `base.` already make.

use tree_sitter::Node;

use crate::model::{ExtractedCall, ExtractedReference, ReferenceKind};
use crate::treesitter::{node_span, split_call_target};

use super::scope::{enclosing_emitted_symbol, receiver_from};

/// Record the call `node` makes, if it makes one.
pub fn extract_pascal_call(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    let Some((name_node, receiver)) = call_target(node, source) else {
        return;
    };
    // The one place a callee name is built, so an expression can never be
    // recorded as a callee (SC26/SC32).
    let Some((callee_name, inner_receiver)) = split_call_target(name_node, source) else {
        return;
    };
    let caller_symbol = enclosing_emitted_symbol(node, source, "pascal", file_symbol_name);
    references.push(ExtractedReference {
        name: callee_name.clone(),
        kind: ReferenceKind::Call,
        span: node_span(name_node),
        enclosing_symbol: caller_symbol.clone(),
        // A Pascal variable is declared with its type in a `declVars` block,
        // never inferred from the call it is assigned from, so the receiver a
        // binding would carry is already stated where the variable is declared
        // and repeating a weaker guess here would add nothing.
        assigned_to: None,
        // The mirrored call already carries the receiver; repeating it here would be a second copy of one fact.
        receiver_expr: None,
    });
    calls.push(ExtractedCall {
        caller_symbol,
        callee_name,
        receiver_expr: receiver.or(inner_receiver),
        span: node_span(node),
    });
}

/// `(the node naming the callee, the receiver it is reached through)`, or
/// `None` when `node` is not a call.
fn call_target<'tree>(node: Node<'tree>, source: &str) -> Option<(Node<'tree>, Option<String>)> {
    match node.kind() {
        // `Helper(2)`, `W.Calc(A)`, `Format('%d', [1])`.
        "exprCall" => {
            let entity = node.child_by_field_name("entity")?;
            match entity.kind() {
                // `W.Calc(A)`, `Self.Calc(A)`, `A.B.Calc(1)`.
                //
                // `Self` is passed through unchanged: the resolver already
                // treats it as a self-receiver alongside `self`, `this` and
                // `$this`, so normalising it here would only hide it from a
                // table that is expecting it.
                "exprDot" => {
                    let name = entity.child_by_field_name("rhs")?;
                    let receiver = entity
                        .child_by_field_name("lhs")
                        .and_then(|lhs| dotted_receiver(lhs, source));
                    Some((name, receiver))
                }
                _ => Some((entity, None)),
            }
        }
        // `P;` — a statement that is exactly one identifier.
        "statement" => {
            let only = node
                .named_child(0)
                .filter(|_| node.named_child_count() == 1)?;
            (only.kind() == "identifier").then_some((only, None))
        }
        _ => None,
    }
}

/// The receiver a dotted qualifier contributes.
///
/// `A.B.Calc(1)` is reached through `B`, not through `A.B`: a receiver is
/// looked up as a *variable name*, so the whole path matches no binding, while
/// the last segment is the one a `declVar` can actually have declared. Same
/// reduction C# applies to a qualified `member_access_expression`.
fn dotted_receiver(lhs: Node, source: &str) -> Option<String> {
    match lhs.kind() {
        "exprDot" => lhs
            .child_by_field_name("rhs")
            .and_then(|rhs| receiver_from(rhs, source)),
        _ => receiver_from(lhs, source),
    }
}
