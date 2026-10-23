use super::*;

/// Grammar keys that parse the C family.
///
/// Metal has no grammar of its own — its spec routes it to `cpp` — so `"cpp"`
/// covers both, exactly as `is_metal_path` exists to tell them apart again when
/// a rule holds for one and not the other.
pub(crate) fn is_c_family_grammar(lang: &str) -> bool {
    matches!(lang, "c" | "cpp" | "objc" | "cuda")
}

/// Whether `path` is a C-family header rather than an implementation file.
///
/// In C, C++ and Objective-C the header *is* the declaration surface: a
/// definition in a `.c`/`.cpp`/`.m` is an implementation detail unless a header
/// publishes it. This is the only structural evidence of visibility the family
/// has, and `c_family_is_exported` is its only consumer.
pub(super) fn is_c_header_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    C_HEADER_EXTENSIONS
        .iter()
        .any(|extension| lower.ends_with(extension))
}

/// The C-family header extensions, taken from the frozen `LANGUAGE_SPECS`:
/// `.h` is C's, `.hh`/`.hpp`/`.hxx` are C++'s, `.cuh` is CUDA's. An extension
/// the registry does not list never reaches a C-family grammar, so adding one
/// here would be configuration that can never fire. `devmap-analyze` keeps a
/// mirror of this list, and the two are pinned to agree by
/// `the_two_header_tables_agree_extension_by_extension`.
pub(super) const C_HEADER_EXTENSIONS: &[&str] = &[".h", ".hh", ".hpp", ".hxx", ".cuh"];

/// A declaration's own leading tokens: the text before its declarator, bounded.
///
/// Two C-family facts are absent from the parse tree and can only be read here.
/// tree-sitter-cuda drops `__global__`/`__device__` entirely — probing
/// `__global__ void kern(int*){}` yields a `function_definition` carrying no
/// trace of the qualifier — and Metal's `kernel`/`vertex`/`fragment` are parsed
/// by the C++ grammar, which does not model them either. Stopping at the
/// declarator means a parameter, an attribute on an argument, or a body token
/// can never promote an ordinary helper, and the window bounds the scan on a
/// large declaration.
pub(super) fn c_declaration_head<'a>(node: Node, source: &'a str) -> &'a str {
    const HEAD_WINDOW: usize = 256;
    let start = node.start_byte();
    let end = node
        .child_by_field_name("declarator")
        .map_or_else(|| node.end_byte(), |declarator| declarator.start_byte())
        .min(start + HEAD_WINDOW)
        .max(start);
    // The window edge is an arbitrary byte offset, so it lands mid-character
    // whenever the head contains any multi-byte text — a comment, an identifier
    // in a non-ASCII script. `.get()` then yielded `None` and the whole head
    // became `""`, so `head_has_word("__global__")` answered `false` for a file
    // whose first ten bytes are `__global__`, and the kernel lost the
    // entry-point exemption that keeps it out of the dead-code report.
    let end = crate::langcalls::scope::floor_char_boundary(source, end).max(start);
    source.get(start..end).unwrap_or_default()
}

/// Whether `head` contains `word` as a whole token rather than as a substring.
///
/// `static` must not be found inside `staticAssert`, and `kernel` must not be
/// found inside `kernelSize`.
pub(super) fn head_has_word(head: &str, word: &str) -> bool {
    head.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .any(|token| token == word)
}

/// Whether a C-family declaration carries explicit evidence of an external
/// interface.
///
/// Only markers that *say so* count. `extern "C"` exists so another language can
/// call the symbol, and `dllexport`/`visibility` exist so a shared object can;
/// each is direct evidence that a call site may lie outside the indexed corpus.
/// A bare non-`static` definition is deliberately not evidence: it has external
/// linkage, but so does every ordinary helper in every `.c` file, which is
/// exactly why linkage alone cannot serve as the visibility test.
pub(super) fn c_declaration_is_explicitly_external(node: Node, source: &str) -> bool {
    let head = c_declaration_head(node, source);
    if head.contains("dllexport") || head.contains("visibility") {
        return true;
    }
    // `extern "C" { … }` and `extern "C" int f()` both wrap the declaration in a
    // linkage specification. Stop at the first enclosing body so a declaration
    // merely nested somewhere inside one cannot inherit it.
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        match parent.kind() {
            "linkage_specification" => return true,
            "function_definition" | "compound_statement" | "translation_unit" => return false,
            _ => ancestor = bounded_parent(parent),
        }
    }
    false
}

/// Visibility of a C-family declaration.
///
/// Anything declared in a header is public API and must never become a
/// dead-code candidate. A definition in an implementation file is not itself an
/// export — the header publishes it — so it is visible only on the explicit
/// evidence above. That puts C on the same footing as Python, where a
/// module-level function is likewise not "exported" and an uncalled one is a
/// candidate.
///
/// This replaces `generic_is_exported`, whose fallback is `!name.starts_with('_')`
/// and therefore answered `true` for 1,393 of the 1,464 C-family symbols on the
/// measurement corpus. Every one of them was exempt, so the C family had no
/// dead-code analysis at all: 1,094 reports, all of them at confidence 0.3
/// "Exported or exempt".
pub(crate) fn c_family_is_exported(node: Node, source: &str, path: &str) -> bool {
    is_c_header_path(path) || c_declaration_is_explicitly_external(node, source)
}

/// The node a C-family declarator chain ultimately names.
///
/// `function_definition -> function_declarator -> identifier` is the common
/// shape; pointer, reference and parenthesized declarators sit in between for
/// anything returning or holding a pointer.
pub(super) fn c_declarator_name_node(declarator: Node) -> Option<Node> {
    let mut current = declarator;
    for _ in 0..16 {
        match current.kind() {
            "identifier"
            | "field_identifier"
            | "type_identifier"
            | "qualified_identifier"
            | "destructor_name"
            | "operator_name" => return Some(current),
            _ => {
                current = current
                    .child_by_field_name("declarator")
                    .or_else(|| current.named_child(0))?;
            }
        }
    }
    None
}

/// Whether a C-family declarator chain declares a parameter list.
///
/// This is what makes a definition a *function* rather than something the
/// grammar could not parse. The walk follows `declarator` links and the
/// parenthesised/pointer wrappers that sit between, so
/// `int (*handler(int))(char)` still answers yes.
pub(super) fn c_declarator_declares_parameters(declarator: Node) -> bool {
    let mut current = declarator;
    for _ in 0..16 {
        if current.kind() == "function_declarator" {
            return true;
        }
        let Some(next) = current
            .child_by_field_name("declarator")
            .or_else(|| current.named_child(0))
        else {
            return false;
        };
        current = next;
    }
    false
}

/// Splits a C++ qualified name into `(innermost scope, final name)`.
///
/// tree-sitter-cpp nests these to the right — `a::b::c` is `a::(b::c)` — so the
/// scope that owns the final name is the *innermost* one. `void ns::S::m()`
/// therefore yields `(Some("S"), "m")`, which is the type the in-class
/// declaration would have given, rather than the namespace.
pub(super) fn split_qualified_identifier(node: Node, source: &str) -> (Option<String>, String) {
    let mut scope = None;
    let mut current = node;
    for _ in 0..16 {
        if current.kind() != "qualified_identifier" {
            break;
        }
        let Some(name) = current.child_by_field_name("name") else {
            break;
        };
        scope = current
            .child_by_field_name("scope")
            .map(|node| get_node_text(node, source))
            .filter(|text| !text.is_empty());
        current = name;
    }
    (scope, get_node_text(current, source))
}

/// The full Objective-C selector a method definition declares: `run`, `setX:`,
/// `a:b:`.
///
/// The whole selector, not its first part, is the method's identity — `-a:b:`
/// and `-a:c:` are different methods. The call site is built by the matching
/// rule in `objc_message_selector`, so definition and call join.
pub(super) fn objc_selector(node: Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    let mut selector = String::new();
    for (index, child) in children.iter().enumerate() {
        if child.kind() != "identifier" {
            continue;
        }
        let text = get_node_text(*child, source);
        if text.is_empty() {
            continue;
        }
        selector.push_str(&text);
        if children
            .get(index + 1)
            .is_some_and(|next| next.kind() == "method_parameter")
        {
            selector.push(':');
        }
    }
    (!selector.is_empty()).then_some(selector)
}

/// The selector a `message_expression` sends: `[self run]` is `run`,
/// `[self a:1 b:2]` is `a:b:`.
///
/// tree-sitter-objc gives a keyword message one `method` field *per* selector
/// part, so `child_by_field_name` sees only the first and would report `a:b:`
/// as `a` — an identity that no two-part method carries.
pub(super) fn objc_message_selector(node: Node, source: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut argument_count = 0usize;
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let field = cursor.field_name();
            let child = cursor.node();
            match field {
                Some("method") => parts.push(get_node_text(child, source)),
                Some("receiver") => {}
                _ => {
                    // Anything else named, once a selector part has been seen,
                    // is an argument to it.
                    if child.is_named() && !parts.is_empty() {
                        argument_count += 1;
                    }
                }
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    if parts.is_empty() || parts.iter().any(String::is_empty) {
        return None;
    }
    Some(if argument_count == 0 {
        parts.concat()
    } else {
        parts
            .iter()
            .map(|part| format!("{part}:"))
            .collect::<String>()
    })
}

/// Name of an Objective-C `@interface`/`@implementation` container.
///
/// tree-sitter-objc puts the class name in an unnamed `identifier` child rather
/// than in a `name` field, so the generic `name`-field lookup finds nothing.
pub(super) fn objc_container_name(node: Node, source: &str) -> Option<String> {
    get_child_text(node, "name", source)
        .filter(|name| !name.is_empty())
        .or_else(|| {
            let mut cursor = node.walk();
            let found = node
                .named_children(&mut cursor)
                .find(|child| child.kind() == "identifier")
                .map(|child| get_node_text(child, source))
                .filter(|name| !name.is_empty());
            found
        })
}

/// Whether this Objective-C `@interface` is also implemented in the same file.
///
/// `@interface Foo` and `@implementation Foo` are two halves of one class, so
/// emitting a symbol for each puts two nodes carrying one qualified name into
/// the graph — the duplicate-identity defect SC14 tracks, and a broken join key.
/// The implementation wins because it is the definition; a header carrying only
/// the `@interface` still yields the class, and a `.h`/`.m` pair are different
/// files whose qualified names never collided in the first place.
pub(super) fn objc_interface_is_implemented_here(node: Node, source: &str) -> bool {
    let implementation_kind = match node.kind() {
        "class_interface" => "class_implementation",
        "category_interface" => "category_implementation",
        _ => return false,
    };
    let Some(name) = objc_container_name(node, source) else {
        return false;
    };
    let mut root = node;
    while let Some(parent) = bounded_parent(root) {
        root = parent;
    }
    let mut cursor = root.walk();
    let found = root.named_children(&mut cursor).any(|sibling| {
        sibling.kind() == implementation_kind
            && objc_container_name(sibling, source).as_deref() == Some(name.as_str())
    });
    found
}

/// Whether `node` is the name of a C-family type specifier that has no body,
/// i.e. a *use* of an already-declared type rather than a declaration of one.
///
/// C spells `struct chunk *prev` and `struct chunk { … }` with the same node
/// kind and the same `name` field; only the definition carries a body.
pub(super) fn is_bodyless_type_specifier_name(node: Node) -> bool {
    let Some(parent) = bounded_parent(node) else {
        return false;
    };
    matches!(
        parent.kind(),
        "struct_specifier" | "union_specifier" | "enum_specifier" | "class_specifier"
    ) && parent.child_by_field_name("body").is_none()
        && parent
            .child_by_field_name("name")
            .is_some_and(|name| name.id() == node.id())
}

/// Whether `node` is the identifier an Objective-C declaration names itself
/// with.
///
/// Objective-C hangs both a container's name and a method's selector parts off
/// *unnamed* `identifier` children, so none of them is reachable through the
/// `name`-field tests in `is_defining_name` and every declaration read as a
/// reference to itself — the same shape as the C-family declarator defect, in
/// the one C-family grammar that does not use declarators.
///
/// A superclass or an adopted protocol is deliberately still a reference: only
/// the container's *first* identifier is its own name.
pub(super) fn is_objc_declaring_identifier(node: Node) -> bool {
    if node.kind() != "identifier" {
        return false;
    }
    let Some(parent) = bounded_parent(node) else {
        return false;
    };
    match parent.kind() {
        "class_interface"
        | "class_implementation"
        | "category_interface"
        | "category_implementation"
        | "protocol_declaration" => {
            let mut cursor = parent.walk();
            let first = parent
                .named_children(&mut cursor)
                .find(|child| child.kind() == "identifier")
                .map(|child| child.id());
            first == Some(node.id())
        }
        // Go also spells a method `method_declaration` and JavaScript spells one
        // `method_definition`, but both name it in a `name` field. Requiring the
        // field to be absent keeps this rule to the grammar that has no other
        // way of naming a method.
        "method_definition" | "method_declaration" => parent.child_by_field_name("name").is_none(),
        _ => false,
    }
}

/// `(owner, name)` a C-family callable carries as a graph node.
///
/// **One canonical owner for C-family identity**, read by the symbol emitter and
/// by `enclosing_callable_qualified` alike. SC14 recorded that deriving member
/// identity and call-scope identity through two independent walks is what makes
/// them silently disagree; for the C family they did disagree in two ways at
/// once — `callable_binding_name` looks for a `name` field no C-family
/// declaration has, and `enclosing_type_name` knows nothing of
/// `struct_specifier` — so an edge out of any C-family function named a source
/// no node carried. Both now come from here.
///
/// `Some(owner)` for a member, `None` for a free function. An out-of-line
/// definition (`int S::m(…)`) resolves to the same `(S, m)` its in-class
/// declaration would, so the two are one symbol rather than two.
pub(super) fn c_callable_identity(node: Node, source: &str) -> Option<(Option<String>, String)> {
    if node.kind() == "method_definition" {
        // Objective-C. The JS grammar reuses this node kind but puts the name in
        // a `name` field, so a JS method never reaches the selector rule.
        if node.child_by_field_name("name").is_none() {
            let selector = objc_selector(node, source)?;
            return Some((objc_enclosing_container(node, source), selector));
        }
        return None;
    }
    if is_c_macro_invocation(node) {
        return None;
    }
    let declarator = node.child_by_field_name("declarator")?;
    // A function definition declares parameters. Anything else that a C-family
    // grammar reports as a `function_definition` is a construct it could not
    // parse, and there are two common ones — both of which appear thousands of
    // times in a real header tree.
    //
    // `.h` is ambiguous between C and C++ and routes to the C grammar, which
    // has no notion of a namespace, so `namespace at { … }` parses as a
    // definition with `namespace` as the return type and `at` as the whole
    // declarator. LibTorch declares `namespace at` in 2,486 of 3,028 sampled
    // headers, and taking each as a function put 2,486 nodes named `at` into
    // one corpus. `struct TORCH_API Foo { … }` misparses the same way, the
    // macro standing where the grammar expects the declarator.
    //
    // Neither has a parameter list, and every real definition does — including
    // constructors, destructors, conversion operators and trailing-return-type
    // functions, all of which keep a `function_declarator` in the chain.
    if !c_declarator_declares_parameters(declarator) {
        return None;
    }
    let name_node = c_declarator_name_node(declarator)?;
    let (scope, name) = if name_node.kind() == "qualified_identifier" {
        split_qualified_identifier(name_node, source)
    } else {
        (None, get_node_text(name_node, source))
    };
    if name.is_empty() {
        return None;
    }
    // A qualified declarator names its own owner; otherwise the owner is the
    // type whose body lexically encloses the definition.
    Some((scope.or_else(|| c_enclosing_type(node, source)), name))
}

/// Whether a C-family `function_definition` is really a function-like macro
/// invocation that happens to be followed by a brace-enclosed block.
///
/// `PYBIND11_MODULE(NAME, m) { … }`, `TORCH_LIBRARY(ops, m) { … }` and
/// `CMARK_DEFINE_LOCK(arena)` are macros, but each has a function's *shape*, and
/// the grammar runs no preprocessor, so each is reported as a
/// `function_definition`. The discriminator is the return type: a C or C++
/// function definition carries one, and the only definitions that legitimately
/// do not are constructors, destructors and conversion operators, which either
/// name their type in a qualified declarator or sit inside a class body.
///
/// Suppressed here, in the single canonical owner of C-family identity, rather
/// than at the symbol emitter: a macro that stops being a symbol must also stop
/// being a *call scope*, or the calls inside its block would name a source
/// symbol no node carries — the orphaned-edge failure of SC9/SC10. Attributing
/// them to the file is what the code did before any of this and is truthful.
///
/// Accepted cost: a K&R-era definition relying on implicit `int` is suppressed
/// too. That errs toward omitting a symbol rather than toward inventing one,
/// which is the safe direction for dead-code reporting.
pub(super) fn is_c_macro_invocation(node: Node) -> bool {
    if node.kind() != "function_definition" || node.child_by_field_name("type").is_some() {
        return false;
    }
    let Some(name_node) = node
        .child_by_field_name("declarator")
        .and_then(c_declarator_name_node)
    else {
        return false;
    };
    if name_node.kind() != "identifier" {
        return false;
    }
    // An in-class constructor is a bare `identifier` with no return type too, so
    // anything inside a type body is left alone.
    !matches!(
        bounded_parent(node).map(|parent| parent.kind()),
        Some("field_declaration_list")
    )
}

/// Nearest enclosing Objective-C class container.
pub(super) fn objc_enclosing_container(node: Node, source: &str) -> Option<String> {
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        if matches!(
            parent.kind(),
            "class_implementation"
                | "class_interface"
                | "category_implementation"
                | "category_interface"
        ) {
            return objc_container_name(parent, source);
        }
        ancestor = bounded_parent(parent);
    }
    None
}

/// Nearest enclosing C-family type whose body contains `node`.
///
/// Stops at a function body so a type declared inside a function does not adopt
/// a definition that merely follows it, and stops at a `function_definition` so
/// a nested lambda is not made a member.
pub(super) fn c_enclosing_type(node: Node, source: &str) -> Option<String> {
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        match parent.kind() {
            "struct_specifier" | "class_specifier" | "union_specifier" => {
                return get_child_text(parent, "name", source).filter(|name| !name.is_empty());
            }
            "class_implementation"
            | "class_interface"
            | "category_implementation"
            | "category_interface" => return objc_container_name(parent, source),
            "function_definition" | "compound_statement" => return None,
            _ => ancestor = bounded_parent(parent),
        }
    }
    None
}

/// Whether `node` sits inside a C-family attribute.
///
/// `__attribute__((visibility("default")))` parses its contents as a real
/// `call_expression`, so without this guard every such attribute would emit a
/// call to `visibility` — a callee that can never resolve and that names
/// nothing in the program.
pub(super) fn is_inside_c_attribute(node: Node) -> bool {
    let mut ancestor = bounded_parent(node);
    for _ in 0..16 {
        let Some(parent) = ancestor else {
            return false;
        };
        if matches!(
            parent.kind(),
            "attribute_specifier"
                | "attribute_declaration"
                | "attribute"
                | "ms_declspec_modifier"
                | "alignas_qualifier"
        ) {
            return true;
        }
        ancestor = bounded_parent(parent);
    }
    false
}

/// `(kind, owner, name)` for a C-family declaration node.
///
/// Diverges from `generic_declaration` in exactly the places where the C family
/// does: functions take their identity from `c_callable_identity`, Objective-C
/// containers name themselves in an unnamed child, and a bare prototype is not a
/// declaration this graph records.
pub(crate) fn c_family_declaration(
    node: Node,
    source: &str,
) -> Option<crate::langdecl::Declaration> {
    match node.kind() {
        "class_implementation"
        | "class_interface"
        | "category_implementation"
        | "category_interface" => {
            if objc_interface_is_implemented_here(node, source) {
                return None;
            }
            crate::langdecl::Declaration::new(
                SymbolKind::Class,
                None,
                objc_container_name(node, source)?,
            )
        }
        "function_definition" | "method_definition" => {
            let (owner, name) = c_callable_identity(node, source)?;
            crate::langdecl::Declaration::new(SymbolKind::Function, owner, name)
        }
        // A function-like macro is a callable in C, and SC31 made that
        // observable: once C-family calls are extracted, `ACTIONS(1)` is
        // recorded as a call, and with no symbol behind the `#define` the graph
        // holds a call whose target it never emitted. On this repository that
        // asymmetry alone put 13,630 rows into the defect tier — 7,372 of them
        // `ACTIONS` inside one generated `parser.c` table — which is precisely
        // the drowning-out that SC18 and SC30 exist to prevent.
        //
        // Only the *function-like* form. `preproc_def` (`#define MAX 10`) is a
        // constant: it carries no `parameters`, it is never the callee of a
        // call, and emitting it would add a node per object-like macro for no
        // resolution benefit. Recording the call while refusing the definition
        // is the asymmetry being fixed here, so the test pins both directions.
        "preproc_function_def" => {
            let name = get_child_text(node, "name", source)?;
            if name.is_empty() || node.child_by_field_name("parameters").is_none() {
                return None;
            }
            crate::langdecl::Declaration::new(SymbolKind::Function, None, name)
        }
        // A prototype declares; it does not define. Emitting it alongside the
        // definition would put two nodes carrying one name into the graph, and
        // an ambiguous resolution downgrades every call to that name to the
        // speculative tier — which `analyze_liveness` reports as
        // `only_ambiguous_callers` rather than as a resolved call. The header
        // stays the visibility evidence (`c_family_is_exported`) without
        // becoming a second symbol.
        //
        // This is also what the code already did, by accident:
        // `generic_declaration_name` looks one level too deep for a declarator
        // that is a bare identifier and returned `None` for every prototype.
        // Making it explicit keeps the new identity rule from silently
        // reversing it.
        "function_declarator" => None,
        // `struct arena_chunk *prev` is a *use* of a type, not a declaration of
        // one, and C spells the use with the same node kind as the definition —
        // the difference is that only a definition has a `body`.
        //
        // Without this every mention of an elaborated type emitted another
        // symbol carrying that type's name: `arena.c` alone produced eight
        // `arena_chunk` nodes for one struct. Duplicate qualified names are a
        // broken join key (SC14), and with C-family visibility now meaningful
        // each copy also became its own dead-code candidate.
        "struct_specifier" | "union_specifier" | "enum_specifier" | "class_specifier" => {
            node.child_by_field_name("body")?;
            let kind = generic_symbol_kind(node.kind())?;
            let name = generic_declaration_name(node, source).filter(|name| !name.is_empty())?;
            crate::langdecl::Declaration::new(kind, generic_enclosing_type(node, source), name)
        }
        _ => {
            let kind = generic_symbol_kind(node.kind())?;
            let name = generic_declaration_name(node, source).filter(|name| !name.is_empty())?;
            crate::langdecl::Declaration::new(kind, generic_enclosing_type(node, source), name)
        }
    }
}

/// Why a C-family function is reached without an observable call site.
///
/// Each is evidence from the declaration itself, never a guess from the name
/// alone: `main` is the program entry point the C standard names, a CUDA
/// `__global__` function is launched from host code by name, and an
/// Objective-C method is reachable through the runtime's selector dispatch —
/// delegates, target/action and protocol conformance all invoke methods that no
/// call expression in the corpus mentions. Metal's shader qualifiers are handled
/// by `metal_shader_entry_reason_of`, which already owns that rule.
pub(crate) fn c_family_entry_point_reason(
    node: Node,
    source: &str,
    path: &str,
    name: &str,
) -> Option<&'static str> {
    if node.kind() == "method_definition" {
        return Some("Objective-C method reachable by runtime selector dispatch");
    }
    if name == "main" && !is_c_header_path(path) {
        return Some("Program entry point");
    }
    // libFuzzer defines these two names as the harness's entry points and calls
    // them by name from its own driver, exactly as the C standard defines
    // `main`. Neither is ever called from the corpus that declares it.
    if matches!(name, "LLVMFuzzerTestOneInput" | "LLVMFuzzerInitialize") {
        return Some("libFuzzer harness entry point");
    }
    if head_has_word(c_declaration_head(node, source), "__global__") {
        return Some("CUDA kernel launched by name from host code");
    }
    None
}

/// The interface a C-family header publishes.
///
/// A prototype in a header is precisely an export declaration: it says this
/// unit's interface contains this name, and it is the only place C, C++ and
/// Objective-C state that. `analyze_liveness` joins these against definitions in
/// implementation files, which is the cross-file half of C-family visibility —
/// the same split SC6a used for Go interfaces, where extraction records the
/// per-file evidence and analysis performs the join because only it holds every
/// file at once.
///
/// Recorded as an *export* rather than as a symbol deliberately. A prototype and
/// its definition are one function, so emitting a symbol for each would put two
/// nodes carrying one name into the graph and make every call to that name
/// resolve ambiguously — measured on this corpus as the difference between one
/// `cmark_node` node and nine, where the nine made 841 real type references
/// unresolvable. `Extraction::exports` feeds no edge and no symbol index, so
/// this adds evidence without adding graph.
pub(super) fn extract_c_header_export(
    node: Node,
    source: &str,
    path: &str,
    exports: &mut Vec<ExtractedExport>,
) {
    if !is_c_header_path(path) {
        return;
    }
    let exported_name = match node.kind() {
        // C and C++: `int yaml_emitter_delete(yaml_emitter_t *emitter);`
        "function_declarator" => {
            // A definition's own declarator is not a declaration of an
            // interface; the definition is already the symbol.
            if bounded_parent(node).is_some_and(|parent| parent.kind() == "function_definition") {
                return;
            }
            let Some(name_node) = c_declarator_name_node(node) else {
                return;
            };
            if name_node.kind() == "qualified_identifier" {
                split_qualified_identifier(name_node, source).1
            } else {
                get_node_text(name_node, source)
            }
        }
        // Objective-C: a method spec inside `@interface`/`@protocol`.
        "method_declaration" => match objc_selector(node, source) {
            Some(selector) => selector,
            None => return,
        },
        _ => return,
    };
    if exported_name.is_empty() {
        return;
    }
    exports.push(ExtractedExport {
        exported_name,
        local_name: None,
        module_specifier: None,
        span: node_span(node),
    });
}

/// Calls made by C, C++, Objective-C, CUDA and Metal code.
///
/// The C family reached `extract_node`'s generic arm, which emits declarations
/// and nothing else, so **not one C-family call was extracted** — measured as 0
/// `Calls` edges across 183 real C-family files and 9,082 C++ headers. Every
/// consumer that reads the call graph (`impact`, `trace`, dead code, the PDG)
/// was answering for the whole family from nothing.
///
/// The shapes are taken from each grammar's own parse tree rather than assumed:
/// a plain call, a call through a function pointer and a CUDA `<<<…>>>` launch
/// are all `call_expression` with a `function` field, so they need no separate
/// handling, while `new` and Objective-C messages are distinct node kinds.
pub(super) fn extract_c_family_call(
    lang: &str,
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    if is_inside_c_attribute(node) {
        return;
    }
    if node.kind() == "preproc_function_def" {
        extract_c_macro_body_calls(lang, node, source, file_symbol_name, calls, references);
        return;
    }
    let span = node_span(node);
    let (callee_name, receiver_expr, target, reference_kind) = match node.kind() {
        "call_expression" => {
            let Some(function) = node.child_by_field_name("function") else {
                return;
            };
            if is_anonymous_callable(function.kind()) {
                return;
            }
            let Some((callee, receiver)) = split_call_target(function, source) else {
                return;
            };
            (callee, receiver, function, ReferenceKind::Call)
        }
        // `new S(…)` constructs `S`. Fail closed on `new int[10]`: a predeclared
        // type names no user symbol, so recording it would manufacture a callee
        // that can never resolve — the rule SC17 established for Go composite
        // literals.
        "new_expression" => {
            let Some(type_node) = node.child_by_field_name("type") else {
                return;
            };
            if type_node.kind() == "primitive_type" {
                return;
            }
            let Some((callee, receiver)) = split_call_target(type_node, source) else {
                return;
            };
            (callee, receiver, type_node, ReferenceKind::Constructor)
        }
        "message_expression" => {
            let Some(selector) = objc_message_selector(node, source) else {
                return;
            };
            let receiver = node
                .child_by_field_name("receiver")
                .map(|receiver| get_node_text(receiver, source))
                .filter(|text| is_user_ident(text));
            (selector, receiver, node, ReferenceKind::Call)
        }
        _ => return,
    };
    if callee_name.is_empty() || !is_user_ident(&callee_name.replace(':', "")) {
        return;
    }
    references.push(extracted_reference(
        target,
        source,
        file_symbol_name,
        callee_name.clone(),
        reference_kind,
        assignment_binding(node, source),
    ));
    calls.push(ExtractedCall {
        caller_symbol: enclosing_callable_qualified(node, source, file_symbol_name),
        callee_name,
        receiver_expr,
        span,
    });
}

thread_local! {
    /// One parser per grammar, reused for C-family macro-body probes — the
    /// same economy as `MACRO_PROBE_PARSER`, keyed by grammar because a Metal
    /// body is C++ and a CUDA body is CUDA.
    pub(super) static C_MACRO_PROBE_PARSER: RefCell<Option<(&'static str, Parser)>> =
        const { RefCell::new(None) };
}

/// Longest `#define` body the probe parses.
///
/// Past it the macro is simply not read, which is the fail-open direction for
/// this rule: a missing edge is one more finding a reader sees, never a hidden
/// one. No function-like macro in any corpus measured here comes within two
/// orders of magnitude of it.
pub(crate) const C_MACRO_BODY_MAX_BYTES: usize = 64 * 1024;

/// Parse a C-family fragment that exists only in memory — a macro body, or a
/// macro invocation's expansion — with `lang`'s grammar, on this thread's
/// reused probe parser, against the file's deadline ([`parse_probe`]).
pub(crate) fn parse_c_probe(lang: &str, text: &str) -> Option<tree_sitter::Tree> {
    let (grammar, language) = grammar_for(lang)?;
    C_MACRO_PROBE_PARSER.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.as_ref().is_none_or(|(held, _)| *held != grammar) {
            let mut parser = Parser::new();
            if parser.set_language(&language).is_err() {
                return None;
            }
            *slot = Some((grammar, parser));
        }
        slot.as_mut()
            .and_then(|(_, parser)| parse_probe(parser, text))
    })
}

/// Most probe-tree nodes one macro body is walked for.
pub(super) const C_MACRO_PROBE_MAX_NODES: usize = 50_000;

/// The name the statement-shaped probe wraps a body in, and never a callee.
pub(super) const C_MACRO_PROBE: &str = "__devmap_macro_probe";

/// Calls written inside a function-like macro's body, attributed to the macro.
///
/// The grammar leaves a `#define` body as one `preproc_arg` token, so nothing
/// inside it is a node and no call in it was ever recorded. The macro itself is
/// a symbol (`c_family_declaration` emits `preproc_function_def` as a
/// function), and every use of it is recorded as a call, so a helper the macro
/// body calls had a live caller with no edge to it. Measured on MLSystemsLab:
/// nine Metal `inline` template helpers, each stamped into a kernel by a
/// `KERNEL(...)` macro and each published confidently dead.
///
/// The body is parsed, not scanned — the way [`probe_macro_body`] treats a
/// Rust macro invocation — by the file's own grammar, twice: bare, for a
/// declaration-shaped body (`kernel void NAME(…) { … }`), and wrapped in a
/// function, for a statement-shaped one. A `call_expression` outside any
/// `ERROR` subtree in either parse is a syntactic call; a name inside a comment
/// or a string, a keyword before `(`, and a token the grammar could not place
/// are not. The macro's own parameters are excluded: `NAME(float *p)` is the
/// stamped kernel's declarator, not a call to `NAME`.
///
/// Line continuations are removed and `##` is pasted first, which is exactly
/// what the preprocessor does with them. Stringification (`#x`) is left as
/// written and parses as an error around the one token, costing at most the
/// calls in that statement.
pub(super) fn extract_c_macro_body_calls(
    lang: &str,
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    let Some(name) = get_child_text(node, "name", source).filter(|name| is_user_ident(name)) else {
        return;
    };
    let Some(value) = node.child_by_field_name("value") else {
        return;
    };
    let raw = get_node_text(value, source);
    if raw.len() > C_MACRO_BODY_MAX_BYTES {
        return;
    }
    let mut parameters = BTreeSet::new();
    if let Some(params) = node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for child in params.named_children(&mut cursor) {
            if child.kind() == "identifier" {
                parameters.insert(get_node_text(child, source));
            }
        }
    }
    let body = raw
        .replace("\\\r\n", "\n")
        .replace("\\\n", "\n")
        .replace("##", "");
    let mut found: BTreeSet<(String, Option<String>)> = BTreeSet::new();
    for probe in [
        body.clone(),
        format!("void {C_MACRO_PROBE}() {{\n{body}\n}}\n"),
    ] {
        let Some(tree) = parse_c_probe(lang, &probe) else {
            continue;
        };
        collect_c_probe_calls(tree.root_node(), &probe, &parameters, &mut found);
    }
    let caller = format!("{file_symbol_name}::{name}");
    let span = node_span(value);
    for (callee, receiver) in found {
        references.push(ExtractedReference {
            name: callee.clone(),
            kind: ReferenceKind::Call,
            span: span.clone(),
            enclosing_symbol: Some(caller.clone()),
            assigned_to: None,
            receiver_expr: receiver.clone(),
        });
        calls.push(ExtractedCall {
            caller_symbol: Some(caller.clone()),
            callee_name: callee,
            receiver_expr: receiver,
            span: span.clone(),
        });
    }
}

/// Every `call_expression` in a probe tree that sits outside an `ERROR`
/// subtree and names something other than a macro parameter or the probe
/// wrapper itself.
pub(super) fn collect_c_probe_calls(
    root: Node,
    probe: &str,
    parameters: &BTreeSet<String>,
    found: &mut BTreeSet<(String, Option<String>)>,
) {
    let mut stack = vec![root];
    let mut visited = 0usize;
    while let Some(current) = stack.pop() {
        visited += 1;
        if visited > C_MACRO_PROBE_MAX_NODES {
            break;
        }
        // Nothing under a node the grammar could not place is a syntactic
        // fact; a fabricated call is worse than a missing one.
        if current.is_error() {
            continue;
        }
        if current.kind() == "call_expression" {
            if let Some(function) = current.child_by_field_name("function") {
                if let Some((callee, receiver)) = split_call_target(function, probe) {
                    if is_user_ident(&callee)
                        && callee != C_MACRO_PROBE
                        && !parameters.contains(&callee)
                    {
                        found.insert((callee, receiver));
                    }
                }
            }
        }
        push_named_children(current, &mut stack);
    }
}

/// The callee's identity — its name, and the receiver expression it is reached
/// through — or `None` when the target names nothing.
///
/// **Returning `None` is the point of this signature.** Every arm below can fail
/// to find a name, and the historical answer was to record the target's own
/// source text instead. That is the SC26 defect class, and the original fix
/// reached five of its shapes while the same text fallback survived in every
/// other arm. Measured on the DevCouncil repository, it was still recording a
/// Python curried call (`app.command(name="apply-patch")(fn)`), a minified JS
/// immediately-invoked literal (`(function(t){…})(x)`) and a Rust
/// immediately-invoked closure as callee *names* — rows that can never join to
/// a symbol, crowding the one tier reserved for genuine resolution defects.
///
/// Two rungs, structural then lexical. An inline function literal has no callee
/// identity by construction. Anything else must be identifier-shaped: every
/// grammar that reaches here names a callee with a single identifier token, so
/// text carrying an argument list, an operator or a brace is an *expression*
/// that fell through to the catch-all below.
///
/// Nothing real is lost by refusing them. The inner call of a curried
/// expression is itself a call node extracted on its own — `command` and
/// `_default_runner` keep the edges they always had — and an inline literal has
/// no symbol to point at. The rule is fail-closed in the direction that
/// matters: it can only drop a name no symbol could carry.
pub(crate) fn split_call_target(
    function_node: Node,
    source: &str,
) -> Option<(String, Option<String>)> {
    if is_anonymous_callable(function_node.kind()) {
        return None;
    }
    let (name, receiver) = split_call_target_inner(function_node, source, 0);
    is_callee_identity(&name).then_some((name, receiver))
}

/// Whether text can be the *name* of a callee, as opposed to an expression.
///
/// Deliberately not `is_user_ident`, which is ASCII-only and rejects `#`. Both
/// would cost real edges here: Python, Go and Rust all admit non-ASCII
/// identifiers, and a JavaScript private method is declared *and* called as
/// `#name`, so the symbol and the callee agree and the edge resolves. Rust raw
/// identifiers (`r#type`) carry the same character for the same reason.
///
/// What it excludes is everything an expression brings with it — whitespace,
/// brackets, operators, quotes, `.`, `,`, `;` — which is the signature of source
/// text that reached a fallback instead of a name.
pub(crate) fn is_callee_identity(name: &str) -> bool {
    let Some(first) = name.chars().next() else {
        return false;
    };
    (first.is_alphabetic() || matches!(first, '_' | '$' | '#'))
        && name
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '$' | '#'))
}

/// Whether a call target is an inline function literal rather than a name.
///
/// An immediately-invoked literal is a call with no callee *identity*: there is
/// no symbol to join to, and recording its source text as a callee name — which
/// is what a text fallback does — manufactures a row that can never match.
pub(crate) fn is_anonymous_callable(kind: &str) -> bool {
    matches!(
        kind,
        "func_literal" | "closure_expression" | "function_expression" | "arrow_function" | "lambda"
    )
}
