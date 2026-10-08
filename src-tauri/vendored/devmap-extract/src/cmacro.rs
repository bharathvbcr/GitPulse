//! Functions a C-family file defines by invoking a function-like macro.
//!
//! A shader library stamps one body out at many sizes:
//!
//! ```c
//! #define ROWS_KERNEL(NAME, D, R) kernel void NAME(...) { ... }
//! ROWS_KERNEL(flash_attn_rows_h256_r16_g32, 256, 16, 32)
//! ```
//!
//! The grammar sees the invocation as a call expression (or, when an argument
//! is a type, as an `ERROR` node) and the macro's body as one opaque
//! `preproc_arg` token, so `flash_attn_rows_h256_r16_g32` — the name the host
//! dispatches — was in no symbol. Measured on tessl: 21 `flash_attn_rows_*`,
//! two `encoder_attn_rows_*` and four `qwen35_attn_tiled_*` kernels, every one
//! of them named by a Rust `pipeline("…")` call and none of them searchable.
//!
//! The rule is the preprocessor's own, applied to macros **this file defines**:
//! substitute the arguments, paste `##`, stringify `#`, rescan for further
//! invocations of this file's macros, then parse the expansion with the file's
//! grammar and read its function definitions through the same declaration path
//! every other function takes. A macro defined in an included header is not
//! expanded — which header a `#include` reaches is the import resolver's
//! question, not the extractor's — so its instantiations stay unnamed, exactly
//! as before.
//!
//! **A stamped name must come from the invocation.** A definition whose name is
//! spelled literally in some macro body here would be the same name at every
//! use, so emitting it would mint one duplicate identity per invocation; it is
//! refused, and only names built from arguments (directly or by pasting) are
//! emitted. A name the file already declares is not emitted twice either.

use std::collections::{BTreeMap, BTreeSet};

use tree_sitter::Node;

use crate::model::{
    ExtractedCall, ExtractedReference, ExtractedSymbol, ReferenceKind, Span, SymbolKind,
    WiringAnnotation, WiringKind,
};

/// Most function-like macros one file's definitions are collected for.
const MAX_MACROS: usize = 4_096;

/// Most invocations one file is expanded for.
const MAX_INVOCATIONS: usize = 4_096;

/// Longest argument list read for one invocation.
const MAX_ARGUMENT_BYTES: usize = 16 * 1024;

/// Deepest chain of one macro invoking another that is followed.
const MAX_EXPANSION_DEPTH: usize = 16;

/// Largest expansion of one invocation, after every rescan.
const MAX_EXPANSION_BYTES: usize = 256 * 1024;

/// Most syntax-tree nodes read per expansion, and per definition walk.
const MAX_NODES: usize = 50_000;

/// One `#define NAME(params) body` in this file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MacroDefinition {
    /// Parameter names in order. A variadic tail is recorded as `__VA_ARGS__`
    /// (for `...`) or as its own name (GNU `args...`), with `variadic` set.
    parameters: Vec<String>,
    variadic: bool,
    /// The body with every line continuation removed.
    body: String,
}

/// One stamped function: the invocation that produced it and what it declares.
struct Stamp {
    macro_name: String,
    /// The invocation, from the macro's name through its closing parenthesis.
    span: Span,
    name: String,
    is_exported: bool,
    entry_reason: Option<&'static str>,
}

/// Emit a `Function` symbol for every function this file defines through one of
/// its own function-like macros, and attribute the invocation to it.
///
/// The invocation is recorded as a call from the stamped function to the macro
/// — which is what it is: the kernel's body *is* the macro's — so a change to
/// the macro reaches every kernel it stamps, and through each kernel whatever
/// dispatches it. References the grammar recovered from the argument list are
/// re-owned by the stamped function for the same reason; the one naming the
/// stamped function itself is dropped, because a declaration does not refer to
/// itself.
#[allow(clippy::too_many_arguments)]
pub(crate) fn stamp_macro_instantiations(
    root: Node,
    source: &str,
    lang: &str,
    file_symbol_name: &str,
    is_metal: bool,
    symbols: &mut Vec<ExtractedSymbol>,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
    wiring: &mut Vec<WiringAnnotation>,
    deadline: std::time::Instant,
) {
    let macros = collect_macro_definitions(root, source);
    if macros.is_empty() {
        return;
    }
    // Every identifier any body here spells. A definition named by one of them
    // is the same name at every use and is refused (see the module docs).
    let body_identifiers: BTreeSet<&str> = macros
        .values()
        .flat_map(|definition| {
            identifiers(&definition.body).map(|(start, end)| &definition.body[start..end])
        })
        .collect();
    let mut declared: BTreeSet<String> = symbols
        .iter()
        .map(|symbol| symbol.qualified_name.clone())
        .collect();

    let mut stamps: Vec<Stamp> = Vec::new();
    let mut invocations = 0usize;
    let mut resume_at = 0usize;
    for (start, end) in identifiers(source) {
        if start < resume_at {
            continue;
        }
        let name = &source[start..end];
        let Some(definition) = macros.get(name) else {
            continue;
        };
        let Some((arguments, close)) = invocation_arguments(source, end) else {
            continue;
        };
        if !at_declaration_scope(root, start, end) {
            continue;
        }
        invocations += 1;
        if invocations > MAX_INVOCATIONS || crate::treesitter::extraction_overran(deadline) {
            break;
        }
        // An invocation nested in another's arguments is part of that
        // expansion, and is read there.
        resume_at = close + 1;
        let mut active = vec![name.to_string()];
        let Some(substituted) = substitute(definition, &arguments) else {
            continue;
        };
        let Some(expansion) = rescan(&substituted, &macros, &mut active, 1) else {
            continue;
        };
        for (stamped, is_exported, entry_reason) in
            stamped_functions(&expansion, lang, file_symbol_name, is_metal)
        {
            if body_identifiers.contains(stamped.as_str()) {
                continue;
            }
            if !declared.insert(format!("{file_symbol_name}::{stamped}")) {
                continue;
            }
            stamps.push(Stamp {
                macro_name: name.to_string(),
                span: Span {
                    start_byte: start,
                    end_byte: close + 1,
                },
                name: stamped,
                is_exported,
                entry_reason,
            });
        }
    }

    for stamp in stamps {
        let qualified = format!("{file_symbol_name}::{}", stamp.name);
        if let Some(reason) = stamp.entry_reason {
            wiring.push(WiringAnnotation {
                kind: WiringKind::RuntimeEntryPoint,
                target_symbol: qualified.clone(),
                details: reason.to_string(),
            });
        }
        attribute_invocation(&stamp, &qualified, calls, references);
        symbols.push(ExtractedSymbol {
            name: stamp.name,
            qualified_name: qualified,
            kind: SymbolKind::Function,
            span: stamp.span,
            is_exported: stamp.is_exported,
            docstring: None,
            signature: None,
            parent_symbol: Some(file_symbol_name.to_string()),
            body_signature: None,
            declaration_hash: None,
            return_type: None,
        });
    }
}

/// Make the stamped function the owner of its own invocation.
///
/// The walk already recorded the invocation as a file-scope call when the
/// grammar parsed it as a call expression; that record is re-owned. When the
/// grammar parsed it as an `ERROR` — an argument that is a type, `KERNEL(float,
/// name)` — no call was recorded, and one is added, so both shapes leave the
/// same edge.
fn attribute_invocation(
    stamp: &Stamp,
    qualified: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    let within = |span: &Span| {
        span.start_byte >= stamp.span.start_byte && span.end_byte <= stamp.span.end_byte
    };
    let mut call_found = false;
    for call in calls.iter_mut() {
        if call.caller_symbol.is_none()
            && call.callee_name == stamp.macro_name
            && call.span.start_byte == stamp.span.start_byte
        {
            call.caller_symbol = Some(qualified.to_string());
            call_found = true;
        }
    }
    references.retain(|reference| {
        !(reference.enclosing_symbol.is_none()
            && reference.name == stamp.name
            && within(&reference.span))
    });
    let mut reference_found = false;
    for reference in references.iter_mut() {
        if reference.enclosing_symbol.is_none() && within(&reference.span) {
            if reference.kind == ReferenceKind::Call && reference.name == stamp.macro_name {
                reference_found = true;
            }
            reference.enclosing_symbol = Some(qualified.to_string());
        }
    }
    let callee_span = Span {
        start_byte: stamp.span.start_byte,
        end_byte: stamp.span.start_byte + stamp.macro_name.len(),
    };
    if !call_found {
        calls.push(ExtractedCall {
            caller_symbol: Some(qualified.to_string()),
            callee_name: stamp.macro_name.clone(),
            receiver_expr: None,
            span: stamp.span.clone(),
        });
    }
    if !reference_found {
        references.push(ExtractedReference {
            name: stamp.macro_name.clone(),
            kind: ReferenceKind::Call,
            span: callee_span,
            enclosing_symbol: Some(qualified.to_string()),
            assigned_to: None,
            receiver_expr: None,
        });
    }
}

/// Every function-like macro this file defines, by name.
///
/// A name defined twice with different bodies — the two arms of an `#if` — is
/// dropped: which arm the build takes is not knowable here, and expanding
/// either would assert a declaration the build may not contain.
fn collect_macro_definitions(root: Node, source: &str) -> BTreeMap<String, MacroDefinition> {
    let mut found: BTreeMap<String, MacroDefinition> = BTreeMap::new();
    let mut conflicting: BTreeSet<String> = BTreeSet::new();
    let mut stack = vec![root];
    let mut visited = 0usize;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited > MAX_NODES * 4 || found.len() >= MAX_MACROS {
            break;
        }
        if node.kind() != "preproc_function_def" {
            crate::treesitter::push_named_children(node, &mut stack);
            continue;
        }
        let Some(name) = node
            .child_by_field_name("name")
            .and_then(|name| source.get(name.byte_range()))
        else {
            continue;
        };
        let Some(definition) = macro_definition(node, source) else {
            continue;
        };
        match found.get(name) {
            Some(known) if *known != definition => {
                conflicting.insert(name.to_string());
            }
            Some(_) => {}
            None => {
                found.insert(name.to_string(), definition);
            }
        }
    }
    for name in conflicting {
        found.remove(&name);
    }
    found
}

fn macro_definition(node: Node, source: &str) -> Option<MacroDefinition> {
    let value = node.child_by_field_name("value")?;
    let raw = source.get(value.byte_range())?;
    if raw.len() > crate::treesitter::C_MACRO_BODY_MAX_BYTES {
        return None;
    }
    let params = node.child_by_field_name("parameters")?;
    let mut parameters = Vec::new();
    let mut variadic = false;
    let mut cursor = params.walk();
    let mut previous_kind = "";
    for child in params.children(&mut cursor) {
        let text = source.get(child.byte_range())?;
        match child.kind() {
            "identifier" => parameters.push(text.to_string()),
            // GNU `args...` names the variadic parameter itself: the grammar
            // gives an identifier immediately followed by `...`, with no comma
            // between, and that identifier is the one the body spells.
            "..." if previous_kind == "identifier" => variadic = true,
            "..." => {
                parameters.push("__VA_ARGS__".to_string());
                variadic = true;
            }
            _ => {}
        }
        previous_kind = child.kind();
    }
    Some(MacroDefinition {
        parameters,
        variadic,
        body: raw
            .replace("\\\r\n", "\n")
            .replace("\\\n", "\n")
            .trim()
            .to_string(),
    })
}

/// Whether the invocation at `start..end` sits where a declaration can: at
/// file scope or inside a namespace, never inside a function body, a type's
/// member list, an initializer or another declaration, or a directive.
///
/// The initializer case is the one that costs: a generated `parser.c` holds
/// thousands of `ACTIONS(n)` invocations inside one file-scope array, none of
/// which can declare a function, and each would otherwise be expanded and
/// parsed.
fn at_declaration_scope(root: Node, start: usize, end: usize) -> bool {
    let Some(mut node) = root.descendant_for_byte_range(start, end) else {
        return false;
    };
    for _ in 0..256 {
        if matches!(
            node.kind(),
            "compound_statement"
                | "field_declaration_list"
                | "initializer_list"
                | "declaration"
                | "preproc_function_def"
                | "preproc_def"
                | "preproc_arg"
                | "comment"
                | "string_literal"
                | "raw_string_literal"
                | "char_literal"
        ) {
            return false;
        }
        match node.parent() {
            Some(parent) => node = parent,
            None => return true,
        }
    }
    false
}

/// The arguments of an invocation whose name ends at `name_end`, and the byte
/// of its closing parenthesis — or `None` when no `(` follows the name.
///
/// Split on the commas at the invocation's own depth; a comma inside nested
/// parentheses, brackets, braces, a string or a character literal belongs to
/// one argument.
fn invocation_arguments(text: &str, name_end: usize) -> Option<(Vec<String>, usize)> {
    let bytes = text.as_bytes();
    let mut at = name_end;
    while at < bytes.len() && matches!(bytes[at], b' ' | b'\t' | b'\r' | b'\n') {
        at += 1;
    }
    if bytes.get(at) != Some(&b'(') {
        return None;
    }
    let open = at;
    let mut depth = 0usize;
    let mut arguments = Vec::new();
    let mut argument_start = open + 1;
    while at < bytes.len() {
        if at - open > MAX_ARGUMENT_BYTES {
            return None;
        }
        match bytes[at] {
            quote @ (b'"' | b'\'') => {
                at = skip_quoted(bytes, at, quote);
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    arguments.push(text.get(argument_start..at)?.trim().to_string());
                    // `F()` is one empty argument to a one-parameter macro and
                    // none to a zero-parameter one; `substitute` decides.
                    return Some((arguments, at));
                }
            }
            b',' if depth == 1 => {
                arguments.push(text.get(argument_start..at)?.trim().to_string());
                argument_start = at + 1;
            }
            _ => {}
        }
        at += 1;
    }
    None
}

/// The byte after a string or character literal opening at `start`.
fn skip_quoted(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut at = start + 1;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => at += 2,
            byte if byte == quote => return at + 1,
            b'\n' => return at,
            _ => at += 1,
        }
    }
    bytes.len()
}

/// The identifier tokens of C-family `text`, as byte ranges, skipping comments,
/// string and character literals, and preprocessor directive lines (with their
/// continuations) — a `#define` body names macros it does not invoke here.
fn identifiers(text: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    let bytes = text.as_bytes();
    let mut at = 0usize;
    let mut line_start = true;
    std::iter::from_fn(move || {
        while at < bytes.len() {
            let byte = bytes[at];
            match byte {
                b'\n' => {
                    line_start = true;
                    at += 1;
                }
                b' ' | b'\t' | b'\r' => at += 1,
                b'#' if line_start => {
                    // A directive runs to the first newline not escaped by `\`.
                    while at < bytes.len() {
                        if bytes[at] == b'\\' {
                            at += 2;
                            continue;
                        }
                        if bytes[at] == b'\n' {
                            break;
                        }
                        at += 1;
                    }
                }
                b'/' if bytes.get(at + 1) == Some(&b'/') => {
                    while at < bytes.len() && bytes[at] != b'\n' {
                        at += 1;
                    }
                }
                b'/' if bytes.get(at + 1) == Some(&b'*') => {
                    line_start = false;
                    at += 2;
                    while at < bytes.len()
                        && !(bytes[at] == b'*' && bytes.get(at + 1) == Some(&b'/'))
                    {
                        at += 1;
                    }
                    at = (at + 2).min(bytes.len());
                }
                b'"' | b'\'' => {
                    line_start = false;
                    at = skip_quoted(bytes, at, byte);
                }
                _ if byte.is_ascii_alphabetic() || byte == b'_' => {
                    line_start = false;
                    let start = at;
                    while at < bytes.len()
                        && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_')
                    {
                        at += 1;
                    }
                    return Some((start, at));
                }
                _ if byte.is_ascii_digit() => {
                    // A number's suffix (`1.0f`, `0x1Fu`) is not an identifier.
                    line_start = false;
                    while at < bytes.len()
                        && (bytes[at].is_ascii_alphanumeric() || matches!(bytes[at], b'_' | b'.'))
                    {
                        at += 1;
                    }
                }
                _ => {
                    line_start = false;
                    at += 1;
                }
            }
        }
        None
    })
}

/// The body of `definition` with `arguments` substituted, `#x` stringified and
/// `##` pasted — or `None` when the argument count does not fit.
fn substitute(definition: &MacroDefinition, arguments: &[String]) -> Option<String> {
    let fixed = definition.parameters.len() - usize::from(definition.variadic);
    let mut bound: BTreeMap<&str, String> = BTreeMap::new();
    if definition.variadic {
        if arguments.len() < fixed {
            return None;
        }
        for (parameter, argument) in definition.parameters.iter().zip(arguments) {
            bound.insert(parameter, argument.clone());
        }
        bound.insert(
            definition
                .parameters
                .last()
                .map(String::as_str)
                .unwrap_or("__VA_ARGS__"),
            arguments[fixed..].join(", "),
        );
    } else {
        // `F()` reads as one empty argument; a zero-parameter macro takes it.
        let arguments: &[String] = if fixed == 0 && arguments.len() == 1 && arguments[0].is_empty()
        {
            &[]
        } else {
            arguments
        };
        if arguments.len() != fixed {
            return None;
        }
        for (parameter, argument) in definition.parameters.iter().zip(arguments) {
            bound.insert(parameter, argument.clone());
        }
    }

    // `\u{1}` marks a paste point; whitespace around it is removed afterwards,
    // which is exactly what `##` does to its two operands.
    const PASTE: char = '\u{1}';
    let body = &definition.body;
    let bytes = body.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut at = 0usize;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte == b'#' && bytes.get(at + 1) == Some(&b'#') {
            out.push(PASTE);
            at += 2;
            continue;
        }
        if byte == b'#' {
            let mut next = at + 1;
            while next < bytes.len() && matches!(bytes[next], b' ' | b'\t') {
                next += 1;
            }
            let name_end = identifier_end(bytes, next);
            if let Some(argument) = body.get(next..name_end).and_then(|name| bound.get(name)) {
                out.push('"');
                out.push_str(&argument.replace('\\', "\\\\").replace('"', "\\\""));
                out.push('"');
                at = name_end;
                continue;
            }
        }
        if byte == b'"' || byte == b'\'' {
            let end = skip_quoted(bytes, at, byte);
            out.push_str(body.get(at..end)?);
            at = end;
            continue;
        }
        if byte.is_ascii_alphabetic() || byte == b'_' {
            let end = identifier_end(bytes, at);
            let name = body.get(at..end)?;
            match bound.get(name) {
                Some(argument) => out.push_str(argument),
                None => out.push_str(name),
            }
            at = end;
            continue;
        }
        let ch = body.get(at..)?.chars().next()?;
        out.push(ch);
        at += ch.len_utf8();
    }
    if !out.contains(PASTE) {
        return Some(out);
    }
    let mut pasted = String::with_capacity(out.len());
    for (index, piece) in out.split(PASTE).enumerate() {
        if index == 0 {
            pasted.push_str(piece.trim_end());
        } else {
            let trimmed = pasted.trim_end().len();
            pasted.truncate(trimmed);
            pasted.push_str(piece.trim_start());
        }
    }
    Some(pasted)
}

fn identifier_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    if end < bytes.len() && (bytes[end].is_ascii_alphabetic() || bytes[end] == b'_') {
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            end += 1;
        }
    }
    end
}

/// `text` with every invocation of one of this file's macros expanded, the way
/// the preprocessor rescans a replacement list. A macro is never expanded
/// inside its own expansion (`active`), which is C's rule and also the bound
/// on self-reference; depth and size are bounded besides.
fn rescan(
    text: &str,
    macros: &BTreeMap<String, MacroDefinition>,
    active: &mut Vec<String>,
    depth: usize,
) -> Option<String> {
    if depth > MAX_EXPANSION_DEPTH || text.len() > MAX_EXPANSION_BYTES {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut copied = 0usize;
    let mut resume_at = 0usize;
    for (start, end) in identifiers(text) {
        if start < resume_at {
            continue;
        }
        let name = &text[start..end];
        let Some(definition) = macros.get(name) else {
            continue;
        };
        if active.iter().any(|open| open == name) {
            continue;
        }
        let Some((arguments, close)) = invocation_arguments(text, end) else {
            continue;
        };
        let substituted = substitute(definition, &arguments)?;
        active.push(name.to_string());
        let expanded = rescan(&substituted, macros, active, depth + 1);
        active.pop();
        out.push_str(&text[copied..start]);
        out.push_str(&expanded?);
        if out.len() > MAX_EXPANSION_BYTES {
            return None;
        }
        copied = close + 1;
        resume_at = copied;
    }
    out.push_str(&text[copied..]);
    Some(out)
}

/// The free functions an expansion defines: each one's name, its visibility,
/// and the reason a runtime reaches it when it is a shader or kernel entry.
fn stamped_functions(
    expansion: &str,
    lang: &str,
    file_symbol_name: &str,
    is_metal: bool,
) -> Vec<(String, bool, Option<&'static str>)> {
    let Some(tree) = crate::treesitter::parse_c_probe(lang, expansion) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut stack = vec![tree.root_node()];
    let mut visited = 0usize;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited > MAX_NODES {
            break;
        }
        // Nothing under a node the grammar could not place is a declaration.
        if node.is_error() || node.kind() == "compound_statement" {
            continue;
        }
        if node.kind() == "function_definition" {
            if let Some(declaration) = crate::langdecl::declaration_of(lang, node, expansion) {
                if declaration.declared_kind == SymbolKind::Function
                    && declaration.owner.is_none()
                    && crate::treesitter::is_user_ident(&declaration.name)
                {
                    let is_exported = crate::langdecl::is_exported_of(
                        lang,
                        node,
                        expansion,
                        &declaration.name,
                        file_symbol_name,
                    );
                    let entry_reason = if is_metal {
                        crate::treesitter::metal_shader_entry_reason_of(node, expansion)
                    } else {
                        crate::treesitter::c_family_entry_point_reason(
                            node,
                            expansion,
                            file_symbol_name,
                            &declaration.name,
                        )
                    };
                    found.push((declaration.name, is_exported, entry_reason));
                }
            }
            continue;
        }
        crate::treesitter::push_named_children(node, &mut stack);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(parameters: &[&str], variadic: bool, body: &str) -> MacroDefinition {
        MacroDefinition {
            parameters: parameters.iter().map(|p| p.to_string()).collect(),
            variadic,
            body: body.to_string(),
        }
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn substitution_replaces_whole_identifiers_only() {
        let def = definition(&["N", "D"], false, "void N(int ND) { return D + N_x; }");
        assert_eq!(
            substitute(&def, &args(&["k", "4"])).as_deref(),
            Some("void k(int ND) { return 4 + N_x; }")
        );
    }

    #[test]
    fn paste_and_stringify_follow_the_preprocessor() {
        let def = definition(&["N", "T"], false, "void N ## _ ## T() { log(#N); }");
        assert_eq!(
            substitute(&def, &args(&["gate", "f32"])).as_deref(),
            Some("void gate_f32() { log(\"gate\"); }")
        );
    }

    #[test]
    fn a_wrong_argument_count_expands_to_nothing() {
        let def = definition(&["A", "B"], false, "void A() {}");
        assert_eq!(substitute(&def, &args(&["x"])), None);
        assert_eq!(substitute(&def, &args(&["x", "y", "z"])), None);
    }

    #[test]
    fn variadic_arguments_join_into_va_args() {
        let def = definition(&["N", "__VA_ARGS__"], true, "void N() { f(__VA_ARGS__); }");
        assert_eq!(
            substitute(&def, &args(&["k", "1", "2"])).as_deref(),
            Some("void k() { f(1, 2); }")
        );
    }

    #[test]
    fn arguments_split_only_at_their_own_depth() {
        let text = "K(a, f(b, c), \"x,y\", ',' )";
        let (arguments, close) = invocation_arguments(text, 1).unwrap();
        assert_eq!(arguments, args(&["a", "f(b, c)", "\"x,y\"", "','"]));
        assert_eq!(close, text.len() - 1);
        assert_eq!(
            invocation_arguments("K (a", 1),
            None,
            "an unclosed list is no invocation"
        );
        assert_eq!(
            invocation_arguments("K + 1", 1),
            None,
            "a name with no ( is no invocation"
        );
    }

    #[test]
    fn identifiers_skip_comments_strings_directives_and_number_suffixes() {
        let text = "#define A(x) B(x)\n// C(\n/* D( */ \"E(\" 1.0f G(y)";
        let found: Vec<&str> = identifiers(text).map(|(s, e)| &text[s..e]).collect();
        assert_eq!(found, vec!["G", "y"]);
    }

    #[test]
    fn rescan_expands_nested_macros_and_never_a_macro_inside_itself() {
        let mut macros = BTreeMap::new();
        macros.insert(
            "OUTER".to_string(),
            definition(&["N"], false, "INNER(N, float)"),
        );
        macros.insert(
            "INNER".to_string(),
            definition(&["N", "T"], false, "T N(T x) { return OUTER(x); }"),
        );
        let mut active = vec!["OUTER".to_string()];
        let substituted = substitute(&macros["OUTER"], &args(&["k"])).unwrap();
        assert_eq!(
            rescan(&substituted, &macros, &mut active, 1).as_deref(),
            Some("float k(float x) { return OUTER(x); }")
        );
    }

    #[test]
    fn unbounded_self_growth_is_refused_not_followed() {
        let mut macros = BTreeMap::new();
        macros.insert(
            "A".to_string(),
            definition(&["x"], false, "B(x x x x x x x x)"),
        );
        macros.insert(
            "B".to_string(),
            definition(&["x"], false, "A(x x x x x x x x)"),
        );
        let mut active = vec![];
        // A -> B -> A is stopped by `active`, so this terminates with A left
        // unexpanded rather than recursing.
        let out = rescan("A(q)", &macros, &mut active, 1).unwrap();
        assert!(out.starts_with("A("), "{out}");
    }
}
