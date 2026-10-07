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
//! The one send that is *not* a `call` is the bare one: `def entry; helper;
//! end`, `x = compute`, `current_user.name`. tree-sitter-ruby gives it a plain
//! `identifier`, the same node a local variable read gets, and leaving it
//! unclaimed made `impact` answer a confident zero for a method with a caller
//! (measured 2026-10-06). Ruby's own parser settles which it is *lexically*: an
//! identifier is a local only if a binding of that name precedes it in the
//! enclosing scope, where `def`, `class`, `module` and the file are scope gates
//! and blocks are not. `extract_bare_sends` applies the conservative half of
//! that rule — a binding of the name *anywhere* in the scope refuses it, before
//! or after — so the error it can make is a missing edge, never a wrong one
//! (SC9). See `extract_bare_sends` for what it still refuses.
//!
//! Shapes deliberately **not** claimed, because the grammar gives them no
//! callee identity: `super` and `yield`.
//!
//! Caller attribution comes from `super::scope`, not from
//! `enclosing_callable_qualified`. A call edge's `caller_symbol` is a join key,
//! so it has to be the identity the *symbol emitter* gives the enclosing
//! declaration, and these two languages reach the emitter through the generic
//! declaration arm. The shared helper mirrors that arm's own tables; the
//! per-language disagreements it exists to avoid are listed there.

use std::collections::HashSet;

use tree_sitter::Node;

use super::scope::{clamp_receiver, enclosing_emitted_symbol};
use crate::model::{ExtractedCall, ExtractedReference, ReferenceKind};
use crate::treesitter::{
    get_child_text, get_node_text, is_callee_identity, node_span, split_call_target,
    walk_deadline_passed, DEADLINE_CHECK_STRIDE,
};

/// The grammar key this module answers for, so caller attribution asks
/// `langdecl` the same question the declaration emitter asks.
const LANG: &str = "ruby";

/// Calls made by Ruby code.
///
/// A `call` node is one send. A scope gate is visited once, and claims the
/// bare sends that scope makes; nested gates are visited by the walk in their
/// own turn, so every identifier is judged by exactly one scope and the extra
/// work is one pass over each scope's subtree.
pub(crate) fn extract_ruby_calls(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    match node.kind() {
        "call" => extract_send(node, source, file_symbol_name, calls, references),
        kind if is_scope_gate(kind) => {
            extract_bare_sends(node, source, file_symbol_name, calls, references)
        }
        _ => {}
    }
}

fn extract_send(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
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
    crate::treesitter::bounded_parent(node).is_some_and(|parent| {
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
    let parent = crate::treesitter::bounded_parent(node)?;
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

// ------------------------------------------------------------ bare sends

/// The nodes that open a fresh local-variable scope. A block (`{ … }`,
/// `do … end`, a lambda body) is deliberately absent: it sees its scope's
/// locals, so it is judged as part of that scope.
fn is_scope_gate(kind: &str) -> bool {
    matches!(
        kind,
        "program" | "class" | "module" | "singleton_class" | "method" | "singleton_method"
    )
}

/// Claim the bare, receiverless, parenthesis-less sends `scope` makes.
///
/// An `identifier` is claimed only when all of these hold:
///
/// * it sits in a slot `is_value_position` lists — a positive list, so a slot
///   nobody judged is a refusal rather than a guess;
/// * it is not inside a pattern (`in [x]`, `v => {k:}`), where an identifier
///   binds rather than reads, nor the operand of `defined?`, which tests for a
///   method without calling it;
/// * no binding of the name exists anywhere in the scope (`local_bindings`);
/// * it is not an implicit block parameter: `_1`…`_9` always, and `it` inside
///   any block (Ruby 3.4). Outside a block `it` is a send, but refusing it
///   everywhere in a block costs only RSpec-style edges, never adds one;
/// * the scope parsed without error. Recovery can drop the very assignment that
///   makes a name local, so a scope containing an `ERROR` or `MISSING` node has
///   no trustworthy binding list and claims nothing;
/// * no enclosing class or module defines the name without `def`
///   (`methods_defined_without_def`).
///
/// Not claimed either, because the scope that owns them is not the gate they
/// sit under: a superclass expression (`class K < base`) and the `class <<
/// expr` value are evaluated in the *enclosing* scope, and `def obj.m` names its
/// object there too. None of those slots is on the positive list.
///
/// Why this does not reuse `treesitter::collect_non_symbol_locals`: that set
/// is an allowlist of *binding* positions, and for this question a binding
/// form it does not know becomes a wrong edge. Here the default runs the other
/// way — an identifier not proven to be a read counts as a binding — so a form
/// this module has never seen costs an edge instead of inventing one.
fn extract_bare_sends(
    scope: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    if scope.has_error() {
        return;
    }
    let Some(bound) = local_bindings(scope, source) else {
        return;
    };
    let mut sends = Vec::new();
    let completed = walk_scope(scope, false, |node, slot| {
        if node.kind() == "identifier" && slot.is_read() {
            sends.push((node, slot.in_block));
        }
    });
    // A walk the deadline cut short has not seen the whole scope. The file is
    // refused by the latch either way; claiming half of it would only put
    // edges into an extraction that is about to be thrown away.
    if !completed {
        return;
    }
    // Computed only once a send survives the cheaper refusals: most scopes
    // make none, and this one climbs to every enclosing class.
    let mut defined_without_def: Option<HashSet<String>> = None;
    for (node, in_block) in sends {
        let name = get_node_text(node, source);
        if bound.contains(&name) || is_implicit_block_parameter(&name, in_block) {
            continue;
        }
        if !is_callee_identity(&name) {
            continue;
        }
        if defined_without_def.is_none() {
            let Some(names) = methods_defined_without_def(scope, source) else {
                return;
            };
            defined_without_def = Some(names);
        }
        if defined_without_def
            .as_ref()
            .is_some_and(|names| names.contains(&name))
        {
            continue;
        }
        let caller_symbol = enclosing_emitted_symbol(node, source, LANG, file_symbol_name);
        let assigned_to = ruby_assigned_binding(node, source);
        references.push(ExtractedReference {
            name: name.clone(),
            kind: ReferenceKind::Call,
            span: node_span(node),
            enclosing_symbol: caller_symbol.clone(),
            assigned_to,
            receiver_expr: None,
        });
        calls.push(ExtractedCall {
            caller_symbol,
            callee_name: name,
            receiver_expr: None,
            span: node_span(node),
        });
    }
}

/// Every name `scope` could be binding as a local, or `None` when the walk
/// was cut short by the deadline.
///
/// Over-collection is the safe direction, so this walks the whole subtree —
/// nested `def`s and classes included, although Ruby would not let their
/// locals reach this scope — and files every identifier that is not provably
/// evaluated as a value (`Slot::may_bind`). The only others exempted are the
/// two that are provably not locals: the method a `call` names, and the name a
/// `def` declares. Without the second, `def main … end; main` at the top of a
/// script would refuse the very send it is the canonical example of.
///
/// Two bindings the grammar does not spell as an identifier are added by hand:
/// the shorthand key of a hash pattern (`in {name:}` binds `name`) and a named
/// capture in a regular expression literal (`/(?<name>…)/ =~ s` binds `name`).
fn local_bindings(scope: Node, source: &str) -> Option<HashSet<String>> {
    let mut bound = HashSet::new();
    let completed = walk_scope(scope, true, |node, slot| match node.kind() {
        "identifier" if slot.may_bind() => {
            bound.insert(get_node_text(node, source));
        }
        "keyword_pattern" if node.child_by_field_name("value").is_none() => {
            if let Some(key) = node.child_by_field_name("key") {
                let text = get_node_text(key, source);
                bound.insert(text.trim_matches(['"', '\'', ':']).to_string());
            }
        }
        "regex" => bound.extend(named_captures(&get_node_text(node, source))),
        _ => {}
    });
    completed.then_some(bound)
}

/// The group names a regular expression literal declares, in either of Ruby's
/// two spellings: `(?<name>…)` and `(?'name'…)`. The lookbehinds `(?<=` and
/// `(?<!` share the prefix and declare nothing.
fn named_captures(regex: &str) -> Vec<String> {
    let mut names = Vec::new();
    for (open, close) in [("(?<", '>'), ("(?'", '\'')] {
        let mut rest = regex;
        while let Some(at) = rest.find(open) {
            rest = &rest[at + open.len()..];
            let Some(end) = rest.find(close) else {
                break;
            };
            let name = &rest[..end];
            if is_callee_identity(name) {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// Receiverless sends that define methods on the class they are written in,
/// named by their symbol or string arguments. Every argument is collected —
/// including `def_delegators`' accessor and `delegate`'s `to:` target, which
/// define nothing — because an extra name only costs an edge.
const METHOD_DEFINING_MACROS: &[&str] = &[
    "attr",
    "attr_reader",
    "attr_writer",
    "attr_accessor",
    "define_method",
    "alias_method",
    "def_delegator",
    "def_delegators",
    "delegate",
];

/// Method names the classes and modules enclosing `scope` define without a
/// `def`, or `None` when the deadline cut a walk short.
///
/// Why this is a refusal (measured on Homebrew 2026-10-06): such a method has
/// no declaration in the graph, so a bare send naming it can only resolve to a
/// *different* method of that name. `TapCaskUnavailableError#to_s` reads its
/// own `attr_reader :tap`, and the resolver, finding exactly one `def tap` in
/// the corpus, bound it to `Cask.tap` at 0.9. Parenthesised calls never showed
/// this — nobody writes `tap()` on an attribute — so it arrived with the bare
/// send: 26 of the 126 new cross-class edges had an attribute of the callee's
/// name in the caller's own file, and 0 of the 807 such edges before it did.
/// Refusing costs no correct edge: with no `def` to bind, the only outcomes
/// were a wrong edge or none.
///
/// Covers the macros in `METHOD_DEFINING_MACROS` (also when wrapped, as in
/// `private attr_reader :x`), the `alias` keyword, and a `Struct.new(:x)` /
/// `Data.define(:x)` superclass, whose symbols become readers. Every enclosing
/// gate up to the file counts, though only the nearest class's would be
/// inherited — over-collection again.
///
/// Not covered, because the defining site is not an enclosing gate: an
/// attribute inherited from a superclass — even one in the same file, which is
/// why `CaskInvalidError#to_s`, inheriting `attr_reader :reason` from
/// `AbstractCaskErrorWithToken`, still binds to `Denylist.reason` (6 of the 26
/// remain) — one mixed in from a module, a splatted macro argument
/// (`def_delegators :@dsl, *METHODS`), and `method_missing`. The owner of the
/// whole class is the declaration emitter: an `attr_*` macro declares a
/// method, and once it is a symbol the resolver binds the send to it.
fn methods_defined_without_def(scope: Node, source: &str) -> Option<HashSet<String>> {
    let mut names = HashSet::new();
    let mut gate = Some(scope);
    while let Some(node) = gate {
        if matches!(node.kind(), "class" | "module" | "singleton_class") {
            let completed = walk_scope(node, false, |child, _| match child.kind() {
                "call" if is_method_defining_macro(child, source) => {
                    if let Some(arguments) = child.child_by_field_name("arguments") {
                        names.extend(literal_names(arguments, source));
                    }
                }
                "alias" => {
                    let mut cursor = child.walk();
                    for name in child.named_children(&mut cursor) {
                        names.insert(get_node_text(name, source));
                    }
                }
                _ => {}
            });
            if !completed {
                return None;
            }
            if let Some(superclass) = node.child_by_field_name("superclass") {
                let mut found = Vec::new();
                if !walk_scope(superclass, true, |child, _| {
                    if child.kind() == "argument_list" {
                        found.extend(literal_names(child, source));
                    }
                }) {
                    return None;
                }
                names.extend(found);
            }
        }
        if node.kind() == "program" {
            break;
        }
        gate = crate::treesitter::bounded_parent(node);
    }
    Some(names)
}

fn is_method_defining_macro(call: Node, source: &str) -> bool {
    call.child_by_field_name("receiver").is_none()
        && call.child_by_field_name("method").is_some_and(|method| {
            METHOD_DEFINING_MACROS.contains(&get_node_text(method, source).as_str())
        })
}

/// The names an argument list spells as literals: `:x`, `:"x"`, `'x'`, `"x"`.
fn literal_names(arguments: Node, source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut cursor = arguments.walk();
    for argument in arguments.named_children(&mut cursor) {
        if matches!(
            argument.kind(),
            "simple_symbol" | "delimited_symbol" | "string"
        ) {
            let text = get_node_text(argument, source);
            let name = text.trim_start_matches(':').trim_matches(['"', '\'']);
            if !name.is_empty() {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// Ruby's implicit block parameters, which read as bare identifiers.
fn is_implicit_block_parameter(name: &str, in_block: bool) -> bool {
    let numbered = matches!(name.as_bytes(), [b'_', b'1'..=b'9']);
    numbered || (in_block && name == "it")
}

/// Where a node sits: the kind of its parent, the field it fills there, and
/// what the walk has passed through on the way down.
#[derive(Clone, Copy)]
struct Slot {
    parent: &'static str,
    field: Option<&'static str>,
    in_block: bool,
    in_pattern: bool,
    in_defined: bool,
}

impl Slot {
    /// Whether an identifier here is a send this module may claim.
    fn is_read(self) -> bool {
        !self.in_defined && self.is_value()
    }

    /// Whether an identifier here is evaluated as a value. `defined?(x)` is
    /// one — it is never a binding — though it is not a send.
    fn is_value(self) -> bool {
        !self.in_pattern && is_value_position(self.parent, self.field)
    }

    /// Whether an identifier here may bind a local: anything not evaluated as
    /// a value, except the two names that are certainly not locals — the
    /// method a send names, and the method a `def` declares.
    fn may_bind(self) -> bool {
        !self.is_value()
            && !matches!(
                (self.parent, self.field),
                ("call", Some("method")) | ("method" | "singleton_method", Some("name"))
            )
    }

    /// The slot `child` fills under `node`, which itself filled `self`.
    fn descend(self, node: Node, field: Option<&'static str>) -> Slot {
        let kind = node.kind();
        let opens_pattern = self.field == Some("pattern")
            && matches!(self.parent, "in_clause" | "match_pattern" | "test_pattern");
        let is_defined = kind == "unary"
            && node
                .child_by_field_name("operator")
                .is_some_and(|operator| operator.kind() == "defined?");
        Slot {
            parent: kind,
            field,
            in_block: self.in_block || matches!(kind, "block" | "do_block"),
            in_pattern: self.in_pattern || opens_pattern,
            in_defined: self.in_defined || is_defined,
        }
    }
}

/// The slots in which an `identifier` is a value being read, from the
/// grammar's `node-types.json` (tree-sitter-ruby 0.23): every field or child
/// list whose type admits an expression. Anything absent is not a read.
fn is_value_position(parent: &str, field: Option<&str>) -> bool {
    match field {
        None => matches!(
            parent,
            "program"
                | "body_statement"
                | "block_body"
                | "begin"
                | "begin_block"
                | "end_block"
                | "then"
                | "else"
                | "ensure"
                | "do"
                | "parenthesized_statements"
                | "interpolation"
                | "argument_list"
                | "array"
                | "element_reference"
                | "right_assignment_list"
                | "splat_argument"
                | "hash_splat_argument"
                | "block_argument"
                | "exceptions"
                | "in"
                | "pattern"
        ),
        Some(field) => matches!(
            (parent, field),
            ("assignment" | "operator_assignment", "right")
                | ("binary", "left" | "right")
                | ("unary", "operand")
                | ("call", "receiver")
                | ("element_reference", "object")
                | (
                    "case" | "case_match" | "match_pattern" | "test_pattern",
                    "value"
                )
                | ("conditional", "condition" | "consequence" | "alternative")
                | (
                    "if" | "elsif" | "unless" | "while" | "until" | "if_guard" | "unless_guard",
                    "condition"
                )
                | (
                    "if_modifier" | "unless_modifier" | "while_modifier" | "until_modifier",
                    "body" | "condition"
                )
                | ("rescue_modifier", "body" | "handler")
                | ("pair", "key" | "value")
                | ("optional_parameter" | "keyword_parameter", "value")
                | ("method" | "singleton_method", "body")
                | ("range", "begin" | "end")
        ),
    }
}

/// Visit every named node under `scope` with the slot it fills, without
/// descending into nested scope gates unless `into_gates`.
///
/// An explicit worklist, because a recursive walk over a hostile file would
/// recurse as deep as the file nests. Returns `false` when the extraction
/// deadline cut the walk short, the same check `walk_tree` makes, so this
/// per-scope pass cannot run past the file's budget on its own.
fn walk_scope<'tree>(
    scope: Node<'tree>,
    into_gates: bool,
    mut visit: impl FnMut(Node<'tree>, Slot),
) -> bool {
    let root = Slot {
        parent: scope.kind(),
        field: None,
        in_block: false,
        in_pattern: false,
        in_defined: false,
    };
    let mut worklist: Vec<(Node<'tree>, Slot)> = Vec::new();
    push_named_children(scope, root, &mut worklist);
    let mut since_check = 0u32;
    while let Some((node, slot)) = worklist.pop() {
        since_check += 1;
        if since_check >= DEADLINE_CHECK_STRIDE {
            since_check = 0;
            if walk_deadline_passed() {
                return false;
            }
        }
        visit(node, slot);
        if !into_gates && is_scope_gate(node.kind()) {
            continue;
        }
        push_named_children(node, slot, &mut worklist);
    }
    true
}

/// Push `node`'s named children with their slots, in reverse so the worklist
/// pops them in source order.
fn push_named_children<'tree>(
    node: Node<'tree>,
    slot: Slot,
    worklist: &mut Vec<(Node<'tree>, Slot)>,
) {
    let mark = worklist.len();
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            if child.is_named() {
                worklist.push((child, slot.descend(node, cursor.field_name())));
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    worklist[mark..].reverse();
}
