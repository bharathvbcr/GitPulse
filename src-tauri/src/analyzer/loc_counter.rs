//! Line accounting: how many lines of a file are code, comment, or blank.
//!
//! The previous counter classified a line by one test — "does the trimmed line
//! start with this language's single comment prefix?" — which is wrong for
//! every language that has block comments. `/* … */` spanning four lines
//! reported four lines of *code*; a Python module docstring reported code; a
//! CSS or HTML comment reported exactly one comment line (the opener) and the
//! rest as code. Since [`crate::engine::git_reader`] sums `code_lines` into the
//! repository's headline LOC number and every language-bar percentage, those
//! were not cosmetic errors — they were the number.
//!
//! What replaces it is a per-language scanner ([`CommentSyntax`]) that tracks
//! block-comment nesting and string literals across lines. Strings matter for
//! correctness, not completeness: without them a `"/*"` inside a string literal
//! would open a comment that swallows the rest of the file.
//!
//! ## Deliberate boundaries
//!
//! This is a line classifier, not a parser, and the places it is approximate
//! are chosen rather than accidental:
//!
//! * **Mixed lines count as code.** A line holding both code and a trailing
//!   comment is one code line. This matches `cloc` and `tokei`, so GitPulse's
//!   numbers are comparable to the tools users already run.
//! * **A blank line is blank even inside a block comment.** Also `cloc`'s rule;
//!   an empty line reads as empty to a human regardless of context.
//! * **Single-line strings cannot span lines** unless the language says they
//!   can ([`StringSyntax::multiline`]). This is the load-bearing bound: an
//!   unbalanced apostrophe in YAML prose would otherwise leave the scanner
//!   inside a string forever, silently reclassifying every comment below it as
//!   code. Resetting at end-of-line confines the damage to the line that
//!   caused it.
//! * **Raw-string sigils are not modelled** (Rust `r#"…"#`, C++ `R"(…)"`).
//!   A `//` inside one is miscounted as a comment on a line that is already
//!   code, so the code/comment split shifts by at most that line.
//!
//! Nothing here allocates per line and every scan is a single pass over the
//! bytes, so the counter stays cheap enough to run over a whole worktree.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LineCounts {
    pub total_lines: usize,
    pub code_lines: usize,
    pub comment_lines: usize,
    pub blank_lines: usize,
}

/// One string-literal form in a language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StringSyntax {
    pub open: &'static str,
    pub close: &'static str,
    /// A backslash escapes the closing delimiter (`"a\"b"`).
    pub escaped: bool,
    /// The literal may contain a newline. When false the scanner drops back to
    /// code at end-of-line, so an unbalanced quote cannot corrupt the rest of
    /// the file.
    pub multiline: bool,
    /// Counts as a comment when it opens a line with only whitespace before it
    /// — Python and Elixir module/function docstrings.
    pub doc: bool,
}

impl StringSyntax {
    const fn quote(open: &'static str, escaped: bool) -> Self {
        Self {
            open,
            close: open,
            escaped,
            multiline: false,
            doc: false,
        }
    }

    const fn multiline_quote(open: &'static str, escaped: bool) -> Self {
        Self {
            open,
            close: open,
            escaped,
            multiline: true,
            doc: false,
        }
    }

    const fn docstring(open: &'static str) -> Self {
        Self {
            open,
            close: open,
            escaped: true,
            multiline: true,
            doc: true,
        }
    }
}

/// Comment and string syntax for one language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommentSyntax {
    /// Markers that comment out the rest of the line. Order is irrelevant; the
    /// scanner takes the longest match so `///` and `//` can coexist.
    pub line: &'static [&'static str],
    /// Block delimiter pairs, e.g. `("/*", "*/")`.
    pub block: &'static [(&'static str, &'static str)],
    /// Block comments nest (Rust, D, Swift).
    pub nests: bool,
    /// String forms, so a delimiter inside a literal cannot open a comment.
    pub strings: &'static [StringSyntax],
}

impl CommentSyntax {
    /// Syntax that recognises nothing — JSON, CSV, plain text. Every non-blank
    /// line is content.
    pub const NONE: CommentSyntax = CommentSyntax {
        line: &[],
        block: &[],
        nests: false,
        strings: &[],
    };

    /// The first line marker, for callers that only need a prefix.
    pub fn first_line_marker(&self) -> Option<&'static str> {
        self.line.first().copied()
    }
}

// ---- Shared string tables ------------------------------------------------

/// Double and single quotes with backslash escapes, neither spanning lines.
/// The C family, Java, C#, Swift, Kotlin, Scala, Zig.
const STR_DQ_SQ: &[StringSyntax] = &[
    StringSyntax::quote("\"", true),
    StringSyntax::quote("'", true),
];

/// Double quotes only. For languages where `'` is part of an identifier or a
/// lifetime (Rust `'a`, Haskell `x'`, OCaml `'a`, Erlang atoms): treating it as
/// a string opener there would swallow real code.
const STR_DQ: &[StringSyntax] = &[StringSyntax::quote("\"", true)];

/// JavaScript and friends: template literals genuinely span lines.
const STR_JS: &[StringSyntax] = &[
    StringSyntax::quote("\"", true),
    StringSyntax::quote("'", true),
    StringSyntax::multiline_quote("`", true),
];

/// Go: backtick strings are raw and multi-line, with no escapes inside them.
const STR_GO: &[StringSyntax] = &[
    StringSyntax::quote("\"", true),
    StringSyntax::quote("'", true),
    StringSyntax::multiline_quote("`", false),
];

/// Python: triple quotes are listed first for readability, though the scanner's
/// longest-match rule is what actually picks them over the single-character
/// forms.
const STR_PYTHON: &[StringSyntax] = &[
    StringSyntax::docstring("\"\"\""),
    StringSyntax::docstring("'''"),
    StringSyntax::quote("\"", true),
    StringSyntax::quote("'", true),
];

/// Shell: both quote forms may span lines, which is how here-strings and
/// multi-line arguments are written.
const STR_SHELL: &[StringSyntax] = &[
    StringSyntax::multiline_quote("\"", true),
    StringSyntax::multiline_quote("'", false),
];

/// YAML/TOML style: quotes never span lines. YAML's own multi-line scalars are
/// indentation-based, and plain scalars are full of unbalanced apostrophes
/// (`name: it's fine`) that must not leak into the next line.
const STR_CONFIG: &[StringSyntax] = &[
    StringSyntax::quote("\"", true),
    StringSyntax::quote("'", false),
];

// ---- Shared syntax tables ------------------------------------------------

const C_STYLE: CommentSyntax = CommentSyntax {
    line: &["//"],
    block: &[("/*", "*/")],
    nests: false,
    strings: STR_DQ_SQ,
};

const GO_STYLE: CommentSyntax = CommentSyntax {
    line: &["//"],
    block: &[("/*", "*/")],
    nests: false,
    strings: STR_GO,
};

const JS_STYLE: CommentSyntax = CommentSyntax {
    line: &["//"],
    block: &[("/*", "*/")],
    nests: false,
    strings: STR_JS,
};

/// Rust nests block comments, and `'` is a lifetime rather than a string.
const RUST_STYLE: CommentSyntax = CommentSyntax {
    line: &["//"],
    block: &[("/*", "*/")],
    nests: true,
    strings: STR_DQ,
};

const HASH_STYLE: CommentSyntax = CommentSyntax {
    line: &["#"],
    block: &[],
    nests: false,
    strings: STR_CONFIG,
};

const SHELL_STYLE: CommentSyntax = CommentSyntax {
    line: &["#"],
    block: &[],
    nests: false,
    strings: STR_SHELL,
};

const PYTHON_STYLE: CommentSyntax = CommentSyntax {
    line: &["#"],
    block: &[],
    nests: false,
    strings: STR_PYTHON,
};

/// Markup: only `<!-- -->`, and no string syntax. Attribute quotes are noise
/// here — they never contain a comment delimiter that matters.
const MARKUP_STYLE: CommentSyntax = CommentSyntax {
    line: &[],
    block: &[("<!--", "-->")],
    nests: false,
    strings: &[],
};

/// Single-file components: script and style blocks make `//` and `/* */` real,
/// alongside the markup comment.
const COMPONENT_STYLE: CommentSyntax = CommentSyntax {
    line: &["//"],
    block: &[("<!--", "-->"), ("/*", "*/")],
    nests: false,
    strings: STR_JS,
};

const CSS_STYLE: CommentSyntax = CommentSyntax {
    line: &[],
    block: &[("/*", "*/")],
    nests: false,
    strings: STR_DQ_SQ,
};

/// SCSS/Less/Stylus add `//` on top of CSS's block comments.
const CSS_LINE_STYLE: CommentSyntax = CommentSyntax {
    line: &["//"],
    block: &[("/*", "*/")],
    nests: false,
    strings: STR_DQ_SQ,
};

const SQL_STYLE: CommentSyntax = CommentSyntax {
    line: &["--"],
    block: &[("/*", "*/")],
    nests: false,
    strings: STR_CONFIG,
};

/// Haskell, Elm, PureScript: `--` plus a nesting `{- -}`.
const HASKELL_STYLE: CommentSyntax = CommentSyntax {
    line: &["--"],
    block: &[("{-", "-}")],
    nests: true,
    strings: STR_DQ,
};

const LUA_STYLE: CommentSyntax = CommentSyntax {
    line: &["--"],
    block: &[("--[[", "]]")],
    nests: false,
    strings: STR_DQ_SQ,
};

/// Lisps: `;` to end of line, `#| |#` block, and `'` is quote, never a string.
const LISP_STYLE: CommentSyntax = CommentSyntax {
    line: &[";"],
    block: &[("#|", "|#")],
    nests: true,
    strings: STR_DQ,
};

/// OCaml/ReasonML/F#/Pascal: `(* *)`, which nests in OCaml. `'` is a type
/// variable, so it is not a string opener.
const ML_STYLE: CommentSyntax = CommentSyntax {
    line: &["//"],
    block: &[("(*", "*)")],
    nests: true,
    strings: STR_DQ,
};

const ERLANG_STYLE: CommentSyntax = CommentSyntax {
    line: &["%"],
    block: &[],
    nests: false,
    strings: STR_DQ,
};

/// Ruby: `#` plus the `=begin`/`=end` block form.
const RUBY_STYLE: CommentSyntax = CommentSyntax {
    line: &["#"],
    block: &[("=begin", "=end")],
    nests: false,
    strings: STR_DQ_SQ,
};

/// Julia: `#` plus the nesting `#= =#`.
const JULIA_STYLE: CommentSyntax = CommentSyntax {
    line: &["#"],
    block: &[("#=", "=#")],
    nests: true,
    strings: STR_DQ,
};

/// Elixir: `#`, and `"""` heredocs that carry `@doc` text.
const ELIXIR_STYLE: CommentSyntax = CommentSyntax {
    line: &["#"],
    block: &[],
    nests: false,
    strings: &[
        StringSyntax::docstring("\"\"\""),
        StringSyntax::quote("\"", true),
    ],
};

/// D: `//`, `/* */`, and the nesting `/+ +/`.
const D_STYLE: CommentSyntax = CommentSyntax {
    line: &["//"],
    block: &[("/*", "*/"), ("/+", "+/")],
    nests: true,
    strings: STR_DQ_SQ,
};

/// WebAssembly text: `;;` line comments and `(; ;)` blocks.
const WAT_STYLE: CommentSyntax = CommentSyntax {
    line: &[";;"],
    block: &[("(;", ";)")],
    nests: true,
    strings: STR_DQ,
};

const FORTRAN_STYLE: CommentSyntax = CommentSyntax {
    line: &["!"],
    block: &[],
    nests: false,
    strings: STR_CONFIG,
};

const BATCH_STYLE: CommentSyntax = CommentSyntax {
    line: &["REM", "@REM", "::"],
    block: &[],
    nests: false,
    strings: STR_DQ,
};

/// PHP accepts both `//` and `#` for line comments.
const PHP_STYLE: CommentSyntax = CommentSyntax {
    line: &["//", "#"],
    block: &[("/*", "*/")],
    nests: false,
    strings: STR_DQ_SQ,
};

const RST_STYLE: CommentSyntax = CommentSyntax {
    line: &[".."],
    block: &[],
    nests: false,
    strings: &[],
};

/// Returns the comment and string syntax for a detected language name.
///
/// Names are the ones [`crate::analyzer::LanguageDetector`] emits. The
/// fallback is C-style rather than "no comments": an unrecognised language is
/// far more likely to use `//` and `/* */` than nothing at all, and treating
/// comments as code inflates the very metric this module reports.
pub fn comment_syntax(lang: &str) -> CommentSyntax {
    match lang {
        // C family and the languages that borrowed its syntax.
        "C" | "C++" | "CUDA" | "Objective-C" | "Objective-C++" | "Java" | "C#" | "Kotlin"
        | "Scala" | "Groovy" | "Swift" | "Zig" | "Solidity" | "Verilog" | "SystemVerilog"
        | "Protocol Buffer" | "GraphQL" | "AsciiDoc" | "ReScript" => C_STYLE,
        "Go" => GO_STYLE,
        "JavaScript" | "TypeScript" | "JSX" | "TSX" => JS_STYLE,
        "Rust" => RUST_STYLE,
        "D" => D_STYLE,

        // Hash-comment scripting and configuration.
        "Python" => PYTHON_STYLE,
        "Shell" | "Dockerfile" | "Makefile" | "Perl" | "PowerShell" | "Procfile" => SHELL_STYLE,
        "YAML" | "TOML" | "INI" | "Git Config" | "CMake" | "R" | "Nim" | "Crystal" | "Prisma"
        | "Nix" | "Terraform" | "GDScript" => HASH_STYLE,
        "Ruby" => RUBY_STYLE,
        "Julia" => JULIA_STYLE,
        "Elixir" => ELIXIR_STYLE,
        "Erlang" => ERLANG_STYLE,

        // Markup and styles.
        "HTML" | "XML" | "Markdown" | "HTML+ERB" | "Blade" => MARKUP_STYLE,
        "Vue" | "Svelte" | "Astro" => COMPONENT_STYLE,
        "CSS" => CSS_STYLE,
        "Less" | "Sass" | "SCSS" | "Stylus" | "PostCSS" => CSS_LINE_STYLE,

        // Dash-comment languages.
        "SQL" => SQL_STYLE,
        "Haskell" | "Elm" | "PureScript" | "Ada" | "VHDL" => HASKELL_STYLE,
        "Lua" => LUA_STYLE,

        // Lisps and ML.
        "Clojure" | "Common Lisp" | "Racket" | "Scheme" | "Assembly" => LISP_STYLE,
        "OCaml" | "ReasonML" | "F#" | "Pascal" => ML_STYLE,
        "WebAssembly" => WAT_STYLE,

        "Fortran" => FORTRAN_STYLE,
        "Batchfile" => BATCH_STYLE,
        "PHP" => PHP_STYLE,
        "reStructuredText" => RST_STYLE,

        // Formats with no comment syntax at all.
        "Diff" | "CSV" | "TSV" | "JSON" | "Text" => CommentSyntax::NONE,

        // Unknown: assume the most common shape rather than "no comments".
        _ => C_STYLE,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DiffChurn {
    pub additions: usize,
    pub deletions: usize,
    pub files_changed: usize,
}

impl DiffChurn {
    pub fn net(&self) -> i64 {
        self.additions as i64 - self.deletions as i64
    }

    pub fn total_changes(&self) -> usize {
        self.additions.saturating_add(self.deletions)
    }

    /// Parses `git diff --shortstat` output such as
    /// ` 3 files changed, 42 insertions(+), 7 deletions(-)`.
    ///
    /// Counts saturate rather than falling back to zero. A run of digits too
    /// large for `usize` used to parse as `None` and become `0`, so the largest
    /// possible diff reported as "no change" — a reading the caller cannot tell
    /// apart from a genuinely empty diff.
    pub fn parse_shortstat(stat: &str) -> Self {
        let mut churn = DiffChurn::default();
        for part in stat.split(',') {
            let part = part.trim();
            let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                continue;
            }
            let n: usize = digits.parse().unwrap_or(usize::MAX);
            if part.contains("file") {
                churn.files_changed = n;
            } else if part.contains("insertion") {
                churn.additions = n;
            } else if part.contains("deletion") {
                churn.deletions = n;
            }
        }
        churn
    }
}

pub struct LocCounter;

/// Where the scanner is between lines. Block comments and multi-line strings
/// carry over; single-line strings deliberately do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Carry {
    Code,
    /// Inside `syntax.block[idx]`, nested `depth` deep.
    Block {
        idx: usize,
        depth: u32,
    },
    /// Inside `syntax.strings[idx]`; `doc` when it opened a line as a docstring.
    Str {
        idx: usize,
        doc: bool,
    },
}

/// Nesting bound. Real code never nests block comments this deep; the cap keeps
/// a hostile file from driving the depth counter to overflow.
const MAX_BLOCK_NESTING: u32 = 1_024;

/// Length of the longest marker in `markers` matching at `rest`, if any.
///
/// Longest-match, not first-match: `///` and `//` can both be listed, and
/// Python's `"""` must win over `"`.
fn match_longest(rest: &[u8], markers: &[&'static str]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for marker in markers {
        let bytes = marker.as_bytes();
        if !bytes.is_empty() && rest.starts_with(bytes) && best.is_none_or(|len| bytes.len() > len)
        {
            best = Some(bytes.len());
        }
    }
    best
}

/// Index and open-length of the longest block delimiter matching at `rest`.
fn match_block_open(
    rest: &[u8],
    blocks: &[(&'static str, &'static str)],
) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for (idx, (open, _)) in blocks.iter().enumerate() {
        let bytes = open.as_bytes();
        if !bytes.is_empty()
            && rest.starts_with(bytes)
            && best.is_none_or(|(_, len)| bytes.len() > len)
        {
            best = Some((idx, bytes.len()));
        }
    }
    best
}

/// Index and open-length of the longest string delimiter matching at `rest`.
fn match_string_open(rest: &[u8], strings: &[StringSyntax]) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for (idx, form) in strings.iter().enumerate() {
        let bytes = form.open.as_bytes();
        if !bytes.is_empty()
            && rest.starts_with(bytes)
            && best.is_none_or(|(_, len)| bytes.len() > len)
        {
            best = Some((idx, bytes.len()));
        }
    }
    best
}

/// How one line was classified, before the blank-line rule is applied.
#[derive(Default)]
struct LineFlags {
    code: bool,
    comment: bool,
}

impl LocCounter {
    /// Counts a file whose language is known.
    ///
    /// This is the entry point production code should use: the language name
    /// selects block-comment and string rules, which is the difference between
    /// counting a four-line `/* … */` as comment and counting it as code.
    pub fn count_for_language(content: &str, lang: &str) -> LineCounts {
        Self::count_with_syntax(content, comment_syntax(lang))
    }

    /// Back-compatible entry point: a single line-comment prefix, no block
    /// comments.
    ///
    /// Kept because it is the shape of the `cmd_count_loc` IPC payload, where
    /// the caller supplies a prefix rather than a language. `None` means
    /// C-style, matching the previous default. Prefer
    /// [`LocCounter::count_for_language`] wherever the language is known.
    pub fn count(content: &str, line_comment_prefix: Option<&str>) -> LineCounts {
        match line_comment_prefix {
            None => Self::count_with_syntax(content, C_STYLE),
            // A caller-supplied prefix is a runtime value while CommentSyntax
            // holds `&'static str`, so classify against it directly.
            Some(prefix) => Self::count_with_prefix(content, prefix),
        }
    }

    /// Counts against caller-supplied [`CommentSyntax`].
    pub fn count_with_syntax(content: &str, syntax: CommentSyntax) -> LineCounts {
        let mut counts = LineCounts::default();
        let mut carry = Carry::Code;

        for line in strip_bom(content).lines() {
            counts.total_lines += 1;
            let flags = scan_line(line, &syntax, &mut carry);
            if line.trim().is_empty() {
                counts.blank_lines += 1;
            } else if flags.code {
                counts.code_lines += 1;
            } else if flags.comment {
                counts.comment_lines += 1;
            } else {
                counts.code_lines += 1;
            }
        }
        counts
    }

    /// The old prefix-only rule, for the one caller that supplies a runtime
    /// prefix. No block comments and no string tracking: a line is a comment
    /// when its trimmed form starts with `prefix`.
    fn count_with_prefix(content: &str, prefix: &str) -> LineCounts {
        let mut counts = LineCounts::default();
        for line in strip_bom(content).lines() {
            counts.total_lines += 1;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                counts.blank_lines += 1;
            } else if !prefix.is_empty() && trimmed.starts_with(prefix) {
                counts.comment_lines += 1;
            } else {
                counts.code_lines += 1;
            }
        }
        counts
    }
}

/// Drops a leading UTF-8 byte-order mark.
///
/// Without this the BOM sits in front of the first token, so `\u{feff}// x`
/// does not start with `//` and the file's first comment counts as code.
/// Editors on Windows write BOMs routinely, so this was not a rare path.
fn strip_bom(content: &str) -> &str {
    content.strip_prefix('\u{feff}').unwrap_or(content)
}

/// Classifies one line and advances the cross-line `carry` state.
fn scan_line(line: &str, syntax: &CommentSyntax, carry: &mut Carry) -> LineFlags {
    let bytes = line.as_bytes();
    let mut flags = LineFlags::default();
    let mut i = 0usize;
    // Tracks whether only whitespace has been seen, so a docstring opener can
    // tell "this line is a docstring" from "this expression contains a string".
    let mut at_line_start = true;

    while i < bytes.len() {
        match *carry {
            Carry::Block { idx, depth } => {
                flags.comment = true;
                let (open, close) = syntax.block[idx];
                let rest = &bytes[i..];
                if rest.starts_with(close.as_bytes()) {
                    i += close.len();
                    *carry = if depth <= 1 {
                        Carry::Code
                    } else {
                        Carry::Block {
                            idx,
                            depth: depth - 1,
                        }
                    };
                } else if syntax.nests && rest.starts_with(open.as_bytes()) {
                    i += open.len();
                    *carry = Carry::Block {
                        idx,
                        depth: depth.saturating_add(1).min(MAX_BLOCK_NESTING),
                    };
                } else {
                    i += 1;
                }
            }
            Carry::Str { idx, doc } => {
                // A continuation line of a docstring reads as comment; of any
                // other string, as code.
                if doc {
                    flags.comment = true;
                } else {
                    flags.code = true;
                }
                let form = syntax.strings[idx];
                let rest = &bytes[i..];
                if form.escaped && rest.first() == Some(&b'\\') {
                    // Skip the escape and whatever it escapes, so `\"` cannot
                    // close the literal. A trailing backslash just ends the line.
                    i += if rest.len() >= 2 { 2 } else { 1 };
                } else if rest.starts_with(form.close.as_bytes()) {
                    i += form.close.len();
                    *carry = Carry::Code;
                } else {
                    i += 1;
                }
            }
            Carry::Code => {
                let rest = &bytes[i..];
                if rest[0].is_ascii_whitespace() {
                    i += 1;
                    continue;
                }
                if match_longest(rest, syntax.line).is_some() {
                    // Rest of the line is comment; nothing after it can change
                    // the classification.
                    flags.comment = true;
                    break;
                }
                if let Some((idx, len)) = match_block_open(rest, syntax.block) {
                    flags.comment = true;
                    *carry = Carry::Block { idx, depth: 1 };
                    i += len;
                    at_line_start = false;
                    continue;
                }
                if let Some((idx, len)) = match_string_open(rest, syntax.strings) {
                    let doc = syntax.strings[idx].doc && at_line_start;
                    if doc {
                        flags.comment = true;
                    } else {
                        flags.code = true;
                    }
                    *carry = Carry::Str { idx, doc };
                    i += len;
                    at_line_start = false;
                    // A same-line close (`"""one line"""`) is handled by the
                    // Str arm on the next iteration.
                    continue;
                }
                flags.code = true;
                i += 1;
                at_line_start = false;
            }
        }
    }

    // Single-line strings do not survive the newline. This is the bound that
    // keeps one unbalanced quote — an apostrophe in a YAML scalar, a stray tick
    // anywhere — from reclassifying the whole rest of the file as code.
    if let Carry::Str { idx, .. } = *carry {
        if !syntax.strings[idx].multiline {
            *carry = Carry::Code;
        }
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rust(src: &str) -> LineCounts {
        LocCounter::count_for_language(src, "Rust")
    }

    #[test]
    fn test_loc_count() {
        let code = r#"
// This is a comment
fn main() {

}
"#;
        let counts = LocCounter::count(code, Some("//"));
        assert_eq!(counts.total_lines, 5);
        assert_eq!(counts.comment_lines, 1);
        assert_eq!(counts.code_lines, 2);
        assert_eq!(counts.blank_lines, 2);
    }

    /// THE REGRESSION: a four-line block comment counted as four lines of code.
    #[test]
    fn block_comments_are_comments_not_code() {
        let counts = rust("/*\n * doc\n * more\n */\nfn a() {}\n");
        assert_eq!(counts.total_lines, 5);
        assert_eq!(counts.comment_lines, 4);
        assert_eq!(counts.code_lines, 1);
    }

    #[test]
    fn c_block_comments_are_found_even_though_the_line_marker_is_slash_slash() {
        let counts =
            LocCounter::count_for_language("/* one */\n/*\nmulti\n*/\nint main(){}\n", "C");
        assert_eq!(counts.comment_lines, 4);
        assert_eq!(counts.code_lines, 1);
    }

    #[test]
    fn python_docstrings_count_as_comments() {
        let counts = LocCounter::count_for_language(
            "\"\"\"\nmodule doc\nmore doc\n\"\"\"\ndef a():\n    pass\n",
            "Python",
        );
        assert_eq!(counts.comment_lines, 4);
        assert_eq!(counts.code_lines, 2);
    }

    /// A triple-quoted string used as a value is code, not a docstring: the
    /// difference is whether anything precedes it on the line.
    #[test]
    fn a_triple_quoted_value_is_code_not_a_docstring() {
        let counts = LocCounter::count_for_language("x = \"\"\"\nbody\n\"\"\"\n", "Python");
        assert_eq!(counts.code_lines, 3);
        assert_eq!(counts.comment_lines, 0);
    }

    #[test]
    fn css_and_html_block_comments_count_every_line() {
        let css = LocCounter::count_for_language("/*\n  a\n  b\n*/\nbody { color: red; }\n", "CSS");
        assert_eq!(css.comment_lines, 4);
        assert_eq!(css.code_lines, 1);

        let html = LocCounter::count_for_language("<!--\n  a\n  b\n-->\n<p>hi</p>\n", "HTML");
        assert_eq!(html.comment_lines, 4);
        assert_eq!(html.code_lines, 1);
    }

    #[test]
    fn rust_block_comments_nest() {
        // The inner `*/` closes only the inner comment; `fn a` stays code.
        let counts = rust("/* outer /* inner */ still comment */\nfn a() {}\n");
        assert_eq!(counts.comment_lines, 1);
        assert_eq!(counts.code_lines, 1);
    }

    #[test]
    fn a_comment_delimiter_inside_a_string_does_not_open_a_comment() {
        // Without string tracking the `/*` swallows the rest of the file.
        let counts = rust("let s = \"/*\";\nfn a() {}\nfn b() {}\n");
        assert_eq!(counts.code_lines, 3);
        assert_eq!(counts.comment_lines, 0);
    }

    #[test]
    fn a_url_in_a_string_is_not_a_comment() {
        let counts =
            LocCounter::count_for_language("const u = \"http://example.com\";\n", "TypeScript");
        assert_eq!(counts.code_lines, 1);
        assert_eq!(counts.comment_lines, 0);
    }

    /// THE BOUND: an unbalanced quote must not leak past its own line.
    #[test]
    fn an_unbalanced_quote_does_not_swallow_the_rest_of_the_file() {
        let counts =
            LocCounter::count_for_language("name: it's fine\n# a comment\nkey: 1\n", "YAML");
        assert_eq!(counts.comment_lines, 1);
        assert_eq!(counts.code_lines, 2);
    }

    #[test]
    fn rust_lifetimes_are_not_string_openers() {
        let counts = rust("fn f<'a>(x: &'a str) {}\n// trailing comment\n");
        assert_eq!(counts.code_lines, 1);
        assert_eq!(counts.comment_lines, 1);
    }

    #[test]
    fn escaped_quotes_do_not_close_a_string() {
        let counts = rust("let s = \"a\\\"/*b\";\nfn a() {}\n");
        assert_eq!(counts.code_lines, 2);
        assert_eq!(counts.comment_lines, 0);
    }

    #[test]
    fn a_byte_order_mark_does_not_hide_the_first_comment() {
        let counts = rust("\u{feff}// c\nfn a() {}\n");
        assert_eq!(counts.comment_lines, 1);
        assert_eq!(counts.code_lines, 1);
    }

    #[test]
    fn mixed_lines_count_as_code() {
        let counts = rust("fn a() {} // trailing\n");
        assert_eq!(counts.code_lines, 1);
        assert_eq!(counts.comment_lines, 0);
    }

    #[test]
    fn a_blank_line_inside_a_block_comment_is_blank() {
        let counts = rust("/*\n\n*/\n");
        assert_eq!(counts.blank_lines, 1);
        assert_eq!(counts.comment_lines, 2);
    }

    #[test]
    fn crlf_line_endings_classify_the_same_as_lf() {
        let lf = rust("a\n// c\n\n");
        let crlf = rust("a\r\n// c\r\n\r\n");
        assert_eq!(lf, crlf);
    }

    #[test]
    fn languages_without_comment_syntax_count_every_line_as_code() {
        let counts = LocCounter::count_for_language("{\n  \"//\": 1\n}\n", "JSON");
        assert_eq!(counts.code_lines, 3);
        assert_eq!(counts.comment_lines, 0);
    }

    #[test]
    fn hash_languages_that_used_to_fall_through_to_slash_slash_are_correct_now() {
        for lang in ["PowerShell", "CMake", "Git Config", "GDScript", "Procfile"] {
            let counts = LocCounter::count_for_language("# a comment\nvalue\n", lang);
            assert_eq!(counts.comment_lines, 1, "{lang} must treat # as a comment");
            assert_eq!(counts.code_lines, 1, "{lang}");
        }
    }

    #[test]
    fn go_raw_strings_span_lines_and_hide_comment_markers() {
        let counts = LocCounter::count_for_language("s := `\n// not a comment\n`\n", "Go");
        assert_eq!(counts.comment_lines, 0);
        assert_eq!(counts.code_lines, 3);
    }

    #[test]
    fn sql_understands_both_dash_and_block_comments() {
        let counts = LocCounter::count_for_language("-- one\n/*\ntwo\n*/\nSELECT 1;\n", "SQL");
        assert_eq!(counts.comment_lines, 4);
        assert_eq!(counts.code_lines, 1);
    }

    #[test]
    fn an_unterminated_block_comment_does_not_panic_and_stays_a_comment() {
        let counts = rust("/* open\nstill open\n");
        assert_eq!(counts.comment_lines, 2);
        assert_eq!(counts.code_lines, 0);
    }

    #[test]
    fn multibyte_content_is_counted_without_panicking() {
        let counts = rust("let s = \"日本語のテキスト\"; // コメント\nfn a() {}\n");
        assert_eq!(counts.total_lines, 2);
        assert_eq!(counts.code_lines, 2);
    }

    #[test]
    fn totals_always_partition_the_lines() {
        for lang in [
            "Rust",
            "C",
            "Python",
            "YAML",
            "HTML",
            "CSS",
            "Go",
            "SQL",
            "Lua",
            "Haskell",
            "JSON",
            "Elixir",
            "Ruby",
            "Julia",
            "OCaml",
            "Clojure",
            "WebAssembly",
            "Batchfile",
            "PHP",
        ] {
            let src = "a\n/* b */\n\n# c\n-- d\n\"\"\"e\"\"\"\n<!-- f -->\n// g\n";
            let counts = LocCounter::count_for_language(src, lang);
            assert_eq!(
                counts.code_lines + counts.comment_lines + counts.blank_lines,
                counts.total_lines,
                "{lang} must partition every line exactly once"
            );
        }
    }

    #[test]
    fn deeply_nested_block_comments_stay_bounded() {
        let src = format!("{}\nstill comment\n", "/*".repeat(5_000));
        let counts = rust(&src);
        assert_eq!(counts.comment_lines, 2);
        assert_eq!(counts.code_lines, 0);
    }

    #[test]
    fn test_parse_shortstat_all_fields() {
        let churn =
            DiffChurn::parse_shortstat(" 3 files changed, 42 insertions(+), 7 deletions(-)");
        assert_eq!(churn.files_changed, 3);
        assert_eq!(churn.additions, 42);
        assert_eq!(churn.deletions, 7);
        assert_eq!(churn.total_changes(), 49);
        assert_eq!(churn.net(), 35);
    }

    #[test]
    fn test_parse_shortstat_singular_and_partial() {
        assert_eq!(
            DiffChurn::parse_shortstat(" 1 file changed, 1 insertion(+)"),
            DiffChurn {
                additions: 1,
                deletions: 0,
                files_changed: 1
            }
        );
        assert_eq!(
            DiffChurn::parse_shortstat(" 1 file changed, 4 deletions(-)"),
            DiffChurn {
                additions: 0,
                deletions: 4,
                files_changed: 1
            }
        );
        assert_eq!(DiffChurn::parse_shortstat(""), DiffChurn::default());
    }

    /// A count too large for `usize` used to parse as `None` and fall back to
    /// zero, so the biggest possible diff reported as no diff at all.
    #[test]
    fn oversized_counts_saturate_rather_than_reading_as_zero() {
        let huge = "9".repeat(40);
        let churn =
            DiffChurn::parse_shortstat(&format!(" {huge} files changed, {huge} insertions(+)"));
        assert_eq!(churn.files_changed, usize::MAX);
        assert_eq!(churn.additions, usize::MAX);
        assert_eq!(churn.total_changes(), usize::MAX);
    }
}
