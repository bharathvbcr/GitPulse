use super::*;

// Keep the node handler pure with respect to traversal state; the explicit
// accumulators make ownership and mutation sites visible to the caller.
#[allow(clippy::too_many_arguments)]
pub(super) fn extract_node(
    node: Node,
    source: &str,
    lang: &str,
    file_symbol_name: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    calls: &mut Vec<ExtractedCall>,
    exports: &mut Vec<ExtractedExport>,
    references: &mut Vec<ExtractedReference>,
    wiring: &mut Vec<WiringAnnotation>,
) {
    let kind = node.kind();
    let span = node_span(node);

    match lang {
        "python" => match kind {
            // A module-scope binding is a module attribute, importable by name,
            // and so a symbol. `is_exported` is decided after the walk, from
            // `__all__`, once the whole file has been seen.
            "assignment" if python_is_module_scope(node) => {
                if let Some(target) = node.child_by_field_name("left") {
                    for name in python_bound_names(target, source) {
                        if name != "__all__" {
                            symbols.push(module_binding_symbol(
                                file_symbol_name,
                                name,
                                false,
                                span.clone(),
                            ));
                        }
                    }
                }
            }
            // PEP 695 `type Vec = list[float]` / `type Pair[T] = tuple[T, T]`:
            // a named type, so a type-like kind that `Type` references bind to.
            // `Interface` is what a TypeScript `type` alias already is.
            "type_alias_statement" if python_is_module_scope(node) => {
                let named = node.child_by_field_name("left").and_then(|left| {
                    let mut current = left;
                    for _ in 0..3 {
                        match current.kind() {
                            "identifier" => return Some(current),
                            "type" | "generic_type" => current = current.named_child(0)?,
                            _ => return None,
                        }
                    }
                    None
                });
                if let Some(name) = named.map(|name| get_node_text(name, source)) {
                    if !name.is_empty() {
                        symbols.push(ExtractedSymbol {
                            qualified_name: format!("{file_symbol_name}::{name}"),
                            name,
                            kind: SymbolKind::Interface,
                            span,
                            is_exported: false,
                            docstring: None,
                            signature: None,
                            parent_symbol: Some(file_symbol_name.to_string()),
                            body_signature: None,
                            declaration_hash: None,
                            return_type: None,
                        });
                    }
                }
            }
            "function_definition" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    let enclosing_type = enclosing_type_name(node, source);
                    let is_method = enclosing_type.is_some();
                    let parent_symbol = enclosing_type
                        .as_ref()
                        .map(|type_name| format!("{}::{}", file_symbol_name, type_name))
                        .or_else(|| enclosing_callable_qualified(node, source, file_symbol_name))
                        .unwrap_or_else(|| file_symbol_name.to_string());
                    let qualified_name = enclosing_type
                        .map(|type_name| format!("{}::{}.{}", file_symbol_name, type_name, n))
                        .unwrap_or_else(|| {
                            scoped_qualified_name(node, source, file_symbol_name, &n)
                        });
                    if let Some(reason) = crate::wiring::python_harness_entry_reason(&n) {
                        wiring.push(WiringAnnotation {
                            kind: WiringKind::RuntimeEntryPoint,
                            target_symbol: qualified_name.clone(),
                            details: reason.to_string(),
                        });
                    }
                    // A dispatch contract inherited from the class's base —
                    // `torch.autograd.Function.forward` and friends. Gated on
                    // `is_method` because these are method contracts: a
                    // module-level `def forward` is an ordinary function and
                    // shares nothing with them but a name.
                    if is_method {
                        if let Some(reason) = python_enclosing_superclasses(node, source)
                            .iter()
                            .find_map(|base| {
                                let qualified = python_torch_qualified_base(node, base, source)?;
                                crate::wiring::python_base_dispatch_reason(&qualified, &n)
                            })
                        {
                            wiring.push(WiringAnnotation {
                                kind: WiringKind::RuntimeEntryPoint,
                                target_symbol: qualified_name.clone(),
                                details: reason.to_string(),
                            });
                        }
                    }
                    // The structural rule the name tables above cannot
                    // express: `@test` inside `def register(test, …)` hands
                    // the function to a callable the caller injected, and
                    // that callable — not this corpus — decides when it runs.
                    if let Some((decorator, owner)) = python_injected_decorator(node, source) {
                        wiring.push(WiringAnnotation {
                            kind: WiringKind::RuntimeEntryPoint,
                            target_symbol: qualified_name.clone(),
                            details: crate::wiring::python_injected_decorator_reason(
                                &decorator, &owner,
                            ),
                        });
                    }
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name,
                        kind: if is_method {
                            SymbolKind::Method
                        } else {
                            SymbolKind::Function
                        },
                        span,
                        is_exported: false,
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(parent_symbol),
                        body_signature: None,
                        declaration_hash: None,
                        return_type: None,
                    });
                }
            }
            "class_definition" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name: scoped_qualified_name(node, source, file_symbol_name, &n),
                        kind: SymbolKind::Class,
                        span,
                        is_exported: false,
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(file_symbol_name.to_string()),
                        body_signature: None,
                        declaration_hash: None,
                        return_type: None,
                    });
                }
            }
            // One import per module. `import os, sys` binds two modules and
            // `import a.b as c, d` binds `a.b` as `c` and `d` as itself; read as
            // text, the first was one module named `os, sys` and the second
            // aliased `a.b` to `c, d` and lost `d`.
            "import_statement" => {
                let text = get_node_text(node, source);
                let mut cursor = node.walk();
                for target in node.children_by_field_name("name", &mut cursor) {
                    let (module_node, alias) = match target.kind() {
                        "aliased_import" => (
                            target.child_by_field_name("name"),
                            target
                                .child_by_field_name("alias")
                                .map(|alias| get_node_text(alias, source))
                                .filter(|alias| !alias.is_empty()),
                        ),
                        _ => (Some(target), None),
                    };
                    let Some(module_specifier) = module_node
                        .map(|module| python_module_path(module, source))
                        .filter(|module| !module.is_empty())
                    else {
                        continue;
                    };
                    imports.push(ExtractedImport {
                        raw_import: text.clone(),
                        module_specifier,
                        imported_names: Vec::new(),
                        local_names: Vec::new(),
                        alias,
                        span: span.clone(),
                        path_load: None,
                    });
                }
            }
            "import_from_statement" => {
                let module_specifier = node
                    .child_by_field_name("module_name")
                    .map(|module| python_module_path(module, source))
                    .unwrap_or_default();
                let (imported_names, local_names) = python_import_bindings(node, source);
                // `from m import *` names nothing, so it has no `imported_names`
                // (a `*` there once bound a local called `*`); it is marked by
                // the alias `*` instead, as a Go dot-import is by `.`: the
                // marker for "binds whatever this module exports", and still
                // evidence about every bare name the file uses.
                let mut cursor = node.walk();
                let wildcard = node
                    .children(&mut cursor)
                    .any(|child| child.kind() == "wildcard_import");
                imports.push(ExtractedImport {
                    raw_import: get_node_text(node, source),
                    module_specifier,
                    imported_names,
                    local_names,
                    alias: wildcard.then(|| "*".to_string()),
                    span,
                    path_load: None,
                });
            }
            "call" => {
                // A curried call — `app.command(name="apply-patch")(fn)`,
                // `_default_runner(5)(argv, cwd)` — has a `call` in its
                // `function` field and so no callee name of its own.
                // `split_call_target` refuses it rather than recording the
                // inner call's whole source text as a callee name. The inner
                // call is its own `call` node, so `command` and
                // `_default_runner` keep the edges they always had.
                if let Some(f) = node.child_by_field_name("function") {
                    if let Some((callee_name, receiver_expr)) = split_call_target(f, source) {
                        references.push(extracted_reference(
                            f,
                            source,
                            file_symbol_name,
                            callee_name.clone(),
                            ReferenceKind::Call,
                            assignment_binding(node, source),
                        ));
                        calls.push(ExtractedCall {
                            caller_symbol: enclosing_callable_qualified(
                                node,
                                source,
                                file_symbol_name,
                            ),
                            callee_name,
                            receiver_expr,
                            span,
                        });
                    }
                }
            }
            // `with Engine(1) as eng:` calls `Engine.__enter__` with no call
            // syntax anywhere. Recorded as that call, against the class the
            // statement names, so the resolver can bind it (or say it could
            // not). Only a capitalised identifier is read as a class: the
            // result of an arbitrary function (`with open(p):`) has no type
            // this extractor knows, and a row per such statement would bury
            // the ledger in questions nothing can answer.
            "with_item" => {
                let constructed = node
                    .child_by_field_name("value")
                    .map(|value| {
                        if value.kind() == "as_pattern" {
                            value.named_child(0).unwrap_or(value)
                        } else {
                            value
                        }
                    })
                    .filter(|expression| expression.kind() == "call")
                    .and_then(|call| call.child_by_field_name("function"))
                    .filter(|function| function.kind() == "identifier")
                    .map(|function| get_node_text(function, source))
                    .filter(|name| name.chars().next().is_some_and(char::is_uppercase));
                if let Some(class_name) = constructed {
                    calls.push(ExtractedCall {
                        caller_symbol: enclosing_callable_qualified(node, source, file_symbol_name),
                        callee_name: "__enter__".to_string(),
                        receiver_expr: Some(class_name),
                        span,
                    });
                }
            }
            _ => {}
        },
        "javascript" | "typescript" | "tsx" => match kind {
            // `generator_function_declaration` covers `function*` and
            // `async function*`. It is as much a declaration as
            // `function_declaration` — and frequently an exported API — but was
            // omitted here while `callable_binding_name` still named it as a
            // call scope, so its calls pointed at a source symbol no node had
            // (SC10).
            "function_declaration"
            | "generator_function_declaration"
            | "method_definition"
            | "arrow_function" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    let is_exported = js_symbol_is_exported(node, source);
                    let enclosing_type = (kind == "method_definition")
                        .then(|| enclosing_type_name(node, source))
                        .flatten();
                    // `const svc = { fetch() {} }`: the method is `svc.fetch`.
                    let object_owner = (kind == "method_definition" && enclosing_type.is_none())
                        .then(|| js_object_member_owner(node))
                        .flatten()
                        .and_then(|declarator| get_child_text(declarator, "name", source))
                        .map(|object| format!("{}::{}", file_symbol_name, object));
                    let parent_symbol = enclosing_type
                        .as_ref()
                        .map(|type_name| format!("{}::{}", file_symbol_name, type_name))
                        .or_else(|| object_owner.clone())
                        .or_else(|| enclosing_callable_qualified(node, source, file_symbol_name))
                        .unwrap_or_else(|| file_symbol_name.to_string());
                    let qualified_name = enclosing_type
                        .map(|type_name| format!("{}::{}.{}", file_symbol_name, type_name, n))
                        .or_else(|| object_owner.map(|object| format!("{object}.{n}")))
                        .unwrap_or_else(|| {
                            scoped_qualified_name(node, source, file_symbol_name, &n)
                        });
                    if kind == "method_definition" {
                        if let Some(reason) = crate::wiring::js_lifecycle_hook_reason(&n)
                            .or_else(|| crate::wiring::js_bundler_plugin_hook_reason(&n))
                        {
                            wiring.push(WiringAnnotation {
                                kind: WiringKind::RuntimeEntryPoint,
                                target_symbol: qualified_name.clone(),
                                details: reason.to_string(),
                            });
                        } else if let Some(callee) = js_object_literal_argument_callee(node, source)
                        {
                            // The structural rule the two name lists above
                            // cannot express. Second, not first: where a name
                            // already identifies the framework, that reason is
                            // the more specific one and stays.
                            wiring.push(WiringAnnotation {
                                kind: WiringKind::RuntimeEntryPoint,
                                target_symbol: qualified_name.clone(),
                                details: crate::wiring::js_object_literal_argument_reason(&callee),
                            });
                        }
                    }
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name,
                        kind: if kind == "method_definition" {
                            SymbolKind::Method
                        } else {
                            SymbolKind::Function
                        },
                        span,
                        is_exported,
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(parent_symbol),
                        body_signature: None,
                        declaration_hash: None,
                        return_type: None,
                    });
                }
            }
            // `handle = () => this.get(k)` in a class body and `remove: () => …`
            // in an object literal bound to a module-level `const`: a function
            // stored in a member is a method of its owner, and a call written
            // `owner.handle()` names it. Without the symbol the arrow's own calls
            // were attributed to the file and its callers had nothing to reach.
            "public_field_definition" | "field_definition" | "pair" => {
                let value = node.child_by_field_name("value");
                let function = value.filter(|value| {
                    matches!(value.kind(), "arrow_function" | "function_expression")
                });
                if let Some(function) = function {
                    let member = js_function_valued_member(function, source);
                    let owner = if kind == "pair" {
                        js_object_member_owner(function)
                            .and_then(|declarator| get_child_text(declarator, "name", source))
                            .map(|object| format!("{}::{}", file_symbol_name, object))
                    } else {
                        enclosing_type_name(node, source)
                            .map(|type_name| format!("{}::{}", file_symbol_name, type_name))
                    };
                    if let (Some(name), Some(owner)) = (member, owner) {
                        symbols.push(ExtractedSymbol {
                            qualified_name: format!("{owner}.{name}"),
                            name,
                            kind: SymbolKind::Method,
                            span,
                            is_exported: js_symbol_is_exported(node, source),
                            docstring: None,
                            signature: None,
                            parent_symbol: Some(owner),
                            body_signature: None,
                            declaration_hash: None,
                            return_type: None,
                        });
                    }
                }
            }
            "variable_declarator" => {
                let value = node.child_by_field_name("value");
                // `const f = () => …` and `const f = useCallback(() => …)` both
                // bind `f` to a function; see `JS_FUNCTION_WRAPPERS`.
                let binds_function = value.is_some_and(|value| {
                    matches!(value.kind(), "arrow_function" | "function_expression")
                        || js_wrapped_function(value, source).is_some()
                });
                // A module-level binding — with or without an initializer
                // (`export let U: number;`), and every name a destructuring
                // pattern binds. Kept as a candidate whatever its own keyword
                // says: `export { local }` may publish it further down, so the
                // export filter runs after the walk.
                if !binds_function && is_module_level(node) {
                    if let Some(target) = node.child_by_field_name("name") {
                        let is_exported = js_symbol_is_exported(node, source);
                        for name in js_pattern_bound_names(target, source) {
                            symbols.push(module_binding_symbol(
                                file_symbol_name,
                                name,
                                is_exported,
                                span.clone(),
                            ));
                        }
                    }
                }
                if binds_function {
                    if let Some(n) = get_child_text(node, "name", source) {
                        let is_exported = js_symbol_is_exported(node, source);
                        let parent_symbol =
                            enclosing_callable_qualified(node, source, file_symbol_name)
                                .unwrap_or_else(|| file_symbol_name.to_string());
                        symbols.push(ExtractedSymbol {
                            name: n.clone(),
                            qualified_name: scoped_qualified_name(
                                node,
                                source,
                                file_symbol_name,
                                &n,
                            ),
                            kind: SymbolKind::Function,
                            span,
                            is_exported,
                            docstring: None,
                            signature: None,
                            parent_symbol: Some(parent_symbol),
                            body_signature: None,
                            declaration_hash: None,
                            return_type: None,
                        });
                    }
                }
            }
            "class_declaration" | "abstract_class_declaration" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    let is_exported = js_symbol_is_exported(node, source);
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name: scoped_qualified_name(node, source, file_symbol_name, &n),
                        kind: SymbolKind::Class,
                        span,
                        is_exported,
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(file_symbol_name.to_string()),
                        body_signature: None,
                        declaration_hash: None,
                        return_type: None,
                    });
                }
            }
            "enum_declaration" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    let is_exported = js_symbol_is_exported(node, source);
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name: scoped_qualified_name(node, source, file_symbol_name, &n),
                        kind: SymbolKind::Enum,
                        span,
                        is_exported,
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(file_symbol_name.to_string()),
                        body_signature: None,
                        declaration_hash: None,
                        return_type: None,
                    });
                }
            }
            // An interface method is a requirement a call through the interface
            // can name. A class overload signature is the same node and is not:
            // the method body is already the symbol. Property, call, construct
            // and index signatures are not methods.
            "method_signature" => {
                if let Some(interface) = enclosing_interface_name(node, source) {
                    if let Some(name_node) = node.child_by_field_name("name") {
                        if name_node.kind() == "property_identifier" {
                            let n = get_node_text(name_node, source);
                            if !n.is_empty() {
                                let parent = format!("{file_symbol_name}::{interface}");
                                symbols.push(ExtractedSymbol {
                                    name: n.clone(),
                                    qualified_name: format!("{parent}.{n}"),
                                    kind: SymbolKind::Method,
                                    span,
                                    is_exported: js_symbol_is_exported(node, source),
                                    docstring: None,
                                    signature: None,
                                    parent_symbol: Some(parent),
                                    body_signature: None,
                                    declaration_hash: None,
                                    return_type: None,
                                });
                            }
                        }
                    }
                }
            }
            "interface_declaration" | "type_alias_declaration" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    let is_exported = js_symbol_is_exported(node, source);
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name: scoped_qualified_name(node, source, file_symbol_name, &n),
                        kind: SymbolKind::Interface,
                        span,
                        is_exported,
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(file_symbol_name.to_string()),
                        body_signature: None,
                        declaration_hash: None,
                        return_type: None,
                    });
                }
            }
            "internal_module" | "module" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    let is_exported = js_symbol_is_exported(node, source);
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name: scoped_qualified_name(node, source, file_symbol_name, &n),
                        kind: SymbolKind::Module,
                        span,
                        is_exported,
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(file_symbol_name.to_string()),
                        body_signature: None,
                        declaration_hash: None,
                        return_type: None,
                    });
                }
            }
            "import_statement" | "export_statement" => {
                if let Some(local) = js_default_export_local(node, source) {
                    // `export default Card` / `export default memo(Card)`: the
                    // module's default export *is* that local binding. Recorded
                    // under the name `default`, which is the name an importer's
                    // `import X from` and a barrel's `export { default } from`
                    // ask for.
                    exports.push(ExtractedExport {
                        exported_name: "default".to_string(),
                        local_name: Some(local),
                        module_specifier: None,
                        span: span.clone(),
                    });
                }
                let text = get_node_text(node, source);
                let mut alias = None;
                let mut imported_names = vec![];
                let mut local_names = vec![];
                let mod_spec = find_string_child(node, source).unwrap_or_default();

                if text.contains("import * as ") {
                    if let Some(idx) = text.find("import * as ") {
                        let rest = &text[idx + 12..];
                        if let Some(from_idx) = rest.find(" from") {
                            let ns_alias = rest[..from_idx].trim().to_string();
                            alias = Some(ns_alias.clone());
                            imported_names.push("*".to_string());
                            local_names.push(ns_alias);
                        }
                    }
                } else if text.contains("export *") {
                    imported_names.push("*".to_string());
                    local_names.push("*".to_string());
                    exports.push(ExtractedExport {
                        exported_name: "*".to_string(),
                        local_name: None,
                        module_specifier: (!mod_spec.is_empty()).then(|| mod_spec.clone()),
                        span: span.clone(),
                    });
                } else if let Some(clause) = js_binding_clause(node) {
                    let (names, locals) = js_clause_bindings(clause, source);
                    imported_names = names;
                    local_names = locals;
                    // `import Card, { memo } from './x'` binds the module's
                    // default export beside its named ones. The braces were all
                    // this branch read, so the default's local name was lost and
                    // every use of it named a binding nothing declared.
                    if let Some(default_local) = js_default_import_local(node, source) {
                        imported_names.push("default".to_string());
                        local_names.push(default_local);
                    }
                    if text.trim_start().starts_with("export") {
                        for (local, exported) in imported_names.iter().zip(local_names.iter()) {
                            exports.push(ExtractedExport {
                                exported_name: exported.clone(),
                                local_name: Some(local.clone()),
                                module_specifier: (!mod_spec.is_empty()).then(|| mod_spec.clone()),
                                span: span.clone(),
                            });
                        }
                    }
                } else if text.trim_start().starts_with("import ") {
                    let rest = text.trim_start().trim_start_matches("import ");
                    if let Some(from_index) = rest.find(" from ") {
                        let candidate = rest[..from_index].trim();
                        if !candidate.is_empty()
                            && candidate.chars().all(|character| {
                                character.is_ascii_alphanumeric() || matches!(character, '_' | '$')
                            })
                        {
                            alias = Some(candidate.to_string());
                        }
                    }
                }

                // A module edge needs a module. Gating on `imported_names`
                // instead let `export { a as b };` — a purely local re-export
                // with no `from` — push an import whose `module_specifier` was
                // `""`, an endpoint no resolver can ever bind, and it did the
                // same for every function body the old text scan mis-sliced.
                if !mod_spec.is_empty() {
                    imports.push(ExtractedImport {
                        raw_import: text,
                        module_specifier: mod_spec,
                        imported_names,
                        local_names,
                        alias,
                        span,
                        path_load: None,
                    });
                }
            }
            "jsx_opening_element" | "jsx_self_closing_element" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let tag_name = get_node_text(name_node, source);
                    references.push(extracted_reference(
                        name_node,
                        source,
                        file_symbol_name,
                        tag_name.clone(),
                        ReferenceKind::JsxTag,
                        None,
                    ));
                    // Only a *component* tag is a reference to a symbol. A
                    // lowercase tag is an intrinsic host element: JSX compiles
                    // `<div/>` to the string "div", never to a binding, so a
                    // call edge for it can never resolve and is pure noise.
                    // The JsxTag reference above still records every tag.
                    if jsx_tag_is_component(&tag_name) {
                        // `<motion.div/>` is a member access, not a component
                        // named `motion.div`. Split it like any other member
                        // call so the namespace lands in the receiver, where
                        // import evidence can recognise it — otherwise the tag
                        // names a symbol nothing declares and joins to nothing.
                        let (callee_name, receiver_expr) = match tag_name.rsplit_once('.') {
                            Some((object, property)) if !property.is_empty() => {
                                (property.to_string(), Some(object.to_string()))
                            }
                            _ => (tag_name, None),
                        };
                        calls.push(ExtractedCall {
                            caller_symbol: enclosing_callable_qualified(
                                node,
                                source,
                                file_symbol_name,
                            ),
                            callee_name,
                            receiver_expr,
                            span,
                        });
                    }
                }
            }
            "call_expression" => {
                if let Some(f) = node.child_by_field_name("function") {
                    let callee = get_node_text(f, source);
                    // `import("./CloneModal.svelte")` is a module load, not a
                    // call to a function named `import`. Until this arm treated
                    // it like `require`, every Svelte lazy view had no Import
                    // edge and showed up as unwired / dead.
                    if callee == "require" || callee == "import" {
                        if let Some(args) = node.child_by_field_name("arguments") {
                            if let Some(arg) = args.named_child(0) {
                                let raw = get_node_text(arg, source);
                                if !raw.contains("${") {
                                    let mod_spec = raw
                                        .trim_matches('"')
                                        .trim_matches('\'')
                                        .trim_matches('`')
                                        .to_string();
                                    if !mod_spec.is_empty() {
                                        let (alias, imported_names, local_names) =
                                            if callee == "require" {
                                                js_require_bindings(node, source)
                                            } else {
                                                js_dynamic_import_bindings(node, source)
                                            };
                                        imports.push(ExtractedImport {
                                            raw_import: get_node_text(node, source),
                                            module_specifier: mod_spec,
                                            imported_names,
                                            local_names,
                                            alias,
                                            span: span.clone(),
                                            path_load: None,
                                        });
                                    }
                                }
                            }
                        }
                    } else {
                        // Bundled JavaScript is full of immediately-invoked
                        // literals — `(function(t){if(Array.isArray(t))return t})(x)`
                        // — whose `function` field is the literal itself. SC26
                        // stopped the Go `defer func(){…}()` form at the Go arm
                        // alone; here the whole minified function body was still
                        // being recorded as a callee name.
                        if let Some((callee_name, receiver_expr)) = split_call_target(f, source) {
                            references.push(extracted_reference(
                                f,
                                source,
                                file_symbol_name,
                                callee_name.clone(),
                                ReferenceKind::Call,
                                assignment_binding(node, source),
                            ));
                            calls.push(ExtractedCall {
                                caller_symbol: enclosing_callable_qualified(
                                    node,
                                    source,
                                    file_symbol_name,
                                ),
                                callee_name,
                                receiver_expr,
                                span,
                            });
                        }
                    }
                }
            }
            "new_expression" => {
                // A constructor is split exactly like a call target, and for
                // the same reason. This arm read the constructor's raw source
                // text, so `new pkg.Widget()` named a symbol `pkg.Widget` that
                // nothing declares while discarding the receiver import
                // evidence recognises, and `new (cond ? A : B)()` recorded a
                // ternary as a constructed type. The same defect as the JSX
                // member tag SC26 fixed, in the one JS shape that never went
                // through the splitter.
                if let Some(f) = node.child_by_field_name("constructor") {
                    if let Some((callee_name, receiver_expr)) = split_call_target(f, source) {
                        references.push(extracted_reference(
                            f,
                            source,
                            file_symbol_name,
                            callee_name.clone(),
                            ReferenceKind::Constructor,
                            assignment_binding(node, source),
                        ));
                        calls.push(ExtractedCall {
                            caller_symbol: enclosing_callable_qualified(
                                node,
                                source,
                                file_symbol_name,
                            ),
                            callee_name,
                            receiver_expr,
                            span,
                        });
                    }
                }
            }
            _ => {}
        },
        "rust" => match kind {
            "function_item" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    let text = get_node_text(node, source);
                    // Single owner for "function declared inside a type body":
                    // a `fn` in a trait or impl block is that type's method, not
                    // a second top-level function with the same bare name.
                    let owner = enclosing_type_name(node, source);
                    let qualified_name = match &owner {
                        Some(type_name) => format!("{}::{}.{}", file_symbol_name, type_name, n),
                        None => scoped_qualified_name(node, source, file_symbol_name, &n),
                    };
                    // Attribute first: `#[test] fn helper` inside a trait impl
                    // is reached by the harness, which is the sharper reason.
                    // Bind each parameter name to its declared type, scoped
                    // to this function so two functions using the same
                    // parameter name cannot collide (the SC9 lesson).
                    for (param, type_name, qualifier) in param_type_bindings(node, source, lang) {
                        let scope = node.child_by_field_name("body").and_then(|body| {
                            enclosing_callable_qualified(body, source, file_symbol_name)
                        });
                        // The qualifying module, when the type was written with
                        // one. Carried beside the type rather than inside it so
                        // dispatch keeps the bare name (SC25).
                        if let Some(qualifier) = qualifier {
                            references.push(ExtractedReference {
                                name: qualifier,
                                kind: ReferenceKind::TypeQualifier,
                                span: node_span(node),
                                enclosing_symbol: scope.clone(),
                                assigned_to: Some(param.clone()),
                                // The mirrored call already carries the receiver; repeating it here would be a second copy of one fact.
                                receiver_expr: None,
                            });
                        }
                        references.push(ExtractedReference {
                            name: type_name,
                            kind: ReferenceKind::Type,
                            span: node_span(node),
                            enclosing_symbol: scope,
                            assigned_to: Some(param),
                            // The mirrored call already carries the receiver; repeating it here would be a second copy of one fact.
                            receiver_expr: None,
                        });
                    }
                    let entry_reason = rust_attribute_paths(node, source)
                        .iter()
                        .find_map(|path| crate::wiring::rust_attribute_entry_reason(path))
                        .map(|reason| (WiringKind::RuntimeEntryPoint, reason.to_string()))
                        .or_else(|| {
                            rust_method_structural_reason(node)
                                .map(|reason| (WiringKind::StructuralExempt, reason.to_string()))
                        })
                        .or_else(|| {
                            (owner.is_none()
                                && n == "main"
                                && crate::wiring::rust_path_declares_main(file_symbol_name))
                            .then(|| {
                                (
                                    WiringKind::RuntimeEntryPoint,
                                    "binary entry point invoked by the toolchain".to_string(),
                                )
                            })
                        });
                    if let Some((wiring_kind, details)) = entry_reason {
                        wiring.push(WiringAnnotation {
                            kind: wiring_kind,
                            target_symbol: qualified_name.clone(),
                            details,
                        });
                    }
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name,
                        kind: if owner.is_some() {
                            SymbolKind::Method
                        } else {
                            SymbolKind::Function
                        },
                        span,
                        is_exported: text.starts_with("pub"),
                        docstring: None,
                        signature: rust_fn_signature(node, source),
                        parent_symbol: Some(match &owner {
                            Some(type_name) => format!("{}::{}", file_symbol_name, type_name),
                            None => enclosing_callable_qualified(node, source, file_symbol_name)
                                .unwrap_or_else(|| file_symbol_name.to_string()),
                        }),
                        body_signature: None,
                        declaration_hash: None,
                        return_type: None,
                    });
                }
            }
            // Calls hidden inside a macro body (SC13). The token tree is
            // re-parsed; nothing is emitted when it is not a valid argument
            // list, so a declarative macro body contributes no fabricated call.
            "macro_invocation" => {
                let caller = enclosing_callable_qualified(node, source, file_symbol_name);
                // `tauri::generate_handler![a::cmd, cmd2]` hands each listed
                // function to the IPC dispatcher: a use by name, with no call
                // expression anywhere.
                for (name, receiver_expr) in rust_handler_list(node, source) {
                    references.push(ExtractedReference {
                        name,
                        kind: ReferenceKind::Name,
                        span: span.clone(),
                        enclosing_symbol: caller.clone(),
                        assigned_to: None,
                        receiver_expr,
                    });
                }
                for (callee_name, receiver_expr) in rust_macro_calls(node, source, file_symbol_name)
                    .into_iter()
                    .map(|(callee_name, receiver_expr)| {
                        // A call written inside a macro argument — which is
                        // where `assert_eq!(super::helper(1), 2)` puts most of
                        // a test module's calls — reaches here instead of the
                        // `call_expression` arm, and needs the same absorption.
                        // The nesting is read from the macro invocation, whose
                        // ancestors are the real tree; the re-parsed token tree
                        // inside it has none.
                        (callee_name, rust_absorb_inline_super(node, receiver_expr))
                    })
                {
                    references.push(extracted_reference(
                        node,
                        source,
                        file_symbol_name,
                        callee_name.clone(),
                        ReferenceKind::Call,
                        None,
                    ));
                    calls.push(ExtractedCall {
                        caller_symbol: caller.clone(),
                        callee_name,
                        receiver_expr,
                        span: span.clone(),
                    });
                }
            }
            // A bare trait method signature (`fn greet(&self) -> String;`) is a
            // `function_signature_item`, not a `function_item`, and was never
            // extracted (SC6b). A defaulted method — which has a body — was, so
            // a trait's declared surface was represented only where it happened
            // to carry an implementation. The signature IS the trait's contract;
            // omitting it left the declaration invisible to the graph while the
            // implementations pointed at nothing.
            //
            // This depends on SC11: while impl methods were qualified by the
            // trait, adding the signature would have produced a third source of
            // the same name. With impls qualified by their type, `Trait.method`
            // now belongs to the declaration alone.
            "function_signature_item" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    let owner = enclosing_type_name(node, source);
                    let qualified_name = match &owner {
                        Some(type_name) => format!("{}::{}.{}", file_symbol_name, type_name, n),
                        None => scoped_qualified_name(node, source, file_symbol_name, &n),
                    };
                    // Rust forbids `pub` on trait items, so exportedness comes
                    // from the trait, not the signature's own text. Treating the
                    // absent `pub` as evidence of deadness is the false positive
                    // `rust_method_structural_reason` exists to prevent, and it
                    // already covers `trait_item` ancestors.
                    if let Some(reason) = rust_method_structural_reason(node) {
                        wiring.push(WiringAnnotation {
                            kind: WiringKind::StructuralExempt,
                            target_symbol: qualified_name.clone(),
                            details: reason.to_string(),
                        });
                    }
                    symbols.push(ExtractedSymbol {
                        name: n,
                        qualified_name,
                        // A trait or impl signature is a method. An `extern`
                        // block at module or function scope is a free function:
                        // `extern "C" { fn tree_sitter_liquid(); }` is not a
                        // method of any type, and labelling it one hid it from
                        // the same-file ladder (Rust methods need a receiver).
                        kind: if owner.is_some() {
                            SymbolKind::Method
                        } else {
                            SymbolKind::Function
                        },
                        span,
                        is_exported: false,
                        docstring: None,
                        signature: rust_fn_signature(node, source),
                        parent_symbol: Some(match &owner {
                            Some(type_name) => format!("{}::{}", file_symbol_name, type_name),
                            None => enclosing_callable_qualified(node, source, file_symbol_name)
                                .unwrap_or_else(|| file_symbol_name.to_string()),
                        }),
                        body_signature: None,
                        declaration_hash: None,
                        return_type: None,
                    });
                }
            }
            "struct_item" | "enum_item" | "trait_item" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    let text = get_node_text(node, source);
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name: scoped_qualified_name(node, source, file_symbol_name, &n),
                        kind: match kind {
                            "struct_item" => SymbolKind::Struct,
                            "enum_item" => SymbolKind::Enum,
                            _ => SymbolKind::Trait,
                        },
                        span,
                        is_exported: text.starts_with("pub"),
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(file_symbol_name.to_string()),
                        body_signature: None,
                        declaration_hash: None,
                        return_type: None,
                    });
                }
            }
            // `pub type Result<T> = …`: a named type, so a type-like kind that
            // `Type` references bind to — `Interface`, as a TypeScript `type`
            // alias is. Module level only: a `type Output = …` inside an
            // `impl` is an associated type, reached through the trait, and
            // naming every `Output`/`Item`/`Error` would flood the graph with
            // symbols no path reaches by name.
            "type_item" if is_module_level(node) => {
                if let Some(name) = get_child_text(node, "name", source) {
                    symbols.push(ExtractedSymbol {
                        name: name.clone(),
                        qualified_name: scoped_qualified_name(
                            node,
                            source,
                            file_symbol_name,
                            &name,
                        ),
                        kind: SymbolKind::Interface,
                        span,
                        is_exported: rust_item_is_pub(node, source),
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(file_symbol_name.to_string()),
                        body_signature: None,
                        declaration_hash: None,
                        return_type: None,
                    });
                }
            }
            "const_item" | "static_item" => {
                if let Some(name) = get_child_text(node, "name", source) {
                    push_module_binding(
                        node,
                        file_symbol_name,
                        name,
                        rust_item_is_pub(node, source),
                        span.clone(),
                        symbols,
                    );
                }
            }
            // Methods inside an `impl` block are emitted by the `function_item`
            // arm, which resolves its owning type via `enclosing_type_name`.
            // Emitting them here as well produced the same method twice.
            "impl_item" => {}
            "use_declaration" => {
                rust_use_imports(node, source, span, imports, exports);
            }
            "attribute_item" => {
                rust_attribute_callback_refs(node, source, file_symbol_name, references);
            }
            "call_expression" => {
                if let Some(f) = node.child_by_field_name("function") {
                    // Split `receiver.method()` the way every other language
                    // already does. Emitting the whole dotted chain as one
                    // callee with no receiver meant Rust method calls could
                    // never reach receiver-type resolution at all: the call was
                    // recorded, but with a name no symbol has and no receiver
                    // to type, so it resolved to nothing (SC12).
                    //
                    // The text fallback that used to sit here is gone: an
                    // immediately-invoked closure (`(|| { … })()`) has no callee
                    // to name, and falling back turned its whole body into one —
                    // the shape SC26 removed from the Go arm and left live here.
                    if let Some((callee, receiver_expr)) = split_call_target(f, source) {
                        let receiver_expr = rust_absorb_inline_super(node, receiver_expr);
                        references.push(extracted_reference(
                            f,
                            source,
                            file_symbol_name,
                            callee.clone(),
                            ReferenceKind::Call,
                            // `let w = Worker::new()` binds `w` to `Worker` exactly
                            // as the Python, TS and Go arms record it. This arm
                            // passed `None`, so a Rust value that was constructed
                            // locally had a receiver with no type behind it and
                            // `w.go()` resolved to nothing — the SC12 failure mode
                            // reached by assignment rather than by parameter.
                            assignment_binding(node, source),
                        ));
                        calls.push(ExtractedCall {
                            caller_symbol: enclosing_callable_qualified(
                                node,
                                source,
                                file_symbol_name,
                            ),
                            callee_name: callee,
                            receiver_expr,
                            span,
                        });
                    }
                }
            }
            // `let service = Service { n: 0 };` types `service` as `Service`,
            // exactly as `let service = Service::new();` already did one arm
            // above — and as Go's `composite_literal` has done since SC17.
            //
            // Rust was the one grammar with no arm here at all: measured,
            // `struct_expression` appeared nowhere in this file, so a binding
            // initialised by a struct literal arrived at the resolver with
            // `declared_type: None, initializer: None` and every method called
            // on it resolved to nothing. The resolver's own `T{..}` branch
            // (`type_from_initializer_shape`) was therefore **unreachable for
            // Rust** — it was written for this shape and never fed by it.
            //
            // The name is read through `rust_type_name`, so `Wrapper::<T> { … }`
            // and a scoped `crate::a::Service { … }` reduce to the bare type the
            // same way every other type position does, rather than recording
            // path text no symbol can match.
            "struct_expression" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    if let Some(constructed) = rust_type_name(name_node, source, 0) {
                        references.push(extracted_reference(
                            name_node,
                            source,
                            file_symbol_name,
                            constructed,
                            ReferenceKind::Constructor,
                            assignment_binding(node, source),
                        ));
                    }
                }
            }
            _ => {}
        },
        "go" => match kind {
            "function_declaration" | "method_declaration" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    let receiver = (kind == "method_declaration")
                        .then(|| go_receiver(node, source))
                        .flatten();
                    let parent_symbol = receiver
                        .as_ref()
                        .map(|(_, type_name)| format!("{file_symbol_name}::{type_name}"))
                        .unwrap_or_else(|| file_symbol_name.to_string());
                    let qualified_name = receiver
                        .as_ref()
                        .map(|(_, type_name)| format!("{file_symbol_name}::{type_name}.{n}"))
                        .unwrap_or_else(|| format!("{file_symbol_name}::{n}"));
                    if let Some(reason) = crate::wiring::go_runtime_entry_reason(
                        &n,
                        go_package_name(ast_root(node), source).as_deref(),
                        receiver.is_some(),
                    ) {
                        wiring.push(WiringAnnotation {
                            kind: WiringKind::RuntimeEntryPoint,
                            target_symbol: qualified_name.clone(),
                            details: reason.to_string(),
                        });
                    }
                    // The directive comments directly above the declaration:
                    // cgo's `//export` and `//go:linkname`.
                    let mut above = node.prev_sibling();
                    while let Some(comment) = above.filter(|sibling| sibling.kind() == "comment") {
                        let text = &source[comment.start_byte()..comment.end_byte()];
                        if let Some(reason) = crate::wiring::go_directive_entry_reason(text, &n) {
                            wiring.push(WiringAnnotation {
                                kind: WiringKind::RuntimeEntryPoint,
                                target_symbol: qualified_name.clone(),
                                details: reason.to_string(),
                            });
                            break;
                        }
                        above = comment.prev_sibling();
                    }
                    // Bind each parameter name to its declared type, scoped
                    // to this function so two functions using the same
                    // parameter name cannot collide (the SC9 lesson).
                    for (param, type_name, qualifier) in param_type_bindings(node, source, lang) {
                        if receiver.is_none() {
                            if let Some(reason) = crate::wiring::go_framework_param_entry_reason(
                                &type_name,
                                qualifier.as_deref(),
                            ) {
                                // Framework dispatch, not an ordinary helper.
                                // Deduped below would be wrong: one Matcher
                                // parameter is enough; further parameters must
                                // not emit a second annotation for the same
                                // target.
                                if !wiring.iter().any(|annotation| {
                                    annotation.target_symbol == qualified_name
                                        && annotation.kind == WiringKind::RuntimeEntryPoint
                                        && annotation.details == reason
                                }) {
                                    wiring.push(WiringAnnotation {
                                        kind: WiringKind::RuntimeEntryPoint,
                                        target_symbol: qualified_name.clone(),
                                        details: reason.to_string(),
                                    });
                                }
                            }
                        }
                        let scope = node.child_by_field_name("body").and_then(|body| {
                            enclosing_callable_qualified(body, source, file_symbol_name)
                        });
                        // The qualifying module, when the type was written with
                        // one. Carried beside the type rather than inside it so
                        // dispatch keeps the bare name (SC25).
                        if let Some(qualifier) = qualifier {
                            references.push(ExtractedReference {
                                name: qualifier,
                                kind: ReferenceKind::TypeQualifier,
                                span: node_span(node),
                                enclosing_symbol: scope.clone(),
                                assigned_to: Some(param.clone()),
                                // The mirrored call already carries the receiver; repeating it here would be a second copy of one fact.
                                receiver_expr: None,
                            });
                        }
                        references.push(ExtractedReference {
                            name: type_name,
                            kind: ReferenceKind::Type,
                            span: node_span(node),
                            enclosing_symbol: scope,
                            assigned_to: Some(param),
                            // The mirrored call already carries the receiver; repeating it here would be a second copy of one fact.
                            receiver_expr: None,
                        });
                    }
                    if let Some((recv_name, type_name)) = &receiver {
                        if !recv_name.is_empty() {
                            // The receiver binding is scoped to THIS method, not
                            // to the file (SC9).
                            //
                            // `extracted_reference` derives `enclosing_symbol` by
                            // walking *up* from the node it is handed. Passing the
                            // `method_declaration` started that walk at its parent,
                            // skipping straight past the method to file scope and
                            // yielding `None`. The binding then looked file-wide, so
                            // two types using the same receiver name in one file —
                            // `func (s *A)` and `func (s *B)`, ordinary Go — collided
                            // on one key and every `s.method()` in the file resolved
                            // against whichever type was indexed last, at full
                            // confidence.
                            //
                            // Deriving the scope from the body makes the walk stop at
                            // this method and produces exactly the string a call made
                            // inside it reports as `caller_symbol`, which is what lets
                            // the resolver join the two. The span still covers the
                            // declaration, so parse-error overlap is unchanged.
                            references.push(ExtractedReference {
                                name: type_name.clone(),
                                kind: ReferenceKind::Type,
                                span: node_span(node),
                                enclosing_symbol: node.child_by_field_name("body").and_then(
                                    |body| {
                                        enclosing_callable_qualified(body, source, file_symbol_name)
                                    },
                                ),
                                assigned_to: Some(recv_name.clone()),
                                // The mirrored call already carries the receiver; repeating it here would be a second copy of one fact.
                                receiver_expr: None,
                            });
                        }
                    }
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name,
                        kind: if kind == "method_declaration" {
                            SymbolKind::Method
                        } else {
                            SymbolKind::Function
                        },
                        span,
                        is_exported: n.chars().next().is_some_and(|c| c.is_uppercase()),
                        docstring: None,
                        signature: receiver.as_ref().map(|(recv, ty)| {
                            if recv.is_empty() {
                                format!("({ty})")
                            } else {
                                format!("({recv} *{ty})")
                            }
                        }),
                        parent_symbol: Some(parent_symbol),
                        body_signature: None,
                        declaration_hash: None,
                        return_type: None,
                    });
                }
            }
            "type_declaration" | "type_spec" => {
                let node_to_check = if kind == "type_declaration" {
                    node.child(0).unwrap_or(node)
                } else {
                    node
                };
                if let Some(n) = get_child_text(node_to_check, "name", source) {
                    let qualified_name = scoped_qualified_name(node, source, file_symbol_name, &n);
                    // An interface is an interface, not a struct: its method
                    // requirements are symbols a call through the interface can
                    // name, and what a dispatch edge's `via` points at.
                    let interface = node_to_check
                        .child_by_field_name("type")
                        .filter(|ty| ty.kind() == "interface_type");
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name: qualified_name.clone(),
                        kind: if interface.is_some() {
                            SymbolKind::Interface
                        } else {
                            SymbolKind::Struct
                        },
                        span,
                        is_exported: n.chars().next().is_some_and(|c| c.is_uppercase()),
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(file_symbol_name.to_string()),
                        body_signature: None,
                        declaration_hash: None,
                        return_type: None,
                    });
                    if let Some(interface) = interface {
                        let mut cursor = interface.walk();
                        for member in interface.named_children(&mut cursor) {
                            if member.kind() != "method_elem" {
                                continue;
                            }
                            let Some(method) = get_child_text(member, "name", source) else {
                                continue;
                            };
                            let method_qualified = format!("{qualified_name}.{method}");
                            wiring.push(WiringAnnotation {
                                kind: WiringKind::StructuralExempt,
                                target_symbol: method_qualified.clone(),
                                details: GO_INTERFACE_REQUIREMENT_REASON.to_string(),
                            });
                            symbols.push(ExtractedSymbol {
                                name: method.clone(),
                                qualified_name: method_qualified,
                                kind: SymbolKind::Method,
                                span: node_span(member),
                                is_exported: method
                                    .chars()
                                    .next()
                                    .is_some_and(|c| c.is_uppercase()),
                                docstring: None,
                                signature: None,
                                parent_symbol: Some(qualified_name.clone()),
                                body_signature: None,
                                declaration_hash: None,
                                return_type: None,
                            });
                        }
                    }
                }
            }
            // `const P, Q = 1, 2` declares two constants. `name` is a repeated
            // field (its siblings include the `,` tokens), and reading only the
            // first dropped every later name.
            "const_spec" | "var_spec" => {
                let mut cursor = node.walk();
                let names: Vec<String> = node
                    .children_by_field_name("name", &mut cursor)
                    .filter(|name| name.kind() == "identifier")
                    .map(|name| get_node_text(name, source))
                    .collect();
                for name in names {
                    let exported = name.chars().next().is_some_and(char::is_uppercase);
                    push_module_binding(
                        node,
                        file_symbol_name,
                        name,
                        exported,
                        span.clone(),
                        symbols,
                    );
                }
            }
            // `type A = B`: the grammar spells an alias `type_alias`, not
            // `type_spec`, so the arm above never saw it. A named type like any
            // other `type` declaration here.
            "type_alias" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name: scoped_qualified_name(node, source, file_symbol_name, &n),
                        kind: SymbolKind::Struct,
                        span,
                        is_exported: n.chars().next().is_some_and(|c| c.is_uppercase()),
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(file_symbol_name.to_string()),
                        body_signature: None,
                        declaration_hash: None,
                        return_type: None,
                    });
                }
            }
            "import_spec" => {
                if let Some(p) = node.child_by_field_name("path") {
                    let mod_spec = get_node_text(p, source).trim_matches('"').to_string();
                    let alias = node
                        .child_by_field_name("name")
                        .map(|name| get_node_text(name, source))
                        .filter(|name| !name.is_empty());
                    imports.push(ExtractedImport {
                        raw_import: get_node_text(node, source),
                        module_specifier: mod_spec,
                        imported_names: vec![],
                        local_names: vec![],
                        alias,
                        span,
                        path_load: None,
                    });
                }
            }
            "call_expression" | "composite_literal" | "type_conversion_expression" => {
                let callee_node = if kind == "call_expression" {
                    node.child_by_field_name("function")
                } else if kind == "type_conversion_expression" {
                    // `pkg.New[int](3)`: one explicit type argument followed by
                    // value arguments parses as a conversion to the generic
                    // *type* `pkg.New[int]`, not as a call, because `f[T](x)`
                    // is also how a type is converted. The grammar cannot tell
                    // them apart and neither can the extractor: it names the
                    // callee and the resolver, which knows whether that name
                    // is a function or a type, decides. A conversion to a
                    // non-generic type is a plain `call_expression` already.
                    node.child_by_field_name("type")
                        .filter(|ty| ty.kind() == "generic_type")
                        .and_then(|ty| ty.child_by_field_name("type"))
                } else {
                    // `composite_literal` declares `type` as a required field
                    // (tree-sitter-go node-types.json), so this cannot be None
                    // for a well-formed node. A child-scanning fallback used to
                    // sit here and was measured unreachable: zero invocations
                    // across 2,386 real Go files and across every literal shape
                    // (plain, addressed, sliced, mapped, nested, anonymous
                    // struct, implicit-length array). If a future grammar makes
                    // the field optional this yields None and the literal is
                    // skipped, which is the safe direction — no fabricated call.
                    //
                    // The type is unwrapped to the named type it constructs, so
                    // `[]*Foo{...}` references `Foo` rather than recording the
                    // literal text `[]*Foo` as a callee no symbol can match.
                    node.child_by_field_name("type")
                        .and_then(|t| go_composite_literal_type(t, source, 0))
                };
                // An immediately-invoked function literal — `defer func(){…}()`
                // — has no callee to name, and neither does any other
                // expression that is not a name. Both the literal filter that
                // stood here and the text fallback that followed it now live in
                // `split_call_target`, the one place every grammar's callee name
                // is built and therefore the only place the rule cannot be
                // forgotten by a new arm.
                if let Some(f) = callee_node {
                    if let Some((callee_name, receiver_expr)) = split_call_target(f, source) {
                        references.push(extracted_reference(
                            f,
                            source,
                            file_symbol_name,
                            callee_name.clone(),
                            if kind == "composite_literal" {
                                ReferenceKind::Constructor
                            } else {
                                ReferenceKind::Call
                            },
                            assignment_binding(node, source),
                        ));
                        calls.push(ExtractedCall {
                            caller_symbol: enclosing_callable_qualified(
                                node,
                                source,
                                file_symbol_name,
                            ),
                            callee_name,
                            receiver_expr,
                            span,
                        });
                    }
                }
            }
            _ => {}
        },
        // HCL/Terraform. Its declarations are `block` nodes rather than
        // functions or types, so the generic declaration table below cannot see
        // them: a Terraform file would index with a File node and nothing else.
        // Blocks are named by their Terraform address (`resource.aws_s3_bucket.b`,
        // `variable.region`), which is the identity operators already use and
        // the one `terraform plan` prints.
        "hcl" => {
            if kind == "block" {
                if let Some(name) = hcl_block_address(node, source) {
                    let block_type = node
                        .named_child(0)
                        .map(|child| get_node_text(child, source))
                        .unwrap_or_default();
                    symbols.push(ExtractedSymbol {
                        name: name.clone(),
                        qualified_name: format!("{file_symbol_name}::{name}"),
                        kind: match block_type.as_str() {
                            "variable" | "output" | "locals" => SymbolKind::Variable,
                            _ => SymbolKind::Module,
                        },
                        span,
                        // Terraform has no private declarations; every block is
                        // addressable from outside the file.
                        is_exported: true,
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(file_symbol_name.to_string()),
                        body_signature: None,
                        declaration_hash: None,
                        return_type: None,
                    });
                }
            }
        }

        // Every other linked grammar. The specialised arms above encode
        // language-specific call, import and receiver semantics; this one
        // recovers the declaration skeleton — types and functions — which is
        // what a code map needs first and what the frozen Python baseline
        // produces for these languages through its own generic extractor.
        _ => {
            let c_family = is_c_family_grammar(lang);
            if c_family {
                extract_c_family_call(lang, node, source, file_symbol_name, calls, references);
                extract_c_header_export(node, source, file_symbol_name, exports);
            } else {
                // Every other language reaching this arm gets declarations only
                // unless `langcalls` has a module for it (SC34). Five of them —
                // Ruby, Swift, PHP, Scala, Lua — were measured recovering calls
                // under the Python implementation this port replaces, so their
                // absence here was a migration regression, not a shared gap.
                crate::langcalls::extract_calls(
                    lang,
                    node,
                    source,
                    file_symbol_name,
                    calls,
                    references,
                );
            }
            // Declaration identity, kind and visibility all come from the
            // language's own module in `langdecl`, which is the single owner of
            // the answer `langcalls::scope` mirrors for caller attribution.
            if let Some(declaration) = crate::langdecl::declaration_of(lang, node, source) {
                let qualified = declaration.qualified(file_symbol_name);
                let name = declaration.name.clone();
                if declaration.declared_kind == SymbolKind::Function
                    && is_metal_path(file_symbol_name)
                {
                    if let Some(reason) = metal_shader_entry_reason_of(node, source) {
                        wiring.push(WiringAnnotation {
                            kind: WiringKind::RuntimeEntryPoint,
                            target_symbol: qualified.clone(),
                            details: reason.to_string(),
                        });
                    }
                }
                if c_family && declaration.declared_kind == SymbolKind::Function {
                    if let Some(reason) =
                        c_family_entry_point_reason(node, source, file_symbol_name, &name)
                    {
                        wiring.push(WiringAnnotation {
                            kind: WiringKind::RuntimeEntryPoint,
                            target_symbol: qualified.clone(),
                            details: reason.to_string(),
                        });
                    }
                }
                // Reading visibility is what makes an exemption necessary: until
                // a symbol can report `is_exported = false` nothing could ever
                // be dead, so nothing needed exempting.
                if let Some((wiring_kind, reason)) =
                    crate::langdecl::exemption(lang, node, source, &declaration)
                {
                    wiring.push(WiringAnnotation {
                        kind: wiring_kind,
                        target_symbol: qualified.clone(),
                        details: reason.to_string(),
                    });
                }
                symbols.push(ExtractedSymbol {
                    name,
                    qualified_name: qualified,
                    kind: declaration.emitted_kind(),
                    span,
                    is_exported: crate::langdecl::is_exported_of(
                        lang,
                        node,
                        source,
                        &declaration.name,
                        file_symbol_name,
                    ),
                    docstring: None,
                    // A Swift member property says so, and whether it stores a
                    // value: it shares `Field` with an enum case.
                    signature: (lang == "swift")
                        .then(|| crate::langdecl::swift::property_signature(node))
                        .flatten()
                        .map(str::to_string),
                    parent_symbol: Some(declaration.parent_symbol(file_symbol_name)),
                    body_signature: None,
                    declaration_hash: None,
                    return_type: None,
                });
                // Same SC12 binding Rust and Go already emit: a parameter's
                // declared type is the only typed binding available when the
                // function did not construct the value. Without it, C-family
                // method calls on parameters (`s.compute(1)`) stay uninferred.
                if c_family && declaration.declared_kind == SymbolKind::Function {
                    for (param, type_name, qualifier) in param_type_bindings(node, source, lang) {
                        let scope = node.child_by_field_name("body").and_then(|body| {
                            enclosing_callable_qualified(body, source, file_symbol_name)
                        });
                        if let Some(qualifier) = qualifier {
                            references.push(ExtractedReference {
                                name: qualifier,
                                kind: ReferenceKind::TypeQualifier,
                                span: node_span(node),
                                enclosing_symbol: scope.clone(),
                                assigned_to: Some(param.clone()),
                                receiver_expr: None,
                            });
                        }
                        references.push(ExtractedReference {
                            name: type_name,
                            kind: ReferenceKind::Type,
                            span: node_span(node),
                            enclosing_symbol: scope,
                            assigned_to: Some(param),
                            receiver_expr: None,
                        });
                    }
                }
            }
        }
    }
    // Import extraction for the languages whose specifier names a file
    // (W0.3 move 2), after the `match` rather than inside its generic arm.
    //
    // Two of the languages served here reach `extract_node` through a
    // *specialised* arm — `hcl` has one of its own, and the C family takes the
    // `c_family` branch — so a call wired beside `langcalls::extract_calls`
    // would have missed both, including the one family that had no import
    // handler anywhere. This position is also the one that cannot rot: a
    // language gaining a specialised arm later keeps its imports, where the
    // inner position would have taken them away silently.
    //
    // Languages whose arm already pushes imports — Python, JS/TS, Rust, Go —
    // are absent from the dispatcher's match, so nothing is counted twice.
    crate::langimports::extract_imports(lang, node, source, imports);
    // A declared field's type, for the same reason and from the same position:
    // Rust, Go, Python and HCL take specialised arms above and the C family
    // takes `c_family`, so the one position that serves every grammar — and
    // keeps serving one that gains a specialised arm later — is this one.
    let _ = crate::langfields::extract_field_type(lang, node, source, file_symbol_name, references);
    maybe_push_name_reference(node, source, lang, file_symbol_name, references);
}
