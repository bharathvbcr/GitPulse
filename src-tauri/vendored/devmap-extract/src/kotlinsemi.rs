//! Kotlin one-line class bodies (extract-v98 row 16): the same-length `;` rewrite.
//!
//! tree-sitter-kotlin-ng reads a class, interface or object body whose last
//! member shares a line with the closing brace as a whole-file ERROR:
//! `class A { val x = 1 }`, `interface Cb { fun call() }`,
//! `val o = object { val x = 1 }`, `class C { companion object { fun f() = 1 } }`.
//! The grammar's separator between members is a zero-width token it accepts once
//! per line; the same shapes on several lines, or with `;` before the `}`, parse
//! cleanly (measured on tree-sitter-kotlin-ng 1.1.0, not read from its source).
//! The ERROR swallows the class and every declaration after it, and a member
//! inside it keeps the file as its owner (`K.kt::f` for `C.f`).
//!
//! # The rewrite
//!
//! A `;` is a legal separator everywhere Kotlin has one, so putting one where the
//! grammar wants it changes no meaning. The rewrite replaces **one existing
//! space or tab** with `;`, so the text keeps its length and every byte offset
//! the tree reports is an offset of the original file. The extractor then reads
//! the original text through the rewritten tree: a node's text differs from
//! the file's only where a `;` replaced a space, outside every name.
//!
//! Which spaces: the one directly before a closing `}` that has code before it
//! on its line and that closes a class-like body (the `}` is a child of a
//! `class_body`, an `enum_class_body` or an ERROR). A `}` that closes a block
//! or a lambda needs no separator and is left alone. The tokens come from the
//! tree of the failed parse, which still holds them, outside strings and
//! comments. They are not all visible to the
//! first parse (a nested `}` hides inside the ERROR until the outer one is
//! fixed), so the rewrite repeats, each pass over the top-level statements that
//! still hold an error, at most [`MAX_PASSES`] times and with at most
//! [`MAX_EDITS`] separators (every pass that goes on writes at least one no
//! earlier pass wrote). Sibling one-liners are revealed in growing batches, so
//! the pass limit is far above any plain file; a chain of nested one-liners
//! reveals one level per pass and is what the limit stops. The file deadline
//! bounds each parse and the scan between them.
//!
//! A file the first mode leaves with an error is tried again from the original
//! with a second mode that also writes a separator after a `}` followed on its
//! line by a declaration: `class A { .. } class B { .. }` wants one between the
//! two classes, and the grammar reads the pair as an error until it has it.
//!
//! The result is accepted **only if the re-parse is fully clean**. Anything
//! else, including a deadline, leaves the file exactly as it was: `Partial`.
//!
//! # No room for a separator
//!
//! `class A { fun f() {}}` has no space before the last `}`. Inserting a `;`
//! would shift every later offset. The rewrite does not map offsets back; it
//! leaves such a file unrecovered, and the file stays `Partial`.

use std::time::Instant;

use tree_sitter::{Node, Parser, Tree};

use crate::model::TextRange;
use crate::treesitter::{bounded_parent, parse_until, walk_deadline_passed, DEADLINE_CHECK_STRIDE};

/// Separators written per file. A file that wants more is not rewritten.
pub(crate) const MAX_EDITS: usize = 512;

/// Parses after the first one. Measured: sibling one-liners are revealed in
/// growing batches (7 need 8 parses, 40 and 200 need 11), so this is far above
/// any plain file, while a chain of nested one-liners reveals one level per
/// parse and 512 of them (7.6 KB) took about 6 s, past the file budget. No
/// real file nests 32 one-line bodies.
pub(crate) const MAX_PASSES: usize = 32;

/// What a declaration may start with after a `}` on the same line. A `;` is
/// wanted between `class A { .. } class B { .. }`, and only before one of
/// these (never before `else`, `catch`, `as`, `by`, `.`, an infix name...).
///
/// Accepted residual risk: a user-defined infix function named like one of
/// these (`infix fun init`, `companion`, `data`...) written after a `}` in a
/// file that has an unrelated error can re-parse clean as a block of its own.
/// It is rare, the re-parse must be fully clean, and it is accepted.
const DECLARATION_STARTS: [&str; 32] = [
    "class",
    "object",
    "interface",
    "fun",
    "val",
    "var",
    "typealias",
    "enum",
    "sealed",
    "data",
    "open",
    "abstract",
    "private",
    "public",
    "internal",
    "protected",
    "inner",
    "annotation",
    "companion",
    "const",
    "lateinit",
    "override",
    "final",
    "inline",
    "operator",
    "infix",
    "suspend",
    "tailrec",
    "external",
    "expect",
    "actual",
    "init",
];

/// What the rewrite did.
pub(crate) enum Repair {
    /// The re-parse is clean. `tree` is its tree, on the same offsets as the
    /// file; `ranges` are the top-level statements a `;` was written into, and
    /// `edits` the number of separators written.
    Repaired {
        tree: Tree,
        ranges: Vec<TextRange>,
        edits: usize,
        /// The second mode was needed: a separator was written between two
        /// declarations that share a line.
        boundaries: bool,
    },
    /// Nothing changed: the original tree is handed back, with the reason.
    Unchanged { tree: Tree, why: String },
}

/// Why a rewrite stopped without a clean tree.
enum Stop {
    /// No candidate was left, or the passes ran out: the other mode may still
    /// succeed.
    Retry(String),
    /// A cap, a deadline or a bad rewrite: nothing else is tried.
    Final(String),
}

/// Try to make `tree`, which has an error, parse cleanly by writing `;` over
/// spaces. `source` is the text `tree` was parsed from.
///
/// The first attempt writes a separator before each closing brace that follows
/// code on its line. A file that still has an error is tried again from the
/// original with the second mode, which also writes one after a `}` that is
/// followed on its line by a declaration (`class A { .. } class B { .. }`, a
/// separator the grammar wants between two declarations on one line). The
/// second mode runs only where the first has already failed, so it cannot
/// change a file the first repairs.
pub(crate) fn repair(parser: &mut Parser, tree: Tree, source: &str, deadline: Instant) -> Repair {
    let first = match rewrite(parser, &tree, source, deadline, false) {
        Ok(done) => return done,
        Err(stop) => stop,
    };
    let why = match first {
        Stop::Final(why) => why,
        Stop::Retry(why) => match rewrite(parser, &tree, source, deadline, true) {
            Ok(done) => return done,
            Err(Stop::Final(later)) => later,
            Err(Stop::Retry(_)) => why,
        },
    };
    Repair::Unchanged { tree, why }
}

/// One mode of the rewrite. `Ok` is a fully clean re-parse.
fn rewrite(
    parser: &mut Parser,
    tree: &Tree,
    source: &str,
    deadline: Instant,
    boundaries: bool,
) -> Result<Repair, Stop> {
    let mut text = source.as_bytes().to_vec();
    let mut edited: Vec<usize> = Vec::new();
    let mut current = tree.clone();
    for pass in 0..=MAX_PASSES {
        if !current.root_node().has_error() {
            let ranges = touched(&current, &edited);
            return Ok(Repair::Repaired {
                tree: current,
                ranges,
                edits: edited.len(),
                boundaries,
            });
        }
        if pass == MAX_PASSES {
            break;
        }
        let fresh =
            candidates(current.root_node(), &text, &edited, boundaries).map_err(Stop::Final)?;
        if fresh.is_empty() {
            return Err(Stop::Retry(
                "no space before a closing brace is left to write a separator over".into(),
            ));
        }
        if edited.len() + fresh.len() > MAX_EDITS {
            return Err(Stop::Final(format!(
                "more than {MAX_EDITS} separators would be written"
            )));
        }
        for at in fresh {
            text[at] = b';';
            edited.push(at);
        }
        // Only an ASCII space or tab was replaced by an ASCII byte.
        let Ok(rewritten) = std::str::from_utf8(&text) else {
            return Err(Stop::Final("the rewritten text is not UTF-8".into()));
        };
        match parse_until(parser, rewritten, deadline) {
            (Some(next), false) => current = next,
            _ => {
                return Err(Stop::Final(
                    "the re-parse did not finish within the budget".into(),
                ))
            }
        }
    }
    Err(Stop::Retry(format!(
        "still an error after {MAX_PASSES} rewrites"
    )))
}

/// The last character of `before`, which is UTF-8 text cut at a character
/// boundary: the shortest suffix of at most four bytes that decodes.
fn last_char(before: &[u8]) -> Option<char> {
    let tail = &before[before.len().saturating_sub(4)..];
    (0..tail.len()).rev().find_map(|from| {
        std::str::from_utf8(&tail[from..])
            .ok()
            .and_then(|text| text.chars().next_back())
    })
}

/// Whether the code `before` ends a member, so that a `;` is wanted after it.
/// An arrow, an operator or an opening bracket before `}` is not a member. A
/// letter or digit of any script ends one (`café`), and so does a postfix
/// `++` or `--` that follows an operand (`x++`, `a[0]--`).
fn ends_a_member(before: &[u8]) -> bool {
    let Some(last) = last_char(before) else {
        return false;
    };
    match last {
        '>' => before.len() < 2 || before[before.len() - 2] != b'-',
        '+' | '-' => {
            let operator = last as u8;
            let stem = &before[..before.len() - 1];
            stem.last() == Some(&operator)
                && last_char(&stem[..stem.len() - 1]).is_some_and(|operand| {
                    operand.is_alphanumeric() || matches!(operand, '_' | ')' | ']')
                })
        }
        other => {
            other.is_alphanumeric()
                || matches!(other, '_' | ')' | ']' | '}' | '"' | '\'' | '`' | '!' | '?')
        }
    }
}

/// The offset of the space to write `;` over after the `}` at `at`, when the
/// brace is followed on its line by a space and then a declaration.
fn boundary_after(text: &[u8], at: usize) -> Option<usize> {
    let mut next = at + 1;
    while matches!(text.get(next), Some(b' ' | b'\t')) {
        next += 1;
    }
    if next == at + 1 {
        return None;
    }
    let rest = text.get(next..)?;
    let word = rest.iter().take_while(|b| b.is_ascii_alphabetic()).count();
    let starts_a_declaration = rest.first() == Some(&b'@')
        || std::str::from_utf8(&rest[..word]).is_ok_and(|word| DECLARATION_STARTS.contains(&word))
            && !rest
                .get(word)
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
    starts_a_declaration.then_some(at + 1)
}

/// The offsets of the spaces to write `;` over: one directly before each
/// closing `}` that follows code on its line, in the top-level statements that
/// hold an error, outside strings and comments.
fn candidates(
    root: Node,
    text: &[u8],
    done: &[usize],
    boundaries: bool,
) -> Result<Vec<usize>, String> {
    let mut found: Vec<usize> = Vec::new();
    let mut cursor = root.walk();
    // Each node waits with the end of the last sibling before it that is not a
    // comment, read while its parent's children are pushed, so no ancestor or
    // sibling lookup (the unbounded native parent walk) is made for it.
    let mut stack: Vec<(Node, Option<usize>)> = Vec::new();
    if root.is_error() {
        // A tree whose root is itself the ERROR node (no `source_file` around
        // it) has the file's tokens as its direct children, and none of them
        // holds an error of its own, so the walk starts at the root.
        stack.push((root, None));
    } else {
        let mut top = root.walk();
        for statement in root.children(&mut top) {
            if statement.is_error() || statement.has_error() {
                stack.push((statement, None));
            }
        }
        // No statement holds the error: it is on the root itself, a hidden
        // token between two declarations (`class A { .. } class B { .. }` with
        // each body over several lines), so the scan covers the whole file.
        if stack.is_empty() {
            stack.push((root, None));
        }
    }
    let mut visited = 0u32;
    while let Some((node, code_end)) = stack.pop() {
        visited += 1;
        if visited >= DEADLINE_CHECK_STRIDE {
            visited = 0;
            if walk_deadline_passed() {
                return Err("the scan for separators ran past the deadline".into());
            }
        }
        let kind = node.kind();
        if kind.contains("string") || kind.contains("template") || kind.ends_with("comment") {
            continue;
        }
        if !node.is_named() && kind == "}" {
            let at = node.start_byte();
            let closes_a_body = bounded_parent(node).is_some_and(|parent| {
                parent.is_error() || matches!(parent.kind(), "class_body" | "enum_class_body")
            });
            if closes_a_body && at > 0 && matches!(text.get(at - 1), Some(b' ' | b'\t')) {
                // The member ends at the last node that is not a comment, and
                // the brace shares its line: a brace that starts its line, or
                // follows a comment on a line of its own, is already apart.
                let mut end = code_end.map_or(0, |end| end.min(at));
                while end > 0 && matches!(text[end - 1], b' ' | b'\t') {
                    end -= 1;
                }
                if end > 0
                    && !text[end..at].iter().any(|b| matches!(b, b'\n' | b'\r'))
                    && ends_a_member(&text[..end])
                    && !done.contains(&(at - 1))
                {
                    found.push(at - 1);
                }
            }
            if boundaries && closes_a_body {
                if let Some(site) = boundary_after(text, at) {
                    if !done.contains(&site) {
                        found.push(site);
                    }
                }
            }
            continue;
        }
        cursor.reset(node);
        if cursor.goto_first_child() {
            let mut code_end: Option<usize> = None;
            loop {
                let child = cursor.node();
                stack.push((child, code_end));
                if !child.kind().ends_with("comment") {
                    code_end = Some(child.end_byte());
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }
    found.sort_unstable();
    found.dedup();
    Ok(found)
}

/// The top-level statements of the repaired tree a separator was written into.
///
/// A separator written inside a statement lands on a node of it. One written
/// between two statements (the second mode's, after a `}`) becomes the
/// grammar's hidden separator token, which no node shows, so the lookup lands
/// on the root: the statement that ends at the separator and the one that
/// starts after it are the ones it joined, and both are recorded.
fn touched(tree: &Tree, edits: &[usize]) -> Vec<TextRange> {
    let root = tree.root_node();
    let mut ranges: Vec<TextRange> = Vec::new();
    let mut top: Option<Vec<TextRange>> = None;
    for &at in edits {
        let Some(mut node) = root.descendant_for_byte_range(at, at + 1) else {
            continue;
        };
        while let Some(parent) = bounded_parent(node) {
            if parent.id() == root.id() {
                break;
            }
            node = parent;
        }
        if node.id() != root.id() {
            ranges.push(TextRange {
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
            });
            continue;
        }
        let statements = top.get_or_insert_with(|| {
            let mut cursor = root.walk();
            root.children(&mut cursor)
                .map(|child| TextRange {
                    start_byte: child.start_byte(),
                    end_byte: child.end_byte(),
                })
                .collect()
        });
        let after = statements.partition_point(|range| range.end_byte <= at);
        if let Some(before) = after.checked_sub(1) {
            ranges.push(statements[before].clone());
        }
        if let Some(next) = statements.get(after) {
            if next.start_byte > at {
                ranges.push(next.clone());
            }
        }
    }
    ranges.sort();
    ranges.dedup();
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_character_is_decoded_not_taken_as_a_byte() {
        assert_eq!(last_char("val x = café".as_bytes()), Some('é'));
        assert_eq!(last_char("val x = 日本".as_bytes()), Some('本'));
        assert_eq!(last_char("val x = 😀".as_bytes()), Some('😀'));
        assert_eq!(last_char(b"x"), Some('x'));
        assert_eq!(last_char(b""), None);
    }

    #[test]
    fn a_member_ends_in_a_letter_of_any_script_a_postfix_or_a_closer() {
        for member in [
            "val x = 1",
            "val x = café",
            "val x = 日本",
            "val x = x++",
            "val x = a[0]--",
            "val x = f()++",
            "val x = y!!",
            "val x: Int?",
            "val x = \"s\"",
            "fun f() {}",
            "val f: () -> List<Int>",
        ] {
            assert!(ends_a_member(member.as_bytes()), "{member}");
        }
    }

    #[test]
    fn an_operator_an_arrow_or_an_opener_does_not_end_a_member() {
        for dangling in [
            "",
            "val x =",
            "val x = a +",
            "val x = a -",
            "val x = a + +",
            "val x = ++",
            "val f: () ->",
            "val f = { x ->",
            "class B(",
            "class B :",
            "val x = a.",
            "val x = [",
            "val x = 1,",
            "val x = 1;",
            "@",
        ] {
            assert!(!ends_a_member(dangling.as_bytes()), "{dangling:?}");
        }
    }
}
