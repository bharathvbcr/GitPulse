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
        type_of_local(self, use_node, name, scope, 0)
            .and_then(|ty| rust_type_name(ty, self.source, 0))
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
                associated_call_hint(inner, unwrapped, source)
            }
            Binder::ClosureParam(closure, index) => closure_argument_hint(closure, index, source),
            Binder::Typed(_) | Binder::OkPattern(_) | Binder::Loop(_) => None,
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

/// `T::f()` / `T::f()?` for a call written `path::T::f(..)`.
fn associated_call_hint(call: Node, unwrapped: bool, source: &str) -> Option<String> {
    if call.kind() != "call_expression" {
        return None;
    }
    let function = call.child_by_field_name("function")?;
    if function.kind() != "scoped_identifier" {
        return None;
    }
    let path = function.child_by_field_name("path")?;
    let method = get_node_text(function.child_by_field_name("name")?, source);
    let path = get_node_text(path, source);
    let type_name = path.rsplit("::").next()?.trim();
    let type_name = type_name.split('<').next()?.trim();
    if !is_plain_ident(type_name) || !is_plain_ident(&method) {
        return None;
    }
    // A type is capitalised; `module::f()` is a free function of a module,
    // whose return type this shape does not describe.
    if !type_name.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }
    Some(format!(
        "{type_name}::{method}(){}",
        if unwrapped { "?" } else { "" }
    ))
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

/// What introduced a name, as far as this pass can say.
#[derive(Clone, Copy)]
enum Binder<'tree> {
    /// A parameter or `let` with a written type.
    Typed(Node<'tree>),
    /// An untyped `let`, with its value.
    Value(Node<'tree>),
    /// `Ok(name)` in a `match` arm or `if let`/`while let`, with the scrutinee.
    OkPattern(Node<'tree>),
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
        Binder::OkPattern(scrutinee) => lock_guard_target(binders, scrutinee, scope, hops),
        Binder::Loop(iterable) => loop_element(binders, iterable, scope, hops),
        Binder::ClosureParam(..) => None,
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
                    return ok_pattern_binds(pattern, name, source)
                        .then_some(Binder::OkPattern(scrutinee));
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
                        return ok_pattern_binds(pattern, name, source)
                            .then_some(Binder::OkPattern(value));
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
fn simple_pattern_is(pattern: Node, name: &str, source: &str) -> bool {
    match pattern.kind() {
        "identifier" => get_node_text(pattern, source) == name,
        "mut_pattern" => pattern
            .named_child(0)
            .is_some_and(|inner| simple_pattern_is(inner, name, source)),
        _ => false,
    }
}

/// `Ok(name)` with exactly that one binding. An arm's pattern arrives wrapped
/// in a `match_pattern`, which also holds an `if` guard when there is one; a
/// guard does not change what the pattern binds.
fn ok_pattern_binds(pattern: Node, name: &str, source: &str) -> bool {
    let pattern = if pattern.kind() == "match_pattern" {
        match pattern.named_child(0) {
            Some(inner) => inner,
            None => return false,
        }
    } else {
        pattern
    };
    if pattern.kind() != "tuple_struct_pattern" {
        return false;
    }
    let is_ok = pattern
        .child_by_field_name("type")
        .is_some_and(|ty| get_node_text(ty, source) == "Ok");
    let mut cursor = pattern.walk();
    let bindings: Vec<Node> = pattern
        .named_children(&mut cursor)
        .filter(|child| {
            pattern
                .child_by_field_name("type")
                .is_none_or(|ty| ty.id() != child.id())
        })
        .collect();
    is_ok && matches!(bindings.as_slice(), [only] if simple_pattern_is(*only, name, source))
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
    let ty = peel(
        type_of_local(binders, collection, &local, scope, hops + 1)?,
        source,
    )?;
    if ty.kind() == "array_type" {
        return ty.child_by_field_name("element");
    }
    single_argument_of(ty, &["Vec", "VecDeque", "HashSet", "BTreeSet"], source)
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

fn is_plain_ident(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}
