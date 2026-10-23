use super::*;

/// Names a Python module declares public via `__all__`.
///
/// `__all__` is the module's explicit export list, so a symbol named in it is
/// public API by definition and must never be reported as dead code. Handles
/// both `__all__ = [...]` and the `__all__ += [...]` accumulation form, and
/// spans multiple lines because real modules wrap long lists.
///
/// Three things must *not* be read as a declaration, because each would publish
/// a name the module never exported and so suppress a real dead-code finding:
/// a longer identifier that merely ends in `__all__` (`my__all__ = [...]`), an
/// occurrence inside a comment, and any occurrence not followed by an
/// assignment. The first two were claimed by this comment but not implemented —
/// the token was matched anywhere in the source, so `my__all__ = ["nope"]`
/// exported `nope`.
///
/// Known limit: the comment test looks for `#` earlier on the same line, so a
/// `#` inside a string literal preceding `__all__` on that same line would hide
/// a real declaration. `__all__` sits on its own top-level line in practice.
pub(super) fn python_all_exports(source: &str) -> std::collections::BTreeSet<String> {
    let mut exported = std::collections::BTreeSet::new();
    let mut line_start = 0usize;
    for line in source.split_inclusive('\n') {
        // `match_indices` walks non-overlapping matches left to right, so the
        // scan cannot fail to advance. A hand-rolled cursor here previously made
        // termination depend on the increment arithmetic being right — mutating
        // that one expression hung the extractor instead of producing a wrong
        // answer, which is a worse failure than any it was guarding.
        for (at, matched) in line.match_indices("__all__") {
            let searched = at + matched.len();
            let before = &line[..at];

            // Stand-alone token only: not the tail of a longer identifier, and
            // not inside a comment.
            if before
                .chars()
                .next_back()
                .is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
                || before.contains('#')
            {
                continue;
            }

            // The list itself may wrap across lines, so it is read from the
            // whole source rather than from this line.
            let after = &source[line_start + searched..];
            let Some(open) = after.find(['[', '(']) else {
                continue;
            };
            let assignment = after[..open].trim();
            if !(assignment == "=" || assignment == "+=") {
                continue;
            }

            let list = &after[open..];
            let end = list.find([']', ')']).unwrap_or(list.len());
            let mut chars = list[..end].chars();
            while let Some(quote) = chars.next() {
                if quote != '"' && quote != '\'' {
                    continue;
                }
                let mut name = String::new();
                for inner in chars.by_ref() {
                    if inner == quote {
                        break;
                    }
                    name.push(inner);
                }
                if !name.is_empty() {
                    exported.insert(name);
                }
            }
        }
        line_start += line.len();
    }
    exported
}

/// Whether a Python node executes in its module's namespace.
///
/// Python's scopes are functions, lambdas and class bodies — never blocks. A
/// binding under a module-level `if`, `try`, `with`, `for`, `while` or `match`
/// is a module attribute exactly like one written at the top:
/// `try: import numpy; HAS_NUMPY = True` is how an optional dependency is
/// declared, and `from mod import HAS_NUMPY` imports it. The shared
/// [`is_module_level`] treats every `block` as a scope, which is right for the
/// brace languages it serves and dropped every such Python binding — even one
/// `__all__` named.
///
/// The walk ends at the tree's root, which must be Python's `module`, so a
/// caller that cannot otherwise tell the grammar apart — another language's
/// `assignment` — gets `false`.
pub(super) fn python_is_module_scope(node: Node) -> bool {
    let mut current = node;
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        if is_callable_node(parent) || parent.kind() == "class_definition" {
            return false;
        }
        current = parent;
        ancestor = bounded_parent(parent);
    }
    current.kind() == "module"
}

/// Every name a Python assignment target binds, in source order.
///
/// `A, B = …`, `(A, B) = …`, `[A, B] = …` and `A, *REST = …` bind each name;
/// `obj.attr = …` and `table[k] = …` bind none, because they write into an
/// existing object rather than into the namespace. Reading only an
/// `identifier` target emitted nothing for every unpacking assignment.
///
/// An explicit stack rather than recursion, so an adversarially nested target
/// cannot overflow the call stack; every node is visited once, so the walk is
/// linear in the target's size.
pub(super) fn python_bound_names(target: Node, source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut stack = vec![target];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "identifier" => {
                let name = get_node_text(node, source);
                if !name.is_empty() {
                    names.push(name);
                }
            }
            // `tuple` and `as_pattern_target` are the shapes a `with … as (A, B)`
            // alias and an `except … as e` alias are parsed into.
            "pattern_list"
            | "tuple_pattern"
            | "list_pattern"
            | "list_splat_pattern"
            | "tuple"
            | "list"
            | "expression_list"
            | "parenthesized_expression"
            | "as_pattern_target" => {
                let mut cursor = node.walk();
                let children: Vec<Node> = node.named_children(&mut cursor).collect();
                stack.extend(children.into_iter().rev());
            }
            _ => {}
        }
    }
    names
}

/// The names a `case` pattern captures: a bare name (`case [A, _]`, `case x as
/// A`, `case {'k': A}`, `case P(x=A)`, `case [*A]`, `case {**A}`), never the
/// class of a class pattern (`case A()`), a dotted value (`case m.n`) or a
/// keyword (`x` in `P(x=A)`).
pub(super) fn python_pattern_captures(pattern: Node, source: &str, out: &mut Vec<String>) {
    let mut stack = vec![pattern];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        let children: Vec<Node> = node.named_children(&mut cursor).collect();
        match node.kind() {
            "dotted_name" => {
                if let [only] = children.as_slice() {
                    out.push(get_node_text(*only, source));
                }
            }
            "splat_pattern" => {
                if let Some(name) = children.first().filter(|c| c.kind() == "identifier") {
                    out.push(get_node_text(*name, source));
                }
            }
            "as_pattern" => {
                if let Some(alias) = children.last().filter(|c| c.kind() == "identifier") {
                    out.push(get_node_text(*alias, source));
                }
                stack.extend(children.first());
            }
            // The first child is the class (or the keyword), the rest are patterns.
            "class_pattern" | "keyword_pattern" => stack.extend(children.into_iter().skip(1)),
            "string" | "concatenated_string" | "integer" | "float" => {}
            _ => stack.extend(children),
        }
    }
}

/// How a module-scope binding binds, and where.
#[derive(Debug, Clone)]
pub(super) struct PythonBinding {
    pub(super) name: String,
    /// Byte offset of the binding statement; `0` for a binding a function makes
    /// through `global`, which may run at any time.
    pub(super) at: usize,
}

/// What a Python file's module scope says about the names it binds, computed
/// once per file ([`python_file_facts`]) and read by every decorator, base and
/// scope-local question about that file.
#[derive(Debug, Default)]
pub(super) struct PythonFileFacts {
    /// Every module-scope binding of a name: assignment, `def`, `class`,
    /// import, `for`/`with`/`except` target, walrus, `del`, `match` capture, and
    /// a name a function both declares `global` and binds. Not a `from
    /// builtins import X` or `import builtins`, which bind the builtins
    /// themselves.
    pub(super) bindings: Vec<PythonBinding>,
    /// Local names bound to the `builtins` module (`import builtins [as b]`).
    pub(super) builtins_modules: Vec<PythonBinding>,
    /// `from builtins import X [as L]`: `(L, X)` and where.
    pub(super) builtins_members: Vec<(PythonBinding, String)>,
    /// Module-scope imports: local name -> the module (or member) each binds it
    /// to, spelled as the import system names it.
    pub(super) imports: BTreeMap<String, Vec<String>>,
    /// The names bound other than by a plain assignment, `def`, `class` or
    /// import (and by a function through `global`): what the resolver must read
    /// as "this module-scope name may hold something else", emitted as
    /// file-scope locals.
    pub(super) rebound: BTreeSet<String>,
}

impl PythonFileFacts {
    /// Whether a binding of `name` other than the builtins' own is written
    /// textually before `at`.
    pub(super) fn binds_before(&self, name: &str, at: usize) -> bool {
        self.bindings
            .iter()
            .any(|binding| binding.name == name && binding.at < at)
    }

    /// Whether `name`, read at `at`, is the `builtins` module.
    pub(super) fn names_builtins_module(&self, name: &str, at: usize) -> bool {
        (name == "builtins"
            || self
                .builtins_modules
                .iter()
                .any(|bound| bound.name == name && bound.at < at))
            && !self.binds_before(name, at)
    }

    /// The builtin (`staticmethod`/`classmethod`) the bare name `name`, read at
    /// `at`, is: itself unless a binding precedes it, or the member a `from
    /// builtins import X as name` bound.
    pub(super) fn builtin_for_bare_name(&self, name: &str, at: usize) -> Option<String> {
        if self.binds_before(name, at) {
            return None;
        }
        self.builtins_members
            .iter()
            .find(|(bound, _)| bound.name == name && bound.at < at)
            .map(|(_, original)| original.clone())
            .or_else(|| Some(name.to_string()))
    }
}

thread_local! {
    /// The facts of the file being extracted, with the identity of the tree
    /// they were read from. Cleared between files with the scope-local cache:
    /// a node id is an address that a later tree reuses.
    pub(super) static PYTHON_FILE_FACTS: RefCell<Option<((usize, usize, usize), std::rc::Rc<PythonFileFacts>)>> =
        const { RefCell::new(None) };
}

/// The module-scope facts of the file `root` is the tree of, computed on first
/// use. Every question about a file's names asks this, so a file with thousands
/// of decorated methods reads its bindings once, not once per method.
pub(super) fn python_file_facts(root: Node, source: &str) -> std::rc::Rc<PythonFileFacts> {
    let key = (root.id(), source.as_ptr() as usize, source.len());
    if let Some(known) = PYTHON_FILE_FACTS.with(|cache| {
        cache
            .borrow()
            .as_ref()
            .filter(|(known, _)| *known == key)
            .map(|(_, facts)| facts.clone())
    }) {
        return known;
    }
    let facts = std::rc::Rc::new(python_read_file_facts(root, source));
    PYTHON_FILE_FACTS.with(|cache| *cache.borrow_mut() = Some((key, facts.clone())));
    facts
}

/// One walk of each scope (the module, then every function and class body it
/// reaches) into [`PythonFileFacts`].
///
/// A scope's walk does not enter a nested function, class or lambda: those are
/// other scopes, queued, and their only effect on the module is a name they
/// declare `global` and then bind. A comprehension is not a scope for a
/// walrus, so the walk enters it; its own `for` target is not a `for_statement`
/// and binds nothing here.
pub(super) fn python_read_file_facts(root: Node, source: &str) -> PythonFileFacts {
    // `(name, at, is_rebinding)`: a rebinding is one the resolver's tables
    // would not otherwise carry.
    type Bound = (String, usize, bool);
    let mut facts = PythonFileFacts::default();
    let mut scopes: Vec<(Node, bool)> = vec![(root, true)];
    while let Some((scope, is_module)) = scopes.pop() {
        let mut bound: Vec<Bound> = Vec::new();
        let mut globals: BTreeSet<String> = BTreeSet::new();
        let mut work: Vec<Node> = Vec::new();
        fn body<'t>(node: Node<'t>) -> Vec<Node<'t>> {
            let mut cursor = node.walk();
            node.named_children(&mut cursor).collect()
        }
        work.extend(body(scope).into_iter().rev());
        while let Some(node) = work.pop() {
            let at = node.start_byte();
            let targets = |target: Option<Node>, rebinding: bool, bound: &mut Vec<Bound>| {
                if let Some(target) = target {
                    for name in python_bound_names(target, source) {
                        bound.push((name, at, rebinding));
                    }
                }
            };
            match node.kind() {
                "function_definition" | "class_definition" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        bound.push((get_node_text(name, source), at, false));
                    }
                    scopes.push((node, false));
                    // Everything but the body (parameters, bases) is read here.
                    let inner = node.child_by_field_name("body");
                    work.extend(
                        body(node)
                            .into_iter()
                            .rev()
                            .filter(|child| inner.is_none_or(|b| b.id() != child.id())),
                    );
                    continue;
                }
                "lambda" => continue,
                "global_statement" => {
                    let mut cursor = node.walk();
                    globals.extend(
                        node.named_children(&mut cursor)
                            .filter(|child| child.kind() == "identifier")
                            .map(|child| get_node_text(child, source)),
                    );
                }
                "for_statement" => targets(node.child_by_field_name("left"), true, &mut bound),
                "with_item" | "except_clause" | "except_group_clause" => targets(
                    node.child_by_field_name("value")
                        .or_else(|| node.named_child(0))
                        .filter(|value| value.kind() == "as_pattern")
                        .and_then(|value| value.child_by_field_name("alias")),
                    true,
                    &mut bound,
                ),
                "named_expression" => targets(node.child_by_field_name("name"), true, &mut bound),
                "delete_statement" => targets(node.named_child(0), true, &mut bound),
                "case_clause" => {
                    let mut names = Vec::new();
                    let mut cursor = node.walk();
                    for pattern in node
                        .named_children(&mut cursor)
                        .filter(|child| child.kind() == "case_pattern")
                    {
                        python_pattern_captures(pattern, source, &mut names);
                    }
                    bound.extend(names.into_iter().map(|name| (name, at, true)));
                }
                "assignment" | "augmented_assignment" => {
                    targets(node.child_by_field_name("left"), false, &mut bound)
                }
                "import_statement" => {
                    let mut cursor = node.walk();
                    for target in node.children_by_field_name("name", &mut cursor) {
                        let (local, module) = match target.kind() {
                            "aliased_import" => (
                                target.child_by_field_name("alias"),
                                target.child_by_field_name("name"),
                            ),
                            _ => (target.named_child(0), Some(target)),
                        };
                        let (Some(local), Some(module)) = (local, module) else {
                            continue;
                        };
                        let local = get_node_text(local, source);
                        let module = python_module_path(module, source);
                        if is_module {
                            let root_name = if target.kind() == "aliased_import" {
                                module.clone()
                            } else {
                                local.clone()
                            };
                            facts
                                .imports
                                .entry(local.clone())
                                .or_default()
                                .push(root_name);
                            if module == "builtins" {
                                facts
                                    .builtins_modules
                                    .push(PythonBinding { name: local, at });
                                continue;
                            }
                        }
                        bound.push((local, at, false));
                    }
                }
                "import_from_statement" => {
                    let module = node
                        .child_by_field_name("module_name")
                        .map(|module| python_module_path(module, source))
                        .unwrap_or_default();
                    let (imported, locals) = python_import_bindings(node, source);
                    for (imported, local) in imported.into_iter().zip(locals) {
                        if is_module {
                            facts
                                .imports
                                .entry(local.clone())
                                .or_default()
                                .push(format!("{module}.{imported}"));
                            if module == "builtins" {
                                facts
                                    .builtins_members
                                    .push((PythonBinding { name: local, at }, imported));
                                continue;
                            }
                        }
                        bound.push((local, at, false));
                    }
                }
                _ => {}
            }
            work.extend(body(node).into_iter().rev());
        }
        if is_module {
            for (name, at, rebinding) in bound {
                if rebinding {
                    facts.rebound.insert(name.clone());
                }
                facts.bindings.push(PythonBinding { name, at });
            }
        } else {
            // A function or class body binds the module's name only for a name
            // it declares `global` AND binds: `global A` alone binds nothing.
            for (name, _, _) in bound {
                if globals.contains(&name) {
                    facts.rebound.insert(name.clone());
                    facts.bindings.push(PythonBinding { name, at: 0 });
                }
            }
        }
    }
    facts
}

/// A Python module path as the import system reads it — `pkg.mod`, `.sibling`,
/// `..` — built from the path's identifiers rather than from the node's text,
/// so whitespace or a line continuation inside the path cannot leak into it.
pub(super) fn python_module_path(node: Node, source: &str) -> String {
    match node.kind() {
        "dotted_name" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .filter(|part| part.kind() == "identifier")
                .map(|part| get_node_text(part, source))
                .collect::<Vec<_>>()
                .join(".")
        }
        "relative_import" => {
            let mut path = String::new();
            let mut cursor = node.walk();
            for part in node.named_children(&mut cursor) {
                match part.kind() {
                    "import_prefix" => {
                        path.extend(get_node_text(part, source).chars().filter(|c| *c == '.'));
                    }
                    "dotted_name" => path.push_str(&python_module_path(part, source)),
                    _ => {}
                }
            }
            path
        }
        _ => get_node_text(node, source),
    }
}

/// The `(imported, local)` names a Python import binds, read off the
/// statement's `name` fields — each a `dotted_name` or an `aliased_import`.
///
/// These used to come from splitting the statement's *text* on commas and
/// taking the first word of each part. A comment is a word, so
/// `from m import (  # noqa: E402\n    FIRST,  # why\n    second,\n)` bound
/// `#` twice and lost both real names — no edge, and no unresolved row either,
/// because the reference that named `FIRST` never met a binding for it. The
/// grammar keeps comments out of these fields, so nothing but a name is read.
/// A wildcard is a `wildcard_import` child, not a `name`, so it binds nothing.
pub(super) fn python_import_bindings(node: Node, source: &str) -> (Vec<String>, Vec<String>) {
    let mut imported = Vec::new();
    let mut locals = Vec::new();
    let mut cursor = node.walk();
    for target in node.children_by_field_name("name", &mut cursor) {
        let (name_node, alias_node) = match target.kind() {
            "aliased_import" => (
                target.child_by_field_name("name"),
                target.child_by_field_name("alias"),
            ),
            _ => (Some(target), None),
        };
        let Some(name) = name_node
            .map(|name| python_module_path(name, source))
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        let local = alias_node
            .map(|alias| get_node_text(alias, source))
            .filter(|alias| !alias.is_empty())
            .unwrap_or_else(|| name.clone());
        imported.push(name);
        locals.push(local);
    }
    (imported, locals)
}

/// The `(imported, local)` names a TS/JS binding clause declares, read off its
/// `import_specifier` / `export_specifier` children.
///
/// The text-splitting reader this replaced took the first word of each
/// comma-separated part, so any token that is not a name became one: a comment
/// (`{ a, // why\n b }` bound `//` and lost `b`) and an inline `type` modifier
/// (`{ type Foo, bar }` bound `type` and lost `Foo`). A specifier's `name` and
/// `alias` fields hold only an identifier or a string, so nothing else is read.
/// The names a CommonJS `require(...)` binds, in the shape an ES import of the
/// same meaning records.
///
/// `const path = require("node:path")` is `import * as path`: the alias plus a
/// `*` name. `const { a, b: c } = require("./x")` is `import { a, b as c }`.
/// Only a `require` that *is* a declarator's value binds anything; a bare
/// side-effect `require` and `require("./x").member` name no handle for the
/// module, and inventing one would bind a name the author never wrote.
pub(super) fn js_require_bindings(
    call: Node,
    source: &str,
) -> (Option<String>, Vec<String>, Vec<String>) {
    let unbound = (None, Vec::new(), Vec::new());
    // `const m = await import('./x')` binds the same handle `require` does; the
    // `await` (and any parentheses) is how the promise becomes the module.
    let mut value = call;
    while let Some(parent) = value.parent().filter(|parent| {
        matches!(
            parent.kind(),
            "await_expression" | "parenthesized_expression"
        )
    }) {
        value = parent;
    }
    let Some(declarator) = value
        .parent()
        .filter(|parent| parent.kind() == "variable_declarator")
        .filter(|parent| {
            parent
                .child_by_field_name("value")
                .is_some_and(|bound| bound.id() == value.id())
        })
    else {
        return unbound;
    };
    let Some(pattern) = declarator.child_by_field_name("name") else {
        return unbound;
    };
    match pattern.kind() {
        "identifier" => {
            let local = get_node_text(pattern, source);
            (Some(local.clone()), vec!["*".to_string()], vec![local])
        }
        "object_pattern" => {
            let mut names = Vec::new();
            let mut locals = Vec::new();
            let mut cursor = pattern.walk();
            for property in pattern.named_children(&mut cursor) {
                let (name, local) = match property.kind() {
                    "shorthand_property_identifier_pattern" => {
                        let name = get_node_text(property, source);
                        (name.clone(), name)
                    }
                    // `{ a = fallback }` binds `a`.
                    "object_assignment_pattern" => {
                        let Some(left) = property
                            .child_by_field_name("left")
                            .filter(|left| left.kind() == "shorthand_property_identifier_pattern")
                        else {
                            continue;
                        };
                        let name = get_node_text(left, source);
                        (name.clone(), name)
                    }
                    // `{ a: b }` binds `b` to the module's `a`. A nested
                    // pattern (`{ a: { b } }`) binds a member of a member,
                    // which no import shape can say.
                    "pair_pattern" => {
                        let (Some(key), Some(value)) = (
                            property.child_by_field_name("key"),
                            property.child_by_field_name("value"),
                        ) else {
                            continue;
                        };
                        if key.kind() != "property_identifier" || value.kind() != "identifier" {
                            continue;
                        }
                        (get_node_text(key, source), get_node_text(value, source))
                    }
                    _ => continue,
                };
                if !name.is_empty() && !local.is_empty() {
                    names.push(name);
                    locals.push(local);
                }
            }
            (None, names, locals)
        }
        _ => unbound,
    }
}

/// The names a dynamic `import(...)` binds.
///
/// Two shapes bind something: `const m = await import('./x')` (the module
/// handle, exactly as `require` binds it) and `const Page = lazy(() =>
/// import('./Page'))` or `React.lazy(...)`, which binds `Page` to the module's
/// **default** export — `lazy` demands one. A `.then(m => ({ default: m.X }))`
/// thunk is not matched: which member becomes the component is the author's
/// expression, not a name.
pub(super) fn js_dynamic_import_bindings(
    call: Node,
    source: &str,
) -> (Option<String>, Vec<String>, Vec<String>) {
    let bound = js_require_bindings(call, source);
    if !bound.1.is_empty() {
        return bound;
    }
    let lazy_binding = || -> Option<String> {
        let thunk = call
            .parent()
            .filter(|parent| parent.kind() == "arrow_function")?;
        if thunk
            .child_by_field_name("body")
            .is_none_or(|body| body.id() != call.id())
        {
            return None;
        }
        let arguments = thunk
            .parent()
            .filter(|parent| parent.kind() == "arguments")?;
        let wrapper = arguments
            .parent()
            .filter(|parent| parent.kind() == "call_expression")?;
        let callee = wrapper.child_by_field_name("function")?;
        if !matches!(
            get_node_text(callee, source).as_str(),
            "lazy" | "React.lazy"
        ) {
            return None;
        }
        let declarator = wrapper
            .parent()
            .filter(|parent| parent.kind() == "variable_declarator")?;
        let name = declarator.child_by_field_name("name")?;
        (name.kind() == "identifier").then(|| get_node_text(name, source))
    };
    match lazy_binding() {
        Some(local) => (None, vec!["default".to_string()], vec![local]),
        None => bound,
    }
}

pub(super) fn js_clause_bindings(clause: Node, source: &str) -> (Vec<String>, Vec<String>) {
    let mut names = Vec::new();
    let mut locals = Vec::new();
    let mut cursor = clause.walk();
    for specifier in clause.named_children(&mut cursor) {
        if !matches!(specifier.kind(), "import_specifier" | "export_specifier") {
            continue;
        }
        let Some(name) = specifier
            .child_by_field_name("name")
            .map(|name| js_specifier_name(name, source))
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        let local = specifier
            .child_by_field_name("alias")
            .map(|alias| js_specifier_name(alias, source))
            .filter(|alias| !alias.is_empty())
            .unwrap_or_else(|| name.clone());
        names.push(name);
        locals.push(local);
    }
    (names, locals)
}

/// A specifier name: an identifier verbatim, or the contents of a string name
/// (`import { "a-b" as ab }`), which is how ES2022 spells a non-identifier one.
pub(super) fn js_specifier_name(node: Node, source: &str) -> String {
    let text = get_node_text(node, source);
    if node.kind() == "string" {
        text.trim_matches(['"', '\'']).to_string()
    } else {
        text
    }
}

/// Every name a TS/JS declarator's `name` binds, in source order.
///
/// A destructuring declarator binds each name in its pattern —
/// `const { a, b: renamed, c = 1, ...rest } = obj` binds `a`, `renamed`, `c`
/// and `rest`. Reading the `name` field as text instead emitted one symbol
/// literally named `{ a, b: renamed, c = 1, ...rest }`: a name no program can
/// reference, so a fabricated node that every consumer could only report dead.
/// A member target (`[obj.x] = …`) binds nothing new and is skipped.
///
/// An explicit stack, for the same reason as [`python_bound_names`].
pub(super) fn js_pattern_bound_names(target: Node, source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut stack = vec![target];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "identifier" | "shorthand_property_identifier_pattern" => {
                let name = get_node_text(node, source);
                if !name.is_empty() {
                    names.push(name);
                }
            }
            "object_pattern" | "array_pattern" | "rest_pattern" => {
                let mut cursor = node.walk();
                let children: Vec<Node> = node.named_children(&mut cursor).collect();
                stack.extend(children.into_iter().rev());
            }
            "pair_pattern" => stack.extend(node.child_by_field_name("value")),
            "assignment_pattern" | "object_assignment_pattern" => {
                stack.extend(node.child_by_field_name("left"));
            }
            _ => {}
        }
    }
    names
}

/// Local names a TS/JS module exports by a statement other than the
/// declaration's own `export` keyword: `export { local }`,
/// `export { local as Public }` and `export default local`.
///
/// A clause names a *local* binding, and a module-level binding was only kept
/// when its declaration carried `export`, so `const local = 1; export { local };`
/// left `local` out of the graph while another file imported it. Read from the
/// already-extracted exports (a clause with no `from`) plus the root's
/// `export default <identifier>` statements, which bind no clause.
pub(super) fn js_locally_exported_names(
    root: Node,
    source: &str,
    exports: &[ExtractedExport],
) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = exports
        .iter()
        .filter(|export| export.module_specifier.is_none())
        .filter_map(|export| export.local_name.clone())
        .collect();
    let mut cursor = root.walk();
    for statement in root.named_children(&mut cursor) {
        if statement.kind() != "export_statement" {
            continue;
        }
        if let Some(value) = statement.child_by_field_name("value") {
            if value.kind() == "identifier" {
                names.insert(get_node_text(value, source));
            }
        }
    }
    names
}

/// The `Variable` symbol for a module-level binding, built in one place for the
/// languages that emit them.
pub(super) fn module_binding_symbol(
    file_symbol_name: &str,
    name: String,
    is_exported: bool,
    span: Span,
) -> ExtractedSymbol {
    ExtractedSymbol {
        qualified_name: format!("{file_symbol_name}::{name}"),
        name,
        kind: SymbolKind::Variable,
        span,
        is_exported,
        docstring: None,
        signature: None,
        parent_symbol: Some(file_symbol_name.to_string()),
        body_signature: None,
        declaration_hash: None,
        return_type: None,
    }
}

/// Extraction for a language with no linked grammar.
///
/// Emits the `File` node, then tries tier-2 pattern recovery for declarations.
///
/// A file that reached here used to contribute *no nodes whatsoever*: the
/// `File` node was pushed only on the tree-sitter path above, so there was no
/// symbol, no span, and nothing for an edge to point at. On two real trees that
/// silently swallowed every `.proto`, `.ps1`, `.vb`, `.vue` and `.metal` file
/// in them. Both halves are closed now — the file is always addressable, and
/// its declarations are recovered when the language has any to recover.
///
/// The two *declaration* outcomes stay labelled differently on purpose. When
/// the scanner finds declarations the file reports `RegexFallback` /
/// `ParseOutcome::Fallback`, so no consumer can mistake a pattern-matched
/// symbol for a parsed one. When it finds none the outcome stays `Failed`,
/// because no declaration was recovered and the `File` node is not a
/// declaration — reporting success on the strength of it would be the lie the
/// labelling exists to prevent.
///
/// A parse this build *refused to complete*, reported as such.
///
/// Distinct from `unavailable_extraction`, which answers "there is no grammar
/// for this language". Here a grammar exists and something went wrong with it —
/// a budget overrun or a load fault — and saying "no linked tree-sitter grammar
/// for {lang}" would be false. It matters most for the case that looks like
/// nothing: a tree-sitter ABI break would otherwise downgrade every file of a
/// language to regex fallback, be cache-admitted, exempt them all from
/// dead-code analysis, and leave the build green.
///
/// No declarations are recovered by pattern here. A file whose parse was
/// abandoned has an unknown structure, and a pattern scan over it would produce
/// a plausible-looking symbol set that nothing verified.
pub(crate) fn refused_extraction(
    path: &str,
    lang: &str,
    source: &str,
    reason: String,
) -> Extraction {
    unparsed_extraction(
        path,
        lang,
        source,
        ExtractionEngine::Unavailable {
            requested_language: lang.to_string(),
        },
        ParseOutcome::Failed { reason },
    )
}

/// A parse this build **chose not to attempt**, reported as such.
///
/// Distinct from `refused_extraction` in both fields, and both differences are
/// the same claim: nothing went wrong here. The engine is `NotApplicable`
/// rather than `Unavailable` because no grammar was wanted — one exists and is
/// linked, and asking it was simply not worth the clock — and the outcome is
/// `Skipped` rather than `Failed` because "we did not try" is not a symptom a
/// maintainer should be sent to investigate, and because a decision about a
/// file's name is a stable verdict that may be cached.
///
/// Shares its body with the refusal path deliberately. Both emit the `File`
/// node and the wiring annotations and claim nothing else; when that shape was
/// two copies, one of them was fixed and the other was not.
pub(crate) fn skipped_extraction(
    path: &str,
    lang: &str,
    source: &str,
    reason: String,
) -> Extraction {
    unparsed_extraction(
        path,
        lang,
        source,
        ExtractionEngine::NotApplicable {
            language: lang.to_string(),
        },
        ParseOutcome::Skipped { reason },
    )
}
