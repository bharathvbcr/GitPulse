use super::*;

/// Whether `node` is a C-family callable.
///
/// Told apart from the identically-named nodes in other grammars by structure,
/// because this walk has no language key: Python's `function_definition` and
/// JavaScript's `method_definition` both carry a `name` field, while the
/// C-family ones never do and hang their name off a `declarator` chain or an
/// Objective-C selector instead.
pub(super) fn is_c_family_callable(node: Node) -> bool {
    match node.kind() {
        "function_definition" => {
            node.child_by_field_name("name").is_none()
                && node.child_by_field_name("declarator").is_some()
        }
        "method_definition" => node.child_by_field_name("name").is_none(),
        _ => false,
    }
}

/// Binding name of `node` if it is a callable. Unnamed arrows keep walking so
/// `useEffect(() => { persist() })` still attributes `persist` to the outer
/// named function instead of dropping to file scope.
pub(super) fn callable_binding_name(node: Node, source: &str) -> Option<String> {
    if matches!(
        node.kind(),
        "function_definition"
            | "function_declaration"
            | "method_definition"
            | "function_item"
            | "method_declaration"
            | "generator_function_declaration"
            | "generator_function"
    ) {
        return get_child_text(node, "name", source);
    }
    if !matches!(node.kind(), "arrow_function" | "function_expression") {
        return None;
    }
    // Name a function *expression* by the binding it is assigned to, never by
    // its own internal name.
    //
    // A named function expression's name is only in scope inside itself, so it
    // is deliberately not emitted as a symbol. Returning it here anyway made
    // `dns.lookup = function patchedLookup() { … }` attribute its calls to
    // `file::patchedLookup`, which matches no node — the same unjoinable-edge
    // defect as SC9, and the residual behind SC10. It also disagreed with the
    // symbol emitter for `const f = function inner() { … }`, where the emitted
    // symbol is `f`. Falling through to `None` lets the walk continue to an
    // enclosing symbol that does exist, exactly as unnamed arrows already do.
    let parent = bounded_parent(node)?;
    if parent.kind() == "variable_declarator" {
        return get_child_text(parent, "name", source);
    }
    // `handle = () => …` in a class body, and `remove: () => …` in an object
    // literal bound to a module-level `const`: the member is the binding, and
    // the symbol emitter names it the same way.
    if let Some(member) = js_function_valued_member(node, source) {
        return Some(member);
    }
    // `const save = useCallback(() => { … }, [deps])`: the binding *is* the
    // function, reached through a wrapper that returns something calling it.
    // Named by the declarator, exactly as the symbol emitter names it.
    js_wrapper_declarator(node, source)
        .and_then(|declarator| get_child_text(declarator, "name", source))
}

/// Callees that return a function which, when called, runs the function they
/// were given — so `const f = wrapper(() => …)` binds `f` to that function.
///
/// Chosen from what a real React corpus wraps arrow consts in (ScholarLM, 2026-
/// 10-07: `useCallback` 393 + `React.useCallback` 7, then `memo`/`forwardRef`
/// for components). Deliberately absent: `useMemo` returns the function's
/// *result*, so the calls inside it run in the enclosing render and belong to
/// it; `lazy(() => import(…))` is an import thunk, not the component's body;
/// `setTimeout`/`setInterval` return a handle, and `promise.then` / array
/// methods return values.
pub(super) const JS_FUNCTION_WRAPPERS: &[&str] = &[
    "useCallback",
    "React.useCallback",
    "useEvent",
    "useEffectEvent",
    "React.useEffectEvent",
    "memo",
    "React.memo",
    "forwardRef",
    "React.forwardRef",
    "debounce",
    "throttle",
    "_.debounce",
    "_.throttle",
    "vi.fn",
    "jest.fn",
];

/// How many wrappers deep `memo(forwardRef(() => …))` is followed.
pub(super) const JS_WRAPPER_NESTING: usize = 3;

/// The member name a function-valued class field or object-literal pair gives
/// its function: `handle` for `handle = () => …`, `remove` for
/// `remove: () => …`. `None` when `function` is neither.
///
/// A pair counts only when its object is a module-level `const`'s value
/// ([`js_object_member_owner`]); the same object passed as an argument names no
/// binding a call could be attributed to.
pub(super) fn js_function_valued_member(function: Node, source: &str) -> Option<String> {
    if !matches!(function.kind(), "arrow_function" | "function_expression") {
        return None;
    }
    let parent = bounded_parent(function)?;
    match parent.kind() {
        "public_field_definition" | "field_definition" => {
            let value = parent.child_by_field_name("value")?;
            if value.id() != function.id() {
                return None;
            }
            js_field_name(parent, source)
        }
        "pair" => {
            js_object_member_owner(function)?;
            js_pair_key(parent, source)
        }
        _ => None,
    }
}

/// The name a class field declares. TypeScript calls the field `name`,
/// JavaScript `property`; a computed or string-keyed field names nothing a call
/// could write, so it is left out.
pub(super) fn js_field_name(field: Node, source: &str) -> Option<String> {
    let name = field
        .child_by_field_name("name")
        .or_else(|| field.child_by_field_name("property"))?;
    matches!(
        name.kind(),
        "property_identifier" | "private_property_identifier"
    )
    .then(|| get_node_text(name, source))
}

/// The identifier key of an object-literal pair.
pub(super) fn js_pair_key(pair: Node, source: &str) -> Option<String> {
    let key = pair.child_by_field_name("key")?;
    matches!(key.kind(), "property_identifier" | "identifier").then(|| get_node_text(key, source))
}

/// The module-level `variable_declarator` an object-literal member belongs to,
/// when `node` is a member of the object that declarator initialises:
/// `method() {}` (a `method_definition`) or the function of `name: () => {}`.
/// The initializer may be wrapped in `as const`, `satisfies T` or parentheses.
///
/// Such a member is addressed as `owner.member` — `paperService.fetchOne()` —
/// so it is a symbol qualified by the object, exactly as a method is qualified
/// by its class. A member of an object that is returned, passed or nested is
/// not: nothing names the object, and the factory case already owns its own
/// shape ([`js_returned_object_method`]).
pub(super) fn js_object_member_owner(node: Node) -> Option<Node> {
    let object = match node.kind() {
        "method_definition" => bounded_parent(node)?,
        "arrow_function" | "function_expression" => {
            let pair = bounded_parent(node)?;
            if pair.kind() != "pair"
                || pair
                    .child_by_field_name("value")
                    .is_none_or(|value| value.id() != node.id())
            {
                return None;
            }
            bounded_parent(pair)?
        }
        _ => return None,
    };
    if object.kind() != "object" {
        return None;
    }
    let mut child = object;
    let mut parent = bounded_parent(object)?;
    while matches!(
        parent.kind(),
        "as_expression"
            | "satisfies_expression"
            | "parenthesized_expression"
            | "non_null_expression"
    ) {
        child = parent;
        parent = bounded_parent(parent)?;
    }
    (parent.kind() == "variable_declarator"
        && parent
            .child_by_field_name("value")
            .is_some_and(|value| value.id() == child.id())
        && parent
            .child_by_field_name("name")
            .is_some_and(|name| name.kind() == "identifier")
        && is_module_level(parent))
    .then_some(parent)
}

/// The function a wrapper call binds, when `value` is one: the first argument
/// of a [`JS_FUNCTION_WRAPPERS`] call, through at most [`JS_WRAPPER_NESTING`]
/// nested wrappers.
pub(super) fn js_wrapped_function<'tree>(value: Node<'tree>, source: &str) -> Option<Node<'tree>> {
    let mut current = value;
    for _ in 0..JS_WRAPPER_NESTING {
        if current.kind() != "call_expression" {
            return None;
        }
        let callee = current.child_by_field_name("function")?;
        if !JS_FUNCTION_WRAPPERS.contains(&get_node_text(callee, source).as_str()) {
            return None;
        }
        let first = current.child_by_field_name("arguments")?.named_child(0)?;
        if matches!(first.kind(), "arrow_function" | "function_expression") {
            return Some(first);
        }
        current = first;
    }
    None
}

/// The `variable_declarator` whose value wraps `function` in
/// [`JS_FUNCTION_WRAPPERS`] calls, or `None` when `function` is anything else
/// — an argument of an ordinary call, or the second argument of a wrapper.
pub(super) fn js_wrapper_declarator<'tree>(
    function: Node<'tree>,
    source: &str,
) -> Option<Node<'tree>> {
    let mut current = function;
    for _ in 0..JS_WRAPPER_NESTING {
        let arguments = bounded_parent(current)?;
        if arguments.kind() != "arguments" {
            return None;
        }
        let call = bounded_parent(arguments)?;
        if call.kind() != "call_expression" {
            return None;
        }
        let parent = bounded_parent(call)?;
        if parent.kind() == "variable_declarator" {
            let value = parent.child_by_field_name("value")?;
            return (value.id() == call.id()
                && js_wrapped_function(value, source).is_some_and(|f| f.id() == function.id()))
            .then_some(parent);
        }
        current = call;
    }
    None
}

/// The object half of a member access, when `node` is the member half.
///
/// `cfg` for `cfg.enabled`, `cmd` for `cmd.baseline`, `this.backend` for
/// `this.backend.hydrate`. `None` for a bare identifier, and `None` for the
/// object half itself — `cfg` in `cfg.enabled` is a use of `cfg`, not of
/// anything owned by it.
///
/// Recording this is what lets the resolver tell a *member* name, which names
/// something another symbol owns, from a *local* name, which names a binding in
/// the current scope. It refuses to resolve the latter globally on purpose, so
/// that `except Exception as e` cannot bind to an unrelated `def e`; without
/// the distinction that refusal swallowed every property read and every
/// callback passed by attribute along with it.
/// Drop the `Name` reference that a call's own callee already accounts for.
///
/// Every call extractor records a `Call` (or `Constructor`) reference naming
/// the callee and spanning the callee *expression* — `r.mm` for `r.mm()`,
/// `Z::pp` for `Z::pp()`. The generic identifier walker then visits the same
/// method identifier and, unless [`is_call_callee`] recognises the grammar's
/// spelling for it, records a **second** reference for it as an ordinary `Name`
/// use of a symbol.
///
/// That suppression had drifted out of step with the very facts it depends on.
/// Three functions each carry their own copy of "which grammars spell a name
/// reached through something else, and in which fields":
/// [`member_access_fields`] lists four spellings, [`member_access_receiver`]
/// five, and `is_call_callee` three. The reference the walker emits is recorded
/// by the resolver exactly when `member_access_receiver` gives it a receiver —
/// so every spelling in the five-entry list and not in the three-entry one
/// produced a duplicate row, and the two missing entries are Rust's and Scala's
/// `field_expression` and Rust's `scoped_identifier`.
///
/// **Eleven** languages were measured emitting the duplicate — C, C++, Java,
/// Kotlin, Lua, R, Ruby, Rust, Scala, Solidity, Swift — by running
/// `a_call_is_one_site_not_two`'s matrix against the code this replaces; CUDA
/// and Luau share the C++ and Lua grammars and the shapes that produced it.
/// Seven were already correct: Go, Python, JS/TS, C#, PHP, Dart, Objective-C.
///
/// On this repository it cost Rust **49,989** of its 150,163 attribution sites
/// — 27% of the corpus total of 182,181, every one of them a call already
/// counted once. And the second copy was filed as a *failure* even where the
/// first resolved: a `Widget::omegafn()` that produced a `Calls` edge at full
/// confidence also produced an `uninferred_receiver` row claiming it had not.
/// 1,937 resolved `References` edges duplicated a `Calls` edge to the same
/// pair, and four more bound a C function-pointer field (`lexer->advance`) to
/// the file's own same-named `static` function at confidence 1.0.
///
/// The test is structural and needs no grammar table, which is why it is here
/// rather than a fourteenth arm in `is_call_callee`: a callee's name is the
/// **last** token of the callee expression in every grammar that spells a call
/// as `<receiver> <separator> <name>`, so a `Name` reference is the callee's
/// own second copy exactly when a `Call`/`Constructor` reference of the same
/// name ends at the same byte and starts at or before it.
///
/// The suffix requirement is what keeps the receiver, which is the property the
/// three-entry list was written to protect (see `is_call_callee`, and
/// `member_use_liveness`): in `mm::mm()` both halves are spelled `mm` and both
/// lie inside the callee span, and only the right-hand one ends where the
/// call's does. Requiring the *name* to match as well keeps a whole-expression
/// callee span — an Objective-C `message_expression`, a C `Type` reference over
/// a declarator — from swallowing an unrelated identifier that happens to end
/// where it does.
///
/// # What this deliberately leaves behind
///
/// A callee reference whose span is *wider than the callee expression* keeps
/// its duplicate. Two shapes have one: a Rust turbofish, where `::<_, String>`
/// follows the name inside the `generic_function` the call records; and a call
/// recovered from a Rust macro body, which is stamped with the whole
/// `macro_invocation`'s span because the re-parsed tree has no coordinates in
/// this file. `assert_eq!(calls_of(src), 3)` is the common one.
///
/// A second tier — "the callee is the only mention of its own name inside the
/// span" — takes those, and was built and measured rather than reasoned about.
/// A/B over one snapshot of this repository, 1,101 files: it removes **1,164**
/// edges, of which **1,150** are genuine duplicates of a `Calls` edge to the
/// same pair and **14** are the only edge that pair has. All 14 are a call
/// inside a Rust macro whose *macro-borne call* did not resolve while the
/// identifier did — `RpcError::new -> codes::is_reserved_and_undefined` in
/// `devmap-serve`, where the receiver is a module path — so the identifier is
/// the whole of the evidence that the target is reached.
///
/// Extraction cannot tell the 1,150 from the 14: they are the same syntax, and
/// what separates them is what the *resolver* later makes of each. A duplicate
/// edge costs a row; a dropped reference can report a live symbol dead. So this
/// abstains, and the residue is stated rather than hidden —
/// `a_turbofish_keeps_its_duplicate_and_that_is_known` pins it, and
/// `a_macro_borne_calls_argument_keeps_its_reference` pins why.
pub(super) fn drop_duplicate_callee_names(references: &mut Vec<ExtractedReference>) {
    // Keyed on the byte the callee's name ends at, which is a position in one
    // file and therefore unique; the value is the leftmost byte a callee
    // expression of that name reaches back to.
    let mut callee_ends: HashMap<(&str, usize), usize> = HashMap::new();
    for reference in references.iter() {
        if matches!(
            reference.kind,
            ReferenceKind::Call | ReferenceKind::Constructor
        ) {
            let start = callee_ends
                .entry((reference.name.as_str(), reference.span.end_byte))
                .or_insert(reference.span.start_byte);
            *start = (*start).min(reference.span.start_byte);
        }
    }
    if callee_ends.is_empty() {
        return;
    }
    // Two passes rather than one `retain` closure, because the map borrows the
    // names it is keyed by.
    let mut keep = Vec::with_capacity(references.len());
    for reference in references.iter() {
        keep.push(
            reference.kind != ReferenceKind::Name
                || !callee_ends
                    .get(&(reference.name.as_str(), reference.span.end_byte))
                    .is_some_and(|start| *start <= reference.span.start_byte),
        );
    }
    drop(callee_ends);
    let mut index = 0;
    references.retain(|_| {
        let keep_this = keep[index];
        index += 1;
        keep_this
    });
}

pub(super) fn member_access_receiver(node: Node, source: &str) -> Option<String> {
    let parent = bounded_parent(node)?;
    // Swift nests the member one level deeper: `Meter.privDouble` is a
    // `navigation_expression` whose `suffix` is a `navigation_suffix` holding
    // the name. Without this a function passed by name — `map(Meter.privDouble)`
    // — arrived as a bare `privDouble`, which the resolver declines by design,
    // and the method was published dead at 0.9 (the audit's Swift P4).
    if parent.kind() == "navigation_suffix" {
        if parent
            .child_by_field_name("suffix")
            .is_none_or(|suffix| suffix.id() != node.id())
        {
            return None;
        }
        let navigation =
            bounded_parent(parent).filter(|nav| nav.kind() == "navigation_expression")?;
        let target = navigation.child_by_field_name("target")?;
        // A key path names its member through the root type written after the
        // backslash: `\S.secret` reads `S.secret`, and `\.secret` names no root.
        if target.kind() == "key_path_expression" {
            let mut cursor = target.walk();
            let root = target
                .named_children(&mut cursor)
                .find(|child| matches!(child.kind(), "type_identifier" | "user_type"))
                .map(|child| get_node_text(child, source));
            return root.filter(|root| !root.is_empty());
        }
        // `p.a + q.b` parses as `((p.a + q).b)`: the sum is the target of the
        // member access, which binds tighter than `+`. The receiver is the
        // right operand (and what follows it), as `langcalls::swift` reads it
        // for a call.
        if let Some(tail) = crate::langcalls::swift::swift_sum_tail(target) {
            return source
                .get(tail.start_byte()..target.end_byte())
                .and_then(crate::langcalls::scope::clamp_receiver);
        }
        let operand = crate::langcalls::swift::swift_call_operand(target);
        let text = receiver_identity(operand, source, 0);
        return (!text.is_empty()).then_some(text);
    }
    // The member of a leading-dot expression (`.circle(1)`) names a member of
    // the type the expression is written against, where that type is stated.
    if parent.kind() == "prefix_expression" && swift_leading_dot_member(node, source) {
        return swift_leading_dot_context(node, source);
    }
    // Kotlin's `callable_reference` has no fields either: `Type::m` names the
    // receiver type in its first named child and the member in its last.
    // Without the receiver the member arrived as a bare name, which never
    // matches an extension keyed `Type.m` and which the resolver declines
    // silently, so the namesake veto never saw it. An unbound `::f` has no
    // receiver. (`template::m`, a bound reference on a value, parses as the
    // fieldless `navigation_expression` below.)
    if parent.kind() == "callable_reference" {
        let mut cursor = parent.walk();
        let named: Vec<Node> = parent.named_children(&mut cursor).collect();
        let [receiver @ .., member] = named.as_slice() else {
            return None;
        };
        if member.id() != node.id() {
            return None;
        }
        let receiver = receiver
            .iter()
            .rfind(|child| child.kind() != "type_modifiers")?;
        let text = receiver_identity(*receiver, source, 0);
        return (!text.is_empty()).then_some(text);
    }
    // Kotlin's `navigation_expression` has no fields: the object is the first
    // named child and the member the last (`task.label`, `a.b.c`).
    if parent.kind() == "navigation_expression" {
        let count = parent.named_child_count();
        if count < 2
            || parent
                .named_child(count - 1)
                .is_none_or(|member| member.id() != node.id())
        {
            return None;
        }
        let text = receiver_identity(parent.named_child(0)?, source, 0);
        return (!text.is_empty()).then_some(text);
    }
    // The grammars spell the same shape several ways: `attribute` in Python,
    // `member_expression` in JS/TS, `selector_expression` in Go, Rust's
    // `field_expression` (`self.field`) and `scoped_identifier` (`Self::f`).
    let (object_field, member_field) = match parent.kind() {
        "attribute" => ("object", "attribute"),
        "member_expression" => ("object", "property"),
        "selector_expression" => ("operand", "field"),
        "field_expression" => ("value", "field"),
        "scoped_identifier" | "scoped_type_identifier" => ("path", "name"),
        _ => return None,
    };
    // Only the member half has a receiver. The object half is a use in its own
    // right and must keep resolving as one.
    let member = parent.child_by_field_name(member_field)?;
    if member.id() != node.id() {
        return None;
    }
    let object = parent.child_by_field_name(object_field)?;
    // X44. The object's *identity*, not its source text: `runner.invoke(app,
    // ["init"]).output` names the call it reads from, and a receiver that is a
    // block copied into a column groups with nothing.
    let text = receiver_identity(object, source, 0);
    (!text.is_empty()).then_some(text)
}

pub(super) fn extracted_reference(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    name: String,
    kind: ReferenceKind,
    assigned_to: Option<String>,
) -> ExtractedReference {
    ExtractedReference {
        name,
        kind,
        span: node_span(node),
        enclosing_symbol: enclosing_callable_qualified(node, source, file_symbol_name),
        assigned_to,
        // The object half of a member access, so the resolver can tell
        // `cfg.enabled` from a bare local named `enabled`.
        receiver_expr: member_access_receiver(node, source),
    }
}

/// The superclass expressions of the Python `class_definition` a node sits in,
/// exactly as written.
///
/// As *written*, deliberately: `crate::wiring::python_base_dispatch_reason`
/// matches on a qualified tail (`autograd.Function`, `nn.Module`), and the
/// qualifier is the whole reason that rule is safe to apply. Reducing
/// `torch.autograd.Function` to the bare `Function` here — which is what every
/// other type-name path in this file does — would hand that registry a name it
/// must refuse, and the caller would then be free to inherit torch's dispatch
/// contract from any local class called `Function`.
///
/// Bounded by [`bounded_parent`] like every other ancestor walk here, and stops
/// at the first enclosing class: a method's dispatch contract comes from the
/// class it is declared on, never from an outer class it is nested inside.
pub(super) fn python_enclosing_superclasses(node: Node, source: &str) -> Vec<String> {
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        if parent.kind() == "class_definition" {
            let Some(bases) = parent.child_by_field_name("superclasses") else {
                return Vec::new();
            };
            let mut cursor = bases.walk();
            return bases
                .named_children(&mut cursor)
                .map(|base| get_node_text(base, source))
                .filter(|text| !text.is_empty())
                .collect();
        }
        ancestor = bounded_parent(parent);
    }
    Vec::new()
}

/// A base written `written` (`autograd.Function`, `nn.Module`, `Function`) with
/// its first segment replaced by what the file's module-scope imports bind it
/// to (`torch.autograd.Function` under `from torch import autograd`), so the
/// dispatch registry matches the library and not a spelling. `None` when an
/// import binds the root to something outside `torch` (`from local import
/// autograd` makes `autograd.Function` the repository's own class). A root no
/// import binds is returned as written: a name declared in the file, or a
/// global.
///
/// Read from [`python_file_facts`], so a function-scope import of the same
/// name does not change the module-level reading, and the file's imports are
/// read once per file rather than once per method.
pub(super) fn python_torch_qualified_base(
    node: Node,
    written: &str,
    source: &str,
) -> Option<String> {
    let (root_name, rest) = match written.trim().split_once('.') {
        Some((root_name, rest)) => (root_name.trim(), Some(rest)),
        None => (written.trim(), None),
    };
    let facts = python_file_facts(python_tree_root(node), source);
    let Some(bound) = facts.imports.get(root_name) else {
        return Some(written.trim().to_string());
    };
    if !bound
        .iter()
        .all(|module| module == "torch" || module.starts_with("torch."))
    {
        return None;
    }
    let first = bound.first()?;
    Some(match rest {
        Some(rest) => format!("{first}.{rest}"),
        None => first.clone(),
    })
}

pub(super) fn enclosing_type_name(node: Node, source: &str) -> Option<String> {
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        match parent.kind() {
            // The class a Kotlin loss recovery wraps a statement in is not a
            // class the source has; what it holds is not a member of anything.
            "class_declaration" if crate::kotlinloss::is_recovery_wrapper(parent, source) => {
                ancestor = bounded_parent(parent);
            }
            "class_definition" | "class_declaration" | "abstract_class_declaration" => {
                return get_child_text(parent, "name", source);
            }
            "trait_item" => {
                let name = get_child_text(parent, "name", source)?;
                return Some(rust_function_local_type(parent, source, &name).unwrap_or(name));
            }
            // An impl's methods belong to the TYPE, including in
            // `impl Trait for Type` (SC11).
            //
            // Qualifying them by the trait gave every implementor the same
            // identity. That is not an exotic case: a single `impl` overriding
            // a *defaulted* trait method already produced `Trait.method` twice
            // — the declaration and the override — so the graph held two nodes
            // no edge could tell apart. A qualified name is the graph's join
            // key; two things sharing one is a broken key, not a naming
            // preference. The type is what makes an implementation unique.
            //
            // The trait relationship is not lost: `rust_method_structural_reason`
            // reads the `trait` field directly to exempt trait-impl methods from
            // dead-code reporting, and does not depend on this name.
            //
            // This diverges from the Python incumbent, which also names these by
            // the trait. The divergence is deliberate and recorded in
            // DIVERGENCES.md; correctness of the join key outranks bug-for-bug
            // parity here.
            "impl_item" => {
                let owner = parent
                    .child_by_field_name("type")
                    .or_else(|| parent.child_by_field_name("trait"))?;
                let text = get_node_text(owner, source);
                let bare = text.split('<').next().unwrap_or(&text).trim().to_string();
                return Some(rust_function_local_type(parent, source, &bare).unwrap_or(bare));
            }
            // A nested function is not a class method merely because a class
            // occurs farther up the ancestor chain. Rust spells the same node
            // `function_item`; without it a helper declared inside an impl
            // method inherited the impl type and collided with a real method
            // of that name (`Store.run` inside `checkpoint_wal`).
            "function_definition"
            | "function_declaration"
            | "function_expression"
            | "arrow_function"
            | "method_definition"
            | "function_item"
            | "closure_expression" => return None,
            _ => ancestor = bounded_parent(parent),
        }
    }
    None
}

/// The interface a `method_signature` belongs to, or `None` when it does not.
///
/// A class overload is the same node kind (`area(): number;` beside
/// `area(): number { … }`) and is the method body's declaration, not a second
/// symbol. Stopping at a class or a function keeps that signature — and a
/// signature nested inside one — from being filed under an outer interface.
pub(super) fn enclosing_interface_name(node: Node, source: &str) -> Option<String> {
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        match parent.kind() {
            "interface_declaration" => return get_child_text(parent, "name", source),
            "class_declaration"
            | "abstract_class_declaration"
            | "class"
            | "function_declaration"
            | "generator_function_declaration"
            | "method_definition"
            | "function_expression"
            | "arrow_function" => return None,
            _ => ancestor = bounded_parent(parent),
        }
    }
    None
}

/// The owner name of a Rust type declared inside a function body, keyed under
/// that function: `with_guard.Guard` for `fn with_guard() { struct Guard; impl
/// Drop for Guard { fn drop(..) } }`.
///
/// The struct itself is already keyed that way (`scoped_qualified_name`), so
/// its methods must be too, or the method's `parent_symbol` names a type the
/// graph does not hold and a module-level `Guard::drop` in the same file shares
/// the local one's identity. A qualified name is the join key; this is the one
/// place a method's owner is computed, so the symbol emitters and the call
/// attribution in `enclosing_callable_qualified` move together.
///
/// `item` is the `impl_item` or `trait_item`. Only a type declared in a block
/// of the same function counts: `impl Display for Outer` written inside a
/// function still implements the module-level `Outer`, and keying it under the
/// function would sever it from that type. `None` keeps the bare name.
pub(super) fn rust_function_local_type(item: Node, source: &str, bare: &str) -> Option<String> {
    let mut ancestor = bounded_parent(item);
    while let Some(parent) = ancestor {
        match parent.kind() {
            "block" => {
                let mut cursor = parent.walk();
                let declared = parent.named_children(&mut cursor).any(|child| {
                    matches!(
                        child.kind(),
                        "struct_item" | "enum_item" | "union_item" | "trait_item"
                    ) && get_child_text(child, "name", source).as_deref() == Some(bare)
                });
                if declared {
                    // An empty file prefix yields `::scope`; the callers add
                    // the file themselves, exactly as for a bare type name.
                    let scope = enclosing_callable_qualified(item, source, "")?;
                    return Some(format!("{}.{bare}", scope.trim_start_matches("::")));
                }
                ancestor = bounded_parent(parent);
            }
            "function_item" | "closure_expression" | "source_file" | "declaration_list" => {
                return None
            }
            _ => ancestor = bounded_parent(parent),
        }
    }
    None
}

/// Attribute paths attached to `node`, innermost first.
///
/// tree-sitter-rust emits `#[...]` as an `attribute_item` *sibling* preceding
/// the item it decorates, never as a child. Reading only the item's own text —
/// which is all the `pub` export check does — is therefore structurally blind
/// to every attribute, including `#[test]` and `#[no_mangle]`.
pub(super) fn rust_attribute_paths(node: Node, source: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut sibling = node.prev_sibling();
    while let Some(candidate) = sibling {
        match candidate.kind() {
            "line_comment" | "block_comment" => {}
            "attribute_item" => {
                if let Some(attribute) = candidate.named_child(0) {
                    if let Some(path) = attribute.named_child(0) {
                        paths.push(get_node_text(path, source).replace(char::is_whitespace, ""));
                    }
                }
            }
            _ => break,
        }
        sibling = candidate.prev_sibling();
    }
    paths
}

/// Serde (and the same-shaped) attributes name functions by string:
/// `#[serde(default = "unknown_ctime")]`. Those strings are not call nodes, so
/// without this the helper is a private function with no inbound edge.
pub(super) fn rust_attribute_callback_refs(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    references: &mut Vec<ExtractedReference>,
) {
    const FN_KEYS: &[&str] = &[
        "default",
        "serialize_with",
        "deserialize_with",
        "skip_serializing_if",
        "skip_deserializing_if",
    ];
    // A field's attribute belongs to the type that declares the field: the
    // derive reads `#[serde(default = "f")]` when it expands the struct, so the
    // struct is what uses `f`. A callable ancestor still wins.
    let enclosing = enclosing_callable_qualified(node, source, file_symbol_name).or_else(|| {
        let mut ancestor = bounded_parent(node);
        while let Some(parent) = ancestor {
            if matches!(parent.kind(), "struct_item" | "enum_item" | "union_item") {
                return get_child_text(parent, "name", source)
                    .map(|name| format!("{file_symbol_name}::{name}"));
            }
            ancestor = bounded_parent(parent);
        }
        None
    });
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == "token_tree" {
            let mut cursor = current.walk();
            let children: Vec<Node> = current.children(&mut cursor).collect();
            let mut index = 0;
            while index < children.len() {
                let key_name = get_node_text(children[index], source);
                if FN_KEYS.contains(&key_name.as_str()) {
                    // `default` is a keyword in token trees, not an identifier.
                    // `=` may be absent as a child (punctuation extras). The
                    // string is the next string-shaped sibling.
                    let mut look = index + 1;
                    while look < children.len() {
                        let candidate = children[look];
                        let text = get_node_text(candidate, source);
                        if text == "=" || text == "," {
                            look += 1;
                            continue;
                        }
                        if candidate.kind().contains("string") {
                            if let Some(path) = rust_string_literal_content(candidate, source) {
                                rust_push_attr_callback(
                                    path,
                                    node_span(candidate),
                                    enclosing.clone(),
                                    references,
                                );
                            }
                        }
                        break;
                    }
                }
                index += 1;
            }
        }
        push_named_children(current, &mut stack);
    }
}

pub(super) fn rust_push_attr_callback(
    path: String,
    span: Span,
    enclosing_symbol: Option<String>,
    references: &mut Vec<ExtractedReference>,
) {
    let name = path.rsplit("::").next().unwrap_or(&path).trim().to_string();
    if !is_user_ident(&name) {
        return;
    }
    let receiver_expr = path
        .rsplit_once("::")
        .map(|(prefix, _)| {
            prefix
                .rsplit("::")
                .next()
                .unwrap_or(prefix)
                .trim()
                .to_string()
        })
        .filter(|receiver| is_user_ident(receiver));
    references.push(ExtractedReference {
        name,
        kind: ReferenceKind::Name,
        span,
        enclosing_symbol,
        assigned_to: None,
        receiver_expr,
    });
}

pub(super) fn rust_string_literal_content(node: Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "string_content" {
            let text = get_node_text(child, source);
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    get_node_text(node, source)
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .map(str::to_string)
        .filter(|text| !text.is_empty())
}

/// Why a Rust `fn` can never be observed as `pub` regardless of its liveness.
///
/// A trait-impl method and a defaulted trait method both reject `pub`, so
/// `is_exported` is a constant `false` for them and carries no information.
/// Treating that constant as evidence of deadness is the single highest-volume
/// Rust false positive.
pub(super) fn rust_method_structural_reason(node: Node) -> Option<&'static str> {
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        match parent.kind() {
            "impl_item" => {
                return parent
                    .child_by_field_name("trait")
                    .map(|_| "implements a trait method; Rust forbids `pub` on trait-impl items")
            }
            "trait_item" => {
                // A bare signature declares the contract; a defaulted method
                // also supplies a body. Both are exempt for the same structural
                // reason, but the reason string is user-facing and should not
                // call a declaration a default.
                return Some(if node.kind() == "function_signature_item" {
                    "declared trait method; Rust forbids `pub` on trait items"
                } else {
                    "defaulted trait method; Rust forbids `pub` on trait items"
                });
            }
            // A nested `fn` is not a method merely because an impl block is
            // farther up the ancestor chain.
            "function_item" | "closure_expression" => return None,
            _ => ancestor = bounded_parent(parent),
        }
    }
    None
}

/// Root of the syntax tree containing `node`.
pub(super) fn ast_root(node: Node) -> Node {
    let mut current = node;
    while let Some(parent) = bounded_parent(current) {
        current = parent;
    }
    current
}

/// Whether a JS/TS declaration is externally reachable.
///
/// **Single owner of this test.** Six declaration forms need it — function,
/// method, `const` arrow, class, enum, interface/type alias, and namespace —
/// and five of them had inlined their own copy of
/// `parent().kind() == "export_statement" || …`. Two of those copies were
/// subtly different, and every one of them was independently mutable, so a
/// broken copy would silently mark one *kind* of declaration unexported while
/// the others stayed right. `is_exported` is direct evidence for dead code, so
/// that reads as "this enum is private and unused" rather than as a bug.
///
/// Two forms are not their own owner:
/// - `bounded_parent(method_definition)` is always `class_body`, so a direct parent
///   check can never be true — a method is exported exactly when its class is.
/// - `bounded_parent(variable_declarator)` is the `lexical_declaration`; the export
///   statement wraps that, not the declarator.
pub(super) fn js_symbol_is_exported(node: Node, source: &str) -> bool {
    // One hop only: skip the factory that returns `{ method() {} }`, then
    // resume. Skipping every enclosing function would publish methods of an
    // unused helper nested inside an exported function.
    let skip_one_factory = js_returned_object_method(node);
    let mut skipped_factory = false;
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        match parent.kind() {
            "export_statement" | "export_declaration" => return true,
            // The value escapes to the host runtime. `globalThis.ResizeObserver
            // = class { observe() {} }` publishes `observe` to anything that
            // constructs a `ResizeObserver` — the browser, a test harness, a
            // library — and none of those call sites is in the corpus. That is
            // the same claim `export` makes, made a different way.
            "assignment_expression" => {
                if parent
                    .child_by_field_name("left")
                    .is_some_and(|left| js_target_is_global(left, source))
                {
                    return true;
                }
                ancestor = bounded_parent(parent);
            }
            // A value bound inside a callable does not escape by being written,
            // except the one-level returned-object case above.
            "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "arrow_function"
            | "method_definition" => {
                if skip_one_factory && !skipped_factory {
                    skipped_factory = true;
                    ancestor = bounded_parent(parent);
                } else {
                    return false;
                }
            }
            _ => ancestor = bounded_parent(parent),
        }
    }
    false
}

/// A method on an object that is the (possibly nested / parenthesized) operand
/// of a `return`, or the implicit return of an arrow.
///
/// Nested functions stay private: `export function outer() { function inner() {}
/// return inner; }` is not this shape. An object passed as an argument is not
/// this shape either — `return foo({ get() {} })` stops at `arguments`.
pub(super) fn js_returned_object_method(node: Node) -> bool {
    if node.kind() != "method_definition" {
        return false;
    }
    let mut ancestor = bounded_parent(node);
    let mut saw_object = false;
    while let Some(parent) = ancestor {
        match parent.kind() {
            "object" => {
                saw_object = true;
                ancestor = bounded_parent(parent);
            }
            "pair" | "parenthesized_expression" | "sequence_expression" => {
                ancestor = bounded_parent(parent);
            }
            "return_statement" | "arrow_function" => return saw_object,
            _ => return false,
        }
    }
    false
}

/// Whether an assignment target names a property of the global object.
///
/// The four spellings are the ones a JavaScript program can actually reach:
/// `globalThis` is the standard, `window` and `self` are the web ones, `global`
/// is Node's. A bare identifier is deliberately excluded — `x = class {…}`
/// assigns to a binding, not to the runtime.
pub(super) fn js_target_is_global(left: Node, source: &str) -> bool {
    if left.kind() != "member_expression" {
        return false;
    }
    left.child_by_field_name("object")
        .map(|object| get_node_text(object, source))
        .is_some_and(|root| matches!(root.as_str(), "globalThis" | "window" | "global" | "self"))
}

/// Declared parameter count of a `parameter_list`.
///
/// A grouped declaration binds several names to one type (`m(x, y int)` is two
/// parameters, not one), and an interface spec may omit names entirely
/// (`m(int)` is one). Counting `parameter_list` children would get both wrong,
/// so count the `name` fields and fall back to one for an unnamed declaration.
pub(super) fn go_param_count(parameters: Node) -> usize {
    let mut cursor = parameters.walk();
    let mut total = 0;
    let mut param_cursor = parameters.walk();
    for declaration in parameters.named_children(&mut param_cursor) {
        if !matches!(
            declaration.kind(),
            "parameter_declaration" | "variadic_parameter_declaration"
        ) {
            continue;
        }
        total += declaration
            .children_by_field_name("name", &mut cursor)
            .count()
            .max(1);
    }
    total
}

/// Interface method specs and method-declaration arities declared in this file.
///
/// Both halves of the Go interface-satisfaction join are per-file facts, but the
/// two halves routinely live in different files of the same package, so they are
/// carried on the `Extraction` and joined in `devmap-analyze`. Embedded
/// interfaces (`interface { io.Reader }`) parse as `type_elem`, not
/// `method_elem`, so their methods are not represented here.
///
/// The third list is [`Extraction::go_member_names`]: every unexported field
/// and interface-method name in the file, from **any** struct or interface
/// literal. Collected on the `struct_type` / `interface_type` node itself rather
/// than under `type_spec`, because a generic constraint
/// (`[T interface{ flush() }]`) and an anonymous struct (`var cfg struct{ run
/// func() }`) declare selectors too, and missing one would let the resolver
/// bind `x.flush()` to a concrete method it does not name.
pub(super) fn go_method_sets(
    root: Node,
    source: &str,
    file_symbol_name: &str,
) -> (Vec<GoInterfaceMethod>, Vec<GoMethodParams>, Vec<String>) {
    let mut interface_methods = Vec::new();
    let mut method_params = Vec::new();
    let mut member_names = Vec::new();
    let mut record_member = |name: &str| {
        if go_name_is_unexported(name) {
            member_names.push(name.to_string());
        }
    };
    let mut worklist = vec![root];
    // Same stride idiom as `walk_tree`. This pass runs *after* the walk has
    // returned, over the whole tree again, and until now had no deadline at
    // all: a tree that the walk finished just inside its budget could spend
    // unbounded time here and still be published. The caller re-reads the latch
    // this sets and refuses the file.
    let mut since_check = 0u32;
    while let Some(node) = worklist.pop() {
        since_check += 1;
        if since_check >= DEADLINE_CHECK_STRIDE {
            since_check = 0;
            if walk_deadline_passed() {
                break;
            }
        }
        match node.kind() {
            "interface_type" => {
                let mut member_cursor = node.walk();
                for member in node.named_children(&mut member_cursor) {
                    if matches!(member.kind(), "method_elem" | "method_spec") {
                        if let Some(method) = member.child_by_field_name("name") {
                            record_member(get_node_text(method, source).as_str());
                        }
                    }
                }
            }
            "field_declaration" => {
                let mut name_cursor = node.walk();
                let mut named = false;
                for name in node.children_by_field_name("name", &mut name_cursor) {
                    named = true;
                    record_member(get_node_text(name, source).as_str());
                }
                // An embedded field is named by its type: `*pkg.recorder[T]`
                // declares the field `recorder`.
                if !named {
                    if let Some(embedded) = node.child_by_field_name("type") {
                        let text = get_node_text(embedded, source);
                        let head = text.split('[').next().unwrap_or(&text);
                        let bare = head.rsplit('.').next().unwrap_or(head);
                        record_member(bare.trim_start_matches('*').trim());
                    }
                }
            }
            "type_spec" => {
                if let (Some(name), Some(declared)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("type"),
                ) {
                    if declared.kind() == "interface_type" {
                        let interface_name = get_node_text(name, source);
                        let mut member_cursor = declared.walk();
                        for member in declared.named_children(&mut member_cursor) {
                            if !matches!(member.kind(), "method_elem" | "method_spec") {
                                continue;
                            }
                            let Some(method) = member.child_by_field_name("name") else {
                                continue;
                            };
                            interface_methods.push(GoInterfaceMethod {
                                interface_name: interface_name.clone(),
                                method: get_node_text(method, source),
                                param_count: member
                                    .child_by_field_name("parameters")
                                    .map(go_param_count)
                                    .unwrap_or(0),
                            });
                        }
                    }
                }
            }
            "method_declaration" => {
                if let (Some(name), Some((_, type_name))) = (
                    get_child_text(node, "name", source),
                    go_receiver(node, source),
                ) {
                    method_params.push(GoMethodParams {
                        qualified_name: format!("{file_symbol_name}::{type_name}.{name}"),
                        param_count: node
                            .child_by_field_name("parameters")
                            .map(go_param_count)
                            .unwrap_or(0),
                    });
                }
            }
            _ => {}
        }
        push_children(node, &mut worklist);
    }
    interface_methods.sort_by(|a, b| {
        (&a.method, a.param_count, &a.interface_name).cmp(&(
            &b.method,
            b.param_count,
            &b.interface_name,
        ))
    });
    method_params.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));
    member_names.sort();
    member_names.dedup();
    (interface_methods, method_params, member_names)
}

/// Every identifier-shaped token in `source` that Go would keep unexported.
///
/// Deliberately lexical and over-inclusive — comments, strings and keywords
/// contribute too. It is the veto list for a file whose syntax tree cannot be
/// trusted to have shown every declaration, and a spare veto only costs an
/// answer, never a wrong one. Linear in the source, one pass.
pub(super) fn go_unexported_tokens(source: &str) -> Vec<String> {
    let mut tokens = BTreeSet::new();
    let mut start: Option<usize> = None;
    for (index, character) in source
        .char_indices()
        .chain(std::iter::once((source.len(), ' ')))
    {
        let continues = character == '_' || character.is_alphanumeric();
        match (start, continues) {
            (None, true) if !character.is_numeric() => start = Some(index),
            (Some(from), false) => {
                let token = &source[from..index];
                if go_name_is_unexported(token) {
                    tokens.insert(token);
                }
                start = None;
            }
            _ => {}
        }
    }
    tokens.into_iter().map(str::to_string).collect()
}

/// A Go identifier the language keeps inside its package: its first character
/// is not an uppercase letter. `_` and non-ASCII lowercase are unexported too.
pub(crate) fn go_name_is_unexported(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|first| !first.is_uppercase())
}

/// Same-file half of the interface exemption, as a symbol-scoped annotation.
///
/// The cross-file half cannot be decided here — extraction never sees another
/// file — so `devmap-analyze` re-runs the same join over the whole Go package.
/// Both use `go_interface_method_matches`, so one match cannot yield two
/// different verdicts.
pub(super) fn go_interface_method_exemptions(
    symbols: &[ExtractedSymbol],
    interface_methods: &[GoInterfaceMethod],
    method_params: &[GoMethodParams],
    wiring: &mut Vec<WiringAnnotation>,
) {
    let param_counts: std::collections::BTreeMap<&str, usize> = method_params
        .iter()
        .map(|entry| (entry.qualified_name.as_str(), entry.param_count))
        .collect();
    for (symbol, interface_name) in
        go_interface_method_matches(symbols, &param_counts, interface_methods)
    {
        wiring.push(WiringAnnotation {
            kind: WiringKind::StructuralExempt,
            target_symbol: symbol.qualified_name.clone(),
            details: go_interface_exemption_reason(interface_name),
        });
    }
}

/// Why a Go interface's method requirement is never a dead-code candidate.
pub const GO_INTERFACE_REQUIREMENT_REASON: &str = "Go interface method requirement — it \
     declares what an implementer must provide, so no caller of its own is expected";

/// Why an ambient TypeScript declaration is never a dead-code candidate.
pub const TS_AMBIENT_REASON: &str = "ambient TypeScript declaration (`declare module`, `declare \
     global`, a module augmentation or a `.d.ts` shim) — it describes a type that lives \
     elsewhere, so no call site is ever expected";

/// Exempt every symbol an ambient declaration introduces.
///
/// `declare module '*.svg' { … }`, `declare global { interface Window … }` and
/// `declare module './logger' { interface Extra … }` add to a type the program
/// never names at the declaration — the compiler merges them into a module or
/// the global scope. A `.d.ts` file holds nothing else. The 2026-10-09 audit
/// found these published at 0.9; nothing calls them because nothing *can*.
///
/// Span containment over the tree's `ambient_declaration` nodes, so every
/// symbol nested in one — the module itself and the interfaces inside it — is
/// covered without each symbol arm having to know.
pub(super) fn ts_ambient_exemptions(
    path: &str,
    root: Node,
    symbols: &[ExtractedSymbol],
    wiring: &mut Vec<WiringAnnotation>,
) {
    let whole_file = crate::wiring::is_ambient_declaration(path);
    let mut ambient: Vec<(usize, usize)> = Vec::new();
    if !whole_file {
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "ambient_declaration" {
                ambient.push((node.start_byte(), node.end_byte()));
                continue;
            }
            push_children(node, &mut stack);
        }
        if ambient.is_empty() {
            return;
        }
    }
    for symbol in symbols {
        if symbol.kind == SymbolKind::File {
            continue;
        }
        let inside = whole_file
            || ambient.iter().any(|&(start, end)| {
                start <= symbol.span.start_byte && symbol.span.end_byte <= end
            });
        if inside {
            wiring.push(WiringAnnotation {
                kind: WiringKind::StructuralExempt,
                target_symbol: symbol.qualified_name.clone(),
                details: TS_AMBIENT_REASON.to_string(),
            });
        }
    }
}

/// Why a Rust item written `pub(crate)`, `pub(super)`, `pub(self)` or
/// `pub(in path)` cannot be called from outside its crate.
pub const RUST_CRATE_PRIVATE_PROOF: &str = "its restricted `pub(…)` visibility confines it to its \
     own crate, so nothing outside the corpus can call it";

/// Mark every Rust item whose visibility is restricted to its crate.
///
/// The extractor answers `is_exported` from the `pub` keyword, so `pub(crate)`
/// reads as exported and the item was never examined — yet the language itself
/// proves no other crate can name it, the same proof Go's `internal/` gives.
/// Emitted per symbol as [`WiringKind::ModulePrivateScope`]; liveness widens
/// it into the unconfirmed tier as `exported_unreferenced`, never above.
///
/// Joined by span start: an item's symbol span starts where its node does,
/// visibility included (attributes are sibling nodes).
pub(super) fn rust_crate_private_scopes(
    root: Node,
    source: &str,
    symbols: &[ExtractedSymbol],
    wiring: &mut Vec<WiringAnnotation>,
) {
    let mut restricted: HashSet<usize> = HashSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        if node.children(&mut cursor).any(|child| {
            child.kind() == "visibility_modifier"
                && get_node_text(child, source).starts_with("pub(")
        }) {
            restricted.insert(node.start_byte());
        }
        push_children(node, &mut stack);
    }
    if restricted.is_empty() {
        return;
    }
    for symbol in symbols {
        if symbol.is_exported && restricted.contains(&symbol.span.start_byte) {
            wiring.push(WiringAnnotation {
                kind: WiringKind::ModulePrivateScope,
                target_symbol: symbol.qualified_name.clone(),
                details: RUST_CRATE_PRIVATE_PROOF.to_string(),
            });
        }
    }
}

/// Whether a declaration sits at module/package level rather than inside a
/// callable or type body.
///
/// Module-level bindings are public API surface; a constant declared inside a
/// function is a local and belongs in nobody's graph.
pub(crate) fn is_module_level(node: Node) -> bool {
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        if is_callable_node(parent) {
            return false;
        }
        if matches!(
            parent.kind(),
            "class_declaration"
                | "class_definition"
                | "class_body"
                | "abstract_class_declaration"
                | "impl_item"
                | "trait_item"
                | "block"
                | "statement_block"
        ) {
            return false;
        }
        ancestor = bounded_parent(parent);
    }
    true
}

/// Emit an exported module-level Rust or Go binding as a `Variable` symbol.
///
/// The frozen Python baseline emits these too (misclassified as `function`);
/// devmap records them with the correct kind. Without them an exported
/// constant is invisible: it cannot be searched, nothing can reference it, and
/// it can be neither confirmed live nor reported dead — it simply is not in
/// the map. Only *exported* bindings are emitted here: these languages enforce
/// visibility, so a private constant is unreachable from any other file.
/// Python binds every module-scope name and TS/JS filters after the walk, once
/// `export { … }` clauses have been seen; both build the same symbol through
/// [`module_binding_symbol`].
pub(super) fn push_module_binding(
    node: Node,
    file_symbol_name: &str,
    name: String,
    is_exported: bool,
    span: Span,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    if !is_exported || name.is_empty() || !is_module_level(node) {
        return;
    }
    symbols.push(module_binding_symbol(
        file_symbol_name,
        name,
        is_exported,
        span,
    ));
}

/// Terraform address for an HCL block: `resource.aws_s3_bucket.b`.
///
/// A block is its type identifier followed by zero or more quoted labels.
/// Nested blocks (a `lifecycle` inside a `resource`) have no labels and are
/// attributes of their parent rather than addressable units, so they are
/// skipped rather than emitted under a misleading name.
pub(super) fn hcl_block_address(node: Node, source: &str) -> Option<String> {
    let block_type = node.named_child(0)?;
    if block_type.kind() != "identifier" {
        return None;
    }
    let mut parts = vec![get_node_text(block_type, source)];
    for index in 1..node.named_child_count() {
        let child = node.named_child(index)?;
        if child.kind() != "string_lit" {
            break;
        }
        parts.push(get_node_text(child, source).trim_matches('"').to_string());
    }
    if parts.len() < 2 || parts.iter().any(|part| part.is_empty()) {
        return None;
    }
    Some(parts.join("."))
}

/// Declaration node kinds shared across the broad grammar matrix.
///
/// The five specialised languages have their own arms; this table covers every
/// other linked grammar. Kinds were read from each grammar's own parse tree
/// rather than assumed — C in particular does not put a `name` field on
/// `function_definition`, so the name is recovered through its declarator.
pub(crate) fn generic_symbol_kind(kind: &str) -> Option<SymbolKind> {
    Some(match kind {
        "class_declaration" | "class_definition" | "class_specifier" | "class"
        | "object_definition" | "singleton_class" => SymbolKind::Class,
        // A Solidity contract is the unit that owns functions, like a class.
        "contract_declaration" | "library_declaration" => SymbolKind::Class,
        "interface_declaration" | "protocol_declaration" => SymbolKind::Interface,
        "struct_specifier" | "struct_declaration" | "struct_item" => SymbolKind::Struct,
        "enum_declaration" | "enum_specifier" | "enum_item" => SymbolKind::Enum,
        "trait_item" | "trait_declaration" => SymbolKind::Trait,
        "module" | "namespace_declaration" | "package_declaration" => SymbolKind::Module,
        // SQL. A relation is a named set of typed columns, which is what
        // `Struct` already means here, and a stored function/trigger is a
        // callable body. Only schema objects a code map can act on are mapped:
        // indexes, sequences, roles, databases and extensions are deliberately
        // left out because they name no callable and own no fields, so they
        // would add nodes nothing can traverse to.
        "create_table" | "create_view" | "create_materialized_view" | "create_type" => {
            SymbolKind::Struct
        }
        "create_function" | "create_trigger" => SymbolKind::Function,
        "create_schema" => SymbolKind::Module,
        "function_definition"
        | "function_declaration"
        | "function_item"
        | "method_declaration"
        | "method_definition"
        | "method"
        | "singleton_method"
        | "constructor_declaration"
        | "subroutine_declaration"
        | "function_declarator" => SymbolKind::Function,
        _ => return None,
    })
}

/// `(kind, name)` for a declaration node, or `None` when it is not one.
pub(crate) fn generic_declaration(node: Node, source: &str) -> Option<(SymbolKind, String)> {
    let kind = generic_symbol_kind(node.kind())?;
    // `function_declarator` exists to name a C-family function whose
    // `function_definition` carries no name field; taking both would emit the
    // same function twice.
    if node.kind() == "function_declarator"
        && bounded_parent(node).is_some_and(|parent| parent.kind() == "function_definition")
    {
        return None;
    }
    let name = generic_declaration_name(node, source)?;
    (!name.is_empty()).then_some((kind, name))
}

pub(super) fn generic_declaration_name(node: Node, source: &str) -> Option<String> {
    if let Some(name) = get_child_text(node, "name", source).filter(|name| !name.is_empty()) {
        return Some(name);
    }
    // SQL: a `create_*` node carries no `name` field. The name lives in an
    // `object_reference` child, which itself has a required `name` field —
    // taking that rather than the reference's whole text keeps a
    // schema-qualified `analytics.events` from becoming part of the identity.
    let mut ref_cursor = node.walk();
    for child in node.named_children(&mut ref_cursor) {
        if child.kind() == "object_reference" {
            if let Some(name) = get_child_text(child, "name", source).filter(|n| !n.is_empty()) {
                return Some(name);
            }
        }
    }
    // C family: the name lives inside the declarator.
    let declarator = node.child_by_field_name("declarator")?;
    if let Some(name) =
        get_child_text(declarator, "declarator", source).filter(|name| !name.is_empty())
    {
        return Some(name);
    }
    let mut declarator_cursor = declarator.walk();
    for child in declarator.named_children(&mut declarator_cursor) {
        if matches!(
            child.kind(),
            "identifier" | "field_identifier" | "type_identifier"
        ) {
            let text = get_node_text(child, source);
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

/// The shader-entry reason for a Metal declaration, read from its own leading
/// qualifier.
///
/// Both spellings occur and both are accepted: the bare qualifier
/// (`kernel void reduce(...)`) and the attribute form (`[[kernel]] void f()`,
/// `[[kernel, max_total_threads_per_threadgroup(256)]] void f()`). Only the
/// declaration's *leading* tokens are read, within a bounded window, so a
/// `kernel`-typed parameter or an address space further along the signature can
/// never promote an ordinary helper to an entry point.
pub(crate) fn metal_shader_entry_reason_of(node: Node, source: &str) -> Option<&'static str> {
    const HEAD_WINDOW: usize = 256;
    let start = node.start_byte();
    let end = node.end_byte().min(start + HEAD_WINDOW);
    let head = source.get(start..end)?.trim_start();
    if let Some(rest) = head.strip_prefix("[[") {
        let list = rest.split("]]").next().unwrap_or(rest);
        return list
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .find_map(crate::wiring::metal_shader_entry_reason);
    }
    let word: String = head
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect();
    crate::wiring::metal_shader_entry_reason(&word)
}

/// The decorator on a Python function whose root name is a parameter of an
/// enclosing callable, and that callable's name — or `None`.
///
/// `@test` and `@test.skip(…)` both qualify when `test` is a parameter: the
/// value is injected either way, and whatever it registers, the decision is
/// the caller's. The **nearest** enclosing callable that binds the root
/// decides, in either direction: as a parameter the decorator is injected; as
/// a local (`test = functools.lru_cache` one scope in) it is rebound and the
/// outer parameter is shadowed, so no claim is made. A root that is bound
/// nowhere on the way up — an import, a module-level function, `other` in
/// `@other.test` — is not injected, whatever an outer parameter is called.
///
/// Keyed on the binding rather than on the spelling on purpose. The decorator
/// hint table (`crate::wiring::is_wiring_decorator`) is pinned equal to the
/// Python kernel's and exempts the whole *file*; this claims one function, on
/// evidence the syntax carries.
pub(super) fn python_injected_decorator(node: Node, source: &str) -> Option<(String, String)> {
    let decorated =
        bounded_parent(node).filter(|parent| parent.kind() == "decorated_definition")?;
    let mut cursor = decorated.walk();
    let decorators: Vec<String> = decorated
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "decorator")
        .map(|decorator| get_node_text(decorator, source))
        .collect();
    for text in decorators {
        let base = text
            .trim()
            .trim_start_matches('@')
            .split('(')
            .next()
            .unwrap_or("")
            .trim();
        let root = base.split('.').next().unwrap_or("").trim();
        if !is_user_ident(root) {
            continue;
        }
        let mut ancestor = bounded_parent(decorated);
        while let Some(scope) = ancestor {
            if is_callable_node(scope) {
                let mut parameters = BTreeSet::new();
                collect_parameter_names(scope, source, &mut parameters);
                if parameters.contains(root) {
                    let owner = scope
                        .child_by_field_name("name")
                        .map(|name| get_node_text(name, source))
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| "<anonymous>".to_string());
                    return Some((base.to_string(), owner));
                }
                if with_scope_locals(scope, source, |locals| locals.contains(root)) {
                    break;
                }
            }
            ancestor = bounded_parent(scope);
        }
    }
    None
}

/// Nearest enclosing type-like declaration, so a method is owned by its type.
pub(crate) fn generic_enclosing_type(node: Node, source: &str) -> Option<String> {
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        if let Some(kind) = generic_symbol_kind(parent.kind()) {
            if !matches!(kind, SymbolKind::Function) {
                return get_child_text(parent, "name", source).filter(|name| !name.is_empty());
            }
            // A function inside a function is not owned by a type.
            return None;
        }
        ancestor = bounded_parent(parent);
    }
    None
}

/// The declaration's own header: everything before its body.
///
/// A visibility modifier precedes the name in every language that has one, so
/// this is the region a modifier can legally occupy. Returns the whole node
/// when no body can be identified, which is the previous behaviour and is safe
/// for declarations that have no body to confuse it with.
pub(super) fn declaration_header<'a>(node: Node, source: &'a str) -> &'a str {
    let body_start = node
        .child_by_field_name("body")
        .map(|body| body.start_byte())
        .or_else(|| {
            let mut cursor = node.walk();
            let found = node
                .children(&mut cursor)
                .find(|child| {
                    let kind = child.kind();
                    kind.ends_with("_body")
                        || kind == "block"
                        || kind == "declaration_list"
                        || kind == "field_declaration_list"
                        || kind == "template_body"
                })
                .map(|body| body.start_byte());
            found
        })
        .unwrap_or_else(|| node.end_byte());
    source
        .get(node.start_byte()..body_start.max(node.start_byte()))
        .unwrap_or("")
}

/// Visibility for grammars without a single export keyword.
///
/// Falls back to the leading-character convention only where a language
/// actually uses one; otherwise a declaration is treated as visible, which is
/// the safe direction — treating a public symbol as private would make it a
/// dead-code candidate on no evidence.
///
/// The scan is bounded to [`declaration_header`] because it previously read
/// `get_node_text(node, source)` — the whole subtree, bodies included — and so
/// answered a question about the declaration from text belonging to its
/// members. One `private val` inside a public Scala class returned `false`, and
/// `devmap dead` then reported that class at the 0.90 tier with no exemption
/// reason. Java escaped it only because `"public "` is tested first and Java
/// spells the modifier; that is an accident of one keyword set, not a rule.
/// The doc contract above — every `false` rests on evidence the language
/// actually provides — is only true once the evidence is the declaration's own.
pub(crate) fn generic_is_exported(node: Node, source: &str, name: &str) -> bool {
    let text = declaration_header(node, source);
    if text.starts_with("pub ") || text.contains("public ") || text.contains("export ") {
        return true;
    }
    if text.contains("private ") || text.contains("protected ") {
        return false;
    }
    !name.starts_with('_')
}
