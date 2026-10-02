//! Incremental reparsing.
//!
//! # What was tried first, and why it is not here
//!
//! The obvious design is to cut the document at block boundaries, reparse the
//! slice, and splice the result back. It was built, and the property test in
//! `tests/incremental.rs` took it apart in four stages:
//!
//! 1. An edit that opens a code fence swallows the rest of the file.
//! 2. ```` ```# Heading ```` is an *opening* fence with an info string, not a
//!    closing one, so naive fence counting called an unbalanced slice
//!    balanced.
//! 3. A list continues lazily into the paragraph below it, growing past the
//!    boundary the slice was cut on.
//! 4. Even a *correctly bounded* top-level slice reparses its **nested**
//!    blocks differently in isolation — a list item's extent depends on what
//!    follows the list.
//!
//! Each fix revealed the next. The lesson is that a Markdown block's meaning
//! is not determined by its own text, so "reparse this block" has no
//! well-defined answer.
//!
//! # What is here instead
//!
//! A fast path that changes no structure at all, and therefore needs no
//! reparse — only arithmetic. It applies when the edit provably cannot alter
//! anything except offsets:
//!
//! - the inserted and removed text are **inert** (see [`is_inert`]): plain
//!   words, no newlines, nothing that opens a block or an inline construct;
//! - the edit lands inside a **plain-prose** top-level block — one with no
//!   inline-significant characters anywhere in it, so no emphasis, link, or
//!   code span can be created or destroyed by moving text around;
//! - the edit is clear of the line's first four columns, where block markers
//!   live.
//!
//! Under those conditions the correct new parse *is* the old parse with
//! offsets shifted, which is why this path can be trusted. Everything else
//! takes a full reparse — 2.55ms for 10,000 lines in release, which is
//! already inside a 60fps frame.

use std::ops::Range;

use super::model::{ParseResult, MAX_DOCUMENT_BYTES};
use super::parse::{parse_checked, ParseError};

/// What a [`Document::replace`] actually did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reparse {
    /// The edit was invalid or would exceed a parser/resource bound. The text
    /// and the previous parse are unchanged.
    Rejected,
    /// The whole document was reparsed.
    Full,
    /// No reparse was needed; offsets were shifted. Carries the byte range of
    /// the edit, so the caller knows what to restyle.
    Shifted(Range<usize>),
}

/// A Markdown document that avoids reparsing when it provably can.
#[derive(Debug, Clone)]
pub struct Document {
    text: String,
    result: ParseResult,
}

impl Document {
    /// Creates a known-bounded document, parsing it in full.
    ///
    /// Untrusted input must use [`Document::try_from_str`]. This compatibility
    /// entry point panics on rejection rather than manufacturing an empty
    /// document that callers could mistake for a successful parse.
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let result = parse_checked(&text).expect("Markdown source exceeds the parser contract");
        Self { text, result }
    }

    /// Creates a document from untrusted text without allocating its owned
    /// copy until the parser has accepted the complete input.
    pub fn try_from_str(text: &str) -> Result<Self, ParseError> {
        let result = parse_checked(text)?;
        Ok(Self {
            text: text.to_owned(),
            result,
        })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn result(&self) -> &ParseResult {
        &self.result
    }

    /// UTF-16 length of the text, so a caller mirroring this document (an
    /// `NSTextStorage`, say) can detect that the two have drifted apart and
    /// resynchronise rather than apply edits at wrong offsets.
    pub fn len_utf16(&self) -> u32 {
        count_utf16(&self.text)
    }

    /// Converts a UTF-16 offset into a byte offset.
    ///
    /// Callers on the Swift side think in UTF-16 because `NSTextStorage`
    /// does; this crate slices `&str`, which is bytes. Offsets past the end
    /// clamp, so a stale offset truncates rather than panicking.
    pub fn byte_offset(&self, utf16: u32) -> usize {
        if self.text.is_ascii() {
            return (utf16 as usize).min(self.text.len());
        }
        let mut seen = 0u32;
        for (byte, ch) in self.text.char_indices() {
            if seen >= utf16 {
                return byte;
            }
            seen += ch.len_utf16() as u32;
        }
        self.text.len()
    }

    /// Replaces the bytes in `range` with `replacement`.
    ///
    /// Takes the shift-only fast path when the edit cannot change structure;
    /// otherwise reparses in full.
    pub fn replace(&mut self, range: Range<usize>, replacement: &str) -> Reparse {
        let valid = range.start <= range.end
            && range.end <= self.text.len()
            && self.text.is_char_boundary(range.start)
            && self.text.is_char_boundary(range.end);
        if !valid {
            // Refuse to corrupt the buffer: leave it untouched rather than
            // silently mangling it.
            return Reparse::Rejected;
        }

        let removed_bytes = range.end - range.start;
        let Some(new_byte_count) = self
            .text
            .len()
            .checked_sub(removed_bytes)
            .and_then(|count| count.checked_add(replacement.len()))
        else {
            return Reparse::Rejected;
        };
        if new_byte_count > MAX_DOCUMENT_BYTES {
            return Reparse::Rejected;
        }

        let shiftable = self.is_shiftable(&range, replacement);

        // Offsets must be captured against the old text, before it changes.
        let edit_start_u16 = count_utf16(&self.text[..range.start]);
        let removed_u16 = count_utf16(&self.text[range.clone()]);

        if !shiftable {
            // Parse a prospective copy first. Any event, nesting, record, or
            // string-table rejection leaves both the old text and old model
            // intact instead of committing text with a stale parse.
            let mut candidate = self.text.clone();
            candidate.replace_range(range, replacement);
            let Ok(parsed) = parse_checked(&candidate) else {
                return Reparse::Rejected;
            };
            self.text = candidate;
            self.result = parsed;
            return Reparse::Full;
        }

        self.text.replace_range(range.clone(), replacement);

        let added_u16 = count_utf16(replacement);
        let delta_u16 = added_u16 as i64 - removed_u16 as i64;
        let delta_bytes = replacement.len() as i64 - (range.end - range.start) as i64;

        self.shift(
            edit_start_u16,
            removed_u16,
            delta_u16,
            range.start,
            delta_bytes,
        );

        let end = (range.start as i64 + replacement.len() as i64) as usize;
        Reparse::Shifted(range.start..end)
    }

    /// Whether the edit can be applied by shifting offsets alone.
    fn is_shiftable(&self, edit: &Range<usize>, replacement: &str) -> bool {
        if self.result.top_level.is_empty() {
            return false;
        }
        let Some(removed) = self.text.get(edit.clone()) else {
            return false;
        };
        if !is_inert(replacement) || !is_inert(removed) {
            return false;
        }
        if creates_trailing_whitespace(&self.text, edit, replacement) {
            return false;
        }
        if !clear_of_line_start(&self.text, edit.start) {
            return false;
        }

        // The edit must sit strictly inside one plain-prose top-level block.
        let Some(block) = self
            .result
            .top_level
            .iter()
            .find(|range| range.start < edit.start && range.end > edit.end)
        else {
            return false;
        };
        let Some(text) = self.text.get(block.clone()) else {
            return false;
        };
        if !is_plain_prose(text) {
            return false;
        }

        // The edit must not touch a marker, even at its edge.
        //
        // A block's trailing whitespace is itself a marker — the gap between
        // the last text child and the block's end. Typing a space next to it
        // *extends* that marker, so the parse genuinely changes and shifting
        // would be wrong. Requiring a one-unit gap on each side rules that
        // out without needing to reason about which markers are adjacent.
        let start_u16 = count_utf16(&self.text[..edit.start]);
        let end_u16 = start_u16 + count_utf16(removed);
        let low = start_u16.saturating_sub(1);
        let high = end_u16.saturating_add(1);

        let touches = |s: u32, e: u32| s < high && e > low;
        if self.result.markers.iter().any(|m| touches(m.start, m.end)) {
            return false;
        }
        if self.result.spans.iter().any(|s| touches(s.start, s.end)) {
            return false;
        }
        true
    }

    /// Shifts every recorded offset past the edit.
    ///
    /// Ranges are one of three kinds: entirely before the edit (untouched),
    /// entirely after it (moved by the delta), or containing it (only the end
    /// moves). Nothing can partially overlap, because a plain-prose block has
    /// no spans or markers inside it to cut across.
    fn shift(
        &mut self,
        edit_u16: u32,
        removed_u16: u32,
        delta_u16: i64,
        edit_byte: usize,
        delta_bytes: i64,
    ) {
        let removed_end = edit_u16 + removed_u16;

        let shift_pair = |start: &mut u32, end: &mut u32| {
            if *end <= edit_u16 {
                return;
            }
            if *start >= removed_end {
                *start = shift_u32(*start, delta_u16);
                *end = shift_u32(*end, delta_u16);
            } else {
                *end = shift_u32(*end, delta_u16);
            }
        };

        for block in &mut self.result.blocks {
            shift_pair(&mut block.start, &mut block.end);
        }
        for span in &mut self.result.spans {
            shift_pair(&mut span.start, &mut span.end);
        }
        for marker in &mut self.result.markers {
            shift_pair(&mut marker.start, &mut marker.end);
        }

        for range in &mut self.result.top_level {
            if range.end <= edit_byte {
                continue;
            }
            if range.start >= edit_byte {
                range.start = shift_usize(range.start, delta_bytes);
            }
            range.end = shift_usize(range.end, delta_bytes);
        }
    }
}

fn count_utf16(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

fn shift_u32(value: u32, shift: i64) -> u32 {
    (value as i64 + shift).max(0) as u32
}

fn shift_usize(value: usize, shift: i64) -> usize {
    (value as i64 + shift).max(0) as usize
}

/// Whether `text` can be inserted or removed without changing structure.
///
/// Excluded on purpose: `\n`, which creates or destroys lines, and every
/// character that can open a block or an inline construct. An empty string is
/// inert, so a pure deletion of inert text still qualifies.
fn is_inert(text: &str) -> bool {
    text.chars().all(is_inert_char)
}

/// `.` is allowed even though `1.` opens an ordered list: every edit on this
/// path is already clear of the first four columns, so no line start — and so
/// no block marker — can be formed. Excluding it would reject ordinary prose
/// and leave the fast path never firing.
fn is_inert_char(c: char) -> bool {
    c.is_alphanumeric()
        || matches!(
            c,
            ' ' | '.' | ',' | ';' | ':' | '\'' | '"' | '?' | '!' | '(' | ')'
        )
}

/// Whether `offset` is clear of the first four columns of its line.
///
/// Every CommonMark block marker sits within three spaces of indentation plus
/// the marker character, so an edit past that point cannot introduce one.
fn clear_of_line_start(text: &str, offset: usize) -> bool {
    let line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    offset.saturating_sub(line_start) > 4
}

/// Whether the edit makes whitespace the last content before a line or the
/// end of the document.
///
/// CommonMark exposes trailing whitespace as a syntax marker. Shifting can
/// move existing markers but cannot create one, so a replacement such as
/// `"word "` that ends immediately before `\n` must reparse even though all
/// of its characters are otherwise inert.
fn creates_trailing_whitespace(text: &str, edit: &Range<usize>, replacement: &str) -> bool {
    let next = text[edit.end..].chars().next();
    if !matches!(next, None | Some('\n' | '\r')) {
        return false;
    }
    replacement
        .chars()
        .next_back()
        .or_else(|| text[..edit.start].chars().next_back())
        .is_some_and(|c| matches!(c, ' ' | '\t'))
}

/// Whether a block is plain prose — no character that could take part in an
/// inline construct.
///
/// This is what makes shifting safe rather than merely likely. Deleting a
/// space is inert, but in `a *b * c` it would produce `a *b* c` and create
/// emphasis out of nothing. Requiring the whole block to be free of inline
/// delimiters removes that possibility instead of trying to detect it.
///
/// Leading whitespace on any line is also rejected: four spaces make an
/// indented code block, and lazy continuations make indentation meaningful.
fn is_plain_prose(text: &str) -> bool {
    text.lines().all(|line| {
        !line.starts_with(' ') && !line.starts_with('\t') && line.chars().all(is_inert_char)
    })
}
