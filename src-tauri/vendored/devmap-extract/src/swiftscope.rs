//! Swift lexical scopes: which names a callable binds itself, and the type each
//! one is written with.
//!
//! The shared binding passes in `treesitter` read `identifier` nodes and a
//! `parameters` list, and tree-sitter-swift has neither: every name is a
//! `simple_identifier`, parameters are direct children of the declaration, a
//! `let` is a `pattern` whose name sits on the `bound_identifier` field, and a
//! closure is a `lambda_literal`. So no Swift value was ever recorded as a
//! local, and a receiver such as `s` in `s.run()` fell through to the file-wide
//! `receiver_types["file:s"]` map, which another function's parameter had
//! written — `usesLocal` bound `Alpha.run` at 1.0 because `usesParam(_ s: Alpha)`
//! sat beside it (the audit's F2). A name bound by the enclosing scope is
//! authoritative: it either carries a written type here or it abstains.
//!
//! Only what the text states is typed: a parameter or annotated `let`, an
//! unwrapped optional (`guard let m = o`), the element of a collection a
//! `for` loop or an element closure (`xs.map { s in … }`) walks, and a generic
//! parameter, which is typed by its protocol bound (`T: Greeter` → `Greeter`).
//! Anything else is bound but untyped.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use tree_sitter::Node;

use crate::treesitter::{bounded_parent, get_node_text};

/// What a scope knows about one name it binds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Bound {
    /// The nominal type the source writes or the text implies; `None` when it
    /// states none.
    pub ty: Option<String>,
    /// The call shape a `let` is initialised from — `make`, `Factory.make` —
    /// which the resolver types by the callee's written return type.
    pub init: Option<String>,
}

/// A scope's bound names.
pub(crate) type Bindings = BTreeMap<String, Bound>;

thread_local! {
    static CACHE: RefCell<HashMap<usize, Rc<Bindings>>> = RefCell::new(HashMap::new());
}

/// Drop the per-scope cache. Node ids are arena addresses, reused once a tree
/// is dropped, so the cache must not outlive its file.
pub(crate) fn reset() {
    CACHE.with(|cache| cache.borrow_mut().clear());
}

/// Whether `node` opens a Swift scope that binds names.
pub(crate) fn is_scope(node: Node) -> bool {
    matches!(
        node.kind(),
        "function_declaration" | "init_declaration" | "deinit_declaration" | "lambda_literal"
    )
}

/// Methods whose closure argument receives one element of the receiver.
const ELEMENT_METHODS: &[&str] = &[
    "map",
    "compactMap",
    "flatMap",
    "filter",
    "forEach",
    "first",
    "contains",
    "allSatisfy",
    "sorted",
    "min",
    "max",
    "removeAll",
    "drop",
    "prefix",
    "firstIndex",
    "lastIndex",
    "last",
];

/// The bindings of `scope`, computed once per scope node.
pub(crate) fn bindings(scope: Node, source: &str) -> Rc<Bindings> {
    let id = scope.id();
    if let Some(known) = CACHE.with(|cache| cache.borrow().get(&id).cloned()) {
        return known;
    }
    let computed = Rc::new(compute(scope, source));
    CACHE.with(|cache| cache.borrow_mut().insert(id, Rc::clone(&computed)));
    computed
}

fn compute(scope: Node, source: &str) -> Bindings {
    let mut out = Bindings::new();
    let bounds = generic_bounds(scope, source);
    let mut cursor = scope.walk();
    let children: Vec<Node> = scope.named_children(&mut cursor).collect();
    if scope.kind() == "lambda_literal" {
        lambda_parameters(scope, source, &bounds, &mut out);
    } else {
        for child in &children {
            if child.kind() == "parameter" {
                parameter(*child, source, &bounds, &mut out);
            }
        }
    }
    if scope.kind() != "lambda_literal" {
        for member in function_typed_members(scope, source) {
            out.entry(member).or_default();
        }
    }
    let mut stack: Vec<Node> = children
        .into_iter()
        .filter(|child| child.kind() != "parameter")
        .collect();
    stack.reverse();
    while let Some(node) = stack.pop() {
        match node.kind() {
            "function_declaration" => {
                // A nested function is a name this scope binds. It is not a
                // module-level symbol, and a call to it is not a call of a
                // same-named function elsewhere.
                if let Some(name) = node.child_by_field_name("name") {
                    bind(&mut out, get_node_text(name, source), None);
                }
                continue;
            }
            "lambda_literal" | "init_declaration" | "deinit_declaration" => continue,
            "class_declaration" | "protocol_declaration" => continue,
            "property_declaration" => {
                property(node, source, &bounds, &mut out);
                continue;
            }
            "for_statement" => for_statement(node, source, &bounds, &mut out),
            "if_statement" | "guard_statement" | "while_statement" => {
                conditions(node, source, &mut out);
            }
            _ => {}
        }
        // Any other pattern binds a name with no type this pass can state:
        // `case let .some(x)`, `catch let e`, a tuple destructuring.
        let mut walker = node.walk();
        let mut named: Vec<Node> = node.named_children(&mut walker).collect();
        if node.kind() == "catch_block" {
            bind(&mut out, "error".to_string(), None);
        }
        named.reverse();
        for child in named {
            if child.kind() == "simple_identifier"
                && field_of(node, child) == Some("bound_identifier")
            {
                // Already bound, and typed, by the statement's own handler.
                out.entry(get_node_text(child, source)).or_default();
            } else {
                stack.push(child);
            }
        }
    }
    out
}

/// The field `child` occupies on `parent`.
fn field_of<'a>(parent: Node<'a>, child: Node<'a>) -> Option<&'static str> {
    let mut cursor = parent.walk();
    if !cursor.goto_first_child() {
        return None;
    }
    loop {
        if cursor.node().id() == child.id() {
            return cursor.field_name();
        }
        if !cursor.goto_next_sibling() {
            return None;
        }
    }
}

/// Record `name`. The first declaration in source order keeps its type: a later
/// shadowing `let` of the same name in an inner block must not retype uses that
/// precede it, and not knowing is the safe answer.
fn bind(out: &mut Bindings, name: String, ty: Option<String>) {
    if name.is_empty() || name == "_" {
        return;
    }
    match out.get_mut(&name) {
        Some(existing) => {
            if existing.ty != ty {
                existing.ty = None;
                existing.init = None;
            }
        }
        None => {
            out.insert(name, Bound { ty, init: None });
        }
    }
}

fn parameter(node: Node, source: &str, bounds: &BTreeMap<String, String>, out: &mut Bindings) {
    let mut name: Option<String> = None;
    let mut ty: Option<String> = None;
    for index in 0..node.child_count() {
        let Some(child) = node.child(index) else {
            continue;
        };
        if node.field_name_for_child(index as u32) != Some("name") {
            continue;
        }
        if child.kind() == "simple_identifier" && name.is_none() {
            name = Some(get_node_text(child, source));
        } else if child.kind() != "simple_identifier" {
            ty = normalize(child, source, bounds);
        }
    }
    if let Some(name) = name {
        bind(out, name, ty);
    }
}

fn lambda_parameters(
    scope: Node,
    source: &str,
    bounds: &BTreeMap<String, String>,
    out: &mut Bindings,
) {
    let mut explicit: Vec<(String, Option<String>)> = Vec::new();
    let mut cursor = scope.walk();
    for child in scope.named_children(&mut cursor) {
        if child.kind() != "lambda_function_type" {
            continue;
        }
        let mut inner = child.walk();
        for list in child.named_children(&mut inner) {
            if list.kind() != "lambda_function_type_parameters" {
                continue;
            }
            let mut params = list.walk();
            for param in list.named_children(&mut params) {
                if param.kind() != "lambda_parameter" {
                    continue;
                }
                let mut name = None;
                let mut ty = None;
                let mut fields = param.walk();
                for item in param.named_children(&mut fields) {
                    if item.kind() == "simple_identifier" && name.is_none() {
                        name = Some(get_node_text(item, source));
                    } else if item.kind() != "simple_identifier" {
                        ty = normalize(item, source, bounds);
                    }
                }
                if let Some(name) = name {
                    explicit.push((name, ty));
                }
            }
        }
    }
    let element = element_type(scope, source);
    match explicit.as_slice() {
        [] => bind(out, "$0".to_string(), element),
        [(name, ty)] => bind(out, name.clone(), ty.clone().or(element)),
        many => {
            for (name, ty) in many {
                bind(out, name.clone(), ty.clone());
            }
        }
    }
}

fn property(node: Node, source: &str, bounds: &BTreeMap<String, String>, out: &mut Bindings) {
    let mut pending: Vec<String> = Vec::new();
    let mut typed: Option<String> = None;
    let mut init: Option<String> = None;
    let flush = |pending: &mut Vec<String>,
                 typed: &mut Option<String>,
                 init: &mut Option<String>,
                 out: &mut Bindings| {
        let single = pending.len() == 1;
        for name in pending.drain(..) {
            bind(out, name.clone(), if single { typed.clone() } else { None });
            if let Some(entry) = out.get_mut(&name) {
                if single && entry.init.is_none() && entry.ty.is_none() {
                    entry.init = init.clone();
                }
            }
        }
        *typed = None;
        *init = None;
    };
    for index in 0..node.child_count() {
        let Some(child) = node.child(index) else {
            continue;
        };
        match (node.field_name_for_child(index as u32), child.kind()) {
            (Some("name"), "pattern") => {
                flush(&mut pending, &mut typed, &mut init, out);
                collect_bound(child, source, &mut pending);
            }
            (_, "type_annotation") => {
                let mut cursor = child.walk();
                typed = child
                    .named_children(&mut cursor)
                    .find_map(|inner| normalize(inner, source, bounds));
            }
            (Some("value"), "simple_identifier") if typed.is_none() && pending.len() == 1 => {
                typed = lookup_outer(node, source, &get_node_text(child, source), out);
            }
            (Some("value"), "call_expression") if pending.len() == 1 => {
                init = call_shape(child, source);
            }
            // `let c = a <+> b`: typed by the custom operator's declared result.
            (Some("value"), "infix_expression") if pending.len() == 1 => {
                init = child
                    .child_by_field_name("op")
                    .filter(|op| op.kind() == "custom_operator")
                    .map(|op| get_node_text(op, source));
            }
            _ => {}
        }
    }
    flush(&mut pending, &mut typed, &mut init, out);
}

/// `make(…)` → `make`; `Factory.make(…)` → `Factory.make`: the shapes the
/// resolver types by the callee's written return type. A constructor call has
/// the same spelling and is typed by the constructed type instead.
fn call_shape(call: Node, source: &str) -> Option<String> {
    let mut cursor = call.walk();
    let callee = call
        .named_children(&mut cursor)
        .find(|child| child.kind() != "call_suffix")?;
    match callee.kind() {
        "simple_identifier" => Some(get_node_text(callee, source)),
        "navigation_expression" => {
            let target = callee.child_by_field_name("target")?;
            let name = callee
                .child_by_field_name("suffix")?
                .child_by_field_name("suffix")?;
            (target.kind() == "simple_identifier").then(|| {
                format!(
                    "{}.{}",
                    get_node_text(target, source),
                    get_node_text(name, source)
                )
            })
        }
        _ => None,
    }
}

fn collect_bound(pattern: Node, source: &str, out: &mut Vec<String>) {
    let mut stack = vec![pattern];
    while let Some(node) = stack.pop() {
        if node.kind() == "simple_identifier" && field_of_parent(node) == Some("bound_identifier") {
            out.push(get_node_text(node, source));
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    out.reverse();
}

fn field_of_parent(node: Node) -> Option<&'static str> {
    let parent = bounded_parent(node)?;
    field_of(parent, node)
}

fn for_statement(node: Node, source: &str, _bounds: &BTreeMap<String, String>, out: &mut Bindings) {
    let mut names = Vec::new();
    if let Some(item) = node.child_by_field_name("item") {
        collect_bound(item, source, &mut names);
    }
    let element = node
        .child_by_field_name("collection")
        .filter(|collection| collection.kind() == "simple_identifier")
        .and_then(|collection| lookup_outer(node, source, &get_node_text(collection, source), out))
        .and_then(|ty| {
            ty.strip_prefix('[')
                .and_then(|t| t.strip_suffix(']'))
                .map(str::to_string)
        });
    let single = names.len() == 1;
    for name in names {
        bind(out, name, if single { element.clone() } else { None });
    }
}

/// `if let x = y`, `guard let x = y`, `if let x`: each `bound_identifier` is
/// typed by the identifier that follows it when that identifier has a type.
fn conditions(node: Node, source: &str, out: &mut Bindings) {
    let mut pending: Option<String> = None;
    let resolved = |name: String, value: Option<Node>, out: &mut Bindings| {
        let ty = match value {
            Some(value) if value.kind() == "simple_identifier" => {
                lookup_outer(node, source, &get_node_text(value, source), out)
            }
            Some(_) => None,
            None => lookup_outer(node, source, &name, out),
        };
        bind(out, name, ty);
    };
    for index in 0..node.child_count() {
        let Some(child) = node.child(index) else {
            continue;
        };
        match node.field_name_for_child(index as u32) {
            Some("bound_identifier") => {
                if let Some(name) = pending.take() {
                    resolved(name, None, out);
                }
                pending = Some(get_node_text(child, source));
            }
            Some("condition") if child.is_named() && child.kind() != "value_binding_pattern" => {
                if let Some(name) = pending.take() {
                    resolved(name, Some(child), out);
                }
            }
            _ => {}
        }
    }
    if let Some(name) = pending.take() {
        resolved(name, None, out);
    }
}

/// The type `name` has where `from` sits: the bindings collected so far in this
/// scope, then each enclosing scope's. Optionals are already unwrapped.
fn lookup_outer(from: Node, source: &str, name: &str, current: &Bindings) -> Option<String> {
    if let Some(found) = current.get(name) {
        return found.ty.clone();
    }
    let mut ancestor = bounded_parent(from);
    while let Some(scope) = ancestor {
        if is_scope(scope) {
            let found = bindings(scope, source);
            if let Some(found) = found.get(name) {
                return found.ty.clone();
            }
        }
        ancestor = bounded_parent(scope);
    }
    None
}

/// The element a closure passed to `xs.map { … }` receives, when `xs` is a
/// collection whose type is written.
fn element_type(lambda: Node, source: &str) -> Option<String> {
    let suffix = bounded_parent(lambda).filter(|n| n.kind() == "call_suffix")?;
    let call = bounded_parent(suffix).filter(|n| n.kind() == "call_expression")?;
    let mut cursor = call.walk();
    let callee = call
        .named_children(&mut cursor)
        .find(|child| child.kind() != "call_suffix")?;
    if callee.kind() != "navigation_expression" {
        return None;
    }
    let method = callee
        .child_by_field_name("suffix")?
        .child_by_field_name("suffix")
        .map(|node| get_node_text(node, source))?;
    if !ELEMENT_METHODS.contains(&method.as_str()) {
        return None;
    }
    let target = callee.child_by_field_name("target")?;
    if target.kind() != "simple_identifier" {
        return None;
    }
    let collection = lookup_outer(
        lambda,
        source,
        &get_node_text(target, source),
        &Bindings::new(),
    )?;
    collection
        .strip_prefix('[')
        .and_then(|t| t.strip_suffix(']'))
        .map(str::to_string)
}

/// The stored properties of the type `scope` is a member of whose type is a
/// function: `let onDone: () -> Void`. `onDone()` written in a method of that
/// type calls the property — implicit `self` — never a free function that
/// happens to share the name, so the name is bound here like a local. The
/// scope's own parameters and locals shadow it, because they are inserted first.
fn function_typed_members(scope: Node, source: &str) -> Vec<String> {
    let mut owner = bounded_parent(scope);
    while let Some(node) = owner {
        if node.kind() == "class_declaration" {
            break;
        }
        if node.kind() == "function_declaration" || node.kind() == "source_file" {
            return Vec::new();
        }
        owner = bounded_parent(node);
    }
    let Some(body) = owner.and_then(|node| node.child_by_field_name("body")) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    let mut cursor = body.walk();
    for member in body.named_children(&mut cursor) {
        if member.kind() != "property_declaration" {
            continue;
        }
        let mut inner = member.walk();
        let function_typed = member.named_children(&mut inner).any(|child| {
            child.kind() == "type_annotation" && {
                let mut types = child.walk();
                let found = child
                    .named_children(&mut types)
                    .any(|ty| is_function_type(ty));
                found
            }
        });
        if function_typed {
            if let Some(pattern) = member.child_by_field_name("name") {
                collect_bound(pattern, source, &mut names);
            }
        }
    }
    names
}

/// `() -> Void`, `((Int) -> Int)?`, `(() -> Void)!`.
fn is_function_type(node: Node) -> bool {
    match node.kind() {
        "function_type" => true,
        "optional_type" | "implicitly_unwrapped_type" | "parenthesized_type" => {
            let mut cursor = node.walk();
            let found = node.named_children(&mut cursor).any(is_function_type);
            found
        }
        _ => false,
    }
}

/// `T` → its protocol bound, for every generic parameter the declaration lists
/// and for each `where T: P` clause.
fn generic_bounds(scope: Node, source: &str) -> BTreeMap<String, String> {
    let mut bounds = BTreeMap::new();
    let mut cursor = scope.walk();
    for child in scope.named_children(&mut cursor) {
        match child.kind() {
            "type_parameters" => {
                let mut params = child.walk();
                for param in child.named_children(&mut params) {
                    if param.kind() != "type_parameter" {
                        continue;
                    }
                    let mut name = None;
                    let mut bound = None;
                    let mut inner = param.walk();
                    for item in param.named_children(&mut inner) {
                        match item.kind() {
                            "type_identifier" if name.is_none() => {
                                name = Some(get_node_text(item, source));
                            }
                            "user_type" => bound = user_type_name(item, source),
                            _ => {}
                        }
                    }
                    if let (Some(name), Some(bound)) = (name, bound) {
                        bounds.insert(name, bound);
                    }
                }
            }
            "type_constraints" => {
                let mut items = child.walk();
                for constraint in child.named_children(&mut items) {
                    let mut inner = constraint.walk();
                    let mut name = None;
                    let mut bound = None;
                    for item in constraint.named_children(&mut inner) {
                        match item.kind() {
                            "identifier" | "type_identifier" | "user_type" if name.is_none() => {
                                name = Some(get_node_text(item, source));
                            }
                            "user_type" => bound = user_type_name(item, source),
                            _ => {}
                        }
                    }
                    if let (Some(name), Some(bound)) = (name, bound) {
                        bounds.insert(name, bound);
                    }
                }
            }
            _ => {}
        }
    }
    bounds
}

fn user_type_name(node: Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let name = node
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "type_identifier")
        .last()
        .map(|child| get_node_text(child, source));
    name
}

/// The type a written type node names: a nominal name, `[Name]` for an array,
/// `None` for anything a method cannot be dispatched on (functions, tuples,
/// dictionaries). Optionals are unwrapped — `m?.bump()` and `guard let` both
/// reach the wrapped type — and a generic parameter is its protocol bound.
fn normalize(node: Node, source: &str, bounds: &BTreeMap<String, String>) -> Option<String> {
    match node.kind() {
        "user_type" => {
            let name = user_type_name(node, source)?;
            Some(bounds.get(&name).cloned().unwrap_or(name))
        }
        "optional_type"
        | "implicitly_unwrapped_type"
        | "opaque_type"
        | "existential_type"
        | "parenthesized_type" => {
            let mut cursor = node.walk();
            let inner = node
                .named_children(&mut cursor)
                .find_map(|child| normalize(child, source, bounds));
            inner
        }
        "array_type" => {
            let mut cursor = node.walk();
            let element = node
                .named_children(&mut cursor)
                .find_map(|child| normalize(child, source, bounds))?;
            Some(format!("[{element}]"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bound(source: &str, function: &str) -> Bindings {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_swift::LANGUAGE.into())
            .expect("swift grammar");
        let tree = parser.parse(source, None).expect("parse");
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "function_declaration"
                && node
                    .child_by_field_name("name")
                    .is_some_and(|name| get_node_text(name, source) == function)
            {
                return (*bindings(node, source)).clone();
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                stack.push(child);
            }
        }
        panic!("no function {function}");
    }

    #[test]
    fn parameters_annotations_unwraps_and_loops_are_typed() {
        reset();
        let source = "func f(_ m: Meter, o: Meter?, xs: [Beta]) {\n\
            let g: Gauge = .zero\n\
            guard let q = o else { return }\n\
            if let o { o.x() }\n\
            for x in xs { x.run() }\n\
            let copy = m\n\
            let untyped = make()\n\
        }";
        let found = bound(source, "f");
        assert_eq!(
            found.get("m").and_then(|b| b.ty.clone()),
            Some("Meter".to_string())
        );
        assert_eq!(
            found.get("o").and_then(|b| b.ty.clone()),
            Some("Meter".to_string())
        );
        assert_eq!(
            found.get("xs").and_then(|b| b.ty.clone()),
            Some("[Beta]".to_string())
        );
        assert_eq!(
            found.get("g").and_then(|b| b.ty.clone()),
            Some("Gauge".to_string())
        );
        assert_eq!(
            found.get("q").and_then(|b| b.ty.clone()),
            Some("Meter".to_string())
        );
        assert_eq!(
            found.get("x").and_then(|b| b.ty.clone()),
            Some("Beta".to_string())
        );
        assert_eq!(
            found.get("copy").and_then(|b| b.ty.clone()),
            Some("Meter".to_string())
        );
        let untyped = found.get("untyped").expect("bound");
        assert_eq!(untyped.ty, None, "a value with no written type is untyped");
        assert_eq!(untyped.init.as_deref(), Some("make"));
    }

    #[test]
    fn a_generic_parameter_is_its_protocol_bound() {
        reset();
        let found = bound("func loud<T: Greeter>(_ xs: [T], one: T) {}", "loud");
        assert_eq!(
            found.get("xs").and_then(|b| b.ty.clone()),
            Some("[Greeter]".to_string())
        );
        assert_eq!(
            found.get("one").and_then(|b| b.ty.clone()),
            Some("Greeter".to_string())
        );
    }

    #[test]
    fn a_name_bound_twice_with_different_types_is_untyped() {
        reset();
        let found = bound(
            "func f() { let a: One = x(); if true { let a: Two = y() } }",
            "f",
        );
        assert_eq!(found.get("a").map(|b| b.ty.clone()), Some(None));
    }

    #[test]
    fn a_function_typed_member_is_a_name_its_methods_bind() {
        reset();
        let found = bound(
            "final class Job { let onDone: () -> Void; var count = 0; func finish() { onDone() } }",
            "finish",
        );
        assert!(found.contains_key("onDone"));
        assert!(
            !found.contains_key("count"),
            "an ordinary property is not a function value"
        );
    }

    #[test]
    fn a_nested_function_is_a_local_name() {
        reset();
        let found = bound("func outer() { func helper() {} helper() }", "outer");
        assert!(found.contains_key("helper"));
    }
}
