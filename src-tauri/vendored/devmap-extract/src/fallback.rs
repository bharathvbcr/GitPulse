//! Tier-2 extraction: recover declarations from a language with no grammar.
//!
//! **The gap this closes.** A file whose language has no linked tree-sitter
//! grammar reached `unavailable_extraction` and contributed *no nodes at all* —
//! not even the `File` node, which is only ever pushed on the tree-sitter path.
//! The file was recorded in `generation_files` and was absent from the graph:
//! invisible to `devmap search`, and unusable as the target of any edge.
//! Measured on two real trees, that silently swallowed every `.proto`
//! (19 files), `.ps1` (17), `.vb`, `.vue` and `.metal` in them.
//!
//! Recovering declarations was only half of that gap; the other half was that
//! the file itself had no node. `unavailable_extraction` now emits the `File`
//! node unconditionally, so a language this scanner cannot help — Markdown,
//! JSON, anything under [`NON_DECLARATIVE_LANGUAGES`] — is still addressable
//! and can still be the target of an edge. This module is responsible for
//! declarations only.
//!
//! **What it does not do.** This is a line scanner, not a parser. It recovers
//! *named top-level declarations* and nothing else: no calls, no imports, no
//! nesting, no types. It cannot know that a match sits inside a string literal
//! or a block comment. So it is deliberately built for **precision over
//! recall** — a missed declaration costs a search hit, while an invented one
//! puts a symbol in the graph that does not exist, and the graph is what other
//! tools reason about.
//!
//! That is also why the result is labelled rather than blended in.
//! [`ExtractionEngine::RegexFallback`] and [`ParseOutcome::Fallback`] mark every
//! file that came through here, so a consumer can tell a parsed symbol from a
//! pattern-matched one. Presenting the two as equivalent would be the same
//! error as an empty `indexed_hash` reading like a computed digest.

use std::sync::OnceLock;

use regex::Regex;

use crate::model::{ExtractedSymbol, Span, SymbolKind};

/// Most declarations this can find in one file.
///
/// A bound, not a tuning knob: without it a generated or minified file in an
/// unwired language could contribute tens of thousands of speculative symbols
/// and swamp the graph it is meant to enrich. Exceeding it is reported by
/// [`scan_declarations`] so the truncation is never silent.
pub const MAX_FALLBACK_SYMBOLS: usize = 2_000;

/// Longest line the scanner will consider.
///
/// Minified and generated files have single lines of megabytes; running several
/// regexes across them is unbounded work for a result that is meaningless
/// anyway, since a whole minified bundle on one line has no "declaration at a
/// line" to find.
pub(crate) const MAX_LINE_BYTES: usize = 2_000;

/// A declaration recovered by pattern, with the kind its keyword implies.
struct Pattern {
    regex: Regex,
    kind: SymbolKind,
}

/// The keyword → kind table, compiled once.
///
/// Every pattern anchors at the start of the line (after optional indentation
/// and modifiers) and requires the keyword to be followed by an identifier.
/// Anchoring is what keeps precision high: `function` appearing mid-sentence in
/// a comment or mid-expression in a callback does not match, while
/// `export async function handler(` does.
fn patterns() -> &'static [Pattern] {
    static PATTERNS: OnceLock<Vec<Pattern>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        // Modifiers seen across the languages this tier serves: access
        // keywords, `static`/`final`/`abstract`, `export`/`extern`, `async`,
        // and VB's `Shared`/`Overrides`.
        const MODIFIERS: &str = r"(?:(?:pub(?:\([^)]*\))?|public|private|protected|internal|friend|static|final|abstract|override|overrides|shared|virtual|export|extern|async|const|inline|partial|readonly|unsafe|open|sealed)\s+)*";
        // Unicode-aware, not `[A-Za-z_]`: identifiers are legal in non-Latin
        // scripts across the languages this tier serves, and an ASCII class
        // silently truncates `Café` to `Caf` and misses `Виджет` entirely —
        // producing a *wrong* name rather than no name, which is worse.
        // `-` is included for PowerShell's `Get-Widget` verb-noun convention.
        let identifier = r"([\p{Alphabetic}_][\p{Alphabetic}\p{Nd}_\-]*)";

        let build = |keywords: &str, kind: SymbolKind| Pattern {
            regex: Regex::new(&format!(
                r"(?i)^\s*{MODIFIERS}(?:{keywords})\s+{identifier}"
            ))
            .expect("fallback declaration pattern must compile"),
            kind,
        };

        vec![
            // Callables. `rpc` is protobuf's; `sub`/`proc` are VB/Pascal/Tcl.
            build("fn|func|function|def|defn|defun|sub|proc|procedure|method|rpc", SymbolKind::Function),
            // Namespacing constructs. Kept ahead of the aggregate table and
            // given their own kind because `package llm;` in a `.proto` is a
            // module declaration, not a class, and a consumer filtering the
            // graph by kind would otherwise see one class per protobuf file
            // that does not exist.
            build("module|namespace|package", SymbolKind::Module),
            // Aggregates. `message`/`service` are protobuf, `object`/`actor`
            // Kotlin/Scala/Swift, `record` Java/C#.
            build("class|struct|record|object|actor|message|service", SymbolKind::Class),
            build("interface|protocol|trait", SymbolKind::Interface),
            build("enum", SymbolKind::Enum),
            build("type|typedef|typealias|alias", SymbolKind::Struct),
        ]
    })
}

/// Comment prefixes across the languages this tier serves.
///
/// Line comments only. A declaration inside a `/* … */` block cannot be ruled
/// out without tracking state the scanner deliberately does not keep, which is
/// one of the reasons the result is labelled as pattern-matched.
const COMMENT_PREFIXES: [&str; 7] = ["//", "#", "--", "%", ";", "'", "*"];

fn is_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    COMMENT_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

/// Whether a recovered name is worth emitting.
///
/// Rejects the keyword-like and punctuation-like captures that anchoring alone
/// lets through — `type = ...`, `class {`, and single letters that are almost
/// always loop variables rather than declarations.
fn is_plausible_name(name: &str) -> bool {
    if name.len() < 2 || name.len() > 128 {
        return false;
    }
    // A name that is itself a declaration keyword means the regex matched a
    // modifier and consumed the wrong token.
    const KEYWORDS: [&str; 24] = [
        "fn",
        "func",
        "function",
        "def",
        "sub",
        "proc",
        "method",
        "rpc",
        "class",
        "struct",
        "record",
        "object",
        "actor",
        "message",
        "service",
        "module",
        "namespace",
        "package",
        "interface",
        "protocol",
        "trait",
        "enum",
        "type",
        "typedef",
    ];
    let lowered = name.to_ascii_lowercase();
    !KEYWORDS.contains(&lowered.as_str())
}

/// Languages where declaration scanning is meaningless or actively wrong.
///
/// Prose and data formats declare nothing, so anything the scanner matches in
/// them is a false positive — and not a harmless one. Run against this
/// workspace before this guard existed, the tier pulled `ReasoningBank`,
/// `TrajectoryOutcome` and `HyDEService` out of *fenced code blocks inside
/// design documents*: Go and TypeScript types that were being described, not
/// defined, attributed as real symbols of a `.md` file. Those enter the graph,
/// join against node ids, and are what other tools then reason about.
///
/// An allowlist would be the wrong shape here. The tier exists precisely to
/// serve languages nobody enumerated, so its default must be "scan"; only the
/// formats known to have no declarations are named.
const NON_DECLARATIVE_LANGUAGES: [&str; 9] = [
    "markdown", "html", "css", "json", "yaml", "toml", "config", "text",
    // A notebook's raw form is JSON. Its code lives in escaped string arrays,
    // so a line scan over the raw bytes matches JSON structure and cell
    // metadata, not declarations — the K3 error with a different file
    // extension. `crate::notebook` reconstructs the cells and parses them
    // properly; this keeps the raw form away from the line scanner.
    "notebook",
];

/// Whether tier-2 declaration recovery should run for `language`.
pub fn applies_to(language: &str) -> bool {
    !NON_DECLARATIVE_LANGUAGES.contains(&language)
}

/// The outcome of a fallback scan.
pub struct FallbackScan {
    pub symbols: Vec<ExtractedSymbol>,
    /// Declarations found beyond [`MAX_FALLBACK_SYMBOLS`] and therefore
    /// dropped. Non-zero means the file's symbol list is a prefix, not a set.
    pub truncated: usize,
    /// Lines skipped for exceeding [`MAX_LINE_BYTES`], and therefore never
    /// pattern-matched at all.
    ///
    /// The symbol cap and the line cap drop declarations for different reasons
    /// and only the first was counted, so the scanner's "N declaration(s)
    /// recovered" was a count of what it kept presented as a count of what is
    /// there. Generated `.proto` and `.ps1` routinely carry lines past this
    /// limit, so this is the ordinary case, not the exotic one.
    pub skipped_long_lines: usize,
}

/// Recover top-level declarations from `source` by line pattern.
///
/// Spans are byte offsets into `source` covering the matched declaration line,
/// so `Span::line_range` gives the right line and `devmap search` can show the
/// declaration itself. The span deliberately stops at the end of the line: the
/// scanner does not know where a body ends, and guessing would produce spans
/// that overlap the next declaration.
pub fn scan_declarations(file_path: &str, source: &str) -> FallbackScan {
    let mut symbols: Vec<ExtractedSymbol> = Vec::new();
    let mut truncated = 0usize;
    let mut skipped_long_lines = 0usize;
    let mut seen: std::collections::HashSet<(String, usize)> = std::collections::HashSet::new();
    let mut offset = 0usize;

    for line in source.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        let trimmed_len = line.trim_end_matches(['\n', '\r']).len();
        if trimmed_len > MAX_LINE_BYTES {
            // Counted, not silent: a declaration on this line is a declaration
            // the file has and the scan does not.
            skipped_long_lines += 1;
            continue;
        }
        if trimmed_len == 0 || is_comment(line) {
            continue;
        }
        let text = &line[..trimmed_len];

        for pattern in patterns() {
            let Some(captures) = pattern.regex.captures(text) else {
                continue;
            };
            let Some(matched) = captures.get(1) else {
                continue;
            };
            let name = matched.as_str();
            if !is_plausible_name(name) {
                continue;
            }
            // One symbol per name per line. Two patterns can match the same
            // line (`public static class Foo` hits both the callable and the
            // aggregate table on some inputs) and the graph must not carry the
            // same declaration twice.
            if !seen.insert((name.to_string(), line_start)) {
                continue;
            }
            if symbols.len() >= MAX_FALLBACK_SYMBOLS {
                truncated += 1;
                continue;
            }
            symbols.push(ExtractedSymbol {
                name: name.to_string(),
                qualified_name: format!("{file_path}::{name}"),
                kind: pattern.kind,
                span: Span {
                    start_byte: line_start,
                    end_byte: line_start + trimmed_len,
                },
                // Exportedness is a language-specific rule this tier cannot
                // evaluate. `false` is the conservative answer: it keeps a
                // pattern-matched symbol out of any consumer that treats
                // "exported" as a public-API claim.
                is_exported: false,
                docstring: None,
                signature: Some(text.trim().to_string()),
                parent_symbol: None,
                body_signature: None,
                declaration_hash: None,
            });
            // First matching pattern wins for a given line; the tables are
            // ordered from most to least specific.
            break;
        }
    }

    FallbackScan {
        symbols,
        truncated,
        skipped_long_lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(source: &str) -> Vec<String> {
        scan_declarations("f.txt", source)
            .symbols
            .into_iter()
            .map(|symbol| symbol.name)
            .collect()
    }

    /// `package` is a module, not a class.
    ///
    /// Every `.proto` file opens with one, so mapping it to `Class` put a
    /// non-existent class in the graph for each protobuf file in the tree.
    #[test]
    fn namespacing_keywords_are_modules_not_classes() {
        let scan = scan_declarations("api.proto", "package llm;\nmessage Req {\n");
        let package = scan.symbols.iter().find(|s| s.name == "llm").unwrap();
        assert_eq!(package.kind, SymbolKind::Module);
        let message = scan.symbols.iter().find(|s| s.name == "Req").unwrap();
        assert_eq!(message.kind, SymbolKind::Class);
    }

    #[test]
    fn recovers_protobuf_messages_services_and_rpcs() {
        let source = "syntax = \"proto3\";\n\
                      package api.v1;\n\
                      \n\
                      message GetUserRequest {\n  string id = 1;\n}\n\
                      service UserService {\n  rpc GetUser(GetUserRequest) returns (User);\n}\n";
        let found = names(source);
        assert!(found.contains(&"GetUserRequest".to_string()), "{found:?}");
        assert!(found.contains(&"UserService".to_string()), "{found:?}");
        assert!(found.contains(&"GetUser".to_string()), "{found:?}");
    }

    #[test]
    fn recovers_powershell_and_visual_basic_declarations() {
        let found = names(
            "function Get-Widget {\n  param($Id)\n}\n\
             Public Sub Main()\nEnd Sub\n\
             Public Class Widget\nEnd Class\n",
        );
        assert!(found.contains(&"Get-Widget".to_string()), "{found:?}");
        assert!(found.contains(&"Main".to_string()), "{found:?}");
        assert!(found.contains(&"Widget".to_string()), "{found:?}");
    }

    /// Precision matters more than recall here: an invented symbol enters the
    /// graph that other tools then reason about.
    #[test]
    fn does_not_invent_symbols_from_comments_or_prose() {
        assert!(names("// the function handler was removed\n").is_empty());
        assert!(names("# class Foo used to live here\n").is_empty());
        assert!(names("-- type Bar is gone\n").is_empty());
        // Prose that merely contains a keyword mid-sentence.
        assert!(names("This function is described in the docs.\n").is_empty());
        // A call, not a declaration.
        assert!(names("    handler(function() { return 1; });\n").is_empty());
    }

    #[test]
    fn a_keyword_alone_is_not_a_name() {
        assert!(names("type =\n").is_empty());
        assert!(names("class {\n").is_empty());
        // Single letters are loop variables far more often than declarations.
        assert!(names("def f\n").is_empty());
    }

    #[test]
    fn spans_point_at_the_declaration_line() {
        let source = "header\n\nfunction Widget() {\n";
        let scan = scan_declarations("a.ps1", source);
        assert_eq!(scan.symbols.len(), 1);
        let span = &scan.symbols[0].span;
        assert_eq!(
            &source[span.start_byte..span.end_byte],
            "function Widget() {"
        );
        assert_eq!(span.line_range(source), (3, 3));
    }

    #[test]
    fn qualified_names_are_file_scoped_like_every_other_extractor() {
        let scan = scan_declarations("pkg/api.proto", "message User {\n");
        assert_eq!(scan.symbols[0].qualified_name, "pkg/api.proto::User");
    }

    /// A generated file must not be able to flood the graph.
    #[test]
    fn symbol_count_is_bounded_and_the_truncation_is_reported() {
        let mut source = String::new();
        for index in 0..(MAX_FALLBACK_SYMBOLS + 50) {
            source.push_str(&format!("function Sym{index}()\n"));
        }
        let scan = scan_declarations("big.ps1", &source);
        assert_eq!(scan.symbols.len(), MAX_FALLBACK_SYMBOLS);
        assert_eq!(scan.truncated, 50, "dropped declarations must be counted");
    }

    /// A minified single-line bundle is skipped rather than scanned.
    #[test]
    fn absurdly_long_lines_are_skipped() {
        let long = format!("function A(){}\n", "x".repeat(MAX_LINE_BYTES + 10));
        assert!(scan_declarations("min.js", &long).symbols.is_empty());
    }

    #[test]
    fn empty_and_whitespace_sources_yield_nothing() {
        assert!(scan_declarations("e.txt", "").symbols.is_empty());
        assert!(scan_declarations("w.txt", "\n\n   \n").symbols.is_empty());
    }

    /// Non-ASCII must not panic the byte-offset arithmetic.
    #[test]
    fn multibyte_content_keeps_spans_valid() {
        let source = "// коммент\nfunction Виджет()\nclass Café\n";
        let scan = scan_declarations("u.txt", source);
        for symbol in &scan.symbols {
            // Slicing at these offsets would panic if they were not on
            // character boundaries.
            let _ = &source[symbol.span.start_byte..symbol.span.end_byte];
        }
        assert!(scan.symbols.iter().any(|s| s.name == "Café"));
    }
}

#[cfg(test)]
mod applicability_tests {
    use super::*;

    /// Prose and data formats must never be declaration-scanned.
    ///
    /// This is not a style preference. Before the guard, the tier attributed
    /// `ReasoningBank`, `TrajectoryOutcome` and `HyDEService` — Go and
    /// TypeScript types written inside fenced code blocks in design documents —
    /// to the `.md` files that merely described them, putting symbols in the
    /// graph that do not exist anywhere.
    #[test]
    fn prose_and_data_formats_are_not_scanned() {
        for language in ["markdown", "html", "css", "json", "yaml", "toml", "config"] {
            assert!(
                !applies_to(language),
                "{language} declares nothing; scanning it can only invent symbols"
            );
        }
    }

    /// The default is to scan, because the tier exists for languages nobody
    /// enumerated. A new unwired language must be served without a code change.
    #[test]
    fn unknown_and_programming_languages_are_scanned() {
        for language in [
            "protobuf",
            "powershell",
            "vb",
            "generic",
            "some-future-lang",
        ] {
            assert!(
                applies_to(language),
                "{language} should get tier-2 recovery"
            );
        }
    }

    /// A markdown code fence is exactly the shape that produced the false
    /// positives, so it is pinned as a scanner-level fact too: the guard above
    /// is what prevents it, and this documents what it prevents.
    #[test]
    fn a_fenced_code_block_would_otherwise_yield_symbols() {
        let doc = "# Design\n\nWe will add:\n\n```go\ntype ReasoningBank struct {\n}\n```\n";
        let scanned = scan_declarations("plan.md", doc);
        assert!(
            !scanned.symbols.is_empty(),
            "precondition: the raw scanner does match inside fences"
        );
        assert!(
            !applies_to("markdown"),
            "so markdown must be excluded before it ever reaches the scanner"
        );
    }
}
