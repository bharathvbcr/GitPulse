//! Kotlin call extraction.
//!
//! Kotlin reached `extract_node`'s generic arm and produced declarations only —
//! 0 `Calls` edges under this port and under the Python implementation it
//! replaces. New capability, not a restored regression.
//!
//! # What the grammar makes easy, and what it does not
//!
//! `tree-sitter-kotlin-ng` gives `call_expression` **no fields at all**: the
//! callee is simply the first named child and the arguments follow. That single
//! shape absorbs most of Kotlin's surface for free — a safe call `a?.b()`, an
//! elvis-guarded `a?.b() ?: c()`, named and default arguments, a constructor
//! call with no `new` (`Person("x")`), a `suspend` call, `launch { }`,
//! `async { }` and every lambda-with-receiver block are all ordinary
//! `call_expression`s and need no arm of their own.
//!
//! Two shapes do need care, and one is a real trap:
//!
//! * **A trailing lambda applied to a call is a nested `call_expression`.**
//!   `withContext(Dispatchers.IO) { … }` parses as an outer `call_expression`
//!   whose *callee* is the inner `withContext(Dispatchers.IO)` call. Reading
//!   the outer node's callee text would record the whole inner call expression
//!   as a callee name — the SC26/SC32 defect exactly. The outer node is
//!   therefore skipped: the inner node is visited on its own and carries the
//!   edge, which is the same reasoning SC32 applied to Python curried calls.
//! * **`super.m()` is refused.** An `override fun onCreate()` calling
//!   `super.onCreate()` is the single most common shape in Android Kotlin, and
//!   recording callee `onCreate` would let same-file resolution bind it to the
//!   override itself at DETERMINISTIC confidence. That self-edge is wrong, and
//!   worse, it would make every lifecycle override appear called and so exempt
//!   from dead-code analysis.
//!
//! # Known limitation this module does not paper over
//!
//! An extension function *declaration* loses its receiver: `fun Person.extra()`
//! is emitted as `F::extra`, not `F::Person.extra`, because the emitter reads
//! the grammar's `name` field and that field holds only the bare name. That is
//! a declaration-side defect owned elsewhere. The call side here mirrors the
//! identity the emitter actually produced rather than the one it should have,
//! so calls inside an extension function join, and an extension *call*
//! (`"x".myExt()`) is recorded as a receiver-bearing call which resolves only
//! if a matching symbol exists — it is left unresolved rather than invented.

use super::jvm_dotnet::{receiver_of, record, CallSite};
use super::scope::receiver_from;
use crate::model::{ExtractedCall, ExtractedReference, ReferenceKind, ScopeReceiver};
use crate::treesitter::is_anonymous_callable;
use tree_sitter::Node;

pub(crate) fn extract_kotlin_call(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    if let Some(site) = call_site(node, source) {
        record(site, source, "kotlin", file_symbol_name, calls, references);
    }
}

fn call_site<'tree>(node: Node<'tree>, source: &str) -> Option<CallSite<'tree>> {
    match node.kind() {
        "call_expression" => {
            let callee = node.named_child(0)?;
            let (name, receiver) = callee_target(callee, source)?;
            Some(CallSite {
                call: node,
                name,
                receiver,
                kind: ReferenceKind::Call,
            })
        }
        // `vm::onEvent`, `Type::method`, `this::m`: the grammar parses a bound or
        // qualified callable reference as a navigation expression whose
        // separator is the anonymous `::` token. Only that token tells it from
        // the member access `vm.onEvent`, which is not a call and records
        // nothing here.
        "navigation_expression" if has_child_kind(node, "::") => {
            let count = node.named_child_count();
            if count < 2 {
                return None;
            }
            let name = node
                .named_child(count - 1)
                .filter(|name| name.kind() == "identifier")?;
            let target = node.named_child(0)?;
            let receiver = receiver_of(target, source, "this_expression");
            Some(CallSite {
                call: node,
                name,
                receiver,
                kind: ReferenceKind::Call,
            })
        }
        // `::helper`: an unbound reference to a function or a constructor.
        "callable_reference" => {
            let count = node.named_child_count();
            let name = node
                .named_child(count.checked_sub(1)?)
                .filter(|name| name.kind() == "identifier")?;
            let receiver = (count > 1)
                .then(|| node.named_child(0))
                .flatten()
                .and_then(|target| receiver_of(target, source, "this_expression"));
            Some(CallSite {
                call: node,
                name,
                receiver,
                kind: ReferenceKind::Call,
            })
        }
        // `castOrNull<Task>(x)`: tree-sitter-kotlin-ng reads an explicit type
        // argument list as two comparisons, `(castOrNull < Task) > (x)`. The
        // shape is unambiguous — a comparison of a boolean with a parenthesised
        // expression is not code anyone writes — so the callee is the left-most
        // operand and the call is recorded.
        "binary_expression" => {
            let right = node.child_by_field_name("right")?;
            let left = node.child_by_field_name("left")?;
            if right.kind() != "parenthesized_expression"
                || left.kind() != "binary_expression"
                || !has_child_kind(node, ">")
                || !has_child_kind(left, "<")
            {
                return None;
            }
            let (name, receiver) = callee_target(left.child_by_field_name("left")?, source)?;
            Some(CallSite {
                call: node,
                name,
                receiver,
                kind: ReferenceKind::Call,
            })
        }
        // `a to b`, `a shl b`, and any user-declared `infix fun`.
        //
        // Recorded deliberately. An infix call *is* a function call in Kotlin —
        // `infix fun Duration.every(n: Int)` is a declaration like any other,
        // and a repository that defines its own infix DSL would otherwise see
        // every one of those functions as uncalled, which is the dead-code
        // false positive this whole work exists to remove. The cost is bounded:
        // the callee is gated as an identifier like every other, and stdlib
        // infix functions such as `to` are extension functions reached through
        // a receiver, so an unresolved one classifies as an uninferred receiver
        // rather than landing in the tier reserved for defects.
        "infix_expression" => {
            // `left op right`, exactly three named children. Anything else is
            // not the shape this claims to read, so it is refused rather than
            // guessed at.
            if node.named_child_count() != 3 {
                return None;
            }
            let name = node.named_child(1).filter(|op| op.kind() == "identifier")?;
            Some(CallSite {
                call: node,
                name,
                receiver: node.named_child(0).and_then(|left| {
                    literal_receiver(left, source).or_else(|| receiver_from(left, source))
                }),
                kind: ReferenceKind::Call,
            })
        }
        _ => None,
    }
}

/// `(callee name node, receiver)` for whatever stands in a call's callee slot.
fn callee_target<'tree>(
    mut callee: Node<'tree>,
    source: &str,
) -> Option<(Node<'tree>, Option<String>)> {
    // The grammar binds prefix operators inside the callee slot. Unwrap them
    // without spending native stack space on repository-controlled nesting.
    while callee.kind() == "unary_expression" {
        if crate::treesitter::walk_deadline_passed() {
            return None;
        }
        callee = callee.named_child(callee.named_child_count().checked_sub(1)?)?;
    }
    match callee.kind() {
        "identifier" => Some((callee, None)),
        // `o.m()`, `a?.b()`, `obj?.deep?.call()`, `x.let { }`, `it.x()`.
        //
        // `?.` is an anonymous token in this grammar, so a safe call and a
        // plain one arrive with the same shape and neither needs its own arm.
        "navigation_expression" => {
            let count = callee.named_child_count();
            if count < 2 {
                return None;
            }
            let target = callee.named_child(0)?;
            let name = callee.named_child(count - 1)?;
            // `super.m()` is recorded with the receiver `super` so the resolver
            // can read it against the enclosing type's supertypes. Recording
            // the bare name instead let same-file resolution bind it to the
            // override itself; refusing it left the base method uncalled.
            if target.kind() == "super_expression" {
                return Some((name, Some("super".to_string())));
            }
            let receiver = literal_receiver(target, source)
                .or_else(|| receiver_of(target, source, "this_expression"));
            Some((name, receiver))
        }
        // A trailing lambda applied to a call, or a call of a call. The inner
        // node is visited in its own right and carries the edge.
        "call_expression" => None,
        // An immediately-invoked literal has no callee identity by
        // construction. `split_call_target` would refuse it too; refusing it
        // here keeps the reason at the shape that causes it.
        kind if is_anonymous_callable(kind) || kind == "lambda_literal" => None,
        _ => None,
    }
}

/// Whether `node` has a direct child of exactly this kind (an anonymous token).
fn has_child_kind(node: Node, kind: &str) -> bool {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).any(|child| child.kind() == kind);
    found
}

/// The type a literal receiver has, so `"a b".slugify()` and `1 every 3` are
/// receiver-bearing calls on `String` and `Int` and join to an extension
/// function declared on that type. A literal says its own type outright.
fn literal_receiver(node: Node, source: &str) -> Option<String> {
    let text = match node.kind() {
        // `(x as? Door).hello()`: the cast says what the receiver is.
        "parenthesized_expression" => return cast_type(node, source),
        "string_literal" => return Some("String".to_string()),
        "number_literal" => crate::treesitter::get_node_text(node, source),
        _ => return None,
    };
    let kind = if text.ends_with('L') {
        "Long"
    } else if text.starts_with("0x") || text.starts_with("0X") || text.starts_with("0b") {
        "Int"
    } else if text.contains('.') || text.ends_with(['f', 'F']) {
        "Double"
    } else {
        "Int"
    };
    Some(kind.to_string())
}

/// Every `x.apply { .. }`, `x.run { .. }` and `with(x) { .. }` lambda in the
/// file, with the written expression its `this` is.
///
/// Walked once with an explicit stack: the tree is repository-controlled and a
/// recursive walk would spend native stack on its depth.
pub(crate) fn scope_receivers(root: Node, source: &str) -> Vec<ScopeReceiver> {
    let mut found = Vec::new();
    let mut worklist = vec![root];
    while let Some(node) = worklist.pop() {
        if crate::treesitter::walk_deadline_passed() {
            break;
        }
        if node.kind() == "call_expression" {
            if let Some(scope) = scope_function_lambda(node, source) {
                found.push(scope);
            }
        }
        let mut cursor = node.walk();
        worklist.extend(node.named_children(&mut cursor));
    }
    found.sort_by_key(|scope| (scope.body_start, scope.body_end));
    found
}

fn scope_function_lambda(call: Node, source: &str) -> Option<ScopeReceiver> {
    let callee = call.named_child(0)?;
    let lambda = call
        .named_child(1)
        .filter(|node| node.kind() == "annotated_lambda")?
        .named_child(0)
        .filter(|node| node.kind() == "lambda_literal")?;
    let receiver = match callee.kind() {
        // `x.apply { }` / `x.run { }`
        "navigation_expression" if callee.named_child_count() == 2 => {
            let function = callee.named_child(1)?;
            if !matches!(
                crate::treesitter::get_node_text(function, source).as_str(),
                "apply" | "run"
            ) {
                return None;
            }
            receiver_from(callee.named_child(0)?, source)?
        }
        // `with(x) { }`
        "call_expression" => {
            let function = callee.named_child(0)?;
            if function.kind() != "identifier"
                || crate::treesitter::get_node_text(function, source) != "with"
            {
                return None;
            }
            let arguments = callee.named_child(1)?;
            if arguments.named_child_count() != 1 {
                return None;
            }
            receiver_from(arguments.named_child(0)?.named_child(0)?, source)?
        }
        _ => return None,
    };
    Some(ScopeReceiver {
        body_start: lambda.start_byte(),
        body_end: lambda.end_byte(),
        receiver,
    })
}

/// The type a parenthesised `as`/`as?` cast names: `Door` in `(x as? Door)`.
fn cast_type(parenthesized: Node, source: &str) -> Option<String> {
    let cast = parenthesized.named_child(0).filter(|inner| {
        inner.kind() == "as_expression" && parenthesized.named_child_count() == 1
    })?;
    let ty = cast.child_by_field_name("right")?;
    if ty.kind() != "user_type" {
        return None;
    }
    let mut cursor = ty.walk();
    let name = ty
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "identifier")
        .last()?;
    Some(crate::treesitter::get_node_text(name, source))
}
