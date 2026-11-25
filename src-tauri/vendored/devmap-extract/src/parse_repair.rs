//! Same-width parse repairs for upstream tree-sitter grammar gaps in
//! TypeScript/TSX, Rust, and Shell (Bash).
//!
//! Each repair follows the exact contract of [`crate::sveltetext`] and
//! [`crate::kotlinsemi`]:
//!
//! * Never entered unless the first parse holds an error
//!   (`tree.root_node().has_error()`).
//! * Writes only ASCII bytes over ASCII bytes, keeping the exact byte length of
//!   the original source so every byte offset in the repaired tree is an exact
//!   offset of the file on disk.
//! * The repaired copy is only *parsed* and dropped immediately; extractors
//!   always read node text from the original source.
//! * The repaired tree replaces the failed tree **only if** it holds strictly
//!   less error (outermost ERROR bytes first, then MISSING token count). Real
//!   syntax errors stay `Partial` with their original regions.
//! * Bounded by the file's shared `deadline`.

use std::time::Instant;

use tree_sitter::{Node, Parser, Tree};

use crate::treesitter::{parse_until, DEADLINE_CHECK_STRIDE};

/// What a same-width repair produced.
pub(crate) struct Repaired {
    /// The tree parsed from the same-length rewritten copy, on the file's own
    /// byte offsets.
    pub(crate) tree: Tree,
    /// Number of sites repaired.
    pub(crate) edits: usize,
    /// Short description for diagnostics.
    pub(crate) kind: &'static str,
}

/// Maximum repair passes per file so cascading grammar quirks (e.g. `&raw`
/// hiding a `#[cfg(...)]` struct-pattern error on the next line) are uncovered
/// and fixed in bounded time.
const MAX_REPAIR_ROUNDS: usize = 4;

/// Try to reduce grammar-gap parse errors for TypeScript/TSX, Rust, or Shell.
///
/// `None` leaves the file untouched (unsupported language, no applicable sites,
/// no strict error reduction, or deadline exhausted).
pub(crate) fn repair(
    lang: &str,
    parser: &mut Parser,
    first: &Tree,
    source: &str,
    deadline: Instant,
) -> Option<Repaired> {
    if Instant::now() >= deadline || !first.root_node().has_error() {
        return None;
    }
    let kind = match lang {
        "typescript" | "tsx" => "typescript grammar quirk",
        "rust" => "rust grammar quirk",
        "shell" => "shell parameter expansion quirk",
        _ => return None,
    };
    let initial_err = error_of(first.root_node(), deadline)?;
    let mut text = source.as_bytes().to_vec();
    let mut current: Option<Tree> = None;
    let mut best: Option<(Tree, usize)> = None;
    let mut best_err = initial_err;
    let mut total_edits = 0usize;

    for _ in 0..MAX_REPAIR_ROUNDS {
        if Instant::now() >= deadline {
            break;
        }
        let tree_ref = current.as_ref().unwrap_or(first);
        let Some(error_spans) = collect_error_spans(tree_ref.root_node(), deadline) else {
            break;
        };
        if error_spans.is_empty() {
            break;
        }
        let edits = match lang {
            "typescript" | "tsx" => repair_typescript(&mut text, &error_spans),
            "rust" => repair_rust(&mut text, &error_spans),
            "shell" => repair_shell(&mut text, &error_spans),
            _ => 0,
        };
        if edits == 0 {
            break;
        }
        total_edits += edits;
        let Ok(rewritten) = std::str::from_utf8(&text) else {
            break;
        };
        let (Some(next), false) = parse_until(parser, rewritten, deadline) else {
            break;
        };
        let Some(next_err) = error_of(next.root_node(), deadline) else {
            break;
        };
        let clean = !next.root_node().has_error();
        if next_err < best_err {
            best_err = next_err;
            best = Some((next.clone(), total_edits));
        }
        current = Some(next);
        if clean {
            break;
        }
    }

    best.map(|(tree, edits)| Repaired { tree, edits, kind })
}

/// Outermost ERROR and MISSING byte ranges of `root`, sorted by start byte.
fn collect_error_spans(root: Node, deadline: Instant) -> Option<Vec<(usize, usize)>> {
    let mut spans = Vec::new();
    let mut cursor = root.walk();
    let mut stack = vec![root];
    let mut visited = 0u32;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited >= DEADLINE_CHECK_STRIDE {
            visited = 0;
            if Instant::now() >= deadline {
                return None;
            }
        }
        if node.is_error() || node.is_missing() {
            spans.push((node.start_byte(), node.end_byte()));
        } else if node.has_error() {
            stack.extend(node.children(&mut cursor));
        }
    }
    spans.sort_unstable();
    Some(spans)
}

/// `(error_bytes, missing_count)` under `root`; `None` if `deadline` passed.
fn error_of(root: Node, deadline: Instant) -> Option<(usize, usize)> {
    let mut bytes = 0usize;
    let mut missing = 0usize;
    let mut cursor = root.walk();
    let mut stack = vec![root];
    let mut visited = 0u32;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited >= DEADLINE_CHECK_STRIDE {
            visited = 0;
            if Instant::now() >= deadline {
                return None;
            }
        }
        if node.is_error() {
            bytes += node.end_byte() - node.start_byte();
        } else if node.is_missing() {
            missing += 1;
        } else if node.has_error() {
            stack.extend(node.children(&mut cursor));
        }
    }
    Some((bytes, missing))
}

fn near_error(spans: &[(usize, usize)], start: usize, end: usize, slack: usize) -> bool {
    let lo = start.saturating_sub(slack);
    let hi = end.saturating_add(slack);
    spans.iter().any(|&(es, ee)| es <= hi && ee >= lo)
}

fn blank_preserving_newlines(slice: &mut [u8]) {
    for b in slice.iter_mut() {
        if *b != b'\n' && *b != b'\r' {
            *b = b' ';
        }
    }
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn is_rust_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

// ------------------------------------------------------------------ TypeScript

/// TypeScript predefined-type keywords that `tree-sitter-typescript` 0.23.2
/// rejects as named tuple element labels (`[verb: string, object: string]`).
const TS_TUPLE_LABEL_KEYWORDS: &[&[u8]] = &[
    b"object",
    b"string",
    b"number",
    b"boolean",
    b"symbol",
    b"any",
    b"unknown",
    b"never",
    b"void",
    b"undefined",
    b"bigint",
];

/// Repair `tree-sitter-typescript` 0.23.2 grammar gaps in place:
///
/// 1. `typeof import("...")` and `import("...").Type` import types (not in
///    0.23.2's type grammar).
///    - `import("...").Ident`: blanks `import("...").` so `Ident` remains a
///      clean `type_identifier` at its original span.
///    - `typeof import("...")` (or standalone `import("...")` near an error):
///      replaces with `any` + spaces (`predefined_type`, zero bogus refs).
/// 2. Named tuple labels using predefined-type keywords (`[object: string]`):
///    replaces the last byte of the keyword with `x` inside the error span.
/// 3. `< unique.` comparison misparsed as TypeScript's `unique symbol` type
///    operator (`for (let start = 0; start < unique.length; ...)`): replaces
///    `unique` with `uniqux` when immediately preceded by `<` and overlapping
///    an error span.
fn repair_typescript(text: &mut [u8], error_spans: &[(usize, usize)]) -> usize {
    let mut edits = 0usize;

    // Pass 1: import types (`typeof import("...")` and `import("...").Type`).
    let len = text.len();
    let mut i = 0usize;
    while i < len {
        // Skip strings, template literals, and comments so text inside them is
        // never rewritten.
        if let Some(next) = skip_js_non_code(text, i) {
            i = next;
            continue;
        }
        if text[i..].starts_with(b"import")
            && (i == 0 || !is_ident_byte(text[i - 1]))
            && text.get(i + 6).is_none_or(|&b| !is_ident_byte(b))
        {
            if let Some((call_end, dot_member)) = match_import_type_call(text, i) {
                // Only rewrite when an error sits on or immediately around this
                // `import(...)` — runtime dynamic `import("...")` parses clean
                // and is never touched.
                let typeof_start = preceding_keyword(text, i, b"typeof");
                let span_start = typeof_start.unwrap_or(i);
                if near_error(error_spans, span_start, call_end, 16) {
                    if let Some(dot_end) = dot_member {
                        // `import("...").Member` or `typeof import("...").Member`:
                        // blank `import("...").`, leaving `Member` (and any
                        // leading `typeof `) intact as a real type reference.
                        blank_preserving_newlines(&mut text[i..dot_end]);
                        edits += 1;
                        i = dot_end;
                        continue;
                    } else if call_end - span_start >= 3 {
                        // `typeof import("...")` or standalone `import("...")`
                        // in type position: replace with `any` + spaces.
                        text[span_start..span_start + 3].copy_from_slice(b"any");
                        blank_preserving_newlines(&mut text[span_start + 3..call_end]);
                        edits += 1;
                        i = call_end;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }

    // Pass 2: error-localized keyword quirks (`object:` tuple label, `< unique.`).
    for &(es, ee) in error_spans {
        if es >= text.len() || ee > text.len() || es >= ee {
            continue;
        }
        let err = &text[es..ee];
        // Named tuple label: `<keyword>:` or `<keyword>?:` (e.g. `object:`).
        if let Some(kw_len) = match_tuple_label_error(err) {
            text[es + kw_len - 1] = b'x';
            edits += 1;
            continue;
        }
        // `< unique.` comparison misparsed as `unique symbol` type operator.
        if err.starts_with(b"unique")
            && text.get(es + 6).is_none_or(|&b| !is_ident_byte(b))
            && preceded_by_less_than(text, es)
            && !followed_by_word(text, es + 6, b"symbol")
        {
            text[es + 5] = b'x';
            edits += 1;
        }
    }

    edits
}

/// Starting at `import`, match `import ( "..." | '...' )` (with optional
/// whitespace/trailing comma), returning `(end_after_rparen, optional_end_after_dot)`
/// where `optional_end_after_dot` is `Some` when `.Ident` follows and `Ident` is
/// a plain non-keyword type name.
fn match_import_type_call(text: &[u8], import_at: usize) -> Option<(usize, Option<usize>)> {
    let len = text.len();
    let mut j = import_at + 6;
    while j < len && text[j].is_ascii_whitespace() {
        j += 1;
    }
    if text.get(j) != Some(&b'(') {
        return None;
    }
    j += 1;
    while j < len && text[j].is_ascii_whitespace() {
        j += 1;
    }
    let quote = *text.get(j)?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    j += 1;
    while j < len {
        if text[j] == b'\\' {
            j = (j + 2).min(len);
        } else if text[j] == quote {
            j += 1;
            break;
        } else if text[j] == b'\n' {
            return None;
        } else {
            j += 1;
        }
    }
    while j < len && text[j].is_ascii_whitespace() {
        j += 1;
    }
    if text.get(j) == Some(&b',') {
        j += 1;
        while j < len && text[j].is_ascii_whitespace() {
            j += 1;
        }
    }
    if text.get(j) != Some(&b')') {
        return None;
    }
    let call_end = j + 1;
    let mut k = call_end;
    while k < len && text[k].is_ascii_whitespace() {
        k += 1;
    }
    if text.get(k) == Some(&b'.') {
        let dot_end = k + 1;
        let mut m = dot_end;
        while m < len && text[m].is_ascii_whitespace() {
            m += 1;
        }
        let id_start = m;
        while m < len && is_ident_byte(text[m]) {
            m += 1;
        }
        let ident = &text[id_start..m];
        if !ident.is_empty() && ident != b"default" {
            return Some((call_end, Some(dot_end)));
        }
    }
    Some((call_end, None))
}

/// If `kw` is the word immediately preceding `at` (separated only by ASCII
/// whitespace), return `kw`'s start byte.
fn preceding_keyword(text: &[u8], at: usize, kw: &[u8]) -> Option<usize> {
    let mut end = at;
    while end > 0 && text[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let start = end.checked_sub(kw.len())?;
    if &text[start..end] != kw {
        return None;
    }
    if start > 0 && is_ident_byte(text[start - 1]) {
        return None;
    }
    Some(start)
}

fn preceded_by_less_than(text: &[u8], at: usize) -> bool {
    let mut k = at;
    while k > 0 && text[k - 1].is_ascii_whitespace() {
        k -= 1;
    }
    k > 0 && text[k - 1] == b'<' && (k < 2 || text[k - 2] != b'<')
}

fn followed_by_word(text: &[u8], at: usize, word: &[u8]) -> bool {
    let mut k = at;
    while k < text.len() && text[k].is_ascii_whitespace() {
        k += 1;
    }
    text[k..].starts_with(word) && text.get(k + word.len()).is_none_or(|&b| !is_ident_byte(b))
}

fn match_tuple_label_error(err: &[u8]) -> Option<usize> {
    for &kw in TS_TUPLE_LABEL_KEYWORDS {
        if let Some(rest) = err.strip_prefix(kw) {
            let rest = rest.strip_prefix(b"?").unwrap_or(rest);
            let mut k = 0;
            while k < rest.len() && rest[k].is_ascii_whitespace() {
                k += 1;
            }
            if rest.get(k) == Some(&b':') && rest.get(k + 1) != Some(&b':') {
                return Some(kw.len());
            }
        }
    }
    None
}

fn skip_js_non_code(text: &[u8], i: usize) -> Option<usize> {
    let len = text.len();
    match text[i] {
        b'/' if text.get(i + 1) == Some(&b'/') => {
            let mut j = i + 2;
            while j < len && text[j] != b'\n' {
                j += 1;
            }
            Some(j)
        }
        b'/' if text.get(i + 1) == Some(&b'*') => {
            let mut j = i + 2;
            while j + 1 < len {
                if text[j] == b'*' && text[j + 1] == b'/' {
                    return Some(j + 2);
                }
                j += 1;
            }
            Some(len)
        }
        b'\'' | b'"' => {
            let q = text[i];
            let mut j = i + 1;
            while j < len {
                if text[j] == b'\\' {
                    j = (j + 2).min(len);
                } else if text[j] == q {
                    return Some(j + 1);
                } else {
                    j += 1;
                }
            }
            Some(len)
        }
        b'`' => {
            let mut j = i + 1;
            while j < len {
                if text[j] == b'\\' {
                    j = (j + 2).min(len);
                } else if text[j] == b'`' {
                    return Some(j + 1);
                } else {
                    j += 1;
                }
            }
            Some(len)
        }
        _ => None,
    }
}

// ------------------------------------------------------------------------ Rust

/// Repair `tree-sitter-rust` 0.23.3 grammar gaps in place:
///
/// 1. Rust 2024 `unsafe extern ["ABI"] { ... }` blocks: `tree-sitter-rust`
///    0.23.3 predates RFC 3484 and misparses `unsafe extern` as an
///    `unsafe_block`. Blanking `unsafe` before `extern ["ABI"] {` lets the
///    `foreign_mod_item` and all enclosed declarations parse cleanly at their
///    exact original byte spans.
/// 2. `&raw` borrowing a variable named `raw` when NOT followed by `const` or
///    `mut`: `tree-sitter-rust` 0.23.3 greedily lexes `&raw` as a raw-borrow
///    prefix. Rewriting `raw` → `rax` near an error lets the reference parse
///    while the extractor still reads `"raw"` from the original source.
/// 3. `#[cfg(...)]` attributes on struct destructuring pattern fields
///    (`let S { #[cfg(...)] field } = x;`): `tree-sitter-rust` 0.23.3 does not
///    accept `attribute_item` inside `struct_pattern`. Blanking `#[cfg(...)]`
///    when an error starts on it lets the destructured fields parse cleanly;
///    item-level `#[cfg(...)]` never produces an error and is untouched.
fn repair_rust(text: &mut [u8], error_spans: &[(usize, usize)]) -> usize {
    let mut edits = 0usize;
    let len = text.len();

    // Pass 1: `unsafe extern ["ABI"] {` and `&raw` (non-`const`/`mut`).
    let mut i = 0usize;
    while i < len {
        if let Some(next) = skip_rust_non_code(text, i) {
            i = next;
            continue;
        }
        // `unsafe extern ["ABI"] {`
        if text[i..].starts_with(b"unsafe")
            && (i == 0 || !is_rust_ident_byte(text[i - 1]))
            && text.get(i + 6).is_some_and(u8::is_ascii_whitespace)
            && is_unsafe_extern_block(text, i)
        {
            blank_preserving_newlines(&mut text[i..i + 6]);
            edits += 1;
            i += 6;
            continue;
        }
        // `&raw` where `raw` is not followed by `const` or `mut`.
        if text[i] == b'&' {
            let mut k = i + 1;
            while k < len && text[k].is_ascii_whitespace() {
                k += 1;
            }
            if text[k..].starts_with(b"raw")
                && text.get(k + 3).is_none_or(|&b| !is_rust_ident_byte(b))
                && !followed_by_rust_word(text, k + 3, b"const")
                && !followed_by_rust_word(text, k + 3, b"mut")
                && near_error(error_spans, i, k + 3, 12)
            {
                text[k + 2] = b'x';
                edits += 1;
                i = k + 3;
                continue;
            }
        }
        i += 1;
    }

    // Pass 2: `#[cfg(...)]` attributes causing parse errors in struct
    // destructuring patterns (`let S { a, #[cfg(unix)] b } = x;` or
    // `let S { #[cfg(unix)] a } = x;`). `tree-sitter-rust` 0.23.3 emits the
    // ERROR either on `#[cfg(...)]` itself or on the preceding `let S { a,`
    // right up to the whitespace before `#[cfg(...)]`. Item-level and
    // struct-definition `#[cfg(...)]` parse cleanly with no adjacent error.
    let mut p = 0usize;
    while p + 6 <= len {
        if let Some(next) = skip_rust_non_code(text, p) {
            p = next;
            continue;
        }
        if text[p..].starts_with(b"#[cfg(") {
            if let Some(end) = rust_attribute_end(text, p) {
                let mut prev = p;
                while prev > 0 && text[prev - 1].is_ascii_whitespace() {
                    prev -= 1;
                }
                let after_delim = prev > 0 && matches!(text[prev - 1], b',' | b'{');
                let adjacent_to_error = error_spans.iter().any(|&(es, ee)| {
                    (es <= end && ee >= p)
                        || (ee <= p && text[ee..p].iter().all(u8::is_ascii_whitespace))
                });
                if after_delim && adjacent_to_error {
                    blank_preserving_newlines(&mut text[p..end]);
                    edits += 1;
                    p = end;
                    continue;
                }
            }
        }
        p += 1;
    }

    edits
}

/// Whether `unsafe` at `at` starts an `unsafe extern ["ABI"] {` block (and NOT
/// `unsafe extern "C" fn`).
fn is_unsafe_extern_block(text: &[u8], at: usize) -> bool {
    let len = text.len();
    let mut j = at + 6;
    while j < len && text[j].is_ascii_whitespace() {
        j += 1;
    }
    if !text[j..].starts_with(b"extern") || text.get(j + 6).is_some_and(|&b| is_rust_ident_byte(b))
    {
        return false;
    }
    j += 6;
    while j < len && text[j].is_ascii_whitespace() {
        j += 1;
    }
    // Optional ABI string literal `"C"`, `"system"`, etc.
    if text.get(j) == Some(&b'"') {
        j += 1;
        while j < len {
            if text[j] == b'\\' {
                j = (j + 2).min(len);
            } else if text[j] == b'"' {
                j += 1;
                break;
            } else {
                j += 1;
            }
        }
        while j < len && text[j].is_ascii_whitespace() {
            j += 1;
        }
    }
    text.get(j) == Some(&b'{')
}

fn followed_by_rust_word(text: &[u8], at: usize, word: &[u8]) -> bool {
    let mut k = at;
    while k < text.len() && text[k].is_ascii_whitespace() {
        k += 1;
    }
    text[k..].starts_with(word)
        && text
            .get(k + word.len())
            .is_none_or(|&b| !is_rust_ident_byte(b))
}

/// Starting at `#[`, find the index just past the matching `]`, respecting
/// strings, comments, and nested brackets.
fn rust_attribute_end(text: &[u8], start: usize) -> Option<usize> {
    if !text[start..].starts_with(b"#[") {
        return None;
    }
    let len = text.len();
    let mut i = start + 2;
    let mut depth = 1usize;
    while i < len && depth > 0 {
        if let Some(next) = skip_rust_non_code(text, i) {
            i = next;
            continue;
        }
        match text[i] {
            b'[' => depth += 1,
            b']' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    (depth == 0).then_some(i)
}

/// Skip Rust comments, normal/byte/c strings, raw strings `r#"..."#`, and char
/// literals (distinguishing lifetimes `'a` from char literals `'a'`).
fn skip_rust_non_code(text: &[u8], i: usize) -> Option<usize> {
    let len = text.len();
    match text[i] {
        b'/' if text.get(i + 1) == Some(&b'/') => {
            let mut j = i + 2;
            while j < len && text[j] != b'\n' {
                j += 1;
            }
            Some(j)
        }
        b'/' if text.get(i + 1) == Some(&b'*') => {
            let mut j = i + 2;
            let mut depth = 1usize;
            while j + 1 < len && depth > 0 {
                if text[j] == b'/' && text[j + 1] == b'*' {
                    depth += 1;
                    j += 2;
                } else if text[j] == b'*' && text[j + 1] == b'/' {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            Some(j)
        }
        b'"' => Some(skip_rust_quoted_string(text, i)),
        b'b' | b'c'
            if text.get(i + 1) == Some(&b'"') && (i == 0 || !is_rust_ident_byte(text[i - 1])) =>
        {
            Some(skip_rust_quoted_string(text, i + 1))
        }
        b'r' if i == 0 || !is_rust_ident_byte(text[i - 1]) => skip_rust_raw_string(text, i),
        b'b' | b'c'
            if text.get(i + 1) == Some(&b'r') && (i == 0 || !is_rust_ident_byte(text[i - 1])) =>
        {
            skip_rust_raw_string(text, i + 1)
        }
        b'\'' => {
            // Char literal `'x'`, `'\n'`, `'\u{...}'` vs lifetime `'a`.
            if text.get(i + 1) == Some(&b'\\') {
                let mut j = i + 2;
                while j < len && text[j] != b'\'' && text[j] != b'\n' {
                    j += 1;
                }
                if text.get(j) == Some(&b'\'') {
                    return Some(j + 1);
                }
            } else if i + 2 < len && text[i + 1] != b'\'' && text[i + 2] == b'\'' {
                return Some(i + 3);
            }
            None
        }
        _ => None,
    }
}

fn skip_rust_quoted_string(text: &[u8], quote_at: usize) -> usize {
    let len = text.len();
    let mut j = quote_at + 1;
    while j < len {
        if text[j] == b'\\' {
            j = (j + 2).min(len);
        } else if text[j] == b'"' {
            return j + 1;
        } else {
            j += 1;
        }
    }
    len
}

fn skip_rust_raw_string(text: &[u8], r_at: usize) -> Option<usize> {
    let len = text.len();
    let mut j = r_at + 1;
    let mut hashes = 0usize;
    while j < len && text[j] == b'#' {
        hashes += 1;
        j += 1;
    }
    if text.get(j) != Some(&b'"') {
        return None;
    }
    j += 1;
    while j < len {
        if text[j] == b'"' {
            let mut k = j + 1;
            let mut matched = 0usize;
            while matched < hashes && text.get(k) == Some(&b'#') {
                matched += 1;
                k += 1;
            }
            if matched == hashes {
                return Some(k);
            }
        }
        j += 1;
    }
    Some(len)
}

// ----------------------------------------------------------------------- Shell

/// Repair `tree-sitter-bash` 0.25.1 errors inside `${var:+ word}` /
/// `${var:- word}` parameter expansions whose alternate/default text starts
/// with whitespace and/or bare parentheses wrapping a nested expansion, e.g.
/// `"${pin:+ (${pin:0:12})}"`.
///
/// Only rewrites bytes inside an ERROR span that sits directly after a
/// parameter-expansion operator (`:+`, `:-`, `:=`, `:?`, `+`, `-`, `=`, `?`)
/// inside `${...}` and never touches `$(` command substitutions or `${...}`.
fn repair_shell(text: &mut [u8], error_spans: &[(usize, usize)]) -> usize {
    let mut edits = 0usize;
    for &(es, ee) in error_spans {
        if es < 3 || ee > text.len() || es >= ee {
            continue;
        }
        // Check that `es` sits directly after `${<ident><op>` where `<op>` is
        // one of `:+`, `:-`, `:=`, `:?`, `+`, `-`, `=`, `?`.
        if !matches!(text[es - 1], b'+' | b'-' | b'=' | b'?') {
            continue;
        }
        let op_start = if text.get(es - 2) == Some(&b':') {
            es - 2
        } else {
            es - 1
        };
        let mut v = op_start;
        while v > 0 && (text[v - 1].is_ascii_alphanumeric() || text[v - 1] == b'_') {
            v -= 1;
        }
        if v == op_start || v < 2 || &text[v - 2..v] != b"${" {
            continue;
        }
        // Find the matching `}` for the outer `${` (respecting nested `${...}`).
        let mut depth = 1usize;
        let mut j = es;
        let mut close_at = None;
        while j < text.len() {
            if text[j] == b'\\' {
                j = (j + 2).min(text.len());
                continue;
            }
            if text[j] == b'$' && text.get(j + 1) == Some(&b'{') {
                depth += 1;
                j += 2;
                continue;
            }
            if text[j] == b'}' {
                depth -= 1;
                if depth == 0 {
                    close_at = Some(j);
                    break;
                }
            }
            j += 1;
        }
        let Some(end) = close_at else {
            continue;
        };
        // Replace leading spaces/tabs and bare `(` / `)` at depth 1 (outside
        // nested `${...}` and never `$(`) with `x`.
        let mut cur = es;
        let mut nested = 0usize;
        let mut changed = false;
        while cur < end {
            if text[cur] == b'\\' {
                cur = (cur + 2).min(end);
                continue;
            }
            if text[cur] == b'$' && text.get(cur + 1) == Some(&b'{') {
                nested += 1;
                cur += 2;
                continue;
            }
            if text[cur] == b'}' && nested > 0 {
                nested -= 1;
                cur += 1;
                continue;
            }
            if nested == 0 {
                let is_bare_paren =
                    (text[cur] == b'(' && text[cur - 1] != b'$') || text[cur] == b')';
                let is_space = matches!(text[cur], b' ' | b'\t');
                if is_bare_paren || is_space {
                    text[cur] = b'x';
                    changed = true;
                }
            }
            cur += 1;
        }
        if changed {
            edits += 1;
        }
    }
    edits
}
