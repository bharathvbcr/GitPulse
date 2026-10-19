//! How a JS/TS module-scope object is built.
//!
//! ```ts
//! export const debugLogger = { ...coreDebugLogger, clear: clearLogs };
//! const w = Object.assign({}, core);
//! export const aliased = core;
//! ```
//!
//! Each of these gives the const members its own literal never spells. The
//! resolver can only say which declaration a member call reaches if it knows
//! the *order* the properties were applied in — JavaScript lets the last write
//! win — so this pass records the steps, in source order, and decides nothing.
//! Which file an identifier names, and whether a key is defined by a later
//! source, are the resolver's questions: they depend on the rest of the tree,
//! and the extraction cache keys on `(path, source)` alone.
//!
//! Only a `const` is recorded: a `let` or `var` can be rebound, and the
//! resolver would then be reading a table the program replaced. Anything this
//! pass cannot read statically — a spread of a call's result, a computed key, a
//! conditional source — is recorded as [`JsObjectPart::Unknown`], never
//! skipped, so a reader cannot take its absence for "nothing else was written".

use crate::model::{JsObjectFact, JsObjectPart};
use crate::treesitter::{is_module_level, push_named_children, walk_deadline_passed};
use std::collections::HashSet;
use tree_sitter::Node;

/// How deep object literals may nest inside a spread or `Object.assign` source
/// before the rest is read as unknown. A real spread chain is one or two deep.
const MAX_NESTING: usize = 8;
/// Nodes between deadline checks.
const DEADLINE_STRIDE: usize = 4096;

/// Two passes: the declarations first, because only a name some declaration
/// introduced can be written through; then every occurrence of those names,
/// each classified as a write, an escape, or a harmless read.
pub(crate) fn collect(root: Node, source: &str) -> Vec<JsObjectFact> {
    let mut facts = Vec::new();
    let mut tracked: HashSet<String> = HashSet::new();
    if !walk(root, |node| {
        if node.kind() == "variable_declarator" {
            if let Some(fact) = declaration_fact(node, source) {
                tracked.insert(fact.name.clone());
                facts.push(fact);
            }
        }
    }) {
        return Vec::new();
    }
    if !tracked.is_empty()
        && !walk(root, |node| {
            if matches!(node.kind(), "identifier" | "shorthand_property_identifier")
                && tracked.contains(text(node, source))
            {
                if let Some(fact) = occurrence_fact(node, source) {
                    facts.push(fact);
                }
            }
        })
    {
        return Vec::new();
    }
    facts.sort_by_key(|fact| (fact.start_byte, !fact.declaration));
    facts
}

/// Pre-order walk over named nodes; `false` when the walk deadline passed.
fn walk<'tree>(root: Node<'tree>, mut visit: impl FnMut(Node<'tree>)) -> bool {
    let mut worklist = vec![root];
    let mut visited = 0usize;
    while let Some(node) = worklist.pop() {
        visited += 1;
        if visited.is_multiple_of(DEADLINE_STRIDE) && walk_deadline_passed() {
            return false;
        }
        visit(node);
        let mark = worklist.len();
        push_named_children(node, &mut worklist);
        worklist[mark..].reverse();
    }
    true
}

fn text<'a>(node: Node, source: &'a str) -> &'a str {
    source.get(node.start_byte()..node.end_byte()).unwrap_or("")
}

fn is_wrapper(kind: &str) -> bool {
    matches!(
        kind,
        "as_expression"
            | "satisfies_expression"
            | "parenthesized_expression"
            | "non_null_expression"
            | "type_assertion"
    )
}

/// `as T`, `satisfies T`, `<T>x`, `(…)` and `x!` do not change which value an
/// expression is.
fn unwrap(mut node: Node) -> Node {
    while is_wrapper(node.kind()) {
        // `<T>x` writes its type first; every other wrapper writes the value
        // first.
        let inner = if node.kind() == "type_assertion" {
            node.named_child(node.named_child_count().saturating_sub(1))
        } else {
            node.named_child(0)
        };
        match inner {
            Some(inner) => node = inner,
            None => break,
        }
    }
    node
}

/// The outermost wrapper around an expression: the node whose parent decides
/// what the expression is used for.
fn outermost(mut node: Node) -> Node {
    while let Some(parent) = node.parent() {
        if is_wrapper(parent.kind()) {
            node = parent;
        } else {
            break;
        }
    }
    node
}

/// Whether a statement runs inside a callable (any number of times, or never)
/// and whether it runs conditionally at module level.
fn run_context(node: Node) -> (bool, bool) {
    let mut conditional = false;
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        match parent.kind() {
            "function_declaration"
            | "function_expression"
            | "function"
            | "generator_function"
            | "generator_function_declaration"
            | "arrow_function"
            | "method_definition"
            | "class_static_block"
            | "public_field_definition" => return (true, conditional),
            "if_statement" | "ternary_expression" | "for_statement" | "for_in_statement"
            | "while_statement" | "do_statement" | "switch_case" | "switch_default"
            | "try_statement" | "catch_clause" | "finally_clause" => conditional = true,
            "binary_expression" => {
                let operator = parent
                    .child_by_field_name("operator")
                    .map(|operator| operator.kind())
                    .unwrap_or("");
                if matches!(operator, "&&" | "||" | "??") {
                    conditional = true;
                }
            }
            _ => {}
        }
        ancestor = parent.parent();
    }
    (false, conditional)
}

/// A string-literal property key: `'info'` in `w['info']`, `Reflect.set(w,
/// 'info', …)`. `None` for anything computed.
fn literal_key(node: Node, source: &str) -> Option<String> {
    let node = unwrap(node);
    match node.kind() {
        "string" => key_text(node, source),
        "property_identifier" => Some(text(node, source).to_string()),
        _ => None,
    }
}

/// Calls that read an object without changing it or letting it out.
const HARMLESS_STATICS: &[&str] = &[
    "Object.freeze",
    "Object.seal",
    "Object.preventExtensions",
    "Object.isFrozen",
    "Object.isSealed",
    "Object.isExtensible",
    "Object.keys",
    "Object.values",
    "Object.entries",
    "Object.getOwnPropertyNames",
    "Object.getOwnPropertyDescriptor",
    "Object.getOwnPropertyDescriptors",
    "Object.getPrototypeOf",
    "Object.hasOwn",
    "Object.is",
    "Reflect.has",
    "Reflect.get",
    "Reflect.ownKeys",
    "JSON.stringify",
    "structuredClone",
];

/// What one occurrence of a tracked name does to the object it names: a write
/// to a member, a write the extraction cannot read, an escape into code that
/// may write it — or nothing (`None`): a read, the declaration itself, or an
/// alias declaration, which the resolver follows instead.
///
/// An escape (`f(w)`, `return w`, `{ k: w }`, `x.y = w`, a non-const alias) is
/// an unreadable write at that point: anything holding the object may add or
/// replace any member. Exporting a const is *not* treated as an escape —
/// importers' writes are in other files and the extraction cannot see them, and
/// reading every exported singleton as unknowable would give up the answer the
/// card exists for; that cross-file write stays an unverified limit.
fn occurrence_fact(ident: Node, source: &str) -> Option<JsObjectFact> {
    let name = text(ident, source).to_string();
    let node = outermost(ident);
    let parent = node.parent()?;
    let write = |parts: Vec<JsObjectPart>| {
        let (in_callable, conditional) = run_context(ident);
        Some(JsObjectFact {
            name: name.clone(),
            start_byte: ident.start_byte(),
            declaration: false,
            in_callable,
            conditional,
            alias_of: None,
            parts,
        })
    };
    let is_child = |field: &str, of: Node| {
        of.child_by_field_name(field)
            .is_some_and(|child| child.id() == node.id())
    };
    let key_write = |key: Option<String>| match key {
        Some(key) => JsObjectPart::Opaque(key),
        None => JsObjectPart::Unknown,
    };
    match parent.kind() {
        _ if ident.kind() == "shorthand_property_identifier" => write(vec![JsObjectPart::Unknown]),
        "variable_declarator" => {
            if is_child("name", parent) {
                return None;
            }
            // `const alias = w` at module scope is an alias, followed by the
            // resolver; any other binding of the value lets it out.
            if declaration_fact(parent, source).is_some() {
                None
            } else {
                write(vec![JsObjectPart::Unknown])
            }
        }
        "member_expression" | "subscript_expression" if is_child("object", parent) => {
            let key = if parent.kind() == "member_expression" {
                parent
                    .child_by_field_name("property")
                    .filter(|property| property.kind() == "property_identifier")
                    .map(|property| text(property, source).to_string())
            } else {
                parent
                    .child_by_field_name("index")
                    .and_then(|index| literal_key(index, source))
            };
            let access = outermost(parent);
            let grand = access.parent()?;
            match grand.kind() {
                "assignment_expression"
                    if grand
                        .child_by_field_name("left")
                        .is_some_and(|left| left.id() == access.id()) =>
                {
                    let plain = grand
                        .child_by_field_name("operator")
                        .is_none_or(|operator| operator.kind() == "=");
                    let forwarded = grand
                        .child_by_field_name("right")
                        .map(unwrap)
                        .filter(|right| right.kind() == "identifier");
                    match (plain, key, forwarded) {
                        (true, Some(key), Some(value)) => write(vec![JsObjectPart::Forward {
                            key,
                            value: text(value, source).to_string(),
                        }]),
                        (_, key, _) => write(vec![key_write(key)]),
                    }
                }
                "augmented_assignment_expression" | "update_expression" => {
                    write(vec![key_write(key)])
                }
                "unary_expression"
                    if grand
                        .child_by_field_name("operator")
                        .is_some_and(|operator| operator.kind() == "delete") =>
                {
                    write(vec![key_write(key)])
                }
                _ => None,
            }
        }
        "arguments" => {
            let call = parent.parent()?;
            let first = parent
                .named_child(0)
                .is_some_and(|argument| argument.id() == node.id());
            if call.kind() != "call_expression" {
                // `new Holder(w)` lets it out like any call.
                return write(vec![JsObjectPart::Unknown]);
            }
            let callee = call
                .child_by_field_name("function")
                .map(|function| text(function, source))
                .unwrap_or("");
            if callee == "Object.assign" {
                if !first {
                    return None;
                }
                let mut parts = Vec::new();
                let mut cursor = parent.walk();
                for argument in parent.named_children(&mut cursor).skip(1) {
                    parts.extend(source_parts(argument, source, 0));
                }
                return write(parts);
            }
            if HARMLESS_STATICS.contains(&callee) {
                return None;
            }
            if !first {
                // `Object.defineProperty(other, k, w)` and `f(x, w)`: the
                // object is a value handed on.
                return write(vec![JsObjectPart::Unknown]);
            }
            match callee {
                "Object.defineProperty"
                | "Reflect.defineProperty"
                | "Reflect.set"
                | "Reflect.deleteProperty" => write(vec![key_write(
                    parent
                        .named_child(1)
                        .and_then(|key| literal_key(key, source)),
                )]),
                "Object.defineProperties" => {
                    let keys = parent
                        .named_child(1)
                        .map(unwrap)
                        .filter(|descriptors| descriptors.kind() == "object")
                        .map(|descriptors| {
                            let mut cursor = descriptors.walk();
                            descriptors
                                .named_children(&mut cursor)
                                .map(|member| match member.kind() {
                                    "pair" => key_write(
                                        member
                                            .child_by_field_name("key")
                                            .and_then(|key| key_text(key, source)),
                                    ),
                                    "shorthand_property_identifier" => {
                                        JsObjectPart::Opaque(text(member, source).to_string())
                                    }
                                    _ => JsObjectPart::Unknown,
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_else(|| vec![JsObjectPart::Unknown]);
                    write(keys)
                }
                _ => write(vec![JsObjectPart::Unknown]),
            }
        }
        "arrow_function" if is_child("body", parent) => write(vec![JsObjectPart::Unknown]),
        "pair"
        | "array"
        | "return_statement"
        | "yield_expression"
        | "sequence_expression"
        | "ternary_expression" => write(vec![JsObjectPart::Unknown]),
        "assignment_expression" | "augmented_assignment_expression"
            if is_child("right", parent) =>
        {
            write(vec![JsObjectPart::Unknown])
        }
        "binary_expression" => {
            let operator = parent
                .child_by_field_name("operator")
                .map(|operator| operator.kind())
                .unwrap_or("");
            matches!(operator, "&&" | "||" | "??")
                .then(|| write(vec![JsObjectPart::Unknown]))
                .flatten()
        }
        _ => None,
    }
}

/// `name` or `ns.name`: the two spellings the resolver can follow.
fn reference_text(node: Node, source: &str) -> Option<String> {
    match node.kind() {
        "identifier" => Some(text(node, source).to_string()),
        "member_expression" => {
            let object = node.child_by_field_name("object")?;
            let property = node.child_by_field_name("property")?;
            let optional = node
                .children(&mut node.walk())
                .any(|child| child.kind() == "optional_chain");
            (object.kind() == "identifier" && property.kind() == "property_identifier" && !optional)
                .then(|| format!("{}.{}", text(object, source), text(property, source)))
        }
        _ => None,
    }
}

fn declaration_fact(declarator: Node, source: &str) -> Option<JsObjectFact> {
    let name = declarator.child_by_field_name("name")?;
    if name.kind() != "identifier" {
        return None;
    }
    let is_const = declarator
        .parent()
        .filter(|parent| parent.kind() == "lexical_declaration")
        .and_then(|parent| parent.child(0))
        .is_some_and(|keyword| keyword.kind() == "const");
    if !is_const || !is_module_level(declarator) {
        return None;
    }
    let value = unwrap(declarator.child_by_field_name("value")?);
    let (parts, alias_of) = match value.kind() {
        // The one place a function written in place is the symbol
        // `name.key`: the literal is the declarator's own value.
        "object" => (object_parts(value, source, true, 0), None),
        "call_expression" => {
            let parts = assign_parts(value, source, 0)?;
            // `Object.assign(core, …)` returns `core` itself.
            let target = value
                .child_by_field_name("arguments")
                .and_then(|arguments| arguments.named_child(0))
                .map(unwrap)
                .filter(|target| target.kind() == "identifier")
                .map(|target| text(target, source).to_string());
            (parts, target)
        }
        _ => {
            let reference = reference_text(value, source)?;
            (
                vec![JsObjectPart::Spread(reference.clone())],
                Some(reference),
            )
        }
    };
    Some(JsObjectFact {
        name: text(name, source).to_string(),
        start_byte: declarator.start_byte(),
        declaration: true,
        in_callable: false,
        conditional: false,
        alias_of,
        parts,
    })
}

fn is_object_assign(call: Node, source: &str) -> bool {
    call.child_by_field_name("function")
        .is_some_and(|function| {
            function.kind() == "member_expression"
                && function
                    .child_by_field_name("object")
                    .is_some_and(|object| text(object, source) == "Object")
                && function
                    .child_by_field_name("property")
                    .is_some_and(|property| text(property, source) == "assign")
        })
}

/// The parts of `Object.assign(target, ...sources)` as one new object: the
/// target is the first step, each source after it. `None` when `node` is not
/// an `Object.assign` call.
fn assign_parts(call: Node, source: &str, depth: usize) -> Option<Vec<JsObjectPart>> {
    if !is_object_assign(call, source) {
        return None;
    }
    let arguments = call.child_by_field_name("arguments")?;
    let mut parts = Vec::new();
    let mut cursor = arguments.walk();
    for argument in arguments.named_children(&mut cursor) {
        parts.extend(source_parts(argument, source, depth));
    }
    if parts.is_empty() {
        parts.push(JsObjectPart::Unknown);
    }
    Some(parts)
}

/// What one spread element or `Object.assign` argument contributes.
fn source_parts(argument: Node, source: &str, depth: usize) -> Vec<JsObjectPart> {
    // `Object.assign({}, ...list)` and `{ ...a }` both name their source as a
    // child of a spread; the argument itself is the source otherwise.
    let expression = if argument.kind() == "spread_element" {
        // A spread *argument* is a list of sources, which is not nameable.
        let in_arguments = argument.parent().is_some_and(|p| p.kind() == "arguments");
        match argument.named_child(0) {
            Some(inner) if !in_arguments => inner,
            _ => return vec![JsObjectPart::Unknown],
        }
    } else {
        argument
    };
    let expression = unwrap(expression);
    if expression.kind() == "object" {
        if depth >= MAX_NESTING {
            return vec![JsObjectPart::Unknown];
        }
        // A literal nested in a source is no declarator's own value, so its
        // functions are not the symbols `name.key` — they are opaque.
        return object_parts(expression, source, false, depth + 1);
    }
    match reference_text(expression, source) {
        Some(reference) => vec![JsObjectPart::Spread(reference)],
        None => vec![JsObjectPart::Unknown],
    }
}

fn object_parts(object: Node, source: &str, owned: bool, depth: usize) -> Vec<JsObjectPart> {
    let mut parts = Vec::new();
    let mut cursor = object.walk();
    for member in object.named_children(&mut cursor) {
        match member.kind() {
            "comment" => {}
            "spread_element" => parts.extend(source_parts(member, source, depth)),
            "shorthand_property_identifier" => {
                let name = text(member, source).to_string();
                parts.push(JsObjectPart::Forward {
                    key: name.clone(),
                    value: name,
                });
            }
            "method_definition" => match member
                .child_by_field_name("name")
                .and_then(|name| key_text(name, source))
            {
                // `get k() {}` / `set k(v) {}`: a call through `w.k` runs what
                // the getter *returns*, never the accessor itself.
                Some(key)
                    if member
                        .children(&mut member.walk())
                        .any(|child| matches!(child.kind(), "get" | "set")) =>
                {
                    parts.push(JsObjectPart::Opaque(key))
                }
                Some(key) if owned => parts.push(JsObjectPart::Inline(key)),
                Some(key) => parts.push(JsObjectPart::Opaque(key)),
                None => parts.push(JsObjectPart::Unknown),
            },
            "pair" => {
                let Some(key) = member
                    .child_by_field_name("key")
                    .and_then(|key| key_text(key, source))
                else {
                    parts.push(JsObjectPart::Unknown);
                    continue;
                };
                let value = member.child_by_field_name("value").map(unwrap);
                parts.push(match value {
                    Some(value)
                        if matches!(
                            value.kind(),
                            "arrow_function"
                                | "function_expression"
                                | "function"
                                | "generator_function"
                        ) =>
                    {
                        if owned {
                            JsObjectPart::Inline(key)
                        } else {
                            JsObjectPart::Opaque(key)
                        }
                    }
                    Some(value) if value.kind() == "identifier" && key != "__proto__" => {
                        JsObjectPart::Forward {
                            key,
                            value: text(value, source).to_string(),
                        }
                    }
                    _ if key == "__proto__" => JsObjectPart::Unknown,
                    _ => JsObjectPart::Opaque(key),
                });
            }
            _ => parts.push(JsObjectPart::Unknown),
        }
    }
    parts
}

/// The static name of a property key; `None` for a computed one.
fn key_text(key: Node, source: &str) -> Option<String> {
    match key.kind() {
        "property_identifier" | "number" => Some(text(key, source).to_string()),
        "string" => {
            let raw = text(key, source);
            let inner = raw.get(1..raw.len().checked_sub(1)?)?;
            (!inner.contains('\\')).then(|| inner.to_string())
        }
        _ => None,
    }
}
