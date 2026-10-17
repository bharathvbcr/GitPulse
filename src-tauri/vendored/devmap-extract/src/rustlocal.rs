//! The type of a Rust local, read off its own binder when the function's text
//! states it.
//!
//! ```rust,ignore
//! fn skippable(shared: &Mutex<Collected>, rel: &str) -> bool {
//!     match shared.lock() {
//!         Ok(guard) => guard.can_skip(rel),
//!         Err(poisoned) => poisoned.into_inner().can_skip(rel),
//!     }
//! }
//! ```
//!
//! `guard` is a `MutexGuard<Collected>`, which derefs to `Collected`, so
//! `guard.can_skip` is `Collected::can_skip`. Nothing in the extraction said
//! so: the receiver-typing facts are keyed by `(scope, name)` and fed by Type
//! references carrying `assigned_to`, which can neither see a match arm nor
//! carry a generic (`Vec<Collected>` reaches them as `Vec`). The method was
//! reported dead with its caller in plain view.
//!
//! This pass answers from the **binder** — the nearest pattern that introduces
//! the name above its use — and the binder's own type node. It states only
//! what the language does:
//!
//! * a parameter or `let` with a written type has that type;
//! * `x.lock()` on a `Mutex<T>` (and `x.read()` / `x.write()` on an
//!   `RwLock<T>`), unwrapped by `?`, `.unwrap()`, `.expect(..)` or an `Ok(g)`
//!   pattern, is a guard that derefs to `T`;
//! * the loop variable of `for x in xs` — through `&xs`, `&mut xs`,
//!   `.iter()`, `.iter_mut()` or `.into_iter()` — over a `Vec<T>`,
//!   `VecDeque<T>`, `HashSet<T>`, `BTreeSet<T>`, `[T; N]` or `[T]` is a `T`.
//!
//! `Arc`, `Rc` and `Box` are seen through on the way, because they are
//! `Deref` (see [`crate::deref`]). `Mutex` itself is not: a `Mutex<T>` has no
//! `T` methods until it is locked, and a `Result` has none until it is
//! unwrapped, so a lock that is never unwrapped types nothing. Anything else —
//! a closure parameter, a destructuring pattern, an untyped `let` whose value
//! is not one of these shapes — answers `None`, which leaves the existing facts
//! to answer exactly as before.

use crate::treesitter::{bounded_parent, get_node_text, rust_type_name};
use std::collections::HashMap;
use tree_sitter::Node;

/// How many ancestors a binder search climbs, and how deep a type is peeled.
///
/// Both walks are over one function's syntax, so the bound is never reached by
/// real code; it is here so an adversarial nesting cannot become unbounded
/// work. Past it the local is simply untyped.
const MAX_CLIMB: usize = 256;
const MAX_PEEL: usize = 16;
const MAX_PATTERN_NODES: usize = 1024;
/// How many hops a type may follow through other locals: `guard` is typed by
/// `shared`, which is typed by its parameter. Two is what the shapes above
/// need; more would be inference this pass does not claim.
const MAX_HOPS: usize = 2;

/// One block's `let`s: binding name → `(let start byte, binder)` in source
/// order. `None` is a `let` that binds the name in a shape this pass does not
/// read: it still shadows.
type BlockLets<'tree> = HashMap<String, Vec<(usize, Option<Binder<'tree>>)>>;

/// One file's binder index.
///
/// Asked once per use site, and a file can hold thousands of sites in one
/// block, so a block's `let`s are read **once** — on the first question that
/// reaches the block — and answered from the index after that. Scanning the
/// block per site is quadratic, and on a 4,000-call function it spent the
/// whole extraction deadline.
pub(crate) struct Binders<'tree, 'src> {
    source: &'src str,
    /// Block id → that block's `let`s; see [`BlockLets`].
    blocks: HashMap<usize, BlockLets<'tree>>,
}

impl<'tree, 'src> Binders<'tree, 'src> {
    pub(crate) fn new(source: &'src str) -> Self {
        Self {
            source,
            blocks: HashMap::new(),
        }
    }

    /// The nominal type `name` has at `use_node`, when its binder states it.
    /// `scope` is the callable the binding belongs to; the search never climbs
    /// past it.
    ///
    /// Reduced by [`rust_type_name`], the same reduction a parameter's
    /// written type gets, so `&'a Store` answers `Store` here exactly as it
    /// does from the `(scope, name)` facts — the raw text would carry the
    /// lifetime and stop resolving.
    pub(crate) fn binder_type(
        &mut self,
        use_node: Node<'tree>,
        name: &str,
        scope: Node<'tree>,
    ) -> Option<String> {
        type_of_local(self, use_node, name, scope, 0).and_then(|ty| {
            rust_type_name(ty, self.source, 0).or_else(|| {
                // A `vec![Hidden]` element is written as a value expression,
                // and a capitalised identifier there names the type.
                (ty.kind() == "identifier").then(|| get_node_text(ty, self.source))
            })
        })
    }

    /// What the binder says about `name` that only the whole corpus can turn
    /// into a type, as an initializer shape the resolver reads:
    ///
    /// * `T::f()` / `T::f()?` — the value of an associated-function call, and
    ///   whether a `?`, `.unwrap()` or `.expect(..)` unwrapped it. The
    ///   resolver types the binding `T` when `f`'s declared return type says
    ///   so; `T::new()` keeps its old reading when no `new` is indexed.
    /// * `|f|k|p` — parameter `p` of a closure passed as argument `k` of a
    ///   call to the bare function `f`, typed by `f`'s declared closure type.
    ///
    /// Neither shape can collide with the shapes the `(scope, name)` facts
    /// write — `T::new`, `T{..}`, `recv.method`, a bare name.
    pub(crate) fn binder_hint(
        &mut self,
        use_node: Node<'tree>,
        name: &str,
        scope: Node<'tree>,
    ) -> Option<String> {
        if !is_plain_ident(name) {
            return None;
        }
        let source = self.source;
        match binder_of(self, use_node, name, scope)? {
            Binder::Value(value) => {
                let (inner, unwrapped) = strip_unwrap(value, source);
                wrapped_value_hint(inner, source)
                    .or_else(|| associated_call_hint(inner, unwrapped, source))
            }
            Binder::ClosureParam(closure, index) => closure_argument_hint(closure, index, source),
            Binder::Typed(_) | Binder::OkPattern(..) | Binder::Loop(_) => None,
        }
    }

    /// The last `let` in `block` before `child` that binds `name`.
    ///
    /// `Some(None)` is a `let` that binds the name in a shape this pass does
    /// not read — it shadows, so the search ends there.
    fn preceding_let(
        &mut self,
        block: Node<'tree>,
        child: Node<'tree>,
        name: &str,
    ) -> Option<Option<Binder<'tree>>> {
        let source = self.source;
        let lets = self
            .blocks
            .entry(block.id())
            .or_insert_with(|| index_block(block, source));
        // The latest preceding `let` that binds this name, or that is too large
        // to read and so may bind anything (filed under the empty name).
        let latest = |key: &str| {
            let declared = lets.get(key)?;
            let before = declared.partition_point(|(start, _)| *start < child.start_byte());
            before.checked_sub(1).map(|last| declared[last])
        };
        match (latest(name), latest("")) {
            (Some(named), Some(opaque)) if opaque.0 > named.0 => Some(None),
            (Some(named), _) => Some(named.1),
            (None, Some(_)) => Some(None),
            (None, None) => None,
        }
    }
}

/// The initializer shape of the value a smart pointer or cell wraps:
/// `Rc::new(RefCell::new(Bag { .. }))` is a `Bag` for every method call that
/// reaches it through `borrow_mut()`, and `Arc::new(Mutex::new(Bag::new()))`
/// is whatever `Bag::new()` is. Read through `Rc`, `Arc`, `Box`, `RefCell`,
/// `Cell`, `Mutex` and `RwLock` constructors with one argument, down to a
/// struct literal or an associated-function call.
///
/// Only the *type* survives. The resolver reads `rc.borrow_mut` / `m.lock.unwrap`
/// as the wrapped value (see `rust_wrapper_chain_root`), and a call on the
/// wrapper itself that is not valid Rust cannot reach it.
fn wrapped_value_hint(call: Node, source: &str) -> Option<String> {
    const WRAPPERS: &[&str] = &["Rc", "Arc", "Box", "RefCell", "Cell", "Mutex", "RwLock"];
    let mut value = call;
    let mut wrapped = false;
    for _ in 0..MAX_PEEL {
        if value.kind() == "struct_expression" {
            if !wrapped {
                return None;
            }
            let name = rust_type_name(value.child_by_field_name("name")?, source, 0)?;
            return is_plain_ident(&name).then(|| format!("{name}{{..}}"));
        }
        if value.kind() != "call_expression" {
            return None;
        }
        let function = value.child_by_field_name("function")?;
        if function.kind() != "scoped_identifier" {
            return None;
        }
        let owner = get_node_text(function.child_by_field_name("path")?, source);
        let owner = owner.rsplit("::").next()?.trim();
        let method = get_node_text(function.child_by_field_name("name")?, source);
        if !WRAPPERS.contains(&owner) || method != "new" {
            // The innermost expression: an associated-function call is read as
            // it always was, and anything else answers nothing.
            return wrapped
                .then(|| associated_call_hint(value, false, source))
                .flatten();
        }
        let arguments = value.child_by_field_name("arguments")?;
        if arguments.named_child_count() != 1 {
            return None;
        }
        value = arguments.named_child(0)?;
        wrapped = true;
    }
    None
}

/// `T::f()` / `T::f()?` for a call written `path::T::f(..)`, and `f()` /
/// `m::f()` (each optionally under `?`) for a free function's value.
fn associated_call_hint(call: Node, unwrapped: bool, source: &str) -> Option<String> {
    if call.kind() != "call_expression" {
        return None;
    }
    let function = call.child_by_field_name("function")?;
    let suffix = if unwrapped { "?" } else { "" };
    // `f()?` — a free function in scope. The resolver decides whether the name
    // is one: a local closure of that name is a binding, not this function.
    if function.kind() == "identifier" {
        let name = get_node_text(function, source);
        return is_plain_ident(&name).then(|| format!("{name}(){suffix}"));
    }
    if function.kind() != "scoped_identifier" {
        return None;
    }
    let path = function.child_by_field_name("path")?;
    let method = get_node_text(function.child_by_field_name("name")?, source);
    let path = get_node_text(path, source);
    if !is_plain_ident(&method) {
        return None;
    }
    let type_name = path.rsplit("::").next()?.trim();
    let type_name = type_name.split('<').next()?.trim();
    // A type is capitalised: `T::f()`. Anything else is a module path to a free
    // function (`sub::f()`, `crate::m::f()`), kept whole so the resolver can
    // place the module.
    if type_name.starts_with(|c: char| c.is_ascii_uppercase()) {
        return is_plain_ident(type_name).then(|| format!("{type_name}::{method}(){suffix}"));
    }
    path.split("::")
        .all(is_plain_ident)
        .then(|| format!("{path}::{method}(){suffix}"))
}

/// `|f|k|p` for parameter `p` of a closure that is argument `k` of `f(..)`.
fn closure_argument_hint(closure: Node, param: usize, source: &str) -> Option<String> {
    let arguments = bounded_parent(closure).filter(|node| node.kind() == "arguments")?;
    let call = bounded_parent(arguments).filter(|node| node.kind() == "call_expression")?;
    let function = call.child_by_field_name("function")?;
    if function.kind() != "identifier" {
        return None;
    }
    let mut cursor = arguments.walk();
    let arg = arguments
        .named_children(&mut cursor)
        .filter(|child| !child.kind().ends_with("comment"))
        .position(|child| child.id() == closure.id())?;
    Some(format!(
        "|{}|{arg}|{param}",
        get_node_text(function, source)
    ))
}

/// Every `let` in `block`, by each name its pattern mentions.
fn index_block<'tree>(
    block: Node<'tree>,
    source: &str,
) -> HashMap<String, Vec<(usize, Option<Binder<'tree>>)>> {
    let mut lets: HashMap<String, Vec<(usize, Option<Binder<'tree>>)>> = HashMap::new();
    let mut cursor = block.walk();
    for statement in block.named_children(&mut cursor) {
        if statement.kind() != "let_declaration" {
            continue;
        }
        let Some(pattern) = statement.child_by_field_name("pattern") else {
            continue;
        };
        let Some(names) = pattern_identifiers(pattern, source) else {
            lets.entry(String::new())
                .or_default()
                .push((statement.start_byte(), None));
            continue;
        };
        for name in names {
            let binder = if simple_pattern_is(pattern, &name, source) {
                statement
                    .child_by_field_name("type")
                    .map(Binder::Typed)
                    .or_else(|| statement.child_by_field_name("value").map(Binder::Value))
            } else {
                None
            };
            lets.entry(name)
                .or_default()
                .push((statement.start_byte(), binder));
        }
    }
    lets
}

/// The constructor of a `Result`/`Option` pattern that unwraps its value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Unwrapper {
    Ok,
    Some,
}

/// What introduced a name, as far as this pass can say.
#[derive(Clone, Copy)]
enum Binder<'tree> {
    /// A parameter or `let` with a written type.
    Typed(Node<'tree>),
    /// An untyped `let`, with its value.
    Value(Node<'tree>),
    /// `Ok(name)` or `Some(name)` in a `match` arm or `if let`/`while let`,
    /// with the scrutinee.
    OkPattern(Node<'tree>, Unwrapper),
    /// The pattern of a `for` loop, with the iterated expression.
    Loop(Node<'tree>),
    /// An untyped parameter of a closure, with the closure and its position.
    ClosureParam(Node<'tree>, usize),
}

fn type_of_local<'tree>(
    binders: &mut Binders<'tree, '_>,
    use_node: Node<'tree>,
    name: &str,
    scope: Node<'tree>,
    hops: usize,
) -> Option<Node<'tree>> {
    if hops > MAX_HOPS || !is_plain_ident(name) {
        return None;
    }
    let source = binders.source;
    match binder_of(binders, use_node, name, scope)? {
        Binder::Typed(ty) => Some(ty),
        Binder::Value(value) => {
            let (inner, unwrapped) = strip_unwrap(value, source);
            if unwrapped {
                lock_guard_target(binders, inner, scope, hops)
            } else {
                None
            }
        }
        Binder::OkPattern(scrutinee, unwrapper) => {
            lock_guard_target(binders, scrutinee, scope, hops)
                .or_else(|| unwrapped_local(binders, scrutinee, unwrapper, scope, hops))
        }
        Binder::Loop(iterable) => loop_element(binders, iterable, scope, hops),
        Binder::ClosureParam(closure, index) => closure_element(binders, closure, index, hops),
    }
}

/// The nearest binder of `name` above `use_node`, inside `scope`.
///
/// A binder this pass cannot read — a closure parameter, a destructuring
/// `let`, an `Err(e)` arm — still **stops** the search: it shadows every outer
/// binding of the name, so answering from one further out would type the
/// wrong value.
fn binder_of<'tree>(
    binders: &mut Binders<'tree, '_>,
    use_node: Node<'tree>,
    name: &str,
    scope: Node<'tree>,
) -> Option<Binder<'tree>> {
    let source = binders.source;
    let mut child = use_node;
    for _ in 0..MAX_CLIMB {
        let parent = bounded_parent(child)?;
        if parent.id() == scope.id() {
            return parameter_binder(scope, name, source);
        }
        match parent.kind() {
            "match_arm" => {
                let pattern = parent.child_by_field_name("pattern")?;
                if pattern.id() != child.id() && pattern_mentions(pattern, name, source) {
                    let scrutinee = bounded_parent(parent)
                        .and_then(bounded_parent)
                        .filter(|node| node.kind() == "match_expression")
                        .and_then(|matched| matched.child_by_field_name("value"))?;
                    return unwrap_pattern(pattern, name, source)
                        .map(|unwrapper| Binder::OkPattern(scrutinee, unwrapper));
                }
            }
            "if_expression" | "while_expression" => {
                let condition = parent.child_by_field_name("condition")?;
                if condition.id() != child.id()
                    && condition.kind() == "let_condition"
                    && condition
                        .child_by_field_name("pattern")
                        .is_some_and(|pattern| pattern_mentions(pattern, name, source))
                {
                    // Only the body sees the pattern's binding; an `else`
                    // branch sees whatever the name meant outside.
                    let in_body = parent
                        .child_by_field_name("consequence")
                        .or_else(|| parent.child_by_field_name("body"))
                        .is_some_and(|body| body.id() == child.id());
                    if in_body {
                        let pattern = condition.child_by_field_name("pattern")?;
                        let value = condition.child_by_field_name("value")?;
                        return unwrap_pattern(pattern, name, source)
                            .map(|unwrapper| Binder::OkPattern(value, unwrapper));
                    }
                }
                if condition.kind() == "let_chain"
                    && condition.id() != child.id()
                    && pattern_mentions(condition, name, source)
                {
                    return None;
                }
            }
            "for_expression" => {
                let pattern = parent.child_by_field_name("pattern")?;
                let body = parent.child_by_field_name("body")?;
                if body.id() == child.id() && pattern_mentions(pattern, name, source) {
                    let value = parent.child_by_field_name("value")?;
                    return (pattern.kind() == "identifier"
                        && get_node_text(pattern, source) == name)
                        .then_some(Binder::Loop(value));
                }
            }
            "closure_expression" => {
                if parent
                    .child_by_field_name("parameters")
                    .is_some_and(|params| pattern_mentions(params, name, source))
                {
                    return None;
                }
            }
            "block" => {
                if let Some(found) = binders.preceding_let(parent, child, name) {
                    return found;
                }
            }
            _ => {}
        }
        child = parent;
    }
    None
}

/// A function's or closure's own parameter named `name`. A closure parameter
/// with no written type is recorded with its position, for the resolver to
/// type from the function the closure is passed to.
fn parameter_binder<'tree>(scope: Node<'tree>, name: &str, source: &str) -> Option<Binder<'tree>> {
    let params = scope.child_by_field_name("parameters")?;
    let closure = scope.kind() == "closure_expression";
    let mut cursor = params.walk();
    for (index, param) in params
        .named_children(&mut cursor)
        .filter(|param| !param.kind().ends_with("comment"))
        .enumerate()
    {
        if param.kind() == "parameter" {
            let Some(pattern) = param.child_by_field_name("pattern") else {
                continue;
            };
            if simple_pattern_is(pattern, name, source) {
                return param.child_by_field_name("type").map(Binder::Typed);
            }
        } else if closure && simple_pattern_is(param, name, source) {
            return Some(Binder::ClosureParam(scope, index));
        }
    }
    None
}

/// `name` or `mut name`, and nothing else.
///
/// A `mut_pattern` is `mutable_specifier` then the pattern, and the specifier
/// is itself a named node, so the bound pattern is the one named child that is
/// not the specifier — never "the first named child", which is the specifier.
fn simple_pattern_is(pattern: Node, name: &str, source: &str) -> bool {
    match pattern.kind() {
        "identifier" => get_node_text(pattern, source) == name,
        "mut_pattern" => {
            let mut cursor = pattern.walk();
            let mut inner = pattern
                .named_children(&mut cursor)
                .filter(|child| child.kind() != "mutable_specifier");
            match (inner.next(), inner.next()) {
                (Some(only), None) => simple_pattern_is(only, name, source),
                _ => false,
            }
        }
        _ => false,
    }
}

/// `Ok(name)` or `Some(name)` with exactly that one binding. An arm's pattern
/// arrives wrapped in a `match_pattern`, which also holds an `if` guard when
/// there is one; a guard does not change what the pattern binds.
fn unwrap_pattern(pattern: Node, name: &str, source: &str) -> Option<Unwrapper> {
    let pattern = if pattern.kind() == "match_pattern" {
        pattern.named_child(0)?
    } else {
        pattern
    };
    if pattern.kind() != "tuple_struct_pattern" {
        return None;
    }
    let constructor = get_node_text(pattern.child_by_field_name("type")?, source);
    let unwrapper = match constructor.as_str() {
        "Ok" => Unwrapper::Ok,
        "Some" => Unwrapper::Some,
        _ => return None,
    };
    let mut cursor = pattern.walk();
    let bindings: Vec<Node> = pattern
        .named_children(&mut cursor)
        .filter(|child| {
            pattern
                .child_by_field_name("type")
                .is_none_or(|ty| ty.id() != child.id())
        })
        .collect();
    matches!(bindings.as_slice(), [only] if simple_pattern_is(*only, name, source))
        .then_some(unwrapper)
}

/// Whether any identifier in `pattern` spells `name`. Deliberately wider than
/// "binds": an over-match only makes the search stop early, which loses a type
/// and never invents one. A pattern too large to read is treated as
/// mentioning everything, for the same reason.
fn pattern_mentions(pattern: Node, name: &str, source: &str) -> bool {
    let mut stack = vec![pattern];
    let mut visited = 0usize;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited > MAX_PATTERN_NODES {
            return true;
        }
        if node.kind() == "identifier" && get_node_text(node, source) == name {
            return true;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    false
}

/// Every identifier a `let` pattern spells, for [`index_block`], or `None` for
/// a pattern too large to read — which may bind any name, so the caller files
/// it as shadowing every name after it.
fn pattern_identifiers(pattern: Node, source: &str) -> Option<Vec<String>> {
    let mut names = Vec::new();
    let mut stack = vec![pattern];
    let mut visited = 0usize;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited > MAX_PATTERN_NODES {
            return None;
        }
        if node.kind() == "identifier" {
            names.push(get_node_text(node, source));
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    names.sort();
    names.dedup();
    Some(names)
}

/// The expression under `?`, `.unwrap()` or `.expect(..)`, and whether one of
/// them was there.
fn strip_unwrap<'tree>(value: Node<'tree>, source: &str) -> (Node<'tree>, bool) {
    if value.kind() == "try_expression" {
        if let Some(inner) = value.named_child(0) {
            return (inner, true);
        }
    }
    if let Some((receiver, method, args)) = method_call(value, source) {
        let arity = args.named_child_count();
        if (method == "unwrap" && arity == 0) || (method == "expect" && arity == 1) {
            return (receiver, true);
        }
    }
    (value, false)
}

/// `receiver.method(args)`, split.
fn method_call<'tree>(
    node: Node<'tree>,
    source: &str,
) -> Option<(Node<'tree>, String, Node<'tree>)> {
    if node.kind() != "call_expression" {
        return None;
    }
    let function = node.child_by_field_name("function")?;
    if function.kind() != "field_expression" {
        return None;
    }
    let receiver = function.child_by_field_name("value")?;
    let method = get_node_text(function.child_by_field_name("field")?, source);
    let args = node.child_by_field_name("arguments")?;
    Some((receiver, method, args))
}

/// `x.lock()` / `x.read()` / `x.write()` on a local whose type is a `Mutex<T>`
/// or `RwLock<T>`: the `T` the guard derefs to. The caller has already
/// established that the `LockResult` was unwrapped.
fn lock_guard_target<'tree>(
    binders: &mut Binders<'tree, '_>,
    call: Node<'tree>,
    scope: Node<'tree>,
    hops: usize,
) -> Option<Node<'tree>> {
    let source = binders.source;
    let (receiver, method, args) = method_call(call, source)?;
    if args.named_child_count() != 0 || receiver.kind() != "identifier" {
        return None;
    }
    let lock = match method.as_str() {
        "lock" => "Mutex",
        "read" | "write" => "RwLock",
        _ => return None,
    };
    let local = get_node_text(receiver, source);
    let ty = type_of_local(binders, receiver, &local, scope, hops + 1)?;
    single_argument_of(peel(ty, source)?, &[lock], source)
}

/// The element a `for` loop binds over a local collection.
fn loop_element<'tree>(
    binders: &mut Binders<'tree, '_>,
    iterable: Node<'tree>,
    scope: Node<'tree>,
    hops: usize,
) -> Option<Node<'tree>> {
    let source = binders.source;
    let mut collection = iterable;
    if collection.kind() == "reference_expression" {
        collection = collection.child_by_field_name("value")?;
    } else if let Some((receiver, method, args)) = method_call(collection, source) {
        if args.named_child_count() != 0
            || !matches!(method.as_str(), "iter" | "iter_mut" | "into_iter")
        {
            return None;
        }
        collection = receiver;
    }
    if collection.kind() != "identifier" {
        return None;
    }
    let local = get_node_text(collection, source);
    if let Some(Binder::Value(value)) = binder_of(binders, collection, &local, scope) {
        if let Some(element) = vec_macro_element(value, source) {
            return Some(element);
        }
    }
    let ty = peel(
        type_of_local(binders, collection, &local, scope, hops + 1)?,
        source,
    )?;
    if ty.kind() == "array_type" {
        return ty.child_by_field_name("element");
    }
    single_argument_of(ty, &["Vec", "VecDeque", "HashSet", "BTreeSet"], source)
}

/// The `T` a pattern `Some(x)` / `Ok(x)` binds when its scrutinee is a local
/// typed `Option<T>` / `Result<T, E>`.
fn unwrapped_local<'tree>(
    binders: &mut Binders<'tree, '_>,
    scrutinee: Node<'tree>,
    unwrapper: Unwrapper,
    scope: Node<'tree>,
    hops: usize,
) -> Option<Node<'tree>> {
    let source = binders.source;
    let mut value = scrutinee;
    while matches!(
        value.kind(),
        "reference_expression" | "parenthesized_expression"
    ) {
        value = value
            .child_by_field_name("value")
            .or_else(|| value.named_child(0))?;
    }
    if value.kind() != "identifier" {
        return None;
    }
    let local = get_node_text(value, source);
    let ty = peel(
        type_of_local(binders, value, &local, scope, hops + 1)?,
        source,
    )?;
    let outer = match unwrapper {
        Unwrapper::Some => "Option",
        Unwrapper::Ok => "Result",
    };
    first_argument_of(ty, &[outer], source)
}

/// What a closure parameter is when the closure is the argument of an iterator
/// adapter over a typed local collection:
/// `items.iter().filter(|i| i.keep())` gives `i` the element type of `items`.
///
/// Only the adapters whose closure receives the item itself are read, and the
/// chain below it only through adapters that keep the item type, so a `map`
/// earlier in the chain (which changes it) answers nothing. The collection is
/// looked up from the enclosing function: the closure has no bindings of its
/// own that could name it.
fn closure_element<'tree>(
    binders: &mut Binders<'tree, '_>,
    closure: Node<'tree>,
    param: usize,
    hops: usize,
) -> Option<Node<'tree>> {
    let source = binders.source;
    let arguments = bounded_parent(closure).filter(|node| node.kind() == "arguments")?;
    let call = bounded_parent(arguments).filter(|node| node.kind() == "call_expression")?;
    let (receiver, method, _) = method_call(call, source)?;
    let mut cursor = arguments.walk();
    let arg = arguments
        .named_children(&mut cursor)
        .filter(|child| !child.kind().ends_with("comment"))
        .position(|child| child.id() == closure.id())?;
    let receives_item = match method.as_str() {
        "filter" | "take_while" | "skip_while" | "map" | "for_each" | "any" | "all" | "find"
        | "position" | "filter_map" | "flat_map" | "inspect" | "find_map" | "max_by_key"
        | "min_by_key" => arg == 0 && param == 0,
        "fold" => arg == 1 && param == 1,
        _ => false,
    };
    if !receives_item {
        return None;
    }
    let mut scope = bounded_parent(call)?;
    while scope.kind() != "function_item" {
        scope = bounded_parent(scope)?;
    }
    let mut chain = receiver;
    for _ in 0..MAX_PEEL {
        let Some((inner, adapter, args)) = method_call(chain, source) else {
            break;
        };
        if matches!(
            adapter.as_str(),
            "filter"
                | "take_while"
                | "skip_while"
                | "skip"
                | "take"
                | "rev"
                | "inspect"
                | "peekable"
                | "by_ref"
                | "step_by"
                | "cloned"
                | "copied"
        ) {
            chain = inner;
            continue;
        }
        // `v.iter()` and friends are `loop_element`'s own subject.
        if args.named_child_count() != 0
            || !matches!(adapter.as_str(), "iter" | "iter_mut" | "into_iter")
        {
            return None;
        }
        break;
    }
    loop_element(binders, chain, scope, hops)
}

/// The first element's type name in `vec![Hidden, ..]` / `vec![Hidden; n]` /
/// `vec![Bag { .. }]`, as the identifier node that spells it.
///
/// A macro body is an unstructured token tree, so this reads the tokens: a
/// capitalised identifier opening the list, followed by nothing, a separator,
/// or one `{ .. }` / `( .. )` group — a unit struct, a struct literal or a
/// tuple-struct constructor. `Hidden::new()` and `make()` follow with `::` or a
/// lowercase name and answer nothing.
fn vec_macro_element<'tree>(value: Node<'tree>, source: &str) -> Option<Node<'tree>> {
    if value.kind() != "macro_invocation"
        || get_node_text(value.child_by_field_name("macro")?, source) != "vec"
    {
        return None;
    }
    let mut cursor = value.walk();
    let tokens = value
        .named_children(&mut cursor)
        .find(|child| child.kind() == "token_tree")?;
    let first = tokens.named_child(0)?;
    if first.kind() != "identifier"
        || !get_node_text(first, source).starts_with(|c: char| c.is_ascii_uppercase())
    {
        return None;
    }
    match first.next_sibling() {
        None => Some(first),
        Some(next) if matches!(next.kind(), "," | ";" | ")" | "]" | "}") => Some(first),
        Some(next) if next.kind() == "token_tree" => Some(first),
        Some(_) => None,
    }
}

/// `&T`, `&mut T`, `*const T`, `Arc<T>`, `Rc<T>`, `Box<T>` → `T`, repeatedly.
fn peel<'tree>(mut ty: Node<'tree>, source: &str) -> Option<Node<'tree>> {
    for _ in 0..MAX_PEEL {
        match ty.kind() {
            "reference_type" | "pointer_type" => ty = ty.child_by_field_name("type")?,
            "generic_type" => {
                match single_argument_of(ty, crate::deref::RUST_DEREF_TRANSPARENT, source) {
                    Some(inner) => ty = inner,
                    None => return Some(ty),
                }
            }
            _ => return Some(ty),
        }
    }
    None
}

/// The one type argument of `Outer<T>` when `Outer` is one of `outers`.
/// Lifetimes are not type arguments; a second type argument refuses.
fn single_argument_of<'tree>(
    ty: Node<'tree>,
    outers: &[&str],
    source: &str,
) -> Option<Node<'tree>> {
    if ty.kind() != "generic_type" {
        return None;
    }
    let outer = get_node_text(ty.child_by_field_name("type")?, source);
    let bare = outer.rsplit("::").next().unwrap_or(&outer).trim();
    if !outers.contains(&bare) {
        return None;
    }
    let args = ty.child_by_field_name("type_arguments")?;
    let mut cursor = args.walk();
    let types: Vec<Node<'tree>> = args
        .named_children(&mut cursor)
        .filter(|arg| arg.kind() != "lifetime")
        .collect();
    match types.as_slice() {
        [only] => Some(*only),
        _ => None,
    }
}

/// The first type argument of `Outer<T, ..>` when `Outer` is one of `outers`:
/// the `T` of a `Result<T, E>`.
fn first_argument_of<'tree>(ty: Node<'tree>, outers: &[&str], source: &str) -> Option<Node<'tree>> {
    if ty.kind() != "generic_type" {
        return None;
    }
    let outer = get_node_text(ty.child_by_field_name("type")?, source);
    let bare = outer.rsplit("::").next().unwrap_or(&outer).trim();
    if !outers.contains(&bare) {
        return None;
    }
    let args = ty.child_by_field_name("type_arguments")?;
    let mut cursor = args.walk();
    let first = args
        .named_children(&mut cursor)
        .find(|arg| arg.kind() != "lifetime");
    first
}

fn is_plain_ident(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}
