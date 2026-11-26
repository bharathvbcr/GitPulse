//! String literals, and the symbol that writes each one.
//!
//! A name query cannot see a string-keyed protocol. The coupling is the value
//! (`session.spawn`), not a call edge. This walk records each decoded literal
//! with the innermost symbol whose span contains it.
//!
//! A Rust module-level `const` or `static` is often not a symbol — private
//! items are not indexed — so the literal's own site would be unenclosed and
//! the method that *uses* the const would not show up. Those names are resolved
//! inside the file and each use is recorded as another site of the same value.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use tree_sitter::{Node, TreeCursor};

use crate::model::{ExtractedLiteral, ExtractedSymbol, SymbolKind};

/// Most literals one file publishes. A file past this keeps the first ones and
/// says the list is a prefix.
const MAX_LITERALS_PER_FILE: usize = 4096;
/// Values longer than this are not a protocol key. Skipped, not truncated:
/// a prefix of a blob is a different string.
const MAX_LITERAL_BYTES: usize = 1024;
/// How far a const may point at another const before the chain is dropped.
const MAX_CONST_HOPS: usize = 8;
/// Identifiers collected while resolving const uses. Past this the walk stops
/// adding uses and says so.
const MAX_IDENTIFIERS: usize = 8192;

/// Index the string literals of one parsed file.
///
/// When the deadline has already passed, the returned list is empty and the
/// diagnostic says the literals were not indexed. A partial list is not
/// published as a complete one. The cap, by contrast, keeps the literals it
/// did see and names the cut.
pub fn index_literals(
    root: Node,
    source: &str,
    lang: &str,
    symbols: &[ExtractedSymbol],
    deadline: Instant,
) -> (Vec<ExtractedLiteral>, Vec<String>) {
    if Instant::now() >= deadline {
        return exhausted();
    }
    let lines = LineStarts::new(source);
    let mut sites = Vec::new();
    let mut notes = Vec::new();
    let mut capped = false;
    let mut cursor = root.walk();
    loop {
        if Instant::now() >= deadline {
            return exhausted();
        }
        let node = cursor.node();
        if let Some(value) = decode_string(lang, node, source) {
            if accept_value(&value) {
                if sites.len() >= MAX_LITERALS_PER_FILE {
                    capped = true;
                    break;
                }
                sites.push(site_at(&lines, symbols, node.start_byte(), value));
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        if !advance(&mut cursor, root) {
            break;
        }
    }
    if lang == "rust" && !capped {
        match propagate_rust_consts(root, source, &lines, symbols, deadline, &mut sites) {
            Err(note) if note == "exhausted" => return exhausted(),
            Err(note) => notes.push(note),
            Ok(()) => {}
        }
    }
    if capped || sites.len() > MAX_LITERALS_PER_FILE {
        sites.truncate(MAX_LITERALS_PER_FILE);
        notes.push(format!(
            "string literals truncated at {MAX_LITERALS_PER_FILE} for this file"
        ));
    }
    dedup_sort(&mut sites);
    (sites, notes)
}

fn exhausted() -> (Vec<ExtractedLiteral>, Vec<String>) {
    (
        Vec::new(),
        vec!["string literals were not indexed: extraction budget exhausted".to_string()],
    )
}

fn advance(cursor: &mut TreeCursor<'_>, root: Node) -> bool {
    loop {
        if cursor.goto_next_sibling() {
            return true;
        }
        if !cursor.goto_parent() {
            return false;
        }
        if cursor.node().id() == root.id() {
            return false;
        }
    }
}

fn site_at(
    lines: &LineStarts<'_>,
    symbols: &[ExtractedSymbol],
    start: usize,
    value: String,
) -> ExtractedLiteral {
    let (qualified, name) = enclosing(symbols, start);
    ExtractedLiteral {
        value,
        start_byte: start,
        line: lines.line_of(start),
        enclosing_qualified_name: qualified,
        enclosing_name: name,
    }
}

#[cfg(test)]
thread_local! {
    /// Bytes of source read to answer line numbers, for the bound test.
    static LINE_BYTES_READ: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Line numbers for byte offsets in one file.
///
/// The newline offsets are found once, on the first lookup, and each lookup is
/// a binary search. Counting newlines from the start of the file for every site
/// cost (sites x file size): about 0.45 s of extraction for 4,096 literals at
/// the end of a 1 MB file (release build). Sites arrive out of order (const
/// uses are recorded after the walk), so a running count would not do.
struct LineStarts<'a> {
    source: &'a str,
    newlines: OnceCell<Vec<usize>>,
}

impl<'a> LineStarts<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            newlines: OnceCell::new(),
        }
    }

    /// One more than the newlines before `start`; an offset past the end counts
    /// every newline in the file.
    fn line_of(&self, start: usize) -> u32 {
        let newlines = self.newlines.get_or_init(|| {
            #[cfg(test)]
            LINE_BYTES_READ.with(|read| read.set(read.get() + self.source.len()));
            self.source
                .bytes()
                .enumerate()
                .filter_map(|(at, byte)| (byte == b'\n').then_some(at))
                .collect()
        });
        let end = start.min(self.source.len());
        let before = newlines.partition_point(|&at| at < end);
        u32::try_from(before + 1).unwrap_or(u32::MAX)
    }
}

/// Innermost non-file, non-module symbol containing `byte`. A tie keeps the
/// one that starts later.
fn enclosing(symbols: &[ExtractedSymbol], byte: usize) -> (String, String) {
    let mut best: Option<&ExtractedSymbol> = None;
    for symbol in symbols {
        if matches!(symbol.kind, SymbolKind::File | SymbolKind::Module) {
            continue;
        }
        if byte < symbol.span.start_byte || byte >= symbol.span.end_byte {
            continue;
        }
        let replace = match best {
            None => true,
            Some(current) => {
                let span = symbol.span.end_byte - symbol.span.start_byte;
                let current_span = current.span.end_byte - current.span.start_byte;
                span < current_span
                    || (span == current_span && symbol.span.start_byte >= current.span.start_byte)
            }
        };
        if replace {
            best = Some(symbol);
        }
    }
    match best {
        Some(symbol) => (symbol.qualified_name.clone(), symbol.name.clone()),
        None => (String::new(), String::new()),
    }
}

fn accept_value(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_LITERAL_BYTES && !value.as_bytes().contains(&0)
}

struct ConstBinding {
    direct: Vec<String>,
    aliases: Vec<String>,
    name_start: usize,
}

fn propagate_rust_consts(
    root: Node,
    source: &str,
    lines: &LineStarts<'_>,
    symbols: &[ExtractedSymbol],
    deadline: Instant,
    sites: &mut Vec<ExtractedLiteral>,
) -> Result<(), String> {
    let mut bindings: BTreeMap<String, ConstBinding> = BTreeMap::new();
    let mut cursor = root.walk();
    loop {
        if Instant::now() >= deadline {
            return Err("exhausted".to_string());
        }
        let node = cursor.node();
        if matches!(node.kind(), "const_item" | "static_item") && module_level(node) {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node
                    .utf8_text(source.as_bytes())
                    .unwrap_or("")
                    .to_string();
                if !name.is_empty() {
                    let mut direct = Vec::new();
                    let mut aliases = Vec::new();
                    if let Some(value) = node.child_by_field_name("value") {
                        collect_value(value, source, &mut direct, &mut aliases);
                    }
                    bindings.insert(
                        name,
                        ConstBinding {
                            direct,
                            aliases,
                            name_start: name_node.start_byte(),
                        },
                    );
                }
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        if !advance(&mut cursor, root) {
            break;
        }
    }
    if bindings.is_empty() {
        return Ok(());
    }
    let mut seen_idents = 0usize;
    let mut cursor = root.walk();
    loop {
        if Instant::now() >= deadline {
            return Err("exhausted".to_string());
        }
        let node = cursor.node();
        if node.kind() == "identifier" {
            seen_idents += 1;
            if seen_idents > MAX_IDENTIFIERS {
                return Err(format!(
                    "const-use indexing stopped after {MAX_IDENTIFIERS} identifiers"
                ));
            }
            if let Ok(name) = node.utf8_text(source.as_bytes()) {
                let defined_here = bindings
                    .get(name)
                    .is_some_and(|binding| binding.name_start == node.start_byte());
                if bindings.contains_key(name) && !defined_here {
                    let values = resolve_const(name, &bindings, 0, &mut BTreeSet::new());
                    for value in values {
                        if !accept_value(&value) {
                            continue;
                        }
                        if sites.len() >= MAX_LITERALS_PER_FILE {
                            return Ok(());
                        }
                        sites.push(site_at(lines, symbols, node.start_byte(), value));
                    }
                }
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        if !advance(&mut cursor, root) {
            break;
        }
    }
    Ok(())
}

fn module_level(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(ancestor) = current {
        match ancestor.kind() {
            "function_item"
            | "impl_item"
            | "closure_expression"
            | "trait_item"
            | "function_signature_item"
            | "macro_invocation"
            | "block" => return false,
            _ => current = ancestor.parent(),
        }
    }
    true
}

fn collect_value(value: Node, source: &str, direct: &mut Vec<String>, aliases: &mut Vec<String>) {
    if let Some(text) = decode_rust_node(value, source) {
        if accept_value(&text) && !direct.contains(&text) {
            direct.push(text);
        }
        return;
    }
    let mut cursor = value.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let node = cursor.node();
        if let Some(text) = decode_rust_node(node, source) {
            if accept_value(&text) && !direct.contains(&text) {
                direct.push(text);
            }
        } else if node.kind() == "identifier" {
            if let Ok(name) = node.utf8_text(source.as_bytes()) {
                if !name.is_empty() && !aliases.iter().any(|have| have == name) {
                    aliases.push(name.to_string());
                }
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        if !advance(&mut cursor, value) {
            break;
        }
    }
}

fn resolve_const(
    name: &str,
    bindings: &BTreeMap<String, ConstBinding>,
    depth: usize,
    stack: &mut BTreeSet<String>,
) -> Vec<String> {
    if depth > MAX_CONST_HOPS || !stack.insert(name.to_string()) {
        return Vec::new();
    }
    let Some(binding) = bindings.get(name) else {
        stack.remove(name);
        return Vec::new();
    };
    let direct = binding.direct.clone();
    let aliases = binding.aliases.clone();
    let mut values = direct;
    for alias in &aliases {
        for value in resolve_const(alias, bindings, depth + 1, stack) {
            if !values.contains(&value) {
                values.push(value);
            }
        }
    }
    stack.remove(name);
    values
}

pub(crate) fn dedup_sort(sites: &mut Vec<ExtractedLiteral>) {
    sites.sort_by(|left, right| {
        left.start_byte
            .cmp(&right.start_byte)
            .then(left.value.cmp(&right.value))
            .then(
                left.enclosing_qualified_name
                    .cmp(&right.enclosing_qualified_name),
            )
    });
    sites.dedup_by(|left, right| {
        left.start_byte == right.start_byte
            && left.value == right.value
            && left.enclosing_qualified_name == right.enclosing_qualified_name
    });
}

fn decode_string(lang: &str, node: Node, source: &str) -> Option<String> {
    match lang {
        "rust" => decode_rust_node(node, source),
        "go" => decode_go_node(node, source),
        "python" => decode_python_node(node, source),
        "javascript" | "typescript" | "tsx" => decode_js_node(node, source),
        _ => None,
    }
}

fn decode_rust_node(node: Node, source: &str) -> Option<String> {
    let text = node.utf8_text(source.as_bytes()).ok()?;
    match node.kind() {
        "raw_string_literal" => decode_rust_raw(text),
        "string_literal" => decode_quoted(text, '"', Escape::Rust),
        _ => None,
    }
}

fn decode_go_node(node: Node, source: &str) -> Option<String> {
    let text = node.utf8_text(source.as_bytes()).ok()?;
    match node.kind() {
        "raw_string_literal" => {
            let inner = text.strip_prefix('`')?.strip_suffix('`')?;
            Some(inner.to_string())
        }
        "interpreted_string_literal" => decode_quoted(text, '"', Escape::Go),
        _ => None,
    }
}

fn decode_python_node(node: Node, source: &str) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            if cursor.node().kind() == "interpolation" {
                return None;
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    let text = node.utf8_text(source.as_bytes()).ok()?;
    let (quote, body_at) = python_quotes(text)?;
    let inner = text.get(body_at..text.len() - quote.len())?;
    let prefix = text[..body_at].to_ascii_lowercase();
    if prefix.contains('f') {
        return None;
    }
    if prefix.contains('r') {
        return Some(inner.to_string());
    }
    decode_escapes(inner, Escape::Python)
}

fn decode_js_node(node: Node, source: &str) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let text = node.utf8_text(source.as_bytes()).ok()?;
    let quote = text.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    decode_quoted(text, quote, Escape::Js)
}

fn python_quotes(text: &str) -> Option<(&str, usize)> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
        index += 1;
    }
    let rest = text.get(index..)?;
    if rest.starts_with("\"\"\"") && rest.ends_with("\"\"\"") && rest.len() >= 6 {
        return Some(("\"\"\"", index + 3));
    }
    if rest.starts_with("'''") && rest.ends_with("'''") && rest.len() >= 6 {
        return Some(("'''", index + 3));
    }
    if rest.starts_with('"') && rest.ends_with('"') && rest.len() >= 2 {
        return Some(("\"", index + 1));
    }
    if rest.starts_with('\'') && rest.ends_with('\'') && rest.len() >= 2 {
        return Some(("'", index + 1));
    }
    None
}

fn decode_rust_raw(text: &str) -> Option<String> {
    let rest = text.strip_prefix('r')?;
    let hashes = rest.bytes().take_while(|byte| *byte == b'#').count();
    let rest = rest.get(hashes..)?;
    let inner = rest.strip_prefix('"')?;
    if hashes == 0 {
        return Some(inner.strip_suffix('"')?.to_string());
    }
    let close = format!("\"{}", "#".repeat(hashes));
    Some(inner.strip_suffix(&close)?.to_string())
}

fn decode_quoted(text: &str, quote: char, escape: Escape) -> Option<String> {
    let inner = text.strip_prefix(quote)?.strip_suffix(quote)?;
    decode_escapes(inner, escape)
}

#[derive(Clone, Copy)]
enum Escape {
    Rust,
    Go,
    Python,
    Js,
}

fn decode_escapes(inner: &str, escape: Escape) -> Option<String> {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        let next = chars.next()?;
        let decoded = match next {
            '\\' => '\\',
            '\'' => '\'',
            '"' => '"',
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            '0' => '\0',
            'a' if matches!(escape, Escape::Go | Escape::Python) => '\u{0007}',
            'b' if matches!(escape, Escape::Go | Escape::Python) => '\u{0008}',
            'f' if matches!(escape, Escape::Go | Escape::Python) => '\u{000c}',
            'v' if matches!(escape, Escape::Go | Escape::Python) => '\u{000b}',
            'x' => {
                let hex = take_hex(&mut chars, 2)?;
                char::from_u32(hex)?
            }
            'u' => decode_unicode(&mut chars, escape)?,
            'U' if matches!(escape, Escape::Go) => {
                let hex = take_hex(&mut chars, 8)?;
                char::from_u32(hex)?
            }
            '\n' if matches!(escape, Escape::Rust) => {
                while matches!(chars.peek(), Some(' ' | '\t' | '\n' | '\r')) {
                    chars.next();
                }
                continue;
            }
            '\r' if matches!(escape, Escape::Rust) && chars.peek() == Some(&'\n') => {
                chars.next();
                while matches!(chars.peek(), Some(' ' | '\t' | '\n' | '\r')) {
                    chars.next();
                }
                continue;
            }
            '\n' if matches!(escape, Escape::Python) => continue,
            '\r' if matches!(escape, Escape::Python) && chars.peek() == Some(&'\n') => {
                chars.next();
                continue;
            }
            _ => return None,
        };
        out.push(decoded);
    }
    Some(out)
}

fn decode_unicode(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    escape: Escape,
) -> Option<char> {
    match escape {
        Escape::Rust => {
            if chars.next() != Some('{') {
                return None;
            }
            let mut hex = String::new();
            for _ in 0..6 {
                match chars.peek() {
                    Some(ch) if ch.is_ascii_hexdigit() => hex.push(chars.next()?),
                    _ => break,
                }
            }
            if hex.is_empty() || chars.next() != Some('}') {
                return None;
            }
            char::from_u32(u32::from_str_radix(&hex, 16).ok()?)
        }
        Escape::Js | Escape::Go | Escape::Python => {
            if matches!(escape, Escape::Js) && chars.peek() == Some(&'{') {
                chars.next();
                let mut hex = String::new();
                for _ in 0..6 {
                    match chars.peek() {
                        Some(ch) if ch.is_ascii_hexdigit() => hex.push(chars.next()?),
                        _ => break,
                    }
                }
                if hex.is_empty() || chars.next() != Some('}') {
                    return None;
                }
                return char::from_u32(u32::from_str_radix(&hex, 16).ok()?);
            }
            let hex = take_hex(chars, 4)?;
            char::from_u32(hex)
        }
    }
}

fn take_hex(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, n: usize) -> Option<u32> {
    let mut hex = String::with_capacity(n);
    for _ in 0..n {
        let ch = chars.next()?;
        if !ch.is_ascii_hexdigit() {
            return None;
        }
        hex.push(ch);
    }
    u32::from_str_radix(&hex, 16).ok()
}

#[cfg(test)]
mod line_starts_tests {
    use super::*;
    use std::time::Duration;

    /// The per-site count this module used before `LineStarts`, kept as the
    /// oracle.
    fn counted_line(source: &str, start: usize) -> u32 {
        let end = start.min(source.len());
        let newlines = source.as_bytes()[..end]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count();
        u32::try_from(newlines + 1).unwrap_or(u32::MAX)
    }

    #[test]
    fn every_offset_gets_the_line_the_per_site_count_gave() {
        for source in [
            "",
            "\n",
            "\n\n\n",
            "no newline at all",
            "a\nb\r\nc\n",
            "trailing\n",
            "\nleading",
            "é\nü\n€ multi-byte\n",
        ] {
            let lines = LineStarts::new(source);
            // Every offset, the end, and offsets past the end.
            for start in 0..=source.len() + 3 {
                assert_eq!(
                    lines.line_of(start),
                    counted_line(source, start),
                    "offset {start} of {source:?}"
                );
            }
        }
    }

    /// 4,096 literals after ~900 KB of filler, where counting newlines from
    /// byte 0 for every site read about 3.9 GB. The bound is on bytes read for
    /// line numbers, not on time: the file is read once, however many sites
    /// it has.
    #[test]
    fn many_literals_late_in_a_large_file_read_it_once_for_line_numbers() {
        let filler: String = (0..12_000)
            .map(|n| {
                format!("// filler line {n} ....................................................\n")
            })
            .collect();
        let literals: String = (0..MAX_LITERALS_PER_FILE)
            .map(|n| format!("        \"literal-value-{n}\",\n"))
            .collect();
        let source = format!(
            "{filler}pub fn many() -> Vec<&'static str> {{\n    vec![\n{literals}    ]\n}}\n"
        );
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(&source, None).unwrap();

        LINE_BYTES_READ.with(|read| read.set(0));
        let deadline = Instant::now() + Duration::from_secs(600);
        let (sites, notes) = index_literals(tree.root_node(), &source, "rust", &[], deadline);

        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(sites.len(), MAX_LITERALS_PER_FILE);
        // The oracle in one pass: the line each byte starts on, counted
        // forward. Calling `counted_line` per site here is the very cost under
        // test, and took ~20 s in a debug build.
        let mut by_start: Vec<_> = sites.iter().collect();
        by_start.sort_by_key(|site| site.start_byte);
        let (mut line, mut at) = (1u32, 0usize);
        for site in by_start {
            line += u32::try_from(
                source.as_bytes()[at..site.start_byte]
                    .iter()
                    .filter(|byte| **byte == b'\n')
                    .count(),
            )
            .unwrap();
            at = site.start_byte;
            assert_eq!(site.line, line, "{}", site.value);
        }
        let read = LINE_BYTES_READ.with(|read| read.get());
        assert!(
            read <= source.len(),
            "line numbers read {read} bytes of a {}-byte file",
            source.len()
        );
    }
}
