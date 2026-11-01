//! Svelte template text: the same-width repair for a bare `&` or `>`.
//!
//! tree-sitter-svelte-ng reads HTML text with the HTML rules of its own
//! scanner, and two characters that are legal text in a browser are not text to
//! it: a bare `&` (`Code Age & Half-Life`, `Scan & Clean`), and a `>` that
//! follows text on a line of its own (`for > 6 months`). Each becomes an ERROR
//! node that swallows the rest of the text run, and the file is `Partial` with
//! the whole element charged as a region outside every surviving declaration.
//! Measured on the frozen GitPulse corpus: all eleven partial Svelte files, and
//! only those, fail this way (tree-sitter-svelte-ng 1.0.2, the latest release).
//! The one that fails whole (`src/App.svelte`, a root ERROR over 62,974 bytes)
//! holds three such characters in a 40 KB template.
//!
//! # The rewrite
//!
//! A letter is text wherever a bare `&` or `>` is, so writing one over the
//! character changes nothing the extractors read: the repaired copy is only
//! *parsed*. Its length is the file's, so every byte offset its tree reports is
//! an offset of the original file, and the extractors go on reading the
//! original text through the repaired tree, as they do for Kotlin's `;` rewrite
//! ([`crate::kotlinsemi`]). The copy is dropped as soon as the last parse is
//! done; no node's text is ever taken from it.
//!
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
    let shielded = shielded_ranges(first.root_node(), deadline)?;
    let mut edits = write_over_ampersands(&mut text, &shielded);
    // `None` is the failed tree itself: the text has not changed since it.
    let mut current: Option<Tree> = None;
    let mut dirty = edits > 0;
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
    let tree = current?;
    if error_of(tree.root_node(), deadline)? < error_of(first.root_node(), deadline)? {
        Some(Repaired { tree, edits })
    } else {
        None
    }
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
        for named in ["&amp;", "&lt;", "&frac12;", "&#38;", "&#x26;", "&#X26;", "&copy; x"] {
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
