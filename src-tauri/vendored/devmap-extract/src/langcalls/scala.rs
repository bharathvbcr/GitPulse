//! Scala call extraction (SC34).
//!
//! Scala reached `extract_node`'s generic arm, which emits declarations and
//! nothing else, so a Scala file produced `Contains` edges and **zero** `Calls`
//! — verified on a two-function file before this module existed, against a
//! Python implementation that recovers those calls with a regex.
//!
//! tree-sitter-scala is kinder than the Swift grammar here: `call_expression`
//! carries a `function` field, and a method call puts a `field_expression`
//! there, which `split_call_target` already splits into name and receiver. Two
//! shapes need their own handling:
//!
//! * `new Service()` is an `instance_expression`, whose type is an unnamed child
//!   and may be qualified (`new pkg.Other(1)`) or generic.
//! * Infix notation (`words foreach println`) is a method call written without a
//!   dot, and is the only call shape in the language with no parentheses to give
//!   it away. See `scala_infix_call` for what is and is not recorded.
//!
//! Deliberately *not* recorded: a paren-less field access such as `p.toString`.
//! Scala makes no syntactic distinction between reading a field and invoking a
//! nullary method, so recording every `field_expression` as a call would put a
//! call edge on every case-class accessor in the corpus. The frozen Python
//! baseline does not record them either — its regex requires a `(` — so this
//! costs no measured parity.

use tree_sitter::Node;

use crate::model::{ExtractedCall, ExtractedReference, ReferenceKind};
use crate::treesitter::{
    get_node_text, is_anonymous_callable, is_callee_identity, node_span, split_call_target,
};

use super::scope::{clamp_receiver, enclosing_emitted_symbol, receiver_from};

/// Calls made by Scala code.
/// The grammar key this module answers for, so caller attribution asks
/// `langdecl` the same question the declaration emitter asks.
const LANG: &str = "scala";

pub fn extract_scala_call(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    let Some(call) = scala_call_identity(node, source) else {
        return;
    };
    if !is_callee_identity(&call.callee_name) {
        return;
    }
    let enclosing = enclosing_emitted_symbol(node, source, LANG, file_symbol_name);
    references.push(ExtractedReference {
        name: call.callee_name.clone(),
        kind: call.kind,
        span: node_span(call.name_node),
        enclosing_symbol: enclosing.clone(),
        assigned_to: scala_assignment_binding(node, source),
        // The mirrored call already carries the receiver; repeating it here would be a second copy of one fact.
        receiver_expr: None,
    });
    calls.push(ExtractedCall {
        caller_symbol: enclosing,
        callee_name: call.callee_name,
        receiver_expr: call.receiver_expr,
        span: node_span(node),
    });
}

struct ScalaCall<'tree> {
    callee_name: String,
    receiver_expr: Option<String>,
    name_node: Node<'tree>,
    kind: ReferenceKind,
}

fn scala_call_identity<'tree>(node: Node<'tree>, source: &str) -> Option<ScalaCall<'tree>> {
    match node.kind() {
        // `helper(1)`, `obj.method(x)`, `go[Int](3)`, and `apply` sugar —
        // `Registry("named")` and the case-class construction `Point(1, 2)` are
        // both this shape with an identifier in `function`, and both name a
        // symbol the emitter produces, so both resolve.
        "call_expression" => {
            let function = node.child_by_field_name("function")?;
            if is_anonymous_callable(function.kind()) || function.kind() == "lambda_expression" {
                return None;
            }
            let (callee_name, receiver) = split_call_target(function, source)?;
            Some(ScalaCall {
                callee_name,
                receiver_expr: receiver.as_deref().and_then(clamp_receiver),
                name_node: function,
                kind: ReferenceKind::Call,
            })
        }
        "instance_expression" => scala_instance_call(node, source),
        "infix_expression" => scala_infix_call(node, source),
        _ => None,
    }
}

/// `new Service()`, `new pkg.Other(1)`, `new Box[Int]()`.
fn scala_instance_call<'tree>(node: Node<'tree>, source: &str) -> Option<ScalaCall<'tree>> {
    let mut cursor = node.walk();
    let type_node = scala_constructed_type(node, &mut cursor)?;
    let (callee_name, receiver_expr) = scala_type_identity(type_node, source)?;
    Some(ScalaCall {
        callee_name,
        receiver_expr,
        name_node: type_node,
        kind: ReferenceKind::Constructor,
    })
}

/// The type subtree of an `instance_expression`, which the grammar leaves
/// unnamed alongside the `arguments` field.
fn scala_constructed_type<'tree>(
    node: Node<'tree>,
    cursor: &mut tree_sitter::TreeCursor<'tree>,
) -> Option<Node<'tree>> {
    for child in node.named_children(cursor) {
        if !matches!(child.kind(), "arguments" | "comment" | "block_comment") {
            return Some(child);
        }
    }
    None
}

/// The type a `new` expression constructs, and the package or object
/// qualifying it.
fn scala_type_identity(node: Node, source: &str) -> Option<(String, Option<String>)> {
    match node.kind() {
        "type_identifier" => {
            let name = get_node_text(node, source);
            is_callee_identity(&name).then_some((name, None))
        }
        // `pkg.Other` — the trailing `type_identifier` is the type, everything
        // before it qualifies the type, exactly as a receiver does.
        "stable_type_identifier" => {
            let mut names: Vec<String> = Vec::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if matches!(child.kind(), "type_identifier" | "identifier") {
                    names.push(get_node_text(child, source));
                }
            }
            let name = names.pop()?;
            if !is_callee_identity(&name) {
                return None;
            }
            Some((name, names.pop().filter(|q| is_callee_identity(q))))
        }
        // `new Box[Int]()`. The type arguments are not part of the identity.
        "generic_type" => node
            .child_by_field_name("type")
            .and_then(|inner| scala_type_identity(inner, source)),
        _ => None,
    }
}

/// Infix notation: `words foreach println`, `Runner go h`, `a + b`.
///
/// **Alphabetic operators are recorded; symbolic ones are not.** Every infix
/// operator in Scala is a method on the left operand, so `words foreach println`
/// is `words.foreach(println)` and is exactly the kind of edge a call graph
/// exists to carry — paren-less infix is idiomatic in collection code, Akka and
/// every test DSL, and the Python baseline's regex misses all of it.
///
/// A symbolic operator (`+`, `::`, `<-`, `!`) is a different question. Its name
/// is not identifier-shaped, so `is_callee_identity` would reject it anyway; the
/// point of excluding it deliberately is that recording it *with* some escaped
/// spelling would add one row per arithmetic and collection operator in the
/// corpus — overwhelmingly `Int.+` and `List.::` from the standard library,
/// which no indexed file declares. That is volume without a resolvable target,
/// and it would swamp the very tiers SC30 built to make unresolved rows
/// readable. A repository that defines its own `def +` loses that call; the
/// trade is deliberate and measured on the fixture, not assumed.
fn scala_infix_call<'tree>(node: Node<'tree>, source: &str) -> Option<ScalaCall<'tree>> {
    let operator = node.child_by_field_name("operator")?;
    if operator.kind() != "identifier" {
        return None;
    }
    let (callee_name, _) = split_call_target(operator, source)?;
    let receiver_expr = node
        .child_by_field_name("left")
        .and_then(|left| receiver_from(left, source));
    Some(ScalaCall {
        callee_name,
        receiver_expr,
        name_node: operator,
        kind: ReferenceKind::Call,
    })
}

/// The local value a call's result is bound to, when the grammar proves one.
///
/// Feeds the resolver's receiver-type map, so `val s = new Service()` followed
/// by `s.start()` resolves by type instead of by name. Bounded, and stopping at
/// argument and block boundaries so `outer(Inner())` cannot bind the inner call
/// to the outer name.
fn scala_assignment_binding(node: Node, source: &str) -> Option<String> {
    let mut current = node;
    for _ in 0..4 {
        let parent = current.parent()?;
        match parent.kind() {
            "val_definition" | "var_definition" => {
                let value = parent.child_by_field_name("value")?;
                if value.id() != current.id() {
                    return None;
                }
                let pattern = parent.child_by_field_name("pattern")?;
                if pattern.kind() != "identifier" {
                    return None;
                }
                let name = get_node_text(pattern, source);
                return is_callee_identity(&name).then_some(name);
            }
            "assignment_expression" => {
                let right = parent.child_by_field_name("right")?;
                if right.id() != current.id() {
                    return None;
                }
                let left = parent.child_by_field_name("left")?;
                if left.kind() != "identifier" {
                    return None;
                }
                let name = get_node_text(left, source);
                return is_callee_identity(&name).then_some(name);
            }
            "arguments" | "block" | "template_body" | "lambda_expression" => return None,
            _ => current = parent,
        }
    }
    None
}
