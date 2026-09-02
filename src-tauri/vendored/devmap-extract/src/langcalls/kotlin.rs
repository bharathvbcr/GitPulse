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
use crate::model::{ExtractedCall, ExtractedReference, ReferenceKind};
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
                receiver: node
                    .named_child(0)
                    .and_then(|left| receiver_from(left, source)),
                kind: ReferenceKind::Call,
            })
        }
        _ => None,
    }
}

/// `(callee name node, receiver)` for whatever stands in a call's callee slot.
fn callee_target<'tree>(
    callee: Node<'tree>,
    source: &str,
) -> Option<(Node<'tree>, Option<String>)> {
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
            if target.kind() == "super_expression" {
                return None;
            }
            let name = callee.named_child(count - 1)?;
            let receiver = receiver_of(target, source, "this_expression");
            Some((name, receiver))
        }
        // A trailing lambda applied to a call, or a call of a call. The inner
        // node is visited in its own right and carries the edge.
        "call_expression" => None,
        // `!h()` and `-g()`. tree-sitter-kotlin-ng binds the prefix operator
        // *before* the argument list, so the callee of the `call_expression` is
        // the `unary_expression` `!h` — not the identifier the call actually
        // names. Refusing it dropped the call entirely: measured on a 1,032-file
        // Android corpus, `if (!markerPromoted(k))` produced **no call edge and
        // no unresolved row**, and the function it calls was reported dead at
        // 0.9 confidence — a proposal to delete working code, from a call the
        // graph never saw. A postfix `a!!.b()` is a `navigation_expression` and
        // does not arrive here.
        "unary_expression" => {
            let operand = callee.named_child(callee.named_child_count().checked_sub(1)?)?;
            callee_target(operand, source)
        }
        // An immediately-invoked literal has no callee identity by
        // construction. `split_call_target` would refuse it too; refusing it
        // here keeps the reason at the shape that causes it.
        kind if is_anonymous_callable(kind) || kind == "lambda_literal" => None,
        _ => None,
    }
}
