use super::*;

pub(crate) fn rust_type_name(node: Node, source: &str, depth: usize) -> Option<String> {
    if depth > 16 {
        return None;
    }
    match node.kind() {
        "reference_type" | "pointer_type" => node
            .child_by_field_name("type")
            .and_then(|inner| rust_type_name(inner, source, depth + 1)),
        "generic_type" => rust_generic_identity_node(node, source)
            .and_then(|inner| rust_type_name(inner, source, depth + 1)),
        // `dyn Render` / `impl Render`: a value of some type implementing
        // `Render`, so the trait is where a method on it is declared. Only a
        // single named bound: `dyn A + Send` is a `bounded_type` and stays
        // unnamed rather than being read as one of its traits.
        "dynamic_type" | "abstract_type" => node
            .child_by_field_name("trait")
            .filter(|bound| {
                matches!(
                    bound.kind(),
                    "type_identifier" | "scoped_type_identifier" | "generic_type"
                )
            })
            .and_then(|bound| rust_type_name(bound, source, depth + 1)),
        "type_identifier" | "scoped_type_identifier" => {
            let text = get_node_text(node, source);
            let bare = text.rsplit("::").next().unwrap_or(&text);
            // ` a::b::{c};` is a struct expression whose name is `a::b::` and a
            // MISSING type: no name at all, which is `None`, not an empty one.
            Some(bare.split('<').next().unwrap_or(bare).trim().to_string())
                .filter(|name| !name.is_empty())
        }
        _ => None,
    }
}

/// One leaf of a `use` tree: the module it comes from and the name it binds.
pub(super) struct RustUseLeaf {
    /// The path segments before the imported name — `["std", "collections"]`
    /// for `std::collections::BTreeMap`.
    pub(super) module: Vec<String>,
    /// The name imported from that module, or `None` for a glob.
    pub(super) name: Option<String>,
    /// The local binding, when `as` renamed it.
    pub(super) alias: Option<String>,
}

/// How deeply a `use` tree may nest before recovery stops.
///
/// A `use` group is a tree and this walk recurses per level, so an adversarially
/// nested statement is a stack-overflow shape — the same reason
/// [`rust_type_name`] is bounded. There is deliberately **no cap on the number
/// of leaves**: leaves cost source bytes, which `MAX_SOURCE_BYTES` already
/// bounds, and a leaf cap would silently truncate an import list — presenting a
/// capped sample as the file's complete set of imports, which is exactly the
/// shape that makes "nothing imports this" mean two different things.
pub(super) const RUST_USE_MAX_DEPTH: usize = 32;

/// The path segments of a `use` path node, appended to `out`.
///
/// Returns `false` for a node shape this does not recognise, and the caller
/// abandons the leaf rather than recording a partial path — half a module path
/// resolves to a *different* module, which is worse than not resolving.
pub(super) fn rust_path_segments(
    node: Node,
    source: &str,
    depth: usize,
    out: &mut Vec<String>,
) -> bool {
    if depth > RUST_USE_MAX_DEPTH {
        return false;
    }
    match node.kind() {
        "scoped_identifier" | "scoped_type_identifier" => {
            if let Some(path) = node.child_by_field_name("path") {
                if !rust_path_segments(path, source, depth + 1, out) {
                    return false;
                }
            }
            match node.child_by_field_name("name") {
                Some(name) => rust_path_segments(name, source, depth + 1, out),
                None => false,
            }
        }
        "identifier" | "type_identifier" | "primitive_type" | "super" | "crate" | "self"
        | "metavariable" => {
            let text = get_node_text(node, source);
            if text.is_empty() {
                return false;
            }
            out.push(text);
            true
        }
        _ => false,
    }
}

/// Flatten a `use` tree into one leaf per name it binds.
pub(super) fn collect_rust_use_leaves(
    node: Node,
    source: &str,
    prefix: &[String],
    depth: usize,
    out: &mut Vec<RustUseLeaf>,
) {
    if depth > RUST_USE_MAX_DEPTH {
        return;
    }
    match node.kind() {
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_rust_use_leaves(child, source, prefix, depth + 1, out);
            }
        }
        "scoped_use_list" => {
            let mut nested = prefix.to_vec();
            if let Some(path) = node.child_by_field_name("path") {
                if !rust_path_segments(path, source, 0, &mut nested) {
                    return;
                }
            }
            if let Some(list) = node.child_by_field_name("list") {
                collect_rust_use_leaves(list, source, &nested, depth + 1, out);
            }
        }
        "use_as_clause" => {
            let mut segments = prefix.to_vec();
            let Some(path) = node.child_by_field_name("path") else {
                return;
            };
            if !rust_path_segments(path, source, 0, &mut segments) {
                return;
            }
            let alias = node
                .child_by_field_name("alias")
                .map(|child| get_node_text(child, source))
                .filter(|alias| !alias.is_empty());
            if let Some(name) = segments.pop() {
                out.push(RustUseLeaf {
                    module: segments,
                    name: Some(name),
                    alias,
                });
            }
        }
        "use_wildcard" => {
            let mut segments = prefix.to_vec();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if !rust_path_segments(child, source, 0, &mut segments) {
                    return;
                }
            }
            out.push(RustUseLeaf {
                module: segments,
                name: None,
                alias: None,
            });
        }
        _ => {
            let mut segments = prefix.to_vec();
            if !rust_path_segments(node, source, 0, &mut segments) {
                return;
            }
            if let Some(name) = segments.pop() {
                out.push(RustUseLeaf {
                    module: segments,
                    name: Some(name),
                    alias: None,
                });
            }
        }
    }
}

/// How many **inline** `mod { … }` blocks enclose this node.
///
/// Rust's `super` is relative to the module, not to the file, and an inline
/// module is one module deeper without being one file deeper. `mod tests { use
/// super::*; }` therefore names the *file it is written in*; the same statement
/// at file level names the parent directory's module. This repository contains
/// 87 of the first spelling and none of the resolver's rungs could tell them
/// apart.
/// A Rust call receiver of exactly `super`, resolved against the inline-module
/// nesting the call is written inside.
///
/// `mod tests { super::helper() }` spends its `super` on the inline module and
/// therefore names **this file**; the identical text at file level names the
/// parent directory's module. Both reach the resolver as the receiver string
/// `"super"` and nothing downstream can tell them apart — the nesting is only
/// knowable here, while the syntax tree still exists. This is the call half of
/// the fact [`rust_use_imports`] already resolves for `use`, and it reaches the
/// same answer by the same rule: once the nesting absorbs the `super`, the
/// target is this file's own module, spelled `self`.
///
/// Leading `super` segments are spent against the inline-module depth, the
/// same accounting [`rust_use_specifier`] uses. `super::helper()` inside
/// `mod tests` becomes `self`. `super::Thing::build()` becomes `self::Thing`:
/// one `super` paid for the inline module, and `Thing` is still this file's
/// type. A `super` that outlasts the nesting is left in the receiver
/// (`super::super::helper` at depth one stays `super::helper`) so the resolver
/// can walk out of the file. Anything that is not a path of plain identifiers
/// is returned unchanged.
///
/// Depth beyond one is treated the same way because the extractor flattens
/// inline modules: a `fn` inside `mod a { mod b { … } }` is extracted with the
/// file-level qualified name, so every `super` that stays inside the file lands
/// in the one namespace the file has.
pub(super) fn rust_absorb_inline_super(node: Node, receiver: Option<String>) -> Option<String> {
    let receiver = receiver?;
    let depth = rust_inline_module_depth(node);
    if depth == 0
        || !receiver.split("::").all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    {
        return Some(receiver);
    }
    let mut segments: Vec<&str> = receiver.split("::").collect();
    if segments.first() != Some(&"super") {
        return Some(receiver);
    }
    let mut spent = 0usize;
    while spent < depth && segments.first() == Some(&"super") {
        segments.remove(0);
        spent += 1;
    }
    if spent == 0 {
        return Some(receiver);
    }
    if segments.is_empty() {
        return Some("self".to_string());
    }
    if segments.first() == Some(&"super") {
        return Some(segments.join("::"));
    }
    Some(format!("self::{}", segments.join("::")))
}

/// The inline `mod name { … }` blocks and the `impl Deref for T { type Target
/// = U; }` pairs of a Rust file, each sorted and deduplicated.
///
/// A `mod name;` declaration has no body and names a file, so it is not an
/// inline module. The walk is an explicit worklist, bounded by the same
/// deadline every other whole-tree pass here honours.
pub(super) fn collect_rust_item_facts(
    root: Node,
    source: &str,
) -> (Vec<String>, Vec<(String, String)>, Vec<String>) {
    let mut modules = BTreeSet::new();
    let mut derefs = BTreeSet::new();
    let mut blankets = BTreeSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if walk_deadline_passed() {
            break;
        }
        match node.kind() {
            "mod_item" if node.child_by_field_name("body").is_some() => {
                if let Some(name) = get_child_text(node, "name", source) {
                    modules.insert(name);
                }
            }
            "impl_item" => {
                if let Some(pair) = rust_deref_impl(node, source) {
                    derefs.insert(pair);
                }
                if let Some(param) = rust_blanket_impl_param(node, source) {
                    blankets.insert(param);
                }
            }
            _ => {}
        }
        push_named_children(node, &mut stack);
    }
    (
        modules.into_iter().collect(),
        derefs.into_iter().collect(),
        blankets.into_iter().collect(),
    )
}

/// `T` for a blanket impl, `impl<T: Display> Describe for T`: a trait impl
/// whose self type is one of the impl's own type parameters.
pub(super) fn rust_blanket_impl_param(node: Node, source: &str) -> Option<String> {
    node.child_by_field_name("trait")?;
    let self_type = node.child_by_field_name("type")?;
    if self_type.kind() != "type_identifier" {
        return None;
    }
    let self_type = get_node_text(self_type, source);
    let params = node.child_by_field_name("type_parameters")?;
    let mut cursor = params.walk();
    let declared = params.named_children(&mut cursor).any(|param| {
        let name = match param.kind() {
            "type_identifier" => Some(get_node_text(param, source)),
            "constrained_type_parameter" => get_child_text(param, "left", source),
            "type_parameter" | "optional_type_parameter" => get_child_text(param, "name", source),
            _ => None,
        };
        name.as_deref() == Some(self_type.as_str())
    });
    declared.then_some(self_type)
}

/// `(Guard, Inner)` for `impl Deref for Guard { type Target = Inner; .. }`.
pub(super) fn rust_deref_impl(node: Node, source: &str) -> Option<(String, String)> {
    let trait_name = get_node_text(node.child_by_field_name("trait")?, source);
    if trait_name.rsplit("::").next()?.trim() != "Deref" {
        return None;
    }
    let owner = rust_type_name(node.child_by_field_name("type")?, source, 0)?;
    let body = node.child_by_field_name("body")?;
    let mut cursor = body.walk();
    let target = body
        .named_children(&mut cursor)
        .filter(|item| item.kind() == "type_item")
        .find(|item| get_child_text(*item, "name", source).as_deref() == Some("Target"))?;
    let target = rust_type_name(target.child_by_field_name("type")?, source, 0)?;
    Some((owner, target))
}

pub(super) fn rust_inline_module_depth(node: Node) -> usize {
    let mut depth = 0usize;
    let mut current = bounded_parent(node);
    while let Some(parent) = current {
        if parent.kind() == "mod_item" && parent.child_by_field_name("body").is_some() {
            depth += 1;
        }
        current = bounded_parent(parent);
    }
    depth
}

/// The module specifier a leaf's path denotes, with `super` resolved against
/// the inline-module nesting it was written inside.
///
/// `super` spent against an inline module does not leave the file, so once the
/// nesting is used up the target is this file — spelled `self`, which the
/// resolver reads as "the file this import is written in".
///
/// Known limit, stated rather than papered over: where the nesting absorbs every
/// `super` and segments remain (`mod tests { use super::helpers::thing; }`),
/// the remainder is emitted as `self::helpers`, which resolves to a sibling
/// *file* module. That is right when `helpers` is `mod helpers;` and wrong when
/// it is `mod helpers { … }` in this same file — and telling those apart needs
/// the file's own inline-module table, which this function does not have. The
/// wrong case resolves to nothing, which is where it already sat.
pub(super) fn rust_use_specifier(module: &[String], inline_depth: usize) -> String {
    let leading_super = module
        .iter()
        .take_while(|segment| *segment == "super")
        .count();
    let spent = leading_super.min(inline_depth);
    let remaining = leading_super - spent;
    let mut segments: Vec<&str> = Vec::with_capacity(module.len());
    if leading_super > 0 && remaining == 0 {
        segments.push("self");
    } else {
        segments.extend(std::iter::repeat_n("super", remaining));
    }
    segments.extend(module[leading_super..].iter().map(String::as_str));
    segments.join("::")
}

/// Read a `use_declaration` into one [`ExtractedImport`] per module it names.
///
/// The arm this replaces stored the statement's own text as the module
/// specifier: `use tree_sitter::{Language, Node, Parser};` became one import
/// whose module was the literal string `"tree_sitter::{Language, Node,
/// Parser}"`, with an empty `imported_names`. Both fields are what every
/// consumer of an import reads, so nothing downstream worked at all — measured
/// on this repository, no `.rs` file produced a single `Imports` edge and
/// `UnresolvedClass::External` never fired once for Rust, while the same ladder
/// produced 8,734 External rows for Python from the same evidence shape.
///
/// A glob is emitted as the `.` alias rather than as a name. That spelling
/// already exists for Go's dot-import and means exactly this — bind everything
/// this module exports — so the resolver needs no second rung for it.
///
/// A `pub use` is additionally an **export**, and emitting nothing for it was a
/// hole the size of the standard Rust library layout. `lib.rs` republishes the
/// crate surface and every consumer imports through the crate root, so the
/// import binding pointed at `lib.rs`, `lib.rs` declares none of those names,
/// and the ladder fell through to the bare-name global lookup — the exact
/// failure `compute_reexport_chains` was built to stop, with no Rust input to
/// feed it. Measured on this repository: `devmap-store/src/lib.rs` carries
/// `pub use schema::*;`, and `CURRENT_SCHEMA_VERSION` had six callers, all in
/// the one file naming `crate::schema::` directly. The migration-ladder test
/// that guards it had no edge to the schema at all.
///
/// Only a bare `pub` republishes across the crate boundary. A private `use`
/// binds the name for this file, and `pub(crate)` / `pub(super)` / `pub(in …)`
/// stop short of the surface a consumer can import, so recording a chain
/// through any of them would name a route the language does not have.
pub(super) fn rust_use_imports(
    node: Node,
    source: &str,
    span: Span,
    imports: &mut Vec<ExtractedImport>,
    exports: &mut Vec<ExtractedExport>,
) -> Option<()> {
    let raw = get_node_text(node, source);
    let argument = node.child_by_field_name("argument")?;
    let inline_depth = rust_inline_module_depth(node);
    let mut leaves: Vec<RustUseLeaf> = Vec::new();
    collect_rust_use_leaves(argument, source, &[], 0, &mut leaves);

    // Grouped by module so `use std::{fmt, io}` is one import of `std` naming
    // two symbols, the shape every other language's extractor produces. Ordered
    // by module for determinism (R4); names keep source order within a module.
    let mut named: BTreeMap<String, (Vec<String>, Vec<String>)> = BTreeMap::new();
    let mut whole_module: BTreeMap<String, Option<String>> = BTreeMap::new();
    for leaf in leaves {
        // `use serde;` and `use super::{self, thing};` import the module
        // itself, not a name out of it.
        let names_the_module = leaf.module.is_empty() || leaf.name.as_deref() == Some("self");
        if names_the_module {
            let mut module = leaf.module.clone();
            if leaf.name.as_deref() != Some("self") {
                if let Some(name) = leaf.name.clone() {
                    module.push(name);
                }
            }
            if module.is_empty() {
                continue;
            }
            whole_module.insert(rust_use_specifier(&module, inline_depth), leaf.alias);
            continue;
        }
        let specifier = rust_use_specifier(&leaf.module, inline_depth);
        match leaf.name {
            // A glob binds the module's whole surface, which is what the
            // resolver's `.` alias means.
            None => {
                whole_module.insert(specifier, Some(".".to_string()));
            }
            Some(name) => {
                let local = leaf.alias.unwrap_or_else(|| name.clone());
                let entry = named.entry(specifier).or_default();
                entry.0.push(name);
                entry.1.push(local);
            }
        }
    }

    // A `pub use` republishes what it names, so it is an export as well as an
    // import. Derived from the same two maps rather than from a second pass
    // over the leaves, so the two records cannot disagree about what the
    // statement said.
    if rust_use_is_public(node, source) {
        for (specifier, (imported_names, local_names)) in &named {
            for (imported, local) in imported_names.iter().zip(local_names.iter()) {
                // `pub use m::a as b;` publishes `b` and asks `m` for `a`,
                // which is the `exported_name` / `local_name` split the chain
                // builder already reads for `export { a as b } from './m'`.
                exports.push(ExtractedExport {
                    exported_name: local.clone(),
                    local_name: Some(imported.clone()),
                    module_specifier: Some(specifier.clone()),
                    span: span.clone(),
                });
            }
        }
        for (specifier, alias) in &whole_module {
            // Only the glob. `pub use m;` republishes the module itself, which
            // names no symbol for a chain to terminate on.
            if alias.as_deref() == Some(".") {
                exports.push(ExtractedExport {
                    exported_name: "*".to_string(),
                    local_name: None,
                    module_specifier: Some(specifier.clone()),
                    span: span.clone(),
                });
            }
        }
    }

    for (specifier, (imported_names, local_names)) in named {
        imports.push(ExtractedImport {
            raw_import: raw.clone(),
            module_specifier: specifier,
            imported_names,
            local_names,
            alias: None,
            span: span.clone(),
            path_load: None,
        });
    }
    for (specifier, alias) in whole_module {
        imports.push(ExtractedImport {
            raw_import: raw.clone(),
            module_specifier: specifier,
            imported_names: vec![],
            local_names: vec![],
            alias,
            span: span.clone(),
            path_load: None,
        });
    }
    Some(())
}

/// The callee an object-literal method is handed to, when the syntax hands it
/// to one.
///
/// `created.registerLinkProvider({ provideLinks() {…} })` gives the terminal
/// library an object and lets it call the member; there is no call site in this
/// corpus and there never will be, so `provideLinks` was reported dead at the
/// confident tier. Every callback interface has this shape — a comparator, a
/// visitor, an event map, a plugin — and the name lists that covered a handful
/// of them could not generalise.
///
/// **Where the walk stops is the whole of the precision.** Only an `object`,
/// an `array`, a `pair`, or a value-preserving TypeScript wrapper may sit
/// between the method and the argument list, because those carry the value
/// outward unchanged. Anything else — a function body, a statement, an
/// assignment — means the method is *not* part of what the call receives:
///
/// - `f({ m() {} })` and `f({ outer: { m() {} } })` hand `m` over.
/// - `const o = { m() {} }` does not: `o` is a local, and if nothing calls
///   `o.m()` then `m` really is dead. Reporting it is the correct answer and
///   this must not take it away.
/// - `f({ cb() { const o = { helper() {} }; } })` does not hand `helper` over
///   either, and the walk hits `cb`'s body and refuses before reaching the
///   argument list.
///
/// That asymmetry is deliberate: `liveness.rs` states throughout that an
/// over-inclusive exemption costs correctness while an omission costs only
/// noise, so the walk refuses on anything it does not positively recognise.
pub(super) fn js_object_literal_argument_callee(node: Node, source: &str) -> Option<String> {
    let mut current = bounded_parent(node)?;
    loop {
        match current.kind() {
            // Literal structure: the value travels outward unchanged.
            "object" | "array" | "pair" => {}
            // TypeScript wrappers that change the type and not the value.
            "as_expression" | "satisfies_expression" | "parenthesized_expression" => {}
            // The argument list: this call receives the object.
            "arguments" => {
                let call = bounded_parent(current)?;
                if !matches!(call.kind(), "call_expression" | "new_expression") {
                    return None;
                }
                let callee = call
                    .child_by_field_name("function")
                    .or_else(|| call.child_by_field_name("constructor"))?;
                let text = get_node_text(callee, source);
                return (!text.trim().is_empty()).then_some(text);
            }
            _ => return None,
        }
        current = bounded_parent(current)?;
    }
}

/// A Rust function's header — from the item's start to its body, or the whole
/// item for a bodiless signature — with whitespace collapsed to single spaces.
///
/// What the skeleton shows for the function, and what the resolver reads a
/// return type and a closure parameter's type out of (see
/// [`crate::rustsig`]): `fn read(conn: &Connection) -> Result<Self>`.
/// A header longer than [`crate::rustsig::MAX_SIGNATURE_BYTES`] is not
/// recorded, because a truncated one would read as a different header.
pub(super) fn rust_fn_signature(node: Node, source: &str) -> Option<String> {
    let end = node
        .child_by_field_name("body")
        .map_or(node.end_byte(), |body| body.start_byte());
    let header = source.get(node.start_byte()..end)?;
    let header = header.trim().trim_end_matches(';').trim_end();
    if header.is_empty() || header.len() > crate::rustsig::MAX_SIGNATURE_BYTES {
        return None;
    }
    Some(header.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Whether a `use_declaration` republishes beyond the crate that wrote it.
///
/// Exactly a bare `pub`. `pub(crate)`, `pub(super)` and `pub(in path)` are all
/// `visibility_modifier` nodes too, and all of them stop short of the surface
/// an outside consumer can import — so matching on the node kind alone would
/// publish names that cannot be reached, which is the same overreach as
/// publishing a private `use` one scope wider.
pub(super) fn rust_use_is_public(node: Node, source: &str) -> bool {
    let mut cursor = node.walk();
    let public = node.children(&mut cursor).any(|child| {
        child.kind() == "visibility_modifier" && get_node_text(child, source).trim() == "pub"
    });
    public
}

/// Whether the generic declaration arm binds a callable's parameters to their
/// declared types through [`param_type_bindings`].
///
/// The one predicate `extract_node` asks, so adding a language is an arm in
/// `param_type_bindings` and a line here, and never an edit to the arm that
/// asks. Rust and Go take their own arms and call `param_type_bindings`
/// directly; this is the answer for the languages the generic arm serves, which
/// is the C family today.
pub(super) fn binds_param_types_in_generic_arm(lang: &str) -> bool {
    is_c_family_grammar(lang)
}

/// Parameter name → declared type, for languages whose parameters carry one.
///
/// A parameter's declared type binds its name to that type just as firmly as a
/// constructor assignment does, and it is the *only* binding available to a
/// function operating on a value it did not construct. Without it,
/// `fn run(g: &English) { g.greet() }` produced no call edge at all: receiver
/// resolution had nothing to look up, so the call vanished from the graph
/// rather than merely being uncertain (SC12).
/// Parameter bindings as `(name, bare type, qualifying module)`.
///
/// The qualifier is carried separately rather than folded into the type name,
/// because the two answer different questions. The bare name is the dispatch
/// key — it is what `symbol_index` is keyed by, so `pkg.Widget` must still look
/// up as `Widget`. The qualifier is the *provenance*: `*testing.T` names a type
/// no indexed file declares, and `testing` is the import that proves it (SC25).
pub(super) fn param_type_bindings(
    node: Node,
    source: &str,
    lang: &str,
) -> Vec<(String, String, Option<String>)> {
    let Some(params) = parameter_list_of(node) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = params.walk();
    for child in params.named_children(&mut cursor) {
        let binding = match (lang, child.kind()) {
            ("rust", "parameter") => {
                let name = child
                    .child_by_field_name("pattern")
                    .map(|pattern| get_node_text(pattern, source));
                let ty_node = child.child_by_field_name("type");
                let ty = ty_node.and_then(|ty| rust_type_name(ty, source, 0));
                let qualifier = ty_node.and_then(|ty| rust_type_qualifier(ty, source, 0));
                name.zip(ty).map(|(name, ty)| (name, ty, qualifier))
            }
            ("go", "parameter_declaration") => {
                let name = get_child_text(child, "name", source);
                let ty_node = child.child_by_field_name("type");
                let ty = ty_node.and_then(|ty| go_type_name(ty, source, 0));
                let qualifier = ty_node.and_then(|ty| go_type_qualifier(ty, source, 0));
                name.zip(ty).map(|(name, ty)| (name, ty, qualifier))
            }
            // C / C++ / Objective-C / CUDA: parameters hang off the
            // `function_declarator`, and each `parameter_declaration` names
            // its binding through a (possibly pointer/reference-wrapped)
            // `declarator` field rather than a `name` field. Without this
            // arm, `int driver(S s) { return s.compute(1); }` leaves `s`
            // untyped, so the method call stays an uninferred receiver and
            // the out-of-line member looks dead.
            (lang, "parameter_declaration") if is_c_family_grammar(lang) => {
                let name = child
                    .child_by_field_name("declarator")
                    .and_then(|declarator| c_param_name(declarator, source, 0));
                let ty_node = child.child_by_field_name("type");
                let ty = ty_node.and_then(|ty| c_type_name(ty, source, 0));
                name.zip(ty).map(|(name, ty)| (name, ty, None))
            }
            _ => None,
        };
        if let Some((name, ty, qualifier)) = binding {
            if !name.is_empty() && !ty.is_empty() && name != "_" {
                out.push((name, ty, qualifier));
            }
        }
    }
    out
}

/// The `parameter_list` belonging to a callable node.
///
/// Rust and Go put `parameters` on the function/method node itself. The C
/// family puts it on the nested `function_declarator` (and that declarator
/// may itself be wrapped in pointer/reference declarators), so a direct
/// field lookup on the definition always answers `None`.
pub(super) fn parameter_list_of(node: Node<'_>) -> Option<Node<'_>> {
    if let Some(params) = node.child_by_field_name("parameters") {
        return Some(params);
    }
    let mut declarator = node.child_by_field_name("declarator")?;
    for _ in 0..16 {
        if let Some(params) = declarator.child_by_field_name("parameters") {
            return Some(params);
        }
        declarator = declarator.child_by_field_name("declarator")?;
    }
    None
}

/// Parameter binding name under a C-family declarator, unwrapping pointers
/// and references. Bounded so a pathological nesting cannot recurse forever.
pub(super) fn c_param_name(node: Node, source: &str, depth: usize) -> Option<String> {
    if depth > 16 {
        return None;
    }
    match node.kind() {
        "identifier" => {
            let text = get_node_text(node, source);
            (!text.is_empty()).then_some(text)
        }
        "pointer_declarator"
        | "reference_declarator"
        | "array_declarator"
        | "parenthesized_declarator"
        | "function_declarator"
        | "attributed_declarator" => {
            if let Some(inner) = node.child_by_field_name("declarator") {
                return c_param_name(inner, source, depth + 1);
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if let Some(name) = c_param_name(child, source, depth + 1) {
                    return Some(name);
                }
            }
            None
        }
        _ => None,
    }
}

/// Bare type name for a C-family parameter type, unwrapping pointers and
/// references so `S *` / `S&` / `const S` dispatch on `S`.
pub(super) fn c_type_name(node: Node, source: &str, depth: usize) -> Option<String> {
    if depth > 16 {
        return None;
    }
    match node.kind() {
        "type_identifier" | "identifier" | "primitive_type" | "sized_type_specifier" => {
            let text = get_node_text(node, source);
            (!text.is_empty()).then_some(text)
        }
        "type_descriptor" | "qualified_identifier" | "scoped_type_identifier" => {
            if let Some(name) = get_child_text(node, "name", source).filter(|n| !n.is_empty()) {
                return Some(name);
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if let Some(name) = c_type_name(child, source, depth + 1) {
                    return Some(name);
                }
            }
            None
        }
        "pointer_declarator" | "reference_declarator" | "abstract_pointer_declarator" => node
            .child_by_field_name("declarator")
            .and_then(|inner| c_type_name(inner, source, depth + 1))
            .or_else(|| {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if let Some(name) = c_type_name(child, source, depth + 1) {
                        return Some(name);
                    }
                }
                None
            }),
        // The C++ half of the deref-transparent rule `crate::deref`
        // states. `std::shared_ptr<Service> s; s->compute();` forwards
        // through `operator->` to `Service::compute`, so the receiver's nominal
        // type is the argument, not the wrapper — and without this arm the `_`
        // fallback below finds the `name` field first and answers
        // `shared_ptr`, leaving every out-of-line member reachable only through
        // a smart pointer looking dead. That is the same defect the
        // `parameter_declaration` arm above was added to fix for plain values.
        //
        // `weak_ptr` is deliberately absent: it has no `operator->`, and a
        // caller must `.lock()` it into a `shared_ptr` first, so its methods
        // really are `weak_ptr`'s own.
        "template_type" => {
            let outer = node.child_by_field_name("name");
            let unwrapped = outer
                .map(|name| get_node_text(name, source))
                .map(|text| text.rsplit("::").next().unwrap_or(&text).trim().to_string())
                .filter(|bare| crate::deref::CPP_DEREF_TRANSPARENT.contains(&bare.as_str()))
                .and_then(|_| node.child_by_field_name("arguments"))
                .and_then(|args| {
                    let mut cursor = args.walk();
                    let named: Vec<Node> = args.named_children(&mut cursor).collect();
                    match named.as_slice() {
                        [only] => c_type_name(*only, source, depth + 1),
                        _ => None,
                    }
                });
            // Falls back to the wrapper's own name, so a template this rule
            // does not cover answers exactly what it answered before.
            unwrapped.or_else(|| outer.and_then(|name| c_type_name(name, source, depth + 1)))
        }
        _ => {
            // `const S` / elaborated types may put the identifier deeper.
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if let Some(name) = c_type_name(child, source, depth + 1) {
                    return Some(name);
                }
            }
            None
        }
    }
}

/// The package a Go type is qualified by, unwrapping the type constructors
/// around it: `[]*testing.T` is qualified by `testing`.
///
/// Separate from `go_type_name` for the SC17 reason — that function answers
/// "what type is this value, for dispatch", and must keep returning the bare
/// name. Bounded depth so a pathological nesting cannot recurse without end.
pub(crate) fn go_type_qualifier(node: Node, source: &str, depth: usize) -> Option<String> {
    if depth > 16 {
        return None;
    }
    match node.kind() {
        "qualified_type" => get_child_text(node, "package", source).filter(|p| !p.is_empty()),
        "pointer_type" => node
            .named_child(0)
            .and_then(|inner| go_type_qualifier(inner, source, depth + 1)),
        "slice_type" | "array_type" => node
            .child_by_field_name("element")
            .and_then(|inner| go_type_qualifier(inner, source, depth + 1)),
        "map_type" => node
            .child_by_field_name("value")
            .and_then(|inner| go_type_qualifier(inner, source, depth + 1)),
        "generic_type" => node
            .child_by_field_name("type")
            .and_then(|inner| go_type_qualifier(inner, source, depth + 1)),
        _ => None,
    }
}

/// The crate or module root a Rust type path is rooted at: `&reqwest::Client`
/// is qualified by `reqwest`.
///
/// Takes the *first* segment because that is what a `use` binds and therefore
/// what an import can be matched against. `crate::`, `self::` and `super::`
/// roots are returned as written and simply never match an external import,
/// which is the correct outcome for an in-corpus path.
pub(super) fn rust_type_qualifier(node: Node, source: &str, depth: usize) -> Option<String> {
    if depth > 16 {
        return None;
    }
    match node.kind() {
        "reference_type" | "pointer_type" => node
            .child_by_field_name("type")
            .and_then(|inner| rust_type_qualifier(inner, source, depth + 1)),
        // Reads [`rust_generic_identity_node`] rather than the `type` field, so
        // the qualifier describes whichever node gave the *name*. Taking the
        // wrapper's qualifier while the name came from the argument is how
        // `std::sync::Arc<Service>` would come to claim that `Service` lives in
        // `std`.
        "generic_type" => rust_generic_identity_node(node, source)
            .and_then(|inner| rust_type_qualifier(inner, source, depth + 1)),
        "scoped_type_identifier" => {
            let text = get_node_text(node, source);
            let root = text.split("::").next().unwrap_or_default().trim();
            (!root.is_empty()).then(|| root.to_string())
        }
        _ => None,
    }
}

/// The bare name of a Go type, unwrapping the pointers around it.
///
/// Bounded depth for the same reason as `go_type_qualifier` above, and measured
/// rather than assumed: `func (r **…*T) M() {}` at 10,000 levels — a ~10 KB
/// file, four orders of magnitude under `MAX_SOURCE_BYTES` — overflowed the
/// stack and aborted `devmap build` with exit 134. The tree walk itself is an
/// explicit worklist and never recurses, so this function and `rust_type_name`
/// were the whole of that overflow. Past the bound the receiver is not
/// recovered and the method is named `file::M` rather than `file::T.M` — a lost
/// qualification, never a guessed one.
pub(super) fn go_type_name(node: Node, source: &str, depth: usize) -> Option<String> {
    if depth > 16 {
        return None;
    }
    match node.kind() {
        "pointer_type" => {
            let mut pointer_cursor = node.walk();
            for child in node.named_children(&mut pointer_cursor) {
                if let Some(name) = go_type_name(child, source, depth + 1) {
                    return Some(name);
                }
            }
            None
        }
        "type_identifier" | "identifier" | "field_identifier" => {
            let text = get_node_text(node, source);
            (!text.is_empty()).then_some(text)
        }
        "qualified_type" => get_child_text(node, "name", source).or_else(|| {
            let text = get_node_text(node, source);
            text.rsplit('.').next().map(str::to_string)
        }),
        // `Box[T]`, `*Box[T]`, `pkg.Box[int]`: the type arguments are not part
        // of the type's name, so a method declared on `(b *Box[T])` is `Box`'s
        // and a value typed `Box[int]` is a `Box`.
        "generic_type" => node
            .child_by_field_name("type")
            .and_then(|inner| go_type_name(inner, source, depth + 1)),
        _ => None,
    }
}

/// The dotted package a Kotlin or Java file declares, if it declares one.
pub(super) fn jvm_package_name(root: Node, source: &str) -> Option<String> {
    let mut cursor = root.walk();
    let header = root
        .named_children(&mut cursor)
        .find(|child| matches!(child.kind(), "package_header" | "package_declaration"))?;
    let name = header.named_children(&mut header.walk()).find(|child| {
        matches!(
            child.kind(),
            "qualified_identifier" | "scoped_identifier" | "identifier"
        )
    })?;
    let text: String = get_node_text(name, source)
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect();
    (!text.is_empty()).then_some(text)
}

pub(super) fn go_package_name(root: Node, source: &str) -> Option<String> {
    let mut package_cursor = root.walk();
    let mut identifier_cursor = root.walk();
    for child in root.named_children(&mut package_cursor) {
        if child.kind() != "package_clause" {
            continue;
        }
        let name = get_child_text(child, "name", source).or_else(|| {
            child
                .named_children(&mut identifier_cursor)
                .find(|node| node.kind() == "package_identifier")
                .map(|node| get_node_text(node, source))
        })?;
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

/// Every `GOOS` value Go recognises, from `go/build/syslist.go`.
///
/// A filename ending `_<GOOS>.go` carries an implicit build constraint even
/// with no `//go:build` line, so the list is part of the constraint test rather
/// than decoration. Held as a sorted constant so membership is a fact about the
/// toolchain rather than a guess about what an OS name looks like — a
/// name-shaped heuristic would read `_other.go` and `_test.go` as constraints.
pub(super) const GO_OS_VALUES: &[&str] = &[
    "aix",
    "android",
    "darwin",
    "dragonfly",
    "freebsd",
    "hurd",
    "illumos",
    "ios",
    "js",
    "linux",
    "nacl",
    "netbsd",
    "openbsd",
    "plan9",
    "solaris",
    "wasip1",
    "windows",
    "zos",
];

/// Every `GOARCH` value Go recognises, from `go/build/syslist.go`.
pub(super) const GO_ARCH_VALUES: &[&str] = &[
    "386",
    "amd64",
    "amd64p32",
    "arm",
    "arm64",
    "arm64be",
    "armbe",
    "loong64",
    "mips",
    "mips64",
    "mips64le",
    "mips64p32",
    "mips64p32le",
    "mipsle",
    "ppc",
    "ppc64",
    "ppc64le",
    "riscv",
    "riscv64",
    "s390",
    "s390x",
    "sparc",
    "sparc64",
    "wasm",
];

/// Whether a Go file is excluded from some builds — by a `//go:build` or
/// `// +build` directive, or by an implicit `_GOOS`/`_GOARCH` filename suffix.
///
/// Both halves are needed and neither subsumes the other. `procgroup_other.go`
/// has no recognisable suffix and is constrained only by `//go:build !unix`;
/// a `foo_windows.go` with no directive is constrained only by its name.
///
/// The directive scan stops at the package clause because that is where Go
/// stops looking: a `//go:build` line below it is an ordinary comment, and
/// treating it as a constraint would let a comment anywhere in a file suppress
/// a real finding.
pub(super) fn go_build_constrained(path: &str, source: &str) -> bool {
    go_has_build_directive(source) || go_filename_is_constrained(path)
}

pub(super) fn go_has_build_directive(source: &str) -> bool {
    for line in source.lines() {
        let trimmed = line.trim();
        // The header ends at the package clause; nothing below it constrains
        // the build.
        if trimmed.starts_with("package ") || trimmed == "package" {
            return false;
        }
        if trimmed.starts_with("//go:build") {
            return true;
        }
        // The legacy form is `// +build`, with the space required.
        if let Some(rest) = trimmed.strip_prefix("//") {
            if rest.trim_start().starts_with("+build") {
                return true;
            }
        }
    }
    false
}

/// Whether the `_GOOS` / `_GOARCH` / `_GOOS_GOARCH` filename suffix applies.
///
/// `_test` is stripped first: Go reads `foo_linux_test.go` as the linux-only
/// test file for `foo`, so the constraint sits one component further left.
pub(super) fn go_filename_is_constrained(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    let Some(stem) = file.strip_suffix(".go") else {
        return false;
    };
    let stem = stem.strip_suffix("_test").unwrap_or(stem);
    let mut parts = stem.split('_').rev();
    let Some(last) = parts.next() else {
        return false;
    };
    // Go only honours the suffix when something precedes it: a file *named*
    // `linux.go` is not constrained, `net_linux.go` is.
    let has_prefix = parts.clone().next().is_some();
    if !has_prefix {
        return false;
    }
    if GO_OS_VALUES.contains(&last) {
        return true;
    }
    if GO_ARCH_VALUES.contains(&last) {
        // `_GOARCH` alone constrains; `_GOOS_GOARCH` does too, and the OS half
        // is only meaningful when it really is an OS.
        return true;
    }
    false
}
