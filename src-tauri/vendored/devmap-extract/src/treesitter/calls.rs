use super::*;

/// The longest a receiver expression may be recorded as.
///
/// X44. `receiver_expr` is documented as existing "so the classification can be
/// audited rather than trusted", which means being grouped and read. A receiver
/// that is a unique 38,644-character string — the measured maximum on this
/// repository, the whole body of one function, stored as the "receiver" of a
/// method called on the end of it — groups with nothing and answers no
/// question, while costing the store a megabyte of duplicated source.
///
/// 64 characters holds every receiver that is genuinely a path of names, which
/// is what this field is for. Past it the value is cut and **marked** cut, so a
/// truncated string can never be read as a whole expression.
pub(super) const MAX_RECEIVER_CHARS: usize = 64;

/// A receiver expression reduced to something a reader can group by.
///
/// One line, bounded, and marked when it was cut. Collapsing whitespace is part
/// of the identity and not cosmetic: 4,973 receivers on this repository contain
/// a newline, and every one of them is a block that was copied into a column
/// whose job is to name a value.
pub(super) fn bound_receiver_text(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = strip_prefix_operators(&flat);
    if flat.chars().count() <= MAX_RECEIVER_CHARS {
        return flat.to_string();
    }
    let mut out: String = flat.chars().take(MAX_RECEIVER_CHARS).collect();
    out.push('\u{2026}');
    out
}

/// Strip a leading `&` / `*` / `!` from a receiver that is otherwise a plain
/// path of names.
///
/// `(*path).to_string()` records the receiver `*path`, `(&x).use()` records
/// `&x`, and Swift's `!Self.check(v)` records `!Self` — tree-sitter-swift makes
/// `!Self` the navigation target rather than negating the call's result. The
/// classifier and the receiver-type rung both read a receiver's **leftmost
/// segment**, and `*` is not a segment: every one of these rows reached
/// `UninferredReceiver` with the binding that would have typed it sitting one
/// character to the right. Measured: 370 rows on this repository (`*path`,
/// `*name`, `&…`) and 107 on a 306-file Swift corpus, where `!Self.containsNul`
/// is a call to the enclosing type's own static method that the `self` rung
/// could not see.
///
/// **Only when the remainder is a plain path**, and that restriction is the
/// whole of the argument. A reference or a dereference denotes the *same value*
/// as its operand, so the operand's declared type is the receiver's and reading
/// it is not a guess. `!name.is_empty()` does not: it denotes a `bool` computed
/// from a call, and rooting it at `name` would claim the method belongs to
/// whatever `name` is. Requiring what follows to be an identifier or a dotted
/// path — no parentheses, no brackets, no further operators — separates them
/// exactly, and it is why `!Self` is taken and `!a.ok()` is left alone.
///
/// Arithmetic is deliberately absent. `-x` is a different value of a possibly
/// different type, and the reduction has no way to know; it costs a row and
/// invents nothing.
pub(crate) fn strip_prefix_operators(text: &str) -> &str {
    let mut rest = text;
    let mut stripped_any = false;
    // Bounded: `&&x` and `&mut *p` are real, a thousand leading `&` is not.
    for _ in 0..4 {
        let Some(next) = rest
            .strip_prefix('&')
            .or_else(|| rest.strip_prefix('*'))
            .or_else(|| rest.strip_prefix('!'))
            .map(|after| after.strip_prefix("mut ").unwrap_or(after))
            .map(str::trim_start)
            .filter(|next| !next.is_empty())
        else {
            break;
        };
        rest = next;
        stripped_any = true;
    }
    if !stripped_any {
        return text;
    }
    // What is left must be a path of names and nothing else: a parenthesis, a
    // bracket, a quote or a second operator means the operand was an
    // expression, not a binding, and its type is not the receiver's.
    //
    // `$` and `#` are names here for the reason `is_callee_identity` admits
    // them: Swift spells a closure's parameter `$0`, PHP spells every variable
    // `$r`, and a JavaScript private member is `#name`. Refusing them would
    // leave `!$0` and `&$row` rooted at an operator in the two languages whose
    // ordinary bindings look like that.
    let is_name_char =
        |character: char| character.is_alphanumeric() || matches!(character, '_' | '$' | '#');
    let is_plain_path = rest
        .chars()
        .next()
        .is_some_and(|first| first.is_alphabetic() || matches!(first, '_' | '$' | '#'))
        && rest
            .split(['.', ':'])
            .all(|segment| segment.chars().all(is_name_char));
    if is_plain_path {
        rest
    } else {
        text
    }
}

/// Grammar keys for a call, across the languages this crate splits receivers
/// for. A call's identity is the callee it names, never its argument list.
pub(super) fn is_call_node(kind: &str) -> bool {
    matches!(
        kind,
        "call"
            | "call_expression"
            | "function_call_expression"
            | "invocation_expression"
            | "macro_invocation"
            | "member_call_expression"
            | "method_call"
            | "method_invocation"
            | "new_expression"
            | "object_creation_expression"
            | "scoped_call_expression"
    )
}

/// Grammar keys for member access — the same three spellings
/// `member_access_receiver` already reconciles, plus Rust's and C's.
pub(super) fn member_access_fields(kind: &str) -> Option<(&'static str, &'static str)> {
    match kind {
        "attribute" => Some(("object", "attribute")),
        "member_expression" => Some(("object", "property")),
        "selector_expression" => Some(("operand", "field")),
        "field_expression" => Some(("value", "field")),
        _ => None,
    }
}

/// What a receiver expression *is*, rather than what it says.
///
/// X44. The receiver used to be `get_node_text` of the receiver node, whole, so
/// `runner.invoke(app, ["init"]).output.strip()` recorded its entire left-hand
/// side. The classifier reads only the receiver's leftmost segment, and a
/// reader auditing the ledger needs rows that group — neither is served by a
/// copy of the source.
///
/// The reduction is structural, not textual: a receiver that is a **call** is
/// named by that call's callee, and a member access is `<object identity>.
/// <property>`. Anything this walk does not recognise keeps its text, bounded.
pub(crate) fn receiver_identity(node: Node, source: &str, depth: usize) -> String {
    if depth > 16 {
        return bound_receiver_text(&get_node_text(node, source));
    }
    let fallback = || bound_receiver_text(&get_node_text(node, source));
    let kind = node.kind();
    if matches!(
        kind,
        "await_expression" | "parenthesized_expression" | "non_null_expression"
    ) {
        return match node.named_child(0) {
            Some(inner) => receiver_identity(inner, source, depth + 1),
            None => fallback(),
        };
    }
    if is_call_node(kind) {
        return node
            .child_by_field_name("function")
            .or_else(|| node.child_by_field_name("constructor"))
            .map(|target| {
                // The inner call's callee **with its own receiver**, not the
                // callee alone. Measured: reducing `runner.invoke(app, [...])`
                // to `invoke` cost 1,235 `External` classifications on this
                // repository, because the classifier roots its answer at the
                // receiver's leftmost segment and `runner` is where the
                // declared type `CliRunner` — and the import that proves it
                // external — is recorded. `runner.invoke` keeps that root and
                // still drops the argument list, which is the part that made
                // the string unique.
                let (name, receiver) = split_call_target_inner(target, source, depth + 1);
                match receiver.filter(|receiver| !receiver.is_empty()) {
                    Some(receiver) if !name.is_empty() => format!("{receiver}.{name}"),
                    _ => name,
                }
            })
            .map(|identity| bound_receiver_text(&identity))
            .filter(|identity| !identity.is_empty())
            .unwrap_or_else(fallback);
    }
    if let Some((object_field, member_field)) = member_access_fields(kind) {
        let member = node
            .child_by_field_name(member_field)
            .map(|child| get_node_text(child, source))
            .filter(|text| !text.is_empty());
        let object = node
            .child_by_field_name(object_field)
            .or_else(|| node.child_by_field_name("argument"))
            .map(|child| receiver_identity(child, source, depth + 1))
            .filter(|text| !text.is_empty());
        return match (object, member) {
            (Some(object), Some(member)) => bound_receiver_text(&format!("{object}.{member}")),
            _ => fallback(),
        };
    }
    fallback()
}

/// The receiver named by `field` on `node`, as an identity.
pub(super) fn receiver_from_field(
    node: Node,
    field: &str,
    source: &str,
    depth: usize,
) -> Option<String> {
    node.child_by_field_name(field)
        .map(|child| receiver_identity(child, source, depth + 1))
        .filter(|text| !text.is_empty())
}

pub(super) fn split_call_target_inner(
    function_node: Node,
    source: &str,
    depth: usize,
) -> (String, Option<String>) {
    // The unwrapping arms below recurse. Bounded so a pathological nesting
    // cannot exhaust the stack; past the bound the raw text is returned, which
    // is the pre-existing behaviour and joins to nothing.
    if depth > 16 {
        return (get_node_text(function_node, source), None);
    }
    match function_node.kind() {
        // Wrappers that stand between a call and its real callee.
        //
        // `await invoke<Raw>('x')` is the motivating case: with a type argument
        // present, tree-sitter makes the *await expression* the call's
        // `function` field, so the whole text `await invoke` became the callee
        // and matched no symbol. Plain `await invoke('x')` was never affected,
        // which is why this hid — it needs the generic argument to appear.
        "await_expression" | "parenthesized_expression" | "non_null_expression" => {
            match function_node.named_child(0) {
                Some(inner) => split_call_target_inner(inner, source, depth + 1),
                None => (get_node_text(function_node, source), None),
            }
        }
        // Turbofish: `row.get::<_, String>(0)`, `serde_json::from_str::<V>(s)`.
        // The type arguments are not part of the callee's identity, and leaving
        // them in produced a name no symbol can carry.
        "generic_function" => match function_node.child_by_field_name("function") {
            Some(inner) => split_call_target_inner(inner, source, depth + 1),
            None => (get_node_text(function_node, source), None),
        },
        // Rust path call: `Vec::new()`, `MyType::create()`, `std::fs::write()`.
        //
        // Without this the entire path was the callee, so a user type's
        // associated function was invisible to the graph — `MyType::create` is
        // a real edge this was dropping, not just library noise. The path
        // becomes the receiver, matching how `x.method()` already splits.
        "scoped_identifier" => {
            let name = get_child_text(function_node, "name", source);
            // `Wrapper::<u8>::new()`: the type arguments are not part of the
            // type's name, exactly as they are not part of a callee's.
            let path = function_node.child_by_field_name("path").map(|path| {
                match (path.kind(), path.child_by_field_name("type")) {
                    ("generic_type" | "generic_type_with_turbofish", Some(bare)) => {
                        get_node_text(bare, source)
                    }
                    _ => get_node_text(path, source),
                }
            });
            match (name, path) {
                (Some(name), path) if !name.is_empty() => (name, path),
                _ => (get_node_text(function_node, source), None),
            }
        }
        "attribute" => (
            get_child_text(function_node, "attribute", source).unwrap_or_default(),
            receiver_from_field(function_node, "object", source, depth),
        ),
        "member_expression" => (
            get_child_text(function_node, "property", source).unwrap_or_default(),
            receiver_from_field(function_node, "object", source, depth),
        ),
        // C++ scope resolution: `ns::fn()`, `S::sm()`, `a::b::c()`.
        //
        // The whole path was previously the callee, so a namespaced or static
        // member call named a symbol nothing can carry — the C++ shape of the
        // Rust `scoped_identifier` gap SC26 closed. tree-sitter-cpp nests these
        // to the *right* (`a::(b::c)`), so the walk recurses through `name` and
        // the receiver is the innermost scope — `b` for `a::b::c()`, which is
        // the type or namespace that actually owns `c`.
        "qualified_identifier" => match function_node.child_by_field_name("name") {
            Some(name) => {
                let (callee, inner_scope) = split_call_target_inner(name, source, depth + 1);
                let scope = inner_scope
                    .or_else(|| get_child_text(function_node, "scope", source))
                    .filter(|scope| !scope.is_empty());
                (callee, scope)
            }
            None => (get_node_text(function_node, source), None),
        },
        // C++ template call: `tmplFn<int>(5)`, `std::make_unique<S>()`. The
        // type arguments are not part of the callee's identity, exactly as
        // SC26 established for Rust's turbofish.
        "template_function" => match function_node.child_by_field_name("name") {
            Some(name) => split_call_target_inner(name, source, depth + 1),
            None => (get_node_text(function_node, source), None),
        },
        // C call through a dereferenced function pointer: `(*fp)(3)`. The
        // surrounding `parenthesized_expression` is already unwrapped above,
        // which leaves the `pointer_expression` between the call and the name.
        "pointer_expression" => match function_node.child_by_field_name("argument") {
            Some(inner) => split_call_target_inner(inner, source, depth + 1),
            None => (get_node_text(function_node, source), None),
        },
        // Rust method call: `receiver.method()` is a `field_expression`
        // whose `value` is the receiver and whose `field` is the method.
        //
        // tree-sitter-c/cpp use the same node kind for `s.m()` and `p->m()` but
        // name the receiver `argument` rather than `value`, so the receiver was
        // silently dropped for the entire C family. Rust has no `argument`
        // field here, so the fallback cannot change a Rust split.
        "field_expression" => {
            let field = get_child_text(function_node, "field", source);
            let value = receiver_from_field(function_node, "value", source, depth)
                .or_else(|| receiver_from_field(function_node, "argument", source, depth));
            match (field, value) {
                (Some(field), value) if !field.is_empty() => (field, value),
                _ => (get_node_text(function_node, source), None),
            }
        }
        "selector_expression" => {
            let field = get_child_text(function_node, "field", source)
                .or_else(|| get_child_text(function_node, "selector", source));
            let operand = receiver_from_field(function_node, "operand", source, depth);
            match (field, operand) {
                (Some(field), Some(operand)) if !field.is_empty() => (field, Some(operand)),
                _ => (get_node_text(function_node, source), None),
            }
        }
        // Go package-qualified type, reachable only as a composite literal's
        // type (`genai.Part{}`) — a call's `function` field is a
        // `selector_expression`, never this. Split it the same way, so the
        // package lands in the receiver instead of producing a callee named
        // `genai.Part`, which matches how `go_type_name` names a qualified type.
        "qualified_type" => {
            let name = get_child_text(function_node, "name", source);
            let package = get_child_text(function_node, "package", source);
            match (name, package) {
                (Some(name), package) if !name.is_empty() => (name, package),
                _ => (get_node_text(function_node, source), None),
            }
        }
        _ => (get_node_text(function_node, source), None),
    }
}

pub(super) fn go_receiver(node: Node, source: &str) -> Option<(String, String)> {
    let receiver = node.child_by_field_name("receiver")?;
    let mut receiver_cursor = receiver.walk();
    for param in receiver.named_children(&mut receiver_cursor) {
        if param.kind() != "parameter_declaration" {
            continue;
        }
        let recv_name = get_child_text(param, "name", source).unwrap_or_default();
        let type_node = param.child_by_field_name("type")?;
        let type_name = go_type_name(type_node, source, 0)?;
        return Some((recv_name, type_name));
    }
    None
}

thread_local! {
    /// One parser reused for macro-body probes. Creating a `Parser` per macro
    /// invocation would dominate the cost of the recovery below.
    pub(super) static MACRO_PROBE_PARSER: RefCell<Option<Parser>> = const { RefCell::new(None) };
}

/// Recover calls hidden inside a Rust macro invocation (SC13).
///
/// tree-sitter parses macro arguments as an unstructured `token_tree`, so
/// `println!("{}", s.work())` flattens to loose `identifier` tokens and the call
/// disappears from the graph entirely — not resolved weakly, absent. In real
/// Rust (`println!`, `assert!`, `format!`, `vec!`) that hides a large share of
/// calls, and any symbol reached only from inside a macro reads as dead.
///
/// The token text is re-parsed with the real grammar rather than pattern-matched
/// out of the flat token stream, so nesting, chained calls and closures come out
/// correctly. Wrapping the arguments in a synthetic call makes a comma-separated
/// argument list a valid expression position.
///
/// Fail-closed: a token tree that is not a valid argument list (a declarative
/// macro body, a pattern-matching macro) yields nothing rather than a guess.
/// Spans point at the macro invocation, which is where the call textually is.
pub(super) fn rust_macro_calls(
    node: Node,
    source: &str,
    file_symbol_name: &str,
) -> Vec<(String, Option<String>)> {
    let _ = file_symbol_name;
    let mut token_cursor = node.walk();
    let Some(tokens) = node
        .named_children(&mut token_cursor)
        .find(|child| child.kind() == "token_tree")
    else {
        return Vec::new();
    };
    let Some(inner) = macro_token_body(&get_node_text(tokens, source)) else {
        return Vec::new();
    };

    // Macros nest — `assert_eq!(a, format!("{}", b.c()))` hides a call two
    // levels down — so inner macro bodies are queued and probed in turn rather
    // than recursed into, which keeps the thread-local parser borrow scoped to
    // one parse at a time. The depth bound stops a pathological nest from
    // running away.
    const MAX_MACRO_DEPTH: usize = 4;
    let mut pending = vec![(inner, 0usize)];
    let mut found = Vec::new();
    while let Some((body, depth)) = pending.pop() {
        if depth > MAX_MACRO_DEPTH {
            continue;
        }
        let (calls, nested) = probe_macro_body(&body);
        found.extend(calls);
        pending.extend(nested.into_iter().map(|body| (body, depth + 1)));
    }
    found.sort();
    found.dedup();
    found
}

/// Most implicit calls one file records.
pub(super) const MAX_IMPLICIT_CALLS: usize = 20_000;

/// The operator, index, drop and `into` calls a Rust file makes without writing
/// one, as candidates (see `Extraction::rust_implicit_calls`).
///
/// Each is attached only to an operand whose type the syntax or a binding
/// states: a constructor expression (`V(1)`, `V { .. }`, `T::new()`), a unit
/// struct value, or a plain local (the resolver types it from its binding).
/// Literals and casts are numbers and never qualify.
pub(super) fn rust_collect_implicit_calls(
    root: Node,
    source: &str,
    file_symbol_name: &str,
) -> Vec<ExtractedCall> {
    let mut calls = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if walk_deadline_passed() || calls.len() >= MAX_IMPLICIT_CALLS {
            break;
        }
        let candidate: Option<(&str, Option<String>)> = match node.kind() {
            "binary_expression" | "compound_assignment_expr" => {
                let op = node
                    .child_by_field_name("operator")
                    .map(|op| get_node_text(op, source));
                let callee = match op.as_deref() {
                    Some("+") => Some("add"),
                    Some("-") => Some("sub"),
                    Some("*") => Some("mul"),
                    Some("/") => Some("div"),
                    Some("%") => Some("rem"),
                    Some("&") => Some("bitand"),
                    Some("|") => Some("bitor"),
                    Some("^") => Some("bitxor"),
                    Some("<<") => Some("shl"),
                    Some(">>") => Some("shr"),
                    Some("+=") => Some("add_assign"),
                    Some("-=") => Some("sub_assign"),
                    Some("*=") => Some("mul_assign"),
                    Some("/=") => Some("div_assign"),
                    _ => None,
                };
                callee.map(|callee| {
                    (
                        callee,
                        node.child_by_field_name("left")
                            .and_then(|left| rust_typed_operand(left, source)),
                    )
                })
            }
            "index_expression" => Some((
                "index",
                node.named_child(0)
                    .filter(|value| matches!(value.kind(), "identifier" | "field_expression"))
                    .map(|value| receiver_identity(value, source, 0)),
            )),
            "let_declaration" => rust_let_implicit_call(node, source),
            _ => None,
        };
        if let Some((callee, Some(receiver))) = candidate {
            calls.push(ExtractedCall {
                caller_symbol: enclosing_callable_qualified(node, source, file_symbol_name),
                callee_name: callee.to_string(),
                receiver_expr: Some(receiver),
                span: node_span(node),
            });
        }
        push_named_children(node, &mut stack);
    }
    calls.sort_by_key(|call| call.span.start_byte);
    calls
}

/// The receiver text for an operand whose type is stated: see
/// [`rust_collect_implicit_calls`].
///
/// Parentheses and borrows are unwrapped in a loop, at most
/// [`TYPE_NESTING_MAX_DEPTH`] of them: this runs on the left operand of every
/// binary expression, and recursing once per `(` let a source of nested
/// parentheses overflow the extraction thread's stack.
pub(super) fn rust_typed_operand(operand: Node, source: &str) -> Option<String> {
    let mut operand = operand;
    let mut unwrapped = 0usize;
    while matches!(
        operand.kind(),
        "parenthesized_expression" | "reference_expression"
    ) {
        if unwrapped == TYPE_NESTING_MAX_DEPTH {
            note_nesting_cap("rust_typed_operand");
            return None;
        }
        unwrapped += 1;
        operand = operand
            .child_by_field_name("value")
            .or_else(|| operand.named_child(0))?;
    }
    match operand.kind() {
        "identifier" => Some(get_node_text(operand, source)),
        "struct_expression" => rust_type_name(operand.child_by_field_name("name")?, source, 0),
        "call_expression" => {
            let function = operand.child_by_field_name("function")?;
            let named = match function.kind() {
                "identifier" => get_node_text(function, source),
                "scoped_identifier" => get_node_text(function.child_by_field_name("path")?, source)
                    .rsplit("::")
                    .next()?
                    .to_string(),
                _ => return None,
            };
            named
                .starts_with(|c: char| c.is_ascii_uppercase())
                .then(|| receiver_identity(operand, source, 0))
        }
        _ => None,
    }
}

/// `drop` for `let g = <constructed value>;` and `from` for `let m: T =
/// x.into();`.
pub(super) fn rust_let_implicit_call(
    node: Node,
    source: &str,
) -> Option<(&'static str, Option<String>)> {
    let value = node.child_by_field_name("value")?;
    if let Some(ty) = node.child_by_field_name("type") {
        if value.kind() == "call_expression" {
            let function = value.child_by_field_name("function")?;
            let is_into = function.kind() == "field_expression"
                && get_child_text(function, "field", source).as_deref() == Some("into")
                && value
                    .child_by_field_name("arguments")
                    .is_some_and(|arguments| arguments.named_child_count() == 0);
            if is_into {
                return Some(("from", rust_type_name(ty, source, 0)));
            }
        }
        return None;
    }
    let receiver = match value.kind() {
        "identifier" => get_node_text(value, source)
            .starts_with(|c: char| c.is_ascii_uppercase())
            .then(|| get_node_text(value, source)),
        "struct_expression" | "call_expression" => rust_typed_operand(value, source),
        _ => None,
    };
    Some(("drop", receiver))
}

/// The `From::from` candidates a file's `?` operators make: see
/// `Extraction::rust_try_conversions`.
pub(super) fn rust_collect_try_conversions(
    root: Node,
    source: &str,
    file_symbol_name: &str,
) -> Vec<ExtractedCall> {
    // Each function of this file by name, with its declared error type; a name
    // declared twice with different error types (cfg twins, methods of two
    // types) is not one function and identifies nothing.
    let mut error_types: HashMap<String, Option<String>> = HashMap::new();
    let mut functions = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if walk_deadline_passed() {
            return Vec::new();
        }
        if node.kind() == "function_item" {
            if let Some(name) = get_child_text(node, "name", source) {
                let error = rust_result_error_type(node, source);
                error_types
                    .entry(name)
                    .and_modify(|known| {
                        if *known != error {
                            *known = None;
                        }
                    })
                    .or_insert(error);
            }
            functions.push(node);
        }
        push_named_children(node, &mut stack);
    }

    let mut calls = Vec::new();
    for function in functions {
        let Some(error) = rust_result_error_type(function, source) else {
            continue;
        };
        let Some(body) = function.child_by_field_name("body") else {
            continue;
        };
        let mut stack = vec![body];
        while let Some(node) = stack.pop() {
            if walk_deadline_passed() || calls.len() >= MAX_IMPLICIT_CALLS {
                return calls;
            }
            match node.kind() {
                // These return their own type, so a `?` inside converts into
                // theirs; a nested function is visited as its own.
                "closure_expression" | "async_block" | "function_item" => continue,
                "try_expression" => {
                    let identity = node
                        .named_child(0)
                        .and_then(|operand| rust_called_function_name(operand, source))
                        .and_then(|name| error_types.get(&name))
                        .is_some_and(|known| known.as_deref() == Some(error.as_str()));
                    if !identity {
                        calls.push(ExtractedCall {
                            caller_symbol: enclosing_callable_qualified(
                                node,
                                source,
                                file_symbol_name,
                            ),
                            callee_name: "from".to_string(),
                            receiver_expr: Some(error.clone()),
                            span: node_span(node),
                        });
                    }
                }
                _ => {}
            }
            push_named_children(node, &mut stack);
        }
    }
    calls.sort_by_key(|call| call.span.start_byte);
    calls
}

/// `E` for a function declared `-> Result<T, E>` (any path ending `Result`
/// with exactly two type arguments), reduced to its bare name. An alias with
/// one argument (`io::Result<T>`) states no error type here.
pub(super) fn rust_result_error_type(function: Node, source: &str) -> Option<String> {
    let returned = function.child_by_field_name("return_type")?;
    if returned.kind() != "generic_type" {
        return None;
    }
    let head = get_node_text(returned.child_by_field_name("type")?, source);
    if head.rsplit("::").next() != Some("Result") {
        return None;
    }
    let arguments = returned.child_by_field_name("type_arguments")?;
    let mut cursor = arguments.walk();
    let types: Vec<Node> = arguments
        .named_children(&mut cursor)
        .filter(|child| !matches!(child.kind(), "line_comment" | "block_comment"))
        .collect();
    match types.as_slice() {
        [_, error] => rust_type_name(*error, source, 0),
        _ => None,
    }
}

/// The function a call expression names by its last segment: `f(..)`,
/// `m::f(..)`, `Self::f(..)`, `x.f(..)`. Anything else is not a named call.
pub(super) fn rust_called_function_name(operand: Node, source: &str) -> Option<String> {
    if operand.kind() != "call_expression" {
        return None;
    }
    let mut function = operand.child_by_field_name("function")?;
    // `f::<T>(..)`: the turbofish wraps the path once.
    if function.kind() == "generic_function" {
        function = function.child_by_field_name("function")?;
    }
    match function.kind() {
        "identifier" => Some(get_node_text(function, source)),
        "scoped_identifier" => get_child_text(function, "name", source),
        "field_expression" => get_child_text(function, "field", source),
        _ => None,
    }
}

/// Give a call through a `const`/`static` function pointer this file declares
/// the call the pointer makes.
///
/// `pub const F: fn() -> u8 = helper;` then `F()` calls `helper`, and the only
/// place that is written is the item's initializer. Like the macro expansion
/// above, textual and stated as such: only an item whose declared type is a
/// function pointer and whose value is a plain path (`helper`, `m::helper`),
/// called by its bare name; a closure, a cast, or a name declared twice with
/// different targets adds nothing. The call to the item itself stays.
pub(super) fn rust_calls_through_fn_pointer_items(
    root: Node,
    source: &str,
    calls: &mut Vec<ExtractedCall>,
) {
    let mut targets: HashMap<String, Option<(String, Option<String>)>> = HashMap::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if walk_deadline_passed() {
            return;
        }
        if matches!(node.kind(), "const_item" | "static_item")
            && node
                .child_by_field_name("type")
                .is_some_and(|ty| ty.kind() == "function_type")
        {
            let target = node
                .child_by_field_name("value")
                .and_then(|value| match value.kind() {
                    "identifier" => Some((get_node_text(value, source), None)),
                    "scoped_identifier" => Some((
                        get_child_text(value, "name", source)?,
                        Some(get_node_text(value.child_by_field_name("path")?, source)),
                    )),
                    _ => None,
                });
            if let Some(name) = get_child_text(node, "name", source) {
                targets
                    .entry(name)
                    .and_modify(|known| {
                        if *known != target {
                            *known = None;
                        }
                    })
                    .or_insert(target);
            }
            continue;
        }
        push_named_children(node, &mut stack);
    }
    if targets.values().all(Option::is_none) {
        return;
    }
    let added: Vec<ExtractedCall> = calls
        .iter()
        .filter(|call| call.receiver_expr.is_none())
        .filter_map(|call| {
            let (callee_name, receiver_expr) = targets.get(&call.callee_name)?.clone()?;
            Some(ExtractedCall {
                caller_symbol: call.caller_symbol.clone(),
                callee_name,
                receiver_expr,
                span: call.span.clone(),
            })
        })
        .take(MAX_IMPLICIT_CALLS)
        .collect();
    calls.extend(added);
}

/// Most calls and invocation references one file's `macro_rules!` expansions
/// may add.
pub(super) const MAX_MACRO_EXPANSION_CALLS: usize = 10_000;

/// Give each invocation of a `macro_rules!` macro this file defines the calls
/// that macro's rules make.
///
/// `log!("x")` with `macro_rules! log { ($m:expr) => { $crate::emit($m) } }`
/// calls `emit`, and the only place that call is written is the rule's body,
/// which the grammar parses as a loose token tree and which belongs to no
/// function. Read as the token stream it is (the same scan a macro argument
/// the grammar cannot parse gets), every call on a rule's right-hand side is
/// attributed to the callable that invokes the macro. Textual, and stated as
/// such: a call whose callee is built from a macro variable is not a plain
/// identifier and is not read, and a macro this file does not define (it comes
/// from `use`, or another crate) is not expanded — that is the import
/// resolver's question.
///
/// An invocation inside a macro definition is not expanded, so a macro that
/// invokes itself cannot grow the result, and the total is bounded.
///
/// Each definition is also a [`SymbolKind::Macro`] symbol (exported when it is
/// `#[macro_export]`, keyed under its function when it is declared inside
/// one), and each invocation of it in this file references it by name. An
/// invocation of a macro another file defines (`#[macro_use]`, a `use`, or
/// `$crate::m!`) is not linked, which is why liveness never reports a macro.
pub(super) fn rust_expand_local_macros(
    root: Node,
    source: &str,
    file_symbol_name: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    let mut bodies: HashMap<String, Vec<(String, Option<String>)>> = HashMap::new();
    let mut invocations = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if walk_deadline_passed() {
            return;
        }
        match node.kind() {
            "macro_definition" => {
                if let Some(name) = get_child_text(node, "name", source) {
                    let parent = enclosing_callable_qualified(node, source, file_symbol_name);
                    let qualified_name = match &parent {
                        Some(function) => format!("{function}.{name}"),
                        None => format!("{file_symbol_name}::{name}"),
                    };
                    if !symbols
                        .iter()
                        .any(|symbol| symbol.qualified_name == qualified_name)
                    {
                        symbols.push(ExtractedSymbol {
                            name: name.clone(),
                            qualified_name,
                            kind: SymbolKind::Macro,
                            span: node_span(node),
                            is_exported: rust_attribute_paths(node, source)
                                .iter()
                                .any(|path| path == "macro_export"),
                            docstring: None,
                            signature: None,
                            parent_symbol: Some(
                                parent.unwrap_or_else(|| file_symbol_name.to_string()),
                            ),
                            body_signature: None,
                            declaration_hash: None,
                            return_type: None,
                        });
                    }
                    let mut found = Vec::new();
                    let mut cursor = node.walk();
                    for rule in node
                        .named_children(&mut cursor)
                        .filter(|child| child.kind() == "macro_rule")
                    {
                        let Some(right) = rule.child_by_field_name("right") else {
                            continue;
                        };
                        let text = get_node_text(right, source);
                        let inner = text
                            .strip_prefix(['(', '[', '{'])
                            .and_then(|rest| rest.strip_suffix([')', ']', '}']))
                            .unwrap_or(&text);
                        // `$crate` is the defining crate's root, which is `crate`
                        // to every caller in this file.
                        found.extend(scan_rust_token_tree_calls(
                            &inner.replace("$crate", "crate"),
                        ));
                    }
                    found.sort();
                    found.dedup();
                    bodies.insert(name, found);
                }
                // The rules' own invocations are the macro's, not a caller's.
                continue;
            }
            "macro_invocation" => invocations.push(node),
            _ => {}
        }
        push_named_children(node, &mut stack);
    }
    let mut added = 0usize;
    for node in invocations {
        let Some(name) = node
            .child_by_field_name("macro")
            .map(|name| get_node_text(name, source))
        else {
            continue;
        };
        let Some(body) = bodies.get(&name) else {
            continue;
        };
        added += 1;
        if added > MAX_MACRO_EXPANSION_CALLS {
            return;
        }
        references.push(extracted_reference(
            node,
            source,
            file_symbol_name,
            name.clone(),
            ReferenceKind::Name,
            None,
        ));
        let caller = enclosing_callable_qualified(node, source, file_symbol_name);
        for (callee_name, receiver_expr) in body {
            added += 1;
            if added > MAX_MACRO_EXPANSION_CALLS {
                return;
            }
            let receiver_expr = receiver_expr.clone();
            let span = node_span(node);
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
                callee_name: callee_name.clone(),
                receiver_expr,
                span,
            });
        }
    }
}

/// The functions a `generate_handler![ .. ]` invocation lists, as `(name,
/// path receiver)`: `commands::cmd` is `("cmd", Some("commands"))`, a bare `cmd`
/// has no receiver.
///
/// Only that macro, matched on its last path segment (`generate_handler!` or
/// `tauri::generate_handler!`). The list is comma separated paths, each
/// optionally preceded by `#[cfg(..)]`; an item that is not a plain path is
/// skipped rather than guessed at.
pub(super) fn rust_handler_list(node: Node, source: &str) -> Vec<(String, Option<String>)> {
    let Some(macro_name) = node.child_by_field_name("macro") else {
        return Vec::new();
    };
    let macro_name = get_node_text(macro_name, source);
    if macro_name.rsplit("::").next().map(str::trim) != Some("generate_handler") {
        return Vec::new();
    }
    let mut cursor = node.walk();
    let Some(tokens) = node
        .named_children(&mut cursor)
        .find(|child| child.kind() == "token_tree")
    else {
        return Vec::new();
    };
    let text = get_node_text(tokens, source);
    let Some(inner) = text
        .strip_prefix(['(', '[', '{'])
        .and_then(|rest| rest.strip_suffix([')', ']', '}']))
    else {
        return Vec::new();
    };
    let mut listed = Vec::new();
    for item in inner.split(',') {
        let mut item = item.trim();
        while let Some(rest) = item.strip_prefix("#[") {
            let Some(close) = rest.find(']') else {
                item = "";
                break;
            };
            item = rest[close + 1..].trim_start();
        }
        if item.is_empty() || !item.split("::").all(is_user_ident) {
            continue;
        }
        let (receiver, name) = match item.rsplit_once("::") {
            Some((receiver, name)) => (Some(receiver.to_string()), name),
            None => (None, item),
        };
        listed.push((name.to_string(), receiver));
    }
    listed
}

/// Strip a token tree's outer delimiter pair, if it has one worth probing.
pub(super) fn macro_token_body(raw: &str) -> Option<String> {
    let inner = raw
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
        .or_else(|| {
            raw.strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
        })
        .or_else(|| {
            raw.strip_prefix('{')
                .and_then(|rest| rest.strip_suffix('}'))
        })?;
    // No parenthesis means no call to recover; skip the parse entirely.
    (inner.contains('(') && !inner.trim().is_empty()).then(|| inner.to_string())
}

/// Parse one macro body and return `(calls, nested macro bodies)`.
///
/// Returns nothing at all when the body is not a valid argument list — a
/// declarative macro definition, a pattern-matching macro. A fabricated call is
/// worse than a missing one.
pub(super) fn probe_macro_body(inner: &str) -> (Vec<(String, Option<String>)>, Vec<String>) {
    const PROBE: &str = "__devmap_macro_probe";
    let probe_source = format!("fn {PROBE}() {{ {PROBE}({inner}); }}");

    let parsed = MACRO_PROBE_PARSER.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            let mut parser = Parser::new();
            if parser
                .set_language(&tree_sitter_rust::LANGUAGE.into())
                .is_err()
            {
                return None;
            }
            *slot = Some(parser);
        }
        slot.as_mut()
            .and_then(|parser| parse_probe(parser, &probe_source))
    });

    let Some(tree) = parsed else {
        // A probe stopped by the file's deadline has latched the refusal of
        // the whole file; the token scan is for a parser that declined, not
        // more unbounded work after the clock ran out.
        if walk_overran() {
            return (Vec::new(), Vec::new());
        }
        return (scan_rust_token_tree_calls(inner), Vec::new());
    };
    let root = tree.root_node();
    if root.has_error() {
        // `select! { a => { self.f() } }` is not a valid argument list, so the
        // probe parse fail-closes. Scanning the token text recovers the calls
        // that are ident-shaped and skips comments, strings, and `ident!`
        // macros — nothing is invented from punctuation.
        return (scan_rust_token_tree_calls(inner), Vec::new());
    }

    let mut calls = Vec::new();
    let mut nested = Vec::new();
    let mut stack = vec![root];
    while let Some(current) = stack.pop() {
        match current.kind() {
            "call_expression" => {
                if let Some(function) = current.child_by_field_name("function") {
                    if let Some((callee, receiver)) = split_call_target(function, &probe_source) {
                        if callee != PROBE {
                            calls.push((callee, receiver));
                        }
                    }
                }
            }
            "macro_invocation" => {
                let mut nested_cursor = current.walk();
                // Bound in its own statement so the borrowing iterator is
                // dropped here rather than living to the end of the `if let`.
                let tokens = current
                    .named_children(&mut nested_cursor)
                    .find(|child| child.kind() == "token_tree");
                if let Some(tokens) = tokens {
                    if let Some(body) = macro_token_body(&get_node_text(tokens, &probe_source)) {
                        nested.push(body);
                    }
                }
            }
            _ => {}
        }
        push_named_children(current, &mut stack);
    }
    (calls, nested)
}

/// Recover `ident(` / `recv.ident(` / `Type::ident(` from a token tree the
/// grammar could not parse as an argument list.
///
/// Fail-closed: comments, strings, char literals, lifetimes, and `ident!`
/// macros contribute nothing. Keywords that can be followed by `(` (`if`,
/// `match`, `return`, …) are not callees.
pub(super) fn scan_rust_token_tree_calls(source: &str) -> Vec<(String, Option<String>)> {
    const KEYWORDS: &[&str] = &[
        "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "crate",
        "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in",
        "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
        "return", "Self", "self", "static", "struct", "super", "trait", "true", "try", "type",
        "typeof", "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
    ];
    let chars: Vec<char> = source.chars().collect();
    let mut index = 0usize;
    let mut pending_callee: Option<String> = None;
    let mut pending_receiver: Option<String> = None;
    let mut after_dot = false;
    let mut after_path = false;
    let mut after_bang = false;
    let mut found = Vec::new();

    while index < chars.len() {
        let ch = chars[index];
        if ch.is_whitespace() {
            index += 1;
            continue;
        }
        if ch == '/' && index + 1 < chars.len() {
            if chars[index + 1] == '/' {
                index += 2;
                while index < chars.len() && chars[index] != '\n' {
                    index += 1;
                }
                continue;
            }
            if chars[index + 1] == '*' {
                index += 2;
                while index + 1 < chars.len() && !(chars[index] == '*' && chars[index + 1] == '/') {
                    index += 1;
                }
                index = index.saturating_add(2).min(chars.len());
                continue;
            }
        }
        if ch == '"' {
            pending_callee = None;
            pending_receiver = None;
            after_dot = false;
            after_path = false;
            after_bang = false;
            index += 1;
            while index < chars.len() {
                if chars[index] == '\\' {
                    index = index.saturating_add(2);
                    continue;
                }
                if chars[index] == '"' {
                    index += 1;
                    break;
                }
                index += 1;
            }
            continue;
        }
        if ch == '\'' {
            pending_callee = None;
            pending_receiver = None;
            after_dot = false;
            after_path = false;
            after_bang = false;
            if index + 2 < chars.len() && chars[index + 2] == '\'' {
                index += 3;
                continue;
            }
            if index + 1 < chars.len() && chars[index + 1] == '\\' {
                index += 2;
                while index < chars.len() && chars[index] != '\'' {
                    index += 1;
                }
                index = index.saturating_add(1).min(chars.len());
                continue;
            }
            index += 1;
            while index < chars.len()
                && (chars[index].is_ascii_alphanumeric() || chars[index] == '_')
            {
                index += 1;
            }
            continue;
        }
        if ch.is_ascii_alphabetic() || ch == '_' {
            let start = index;
            index += 1;
            while index < chars.len()
                && (chars[index].is_ascii_alphanumeric() || chars[index] == '_')
            {
                index += 1;
            }
            let ident: String = chars[start..index].iter().collect();
            if after_dot {
                pending_receiver = pending_callee.take();
                pending_callee = Some(ident);
                after_dot = false;
            } else if after_path {
                // The whole path before the name, as a call written outside a
                // macro records it: `crate::macros::emit` has the receiver
                // `crate::macros`, not its last segment.
                pending_receiver = match (pending_receiver.take(), pending_callee.take()) {
                    (Some(path), Some(segment)) => Some(format!("{path}::{segment}")),
                    (None, segment) => segment,
                    (path, None) => path,
                };
                pending_callee = Some(ident);
                after_path = false;
            } else {
                pending_callee = Some(ident);
                pending_receiver = None;
            }
            continue;
        }
        if ch == '.' {
            after_dot = true;
            after_path = false;
            after_bang = false;
            index += 1;
            continue;
        }
        if ch == ':' && index + 1 < chars.len() && chars[index + 1] == ':' {
            after_path = true;
            after_dot = false;
            after_bang = false;
            index += 2;
            continue;
        }
        if ch == '!' {
            after_bang = pending_callee.is_some();
            after_dot = false;
            after_path = false;
            index += 1;
            continue;
        }
        if ch == '(' {
            if after_bang {
                after_bang = false;
                pending_callee = None;
                pending_receiver = None;
                index += 1;
                continue;
            }
            if let Some(callee) = pending_callee.take() {
                if is_user_ident(&callee) && !KEYWORDS.contains(&callee.as_str()) {
                    found.push((callee, pending_receiver.take()));
                }
            }
            pending_receiver = None;
            after_dot = false;
            after_path = false;
            index += 1;
            continue;
        }
        pending_callee = None;
        pending_receiver = None;
        after_dot = false;
        after_path = false;
        after_bang = false;
        index += 1;
    }
    found.sort();
    found.dedup();
    found
}

/// Qualified name for a symbol that has no enclosing *type*.
///
/// A definition inside a function body belongs to that function, not to the
/// file. Qualifying it `file::name` made every same-named local definition in a
/// file collapse to one identity (SC14): `run` declared inside 15 different
/// test functions became one node, `struct Out` declared in three different
/// functions became one node. A qualified name is the graph's join key, so
/// collisions are not a cosmetic naming issue — they make distinct code
/// indistinguishable to every edge that names it.
///
/// Top-level definitions are unaffected: with no enclosing callable this is
/// exactly the previous `file::name`.
pub(super) fn scoped_qualified_name(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    name: &str,
) -> String {
    match enclosing_callable_qualified(node, source, file_symbol_name) {
        Some(scope) => format!("{scope}.{name}"),
        None => format!("{file_symbol_name}::{name}"),
    }
}

/// Strip references, pointers and generics down to a Rust type's bare name.
///
/// Bounded depth for the same reason as `rust_type_qualifier` below: the
/// unwrapping arms recurse once per layer, and `&&&…&T` written deeply enough
/// exhausts the stack. A stack overflow aborts the process, so one hostile or
/// generated file would take the whole build with it. Past the bound the type
/// is simply not recovered, which costs a receiver binding and guesses nothing.
/// The single type argument of a deref-transparent wrapper node, or `None`.
///
/// The tree-walking half of the rule [`crate::deref`] owns; the list of wrapper
/// names lives there and is shared with the resolver, which asks the same
/// question of written text. See that module for why `Vec` must never be in it.
///
/// Measured, and the reason this exists:
/// `Service::run_idle_reaper(service: Arc<Service>, …)` calling
/// `service.wait_tick(tick)` **22 lines below the definition in the same file**
/// produced no edge, so `Service.wait_tick` was published as dead at
/// confidence 0.9 with no exemption — the tier whose contract is "safe to act
/// on". The type argument was dropped here, at extraction, so the resolver
/// never had a `Service` to fail to match: it was handed the string `"Arc"`,
/// which is a perfectly plain identifier and so passed every admissibility
/// check on the way down.
///
/// Requires *exactly one* named type argument, which is what keeps
/// `Vec<Store>` reducing to `Vec` and `rust_type_names_reduce_to_the_bare_identifier`
/// passing.
pub(super) fn rust_deref_transparent_argument<'tree>(
    node: Node<'tree>,
    source: &str,
) -> Option<Node<'tree>> {
    let outer = node.child_by_field_name("type")?;
    let text = get_node_text(outer, source);
    let bare = text.rsplit("::").next().unwrap_or(&text);
    let bare = bare.split('<').next().unwrap_or(bare).trim();
    if !crate::deref::RUST_DEREF_TRANSPARENT.contains(&bare) {
        return None;
    }
    let args = node.child_by_field_name("type_arguments")?;
    let mut cursor = args.walk();
    let named: Vec<Node<'tree>> = args.named_children(&mut cursor).collect();
    match named.as_slice() {
        [only] => Some(*only),
        _ => None,
    }
}

/// The node a `generic_type`'s nominal identity is read from.
///
/// **One owner for the decision, deliberately**, because the name and the
/// qualifier are read by two different functions and must agree. They descend
/// the tree separately, so if this choice were made twice
/// `std::sync::Arc<Service>` could take the name `Service` down one path and
/// the qualifier `std` down the other, and the pair would assert that `Service`
/// is declared in `std` — a *fabricated* qualification, which is worse than the
/// missing edge this whole change exists to fix.
///
/// Falls back to the wrapper itself whenever the inner type yields no name, so
/// `Box<dyn Error>` still reduces to `Box` exactly as it did before. That makes
/// this change strictly additive: a shape that resolved before resolves to the
/// same thing now.
///
/// The "yields a name" test is [`rust_type_name_is_reachable`], a constant-time
/// look at the node's own kind, and **not** a trial call to `rust_type_name`.
/// The trial call is what the first draft of this function did, and it is
/// quadratic-shaped in the worst way: the caller recurses into the node this
/// returns, so every level of `Arc<Arc<…>>` would visit its subtree twice and a
/// chain at the depth bound costs 2^16 node visits per annotation — before the
/// qualifier walk doubles it again. Bounded, but for no reason.
pub(super) fn rust_generic_identity_node<'tree>(
    node: Node<'tree>,
    source: &str,
) -> Option<Node<'tree>> {
    if let Some(inner) = rust_deref_transparent_argument(node, source) {
        if rust_type_name_is_reachable(inner) {
            return Some(inner);
        }
    }
    node.child_by_field_name("type")
}

/// Whether [`rust_type_name`] has an arm for this node kind.
///
/// Exactly the kinds that function matches, so the two cannot drift into
/// disagreeing about whether a node names a type. A shape missing here is
/// treated as unnamed and the wrapper's own name is used — the direction that
/// preserves the previous answer rather than inventing a new one.
pub(super) fn rust_type_name_is_reachable(node: Node) -> bool {
    matches!(
        node.kind(),
        "reference_type"
            | "pointer_type"
            | "generic_type"
            | "dynamic_type"
            | "abstract_type"
            | "type_identifier"
            | "scoped_type_identifier"
    )
}
