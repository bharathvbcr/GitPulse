//! PHP call extraction.
//!
//! PHP reached `extract_node`'s generic arm, which emits declarations only, so
//! **not one PHP call was extracted** — reproduced on a two-function file
//! before this module existed, against a Python implementation that recovers
//! that call. A migration regression, not a shared gap (SC34).
//!
//! PHP spells invocation four ways and the grammar gives each its own node:
//! `helper()` and `\App\Deep\fn_call()` are `function_call_expression`,
//! `$obj->run()` is `member_call_expression`, `$obj?->run()` is
//! `nullsafe_member_call_expression`, and `Widget::make()`, `self::stat()`,
//! `static::other()`, `parent::base()` are all `scoped_call_expression`.
//! `new Widget()` is `object_creation_expression`, which — unlike the other
//! four — labels none of its children with a field, so the class is read
//! positionally.
//!
//! **Dynamic targets are refused rather than guessed at.** `$cb()`,
//! `$arr['k']()`, `$obj->$m()`, `Foo::$m()` and `new $cls()` name a callee only
//! at run time. `is_callee_identity` admits a leading `$` on purpose — a
//! JavaScript private method is declared and called as `#name`, and `$` rides
//! the same rule — so `$cb` would pass the lexical gate and be recorded as a
//! callee named `$cb` that no symbol can ever carry. The refusal here is
//! structural, on the node kind, which is the rung `is_anonymous_callable`
//! occupies for inline function literals (SC32).
//!
//! Caller attribution comes from `super::scope`, not from
//! `enclosing_callable_qualified`. A call edge's `caller_symbol` is a join key,
//! so it has to be the identity the *symbol emitter* gives the enclosing
//! declaration, and these two languages reach the emitter through the generic
//! declaration arm. The shared helper mirrors that arm's own tables; the
//! per-language disagreements it exists to avoid are listed there.

use tree_sitter::Node;

use super::scope::{clamp_receiver, enclosing_emitted_symbol};
use crate::model::{ExtractedCall, ExtractedReference, ReferenceKind};
use crate::treesitter::{get_node_text, node_span, split_call_target};

/// Calls made by PHP code.
/// The grammar key this module answers for, so caller attribution asks
/// `langdecl` the same question the declaration emitter asks.
const LANG: &str = "php";

pub(crate) fn extract_php_calls(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    let (callee_name, receiver_expr, target, reference_kind) = match node.kind() {
        "function_call_expression" => {
            let Some(function) = node.child_by_field_name("function") else {
                return;
            };
            let Some((name_node, namespace)) = php_callee_name_node(function, source) else {
                return;
            };
            let Some((callee, _)) = split_call_target(name_node, source) else {
                return;
            };
            (callee, namespace, name_node, ReferenceKind::Call)
        }
        "member_call_expression" | "nullsafe_member_call_expression" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return;
            };
            // `$obj->$method()` picks its callee at run time.
            if name_node.kind() != "name" {
                return;
            }
            let Some((callee, _)) = split_call_target(name_node, source) else {
                return;
            };
            let receiver = node
                .child_by_field_name("object")
                .and_then(|object| clamp_receiver(&get_node_text(object, source)));
            (callee, receiver, name_node, ReferenceKind::Call)
        }
        "scoped_call_expression" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return;
            };
            if name_node.kind() != "name" {
                return;
            }
            let Some((callee, _)) = split_call_target(name_node, source) else {
                return;
            };
            let receiver = node
                .child_by_field_name("scope")
                .and_then(|scope| php_scope_expr(scope, source));
            (callee, receiver, name_node, ReferenceKind::Call)
        }
        // `new Widget(3)` constructs `Widget`. This node labels no child, so
        // the class is the first named child; `new $cls()` and `new class {…}`
        // put a `variable_name` or an anonymous class body there and are
        // refused, the way SC17 refuses a Go composite literal over a
        // predeclared type — a callee that can never resolve is worse than none.
        "object_creation_expression" => {
            let Some(constructed) = node.named_child(0) else {
                return;
            };
            let Some((name_node, namespace)) = php_callee_name_node(constructed, source) else {
                return;
            };
            let Some((callee, _)) = split_call_target(name_node, source) else {
                return;
            };
            (callee, namespace, name_node, ReferenceKind::Constructor)
        }
        _ => return,
    };

    let caller_symbol = enclosing_emitted_symbol(node, source, LANG, file_symbol_name);
    references.push(ExtractedReference {
        name: callee_name.clone(),
        kind: reference_kind,
        span: node_span(target),
        enclosing_symbol: caller_symbol.clone(),
        assigned_to: php_assigned_binding(node, source),
        // The mirrored call already carries the receiver; repeating it here would be a second copy of one fact.
        receiver_expr: None,
    });
    calls.push(ExtractedCall {
        caller_symbol,
        callee_name,
        receiver_expr,
        span: node_span(node),
    });
}

/// The identifier a call target names, plus the namespace qualifying it.
///
/// A `qualified_name` (`\App\Deep\fn_call`) is the PHP shape of the path call
/// SC26 fixed for Rust: passed whole to the splitter its text carries `\`, so
/// `is_callee_identity` rejects it and the edge is lost. Split the same way
/// `scoped_identifier` is — the trailing identifier is the callee, the prefix
/// becomes the receiver, where import evidence can still see it.
///
/// Anything else — `variable_name`, `subscript_expression`, a parenthesized
/// expression — names its callee only at run time and yields `None`.
fn php_callee_name_node<'tree>(
    target: Node<'tree>,
    source: &str,
) -> Option<(Node<'tree>, Option<String>)> {
    match target.kind() {
        "name" => Some((target, None)),
        "qualified_name" => {
            let name_node = (0..target.named_child_count())
                .filter_map(|index| target.named_child(index))
                .next_back()
                .filter(|last| last.kind() == "name")?;
            // The grammar labels every leading token `prefix`, separators
            // included, so `child_by_field_name` hands back the `\` rather
            // than the namespace. The namespace is the one child that is a
            // `namespace_name`, and a call qualified only by a leading `\`
            // (`\strlen()`) has none, which is the right answer: the root
            // namespace qualifies nothing.
            let namespace = (0..target.named_child_count())
                .filter_map(|index| target.named_child(index))
                .find(|child| child.kind() == "namespace_name")
                .map(|prefix| get_node_text(prefix, source))
                .filter(|text| !text.is_empty());
            Some((name_node, namespace))
        }
        _ => None,
    }
}

/// The receiver a `Class::method()` dispatches on.
///
/// A qualified scope is reduced to its bare class name, which is the opposite
/// of what `php_callee_name_node` does with the same node kind — and
/// deliberately so, because the two answer different questions (SC17, SC25).
/// There the prefix is a *namespace* qualifying a free function and is
/// provenance; here the whole path names a *class*, and the bare name is the
/// dispatch key every symbol and type-method lookup is stored under.
fn php_scope_expr(scope: Node, source: &str) -> Option<String> {
    let text = match scope.kind() {
        "qualified_name" => (0..scope.named_child_count())
            .filter_map(|index| scope.named_child(index))
            .next_back()
            .filter(|last| last.kind() == "name")
            .map(|last| get_node_text(last, source))?,
        _ => get_node_text(scope, source),
    };
    clamp_receiver(&text)
}

/// The variable receiving this call's value, when the grammar proves one.
fn php_assigned_binding(node: Node, source: &str) -> Option<String> {
    let parent = node.parent()?;
    if parent.kind() != "assignment_expression" {
        return None;
    }
    let left = parent.child_by_field_name("left")?;
    if left.kind() != "variable_name" {
        return None;
    }
    let name = get_node_text(left, source);
    (!name.is_empty()).then_some(name)
}
