//! Svelte template repairs: same-width rewrites for constructs
//! `tree-sitter-svelte-ng` 1.0.2 rejects.
//!
//! Two shapes break valid Svelte 5 files:
//!
//! 1. **Svelte 5 `{@attach expr}` attachments** inside element/component start
//!    tags (`<div {@attach watchRow}>`). `tree-sitter-svelte-ng` 1.0.2 predates
//!    Svelte 5.29 attachments and its external scanner refuses any `{` followed
//!    by `@` inside an attribute position, which breaks the `start_tag` and can
//!    cascade into a whole-file ERROR. Writing spaces over the 7 bytes `@attach`
//!    turns it into `{       expr}`, which the grammar parses as a shorthand
//!    expression attribute whose `svelte_raw_text` starts directly on `expr`,
//!    keeping every byte offset unchanged so `embedded` extracts the attachment
//!    expression's references and calls at their exact offsets.
//! 2. **Bare `&` or `>` in HTML text.** `tree-sitter-svelte-ng` reads HTML text
//!    with HTML scanner rules that reject a bare `&` (`Code Age & Half-Life`)
//!    and a `>` following text (`for > 6 months`). Each becomes an ERROR node
//!    that swallows the text run.
//!
//! # The rewrite
//!
//! The repaired copy is only *parsed*. Its length is the file's, so every byte
//! offset its tree reports is an offset of the original file, and the
//! extractors go on reading the original text through the repaired tree, as
//! they do for Kotlin's `;` rewrite ([`crate::kotlinsemi`]). The copy is
//! dropped as soon as the last parse is done; no node's text is ever taken from
//! it.
//!
//! * First, `{@attach` inside start tags (outside `<script>`, `<style>`, HTML
//!   comments, and quoted attribute values) has `@attach` overwritten with
//!   spaces. When that produces edits, the file is re-parsed before scanning
//!   ampersands so a cascading whole-file ERROR no longer hides `<script>`,
//!   `<style>`, and `{...}` shields.
//! * Every `&` that does not start a character reference (`&amp;`, `&#38;`,
//!   `&#x26;`) is written over, except inside the places `&` is code: a
//!   `<script>` or `<style>` element, and a `{...}` of any kind (`{a && b}`,
//!   `class={x && y}`). Both are read from the failed tree, which still holds
//!   them as nodes beside its errors.
//! * A `>` is written over only where the re-parse names it: an ERROR or MISSING
//!   node that starts on a `>`, inside an element but outside every tag and
//!   every `{...}`. Each pass can reveal the next one (`a > b > c`), so the
//!   rewrite repeats, at most [`MAX_PASSES`] times.
//!
//! The result replaces the failed tree **only if it holds strictly less error**
//! (bytes first, then MISSING tokens). A file with a real error of its own
//! keeps the failed tree and its regions, exactly as before. Anything that runs
//! past the deadline leaves the file as it was.

use std::time::Instant;

use tree_sitter::{Node, Parser, Tree};

use crate::treesitter::{parse_until, DEADLINE_CHECK_STRIDE};

/// Parses after the first. A `>` is revealed one at a time inside a text run,
/// so this is far above any plain file (the corpus's worst holds three).
pub(crate) const MAX_PASSES: usize = 16;

/// The ASCII letter written over a bare `&` or `>`.
const FILL: u8 = b'x';

#[cfg(test)]
thread_local! {
    /// How many times [`repair`] was entered on this thread.
    pub(crate) static ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Set by a test to see what the code did before the repair existed.
    pub(crate) static DISABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// What a repair made of a file.
pub(crate) struct Repaired {
    /// The tree of the rewritten copy, on the file's own offsets.
    pub(crate) tree: Tree,
    /// How many characters were written over.
    pub(crate) edits: usize,
}

/// Try to make `first`, the failed parse of `source`, hold less error by
/// writing a letter over a bare `&` or `>` of the template text.
///
/// `None` leaves the file as it was: nothing to write over, no gain, a deadline
/// or a rewrite that is not UTF-8. The copy never outlives this call.
pub(crate) fn repair(
    parser: &mut Parser,
    first: &Tree,
    source: &str,
    deadline: Instant,
) -> Option<Repaired> {
    #[cfg(test)]
    {
        ATTEMPTS.with(|count| count.set(count.get() + 1));
        if DISABLED.with(|flag| flag.get()) {
            return None;
        }
    }

    if Instant::now() >= deadline {
        return None;
    }
    let mut text = source.as_bytes().to_vec();
    let mut current: Option<Tree> = None;
    let mut edits = write_over_attachments(&mut text);
    if edits > 0 {
        let rewritten = std::str::from_utf8(&text).ok()?;
        match parse_until(parser, rewritten, deadline) {
            (Some(next), false) => current = Some(next),
            _ => return None,
        }
    }
    let after_attach = current.as_ref().unwrap_or(first);
    if after_attach.root_node().has_error() {
        let shielded = shielded_ranges(after_attach.root_node(), deadline)?;
        let amp_edits = write_over_ampersands(&mut text, &shielded);
        edits += amp_edits;
        let mut dirty = amp_edits > 0;
        for pass in 0..=MAX_PASSES {
            if dirty {
                // Only an ASCII byte was written over an ASCII byte.
                let rewritten = std::str::from_utf8(&text).ok()?;
                match parse_until(parser, rewritten, deadline) {
                    (Some(next), false) => current = Some(next),
                    _ => return None,
                }
            }
            let tree = current.as_ref().unwrap_or(first);
            if !tree.root_node().has_error() || pass == MAX_PASSES {
                break;
            }
            let fresh = greater_thans(tree.root_node(), &text, deadline)?;
            if fresh.is_empty() {
                break;
            }
            for at in fresh {
                text[at] = FILL;
                edits += 1;
            }
            dirty = true;
        }
    }
    let tree = current?;
    if error_of(tree.root_node(), deadline)? < error_of(first.root_node(), deadline)? {
        Some(Repaired { tree, edits })
    } else {
        None
    }
}

/// Blank `@attach` inside Svelte 5 `{@attach expr}` attributes on element and
/// component start tags with ASCII spaces, preserving exact byte length.
///
/// Scans the source linearly while skipping `<script>…</script>`,
/// `<style>…</style>`, HTML comments (`<!--…-->`), quoted attribute values, and
/// nested `{…}` expressions so JS strings or comments containing `{@attach`
/// are never modified.
fn write_over_attachments(text: &mut [u8]) -> usize {
    const PREFIX: &[u8] = b"@attach";
    let mut written = 0usize;
    let mut i = 0usize;
    let len = text.len();
    while i < len {
        // HTML comment `<!-- ... -->`
        if text[i..].starts_with(b"<!--") {
            if let Some(end) = find_bytes(text, i + 4, b"-->") {
                i = end + 3;
                continue;
            }
            break;
        }
        // Start tag `<tag ...>` (skipping `</...>` end tags and `<!...>`).
        if text[i] == b'<'
            && i + 1 < len
            && (text[i + 1].is_ascii_alphabetic() || text[i + 1] == b'_')
        {
            let name_start = i + 1;
            let mut j = name_start;
            while j < len
                && (text[j].is_ascii_alphanumeric() || matches!(text[j], b'-' | b':' | b'.' | b'_'))
            {
                j += 1;
            }
            let is_script = text[name_start..j].eq_ignore_ascii_case(b"script");
            let is_style = text[name_start..j].eq_ignore_ascii_case(b"style");
            // Scan inside the start tag up to its closing `>`.
            let mut self_closing = false;
            while j < len {
                match text[j] {
                    b'>' => {
                        self_closing = j > name_start && text[j - 1] == b'/';
                        j += 1;
                        break;
                    }
                    b'"' | b'\'' => {
                        let quote = text[j];
                        j += 1;
                        while j < len && text[j] != quote {
                            if text[j] == b'{' {
                                j = skip_balanced_js_braces(text, j);
                            } else {
                                j += 1;
                            }
                        }
                        if j < len {
                            j += 1;
                        }
                    }
                    b'{' => {
                        let mut k = j + 1;
                        while k < len && text[k].is_ascii_whitespace() {
                            k += 1;
                        }
                        if text[k..].starts_with(PREFIX)
                            && text
                                .get(k + PREFIX.len())
                                .is_some_and(u8::is_ascii_whitespace)
                        {
                            for b in &mut text[k..k + PREFIX.len()] {
                                *b = b' ';
                            }
                            written += 1;
                        }
                        j = skip_balanced_js_braces(text, j);
                    }
                    _ => j += 1,
                }
            }
            i = j;
            if !self_closing && (is_script || is_style) {
                let close = if is_script {
                    b"</script" as &[u8]
                } else {
                    b"</style"
                };
                if let Some(close_at) = find_ascii_ci_bytes(text, i, close) {
                    if let Some(gt) = text[close_at..].iter().position(|&b| b == b'>') {
                        i = close_at + gt + 1;
                        continue;
                    }
                }
                break;
            }
            continue;
        }
        // Outside tags: skip top-level `{...}` expressions (e.g. `{#if ...}`)
        // so `<` inside JS expressions (`{a < b}`) does not look like a tag.
        if text[i] == b'{' {
            i = skip_balanced_js_braces(text, i);
            continue;
        }
        i += 1;
    }
    written
}

/// Starting at `{`, advance past the matching `}` respecting JS strings,
/// template literals, line/block comments, and nested `{...}`.
fn skip_balanced_js_braces(text: &[u8], start: usize) -> usize {
    let len = text.len();
    if start >= len || text[start] != b'{' {
        return start.saturating_add(1).min(len);
    }
    let mut i = start + 1;
    let mut depth = 1usize;
    while i < len && depth > 0 {
        match text[i] {
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                i += 1;
            }
            b'\'' | b'"' => {
                let q = text[i];
                i += 1;
                while i < len {
                    if text[i] == b'\\' {
                        i = (i + 2).min(len);
                    } else if text[i] == q {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
            }
            b'`' => {
                i = skip_js_template_string(text, i);
            }
            b'/' if text.get(i + 1) == Some(&b'/') => {
                i += 2;
                while i < len && text[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if text.get(i + 1) == Some(&b'*') => {
                if let Some(end) = find_bytes(text, i + 2, b"*/") {
                    i = end + 2;
                } else {
                    return len;
                }
            }
            _ => i += 1,
        }
    }
    i
}

/// Starting at `` ` ``, advance past the closing `` ` `` respecting escapes and
/// `${...}` template interpolations.
fn skip_js_template_string(text: &[u8], start: usize) -> usize {
    let len = text.len();
    let mut i = start + 1;
    while i < len {
        match text[i] {
            b'\\' => i = (i + 2).min(len),
            b'`' => return i + 1,
            b'$' if text.get(i + 1) == Some(&b'{') => {
                i = skip_balanced_js_braces(text, i + 1);
            }
            _ => i += 1,
        }
    }
    len
}

fn find_bytes(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from + needle.len() > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|pos| from + pos)
}

fn find_ascii_ci_bytes(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from + needle.len() > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
        .map(|pos| from + pos)
}

/// How much of a tree is error: the bytes under its outermost ERROR nodes, then
/// the MISSING tokens a hidden zero-width recovery put in.
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

/// The byte ranges where `&` is code, from the tree of the failed parse: whole
/// `<script>` and `<style>` elements, and every `{` .. `}` pair.
///
/// `None` is a deadline.
fn shielded_ranges(root: Node, deadline: Instant) -> Option<Vec<(usize, usize)>> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
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
        if matches!(node.kind(), "script_element" | "style_element") {
            ranges.push((node.start_byte(), node.end_byte()));
            continue;
        }
        let children: Vec<Node> = node.children(&mut cursor).collect();
        let mut open: Option<usize> = None;
        for child in &children {
            if child.is_named() {
                continue;
            }
            match child.kind() {
                "{" => open = Some(child.start_byte()),
                "}" => {
                    if let Some(start) = open.take() {
                        ranges.push((start, child.end_byte()));
                    }
                }
                _ => {}
            }
        }
        stack.extend(children.into_iter().filter(|child| child.is_named()));
    }
    ranges.sort_unstable();
    Some(ranges)
}

/// Whether `at` is inside one of the sorted `ranges`.
fn shielded(ranges: &[(usize, usize)], at: usize) -> bool {
    // Ranges nest and overlap (a `{}` inside a `{}`), so the one that starts
    // last before `at` is not the only one that can hold it: scan back until
    // a start no longer reaches.
    let upto = ranges.partition_point(|&(start, _)| start <= at);
    ranges[..upto].iter().rev().any(|&(_, end)| at < end)
}

/// Whether `rest`, which starts at a `&`, is a character reference:
/// `&name;`, `&#digits;` or `&#xhex;`.
fn is_character_reference(rest: &[u8]) -> bool {
    let body = &rest[1..];
    let (valid, tail): (fn(&u8) -> bool, &[u8]) = match body {
        [b'#', b'x' | b'X', tail @ ..] => (|b| b.is_ascii_hexdigit(), tail),
        [b'#', tail @ ..] => (|b| b.is_ascii_digit(), tail),
        [first, ..] if first.is_ascii_alphabetic() => (|b| b.is_ascii_alphanumeric(), body),
        _ => return false,
    };
    let run = tail.iter().take_while(|b| valid(b)).count();
    run > 0 && tail.get(run) == Some(&b';')
}

/// Write a letter over every `&` outside `ranges` that is not a character
/// reference. Returns how many.
fn write_over_ampersands(text: &mut [u8], ranges: &[(usize, usize)]) -> usize {
    let mut written = 0;
    for at in 0..text.len() {
        if text[at] == b'&' && !is_character_reference(&text[at..]) && !shielded(ranges, at) {
            text[at] = FILL;
            written += 1;
        }
    }
    written
}

/// What the walk below knows about where a node sits.
#[derive(Clone, Copy)]
struct Place {
    /// Inside an `element`.
    in_element: bool,
    /// Inside a tag, an attribute or a `{...}`, where a `>` is not text.
    in_code: bool,
}

/// The offsets of the `>` the tree names as text: an ERROR or MISSING node that
/// starts on one, inside an element and outside every tag, attribute and
/// `{...}`.
///
/// `None` is a deadline.
fn greater_thans(root: Node, text: &[u8], deadline: Instant) -> Option<Vec<usize>> {
    let mut found: Vec<usize> = Vec::new();
    let mut cursor = root.walk();
    let mut stack = vec![(
        root,
        Place {
            in_element: false,
            in_code: false,
        },
    )];
    let mut visited = 0u32;
    while let Some((node, place)) = stack.pop() {
        visited += 1;
        if visited >= DEADLINE_CHECK_STRIDE {
            visited = 0;
            if Instant::now() >= deadline {
                return None;
            }
        }
        if !node.has_error() && !node.is_error() && !node.is_missing() {
            continue;
        }
        if (node.is_error() || node.is_missing())
            && place.in_element
            && !place.in_code
            && text.get(node.start_byte()) == Some(&b'>')
        {
            found.push(node.start_byte());
        }
        let kind = node.kind();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        let braced = children
            .iter()
            .any(|child| !child.is_named() && child.kind() == "{");
        let next = Place {
            in_element: place.in_element || kind == "element",
            in_code: place.in_code
                || braced
                || matches!(
                    kind,
                    "start_tag"
                        | "end_tag"
                        | "self_closing_tag"
                        | "attribute"
                        | "script_element"
                        | "style_element"
                ),
        };
        for child in children {
            stack.push((child, next));
        }
    }
    found.sort_unstable();
    found.dedup();
    Some(found)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::model::ParseOutcome;

    fn parse(source: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_svelte_ng::LANGUAGE.into())
            .expect("the svelte grammar loads");
        parser.parse(source, None).expect("a tree")
    }

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    fn written_over(source: &str) -> String {
        let tree = parse(source);
        let ranges = shielded_ranges(tree.root_node(), far()).expect("no deadline");
        let mut text = source.as_bytes().to_vec();
        write_over_ampersands(&mut text, &ranges);
        String::from_utf8(text).expect("ASCII over ASCII")
    }

    #[test]
    fn a_character_reference_is_named_and_a_bare_ampersand_is_not() {
        for named in [
            "&amp;", "&lt;", "&frac12;", "&#38;", "&#x26;", "&#X26;", "&copy; x",
        ] {
            assert!(is_character_reference(named.as_bytes()), "{named}");
        }
        for bare in [
            "&", "& x", "&&", "&amp", "&amp x", "&#;", "&#x;", "&#x2g;", "&1;", "&;", "&#38",
            "&a b;",
        ] {
            assert!(!is_character_reference(bare.as_bytes()), "{bare}");
        }
    }

    #[test]
    fn only_a_bare_ampersand_of_the_template_text_is_written_over() {
        // The bare `&` after `x`, in a comment, and in `&copy ` (no `;`) go;
        // the references, the `&&` inside `{}` and `class={}`, the script and
        // the style stay. The text keeps its length.
        let source = "<script>if (a && b) {}</script>\n\
                      <p>x & y &amp; &#38; &#x26; &copy {a && b}<!-- c & d --></p>\n\
                      <i class={c && d}>z</i>\n\
                      <style>a::after{content:\"&\"}</style>\n";
        let expected = "<script>if (a && b) {}</script>\n\
                        <p>x x y &amp; &#38; &#x26; xcopy {a && b}<!-- c x d --></p>\n\
                        <i class={c && d}>z</i>\n\
                        <style>a::after{content:\"&\"}</style>\n";
        let got = written_over(source);
        assert_eq!(got, expected);
        assert_eq!(got.len(), source.len());
    }

    #[test]
    fn an_ampersand_in_code_survives_when_the_text_has_a_bare_one_too() {
        // The failed tree is the one the shields are read from: it must still
        // hold the script and the mustache beside the error it has.
        let source = "<script>let t = a && b;</script>\n<p>{a && b} and Q & A</p>\n";
        assert_eq!(
            written_over(source),
            "<script>let t = a && b;</script>\n<p>{a && b} and Q x A</p>\n"
        );
    }

    #[test]
    fn a_nested_mustache_is_shielded_whole() {
        let source = "<p>{#if a && b}Q & A{/if} {f({x: a && b})}</p>\n";
        assert_eq!(
            written_over(source),
            "<p>{#if a && b}Q x A{/if} {f({x: a && b})}</p>\n"
        );
    }

    fn extraction(source: &str) -> crate::model::Extraction {
        crate::extract_file_with_budget("src/T.svelte", source, Duration::from_secs(60))
    }

    fn attempts() -> usize {
        ATTEMPTS.with(|count| count.get())
    }

    #[test]
    fn the_repair_is_never_entered_for_a_file_without_an_error() {
        ATTEMPTS.with(|count| count.set(0));
        for source in [
            "<script>function f(){}</script>\n<p>{a && b}</p>\n",
            "<p>x &amp; y</p>\n",
            "",
        ] {
            let e = extraction(source);
            assert!(
                matches!(e.parse_outcome, ParseOutcome::Clean),
                "{source:?}: {:?}",
                e.parse_outcome
            );
        }
        assert_eq!(attempts(), 0);
    }

    #[test]
    fn the_repair_is_entered_once_for_a_svelte_file_with_an_error_and_for_no_other_language() {
        ATTEMPTS.with(|count| count.set(0));
        let _ = extraction("<p>a & b</p>\n");
        assert_eq!(attempts(), 1);
        // The same bytes under other grammars never reach it, error or not.
        for path in ["a.html", "a.ts", "a.rs", "a.kt", "a.vue", "a.astro"] {
            let _ = crate::extract_file_with_budget(
                path,
                "<p>a & b</p> fn ( {",
                Duration::from_secs(60),
            );
        }
        assert_eq!(attempts(), 1);
    }

    fn json(source: &str, repair_on: bool) -> String {
        DISABLED.with(|flag| flag.set(!repair_on));
        let e = extraction(source);
        DISABLED.with(|flag| flag.set(false));
        serde_json::to_string(&e).expect("serializes")
    }

    #[test]
    fn a_file_with_an_error_of_its_own_keeps_its_first_tree() {
        // Nothing here is a bare `&` or `>` of text, so no letter is written,
        // and the output is the output of the code before the repair existed.
        for source in [
            "{#if x}\n<p>hi</p>\n",
            "<div>\n<p>unclosed\n",
            "<script>let a = {</script>\n<p>{#each}</p>\n",
        ] {
            assert_eq!(json(source, true), json(source, false), "{source:?}");
        }
    }

    #[test]
    fn a_repair_that_gains_nothing_is_refused() {
        // The `&` is shielded by the mustache, so no letter is written over
        // anything but `Q & A`; the unclosed block stays an error either way,
        // and what is kept is the tree with strictly less error.
        let source = "{#if x}\n<p>Q & A {a && b}</p>\n";
        let fixed = json(source, true);
        let kept = json(source, false);
        assert!(fixed.contains("Partial"), "{fixed}");
        assert!(kept.contains("Partial"), "{kept}");
        let tree = parse(source);
        let before = error_of(tree.root_node(), far()).expect("no deadline");
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_svelte_ng::LANGUAGE.into())
            .expect("loads");
        if let Some(done) = repair(&mut parser, &tree, source, far()) {
            let after = error_of(done.tree.root_node(), far()).expect("no deadline");
            assert!(after < before, "{after:?} !< {before:?}");
        }
    }

    #[test]
    fn a_deadline_that_has_passed_leaves_the_file_as_it_was() {
        let source = "<p>a & b</p>\n";
        let tree = parse(source);
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_svelte_ng::LANGUAGE.into())
            .expect("loads");
        let passed = Instant::now() - Duration::from_secs(1);
        assert!(repair(&mut parser, &tree, source, passed).is_none());
        // The same call with time left does repair it.
        let done = repair(&mut parser, &tree, source, far()).expect("repaired");
        assert!(!done.tree.root_node().has_error());
        assert_eq!(done.edits, 1);
    }
}
