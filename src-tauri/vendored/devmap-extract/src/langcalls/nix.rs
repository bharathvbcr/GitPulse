//! Function application in Nix.
//!
//! Nix reached `extract_node`'s generic arm, which recovered nothing:
//! `extract_file` on a realistic `flake.nix` produced **1 symbol** (the `File`
//! node) and **0 calls**. `langdecl::nix` supplies the symbols; this module
//! supplies the edges.
//!
//! # Nix does have a call graph, and it was worth measuring rather than
//! assuming
//!
//! Nix is a lazy expression language, so "does a call graph mean anything here"
//! is a fair question and the answer had to come from a count rather than from
//! intuition. On that `flake.nix`, of **10** `apply_expression` nodes:
//!
//! * **3** apply a lambda bound in the same file — `forAllSystems` twice and
//!   `mkShellFor` once — and each is an intra-file edge between two things the
//!   graph can name;
//! * the rest apply something outside the file (`builtins.substring`,
//!   `nixpkgs.lib.genAttrs`, `pkgs.mkShell`,
//!   `pkgs.rustPlatform.buildRustPackage`), which is an unresolved callee,
//!   exactly as `console.log` is in JavaScript.
//!
//! That ratio is what earns the module. It is also why HCL was declined in the
//! same pass: Terraform has no user-defined function syntax at all, so every
//! `function_call` in a `.tf` file names a built-in and **no** edge could ever
//! resolve.
//!
//! # Currying means one call is several nodes, and only one carries the name
//!
//! `builtins.substring 0 8 self.lastModifiedDate` is three nested
//! `apply_expression`s: the innermost applies `builtins.substring` to `0`, and
//! each outer one applies the *result* to the next argument. Only the innermost
//! has a callee with a name; the outer ones have an `apply_expression` in the
//! function position and are refused. The edge is therefore recorded exactly
//! once per call, from the node that names it — the same rule Lua's `f()()`
//! already follows.

use tree_sitter::Node;

use crate::model::{ExtractedCall, ExtractedReference, ReferenceKind};
use crate::treesitter::{node_span, split_call_target};

use super::scope::{enclosing_emitted_symbol, receiver_from};

/// Record the application `node` performs, if it performs one.
pub fn extract_nix_call(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    if node.kind() != "apply_expression" {
        return;
    }
    let Some(function) = node.child_by_field_name("function") else {
        return;
    };
    let Some((name_node, receiver)) = applied_target(function, source) else {
        return;
    };
    let Some((callee_name, inner_receiver)) = split_call_target(name_node, source) else {
        return;
    };
    let caller_symbol = enclosing_emitted_symbol(node, source, "nix", file_symbol_name);
    references.push(ExtractedReference {
        name: callee_name.clone(),
        kind: ReferenceKind::Call,
        span: node_span(name_node),
        enclosing_symbol: caller_symbol.clone(),
        // Nix binds values and declares no types, so `x = f y;` proves only
        // that `x` holds whatever `f` returned. There is nothing for receiver
        // inference to look up afterwards.
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

/// `(the node naming the applied function, the receiver it is reached
/// through)`, or `None` when the function position names nothing.
fn applied_target<'tree>(
    function: Node<'tree>,
    source: &str,
) -> Option<(Node<'tree>, Option<String>)> {
    match function.kind() {
        // `helper 41`
        "variable_expression" => Some((function.child_by_field_name("name")?, None)),
        // `pkgs.mkShell {…}`, `pkgs.rustPlatform.buildRustPackage {…}`.
        //
        // The last attribute is the callee and the segment before it is the
        // receiver, because a receiver is looked up as a *variable name* and
        // `pkgs.rustPlatform` is not one. Same reduction C# applies to a
        // qualified `member_access_expression`.
        "select_expression" => {
            let attrpath = function.child_by_field_name("attrpath")?;
            let last = attrpath.named_child(attrpath.named_child_count().checked_sub(1)?)?;
            let receiver = match attrpath.named_child_count() {
                0 | 1 => function
                    .child_by_field_name("expression")
                    .and_then(|head| receiver_from(head, source)),
                count => attrpath
                    .named_child(count - 2)
                    .and_then(|qualifier| receiver_from(qualifier, source)),
            };
            Some((last, receiver))
        }
        // A curried application (`f a b`), an immediately-applied lambda
        // (`(x: x) 1`), or an application of a parenthesised expression: none
        // of these names a function, and the one that does — the innermost
        // link of a curried chain — is its own node and is visited separately,
        // so refusing here drops no edge.
        _ => None,
    }
}
