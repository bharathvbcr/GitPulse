//! Ruby call extraction.
//!
//! Ruby reached `extract_node`'s generic arm, which emits declarations only, so
//! **not one Ruby call was extracted** — reproduced on a two-function file
//! before this module existed, and confirmed against the Python implementation
//! this port replaces, which recovers the same call. That makes it a migration
//! regression rather than a shared gap (SC34).
//!
//! Every invocation in Ruby is one node kind. `helper(x)`, `puts "hi"` with no
//! parentheses, `obj.method`, `self.inner`, `obj&.maybe`, `Foo::Bar.baz(1)`,
//! `attr_accessor :name` and `Widget.new` are all a `call` carrying a `method`
//! field, differing only in which of `receiver`, `operator`, `arguments` and
//! `block` are present. Blocks need no handling of their own: a call written
//! inside `{ … }` or `do … end` is its own `call` node and the walk reaches it.
//!
//! Shapes deliberately **not** claimed, because the grammar gives them no
//! callee identity: `super`, `yield` and a bare parenthesis-less receiverless
//! send (`x = compute`, which parses as a plain `identifier` and is
//! indistinguishable from a local variable read without a symbol table).
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
use crate::treesitter::{
    get_child_text, get_node_text, is_callee_identity, node_span, split_call_target,
};

/// Calls made by Ruby code.
/// The grammar key this module answers for, so caller attribution asks
/// `langdecl` the same question the declaration emitter asks.
const LANG: &str = "ruby";

pub(crate) fn extract_ruby_calls(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    if node.kind() != "call" {
        return;
    }
    // An attribute write (`obj.name = x`) sends `name=`, and the grammar gives
    // it the *reader's* node: a `call` whose method is `name`, sitting in the
    // `left` field of an assignment. Recording it would point a write at the
    // reader `def name` — a confidently wrong edge of exactly the SC9 shape —
    // so the writer is refused rather than renamed.
    if is_assignment_target(node) {
        return;
    }
    let Some(method) = node.child_by_field_name("method") else {
        return;
    };
    let Some(callee_name) = ruby_callee_name(method, source) else {
        return;
    };
    let receiver = node.child_by_field_name("receiver");
    let receiver_expr = receiver.and_then(|receiver| ruby_receiver_expr(receiver, source));
    let caller_symbol = enclosing_emitted_symbol(node, source, LANG, file_symbol_name);
    let assigned_to = ruby_assigned_binding(node, source);

    // `Widget.new` is Ruby's constructor — the language has no `new` operator,
    // so the only evidence that a value is a `Widget` is this send. Recorded as
    // a `Constructor` reference naming the *class*, which is the shape the
    // resolver already reads to bind `w` to `Widget`; recording it as an
    // ordinary call to `new` would leave every Ruby receiver untyped.
    //
    // Gated on the receiver being a constant, because `x.new` on a lowercase
    // receiver names a local variable and proves nothing about a type.
    let constructed = (callee_name == "new")
        .then_some(receiver)
        .flatten()
        .filter(|receiver| matches!(receiver.kind(), "constant" | "scope_resolution"))
        .and_then(|receiver| ruby_receiver_expr(receiver, source));

    references.push(match constructed {
        Some(class_name) => ExtractedReference {
            name: class_name,
            kind: ReferenceKind::Constructor,
            span: node_span(node),
            enclosing_symbol: caller_symbol.clone(),
            assigned_to: assigned_to.clone(),
            // The mirrored call already carries the receiver; repeating it here would be a second copy of one fact.
            receiver_expr: None,
        },
        None => ExtractedReference {
            name: callee_name.clone(),
            kind: ReferenceKind::Call,
            span: node_span(method),
            enclosing_symbol: caller_symbol.clone(),
            assigned_to: assigned_to.clone(),
            // The mirrored call already carries the receiver; repeating it here would be a second copy of one fact.
            receiver_expr: None,
        },
    });
    calls.push(ExtractedCall {
        caller_symbol,
        callee_name,
        receiver_expr,
        span: node_span(node),
    });
}

/// The name this send names, or `None` when it names no identifier.
///
/// The splitter owns the answer, as it does for every other grammar, with one
/// addition Ruby needs and only Ruby has: a method name may end in `?` or `!`,
/// and `is_callee_identity` admits neither. That is not a case of the SC26
/// whole-expression defect — the *symbol* emitter reads the same identifier
/// token and names the declaration `ok?`, so refusing the callee would drop
/// every predicate and bang call (`empty?`, `nil?`, `save!`) while its
/// declaration sat in the graph with no inbound edge. The core is still put
/// through `is_callee_identity`, so only that one trailing character is new.
///
/// The structural rung comes first and is what excludes an operator send
/// (`a.+(b)`), whose method node is an `operator` rather than an identifier.
fn ruby_callee_name(method: Node, source: &str) -> Option<String> {
    if let Some((name, _)) = split_call_target(method, source) {
        return Some(name);
    }
    if !matches!(method.kind(), "identifier" | "constant") {
        return None;
    }
    let text = get_node_text(method, source);
    let core = text.strip_suffix(['?', '!']).unwrap_or(text.as_str());
    is_callee_identity(core).then_some(text)
}

/// Whether this node is the target being written to, rather than a value.
fn is_assignment_target(node: Node) -> bool {
    node.parent().is_some_and(|parent| {
        matches!(parent.kind(), "assignment" | "operator_assignment")
            && parent
                .child_by_field_name("left")
                .is_some_and(|left| left.id() == node.id())
    })
}

/// The receiver expression a call dispatches on.
///
/// `Foo::Bar.baz` and `::Kernel.puts` are reduced to the innermost constant.
/// The bare name is the dispatch key — it is what the symbol index and the
/// type-method map are keyed by — so keeping `Foo::Bar` would name a type no
/// lookup can find, which is the whole-expression defect SC26 closed for other
/// grammars. Every other receiver keeps its source text, exactly as the
/// JavaScript member-expression split already does, so `@client`, `[1,2]` and
/// `self` are preserved rather than guessed at.
fn ruby_receiver_expr(receiver: Node, source: &str) -> Option<String> {
    let text = match receiver.kind() {
        "scope_resolution" => get_child_text(receiver, "name", source)?,
        _ => get_node_text(receiver, source),
    };
    clamp_receiver(&text)
}

/// The local name receiving this call's value, when the grammar proves one.
///
/// Only the single-target form counts: `a, b = Foo.new, Bar.new` says nothing
/// about which target receives which value, and a guess would bind a real name
/// to the wrong type — the SC9 class of confidently-wrong edge.
fn ruby_assigned_binding(node: Node, source: &str) -> Option<String> {
    let parent = node.parent()?;
    if parent.kind() != "assignment" {
        return None;
    }
    let left = parent.child_by_field_name("left")?;
    if left.kind() != "identifier" {
        return None;
    }
    let name = get_node_text(left, source);
    (!name.is_empty()).then_some(name)
}
