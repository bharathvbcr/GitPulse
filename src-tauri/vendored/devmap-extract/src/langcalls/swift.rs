//! Swift call extraction (SC34).
//!
//! Swift reached `extract_node`'s generic arm, which emits declarations and
//! nothing else, so a Swift file produced `Contains` edges and **zero** `Calls`
//! — verified on a two-function file before this module existed. The Python
//! implementation this port replaces recovers Swift calls with a regex, so the
//! blackout was a migration regression rather than a shared gap.
//!
//! The shapes come from tree-sitter-swift's own parse trees rather than from
//! assumption, because several of them are not what the surface syntax suggests:
//!
//! * A call is `call_expression` with **no** `function` field — the callee is
//!   the first named child and the arguments are a sibling `call_suffix`. A
//!   trailing closure lives in that same suffix, so `items.map { … }` and
//!   `run(a) { … } onError: { … }` need no separate handling at all.
//! * `obj.method()` puts a `navigation_expression` in callee position, which
//!   `split_call_target` does not know; the name hangs off `suffix.suffix`.
//!   Optional chaining and force-unwrap change only the *target* subtree, never
//!   the suffix, so `a?.b()` and `a!.b()` fall out of the same path.
//! * `try`, `try?`, `try!` and `await` **wrap** the call rather than standing
//!   between it and its callee, so the callee of `try await foo()` is `foo`.
//!   That is the SC26 shape (`await invoke<Raw>('x')` recorded `await invoke`)
//!   and it is pinned by a test.
//! * `Set<Int>()` is a `constructor_expression`, a different node kind from
//!   `Person(name:)`, which is an ordinary `call_expression`.
//!
//! Everything that names no identifier is dropped rather than recorded as text:
//! `split_call_target` returns `Option` and `is_callee_identity` rejects
//! expressions, which is the SC26/SC32 rule. A call whose name has no spelling
//! at all — the zero-width `custom_operator` the grammar inserts when it
//! recovers `x as? String ?? "d"` — is refused at the same gate and counted in
//! one per-file diagnostic, because the file stays `Clean` and would otherwise
//! carry no trace of it.

use tree_sitter::Node;

use crate::model::{ExtractedCall, ExtractedReference, ReferenceKind};
use crate::treesitter::{
    get_node_text, is_anonymous_callable, is_callee_identity, node_span,
    note_swift_unspelled_operator, split_call_target,
};

use super::scope::{clamp_receiver, enclosing_emitted_symbol, receiver_from};

/// Calls made by Swift code.
/// The grammar key this module answers for, so caller attribution asks
/// `langdecl` the same question the declaration emitter asks.
const LANG: &str = "swift";

pub fn extract_swift_call(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    let Some(call) = swift_call_identity(node, source) else {
        return;
    };
    // A call with no spelling names nothing. tree-sitter-swift's recovery of
    // `x as? String ?? "d"` inserts a hidden zero-width `custom_operator`, which
    // would otherwise pass the operator exemption below as a call named `""`.
    // Refused here, before that exemption, because both operator arms bypass
    // `is_callee_identity`; the file says so in one diagnostic.
    if call.callee_name.trim().is_empty() || call.name_node.byte_range().is_empty() {
        note_swift_unspelled_operator();
        return;
    }
    // An operator's name is punctuation, which `is_callee_identity` refuses by
    // design for an *expression*. A `custom_operator` node is a declared
    // operator's own spelling, so it is an identity and passes.
    if !call.operator
        && call.name_node.kind() != "custom_operator"
        && !is_callee_identity(&call.callee_name)
    {
        return;
    }
    let enclosing = enclosing_emitted_symbol(node, source, LANG, file_symbol_name);
    references.push(ExtractedReference {
        name: call.callee_name.clone(),
        kind: call.kind,
        span: node_span(call.name_node),
        enclosing_symbol: enclosing.clone(),
        assigned_to: swift_assignment_binding(node, source),
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

/// One call site's identity: what is called, what it is called on, and the node
/// that carries the name.
struct SwiftCall<'tree> {
    callee_name: String,
    receiver_expr: Option<String>,
    name_node: Node<'tree>,
    kind: ReferenceKind,
    /// The name is an operator's spelling, not an identifier.
    operator: bool,
}

fn swift_call_identity<'tree>(node: Node<'tree>, source: &str) -> Option<SwiftCall<'tree>> {
    match node.kind() {
        "call_expression" => {
            // `items[0]` is a `call_expression` too — the grammar tells a
            // subscript apart only by the bracket that opens its argument list.
            // Recording it would name the *collection variable* as a callee, so
            // an array named after a function in the same file would resolve to
            // that function at deterministic confidence: a fabricated edge of
            // the SC9 class. Swift's own `subscript` declarations are not
            // symbols either (`generic_symbol_kind` has no `subscript_declaration`),
            // so there is nothing a subscript could correctly point at.
            if swift_is_subscript(node) {
                return None;
            }
            let target = swift_callee_target(node)?;
            // An immediately-invoked closure has no callee identity; recording
            // its text would manufacture a name no symbol can carry (SC32).
            if is_anonymous_callable(target.kind()) || target.kind() == "lambda_literal" {
                return None;
            }
            // `!applySecret(x)` and `-g()`. tree-sitter-swift binds the prefix
            // operator *before* the argument list, so the callee of the
            // `call_expression` is the `prefix_expression` `!applySecret`, which
            // `split_call_target` refuses — and the call disappears entirely.
            // Measured on 342 real `.swift` files: two functions called only
            // through `if !f(…)` were reported dead at 0.9 confidence, from a
            // call the graph never saw.
            let target = swift_unwrap_prefix(target).unwrap_or(target);
            let target = swift_call_operand(target);
            if target.kind() == "navigation_expression" {
                return swift_navigation_call(target, source);
            }
            let (callee_name, receiver) = split_call_target(target, source)?;
            // `defer { cleanup() }` parses as a call whose callee is the
            // keyword `defer` and whose argument is a trailing closure. Swift
            // reserves the name, so this cannot be a function the repository
            // declared; recording it produced hundreds of unresolved sites
            // that meant "the file uses defer", not "defer was never found".
            if callee_name == "defer" {
                return None;
            }
            // `Coordinator()` after `typealias Coordinator = Held<Int>` constructs a
            // `Held`; a qualified or lowercase callee is never read as an alias.
            let callee_name = if receiver.is_none() {
                alias_name(target, source, callee_name)
            } else {
                callee_name
            };
            Some(SwiftCall {
                callee_name,
                receiver_expr: receiver.as_deref().and_then(clamp_receiver),
                name_node: target,
                kind: ReferenceKind::Call,
                operator: false,
            })
        }
        // `Set<Int>()`, `Box<Int>(value: 1)`. The constructed type is a field,
        // and the generic arguments are not part of the callee's identity.
        "constructor_expression" => {
            let constructed = node.child_by_field_name("constructed_type")?;
            let (callee_name, receiver_expr) = swift_user_type_identity(constructed, source)?;
            let callee_name = if receiver_expr.is_none() {
                alias_name(constructed, source, callee_name)
            } else {
                callee_name
            };
            Some(SwiftCall {
                callee_name,
                receiver_expr,
                name_node: constructed,
                kind: ReferenceKind::Constructor,
                operator: false,
            })
        }
        // `a <+> b`. A custom operator is a call of the `static func <+>` some
        // type declares; tree-sitter-swift spells the use as an
        // `infix_expression` whose `op` is a `custom_operator`, and before this
        // arm nothing recorded it, so a `fileprivate static func <+>` used only
        // through its operator was published dead at 0.9 (the audit's Swift
        // P1). Standard operators (`+`, `==`) parse as their own expression
        // kinds and are not recorded: every `a + b` on an `Int` would be a
        // ledger row. Their in-repo overloads are exempt at the declaration
        // instead — see `swift_operator_declaration_reason`.
        "infix_expression" => {
            let op = node.child_by_field_name("op")?;
            if op.kind() != "custom_operator" {
                return None;
            }
            Some(SwiftCall {
                callee_name: get_node_text(op, source),
                receiver_expr: None,
                name_node: op,
                kind: ReferenceKind::Call,
                operator: true,
            })
        }
        // `a + b`, `a == b`, `a < b` on an operand that could be a user type:
        // the call of the `static func +` that type declares, with the left
        // operand as its receiver (the type an overload is declared on). A
        // literal left operand is never one; the operators' use on the standard
        // library's own types is not worth a ledger row per expression, and an
        // unindexed receiver type is classified as such, not as a defect.
        "additive_expression"
        | "multiplicative_expression"
        | "comparison_expression"
        | "equality_expression" => {
            let lhs = node.child_by_field_name("lhs")?;
            if !matches!(
                lhs.kind(),
                "simple_identifier" | "navigation_expression" | "self_expression"
            ) {
                return None;
            }
            let rhs = node.child_by_field_name("rhs")?;
            let mut cursor = node.walk();
            let op = node.children(&mut cursor).find(|child| {
                !child.is_named()
                    && child.start_byte() >= lhs.end_byte()
                    && child.end_byte() <= rhs.start_byte()
            })?;
            Some(SwiftCall {
                callee_name: get_node_text(op, source),
                receiver_expr: clamp_receiver(&get_node_text(lhs, source)),
                name_node: op,
                kind: ReferenceKind::Call,
                operator: true,
            })
        }
        _ => None,
    }
}

/// The operand of an operator prefix, or `None` when the prefix is a leading dot.
///
/// `!f()`, `-g()` and `~h()` all name `f`, `g`, `h`. `.text("z")` does not: Swift's
/// leading-dot inference names a member of a type the site never spells, so the
/// only honest callee is one this extractor cannot determine. Unwrapping it
/// anyway would let `.text("z")` bind to any same-named free function in the
/// file at deterministic confidence — the SC9 class of confidently-wrong edge —
/// so it is refused, and the case reached that way stays exempt through the
/// `StructuralExempt` annotation `langdecl::swift` puts on every enum case.
fn swift_unwrap_prefix<'tree>(target: Node<'tree>) -> Option<Node<'tree>> {
    if target.kind() != "prefix_expression" {
        return None;
    }
    if target.child(0).is_some_and(|first| first.kind() == ".") {
        return None;
    }
    target.child_by_field_name("target")
}

/// The operand a call's callee really is, when the grammar bound the operator
/// too tightly.
///
/// tree-sitter-swift parses `"x" + f()` as `call_expression(additive_expression
/// ("x" + f), call_suffix)`: the argument list attaches to the whole sum, so the
/// callee text was `"x" + f`, no identity at all, and the call was dropped
/// (recorded as a bare reference). `1 - q.r()` is the same shape one level down,
/// with the sum as the *target* of the member access, and `a.bump() + g.bump()`
/// typed its second call by the receiver `a.bump() + g`. The right operand of
/// the sum is the callee (or the member target); `+ - * /` are the only
/// operators the grammar treats this way.
pub(crate) fn swift_call_operand<'tree>(node: Node<'tree>) -> Node<'tree> {
    let mut node = node;
    while matches!(
        node.kind(),
        "additive_expression" | "multiplicative_expression"
    ) {
        match node.child_by_field_name("rhs") {
            Some(rhs) => node = rhs,
            None => break,
        }
    }
    node
}

/// Whether a `call_expression` is really a subscript: `items[0]`, `dict["k"]`.
fn swift_is_subscript(node: Node) -> bool {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "call_suffix" {
            continue;
        }
        let mut suffix_cursor = child.walk();
        for grandchild in child.named_children(&mut suffix_cursor) {
            if grandchild.kind() == "value_arguments" {
                return grandchild.child(0).is_some_and(|open| open.kind() == "[");
            }
        }
    }
    false
}

/// The callee subtree of a `call_expression`.
///
/// tree-sitter-swift gives `call_expression` no `function` field: the callee is
/// the first named child and the argument list — parenthesised, a trailing
/// closure, or both — is a `call_suffix` sibling.
fn swift_callee_target<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !matches!(
            child.kind(),
            "call_suffix" | "comment" | "multiline_comment"
        ) {
            return Some(child);
        }
    }
    None
}

/// `receiver.method()`, including `receiver?.method()` and `receiver!.method()`.
fn swift_navigation_call<'tree>(nav: Node<'tree>, source: &str) -> Option<SwiftCall<'tree>> {
    // `b as? Color ?? .foo()`: the member hangs off a cast, so its receiver is
    // cast text and no type. Refused, with the mirrored `Call` reference.
    if swift_on_bare_cast(nav) {
        return None;
    }
    let suffix = nav.child_by_field_name("suffix")?;
    let name_node = suffix.child_by_field_name("suffix")?;
    let (callee_name, _) = split_call_target(name_node, source)?;
    let raw_target = nav.child_by_field_name("target")?;
    let target = swift_call_operand(raw_target);
    // `Person.init(name:)` constructs a `Person`; recording `init` as the callee
    // would name a symbol that does not exist, because the generic declaration
    // arm emits nothing for an `init_declaration`. `self.init` and `super.init`
    // deliberately produce nothing: the first names the type the call is already
    // inside, and the second names a superclass this node cannot identify.
    if callee_name == "init" {
        let (type_name, qualifier) = swift_init_type(target, source)?;
        return Some(SwiftCall {
            callee_name: type_name,
            receiver_expr: qualifier,
            name_node: target,
            kind: ReferenceKind::Constructor,
            operator: false,
        });
    }
    // `a + b.c.run()` parses as `((a + b).c).run()`: the sum is the base of the
    // chain. Member access binds tighter than `+`, so the receiver is `b.c`.
    let receiver = match swift_sum_tail(raw_target) {
        Some(tail) => source
            .get(tail.start_byte()..raw_target.end_byte())
            .and_then(clamp_receiver),
        None => swift_receiver(target, source),
    };
    Some(SwiftCall {
        callee_name,
        receiver_expr: receiver
            .map(|receiver| member_path_type(nav, source, &receiver).unwrap_or(receiver))
            .map(|receiver| alias_name(nav, source, receiver)),
        name_node,
        kind: ReferenceKind::Call,
        operator: false,
    })
}

/// Whether the member chain `chain` is based on an unparenthesised cast (`as`,
/// `as?` or `as!`: any `as_expression`).
///
/// Swift's casts bind looser than postfix `.` and `()`, so a navigation chain
/// whose base is an `as_expression` is **always** a misparse. tree-sitter-swift
/// reads `b as? Color ?? .foo()` as `navigation_expression(as_expression
/// "b as? Color ??", .foo)`, and `(b as? Foo.Bar)` as `(as_expression "b as?
/// Foo", .Bar)`, without an error node. The member's owner is a type the site
/// never spells, so a fact about it is refused rather than repaired: reading it
/// from the leftmost operand would bind `b`'s type, a wrong edge. A parenthesised
/// cast is a `tuple_expression`, a real operand, and is not this. Bounded at the
/// same 16 steps as [`swift_sum_tail`]; a chain longer than that is not refused.
pub(crate) fn swift_on_bare_cast(chain: Node) -> bool {
    let mut node = chain;
    for _ in 0..=16 {
        node = match node.kind() {
            "as_expression" => return true,
            "navigation_expression" => match node.child_by_field_name("target") {
                Some(target) => target,
                None => return false,
            },
            "call_expression" => match swift_callee_target(node) {
                Some(callee) => callee,
                None => return false,
            },
            _ => return false,
        };
    }
    false
}

/// The right operand of the sum at the base of a member chain: for
/// `(a + b).c` the node `b`. `None` when the chain does not start from a sum
/// (the sum directly under the call is [`swift_call_operand`]'s).
pub(crate) fn swift_sum_tail<'tree>(chain: Node<'tree>) -> Option<Node<'tree>> {
    let mut node = chain;
    let mut steps = 0;
    while matches!(node.kind(), "navigation_expression" | "call_expression") {
        node = if node.kind() == "navigation_expression" {
            node.child_by_field_name("target")?
        } else {
            swift_callee_target(node)?
        };
        steps += 1;
        if steps > 16 {
            return None;
        }
    }
    (steps > 0 && node.id() != chain.id())
        .then(|| swift_call_operand(node))
        .filter(|tail| tail.id() != node.id())
}

/// The type of a two-segment member path (`open.transform`) when the scope that
/// binds its head writes the head's tuple type with that member typed; the
/// receiver is then that type, the way a typed local's receiver is.
fn member_path_type(at: Node, source: &str, receiver: &str) -> Option<String> {
    let word = |segment: &str| {
        !segment.is_empty() && segment.chars().all(|c| c.is_alphanumeric() || c == '_')
    };
    let (head, member) = receiver.split_once('.')?;
    // A member is a label or a position (`open.0`); the head is a name.
    if !word(head) || !word(member) || head.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    crate::swiftscope::tuple_member_type(at, source, receiver)
        .filter(|ty| !ty.contains(['[', ']', '?']))
}

/// What a bare type name stands for when this file declares it with
/// `typealias`: `H.duration(…)` after `typealias H = SleepLogHints` is a call on
/// `SleepLogHints`. Anything that is not one bare capitalised name is returned
/// as written.
fn alias_name(at: Node, source: &str, name: String) -> String {
    let bare = name.chars().all(|c| c.is_alphanumeric() || c == '_');
    if !bare {
        return name;
    }
    crate::swiftscope::alias_of_type_name(at, source, &name).unwrap_or(name)
}

/// The longest receiver text kept whole; past it, or across lines, a
/// construction is collapsed to its head (`clamp_receiver` owns the same limit).
const WHOLE_RECEIVER_BYTES: usize = 96;

/// The receiver text for a member call's target.
///
/// Two shapes reach `receiver_from` as text the resolver cannot read:
///
/// * `!Probe().snapshot()`: tree-sitter-swift binds the `!` to `Probe`, so the
///   target is the call `!Probe()` and the text starts with an operator. Swift
///   applies a prefix operator to the whole member chain, so the receiver of
///   `.snapshot` is `Probe()`; the operand is read, never the operator.
/// * `Svc(\n root: "a",\n limit: 2\n).scan()`: `clamp_receiver` keeps the first
///   line and a `…`, which loses the closing parenthesis the resolver needs to
///   see one construction. A construction that is multi-line or longer than the
///   whole-receiver limit is written `Svc(…)`: the callee and a balanced, elided
///   argument list. Only a bare-name callee with a parenthesised argument list
///   is collapsed; a trailing-closure call or a chain keeps the old clamp.
fn swift_receiver(target: Node, source: &str) -> Option<String> {
    let mut target = swift_unwrap_prefix(target).unwrap_or(target);
    // `try Svc(…).run()` and `await Svc(…).run()`: the effect keyword wraps the
    // construction in the target; the receiver is what it wraps.
    for _ in 0..4 {
        if !matches!(target.kind(), "try_expression" | "await_expression") {
            break;
        }
        let mut cursor = target.walk();
        let inner = target
            .named_children(&mut cursor)
            .find(|child| !matches!(child.kind(), "try_operator" | "await_operator" | "comment"));
        match inner {
            Some(inner) => target = inner,
            None => break,
        }
    }
    if let Some(cast) = swift_cast_type(target, source) {
        return Some(cast);
    }
    if target.kind() != "call_expression" {
        return receiver_from(target, source);
    }
    let Some(callee) = swift_callee_target(target) else {
        return receiver_from(target, source);
    };
    let head = swift_unwrap_prefix(callee).unwrap_or(callee);
    if head.kind() != "simple_identifier" || swift_is_subscript(target) {
        return receiver_from(target, source);
    }
    let Some(text) = source.get(head.start_byte()..target.end_byte()) else {
        return receiver_from(target, source);
    };
    let parenthesised = {
        let mut cursor = target.walk();
        let found = target
            .named_children(&mut cursor)
            .find(|child| child.kind() == "call_suffix")
            .is_some_and(|suffix| {
                let mut inner = suffix.walk();
                let first = suffix.named_children(&mut inner).next();
                first.is_some_and(|args| {
                    args.kind() == "value_arguments"
                        && args.child(0).is_some_and(|open| open.kind() == "(")
                }) && text.ends_with(')')
            });
        found
    };
    if parenthesised && (text.contains('\n') || text.len() > WHOLE_RECEIVER_BYTES) {
        return Some(format!("{}(…)", get_node_text(head, source)));
    }
    if head.id() != callee.id() {
        return clamp_receiver(text);
    }
    receiver_from(target, source)
}

/// The type a parenthesised cast is written to: `(x as? AppDelegate)` is an
/// `AppDelegate`, whatever `x` was, so `(x as? AppDelegate)?.open()` is a call
/// on that type. Only one bare capitalised type name is read; a qualified,
/// generic-argument or collection target is left as written text.
fn swift_cast_type(target: Node, source: &str) -> Option<String> {
    if target.kind() != "tuple_expression" || target.named_child_count() != 1 {
        return None;
    }
    let cast = target.named_child(0)?;
    if cast.kind() != "as_expression" {
        return None;
    }
    let written = cast.child_by_field_name("name")?;
    if written.kind() != "user_type" {
        return None;
    }
    let (name, qualifier) = swift_user_type_identity(written, source)?;
    if qualifier.is_some()
        || !starts_capitalised(&name)
        || crate::swiftscope::is_generic_parameter(target, source, &name)
    {
        return None;
    }
    Some(alias_name(target, source, name))
}

/// The type named by a `user_type`, and the type or module qualifying it.
fn swift_user_type_identity(ty: Node, source: &str) -> Option<(String, Option<String>)> {
    let mut names: Vec<String> = Vec::new();
    let mut cursor = ty.walk();
    for child in ty.named_children(&mut cursor) {
        if matches!(child.kind(), "type_identifier" | "simple_identifier") {
            names.push(get_node_text(child, source));
        }
    }
    // The last segment is the type; anything before it qualifies the type.
    let name = names.pop()?;
    if !is_callee_identity(&name) {
        return None;
    }
    let qualifier = names.pop().filter(|q| is_callee_identity(q));
    Some((name, qualifier))
}

/// The type a `T.init(…)` constructs.
///
/// Requires the name to be capitalised. This is the one place where a *value*
/// expression could reach the type path — `anything.init(…)` parses the same way
/// — and Swift's naming convention is the only available evidence that the
/// target is a type. Fail-closed: an unrecognised target yields no call rather
/// than a call named `init`, which could never resolve.
fn swift_init_type(target: Node, source: &str) -> Option<(String, Option<String>)> {
    let identity = match target.kind() {
        "simple_identifier" | "type_identifier" => (get_node_text(target, source), None),
        "user_type" => swift_user_type_identity(target, source)?,
        "navigation_expression" => {
            let suffix = target.child_by_field_name("suffix")?;
            let name_node = suffix.child_by_field_name("suffix")?;
            let qualifier = target
                .child_by_field_name("target")
                .and_then(|node| receiver_from(node, source));
            (get_node_text(name_node, source), qualifier)
        }
        _ => return None,
    };
    (starts_capitalised(&identity.0) && is_callee_identity(&identity.0)).then_some(identity)
}

/// Whether a name follows Swift's convention for a type rather than a value.
fn starts_capitalised(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}

/// The local variable a call's result is bound to, when the grammar proves one.
///
/// This is what lets the resolver bind `let service = Service()` and then
/// resolve `service.run()` by receiver type. The shared `assignment_binding`
/// helper knows neither Swift binding node, so it returns `None` for every Swift
/// call; the walk here is bounded and stops at argument and closure boundaries
/// so an argument's own call (`outer(Inner())`) cannot claim the outer binding.
fn swift_assignment_binding(node: Node, source: &str) -> Option<String> {
    let mut current = node;
    for _ in 0..4 {
        let parent = crate::treesitter::bounded_parent(current)?;
        match parent.kind() {
            // `let service = Service()`
            "property_declaration" => {
                let value = parent.child_by_field_name("value")?;
                if value.id() != current.id() {
                    return None;
                }
                let pattern = parent.child_by_field_name("name")?;
                let bound = pattern.child_by_field_name("bound_identifier")?;
                let name = get_node_text(bound, source);
                return is_callee_identity(&name).then_some(name);
            }
            // `service = Service()`. Only a bare local target binds: `self.x = …`
            // assigns a property, whose type the file-wide receiver map must not
            // learn from a name that is not in scope as a variable.
            "assignment" => {
                let result = parent.child_by_field_name("result")?;
                if result.id() != current.id() {
                    return None;
                }
                let target = parent.child_by_field_name("target")?;
                let name = get_node_text(target, source);
                return is_callee_identity(&name).then_some(name);
            }
            "value_argument" | "value_arguments" | "call_suffix" | "lambda_literal"
            | "statements" | "function_body" => return None,
            _ => current = parent,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The receiver text recorded for every call named `callee`, in source order.
    fn receivers(source: &str, callee: &str) -> Vec<Option<String>> {
        // Node ids are arena addresses, so every parse starts from a clean cache.
        crate::swiftscope::reset();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_swift::LANGUAGE.into())
            .expect("swift grammar");
        let tree = parser.parse(source, None).expect("parse");
        let mut calls = Vec::new();
        let mut references = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            extract_swift_call(node, source, "a.swift", &mut calls, &mut references);
            for index in (0..node.child_count()).rev() {
                if let Some(child) = node.child(index) {
                    stack.push(child);
                }
            }
        }
        calls
            .into_iter()
            .filter(|call| call.callee_name == callee)
            .map(|call| call.receiver_expr)
            .collect()
    }

    fn one(source: &str, callee: &str) -> Option<String> {
        let found = receivers(source, callee);
        assert_eq!(found.len(), 1, "calls to {callee}: {found:?}");
        found.into_iter().next().flatten()
    }

    #[test]
    fn a_prefix_operator_is_not_part_of_a_constructed_receiver() {
        assert_eq!(
            one("func f() -> Bool { !Probe().snapshot().ok }", "snapshot").as_deref(),
            Some("Probe()")
        );
        assert_eq!(
            one("func f() { _ = -Meter(v: 1).bump() }", "bump").as_deref(),
            Some("Meter(v: 1)")
        );
        assert_eq!(
            one("func f() { _ = !!Probe().snapshot() }", "snapshot").as_deref(),
            Some("Probe()")
        );
    }

    #[test]
    fn a_prefix_on_a_plain_name_leaves_the_name() {
        assert_eq!(
            one("func f() { _ = !flag.toggled() }", "toggled").as_deref(),
            Some("flag")
        );
    }

    #[test]
    fn a_construction_receiver_is_whole_on_one_line_and_elided_across_lines() {
        let one_line = "func f() { Svc(root: \"a\", limit: 2).scan() }";
        assert_eq!(
            one(one_line, "scan").as_deref(),
            Some("Svc(root: \"a\", limit: 2)")
        );
        let two = "func f() {\n    Svc(root: \"a\",\n        limit: 2).scan()\n}";
        assert_eq!(one(two, "scan").as_deref(), Some("Svc(…)"));
        let many = "func f() {\n    Svc(\n        root: \"a\",\n        limit: 2,\n        deep: 3\n    ).scan()\n}";
        assert_eq!(one(many, "scan").as_deref(), Some("Svc(…)"));
        let long = format!("func f() {{ Svc(root: \"{}\").scan() }}", "x".repeat(200));
        assert_eq!(one(&long, "scan").as_deref(), Some("Svc(…)"));
    }

    #[test]
    fn an_effect_keyword_is_not_part_of_a_constructed_receiver() {
        for keyword in ["try", "await", "try await"] {
            let source = format!(
                "func f() async throws {{\n    {keyword} Svc(\n        a: 1\n    ).run()\n}}"
            );
            assert_eq!(one(&source, "run").as_deref(), Some("Svc(…)"), "{keyword}");
        }
        assert_eq!(
            one("func f() throws { try Svc(a: 1).run() }", "run").as_deref(),
            Some("Svc(a: 1)")
        );
    }

    #[test]
    fn a_multi_line_construction_under_a_prefix_operator_is_elided_too() {
        let source = "func f() -> Bool {\n    !Probe(\n        a: 1\n    ).snapshot().ok\n}";
        assert_eq!(one(source, "snapshot").as_deref(), Some("Probe(…)"));
    }

    /// The callee name of every call recorded in `source`, in source order.
    fn callees(source: &str) -> Vec<String> {
        crate::swiftscope::reset();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_swift::LANGUAGE.into())
            .expect("swift grammar");
        let tree = parser.parse(source, None).expect("parse");
        let mut calls = Vec::new();
        let mut references = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            extract_swift_call(node, source, "a.swift", &mut calls, &mut references);
            for index in (0..node.child_count()).rev() {
                if let Some(child) = node.child(index) {
                    stack.push(child);
                }
            }
        }
        calls.into_iter().map(|call| call.callee_name).collect()
    }

    #[test]
    fn a_receiver_this_file_declares_with_typealias_is_the_type_it_stands_for() {
        let source = "private typealias H = SleepLogHints\n\
                      func f() { _ = H.formatDuration(45) }";
        assert_eq!(
            one(source, "formatDuration").as_deref(),
            Some("SleepLogHints")
        );
        // A nested alias is the name's meaning inside its type only.
        let nested = "struct Host { typealias H = Hints\n func f() { _ = H.go() } }\n\
                      struct Other { func g() { _ = H.go() } }";
        assert_eq!(
            receivers(nested, "go"),
            vec![Some("Hints".to_string()), Some("H".to_string())]
        );
        // Two aliases of the name, or a type of that name, leave it as written.
        let twice = "typealias H = A\nextension X { typealias H = B }\nfunc f() { _ = H.go() }";
        assert_eq!(one(twice, "go").as_deref(), Some("H"));
        let declared = "typealias H = A\nstruct H {}\nfunc f() { _ = H.go() }";
        assert_eq!(one(declared, "go").as_deref(), Some("H"));
        // An alias of a generic parameter, of a collection or of another alias is no receiver.
        let generic = "struct Box<T> { typealias H = T\n func f() { _ = H.go() } }";
        assert_eq!(one(generic, "go").as_deref(), Some("H"));
        let collection = "typealias H = [Item]\nfunc f() { _ = H.go() }";
        assert_eq!(one(collection, "go").as_deref(), Some("H"));
        let chain = "typealias H = G\ntypealias G = Item\nfunc f() { _ = H.go() }";
        assert_eq!(one(chain, "go").as_deref(), Some("H"));
        // A value is never an alias.
        let value = "typealias h = Item\nfunc f() { _ = h.go() }";
        assert_eq!(one(value, "go").as_deref(), Some("h"));
    }

    #[test]
    fn a_construction_through_a_typealias_constructs_its_target() {
        let source = "typealias Coordinator = Held<Int>\nfunc f() { let c = Coordinator(); _ = c }";
        assert_eq!(callees(source), vec!["Held".to_string()]);
        let qualified = "typealias Coordinator = Held<Int>\nfunc f() { _ = Mod.Coordinator() }";
        assert_eq!(callees(qualified), vec!["Coordinator".to_string()]);
    }

    #[test]
    fn a_member_chain_after_a_sum_is_the_receiver_without_the_sum() {
        // `a + b.c.run()` is `a + (b.c.run())`: member access binds tighter.
        assert_eq!(
            one("func f() { x = a + b.c.run() }", "run").as_deref(),
            Some("b.c")
        );
        assert_eq!(
            one("func f() { x = a * b.make().run() }", "run").as_deref(),
            Some("b.make()")
        );
        assert_eq!(
            one("func f() { x = a - b.c.d.run() }", "run").as_deref(),
            Some("b.c.d")
        );
        // A parenthesised sum is a real receiver; the one directly under the call is not.
        assert_eq!(
            one("func f() { x = (a + b).c.run() }", "run").as_deref(),
            Some("(a + b).c")
        );
        assert_eq!(
            one("func f() { x = a + b.run() }", "run").as_deref(),
            Some("b")
        );
    }

    #[test]
    fn a_tuple_parameters_typed_member_is_a_receiver_of_that_type() {
        let source =
            "final class J {\n    func apply(_ open: (transform: Case, start: Int), _ x: Int) {\n\
                      out = open.transform.apply(x)\n    }\n}";
        assert_eq!(receivers(source, "apply"), vec![Some("Case".to_string())]);
        // The same call as the right operand of a sum.
        let sum =
            "final class J {\n    func apply(_ open: (transform: Case, start: Int), _ x: Int) {\n\
                   out = head + open.transform.apply(x)\n    }\n}";
        assert_eq!(receivers(sum, "apply"), vec![Some("Case".to_string())]);
        // A positional member, through an alias.
        let positional = "typealias C = Case\nfunc f(_ open: (C, Int)) { open.0.go() }";
        assert_eq!(receivers(positional, "go"), vec![Some("Case".to_string())]);
        // An untyped member, a member of a plain parameter and an inner scope
        // that rebinds the tuple leave the path as written.
        let function = "func f(_ h: (run: (Int) -> Void, Int)) { h.run.go() }";
        assert_eq!(receivers(function, "go"), vec![Some("h.run".to_string())]);
        let plain = "func f(_ open: Joiner) { open.transform.go() }";
        assert_eq!(
            receivers(plain, "go"),
            vec![Some("open.transform".to_string())]
        );
        let shadowed = "func f(_ open: (transform: Case, Int)) {\n\
                        let g = { (open: Other) in open.transform.go() }\n}";
        assert_eq!(
            receivers(shadowed, "go"),
            vec![Some("open.transform".to_string())]
        );
        let unbound = "func f() { open.transform.go() }";
        assert_eq!(
            receivers(unbound, "go"),
            vec![Some("open.transform".to_string())]
        );
    }

    #[test]
    fn a_parenthesised_cast_is_a_call_on_the_type_it_casts_to() {
        let source = "func f(_ x: AnyObject) { (x as? AppDelegate)?.openIt(nil) }";
        assert_eq!(one(source, "openIt").as_deref(), Some("AppDelegate"));
        let forced = "func f(_ x: AnyObject) { (x as! AppDelegate).openIt(nil) }";
        assert_eq!(one(forced, "openIt").as_deref(), Some("AppDelegate"));
        // A collection, a qualified type or a lowercase name is left as written.
        let array = "func f(_ x: Any) { (x as? [Item])?.go() }";
        assert_eq!(one(array, "go").as_deref(), Some("(x as? [Item])"));
        let qualified = "func f(_ x: Any) { (x as? Mod.Item)?.go() }";
        assert_eq!(one(qualified, "go").as_deref(), Some("(x as? Mod.Item)"));
        // A cast through an alias is the alias's target.
        let aliased = "typealias Del = AppDelegate\nfunc f(_ x: Any) { (x as? Del)?.go() }";
        assert_eq!(one(aliased, "go").as_deref(), Some("AppDelegate"));
    }

    #[test]
    fn only_a_parenthesised_construction_is_elided() {
        // A trailing-closure call is not a construction with an argument list.
        let closure = "func f() {\n    Stack {\n        a()\n    }.run()\n}";
        let got = one(closure, "run");
        assert_ne!(got.as_deref(), Some("Stack(…)"), "{got:?}");
        // A chain keeps the first-line clamp: its head is not a bare name.
        let chain = "func f() {\n    Svc(\n        a: 1\n    ).make().scan()\n}";
        let got = one(chain, "scan");
        assert_ne!(got.as_deref(), Some("Svc(…)"), "{got:?}");
        // A subscript is not a call of the collection.
        let subscript = "func f() {\n    items[\n        0\n    ].run()\n}";
        let got = one(subscript, "run");
        assert_ne!(got.as_deref(), Some("items(…)"), "{got:?}");
    }
}
