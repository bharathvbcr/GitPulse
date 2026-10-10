//! Ruby method visibility.
//!
//! Ruby has no visibility keyword on a declaration. `private` is a method call
//! that changes the default for the `def`s after it in the same class body,
//! `private def x` wraps one definition, and `private :x, :y` names methods
//! defined anywhere in the body. `generic_is_exported` read only the `def`
//! header, saw none of these, and called every method public — so a private
//! method nothing calls was exempt and never reported (measured 2026-10-09).

use tree_sitter::Node;

use crate::treesitter::{bounded_parent, get_node_text};

/// The method a `call` invokes, when it is written with no receiver.
fn bare_call_method(node: Node, source: &str) -> Option<String> {
    if node.kind() != "call" || node.child_by_field_name("receiver").is_some() {
        return None;
    }
    node.child_by_field_name("method")
        .map(|method| get_node_text(method, source))
}

fn restricts(word: &str) -> bool {
    matches!(word, "private" | "protected")
}

/// Whether a Ruby declaration can be called from outside its class.
pub(crate) fn is_exported(node: Node, source: &str, name: &str) -> bool {
    if node.kind() != "method" {
        return !name.starts_with('_');
    }
    let Some(parent) = bounded_parent(node) else {
        return true;
    };
    // `private def helper; end`
    if parent.kind() == "argument_list" {
        if let Some(call) = bounded_parent(parent) {
            if bare_call_method(call, source).is_some_and(|word| restricts(&word)) {
                return false;
            }
        }
    }
    if parent.kind() != "body_statement" {
        return true;
    }
    // A bare `private` / `protected` / `public` earlier in the same body sets
    // the default for this `def`; the nearest one wins.
    let mut sibling = node.prev_named_sibling();
    while let Some(previous) = sibling {
        if previous.kind() == "identifier" {
            let word = get_node_text(previous, source);
            if restricts(&word) {
                return false;
            }
            if word == "public" {
                break;
            }
        }
        sibling = previous.prev_named_sibling();
    }
    // `private :helper, :other` anywhere in the same body.
    let mut cursor = parent.walk();
    for statement in parent.named_children(&mut cursor) {
        if !bare_call_method(statement, source).is_some_and(|word| restricts(&word)) {
            continue;
        }
        let Some(arguments) = statement.child_by_field_name("arguments") else {
            continue;
        };
        let mut inner = arguments.walk();
        let names_this = arguments.named_children(&mut inner).any(|argument| {
            let text = get_node_text(argument, source);
            text.trim_start_matches(':')
                .trim_matches(|c| c == '"' || c == '\'')
                == name
        });
        if names_this {
            return false;
        }
    }
    true
}
