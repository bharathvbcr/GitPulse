use super::*;

/// Number of cached scopes. Test-only: the clear has no other observable effect
/// until an address happens to be reused, which is exactly what makes a missing
/// clear so hard to catch in the wild.
#[cfg(test)]
pub(super) fn scope_locals_len() -> usize {
    SCOPE_LOCALS.with(|cache| cache.borrow().len())
}

pub(crate) fn is_callable_node(node: Node) -> bool {
    matches!(
        node.kind(),
        "function_definition"
            | "function_declaration"
            | "method_definition"
            | "function_item"
            | "function_signature_item"
            | "method_declaration"
            | "generator_function_declaration"
            | "generator_function"
            | "arrow_function"
            | "function_expression"
            | "lambda"
            // A closure is its own scope. Without this, `collect_non_symbol_locals`
            // walked into `|marker| { … }` and treated the parameter as a local
            // of the *enclosing* function, so a later `marker("x")` in that
            // function was RA1-killed even though the binding is not in scope.
            | "closure_expression"
            // Swift and Kotlin closures: their own scope for the same reason.
            | "lambda_literal"
    )
}

pub(super) fn enclosing_scope_node(node: Node) -> Node {
    let mut current = node;
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        if is_callable_node(parent) {
            return parent;
        }
        current = parent;
        ancestor = bounded_parent(parent);
    }
    current
}

/// Whether a defining identifier is a module-level binding the extractor emits
/// as a `Variable` — the mirror of the emitting arms, so "binds a symbol" and
/// "is emitted as one" cannot disagree.
///
/// Filed as a *local*, such a binding shadowed itself: every read of it at
/// module level — each line of a script, another constant's initializer — was
/// dropped before the resolver saw it, so a constant read only there had no
/// inbound edge and was reported dead at 0.9 (`OUT = Path(__file__)…`, then
/// `OUT / "x"`). Every arm requires module scope, so a function's own locals are
/// untouched.
///
/// The climb passes only through destructuring patterns, and at each step the
/// node it came from must be the *binding* part of its parent — a default
/// value or a computed key is a use. Every comparison is a direct child
/// identity, so the check is linear in the pattern's depth.
pub(super) fn module_binding_is_symbol(node: Node, source: &str) -> bool {
    let is_field_child = |parent: Node, field: &str, child: Node| {
        parent
            .child_by_field_name(field)
            .is_some_and(|bound| bound.id() == child.id())
    };
    let mut current = node;
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        match parent.kind() {
            "pattern_list" | "tuple_pattern" | "list_pattern" | "list_splat_pattern"
            | "object_pattern" | "array_pattern" | "rest_pattern" => {}
            "pair_pattern" => {
                if !is_field_child(parent, "value", current) {
                    return false;
                }
            }
            "assignment_pattern" | "object_assignment_pattern" => {
                if !is_field_child(parent, "left", current) {
                    return false;
                }
            }
            // Python. The root check inside `python_is_module_scope` keeps
            // other grammars' `assignment` out.
            "assignment" => {
                return is_field_child(parent, "left", current) && python_is_module_scope(parent);
            }
            "variable_declarator" => {
                return is_field_child(parent, "name", current)
                    && is_module_level(parent)
                    && (js_symbol_is_exported(parent, source) || js_owns_member_symbols(parent));
            }
            "const_spec" | "var_spec" => {
                let mut cursor = parent.walk();
                return parent
                    .children_by_field_name("name", &mut cursor)
                    .any(|name| name.id() == node.id())
                    && !go_name_is_unexported(&get_node_text(node, source))
                    && is_module_level(parent);
            }
            "const_item" | "static_item" => {
                return is_field_child(parent, "name", current)
                    && rust_item_is_pub(parent, source)
                    && is_module_level(parent);
            }
            _ => return false,
        }
        current = parent;
        ancestor = bounded_parent(parent);
    }
    false
}

/// Whether a Rust item carries a `pub` visibility of any reach.
pub(super) fn rust_item_is_pub(node: Node, source: &str) -> bool {
    let mut cursor = node.walk();
    let is_pub = node.children(&mut cursor).any(|child| {
        child.kind() == "visibility_modifier" && get_node_text(child, source).starts_with("pub")
    });
    is_pub
}

pub(super) fn is_symbol_binding(node: Node, source: &str) -> bool {
    // A C-family function name binds a symbol, not a local. Without this the
    // `declarator` arm of `is_defining_name` would file every function's own
    // identifier into `collect_non_symbol_locals`, and `name_is_shadowed_by_local`
    // would then drop non-call references to it from inside its own body.
    if c_declarator_declaration(node).is_some_and(|declaration| {
        matches!(declaration.kind(), "function_definition" | "function_item")
    }) {
        return true;
    }
    // Same reasoning for Objective-C, whose class names and selector parts are
    // unnamed children rather than declarator chains.
    if is_objc_declaring_identifier(node) {
        return true;
    }
    // A Swift member property is a `Field` symbol. Filed as a file-scope local
    // instead, its own name shadowed every bare read of it written outside a
    // function body — a computed property's getter, a property initializer —
    // and the read was dropped before the resolver saw it.
    //
    // An enum case is the same: it is a symbol, so its own spelling is no local
    // either, and `.circle` in a property initializer must not be dropped
    // because `case circle` is declared in the same file.
    if crate::langdecl::swift::property_declaration_of(node).is_some()
        || crate::langdecl::swift::is_enum_case_name(node)
    {
        return true;
    }
    let Some(parent) = bounded_parent(node) else {
        return false;
    };
    if matches!(
        parent.kind(),
        "function_definition"
            | "function_declaration"
            | "method_definition"
            | "function_item"
            | "function_signature_item"
            | "method_declaration"
            | "generator_function_declaration"
            | "class_definition"
            | "class_declaration"
            | "abstract_class_declaration"
            | "type_spec"
            | "type_declaration"
            | "struct_item"
            | "enum_item"
            | "trait_item"
            | "enum_declaration"
    ) && field_contains(parent, "name", node)
    {
        return true;
    }
    if parent.kind() == "variable_declarator" && field_contains(parent, "name", node) {
        return parent.child_by_field_name("value").is_some_and(|value| {
            matches!(
                value.kind(),
                "arrow_function" | "function_expression" | "generator_function"
            ) || js_wrapped_function(value, source).is_some()
        });
    }
    false
}

pub(super) fn collect_non_symbol_locals(scope: Node, source: &str) -> HashSet<String> {
    let mut locals = HashSet::new();
    // Names a module-level `Variable` binds. Filtered out at the end rather
    // than per occurrence: `lines = [...]` binds the symbol and a later
    // `lines += [...]` is not itself a symbol binding, but both name the one
    // module attribute, so neither may make `lines` a local that shadows the
    // module-level reads of it. Only module scope can contain these.
    let mut module_bindings = HashSet::new();
    let mut worklist = Vec::new();
    push_children(scope, &mut worklist);
    let mut since_check = 0u32;
    while let Some(node) = worklist.pop() {
        // Same stride idiom as `walk_tree`: this loop is per-scope and a
        // pathological tree makes it the dominant cost of the whole
        // extraction, so it has to be interruptible on its own.
        since_check += 1;
        if since_check >= DEADLINE_CHECK_STRIDE {
            since_check = 0;
            if walk_deadline_passed() {
                break;
            }
        }
        if is_callable_node(node) || in_charged_region(node) {
            continue;
        }
        if matches!(
            node.kind(),
            "identifier"
                | "simple_identifier"
                | "shorthand_property_identifier"
                | "shorthand_property_identifier_pattern"
        ) && is_defining_name(node)
            && !js_names_a_loaded_module(node, source)
        {
            let name = get_node_text(node, source);
            if is_user_ident(&name) {
                if module_binding_is_symbol(node, source) {
                    module_bindings.insert(name);
                } else if !is_symbol_binding(node, source) {
                    locals.insert(name);
                }
            }
        }
        push_children_reversed(node, &mut worklist);
    }
    locals.retain(|name| !module_bindings.contains(name));
    locals
}

/// Whether a module-level declarator's value is an object literal with a
/// function-valued member, which makes the declarator the owner of those member
/// symbols ([`js_object_member_owner`]) and therefore a symbol itself even when
/// it is not exported: `const core = { log() {} }; export const l = { ...core }`.
pub(super) fn js_owns_member_symbols(declarator: Node) -> bool {
    let Some(mut value) = declarator.child_by_field_name("value") else {
        return false;
    };
    while matches!(
        value.kind(),
        "as_expression"
            | "satisfies_expression"
            | "parenthesized_expression"
            | "non_null_expression"
    ) {
        let Some(inner) = value.named_child(0) else {
            return false;
        };
        value = inner;
    }
    if value.kind() != "object" || !is_module_level(declarator) {
        return false;
    }
    let mut cursor = value.walk();
    let found = value.named_children(&mut cursor).any(|member| {
        member.kind() == "method_definition"
            || (member.kind() == "pair"
                && member
                    .child_by_field_name("value")
                    .is_some_and(|function| js_object_member_owner(function).is_some()))
    });
    found
}

/// Whether `name` is bound by a declarator whose value is a module load —
/// `const m = await import('./x')`, `const { a } = require('./y')`.
///
/// Such a name is an *import binding*, recorded as one in the file's imports;
/// counting it as a local variable as well made every `m.f()` through it "a
/// call on a local whose value is unknown", and the import rung never saw it.
pub(super) fn js_names_a_loaded_module(name: Node, source: &str) -> bool {
    let mut pattern = name;
    while let Some(parent) = pattern.parent() {
        if !matches!(
            parent.kind(),
            "object_pattern" | "pair_pattern" | "object_assignment_pattern"
        ) {
            break;
        }
        pattern = parent;
    }
    let Some(declarator) = pattern
        .parent()
        .filter(|parent| parent.kind() == "variable_declarator")
        .filter(|parent| {
            parent
                .child_by_field_name("name")
                .is_some_and(|bound| bound.id() == pattern.id())
        })
    else {
        return false;
    };
    let Some(mut value) = declarator.child_by_field_name("value") else {
        return false;
    };
    while matches!(
        value.kind(),
        "await_expression" | "parenthesized_expression"
    ) {
        let Some(inner) = value.named_child(0) else {
            return false;
        };
        value = inner;
    }
    value.kind() == "call_expression"
        && value.child_by_field_name("function").is_some_and(|callee| {
            matches!(get_node_text(callee, source).as_str(), "import" | "require")
        })
}

/// Run `visit` over one scope's local-name set, computing it once per scope.
pub(super) fn with_scope_locals<R>(
    scope: Node,
    source: &str,
    visit: impl FnOnce(&HashSet<String>) -> R,
) -> R {
    let id = scope.id();
    let known = SCOPE_LOCALS.with(|cache| cache.borrow().contains_key(&id));
    if !known {
        let locals = collect_non_symbol_locals(scope, source);
        if walk_overran() {
            // A set the deadline cut short is not this scope's local-name set.
            // The file is refused either way, but caching it would serve the
            // wrong answer to every later lookup of the same scope in the
            // meantime, so it is used once and not kept.
            return visit(&locals);
        }
        SCOPE_LOCALS.with(|cache| cache.borrow_mut().insert(id, locals));
    }
    SCOPE_LOCALS.with(|cache| {
        let borrowed = cache.borrow();
        match borrowed.get(&id) {
            Some(locals) => visit(locals),
            // Unreachable: inserted just above when absent, and nothing removes
            // an entry within a file. Answering "no locals" rather than
            // panicking keeps a cache defect from failing extraction.
            None => visit(&HashSet::new()),
        }
    })
}

/// Module fixtures with an explicit import from pytest. A bare decorator of
/// the same spelling, without that import, cannot establish framework binding.
pub(super) fn python_fixture_names(
    root: Node,
    source: &str,
    imports: &[ExtractedImport],
) -> BTreeSet<String> {
    if root.kind() != "module" {
        return BTreeSet::new();
    }
    let mut decorators = BTreeSet::new();
    for import in imports
        .iter()
        .filter(|import| import.module_specifier == "pytest")
    {
        if import.imported_names.is_empty() {
            decorators.insert(format!(
                "{}.fixture",
                import.alias.as_deref().unwrap_or("pytest")
            ));
        }
        for (index, name) in import
            .imported_names
            .iter()
            .enumerate()
            .filter(|(_, name)| *name == "fixture")
        {
            decorators.insert(
                import
                    .local_names
                    .get(index)
                    .or(import.alias.as_ref())
                    .unwrap_or(name)
                    .clone(),
            );
        }
    }
    if decorators.is_empty() {
        return BTreeSet::new();
    }
    let mut fixtures = BTreeSet::new();
    let mut cursor = root.walk();
    for decorated in root
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "decorated_definition")
    {
        if walk_deadline_passed() {
            break;
        }
        let Some(definition) = decorated
            .child_by_field_name("definition")
            .filter(|node| node.kind() == "function_definition")
        else {
            continue;
        };
        let mut cursor = decorated.walk();
        let registered = decorated
            .named_children(&mut cursor)
            .filter(|node| node.kind() == "decorator")
            .any(|node| {
                let text = get_node_text(node, source);
                let name = text
                    .trim()
                    .trim_start_matches('@')
                    .split('(')
                    .next()
                    .unwrap_or("")
                    .trim();
                decorators.contains(name)
            });
        if registered {
            if let Some(name) = definition.child_by_field_name("name") {
                fixtures.insert(get_node_text(name, source));
            }
        }
    }
    fixtures
}

/// Establish binding facts while the syntax tree still exists. A named graph
/// caller may own many anonymous scopes; it cannot stand in for lexical scope.
pub(super) fn collect_site_bindings(
    root: Node,
    source: &str,
    lang: &str,
    file_symbol_name: &str,
    calls: &[ExtractedCall],
    references: &[ExtractedReference],
    imports: &[ExtractedImport],
) -> Vec<LocalBinding> {
    let fixtures = python_fixture_names(root, source, imports);
    let declared_types = binding_declared_types(references);
    let mut initializers = binding_initializers(references, calls);
    collapse_initializers_to_statements(root, &mut initializers);
    let mut sites = BTreeSet::new();
    let mut parameters: HashMap<usize, BTreeSet<String>> = HashMap::new();
    // Rust only; built lazily per block, so it costs nothing elsewhere.
    let mut binders = crate::rustlocal::Binders::new(source);
    let inputs = calls
        .iter()
        .map(|call| {
            (
                &call.span,
                call.receiver_expr.as_deref().unwrap_or(&call.callee_name),
                false,
            )
        })
        .chain(
            references
                .iter()
                .filter(|r| {
                    matches!(
                        r.kind,
                        ReferenceKind::Name | ReferenceKind::Call | ReferenceKind::Constructor
                    )
                })
                .map(|r| {
                    (
                        &r.span,
                        r.receiver_expr.as_deref().unwrap_or(&r.name),
                        r.kind == ReferenceKind::Name,
                    )
                }),
        );
    for (span, name, is_name_reference) in inputs {
        if walk_deadline_passed() {
            break;
        }
        let Some(node) = root.descendant_for_byte_range(span.start_byte, span.end_byte) else {
            continue;
        };
        let mut ancestor = bounded_parent(node);
        while let Some(scope) = ancestor {
            let swift_scope = lang == "swift" && crate::swiftscope::is_scope(scope);
            if is_callable_node(scope) || swift_scope {
                // Python defaults run in the enclosing scope. The parameter's
                // own declaration is still a binding site, but its value is not.
                let in_default = scope.kind() == "function_definition"
                    && field_contains(scope, "parameters", node)
                    && (node.kind() == "call" || {
                        let mut cursor = bounded_parent(node);
                        let mut value = false;
                        while let Some(parent) = cursor {
                            if parent.id() == scope.id() {
                                break;
                            }
                            if field_contains(parent, "value", node)
                                || field_contains(parent, "type", node)
                            {
                                value = true;
                                break;
                            }
                            cursor = bounded_parent(parent);
                        }
                        value
                    });
                if swift_scope {
                    // Swift's names and written types come from its own scope
                    // reader; the shared passes read `identifier` and a
                    // `parameters` list, neither of which the grammar has.
                    let bound = crate::swiftscope::bindings(scope, source);
                    // `if let task = task(from: x)`: the binding is not in scope in
                    // its own initializer, so the use names the outer `task`.
                    if bound.contains_key(name)
                        && crate::swiftscope::use_is_in_own_initializer(node, source, name)
                    {
                        ancestor = bounded_parent(scope);
                        continue;
                    }
                    if let Some(written) = bound.get(name) {
                        let initializer = written.init.clone();
                        let written = written.ty.clone();
                        let scope_name = scope.child(0).and_then(|child| {
                            enclosing_callable_qualified(child, source, file_symbol_name)
                        });
                        sites.insert(LocalBinding {
                            start_byte: span.start_byte,
                            name: name.to_string(),
                            declared_type: written.or_else(|| {
                                binding_declared_type_for(&declared_types, &scope_name, name)
                            }),
                            initializer,
                            scope: scope_name,
                        });
                        break;
                    }
                } else if !in_default {
                    let names = parameters.entry(scope.id()).or_insert_with(|| {
                        let mut names = BTreeSet::new();
                        collect_parameter_names(scope, source, &mut names);
                        // A JavaScript arrow can have a single unparenthesized parameter.
                        if let Some(parameter) = scope.child_by_field_name("parameter") {
                            if parameter.kind() == "identifier" {
                                names.insert(get_node_text(parameter, source));
                            }
                        }
                        names
                    });
                    if names.contains(name)
                        || with_scope_locals(scope, source, |locals| locals.contains(name))
                    {
                        // A pytest signature requests a declared fixture. Preserve
                        // that dependency at the signature only; uses in its body
                        // refer to the injected value and are still local bindings.
                        let fixture_request = is_name_reference
                            && fixtures.contains(name)
                            && scope.kind() == "function_definition"
                            && field_contains(scope, "parameters", node)
                            && scope.child_by_field_name("name").is_some_and(|n| {
                                let consumer = get_node_text(n, source);
                                consumer.starts_with("test_") || fixtures.contains(&consumer)
                            });
                        if !fixture_request {
                            // Rust `let x = x()` / `if let Some(x) = x()`: the
                            // binding is not in scope in its own initializer, so
                            // the use names the outer item. Skipping this only
                            // when the name is *not* also a parameter keeps
                            // `fn f(x: T) { let x = x(); }` as a local.
                            if rust_use_is_in_same_name_initializer(node, source, name)
                                && !names.contains(name)
                            {
                                break;
                            }
                            // Kotlin's `val habit = habit(..)` is the same
                            // shape: the local is not in scope in its own
                            // initializer, so the call names the function.
                            if lang == "kotlin"
                                && kotlin_use_is_in_same_name_initializer(node, source, name)
                                && !names.contains(name)
                            {
                                break;
                            }
                            let named_scope = callable_binding_name(scope, source).is_some()
                                || is_c_family_callable(scope);
                            let scope_name = named_scope
                                .then(|| {
                                    scope.child(0).and_then(|child| {
                                        // Kotlin names a member `Type.m` and an
                                        // extension `Receiver.m`, which only the
                                        // declaration emitter spells; the generic
                                        // walk would answer `F::m`, so the binding
                                        // would never meet the type facts keyed by
                                        // the emitted identity.
                                        if lang == "kotlin" {
                                            crate::langcalls::scope::enclosing_emitted_symbol(
                                                child,
                                                source,
                                                lang,
                                                file_symbol_name,
                                            )
                                        } else {
                                            enclosing_callable_qualified(
                                                child,
                                                source,
                                                file_symbol_name,
                                            )
                                        }
                                    })
                                })
                                .flatten();
                            let mut declared_type =
                                binding_declared_type_for(&declared_types, &scope_name, name);
                            let mut initializer = initializer_at(&initializers, name, node, scope);
                            // The binder above this very use, where the
                            // function's text states its type — a `MutexGuard`
                            // from `Ok(guard)`, a loop variable over a `Vec<T>`.
                            // More specific than the `(scope, name)` facts,
                            // which one shadowing `let` can make about a
                            // different value, so it wins when it answers.
                            // What it cannot type alone — the value of
                            // `T::f(..)?`, a closure's parameter — it hands the
                            // resolver as an initializer shape, in place of the
                            // facts' guess at the same binding.
                            if lang == "rust" {
                                if let Some(stated) = binders.binder_type(node, name, scope) {
                                    declared_type = Some(stated);
                                } else if let Some(hint) = binders.binder_hint(node, name, scope) {
                                    initializer = Some(hint);
                                }
                            }
                            sites.insert(LocalBinding {
                                start_byte: span.start_byte,
                                name: name.to_string(),
                                scope: scope_name,
                                declared_type,
                                initializer,
                            });
                        }
                        break;
                    }
                }
            }
            ancestor = bounded_parent(scope);
        }
    }
    if matches!(lang, "javascript" | "typescript" | "tsx") {
        for call in calls {
            let on_this = call
                .receiver_expr
                .as_deref()
                .is_some_and(|receiver| receiver == "this" || receiver.starts_with("this."));
            if !on_this {
                continue;
            }
            let Some(node) =
                root.descendant_for_byte_range(call.span.start_byte, call.span.end_byte)
            else {
                continue;
            };
            if let Some(class) = js_lexical_this_class(node, source) {
                sites.insert(LocalBinding {
                    start_byte: call.span.start_byte,
                    name: "this".to_string(),
                    scope: call.caller_symbol.clone(),
                    declared_type: Some(class),
                    initializer: None,
                });
            }
        }
    }
    sites.into_iter().collect()
}

/// The class `this` denotes at `node` when it is *lexical*: written inside a
/// named arrow nested in a method or in a class field's initializer, where the
/// arrow's own caller symbol (`Gateway.open.connect`) is not a member of the
/// class and the enclosing-type rung cannot see the class in it.
///
/// An arrow has no `this` of its own, so the walk passes through arrows and
/// stops at the first thing that does: a method or field names the class; a
/// `function`, generator or nested class means `this` is something else, and
/// nothing is claimed. A call whose nearest named callable is the member itself
/// is the enclosing-type rung's, and is left to it, because that rung also
/// knows an indexed subtype may override the method.
pub(super) fn js_lexical_this_class(node: Node, source: &str) -> Option<String> {
    let mut through_named_arrow = false;
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        match parent.kind() {
            "arrow_function" => {
                if callable_binding_name(parent, source).is_some()
                    && js_function_valued_member(parent, source).is_none()
                {
                    through_named_arrow = true;
                }
            }
            "method_definition" | "public_field_definition" | "field_definition" => {
                return through_named_arrow
                    .then(|| enclosing_type_name(parent, source))
                    .flatten();
            }
            "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "generator_function"
            | "class_declaration"
            | "abstract_class_declaration"
            | "class" => return None,
            _ => {}
        }
        ancestor = bounded_parent(parent);
    }
    None
}

/// Type refs with `assigned_to`, keyed by `(enclosing_symbol, binding name)`.
pub(super) fn binding_declared_types(
    references: &[ExtractedReference],
) -> HashMap<(Option<String>, String), String> {
    let mut out = HashMap::new();
    for reference in references {
        if reference.kind != ReferenceKind::Type {
            continue;
        }
        let Some(bound) = reference.assigned_to.as_ref() else {
            continue;
        };
        if bound.is_empty() || reference.name.is_empty() {
            continue;
        }
        out.entry((reference.enclosing_symbol.clone(), bound.clone()))
            .or_insert_with(|| reference.name.clone());
    }
    out
}

/// Call/Constructor refs with `assigned_to`, reduced to a simple initializer
/// shape, as `binding name -> [(assignment start, shape)]` in source order.
///
/// Only the **outermost** expression of each assignment is kept. Every
/// argument of `p := NewDBLP(dep())` carries `assigned_to: p` too, and taking
/// `dep` would type `p` by its dependency; a candidate whose start lies inside
/// another candidate's call is an argument of it.
///
/// Keyed by name alone, not by enclosing symbol: which assignment a use reads
/// is decided by position in [`initializer_at`], because a name is commonly
/// reassigned in one function — every `t.Run(…, func() { p := … })` subtest
/// rebinds `p` — and the first assignment is not the one a later use sees.
pub(super) fn binding_initializers(
    references: &[ExtractedReference],
    calls: &[ExtractedCall],
) -> HashMap<String, Vec<(usize, String)>> {
    // (start, end of the expression it begins, shape) per bound name.
    let mut candidates: HashMap<String, Vec<(usize, usize, String)>> = HashMap::new();
    for reference in references {
        let Some(bound) = reference.assigned_to.as_ref() else {
            continue;
        };
        if bound.is_empty() {
            continue;
        }
        let Some(shape) = reference.initializer_shape(calls) else {
            continue;
        };
        let extent_end = calls
            .iter()
            .filter(|call| {
                call.callee_name == reference.name
                    && call.span.start_byte <= reference.span.start_byte
                    && call.span.end_byte >= reference.span.end_byte
            })
            .map(|call| call.span.end_byte)
            .min()
            .unwrap_or(reference.span.end_byte);
        candidates.entry(bound.clone()).or_default().push((
            reference.span.start_byte,
            extent_end,
            shape,
        ));
    }
    candidates
        .into_iter()
        .map(|(name, found)| {
            let mut outermost: Vec<(usize, String)> = found
                .iter()
                .filter(|(start, end, _)| {
                    !found.iter().any(|(other_start, other_end, _)| {
                        // An argument of another candidate's call.
                        (other_start < start && start < other_end)
                            // The outer link of a chain rooted at another
                            // candidate: `PipeDrain::new(r).expect(..)` starts
                            // where `PipeDrain::new` starts and contains it.
                            // The value is typed by the root, and the outer
                            // link's shape names a method of an expression.
                            || (other_start == start && other_end < end)
                    })
                })
                .map(|(start, _, shape)| (*start, shape.clone()))
                .collect();
            outermost.sort();
            outermost.dedup();
            (name, outermost)
        })
        .collect()
}

/// Keep one initializer per assignment *statement*: the one that starts it.
///
/// [`binding_initializers`] drops a call's own arguments and a chain's outer
/// links by span, but a struct or composite literal has no call span: in
/// `let s = Scanner { bytes: text.as_bytes(), .. }` the field's
/// `text.as_bytes` carries `assigned_to: s` too, starts after `Scanner`, and —
/// being nearer the use — would be read as the value. Grouping by the
/// statement each candidate sits in, and keeping the earliest, leaves the
/// expression the statement assigns. Statements are told apart with the tree,
/// which this pass has and `binding_initializers` does not.
pub(super) fn collapse_initializers_to_statements(
    root: Node,
    initializers: &mut HashMap<String, Vec<(usize, String)>>,
) {
    for assignments in initializers.values_mut() {
        // Sorted by start, so the first candidate of each statement is its
        // earliest; a candidate whose statement the tree cannot name stands
        // alone.
        let mut current: Option<usize> = None;
        assignments.retain(|(start, _)| {
            let statement = statement_start(root, *start);
            let repeats = statement.is_some() && statement == current;
            current = statement;
            !repeats
        });
    }
}

/// Start byte of the statement holding the byte at `start`: the highest
/// ancestor below a block, a callable, or the file.
pub(super) fn statement_start(root: Node, start: usize) -> Option<usize> {
    statement_node(root, start).map(|statement| statement.start_byte())
}

/// The statement holding the byte at `start`, as [`statement_start`] finds it.
pub(super) fn statement_node(root: Node, start: usize) -> Option<Node> {
    let mut node = root.descendant_for_byte_range(start, start)?;
    while let Some(parent) = bounded_parent(node) {
        if parent.id() == root.id()
            || is_callable_node(parent)
            || matches!(
                parent.kind(),
                "block"
                    | "statement_block"
                    | "compound_statement"
                    | "func_literal"
                    | "source_file"
                    | "program"
                    | "module"
                    | "class_body"
                    | "declaration_list"
            )
        {
            return Some(node);
        }
        node = parent;
    }
    Some(node)
}

/// Whether a use inside the statement that assigns `name` at `start` reads the
/// binding *before* that statement.
///
/// `let wrapped = spawn(move || { wrapped.snapshot(); wrapped })` moves the
/// earlier `wrapped` into the closure: the right-hand side is evaluated before
/// the name is bound, so a use there — `x = wrap(x)` too — sees what the name
/// held before. A closure is the exception where the variable is the same one
/// and is read when the closure runs, after the assignment: `x = make(() =>
/// x.run())` in JavaScript or Python, `c = Wrap(func() { c.Ping() })` in Go.
/// A Rust `let` and a Go `:=` or `var` declare a fresh variable whose scope
/// begins after the statement, so even a closure there reads the earlier one.
/// A Rust closure (`closure_expression`) is never the exception: it captures
/// when it is created, so `w = make(|| w.run())` reads the earlier `w` too.
pub(super) fn use_reads_the_earlier_binding(region: Node, start: usize, use_node: Node) -> bool {
    let Some(statement) = statement_node(region, start) else {
        return false;
    };
    let use_start = use_node.start_byte();
    if !(statement.start_byte() <= use_start && use_start < statement.end_byte()) {
        return false;
    }
    if matches!(
        statement.kind(),
        "let_declaration" | "short_var_declaration" | "var_declaration"
    ) {
        return true;
    }
    let mut node = use_node;
    while let Some(parent) = bounded_parent(node) {
        if parent.id() == statement.id() {
            break;
        }
        // Closures that read a captured variable when they run.
        if matches!(
            parent.kind(),
            "func_literal"
                | "arrow_function"
                | "function_expression"
                | "function"
                | "generator_function"
                | "lambda"
        ) {
            return false;
        }
        node = parent;
    }
    true
}

/// The initializer of `name` that a use at `use_node` reads, searched outward
/// from the use to `scope` — the callable that binds the name.
///
/// At each enclosing region (a callable, or a Go `func_literal`, which
/// `is_callable_node` does not count as a scope) the assignments to `name`
/// inside the region and before the use are walked from the nearest back:
///
/// - `x = x.with_y()` re-assigns the value from itself — a builder step — and
///   says nothing new about its type, so it is passed over;
/// - a use inside the assignment's own right-hand side reads the value from
///   before it ([`use_reads_the_earlier_binding`]), so it is passed over too;
/// - an assignment inside a block that does not also hold the use ran only on
///   some paths (`if ieee { p = NewIEEE() }`), so its shape is collected and
///   the walk continues to the value it may have left in place;
/// - the first assignment that runs on every path to the use ends the walk.
///
/// Every collected shape must agree, or the answer is `None`: naming one
/// branch's constructor would type the other branch's value. A region with no
/// assignment widens the search. This replaced a map that kept the *first*
/// assignment in the enclosing function, which typed every `t.Run` subtest's
/// `p` by the first subtest's constructor.
pub(super) fn initializer_at(
    initializers: &HashMap<String, Vec<(usize, String)>>,
    name: &str,
    use_node: Node,
    scope: Node,
) -> Option<String> {
    let assignments = initializers.get(name)?;
    let use_start = use_node.start_byte();
    let mut region = bounded_parent(use_node);
    while let Some(node) = region {
        let is_scope = node.id() == scope.id();
        if is_scope || is_callable_node(node) || node.kind() == "func_literal" {
            let mut seen: Option<&str> = None;
            let mut settled = false;
            for (start, shape) in assignments
                .iter()
                .rev()
                .filter(|(start, _)| *start >= node.start_byte() && *start < use_start)
            {
                if reassigns_from_itself(shape, name)
                    || use_reads_the_earlier_binding(node, *start, use_node)
                {
                    continue;
                }
                match seen {
                    Some(earlier) if earlier != shape.as_str() => return None,
                    _ => seen = Some(shape.as_str()),
                }
                if !assignment_is_conditional(node, *start, use_start) {
                    settled = true;
                    break;
                }
            }
            if let Some(shape) = seen {
                // Conditional assignments with nothing unconditional before
                // them: on some path the binding came from outside this
                // region, so the shape is not the only value it can hold.
                return settled.then(|| shape.to_string());
            }
        }
        if is_scope {
            break;
        }
        region = bounded_parent(node);
    }
    None
}

/// `x = x.step()`: an initializer whose receiver is the binding itself.
///
/// A method call on the binding, and nothing else: `lexical::store::Builder::
/// new` is a path whose first segment merely shares the binding's name, and a
/// chain written across lines (`window\n  .center()`) carries the line break
/// in its receiver text.
pub(super) fn reassigns_from_itself(shape: &str, name: &str) -> bool {
    let compact: String = shape.chars().filter(|c| !c.is_whitespace()).collect();
    compact
        .strip_prefix(name)
        .is_some_and(|rest| rest.starts_with('.'))
}

/// Whether the assignment starting at `start` sits in a block that does not
/// also contain the use at `use_start`, between itself and `region`.
pub(super) fn assignment_is_conditional(region: Node, start: usize, use_start: usize) -> bool {
    let Some(mut node) = region.descendant_for_byte_range(start, start) else {
        return true;
    };
    while node.id() != region.id() {
        let contains_use = node.start_byte() <= use_start && use_start < node.end_byte();
        if contains_use {
            return false;
        }
        if matches!(
            node.kind(),
            "block" | "statement_block" | "compound_statement" | "else_clause"
        ) {
            return true;
        }
        let Some(parent) = bounded_parent(node) else {
            return true;
        };
        node = parent;
    }
    false
}

pub(super) fn binding_declared_type_for(
    declared_types: &HashMap<(Option<String>, String), String>,
    scope: &Option<String>,
    name: &str,
) -> Option<String> {
    let key = (scope.clone(), name.to_string());
    declared_types.get(&key).cloned().or_else(|| {
        // File-scoped Type bindings (rare) and unscoped parameter rows.
        declared_types.get(&(None, name.to_string())).cloned()
    })
}

pub(super) fn name_is_shadowed_by_local(node: Node, source: &str, name: &str) -> bool {
    let scope = enclosing_scope_node(node);
    with_scope_locals(scope, source, |locals| locals.contains(name))
}

/// Every value a callable binds itself, keyed by the identity a call made inside
/// it reports as its caller.
///
/// The pairing is what makes this usable. `ExtractedCall::caller_symbol` comes
/// from `enclosing_callable_qualified`, and that walk deliberately *skips*
/// unnamed callbacks — `useEffect(() => { … })` attributes to the enclosing
/// named function rather than dropping to file scope. Naming the scopes here by
/// the callable node instead would file an arrow's parameters under a scope no
/// call ever names, and the resolver's lookup would never hit. So each callable
/// is named by running the same walk from one of its own children, and callables
/// that resolve to the same name — a function and every unnamed closure inside
/// it — merge into one set.
///
/// A callable at file scope resolves to `None` and is skipped: a call there
/// reports the file as its caller, and `scope_declares_local` refuses that key
/// outright rather than matching on a string shape.
///
/// `BTreeMap`/`BTreeSet` rather than the hashed containers `collect_non_symbol_locals`
/// returns, because this is serialized and the determinism gate digests it.
/// Nodes that hold a callable's parameters directly, one per grammar family.
pub(super) const PARAMETER_LIST_KINDS: [&str; 5] = [
    // Python `def`/`class`, Rust `fn`.
    "parameters",
    // JavaScript, TypeScript.
    "formal_parameters",
    "lambda_parameters",
    "closure_parameters",
    // Go.
    "parameter_list",
];

/// Swift and Kotlin spell a parameter with `simple_identifier` (Swift) or a
/// bare `identifier` inside `parameter` / `variable_declaration` (Kotlin), and
/// neither sits under one of [`PARAMETER_LIST_KINDS`], so the generic walk found
/// no parameter in either language. A parameter is a binding the bare-name rung
/// must never bind to a project namesake: Swift's `completion`, Kotlin's
/// `onStart`. Returns whether `callable` is one of their callables.
///
/// Covers Swift `func` and `init` parameters (the internal name, not the
/// argument label) and closure parameters, and Kotlin `fun` parameters and
/// lambda parameters. A lambda with no declared parameter binds the implicit
/// `it` (Kotlin) or `$0`.. (Swift) instead.
pub(super) fn collect_swift_kotlin_parameter_names(
    callable: Node,
    source: &str,
    out: &mut BTreeSet<String>,
) -> bool {
    let mut insert = |node: Node| {
        let name = get_node_text(node, source);
        if is_user_ident(&name) {
            out.insert(name);
        }
    };
    let mut cursor = callable.walk();
    match callable.kind() {
        // Swift `func` / `init`: `parameter` children hang off the declaration;
        // the type is also a `name` field, so the kind picks the binding.
        "function_declaration" | "init_declaration"
            if callable
                .child_by_field_name("body")
                .is_some_and(|b| b.kind() == "function_body") =>
        {
            for parameter in callable
                .children(&mut cursor)
                .filter(|c| c.kind() == "parameter")
            {
                let mut inner = parameter.walk();
                let name = parameter
                    .children_by_field_name("name", &mut inner)
                    .find(|n| n.kind() == "simple_identifier");
                if let Some(name) = name {
                    insert(name);
                }
            }
            true
        }
        "lambda_literal" => {
            let mut declared = false;
            let mut stack = vec![callable];
            while let Some(node) = stack.pop() {
                let mut inner = node.walk();
                for child in node.children(&mut inner) {
                    match child.kind() {
                        // Swift: `{ (a: Int, b) in … }` / `{ s in … }`.
                        "lambda_parameter" => {
                            let mut c = child.walk();
                            let name = child
                                .children_by_field_name("name", &mut c)
                                .find(|n| n.kind() == "simple_identifier");
                            if let Some(name) = name {
                                insert(name);
                                declared = true;
                            }
                        }
                        // Kotlin: `{ a: Int, b -> … }`.
                        "variable_declaration" if node.kind() == "lambda_parameters" => {
                            if let Some(name) = child.named_child(0) {
                                insert(name);
                                declared = true;
                            }
                        }
                        "lambda_function_type"
                        | "lambda_function_type_parameters"
                        | "lambda_parameters" => {
                            stack.push(child);
                        }
                        _ => {}
                    }
                }
            }
            if !declared {
                for implicit in ["it", "$0", "$1", "$2"] {
                    out.insert(implicit.to_string());
                }
            }
            true
        }
        // Kotlin `fun`: `function_value_parameters > parameter > identifier`.
        "function_declaration" => {
            let Some(list) = callable
                .children(&mut cursor)
                .find(|c| c.kind() == "function_value_parameters")
            else {
                return false;
            };
            let mut inner = list.walk();
            for parameter in list
                .children(&mut inner)
                .filter(|c| c.kind() == "parameter")
            {
                if let Some(name) = parameter
                    .named_child(0)
                    .filter(|n| n.kind() == "identifier")
                {
                    insert(name);
                }
            }
            true
        }
        _ => false,
    }
}

/// Names a callable's own signature binds.
///
/// Deliberately **not** folded into `collect_non_symbol_locals`, which answers a
/// different question — "does a local here shadow a reference to a symbol" — and
/// whose answers are consumed by reference emission. Merging them looked
/// tempting and was measured wrong: pytest's `def test_x(mapper)` names a
/// module-level fixture, so that parameter is genuinely *both* a binding and a
/// use, and suppressing it as a shadow deleted 231 real `References` edges,
/// several of them the only inbound edge a fixture had. Same data, two
/// questions, two owners — the rule SC17 and SC25 both landed on.
///
/// A parameter's declared type and default value are uses, not bindings, so the
/// `type` and `value` fields are not descended into: `def f(x: Widget)` binds
/// `x` and refers to `Widget`.
pub(super) fn collect_parameter_names(callable: Node, source: &str, out: &mut BTreeSet<String>) {
    if collect_swift_kotlin_parameter_names(callable, source, out) {
        return;
    }
    let mut params_cursor = callable.walk();
    let Some(params) = callable
        .children(&mut params_cursor)
        .find(|child| PARAMETER_LIST_KINDS.contains(&child.kind()))
    else {
        return;
    };
    let mut worklist = vec![params];
    while let Some(node) = worklist.pop() {
        if node.kind() == "identifier" {
            let name = get_node_text(node, source);
            if is_user_ident(&name) {
                out.insert(name);
            }
        }
        let mut cursor = node.walk();
        for (index, child) in node.children(&mut cursor).enumerate() {
            let field = node.field_name_for_child(index as u32);
            if matches!(field, Some("type") | Some("value")) {
                continue;
            }
            // A destructuring default — `{ sleep = defaultSleep }` — keeps its
            // value under `right`, not `value`. It is an expression evaluated
            // when the property is absent, not a name the signature binds;
            // collecting it made `defaultSleep` a "parameter" of the function
            // that reads it, and the resolver refused its only reference.
            if field == Some("right")
                && matches!(
                    node.kind(),
                    "assignment_pattern" | "object_assignment_pattern"
                )
            {
                continue;
            }
            worklist.push(child);
        }
    }
}
/// The **type parameters** the callable declares on itself: `T` and `E` in
/// `fn read<T, E>(…)`, `T` in `func Map[T any](…)`, `K` in
/// `function pick<K extends string>(…)`.
///
/// X40. A type parameter is a name the enclosing item binds in its own
/// signature, which is precisely what `UnresolvedClass::LocalBinding` is
/// defined as — "a bare call to a name the enclosing symbol itself declares".
/// Before this, a use of `T` in the body or the parameter list matched no
/// indexed symbol and landed in the tier documented as the one that indicates a
/// defect, which is not what a generic parameter is.
///
/// Read through the grammar's `type_parameters` node rather than from a naming
/// convention: `T`-shaped single letters are the *style*, not the rule, and a
/// parameter called `Item` is no less bound by the signature that declares it.
/// Every grammar this crate links spells the list `type_parameters`; a language
/// whose grammar does not simply contributes nothing here, which leaves its
/// generics exactly where they are today rather than guessing.
pub(super) fn collect_type_parameter_names(
    callable: Node,
    source: &str,
    out: &mut BTreeSet<String>,
) {
    let mut cursor = callable.walk();
    let Some(params) = callable
        .children(&mut cursor)
        .find(|child| child.kind() == "type_parameters")
    else {
        return;
    };
    let mut worklist = vec![params];
    while let Some(node) = worklist.pop() {
        // The declared name is the *first* identifier of each entry; a bound
        // (`T: Display`, `T any`) is a use of another type and must not be
        // recorded as though this signature declared it.
        if matches!(node.kind(), "type_parameter" | "constrained_type_parameter") {
            if let Some(name) = node
                .child_by_field_name("name")
                .or_else(|| node.named_child(0))
                .map(|child| get_node_text(child, source))
                .filter(|name| is_user_ident(name))
            {
                out.insert(name);
            }
            continue;
        }
        if matches!(node.kind(), "type_identifier" | "identifier")
            && bounded_parent(node).is_some_and(|parent| parent.kind() == "type_parameters")
        {
            let name = get_node_text(node, source);
            if is_user_ident(&name) {
                out.insert(name);
            }
            continue;
        }
        let mut children = node.walk();
        for child in node.named_children(&mut children) {
            worklist.push(child);
        }
    }
}

/// Every name a JS/TS file's top-level `const`/`let`/`var` declarations bind,
/// destructuring included, whether or not the declaration is exported.
pub(super) fn js_module_scope_names(root: Node, source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut cursor = root.walk();
    for statement in root.named_children(&mut cursor) {
        let declaration = match statement.kind() {
            "export_statement" => statement.child_by_field_name("declaration"),
            _ => Some(statement),
        };
        let Some(declaration) = declaration.filter(|declaration| {
            matches!(
                declaration.kind(),
                "lexical_declaration" | "variable_declaration"
            )
        }) else {
            continue;
        };
        let mut inner = declaration.walk();
        for declarator in declaration.named_children(&mut inner) {
            if declarator.kind() != "variable_declarator" {
                continue;
            }
            if let Some(target) = declarator.child_by_field_name("name") {
                names.extend(js_pattern_bound_names(target, source));
            }
        }
    }
    names
}

pub(super) fn collect_scope_locals(
    root: Node,
    source: &str,
    file_symbol_name: &str,
    lang: &str,
) -> Vec<(String, String)> {
    let mut by_scope: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut worklist = vec![root];
    while let Some(node) = worklist.pop() {
        // A charged statement declares no scope of its own: its locals and its
        // lambdas' belong to nothing the file declares.
        if in_charged_region(node) {
            continue;
        }
        if is_callable_node(node) {
            // Unnamed *and* `let`-bound closures must not dump their parameters
            // into the enclosing function's `scope_locals`. That set is what
            // `classify_unresolved` reads for `LocalBinding`, and merging
            // `|marker|` or `|name|` into `run` made a later `marker("x")` /
            // `name.len()` look like a callback of `run`. The `let` binding
            // itself (`handler`) is still a local of `run` via the body walk;
            // only the closure's *parameters* stay out.
            //
            // Previously `let`-bound closures were merged so their parameters
            // shared the caller's scope key. That contradicted
            // `a_rust_closure_parameter_does_not_hide_an_outer_function` and
            // `scope_locals_are_keyed_by_the_identity_calls_report_as_their_caller`.
            let skip_merge = node.kind() == "closure_expression";
            if !skip_merge {
                if let Some(scope) = node
                    .child(0)
                    .and_then(|child| enclosing_callable_qualified(child, source, file_symbol_name))
                {
                    let entry = by_scope.entry(scope).or_default();
                    with_scope_locals(node, source, |locals| {
                        entry.extend(locals.iter().cloned());
                    });
                    // `collect_non_symbol_locals` recognises a parameter only when
                    // the grammar wraps it in a node carrying `name` or `pattern`,
                    // so `def f(a)`, `def f(a: T)`, `function f(a)` and `|handler|`
                    // all fell through. The same declaration counted as a binding or
                    // not depending on whether the author wrote a default or a type,
                    // which is why `next_gap_id: Callable[…]` and `cls` were the two
                    // largest remaining unattributed callees on this repository.
                    collect_parameter_names(node, source, entry);
                    // X40. A type parameter is bound by this signature exactly as a
                    // value parameter is, and the resolver reads both from the same
                    // per-scope set.
                    collect_type_parameter_names(node, source, entry);
                    if lang == "swift" {
                        // A dotted key (`open.transform`) types a member path, not a name.
                        entry.extend(
                            crate::swiftscope::bindings(node, source)
                                .keys()
                                .filter(|name| !name.contains('.') && !name.starts_with('\u{1}'))
                                .cloned(),
                        );
                    }
                }
            }
        }
        push_children(node, &mut worklist);
    }
    // A JS/TS module-scope `const x = …` is a binding of the file itself, keyed
    // by the file's own name — the caller a module-level call reports. Only the
    // exported ones become symbols, so without this a private
    // `const fetchThing = vi.fn()` was invisible and `fetchThing()` bound to
    // whichever other file exports that name.
    if matches!(lang, "javascript" | "typescript" | "tsx") {
        let module_names = js_module_scope_names(root, source);
        if !module_names.is_empty() {
            by_scope
                .entry(file_symbol_name.to_string())
                .or_default()
                .extend(module_names);
        }
    }
    // A Python module-scope name bound by a `for`/`with`/`except` target, a
    // walrus, a `del` or a `global` declaration is a binding of the file itself,
    // keyed by the file's own name as the JS/TS ones are. None of them is a
    // symbol, so without this a name an import brought in could be rebound
    // beneath the resolver without a trace.
    if lang == "python" {
        let facts = python_file_facts(root, source);
        if !facts.rebound.is_empty() {
            by_scope
                .entry(file_symbol_name.to_string())
                .or_default()
                .extend(facts.rebound.iter().cloned());
        }
        // What `from this_module import *` takes, when `__all__` says: filed
        // under a scope no symbol can have (`{file}::__all__`), so a reader that
        // asks about a real scope never sees it.
        let all = python_all_exports(source);
        if !all.is_empty() {
            by_scope
                .entry(format!("{file_symbol_name}::__all__"))
                .or_default()
                .extend(all);
        }
    }
    by_scope
        .into_iter()
        .flat_map(|(scope, locals)| locals.into_iter().map(move |local| (scope.clone(), local)))
        .collect()
}

pub(super) fn is_inside_import_or_export(node: Node) -> bool {
    let mut current = bounded_parent(node);
    while let Some(parent) = current {
        if matches!(
            parent.kind(),
            "import_specifier"
                | "export_specifier"
                | "import_spec"
                | "export_clause"
                | "named_imports"
                | "namespace_import"
                | "import_clause"
                | "use_declaration"
                | "use_list"
        ) {
            return true;
        }
        // A declaration body is a use site. Stop before `export function`
        // / `export const` would otherwise hide every identifier in the
        // exported item.
        if matches!(
            parent.kind(),
            "statement_block"
                | "class_body"
                | "function_body"
                | "block"
                | "compound_statement"
                | "function_declaration"
                | "function_definition"
                | "arrow_function"
                | "function_expression"
                | "method_definition"
                | "class_declaration"
                | "lexical_declaration"
                | "variable_declaration"
        ) {
            return false;
        }
        if matches!(
            parent.kind(),
            "import_statement" | "import_declaration" | "import_from_statement"
        ) {
            return true;
        }
        current = bounded_parent(parent);
    }
    false
}

pub(super) fn is_call_callee(node: Node) -> bool {
    let Some(parent) = bounded_parent(node) else {
        return false;
    };
    // `f<T>(x)`: the call's `function` field is the wrapper, and the name
    // inside it is the callee. Without this the name was also emitted as a
    // `Name` reference beside the `Call` one — two references for one site.
    let (node, parent) = if is_template_wrapper_name(parent, node) {
        match bounded_parent(parent) {
            Some(grand) => (parent, grand),
            None => return false,
        }
    } else {
        (node, parent)
    };
    // An Objective-C keyword message has one `method` field per selector part,
    // so the single-field lookup below would only ever recognise the first.
    // Each part is the callee's name, never a use of some same-named symbol.
    if parent.kind() == "message_expression" {
        let mut cursor = parent.walk();
        if cursor.goto_first_child() {
            loop {
                if cursor.field_name() == Some("method") && cursor.node().id() == node.id() {
                    return true;
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }
    if matches!(
        parent.kind(),
        "call" | "call_expression" | "new_expression" | "composite_literal"
    ) {
        let callee = parent
            .child_by_field_name("function")
            .or_else(|| parent.child_by_field_name("constructor"))
            .or_else(|| parent.child_by_field_name("type"));
        if let Some(callee) = callee {
            // A walk up from `node` used to sit here, looking for the callee
            // among its ancestors. It was dead by construction: this branch is
            // only entered when `bounded_parent(node)` *is* the call, so the first
            // ancestor examined is always that same parent and the walk broke
            // immediately. Instrumented over 4,421 files in five languages it
            // never once reached the callee or iterated twice.
            if callee.id() == node.id() {
                return true;
            }
        }
    }
    // Kotlin: the member half of `a.b(..)`. The call record already carries it,
    // and the walker's own `Name` reference for it was a second row that
    // resolved on the name alone — `super.go()` inside `Child.go` was read as a
    // use of `Child.go` itself.
    if parent.kind() == "navigation_expression"
        && parent
            .named_child(parent.named_child_count().saturating_sub(1))
            .is_some_and(|member| member.id() == node.id())
        && parent.named_child_count() >= 2
        && bounded_parent(parent).is_some_and(|grand| {
            grand.kind() == "call_expression"
                && grand
                    .named_child(0)
                    .is_some_and(|callee| callee.id() == parent.id())
        })
    {
        return true;
    }
    if matches!(
        parent.kind(),
        "selector_expression" | "member_expression" | "attribute"
    ) {
        // Only the *member* half is the callee. `console.print(…)` names two
        // things: the method, which the call record already carries, and the
        // receiver, which is an ordinary use of a different symbol and the only
        // evidence that symbol is alive.
        //
        // Refusing both made every method receiver invisible to the graph. A
        // module-level singleton used the way singletons are used —
        // `console = _common.console` at the top of a file and `console.print`
        // in every function below — had no inbound edge at all and was reported
        // dead at 0.9 in eleven files of this repository at once.
        let is_member_half = parent
            .child_by_field_name("attribute")
            .or_else(|| parent.child_by_field_name("property"))
            .or_else(|| parent.child_by_field_name("field"))
            .is_some_and(|member| member.id() == node.id());
        if is_member_half {
            if let Some(grand) = bounded_parent(parent) {
                // The `function` check is redundant in every linked grammar —
                // arguments are wrapped in `arguments`/`argument_list`, so a
                // member expression that is a direct child of a call is always
                // its callee, measured zero counterexamples over the same
                // corpus. It is kept as a guard: without it, a grammar that
                // nests arguments differently would silently classify an
                // argument as the callee.
                if matches!(grand.kind(), "call" | "call_expression")
                    && grand
                        .child_by_field_name("function")
                        .is_some_and(|function| function.id() == parent.id())
                {
                    return true;
                }
            }
        }
    }
    false
}

/// The field a Python `self.name = callee(..)` assigns, spelled `self.name`,
/// when `origin` is the callee of the call that is the assignment's whole
/// right-hand side.
///
/// Spelled with the `self.` prefix so it can never equal a local's name: the
/// receiver maps key a local by its bare name, and a field is not in scope as
/// a variable (Swift's `swift_assignment_binding` refuses the same spelling for
/// that reason). The resolver reads the prefix and routes the initializer to
/// the owning class's field table instead.
///
/// Only the direct right-hand side counts. `self.xs = [Engine()]` holds a list,
/// `self.e = wrap(Engine())` holds what `wrap` returns, and
/// `self.e = Engine() if c else None` is not one type: binding any of them to
/// the inner call would type the field wrongly, and nothing here can tell a
/// wrapper from the value.
pub(super) fn python_self_field_binding(
    assignment: Node,
    origin: Node,
    source: &str,
) -> Option<String> {
    let left = assignment.child_by_field_name("left")?;
    if left.kind() != "attribute" {
        return None;
    }
    let object = left.child_by_field_name("object")?;
    let attribute = left.child_by_field_name("attribute")?;
    if object.kind() != "identifier"
        || attribute.kind() != "identifier"
        || get_node_text(object, source) != "self"
    {
        return None;
    }
    let right = assignment.child_by_field_name("right")?;
    if right.kind() != "call" {
        return None;
    }
    // The callee must be a name (`Engine`, `mod.make`): `A()()`, `A().build()`,
    // `(lambda: A())()` and `fns[0]()` return what some other value returns,
    // and the inner call is not that value.
    let function = right.child_by_field_name("function")?;
    if !python_is_dotted_name(function) || !python_self_names_an_instance(assignment, source) {
        return None;
    }
    // The call itself, or its callee: an argument lies past the callee's end.
    (origin.id() == right.id()
        || (origin.start_byte() >= right.start_byte() && origin.end_byte() <= function.end_byte()))
    .then(|| format!("self.{}", get_node_text(attribute, source)))
}

/// `name` or `a.b.c`, with nothing but identifiers in it.
pub(crate) fn python_is_dotted_name(node: Node) -> bool {
    match node.kind() {
        "identifier" => true,
        "attribute" => {
            node.child_by_field_name("attribute")
                .is_some_and(|attribute| attribute.kind() == "identifier")
                && node
                    .child_by_field_name("object")
                    .is_some_and(python_is_dotted_name)
        }
        _ => false,
    }
}

/// The name a Python parameter binds: `x`, `x: T`, `x=1`, `x: T = 1`.
pub(super) fn python_parameter_name<'a>(parameter: Node<'a>, source: &str) -> Option<String> {
    match parameter.kind() {
        "identifier" => Some(get_node_text(parameter, source)),
        "typed_parameter" => parameter
            .named_child(0)
            .filter(|name| name.kind() == "identifier")
            .map(|name| get_node_text(name, source)),
        "default_parameter" | "typed_default_parameter" => parameter
            .child_by_field_name("name")
            .filter(|name| name.kind() == "identifier")
            .map(|name| get_node_text(name, source)),
        _ => None,
    }
}

/// The root of the tree `node` is in.
pub(super) fn python_tree_root(node: Node) -> Node {
    let mut root = node;
    while let Some(parent) = bounded_parent(root) {
        root = parent;
    }
    root
}

/// Whether `object`, the left side of `X.staticmethod`, read at byte `at`, is
/// the `builtins` module: the name `builtins` or a module-scope `import builtins
/// as X`, neither rebound before `at`, or the call `__import__('builtins')`.
pub(super) fn python_names_the_builtins_module(
    object: Node,
    at: usize,
    facts: &PythonFileFacts,
    source: &str,
) -> bool {
    match object.kind() {
        "identifier" => facts.names_builtins_module(&get_node_text(object, source), at),
        "call" => {
            let named_import = object
                .child_by_field_name("function")
                .is_some_and(|function| get_node_text(function, source) == "__import__");
            let mut cursor = object.walk();
            let argument = object
                .child_by_field_name("arguments")
                .and_then(|arguments| arguments.named_children(&mut cursor).next());
            named_import
                && argument.is_some_and(|argument| {
                    argument.kind() == "string"
                        && get_node_text(argument, source).trim_matches(['"', '\'']) == "builtins"
                })
        }
        _ => false,
    }
}

/// Whether a decorator makes its method take no instance: its expression is the
/// builtin `staticmethod` or `classmethod`.
///
/// A bare name is the builtin unless the file binds it textually before the
/// decorator (an import, a `def`, an assignment, a `for` target ...), or it is
/// the member `from builtins import staticmethod [as sm]` bound. An attribute
/// `X.staticmethod` is the builtin only when `X` is the `builtins` module
/// ([`python_names_the_builtins_module`]): `deco.staticmethod` is some other
/// object's attribute that merely shares the spelling. Any other expression
/// leaves the method an instance method.
///
/// Read from the expression node, not the decorator's text: the text carries a
/// trailing comment (`@classmethod  # build`), which is no part of the
/// expression. The module's names come from [`python_file_facts`], read once
/// per file.
pub(super) fn python_decorator_drops_instance(decorator: Node, source: &str) -> bool {
    let mut cursor = decorator.walk();
    let expression = decorator
        .named_children(&mut cursor)
        .find(|child| child.kind() != "comment");
    let is_builtin_name = |name: &str| matches!(name, "staticmethod" | "classmethod");
    let Some(expression) = expression.filter(|e| matches!(e.kind(), "identifier" | "attribute"))
    else {
        return false;
    };
    let facts = python_file_facts(python_tree_root(decorator), source);
    let at = decorator.start_byte();
    match expression.kind() {
        "identifier" => facts
            .builtin_for_bare_name(&get_node_text(expression, source), at)
            .is_some_and(|name| is_builtin_name(&name)),
        _ => {
            expression
                .child_by_field_name("attribute")
                .is_some_and(|name| is_builtin_name(&get_node_text(name, source)))
                && expression
                    .child_by_field_name("object")
                    .is_some_and(|object| {
                        python_names_the_builtins_module(object, at, &facts, source)
                    })
        }
    }
}

/// Whether the `self` an assignment names is the instance of a class: the
/// outermost function around the assignment is an instance method of a class
/// (its first parameter is `self`, and it is neither a `@staticmethod` nor a
/// `@classmethod`), and no function nested between rebinds `self` as its own
/// parameter. A closure inside the method still writes the method's `self`.
pub(super) fn python_self_names_an_instance(assignment: Node, source: &str) -> bool {
    let mut method: Option<Node> = None;
    let mut current = assignment;
    for _ in 0..64 {
        let Some(parent) = bounded_parent(current) else {
            return false;
        };
        match parent.kind() {
            "function_definition" => {
                if let Some(inner) = method {
                    // A nested function that takes its own `self` shadows it.
                    let rebinds = inner
                        .child_by_field_name("parameters")
                        .into_iter()
                        .flat_map(|parameters| {
                            let mut cursor = parameters.walk();
                            parameters
                                .named_children(&mut cursor)
                                .filter_map(|p| python_parameter_name(p, source))
                                .collect::<Vec<_>>()
                        })
                        .any(|name| name == "self");
                    if rebinds {
                        return false;
                    }
                }
                method = Some(parent);
            }
            "lambda" => {}
            "class_definition" => {
                let Some(method) = method else {
                    return false;
                };
                let first = method
                    .child_by_field_name("parameters")
                    .and_then(|parameters| parameters.named_child(0))
                    .and_then(|first| python_parameter_name(first, source));
                if first.as_deref() != Some("self") {
                    return false;
                }
                let decorated = bounded_parent(method)
                    .filter(|wrapper| wrapper.kind() == "decorated_definition");
                return !decorated.is_some_and(|wrapper| {
                    let mut cursor = wrapper.walk();
                    let mut decorators = wrapper.named_children(&mut cursor);
                    decorators.any(|decorator| {
                        decorator.kind() == "decorator"
                            && python_decorator_drops_instance(decorator, source)
                    })
                });
            }
            _ => {}
        }
        current = parent;
    }
    false
}

pub(super) fn assignment_binding(mut node: Node, source: &str) -> Option<String> {
    let origin = node;
    for _ in 0..4 {
        node = bounded_parent(node)?;
        if node.kind() == "assignment" {
            if let Some(field) = python_self_field_binding(node, origin, source) {
                return Some(field);
            }
        }
        let binding = match node.kind() {
            "assignment"
            | "assignment_expression"
            | "augmented_assignment"
            | "short_var_declaration"
            | "assignment_statement" => node
                .child_by_field_name("left")
                .or_else(|| node.child_by_field_name("name")),
            "variable_declarator" => node.child_by_field_name("name"),
            // Go `var x = New()` / `const x = …`, at package level or in a
            // function. `name` repeats for `var a, b = …`, which binds two
            // names to two values; nothing here says which value is whose, so
            // only the single-name spec binds.
            "var_spec" | "const_spec" => {
                let mut cursor = node.walk();
                let mut names = node.children_by_field_name("name", &mut cursor);
                match (names.next(), names.next()) {
                    (Some(only), None) => Some(only),
                    _ => return None,
                }
            }
            "let_declaration" => node
                .child_by_field_name("pattern")
                .or_else(|| node.child_by_field_name("name")),
            _ => None,
        };
        if let Some(binding) = binding {
            return simple_binding_name(binding, source);
        }
        if matches!(
            node.kind(),
            "expression_statement"
                | "return_statement"
                | "function_definition"
                | "function_declaration"
                | "method_definition"
        ) {
            break;
        }
    }
    None
}

/// The single local name a binding node introduces, if it introduces exactly one.
///
/// Go wraps a `:=` target in an `expression_list` even when there is only one
/// target, so `worker := NewWorker()` reached this as a list rather than an
/// identifier and bound nothing — every Go value constructed the idiomatic way
/// had a receiver with no type behind it.
///
/// A list with more than one target is deliberately *not* bound: in
/// `value, err := New()` nothing here says which target receives the
/// constructed value, and a guess would bind a real name to the wrong type,
/// which is worse than leaving it unresolved.
pub(super) fn simple_binding_name(node: Node, source: &str) -> Option<String> {
    if matches!(node.kind(), "expression_list" | "pattern_list") {
        if node.named_child_count() != 1 {
            return None;
        }
        return node
            .named_child(0)
            .and_then(|only| simple_binding_name(only, source));
    }
    if matches!(
        node.kind(),
        "identifier" | "variable_name" | "shorthand_property_identifier_pattern"
    ) {
        let name = get_node_text(node, source);
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

/// The parameter a Swift type annotation types, when `node` is that type.
///
/// tree-sitter-swift puts the parameter name *and* its type on the `name`
/// field, so a Type reference for `Reader` in `func load(_ reader: Reader)`
/// used to carry no `assigned_to`. Without that binding, `reader.read()`
/// cannot dispatch on `Reader` and falls to AmbiguousGlobal the moment a
/// second type also declares `read` — the MarkDev save/highlight shape.
pub(super) fn swift_parameter_bound_from_type(node: Node, source: &str) -> Option<String> {
    let mut current = node;
    for _ in 0..8 {
        let parent = bounded_parent(current)?;
        if parent.kind() == "parameter" {
            let mut cursor = parent.walk();
            if !cursor.goto_first_child() {
                return None;
            }
            loop {
                if cursor.field_name() == Some("name")
                    && cursor.node().kind() == "simple_identifier"
                {
                    let name = get_node_text(cursor.node(), source);
                    if is_user_ident(&name) && name != "_" {
                        return Some(name);
                    }
                }
                if !cursor.goto_next_sibling() {
                    return None;
                }
            }
        }
        if matches!(
            parent.kind(),
            "function_declaration" | "function_body" | "statements" | "class_declaration"
        ) {
            return None;
        }
        current = parent;
    }
    None
}

/// The parameter a Python annotation types: `def summarise(shape: Shape)`.
///
/// The annotation was already read as a Type reference — that is how
/// `summarise -> Shape` existed — but nothing bound it to `shape`, so
/// `shape.describe()` had no receiver type and `Shape.describe` was reported
/// dead with its caller in the same signature. Only an annotation that *is*
/// the type counts — the identifier must be the whole `type` node: in
/// `xs: list[Shape]` or `s: Shape | None` the name is not what the parameter
/// holds, so nothing is bound.
pub(super) fn python_parameter_bound_from_type(node: Node, source: &str) -> Option<String> {
    let annotation = bounded_parent(node).filter(|parent| parent.kind() == "type")?;
    let parameter = bounded_parent(annotation)
        .filter(|parent| matches!(parent.kind(), "typed_parameter" | "typed_default_parameter"))?;
    if !parameter
        .child_by_field_name("type")
        .is_some_and(|ty| ty.id() == annotation.id())
    {
        return None;
    }
    // `typed_default_parameter` names its binding; `typed_parameter` holds it
    // as its first child, which is a `list_splat_pattern` for `*args: T`.
    let binding = parameter
        .child_by_field_name("name")
        .or_else(|| parameter.named_child(0))
        .filter(|binding| binding.kind() == "identifier")?;
    let name = get_node_text(binding, source);
    (is_user_ident(&name) && name != "self" && name != "cls").then_some(name)
}

/// The variable a Go `var w T` declares, when `node` is that type.
///
/// The Go spelling of a typed local with no initializer, and the idiomatic
/// one for a value decoded in place: `var w requirementWire;
/// json.Unmarshal(data, &w); w.Priority.valid()`. A parameter's type was
/// already bound to its name; this declaration was not, so `w` had no type and
/// every method reached through it — the `valid` checks on each decoded field
/// — was reported dead.
///
/// Only a pointer or package qualifier may stand between the type and the
/// spec: `var xs []T` and `var m map[K]T` do not make `xs` a `T`. A spec
/// declaring several names (`var a, b T`) types each of them identically,
/// but which one this reference serves is not knowable from one
/// `assigned_to`, so it binds none.
pub(super) fn go_var_bound_from_type(node: Node, source: &str) -> Option<String> {
    let mut current = node;
    for _ in 0..4 {
        let parent = bounded_parent(current)?;
        match parent.kind() {
            "pointer_type" | "qualified_type" => current = parent,
            "var_spec" => {
                if !parent
                    .child_by_field_name("type")
                    .is_some_and(|ty| ty.id() == current.id())
                {
                    return None;
                }
                let mut cursor = parent.walk();
                let mut names = parent
                    .children_by_field_name("name", &mut cursor)
                    .filter(|name| name.kind() == "identifier");
                let only = names.next()?;
                if names.next().is_some() {
                    return None;
                }
                let name = get_node_text(only, source);
                return (is_user_ident(&name) && name != "_").then_some(name);
            }
            _ => return None,
        }
    }
    None
}

/// The local a Rust `let` binds, when `node` is a `field_expression` used as
/// the let's value — including through `&`, `if`/`else`, and a block.
pub(super) fn rust_let_bound_from_field_use(node: Node, source: &str) -> Option<String> {
    let parent = bounded_parent(node)?;
    if parent.kind() != "field_expression" {
        return None;
    }
    let member = parent.child_by_field_name("field")?;
    if member.id() != node.id() {
        return None;
    }
    rust_let_bound_from_value(parent, source)
}

/// The local a Rust `let x: T` binds, when `node` is that type annotation.
pub(super) fn rust_let_bound_from_type_annotation(node: Node, source: &str) -> Option<String> {
    let mut current = node;
    for _ in 0..8 {
        let parent = bounded_parent(current)?;
        if parent.kind() == "let_declaration" {
            if field_contains(parent, "type", node) {
                return parent
                    .child_by_field_name("pattern")
                    .and_then(|pattern| simple_binding_name(pattern, source));
            }
            return None;
        }
        if matches!(
            parent.kind(),
            "function_item" | "closure_expression" | "block" | "source_file"
        ) {
            return None;
        }
        current = parent;
    }
    None
}

/// The parameter a TypeScript/JavaScript type annotation types.
pub(super) fn ts_parameter_bound_from_type(node: Node, source: &str) -> Option<String> {
    let mut current = node;
    for _ in 0..8 {
        let parent = bounded_parent(current)?;
        if matches!(
            parent.kind(),
            "required_parameter" | "optional_parameter" | "typed_parameter"
        ) {
            return parent
                .child_by_field_name("pattern")
                .or_else(|| parent.child_by_field_name("name"))
                .and_then(|pattern| {
                    if pattern.kind() == "identifier" {
                        let name = get_node_text(pattern, source);
                        is_user_ident(&name).then_some(name)
                    } else {
                        simple_binding_name(pattern, source)
                    }
                });
        }
        if matches!(
            parent.kind(),
            "function_declaration"
                | "function_expression"
                | "arrow_function"
                | "method_definition"
                | "generator_function_declaration"
                | "statement_block"
                | "program"
        ) {
            return None;
        }
        current = parent;
    }
    None
}

/// Whether a Python identifier sits in a type annotation (`x: T`, `-> T`).
pub(super) fn python_identifier_is_type_annotation(node: Node) -> bool {
    let mut current = node;
    for _ in 0..8 {
        let Some(parent) = bounded_parent(current) else {
            return false;
        };
        if field_contains(parent, "type", node) || field_contains(parent, "return_type", node) {
            return true;
        }
        if is_callable_node(parent) || matches!(parent.kind(), "module" | "block") {
            return false;
        }
        current = parent;
    }
    false
}

/// Whether a Python identifier is a type argument of `isinstance` / `issubclass`.
///
/// The first argument is the value; every later positional argument (and each
/// element of a tuple of types) names a class. Those are Type uses, not Name.
pub(super) fn python_isinstance_type_argument(node: Node, source: &str) -> bool {
    let mut current = node;
    for _ in 0..8 {
        let Some(parent) = bounded_parent(current) else {
            return false;
        };
        if parent.kind() == "tuple" {
            current = parent;
            continue;
        }
        if parent.kind() == "argument_list" {
            let Some(call) = bounded_parent(parent) else {
                return false;
            };
            if call.kind() != "call" {
                return false;
            }
            let Some(function) = call.child_by_field_name("function") else {
                return false;
            };
            let callee = get_node_text(function, source);
            if callee != "isinstance" && callee != "issubclass" {
                return false;
            }
            // Skip the value (first positional). Everything after is a type.
            let mut cursor = parent.walk();
            let mut index = 0usize;
            for child in parent.named_children(&mut cursor) {
                if child.kind() == "keyword_argument" {
                    continue;
                }
                if index == 0 {
                    index += 1;
                    continue;
                }
                if node_contains(child, node) {
                    return true;
                }
                index += 1;
            }
            return false;
        }
        if matches!(
            parent.kind(),
            "call" | "function_definition" | "module" | "block"
        ) {
            return false;
        }
        current = parent;
    }
    false
}

pub(super) fn rust_let_bound_from_value(mut node: Node, source: &str) -> Option<String> {
    for _ in 0..12 {
        let parent = bounded_parent(node)?;
        if matches!(
            parent.kind(),
            "call_expression" | "function_item" | "closure_expression"
        ) {
            return None;
        }
        if parent.kind() == "let_declaration" {
            return parent
                .child_by_field_name("pattern")
                .and_then(|pattern| simple_binding_name(pattern, source));
        }
        if matches!(
            parent.kind(),
            "reference_expression"
                | "unary_expression"
                | "try_expression"
                | "parenthesized_expression"
                | "await_expression"
                | "block"
                | "expression_statement"
                | "if_expression"
                | "else_clause"
                | "match_expression"
                | "match_arm"
                | "match_block"
        ) {
            node = parent;
            continue;
        }
        return None;
    }
    None
}

/// Whether a use sits in the initializer of a Rust `let` / `if let` that
/// binds the same name — a position where that binding is not yet in scope.
pub(super) fn rust_use_is_in_same_name_initializer(node: Node, source: &str, name: &str) -> bool {
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        // A closure is part of the initializer it is written in, so a *call* of
        // the name walks through it: `let (n, f) = (1, opt.and_then(|p| f(p)))`
        // calls the outer item `f` from inside the closure. A closure that
        // binds the name itself (`|f| f()`) shadows it, and a name used as a
        // value or receiver (`let w = spawn(move || w.run())`) is an earlier
        // *local* the closure captured, not an item, so it still stops here.
        if parent.kind() == "closure_expression" {
            let shadowed = parent
                .child_by_field_name("parameters")
                .is_some_and(|parameters| rust_pattern_binds(parameters, source, name));
            // The use is either the call node or its callee identifier,
            // depending on which span the extractor recorded.
            let callee_is_name = |call: Node| {
                call.kind() == "call_expression"
                    && call
                        .child_by_field_name("function")
                        .is_some_and(|function| {
                            function.kind() == "identifier"
                                && get_node_text(function, source) == name
                        })
            };
            let called = callee_is_name(node)
                || (node.kind() == "identifier"
                    && get_node_text(node, source) == name
                    && bounded_parent(node).is_some_and(callee_is_name));
            if shadowed || !called {
                return false;
            }
        } else if is_callable_node(parent) {
            return false;
        }
        if matches!(parent.kind(), "let_declaration" | "let_condition") {
            let in_value = field_contains(parent, "value", node);
            let binds = parent
                .child_by_field_name("pattern")
                .is_some_and(|pattern| rust_pattern_binds(pattern, source, name));
            return in_value && binds;
        }
        ancestor = bounded_parent(parent);
    }
    false
}

/// Whether a use sits in the initializer of a Kotlin `val` / `var` that
/// declares the same name — a position where that local is not yet in scope.
pub(super) fn kotlin_use_is_in_same_name_initializer(node: Node, source: &str, name: &str) -> bool {
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        if is_callable_node(parent) {
            return false;
        }
        if parent.kind() == "property_declaration" {
            let declares = parent
                .named_children(&mut parent.walk())
                .find(|child| child.kind() == "variable_declaration")
                .and_then(|declaration| declaration.named_child(0))
                .is_some_and(|declared| get_node_text(declared, source) == name);
            // A use inside the declaration's own name or type is not an
            // initializer use; only what follows the `variable_declaration` is.
            let in_declaration = parent
                .named_children(&mut parent.walk())
                .find(|child| child.kind() == "variable_declaration")
                .is_some_and(|declaration| {
                    declaration.start_byte() <= node.start_byte()
                        && node.end_byte() <= declaration.end_byte()
                });
            return declares && !in_declaration;
        }
        ancestor = bounded_parent(parent);
    }
    false
}

pub(super) fn rust_pattern_binds(pattern: Node, source: &str, name: &str) -> bool {
    let mut worklist = vec![pattern];
    while let Some(node) = worklist.pop() {
        if node.kind() == "identifier" && get_node_text(node, source) == name {
            let constructor =
                bounded_parent(node).is_some_and(|parent| field_contains(parent, "type", node));
            if !constructor {
                return true;
            }
        }
        push_named_children(node, &mut worklist);
    }
    false
}

pub(crate) fn get_child_text(node: Node, field: &str, source: &str) -> Option<String> {
    node.child_by_field_name(field)
        .map(|n| get_node_text(n, source))
}

pub(crate) fn get_node_text(node: Node, source: &str) -> String {
    let start = node.start_byte();
    let end = node.end_byte();
    if start <= end && end <= source.len() {
        source[start..end].to_string()
    } else {
        String::new()
    }
}

pub(crate) fn node_span(node: Node) -> Span {
    Span {
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
    }
}
