//! Syntax highlighting for fenced code blocks.
//!
//! Real tree-sitter grammars rather than a regex tokenizer. Raw strings,
//! nested template literals, and regex-vs-division ambiguity are exactly the
//! cases a pattern-based highlighter gets wrong, and they show up constantly
//! in the code people paste into notes.
//!
//! Offsets are UTF-16 and relative to the code block's own text, matching the
//! rest of the FFI so Swift never converts anything.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

/// Maximum UTF-8 bytes admitted to one tree-sitter highlight pass.
pub const MAX_HIGHLIGHT_CODE_BYTES: usize = 1024 * 1024;
/// Maximum UTF-8 bytes in a language identifier before trimming/lowercasing.
pub const MAX_HIGHLIGHT_LANGUAGE_BYTES: usize = 64;
/// Maximum materialized highlight spans per code block.
pub const MAX_HIGHLIGHT_SPANS: usize = 250_000;
/// Maximum events consumed from tree-sitter for one code block.
pub const MAX_HIGHLIGHT_EVENTS: usize = 2_000_000;
/// Maximum nested highlight captures.
pub const MAX_HIGHLIGHT_NESTING: usize = 256;
const UTF16_CHECKPOINT_BYTES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightError {
    CodeTooLarge,
    LanguageTooLong,
    InteriorNul,
    TooManyEvents,
    TooDeep,
    TooManySpans,
    Grammar,
}

/// A highlighted token class.
///
/// Deliberately small: a palette with forty distinct colours is noise. These
/// are the distinctions a reader actually uses to scan code.
///
/// Discriminants are part of the FFI contract — append, never renumber.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HighlightKind {
    Keyword = 0,
    String = 1,
    Number = 2,
    Comment = 3,
    Function = 4,
    Type = 5,
    Constant = 6,
    Variable = 7,
    Operator = 8,
    Punctuation = 9,
    Attribute = 10,
}

/// A highlighted range within a code block.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HighlightSpan {
    /// UTF-16 offset from the start of the code, not the document.
    pub start: u32,
    pub end: u32,
    pub kind: u16,
    pub _padding: u16,
}

/// Capture names requested from each grammar, in priority order.
///
/// tree-sitter resolves a capture to the *first* matching entry, so more
/// specific names must come before their prefixes — `function.builtin` before
/// `function`, or every builtin would resolve as a plain function.
const CAPTURES: &[(&str, HighlightKind)] = &[
    ("attribute", HighlightKind::Attribute),
    ("comment.documentation", HighlightKind::Comment),
    ("comment", HighlightKind::Comment),
    ("constant.builtin", HighlightKind::Constant),
    ("constant", HighlightKind::Constant),
    ("constructor", HighlightKind::Type),
    ("escape", HighlightKind::String),
    ("function.builtin", HighlightKind::Function),
    ("function.method", HighlightKind::Function),
    ("function", HighlightKind::Function),
    ("keyword", HighlightKind::Keyword),
    ("label", HighlightKind::Constant),
    ("number", HighlightKind::Number),
    ("operator", HighlightKind::Operator),
    ("property", HighlightKind::Variable),
    ("punctuation.bracket", HighlightKind::Punctuation),
    ("punctuation.delimiter", HighlightKind::Punctuation),
    ("punctuation.special", HighlightKind::Punctuation),
    ("string.special", HighlightKind::String),
    ("string", HighlightKind::String),
    ("tag", HighlightKind::Type),
    ("type.builtin", HighlightKind::Type),
    ("type", HighlightKind::Type),
    ("variable.builtin", HighlightKind::Constant),
    ("variable.parameter", HighlightKind::Variable),
    ("variable", HighlightKind::Variable),
];

fn capture_names() -> Vec<String> {
    CAPTURES.iter().map(|(name, _)| name.to_string()).collect()
}

/// Builds the per-language configurations once.
///
/// Grammar loading parses a query file, which is slow enough that doing it
/// per code block would be visible while scrolling a document full of code.
fn configurations() -> &'static HashMap<&'static str, HighlightConfiguration> {
    static CONFIGS: OnceLock<HashMap<&'static str, HighlightConfiguration>> = OnceLock::new();
    CONFIGS.get_or_init(|| {
        let names = capture_names();
        let mut map = HashMap::new();

        let mut add = |keys: &[&'static str],
                       language: tree_sitter::Language,
                       highlights: &str,
                       injections: &str,
                       locals: &str| {
            // Configurations are neither cloneable nor shareable, so each
            // alias builds its own from a fresh clone of the language.
            for key in keys {
                let Ok(mut config) = HighlightConfiguration::new(
                    language.clone(),
                    *key,
                    highlights,
                    injections,
                    locals,
                ) else {
                    // A grammar that fails to configure simply offers no
                    // highlighting; the block still renders as plain text.
                    continue;
                };
                config.configure(&names);
                map.insert(*key, config);
            }
        };

        add(
            &["rust", "rs"],
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::HIGHLIGHTS_QUERY,
            tree_sitter_rust::INJECTIONS_QUERY,
            "",
        );
        add(
            &["swift"],
            tree_sitter_swift::LANGUAGE.into(),
            tree_sitter_swift::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_swift::LOCALS_QUERY,
        );
        add(
            &["javascript", "js", "jsx", "typescript", "ts", "tsx"],
            tree_sitter_javascript::LANGUAGE.into(),
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::INJECTIONS_QUERY,
            tree_sitter_javascript::LOCALS_QUERY,
        );
        add(
            &["python", "py"],
            tree_sitter_python::LANGUAGE.into(),
            tree_sitter_python::HIGHLIGHTS_QUERY,
            "",
            "",
        );
        add(
            &["json", "jsonc"],
            tree_sitter_json::LANGUAGE.into(),
            tree_sitter_json::HIGHLIGHTS_QUERY,
            "",
            "",
        );
        add(
            &["bash", "sh", "shell", "zsh"],
            tree_sitter_bash::LANGUAGE.into(),
            tree_sitter_bash::HIGHLIGHT_QUERY,
            "",
            "",
        );

        map
    })
}

/// Whether a language has a grammar available.
pub fn supports(language: &str) -> bool {
    supports_checked(language).unwrap_or(false)
}

/// Bounded language lookup used by untrusted-input boundaries.
pub fn supports_checked(language: &str) -> Result<bool, HighlightError> {
    let key = normalized_language(language)?;
    Ok(configurations().contains_key(key.as_str()))
}

/// Languages MarkDev can highlight, for diagnostics and tests.
pub fn languages() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = configurations().keys().copied().collect();
    names.sort_unstable();
    names
}

/// Highlights `code`, returning UTF-16 ranges relative to it.
///
/// Returns empty for an unknown language or a grammar error — code without
/// highlighting reads fine, so failing soft is right here.
pub fn highlight(language: &str, code: &str) -> Vec<HighlightSpan> {
    highlight_checked(language, code).unwrap_or_default()
}

/// Highlights a bounded input, rejecting the entire result on every limit or
/// grammar error so a prefix can never masquerade as complete highlighting.
pub fn highlight_checked(language: &str, code: &str) -> Result<Vec<HighlightSpan>, HighlightError> {
    if code.len() > MAX_HIGHLIGHT_CODE_BYTES {
        return Err(HighlightError::CodeTooLarge);
    }
    let key = normalized_language(language)?;
    let Some(config) = configurations().get(key.as_str()) else {
        return Ok(Vec::new());
    };

    let mut highlighter = Highlighter::new();
    let events = highlighter
        .highlight(config, code.as_bytes(), None, |_| None)
        .map_err(|_| HighlightError::Grammar)?;

    let mapper = ByteToUtf16::new(code);
    let mut spans: Vec<HighlightSpan> = Vec::new();
    // tree-sitter nests highlights; the innermost is the one that should win,
    // so the stack's top is applied to any source between events.
    let mut stack: Vec<HighlightKind> = Vec::new();
    let mut event_count = 0usize;

    for event in events {
        charge_event(&mut event_count)?;
        let event = event.map_err(|_| HighlightError::Grammar)?;
        match event {
            HighlightEvent::HighlightStart(highlight) => {
                if let Some((_, kind)) = CAPTURES.get(highlight.0) {
                    push_capture(&mut stack, *kind)?;
                }
            }
            HighlightEvent::HighlightEnd => {
                stack.pop();
            }
            HighlightEvent::Source { start, end } => {
                let Some(kind) = stack.last().copied() else {
                    continue;
                };
                if start >= end {
                    continue;
                }
                let span = HighlightSpan {
                    start: mapper.to_utf16(start),
                    end: mapper.to_utf16(end),
                    kind: kind as u16,
                    _padding: 0,
                };
                push_span(&mut spans, span)?;
            }
        }
    }

    Ok(spans)
}

fn charge_event(event_count: &mut usize) -> Result<(), HighlightError> {
    *event_count = event_count
        .checked_add(1)
        .ok_or(HighlightError::TooManyEvents)?;
    if *event_count > MAX_HIGHLIGHT_EVENTS {
        return Err(HighlightError::TooManyEvents);
    }
    Ok(())
}

fn push_capture(stack: &mut Vec<HighlightKind>, kind: HighlightKind) -> Result<(), HighlightError> {
    if stack.len() >= MAX_HIGHLIGHT_NESTING {
        return Err(HighlightError::TooDeep);
    }
    stack.push(kind);
    Ok(())
}

fn push_span(spans: &mut Vec<HighlightSpan>, span: HighlightSpan) -> Result<(), HighlightError> {
    // Adjacent runs of the same kind merge, which keeps the attribute count
    // down on long code blocks without weakening the materialized-span cap.
    if let Some(last) = spans.last_mut() {
        if last.kind == span.kind && last.end == span.start {
            last.end = span.end;
            return Ok(());
        }
    }
    if spans.len() >= MAX_HIGHLIGHT_SPANS {
        return Err(HighlightError::TooManySpans);
    }
    spans.push(span);
    Ok(())
}

fn normalized_language(language: &str) -> Result<String, HighlightError> {
    if language.len() > MAX_HIGHLIGHT_LANGUAGE_BYTES {
        return Err(HighlightError::LanguageTooLong);
    }
    if language.as_bytes().contains(&0) {
        return Err(HighlightError::InteriorNul);
    }
    Ok(language.trim().to_lowercase())
}

/// Byte to UTF-16 offset mapping, with an ASCII fast path.
struct ByteToUtf16<'a> {
    text: &'a str,
    checkpoints: Option<Vec<(u32, u32)>>,
    len: u32,
}

impl<'a> ByteToUtf16<'a> {
    fn new(text: &'a str) -> Self {
        if text.is_ascii() {
            return Self {
                text,
                checkpoints: None,
                len: text.len() as u32,
            };
        }
        let mut checkpoints = Vec::with_capacity(text.len() / UTF16_CHECKPOINT_BYTES + 2);
        let mut utf16 = 0u32;
        let mut last_checkpoint = 0usize;
        for (byte, ch) in text.char_indices() {
            if byte == 0 || byte.saturating_sub(last_checkpoint) >= UTF16_CHECKPOINT_BYTES {
                checkpoints.push((byte as u32, utf16));
                last_checkpoint = byte;
            }
            utf16 += ch.len_utf16() as u32;
        }
        if checkpoints.last().map(|&(byte, _)| byte as usize) != Some(text.len()) {
            checkpoints.push((text.len() as u32, utf16));
        }
        Self {
            text,
            checkpoints: Some(checkpoints),
            len: utf16,
        }
    }

    fn to_utf16(&self, byte: usize) -> u32 {
        let Some(checkpoints) = &self.checkpoints else {
            return (byte as u32).min(self.len);
        };
        let target = byte.min(self.text.len());
        let index = match checkpoints.binary_search_by_key(&(target as u32), |&(b, _)| b) {
            Ok(index) => return checkpoints[index].1,
            Err(0) => 0,
            Err(index) => index - 1,
        };
        let (start_byte, mut utf16) = checkpoints[index];
        for (relative, character) in self.text[start_byte as usize..].char_indices() {
            let character_start = start_byte as usize + relative;
            if character_start >= target
                || character_start.saturating_add(character.len_utf8()) > target
            {
                break;
            }
            utf16 += character.len_utf16() as u32;
        }
        utf16.min(self.len)
    }
}

#[cfg(test)]
mod limit_tests {
    use super::*;

    #[test]
    fn event_budget_accepts_exactly_the_limit_and_rejects_plus_one() {
        let mut count = 0;
        for _ in 0..MAX_HIGHLIGHT_EVENTS {
            charge_event(&mut count).expect("exact event budget");
        }
        assert_eq!(count, MAX_HIGHLIGHT_EVENTS);
        assert_eq!(charge_event(&mut count), Err(HighlightError::TooManyEvents));
    }

    #[test]
    fn capture_depth_accepts_exactly_the_limit_and_rejects_plus_one() {
        let mut stack = Vec::new();
        for _ in 0..MAX_HIGHLIGHT_NESTING {
            push_capture(&mut stack, HighlightKind::Keyword).expect("exact nesting budget");
        }
        assert_eq!(stack.len(), MAX_HIGHLIGHT_NESTING);
        assert_eq!(
            push_capture(&mut stack, HighlightKind::String),
            Err(HighlightError::TooDeep)
        );
    }

    #[test]
    fn span_budget_accepts_exactly_the_limit_and_rejects_plus_one() {
        let mut spans = Vec::new();
        for index in 0..MAX_HIGHLIGHT_SPANS {
            let start = u32::try_from(index * 2).expect("test offset");
            push_span(
                &mut spans,
                HighlightSpan {
                    start,
                    end: start + 1,
                    kind: HighlightKind::Keyword as u16,
                    _padding: 0,
                },
            )
            .expect("exact span budget");
        }
        assert_eq!(spans.len(), MAX_HIGHLIGHT_SPANS);
        assert_eq!(
            push_span(
                &mut spans,
                HighlightSpan {
                    start: 1_000_000,
                    end: 1_000_001,
                    kind: HighlightKind::String as u16,
                    _padding: 0,
                }
            ),
            Err(HighlightError::TooManySpans)
        );
    }

    #[test]
    fn byte_mapping_is_sparse_and_preserves_utf16_boundaries() {
        let text = "𝄞é".repeat(100_000);
        let mapper = ByteToUtf16::new(&text);
        let checkpoints = mapper.checkpoints.as_ref().expect("non-ASCII table");
        assert!(checkpoints.len() <= text.len() / UTF16_CHECKPOINT_BYTES + 2);
        for byte in [0, 1, 63, 64, 65, text.len() / 2, text.len()] {
            let target = byte.min(text.len());
            if text.is_char_boundary(target) {
                let expected = text[..target].chars().map(char::len_utf16).sum::<usize>() as u32;
                assert_eq!(mapper.to_utf16(byte), expected);
            } else {
                let previous = (0..target)
                    .rev()
                    .find(|candidate| text.is_char_boundary(*candidate))
                    .unwrap_or(0);
                let expected = text[..previous].chars().map(char::len_utf16).sum::<usize>() as u32;
                assert_eq!(mapper.to_utf16(byte), expected);
            }
        }
    }
}
