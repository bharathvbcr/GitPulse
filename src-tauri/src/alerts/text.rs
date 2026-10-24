//! The one place text from a program or a peer is made fit to show.
//!
//! What reaches the notification path is attacker-adjacent by construction:
//! any program the user runs can write an OSC body, and any process on the
//! account can send a hook report. A banner is rendered by the OS, so the
//! characters that matter are the ones that change what a reader sees without
//! being visible themselves: controls (`Cc`), the format characters (`Cf`:
//! bidirectional overrides and isolates, zero-width characters, the byte-order
//! mark, the tag block) and private-use code points, which render as whatever
//! a font maps them to.
//!
//! Rust's standard library has no `Cf` or `Co` predicate, so [`is_hidden`] is
//! the list. [`sanitize`] (collapse to one bounded line, for banners and
//! announcements) and [`printable`] (remove, for a token shown in a hook's own
//! error) both read it; nothing else in the crate keeps a second copy. The
//! renderer's `clean` in `sessionActivity.ts` mirrors [`sanitize`], and
//! `scripts/fixtures/sanitize-cases.json` is the table both sides are tested
//! against, row by row.

/// Longest text handed to an OS banner, in characters.
pub(super) const MAX_TEXT_CHARS: usize = 240;

/// Whether `c` is a format character (`Cf`) or private use (`Co`): invisible
/// or reordering, and never text a reader asked to see.
///
/// Dropped without leaving a space, because a zero-width character between two
/// letters is not a word break. The ranges are Unicode's `Cf` set, widened to
/// the whole of the blocks that hold it where the rest of a block is unassigned
/// (`U+2060..=206F`, the tag block).
pub fn is_hidden(c: char) -> bool {
    matches!(
        c,
        '\u{ad}'
            | '\u{600}'..='\u{605}'
            | '\u{61c}'
            | '\u{6dd}'
            | '\u{70f}'
            | '\u{890}'..='\u{891}'
            | '\u{8e2}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{110bd}'
            | '\u{110cd}'
            | '\u{13430}'..='\u{1343f}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0000}'..='\u{e007f}'
            // Private use: the basic plane's block and both supplementary planes.
            | '\u{e000}'..='\u{f8ff}'
            | '\u{f0000}'..='\u{ffffd}'
            | '\u{100000}'..='\u{10fffd}'
    )
}

/// Whether `c` separates text: a control character (a newline starts a new
/// line, an escape starts a sequence), whitespace of any kind, or the line and
/// paragraph separators.
fn is_separator(c: char) -> bool {
    c.is_control() || c.is_whitespace() || c == '\u{2028}' || c == '\u{2029}'
}

/// Collapses `text` to one bounded line a banner or the renderer can show:
/// hidden characters dropped, separators turned into single spaces, the whole
/// cut at [`MAX_TEXT_CHARS`] with an ellipsis.
pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(512));
    let mut chars = 0usize;
    let mut space = false;
    for ch in text.chars() {
        if is_hidden(ch) {
            continue;
        }
        if is_separator(ch) {
            space = true;
            continue;
        }
        if space && chars > 0 {
            out.push(' ');
            chars += 1;
        }
        space = false;
        if chars >= MAX_TEXT_CHARS {
            out.push('…');
            break;
        }
        out.push(ch);
        chars += 1;
    }
    out
}

/// `text` with every hidden character and every separator removed, cut to
/// `max` characters: for a token printed inside a sentence of ours, where a
/// space would be as foreign as a newline.
pub fn printable(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !is_hidden(*c) && !is_separator(*c))
        .take(max)
        .collect()
}
