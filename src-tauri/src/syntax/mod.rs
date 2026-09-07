//! Tree-sitter syntax highlighting via MarkDev's `highlight_checked`.
//!
//! Offsets are UTF-16 code units, matching JavaScript string indices. The
//! six grammars MarkDev ships (rust, swift, js/ts, python, json, bash) are
//! the only languages that reach this path; everything else stays on the
//! TypeScript regex tokenizer behind the same owner in `diff/highlight.ts`.

use markdev::highlight::{highlight_checked, supports_checked, HighlightError, HighlightKind};
use serde::Serialize;

/// One highlighted range, ready for the frontend.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SyntaxHighlightSpan {
    /// UTF-16 start offset into the input.
    pub start: u32,
    /// UTF-16 end offset (exclusive).
    pub end: u32,
    /// Token class name shared with the TypeScript palette.
    pub kind: String,
}

/// Whether MarkDev has a grammar for `language`.
pub fn supports(language: &str) -> bool {
    supports_checked(language).unwrap_or(false)
}

/// Highlights `code` as `language`, returning UTF-16 spans.
///
/// An unknown language yields an empty list (not an error) — the frontend
/// falls back to its regex tokenizer. Bound / grammar failures are errors
/// so a truncated answer cannot look complete.
pub fn highlight(language: &str, code: &str) -> Result<Vec<SyntaxHighlightSpan>, String> {
    let spans = highlight_checked(language, code).map_err(describe_error)?;
    Ok(spans
        .into_iter()
        .map(|span| SyntaxHighlightSpan {
            start: span.start,
            end: span.end,
            kind: kind_name(span.kind).to_string(),
        })
        .collect())
}

fn kind_name(kind: u16) -> &'static str {
    match kind {
        k if k == HighlightKind::Keyword as u16 => "keyword",
        k if k == HighlightKind::String as u16 => "string",
        k if k == HighlightKind::Number as u16 => "number",
        k if k == HighlightKind::Comment as u16 => "comment",
        k if k == HighlightKind::Function as u16 => "function",
        k if k == HighlightKind::Type as u16 => "type",
        k if k == HighlightKind::Constant as u16 => "variable",
        k if k == HighlightKind::Variable as u16 => "variable",
        k if k == HighlightKind::Operator as u16 => "operator",
        k if k == HighlightKind::Punctuation as u16 => "punctuation",
        k if k == HighlightKind::Attribute as u16 => "attribute",
        _ => "text",
    }
}

fn describe_error(err: HighlightError) -> String {
    match err {
        HighlightError::CodeTooLarge => "code exceeds highlight size limit".into(),
        HighlightError::LanguageTooLong => "language identifier too long".into(),
        HighlightError::InteriorNul => "language or code contains an interior NUL".into(),
        HighlightError::TooManyEvents => "highlight produced too many events".into(),
        HighlightError::TooDeep => "highlight nesting exceeded the depth limit".into(),
        HighlightError::TooManySpans => "highlight produced too many spans".into(),
        HighlightError::Grammar => "tree-sitter grammar failed on this input".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_produces_keyword_spans() {
        let spans = highlight("rust", "fn main() {}").expect("highlight");
        assert!(spans.iter().any(|s| s.kind == "keyword"), "{spans:?}");
        assert!(spans.iter().all(|s| s.start < s.end));
    }

    #[test]
    fn unknown_language_is_empty_not_error() {
        let spans = highlight("klingon", "fn main() {}").expect("highlight");
        assert!(spans.is_empty());
        assert!(!supports("klingon"));
        assert!(supports("rust"));
        assert!(supports("typescript"));
        assert!(supports("shell"));
    }

    #[test]
    fn utf16_offsets_cover_non_ascii() {
        let code = "let café = 1;";
        let spans = highlight("rust", code).expect("highlight");
        let len = code.encode_utf16().count() as u32;
        for span in &spans {
            assert!(span.end <= len, "{span:?} past {len}");
        }
    }

    #[test]
    fn oversized_code_is_an_error() {
        let huge = "a".repeat(markdev::highlight::MAX_HIGHLIGHT_CODE_BYTES + 1);
        let err = highlight("rust", &huge).expect_err("should refuse");
        assert!(err.contains("size limit"), "{err}");
    }
}
