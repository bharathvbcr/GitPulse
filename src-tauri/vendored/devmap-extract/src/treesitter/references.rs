use super::*;

pub(super) fn maybe_push_name_reference(
    node: Node,
    source: &str,
    lang: &str,
    file_symbol_name: &str,
    references: &mut Vec<ExtractedReference>,
) {
    let mut ref_kind = match node.kind() {
        // `namespace_identifier` is how tree-sitter-cpp spells the scope of
        // `Plain::K`: a use of `Plain`, which a struct mentioned no other way
        // — `MmaTiles::kThreads`, `MmaTiles::BR` — had no reference for at
        // all. A `namespace ns { … }` head is on the definition's `name`
        // field, which `is_defining_name` below already refuses.
        "type_identifier" | "nested_type_identifier" | "namespace_identifier" => {
            ReferenceKind::Type
        }
        "identifier"
        | "shorthand_property_identifier"
        | "property_identifier"
        | "field_identifier"
        // tree-sitter-swift spells every value-position identifier
        // `simple_identifier`, so without this arm Swift recorded no name
        // references at all — only type positions, which are
        // `type_identifier`. A Swift type or function used as a *value* was
        // therefore invisible: `Const.firebaseTimeoutMs`,
        // `self[SlideUpDismissKey.self]`, `fields.map(csvField)` and
        // `AXObserverCreate(pid, selectionObserverCallback, …)` all name a
        // symbol the graph had no edge for, and each of those symbols was
        // reported dead at 0.9 confidence on real code once Swift visibility
        // became readable. The existing gates still apply: a declaration site
        // (`is_defining_name`), a callee position (`is_call_callee`) and a name
        // shadowed by a local binding are all still refused.
        | "simple_identifier" => ReferenceKind::Name,
        _ => return,
    };
    // Python annotations and `isinstance`/`issubclass` type arguments are
    // ordinary `identifier` nodes. Promote them to Type so resolve can see the
    // class the same way it sees a Rust/Go type annotation.
    if ref_kind == ReferenceKind::Name
        && lang == "python"
        && (python_identifier_is_type_annotation(node)
            || python_isinstance_type_argument(node, source))
    {
        ref_kind = ReferenceKind::Type;
    }
    if is_argument_label(node)
        || name_field_holds_a_use(node)
        || is_defining_name(node)
        || is_inside_import_or_export(node)
        || is_call_callee(node)
        || rust_macro_member_name(node, lang)
        // Last: a declaration's own name is refused above and never pays for
        // the extra ancestor walk.
        || (lang == "swift"
            && swift_leading_dot_member(node, source)
            && swift_leading_dot_context(node, source).is_none())
    {
        return;
    }
    let name = get_node_text(node, source);
    if !is_user_ident(&name) {
        return;
    }
    // X40. `_` in **type position** is the inferred-type placeholder — Rust's
    // `row.get::<_, f64>(1)`, Go's blank identifier — and it references
    // nothing, so there is no attribution to attempt and no honest tier to file
    // the failure under. Measured on this repository: 275 of the 9,790 rows in
    // the tier documented as "the only tier that indicates a defect" were this
    // placeholder, every one of them a turbofish.
    //
    // Restricted to type position on purpose. `_` is a perfectly ordinary
    // value-position identifier in JavaScript (lodash) and Python (gettext), so
    // refusing it everywhere would drop real references; no language names a
    // *type* `_`.
    if ref_kind == ReferenceKind::Type && name.chars().all(|character| character == '_') {
        return;
    }
    if ref_kind == ReferenceKind::Name && name_is_shadowed_by_local(node, source, &name) {
        return;
    }
    // A Go struct literal's key names a field of that struct, not a symbol.
    // Gated here beside the other "this spelling is not a reference" rules
    // rather than in the resolver: the resolver sees a bare identifier with no
    // receiver and has nothing left to distinguish it with.
    if ref_kind == ReferenceKind::Name && lang == "go" && go_struct_literal_field_key(node, source)
    {
        return;
    }
    references.push(ExtractedReference {
        name,
        kind: ref_kind,
        span: node_span(node),
        enclosing_symbol: enclosing_emitted_symbol_for(node, source, lang, file_symbol_name),
        assigned_to: rust_let_bound_from_field_use(node, source)
            .or_else(|| rust_let_bound_from_type_annotation(node, source))
            .or_else(|| {
                (ref_kind == ReferenceKind::Type)
                    .then(|| {
                        swift_parameter_bound_from_type(node, source)
                            .or_else(|| ts_parameter_bound_from_type(node, source))
                            .or_else(|| go_var_bound_from_type(node, source))
                            .or_else(|| python_parameter_bound_from_type(node, source))
                    })
                    .flatten()
            }),
        // The object half of a member access, so the resolver can tell
        // `cfg.enabled` from a bare local named `enabled`.
        receiver_expr: member_access_receiver(node, source),
    });
}

/// Whether `node` is the member name of a `recv.name` written inside a macro's
/// token tree.
///
/// A macro argument is an unstructured `token_tree`, so `assert_eq!(g.level(), 1)`
/// reaches here as the loose tokens `g` `.` `level` and a `( )` group. The
/// identifier after the dot names something `g` owns, never a file-level item,
/// yet as a bare `Name` it bound to the free `fn level` beside it (`References`,
/// 1.0, `SameFile`) and kept that function alive. The method call itself is
/// recovered, with its receiver, by `rust_macro_calls`; a field read has no
/// graph node to reference.
pub(super) fn rust_macro_member_name(node: Node, lang: &str) -> bool {
    lang == "rust"
        && node.kind() == "identifier"
        && bounded_parent(node).is_some_and(|parent| parent.kind() == "token_tree")
        && node.prev_sibling().is_some_and(|prev| prev.kind() == ".")
}

/// Grammar keys with a language-specific arm in `extract_node`.
///
/// Everything else reaches the generic arm, whose declaration identity comes
/// from `langdecl` — so a reference made inside one of those languages must be
/// attributed through `langdecl` too, or it names a symbol the emitter never
/// produced. Measured: with Kotlin extension functions owned by their receiver,
/// `enclosing_callable_qualified` still answered `Main.kt::extra` for a
/// reference inside `fun Person.extra()`, where the emitted symbol is
/// `Main.kt::Person.extra` — an orphaned edge of exactly the SC9/SC10 shape.
///
/// Not trusted: `every_language_attributes_its_references_to_an_emitted_symbol`
/// in `tests/declarations.rs` runs a declaration-plus-use snippet through every
/// language named here **and** through the languages served by the generic arm,
/// and fails if any reference names a symbol the emitter did not produce.
/// Demonstrated in both directions rather than asserted: adding `"kotlin"` here
/// makes a reference inside `fun Person.extra()` report `a.kt::extra`, and
/// removing `"rust"` makes a reference inside an `impl` block report
/// `a.rs::outer` where the emitter said `a.rs::S.outer`. Both are orphans.
pub(super) const SPECIALISED_ARM_LANGUAGES: &[&str] = &[
    "go",
    "hcl",
    "javascript",
    "python",
    "rust",
    "tsx",
    "typescript",
];

/// The enclosing symbol a reference belongs to, asked of whichever path emitted
/// the enclosing declaration.
pub(super) fn enclosing_emitted_symbol_for(
    node: Node,
    source: &str,
    lang: &str,
    file_symbol_name: &str,
) -> Option<String> {
    if SPECIALISED_ARM_LANGUAGES.contains(&lang) {
        return enclosing_callable_qualified(node, source, file_symbol_name);
    }
    crate::langcalls::scope::enclosing_emitted_symbol(node, source, lang, file_symbol_name)
}

/// Whether `node` is the name an R assignment binds.
///
/// `<-`, `<<-` and `=` bind their right operand to their left; `->` and `->>`
/// bind the other way. Every other `binary_operator` — arithmetic, comparison,
/// a pipe — has operands that are uses, and this must not suppress them.
pub(super) fn is_r_binding_target(node: Node) -> bool {
    let Some(parent) = bounded_parent(node) else {
        return false;
    };
    if parent.kind() != "binary_operator" {
        return false;
    }
    let Some(operator) = parent.child_by_field_name("operator") else {
        return false;
    };
    let operator = operator.kind();
    match operator {
        "<-" | "<<-" | "=" => field_contains(parent, "lhs", node),
        "->" | "->>" => field_contains(parent, "rhs", node),
        _ => false,
    }
}

pub(crate) fn is_user_ident(name: &str) -> bool {
    let Some(first) = name.chars().next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_' || first == '$')
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
}

/// Whether `needle` lies inside the first child of `parent` on `field`.
///
/// Deliberately the *first*, not all of them. Several grammars put more than one
/// child on `name`, and they do not agree on what the extra ones are:
/// tree-sitter-dart's `constructor_signature` puts the class, a `.` and the
/// constructor name there, all of them declaration sites, while
/// tree-sitter-swift's `parameter` puts the parameter name **and its type**
/// there. Widening this to every `name` child was measured and reverted: it
/// suppressed 796 real type references across 342 `.swift` files, because every
/// parameter type stopped counting as a use. The grammars that genuinely need
/// the wider rule get a named clause in `is_defining_name` instead.
pub(super) fn field_contains(parent: Node, field: &str, needle: Node) -> bool {
    parent
        .child_by_field_name(field)
        .is_some_and(|haystack| node_contains(haystack, needle))
}

/// Whether `needle` is `haystack` or one of its descendants.
///
/// An explicit stack, not a recursion: the haystack is a whole field — a
/// `let`'s initializer, an assignment's left side — and one frame per level
/// let `let g = ((((g()))))` 20,000 deep abort the process on a 256 KiB stack.
/// Only children whose bytes enclose the needle's are entered, since a node's
/// span encloses every descendant's; that leaves the one path down plus the
/// zero-width nodes at its edges, rather than the whole field.
pub(super) fn node_contains(haystack: Node, needle: Node) -> bool {
    let (start, end) = (needle.start_byte(), needle.end_byte());
    let mut stack = vec![haystack];
    while let Some(node) = stack.pop() {
        if node.id() == needle.id() {
            return true;
        }
        let mut cursor = node.walk();
        stack.extend(
            node.children(&mut cursor)
                .filter(|child| child.start_byte() <= start && end <= child.end_byte()),
        );
    }
    false
}

/// The declaration a C-family `declarator` chain binds, when `node` is the
/// identifier it names.
///
/// C, C++, Objective-C, CUDA and Metal do not put a `name` field on a
/// declaration: the name hangs off a `declarator` chain
/// (`function_definition -> function_declarator -> identifier`). The
/// `name`-field tests in `is_defining_name` therefore never recognised a
/// C-family declaration's own identifier, so every declaration emitted a
/// `Name` reference to itself, `analyze_liveness` counted that self-reference
/// as a use, and no C-family symbol could ever be reported dead.
///
/// The walk follows `declarator` *identity* links, never `field_contains`
/// subtree containment: `function_declarator` holds the signature under
/// `parameters` and `array_declarator` holds its bound under `size`, so a
/// containment test would also swallow parameter types and array bounds, which
/// are genuine references. Returns the outermost node of the chain — the
/// declaration itself — so callers can tell a function definition from a
/// variable or parameter binding.
/// Whether `parent` is a C++ template wrapper whose `name` field is `node`.
///
/// `template_function` (`f<T>` as a callee, or an explicit specialization's
/// declarator), `template_method` (`obj.f<T>`) and `template_type`
/// (`Tiles<4>`) all put the name they wrap on a `name` field, and none of
/// them binds anything: the wrapper is transparent, and whether the name
/// declares or uses is decided one level up. The generic "a `name` field is a
/// binding" rule in [`is_defining_name`] read the wrapper itself as the
/// declaration, so every template *call* filed its callee as a local of the
/// calling function — the resolver then classified the site `local_binding`
/// and the helper was published confidently dead. Measured on MLSystemsLab:
/// every `__device__` helper and every Metal `inline` helper reached through
/// `f<T>(…)`, and every template struct whose only mentions were
/// instantiations.
pub(super) fn is_template_wrapper_name(parent: Node, node: Node) -> bool {
    matches!(
        parent.kind(),
        "template_function" | "template_method" | "template_type"
    ) && parent
        .child_by_field_name("name")
        .is_some_and(|name| name.id() == node.id())
}

pub(super) fn c_declarator_declaration(node: Node) -> Option<Node> {
    let mut current = node;
    let mut climbed = false;
    while let Some(parent) = bounded_parent(current) {
        // `spec<float>` in `template <> void spec<float>(float *p) {…}`: the
        // declarator chain runs through the wrapper, and stopping at it would
        // leave the specialization's own name looking like a use of the
        // primary template — the self-reference shape
        // `c_family_declarations_do_not_reference_themselves` pins.
        if is_template_wrapper_name(parent, current) {
            current = parent;
            continue;
        }
        if parent
            .child_by_field_name("declarator")
            .is_none_or(|declarator| declarator.id() != current.id())
        {
            break;
        }
        climbed = true;
        current = parent;
    }
    climbed.then_some(current)
}

/// Whether `node` is the member of a Swift leading-dot expression: the
/// `minute` of `f(.minute)` or the `size` of `return .size`.
///
/// tree-sitter-swift parses `.minute` as a `prefix_expression` whose target is
/// the bare identifier, so it looked exactly like a bare read of `minute`. A
/// leading-dot member names a case or static member of the *contextual* type —
/// a type this extractor cannot see — so it is no reference to any declaration
/// that merely shares the spelling: a free function `minute`, or a property
/// `size` of the type the expression is written inside.
pub(super) fn swift_leading_dot_member(node: Node, source: &str) -> bool {
    let Some(parent) = bounded_parent(node) else {
        return false;
    };
    parent.kind() == "prefix_expression"
        && source
            .get(parent.start_byte()..node.start_byte())
            .is_some_and(|operator| operator.trim() == ".")
}

/// The type a Swift leading-dot expression is written against, when the source
/// states it: `Shape` for the `.circle(1)` of `func unit() -> Shape { .circle(1) }`
/// (an implicit or explicit `return`), of `var v: Shape = .circle(1)` and of
/// `var w: Shape { .square }`.
///
/// Anything else — an argument (`f(.minute)`, whose parameter type is the
/// callee's), an assignment, a `switch` pattern — has no contextual type this
/// extractor can read, and the member then names nothing. Only a plain
/// capitalised type name counts: an optional, an array or a generic names a
/// different thing than the member's owner.
pub(super) fn swift_leading_dot_context(node: Node, source: &str) -> Option<String> {
    let simple_type = |ty: Node| -> Option<String> {
        if ty.kind() != "user_type" || ty.named_child_count() != 1 {
            return None;
        }
        let name = ty
            .named_child(0)
            .filter(|n| n.kind() == "type_identifier")?;
        let name = get_node_text(name, source);
        // A generic parameter of the enclosing function or type stands for any
        // type: a real `struct T` elsewhere is not what `-> T` names there.
        if crate::swiftscope::is_generic_parameter(node, source, &name) {
            return None;
        }
        name.chars()
            .next()
            .is_some_and(char::is_uppercase)
            .then_some(name)
    };
    let mut current = bounded_parent(node)?;
    let mut returned = false;
    for _ in 0..8 {
        let parent = bounded_parent(current)?;
        match parent.kind() {
            // `.circle(1)`: the member is the callee, the call is the value.
            "call_expression"
                if parent
                    .named_child(0)
                    .is_some_and(|c| c.id() == current.id()) => {}
            "control_transfer_statement"
                if source
                    .get(parent.start_byte()..parent.end_byte())
                    .is_some_and(|text| text.trim_start().starts_with("return")) =>
            {
                returned = true;
            }
            // An implicit return is the body's last expression.
            "statements"
                if returned
                    || parent
                        .named_child(parent.named_child_count().checked_sub(1)?)
                        .is_some_and(|last| last.id() == current.id()) => {}
            "function_body" | "computed_property" | "computed_getter" => {}
            "function_declaration" => {
                let mut cursor = parent.walk();
                let returns = parent
                    .named_children(&mut cursor)
                    .find(|child| child.kind() == "user_type");
                return returns.and_then(simple_type);
            }
            "property_declaration" => {
                let mut cursor = parent.walk();
                let annotation = parent
                    .named_children(&mut cursor)
                    .find(|child| child.kind() == "type_annotation")?;
                return annotation.child_by_field_name("name").and_then(simple_type);
            }
            _ => return None,
        }
        current = parent;
    }
    None
}

/// Whether `node` is a call's argument label — the `x` of Python `f(x=…)`,
/// C# `F(x: …)` and `[A(x = …)]`, R and PHP `f(x = …)` / `f(x: …)`, Solidity
/// `f({x: …})`, Scala `T[x = …]`.
///
/// A label names a parameter of the *callee*: it neither binds anything where
/// it is written nor reads anything. These grammars put it on a `name` field,
/// so [`is_defining_name`] read it as a declaration and filed it as a local of
/// the enclosing scope — after which every read of a same-named symbol there
/// was dropped as shadowed. `register(handler=handler)` lost its only reference
/// to `handler`, and `dict(codegraph="CodeGraph")` at module level hid every
/// later module-level read of the constant `codegraph`, reported dead at 0.9.
/// Reference emission must refuse it too, or the label would become a
/// fabricated read of whatever shares its name.
pub(super) fn is_argument_label(node: Node) -> bool {
    bounded_parent(node).is_some_and(|parent| {
        // Swift's `f(count: 1)` wraps the label in its own node:
        // `value_argument > name: value_argument_label > simple_identifier`, so
        // the label is never on a `name` field of the identifier's own parent.
        parent.kind() == "value_argument_label"
            || (matches!(
                parent.kind(),
                "keyword_argument"
                    | "argument"
                    | "attribute_argument"
                    | "call_struct_argument"
                    | "named_type_argument"
            ) && parent
                .child_by_field_name("name")
                .is_some_and(|label| label.id() == node.id()))
    })
}

/// Whether `node` is the `name` field of a node kind that spells a *use* there.
///
/// The generic rule in [`is_defining_name`] reads any identifier on a `name`
/// field as a declaration, and most grammars agree. These do not — enumerated
/// from every vendored grammar's `node-types.json` (each kind whose `name`
/// field admits an identifier), then classified by hand:
///
/// - JSX `jsx_opening_element` / `jsx_closing_element` /
///   `jsx_self_closing_element`: `<HomePage …/>` names the component it
///   renders. Read as a declaration, the tag made `HomePage` a local of every
///   component that rendered it, and the resolver then refused the tag's own
///   call as "a local binding whose value is not known" — so an imported
///   component rendered inside another component had no caller at all.
///   Measured on ScholarLM: `HomePage`, `ResultsPage`, `UserModeProvider` and
///   3,686 TSX sites in all were filed `local_binding`.
/// - C# `member_access_expression` / `member_binding_expression`:
///   `other.Format` / `x?.Format` read a member; as a declaration it shadowed a
///   bare `Format(…)` call later in the same method.
/// - Java/Dart `annotation` / `marker_annotation`, C-family and C# `attribute`,
///   C `macro_type_specifier`, and the preprocessor's `#ifdef` / `#elifdef` /
///   `#undef`: each names something declared elsewhere.
///
/// Left alone, deliberately: C# `qualified_name` / `alias_qualified_name`
/// (also the spelling of a `namespace A.B` declaration, and only ever in type
/// position inside a body, where it cannot shadow a value call), and C++
/// `qualified_identifier` (the spelling of an out-of-line definition, which
/// [`c_declarator_declaration`] owns).
///
/// Reference emission refuses these positions too, which keeps it exactly as
/// it was: a JSX tag is already a `JsxTag` reference plus a call, and a C#
/// member name carries no receiver here, so a bare `Name` for it could bind to
/// an unrelated symbol of the same name.
pub(super) fn name_field_holds_a_use(node: Node) -> bool {
    bounded_parent(node).is_some_and(|parent| {
        matches!(
            parent.kind(),
            "jsx_opening_element"
                | "jsx_closing_element"
                | "jsx_self_closing_element"
                | "member_access_expression"
                | "member_binding_expression"
                | "annotation"
                | "marker_annotation"
                | "attribute"
                | "macro_type_specifier"
                | "preproc_ifdef"
                | "preproc_elifdef"
                | "preproc_undef"
        ) && field_contains(parent, "name", node)
    })
}

pub(super) fn is_defining_name(node: Node) -> bool {
    if is_argument_label(node) || name_field_holds_a_use(node) {
        return false;
    }
    // A grammar's `name` field is not necessarily a declaration. Java/Lua
    // invocation nodes use it for the callee; recording that use as a local
    // binding suppresses the very call it names (RA1).
    if bounded_parent(node).is_some_and(|parent| {
        matches!(
            parent.kind(),
            "method_invocation"
                | "function_call"
                | "call"
                | "call_expression"
                | "invocation_expression"
        )
    }) {
        return false;
    }
    if c_declarator_declaration(node).is_some() || is_objc_declaring_identifier(node) {
        return true;
    }
    // `struct render_state *state` names the type in the same `name` field that
    // `struct render_state { … }` uses, so the generic test below calls both a
    // definition and suppresses the reference. Only the form with a body
    // defines; the other is a use, and it is frequently the *only* use, because
    // C code that never typedefs a struct mentions it exclusively this way.
    //
    // The mirror of the rule in `c_family_declaration`, which is why they are
    // stated in the same terms: a bodyless specifier neither declares a symbol
    // nor suppresses a reference.
    if is_bodyless_type_specifier_name(node) {
        return false;
    }
    // A Kotlin `enum_entry` names its constant in an unlabelled `identifier`
    // child, so the `name`-field test below cannot see it. Once enum entries
    // became symbols, the declaration site was being recorded as a *use* of the
    // symbol it declares — `Main.kt::Mode.FAST` referencing `Main.kt::Mode.FAST`
    // — which is the self-reference shape
    // `c_family_declarations_do_not_reference_themselves` already pins for C.
    if bounded_parent(node).is_some_and(|parent| parent.kind() == "enum_entry") {
        return true;
    }
    // Kotlin names a local (`val clock = …`, `for (item in …)`) in the first
    // identifier of a `variable_declaration`, and a function parameter in the
    // first identifier of a `parameter`; neither carries a `name` field, so the
    // field test below cannot see them and a local was never a local.
    if bounded_parent(node).is_some_and(|parent| {
        matches!(parent.kind(), "variable_declaration" | "parameter")
            && parent
                .named_child(0)
                .is_some_and(|first| first.id() == node.id())
    }) {
        return true;
    }
    // R spells every function declaration as an assignment — `helper <-
    // function(a) …` — so the name being declared sits on `lhs` of a
    // `binary_operator`, a node kind that also covers `a + 1`. Gated on the
    // operator actually binding, so an arithmetic operand is never suppressed.
    // Without it a four-function R file recorded four file-scoped references to
    // its own declarations, and every uncalled R function looked used.
    if is_r_binding_target(node) {
        return true;
    }
    // tree-sitter-dart spells a named constructor `Widget.named` as three
    // children on the `name` field — `Widget`, `.`, `named` — so the generic
    // first-child test below sees only `Widget` and reported the constructor's
    // own name as a *use* of the symbol it declares:
    // `app.dart::Widget.named` referencing `app.dart::Widget.named`, the
    // self-reference shape `c_family_declarations_do_not_reference_themselves`
    // pins for C. A parameter's identifiers hang off `formal_parameter_list`
    // rather than off the signature, so this reaches only the name.
    if bounded_parent(node).is_some_and(|parent| parent.kind() == "constructor_signature") {
        return true;
    }
    let mut current = node;
    loop {
        let Some(parent) = bounded_parent(current) else {
            return false;
        };
        // A path segment is a use, not a declaration. `Self::stamp_rule` as a
        // value (`.map(Self::stamp_rule)`) puts `stamp_rule` on the `name`
        // field of `scoped_identifier`; treating that as a binding suppressed
        // the only reference the function had.
        if matches!(
            parent.kind(),
            "scoped_identifier" | "scoped_type_identifier"
        ) {
            return false;
        }
        // `Foo<T>` writes the instantiated type on the `name` field of
        // `generic_type`. That is a use: `export type Contract = AssertTrue<…>`
        // is the only mention of `AssertTrue` besides its alias, and treating
        // it as a binding suppresses the Type reference, so a compile-time
        // contract looks confidently dead.
        if parent.kind() == "generic_type" {
            return false;
        }
        // Rust's struct literal writes the type it constructs on the `name`
        // field of `struct_expression`. That is a use: a serde record built
        // only as `to_string(&FileRecord { .. })` has no other mention, and
        // reading it as a binding suppressed the Type reference, so the type
        // was reported dead at 0.9. `Self { .. }` is the same position, and
        // the resolver reads a Rust `Self` type reference as the enclosing
        // impl's type. The arm's `Constructor` reference cannot stand in: it
        // carries the `let` binding only, and the resolver skips that kind as
        // the duplicate of a call no struct literal has.
        if parent.kind() == "struct_expression" && field_contains(parent, "name", current) {
            return false;
        }
        // The C++ spelling of the same shape, but transparent rather than a
        // verdict: `f<T>(x)` is a use and `template <> struct Tiles<8> {…}` is
        // a declaration, and only the node above the wrapper can tell which.
        if is_template_wrapper_name(parent, current) {
            current = parent;
            continue;
        }
        // A destructuring default is a use, not a binding. In
        // `({ sleep = defaultSleep, cwd = repoRoot() }) => …` the left side
        // binds and the right side is an expression evaluated when the
        // property is absent — but the whole pattern sits on the parameter's
        // `pattern` field, so the test below read `defaultSleep` as a
        // declaration and dropped its only reference. The 2026-10-09 audit
        // measured the result: `defaultSleep` published dead at 0.9 (TS V17).
        if matches!(
            parent.kind(),
            "assignment_pattern" | "object_assignment_pattern"
        ) && parent
            .child_by_field_name("right")
            .is_some_and(|right| right.id() == current.id())
        {
            return false;
        }
        if field_contains(parent, "name", node)
            || field_contains(parent, "alias", node)
            || field_contains(parent, "parameter", node)
            || field_contains(parent, "pattern", node)
        {
            return true;
        }
        if matches!(
            parent.kind(),
            "assignment"
                | "assignment_expression"
                | "assignment_statement"
                | "augmented_assignment"
                | "augmented_assignment_expression"
                | "short_var_declaration"
                | "range_clause"
                | "for_statement"
                | "for_in_statement"
                | "for_of_statement"
                | "for_in_clause"
                | "for_of_clause"
                | "var_spec"
                | "const_spec"
        ) && field_contains(parent, "left", node)
        {
            return true;
        }
        // A Rust closure parameter is a binding site. `closure_parameters`
        // has no fields, so neither the `pattern` test above nor the wrapper
        // climb reaches an untyped `|handler|`; `|handler: H|` was recognised
        // only because the `parameter` node in between does carry `pattern`.
        // Half a rule is worse than none: the typed form counted as a local and
        // the untyped form did not. Closures are callables, so uses *inside*
        // them stay local while uses outside no longer inherit the parameter —
        // but the parameter identifier itself is still a binding and must
        // not be emitted as a Name reference to some same-named symbol.
        if parent.kind() == "closure_parameters" {
            return true;
        }
        if parent.kind() == "pair" {
            if let Some(key) = parent.child_by_field_name("key") {
                if key.id() == node.id() {
                    return true;
                }
            }
        }
        if is_binding_wrapper(parent.kind()) {
            current = parent;
            continue;
        }
        return false;
    }
}

/// Nodes that wrap a binding without being one, so the defining-name walk
/// climbs through them.
///
/// The `_target` suffix is load-bearing and easy to mistake for dead breadth:
/// Python's `with … as handle` puts `handle` in an `as_pattern_target`, which
/// appears in `node-types.json` only as a *field type* of `as_pattern` and not
/// as a top-level entry, so a scan of top-level node kinds does not find it.
/// Without the clause the walk stops there, `handle` stops being a binding
/// site, and both it and every later mention become module-level uses.
pub(super) fn is_binding_wrapper(kind: &str) -> bool {
    matches!(
        kind,
        "expression_list"
            | "identifier_list"
            | "pattern_list"
            | "parenthesized_expression"
            | "spread_element"
            | "list_splat_pattern"
            | "pattern"
    ) || kind.ends_with("_pattern")
        || kind.ends_with("_target")
}
