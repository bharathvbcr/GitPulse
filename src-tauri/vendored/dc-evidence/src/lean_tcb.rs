//! Pure lexical and Trusted Computing Base (TCB) boundary auditor for Lean 4 (`.lean`).
//!
//! Canonical owner shared by `dc-verify` (rigor gate on `.lean` diffs) and
//! `devmap-extract` / `devmap-query` / `devmap` CLI (repository TCB inventory
//! and symbol/reference extraction). Keeping one implementation in `dc-evidence`
//! guarantees that `dcverify` and `devmap` never drift on nested `/- ... -/`
//! comments, string/character literals, guillemet `«...»` identifiers, or TCB
//! soundness escapes (`sorry`, `sorryAx`, `admit`, `native_decide`,
//! `Lean.ofReduceBool`, `Lean.trustCompiler`, `axiom`, `opaque`, `constant`,
//! `unsafe`, `@[extern]`, `@[implemented_by]`, `@[csimp]`, `attribute [...]`,
//! unclosed lexical constructs, and byte-cap truncation).

use serde::{Deserialize, Serialize};

/// Byte ceiling for a single `.lean` source scan so generated proof terms never
/// stall verification or indexing. Unread bytes are disclosed in
/// [`LeanTcbReport::unread_bytes`] and fail [`LeanTcbReport::is_sound_tcb`].
pub const MAX_LEAN_BYTES: usize = 512 * 1024;

/// Byte span `[start_byte, end_byte)` within the scanned `.lean` source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeanTcbSpan {
    pub start_byte: usize,
    pub end_byte: usize,
}

/// Module import header extracted from a `.lean` file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeanTcbImport {
    pub raw_import: String,
    pub module_specifier: String,
    pub span: LeanTcbSpan,
}

/// Classification of a Lean 4 declaration with respect to the Trusted Computing Base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanTcbRole {
    /// `theorem` or `lemma` checked by the kernel (provided no [`LeanTcbHole`] lies in its span).
    VerifiedSpec,
    /// `def`, `abbrev`, `structure`, `inductive`, `coinductive`, `class`, or `instance`.
    ExecutableDef,
    /// `axiom`, `opaque`, or `constant` — extends the logical theory or hides an unverified witness.
    TrustedAxiom,
    /// `@[extern ...]`, `@[implemented_by ...]`, or `@[csimp]` — substitutes runtime/FFI code.
    ExternHook,
}

/// Kind of soundness hole or TCB bypass inside a Lean 4 file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanTcbHoleKind {
    /// `sorry` or `sorryAx` axiom placeholder.
    Sorry,
    /// `admit` tactic placeholder.
    Admit,
    /// `native_decide`, `Lean.ofReduceBool`, or `Lean.trustCompiler` (trusts native compiler execution).
    NativeDecide,
    /// `unsafe` declaration bypassing kernel soundness checks.
    UnsafeDecl,
    /// Elaboration-time metaprogramming or syntax-extension escape (`elab`, `macro`, `syntax`,
    /// `notation`, `prefix`/`infix`/`postfix`, `run_cmd`, `by_elab`, `#eval`, `initialize`,
    /// `builtin_initialize`, or `@[macro]`/`@[command_elab]`/`@[term_elab]`/`@[tactic]`/
    /// `@[elab_without_expected_type]`) capable of synthesizing `sorryAx` without spelling `sorry`.
    MetaprogrammingEscape,
    /// Unclosed `/- ... -/` block comment, string literal, or `«...»` identifier (lexical ambiguity; fail-closed).
    UnclosedLexicalConstruct,
    /// Real `lean` kernel type-checking rejected the specification or timed out.
    KernelRejected,
    /// `#print axioms` reported `sorryAx` or an axiom outside `{propext, Classical.choice, Quot.sound}`.
    NonStandardAxiom,
    /// `lean4checker --fresh` rejected the compiled `.olean` environment or timed out.
    Lean4CheckerRejected,
    /// Kernel or `lean4checker` binary was required (`--require-kernel` / `DC_VERIFY_LEAN_KERNEL=require`) and unavailable.
    KernelUnavailable,
}

/// Explicit verification depth reached by a Lean 4 TCB audit so lexical scanning
/// never masquerades as kernel or `lean4checker --fresh` replay verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanVerificationTier {
    LexicalOnly,
    LeanKernel,
    KernelAndLean4checker,
}

/// Requested Lean 4 kernel replay policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeanKernelMode {
    /// Pure lexical + metaprogramming gate only.
    Off,
    /// Run `lean` (and `lean4checker --fresh` when installed) if `lean` is on `PATH`.
    Auto,
    /// Require both `lean` kernel axiom verification and `lean4checker --fresh` environment replay; fail closed otherwise.
    Require,
}

/// Lean 4's three standard foundational axioms; any other axiom in `#print axioms`
/// (including `sorryAx`, `Lean.ofReduceBool`, `Lean.trustCompiler`, or custom axioms) fails closed.
pub const ALLOWED_LEAN_CORE_AXIOMS: &[&str] = &["propext", "Classical.choice", "Quot.sound"];

/// One soundness hole or TCB escape located in a Lean 4 source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeanTcbHole {
    pub kind: LeanTcbHoleKind,
    pub token: String,
    pub enclosing_symbol: Option<String>,
    pub span: LeanTcbSpan,
}

/// Summary of a Lean 4 declaration's TCB status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeanTcbDeclaration {
    pub qualified_name: String,
    pub keyword: String,
    pub role: LeanTcbRole,
    pub attributes: Vec<String>,
    pub is_unsafe: bool,
    pub is_partial: bool,
    pub span: LeanTcbSpan,
}

/// Complete Trusted Computing Base boundary audit of a Lean 4 file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeanTcbReport {
    pub imports: Vec<LeanTcbImport>,
    pub declarations: Vec<LeanTcbDeclaration>,
    pub holes: Vec<LeanTcbHole>,
    pub unread_bytes: usize,
}

impl LeanTcbReport {
    /// Returns `true` iff the entire file was read cleanly with zero `sorry`/`admit`/
    /// `native_decide`/`unsafe` holes, zero `axiom`/`opaque`/`constant` assumptions,
    /// and zero `@[extern]`/`@[implemented_by]`/`@[csimp]` runtime substitutions.
    pub fn is_sound_tcb(&self) -> bool {
        self.unread_bytes == 0
            && self.holes.is_empty()
            && self
                .declarations
                .iter()
                .all(|d| !matches!(d.role, LeanTcbRole::TrustedAxiom | LeanTcbRole::ExternHook))
    }
}

/// Token kind produced by [`tokenize_lean`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeanTokKind {
    Ident,
    Keyword,
    Punct,
}

/// Borrowed token produced by [`tokenize_lean`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeanToken<'a> {
    pub kind: LeanTokKind,
    pub text: &'a str,
    pub start: usize,
    pub end: usize,
    pub line_first: bool,
}

/// Clamp `source` to at most `max_bytes` on a UTF-8 char boundary, returning
/// `(bounded_prefix, unread_bytes)`.
pub fn clamp_utf8_prefix(source: &str, max_bytes: usize) -> (&str, usize) {
    if source.len() <= max_bytes {
        return (source, 0);
    }
    let mut end = max_bytes;
    while end > 0 && !source.is_char_boundary(end) {
        end -= 1;
    }
    (&source[..end], source.len() - end)
}

/// Convert a byte offset within `source` to a 1-based line number.
pub fn byte_to_line_1based(source: &str, byte_offset: usize) -> u32 {
    let clamped = byte_offset.min(source.len());
    let newlines = source.as_bytes()[..clamped]
        .iter()
        .filter(|&&b| b == b'\n')
        .count();
    u32::try_from(newlines.saturating_add(1)).unwrap_or(u32::MAX)
}

pub fn is_lean_keyword(word: &str) -> bool {
    matches!(
        word,
        "import"
            | "prelude"
            | "open"
            | "export"
            | "universe"
            | "universes"
            | "variable"
            | "variables"
            | "namespace"
            | "section"
            | "mutual"
            | "end"
            | "theorem"
            | "lemma"
            | "def"
            | "abbrev"
            | "opaque"
            | "axiom"
            | "constant"
            | "inductive"
            | "coinductive"
            | "structure"
            | "class"
            | "instance"
            | "example"
            | "where"
            | "extends"
            | "deriving"
            | "private"
            | "protected"
            | "noncomputable"
            | "partial"
            | "unsafe"
            | "scoped"
            | "local"
            | "attribute"
            | "macro"
            | "macro_rules"
            | "syntax"
            | "declare_syntax_cat"
            | "elab"
            | "elab_rules"
            | "notation"
            | "infix"
            | "infixl"
            | "infixr"
            | "prefix"
            | "postfix"
            | "run_cmd"
            | "by_elab"
            | "#eval"
            | "#eval!"
            | "initialize"
            | "builtin_initialize"
            | "set_option"
            | "by"
            | "do"
            | "let"
            | "have"
            | "show"
            | "from"
            | "fun"
            | "match"
            | "with"
            | "if"
            | "then"
            | "else"
            | "for"
            | "in"
            | "return"
            | "unless"
            | "try"
            | "catch"
            | "finally"
            | "mut"
            | "calc"
            | "at"
            | "rw"
            | "simp"
            | "exact"
            | "apply"
            | "intro"
            | "intros"
            | "cases"
            | "rcases"
            | "induction"
            | "constructor"
            | "refine"
            | "rfl"
            | "omega"
            | "decide"
            | "aesop"
            | "linarith"
            | "ring"
            | "trivial"
            | "assumption"
            | "contradiction"
            | "sorry"
            | "Prop"
            | "Type"
            | "Sort"
            | "true"
            | "false"
    )
}

pub fn strip_guillemets(name: &str) -> &str {
    name.strip_prefix('«')
        .and_then(|s| s.strip_suffix('»'))
        .unwrap_or(name)
}

pub fn tcb_hole_for_token(raw_text: &str) -> Option<LeanTcbHoleKind> {
    let whole = strip_guillemets(raw_text);
    if matches!(whole, "#eval" | "#eval!") {
        return Some(LeanTcbHoleKind::MetaprogrammingEscape);
    }
    let leaf = strip_guillemets(raw_text.rsplit('.').next().unwrap_or(raw_text));
    match leaf {
        "sorry" | "sorryAx" => Some(LeanTcbHoleKind::Sorry),
        "admit" => Some(LeanTcbHoleKind::Admit),
        "native_decide" | "ofReduceBool" | "trustCompiler" => Some(LeanTcbHoleKind::NativeDecide),
        "elab" | "elab_rules" | "macro" | "macro_rules" | "syntax" | "declare_syntax_cat"
        | "notation" | "prefix" | "infix" | "infixl" | "infixr" | "postfix" | "run_cmd"
        | "by_elab" | "initialize" | "builtin_initialize" => {
            Some(LeanTcbHoleKind::MetaprogrammingEscape)
        }
        _ => None,
    }
}

fn split_lean_ident_segments(raw_name: &str) -> Vec<&str> {
    let mut segs = Vec::new();
    let mut in_guillemet = false;
    let mut seg_start = 0usize;
    for (idx, ch) in raw_name.char_indices() {
        if ch == '«' {
            in_guillemet = true;
        } else if ch == '»' {
            in_guillemet = false;
        } else if ch == '.' && !in_guillemet {
            segs.push(&raw_name[seg_start..idx]);
            seg_start = idx + 1;
        }
    }
    segs.push(&raw_name[seg_start..]);
    segs
}

pub fn qualify_lean_name(scopes: &[String], raw_name: &str) -> (String, String) {
    let raw_segs = split_lean_ident_segments(raw_name);
    let (escaped_root, active_segs) = if raw_segs.len() > 1 && raw_segs[0] == "_root_" {
        (true, &raw_segs[1..])
    } else {
        (false, &raw_segs[..])
    };
    let cleaned: Vec<&str> = active_segs
        .iter()
        .copied()
        .map(strip_guillemets)
        .filter(|s| !s.is_empty())
        .collect();
    if cleaned.is_empty() {
        let fallback = strip_guillemets(raw_name);
        return (fallback.to_string(), fallback.to_string());
    }
    let short = cleaned.last().copied().unwrap_or("").to_string();
    let joined = cleaned.join(".");
    if escaped_root || scopes.is_empty() {
        (short, joined)
    } else {
        let mut prefix = scopes.join(".");
        prefix.push('.');
        prefix.push_str(&joined);
        (short, prefix)
    }
}

pub fn is_top_level_boundary(tok: &LeanToken<'_>) -> bool {
    matches!(
        tok.text,
        "import"
            | "namespace"
            | "section"
            | "mutual"
            | "end"
            | "theorem"
            | "lemma"
            | "def"
            | "abbrev"
            | "opaque"
            | "axiom"
            | "constant"
            | "inductive"
            | "coinductive"
            | "structure"
            | "class"
            | "instance"
            | "example"
            | "attribute"
            | "elab"
            | "elab_rules"
            | "macro"
            | "macro_rules"
            | "syntax"
            | "declare_syntax_cat"
            | "notation"
            | "prefix"
            | "infix"
            | "infixl"
            | "infixr"
            | "postfix"
            | "run_cmd"
            | "#eval"
            | "#eval!"
            | "initialize"
            | "builtin_initialize"
            | "private"
            | "protected"
            | "noncomputable"
            | "partial"
            | "unsafe"
            | "scoped"
            | "local"
            | "@["
    )
}

/// Tokenize Lean 4 source while stripping `--` line comments, nested `/- ... -/`
/// block comments, string literals, and char literals so commented-out `sorry`
/// or `"sorry"` strings never produce false-positive TCB findings.
pub fn tokenize_lean(source: &str) -> (Vec<LeanToken<'_>>, Vec<LeanTcbHole>) {
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    let mut line_start = true;
    let mut tokens = Vec::new();
    let mut lexical_holes = Vec::new();

    while i < len {
        let b = bytes[i];
        if b == b'\n' {
            line_start = true;
            i += 1;
            continue;
        }
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // `--` single-line comment (unless part of `-->`? In Lean `--` always starts a line comment)
        if b == b'-' && i + 1 < len && bytes[i + 1] == b'-' {
            i += 2;
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // `/- ... -/` nested block comment (including `/-! ... -/` module docs and `/-- ... -/` docstrings)
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'-' {
            let comment_start = i;
            let mut depth = 1usize;
            i += 2;
            while i < len && depth > 0 {
                if bytes[i] == b'/' && i + 1 < len && bytes[i + 1] == b'-' {
                    depth += 1;
                    i += 2;
                } else if bytes[i] == b'-' && i + 1 < len && bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    if bytes[i] == b'\n' {
                        line_start = true;
                    }
                    let ch = source[i..].chars().next().unwrap();
                    i += ch.len_utf8();
                }
            }
            if depth > 0 {
                lexical_holes.push(LeanTcbHole {
                    kind: LeanTcbHoleKind::UnclosedLexicalConstruct,
                    token: "/-".to_string(),
                    enclosing_symbol: None,
                    span: LeanTcbSpan {
                        start_byte: comment_start,
                        end_byte: len,
                    },
                });
            }
            continue;
        }
        // Lean 4 raw string literal `r"..."`, `r#"..."#`, `r##"..."##` (backslashes are literal)
        if b == b'r' {
            let mut hashes = 0usize;
            while i + 1 + hashes < len && bytes[i + 1 + hashes] == b'#' {
                hashes += 1;
            }
            if i + 1 + hashes < len && bytes[i + 1 + hashes] == b'"' {
                let raw_start = i;
                let col0 = line_start;
                line_start = false;
                let open_tok = source[raw_start..raw_start + 2 + hashes].to_string();
                i += 2 + hashes;
                let mut closed = false;
                while i < len {
                    if bytes[i] == b'"'
                        && i + 1 + hashes <= len
                        && bytes[i + 1..i + 1 + hashes].iter().all(|&x| x == b'#')
                    {
                        i += 1 + hashes;
                        closed = true;
                        break;
                    }
                    let ch = source[i..].chars().next().unwrap();
                    if ch == '\n' {
                        line_start = true;
                    }
                    i += ch.len_utf8();
                }
                if !closed {
                    lexical_holes.push(LeanTcbHole {
                        kind: LeanTcbHoleKind::UnclosedLexicalConstruct,
                        token: open_tok,
                        enclosing_symbol: None,
                        span: LeanTcbSpan {
                            start_byte: raw_start,
                            end_byte: len,
                        },
                    });
                }
                tokens.push(LeanToken {
                    kind: LeanTokKind::Punct,
                    text: "\"\"",
                    start: raw_start,
                    end: i,
                    line_first: col0,
                });
                continue;
            }
        }
        // `"..."` string literal
        if b == b'"' {
            let str_start = i;
            let col0 = line_start;
            line_start = false;
            i += 1;
            let mut closed = false;
            while i < len {
                let c = bytes[i];
                if c == b'\\' {
                    i += 1;
                    if i < len {
                        let esc = source[i..].chars().next().unwrap();
                        if esc == '\n' {
                            line_start = true;
                        }
                        i += esc.len_utf8();
                    }
                    continue;
                }
                if c == b'"' {
                    i += 1;
                    closed = true;
                    break;
                }
                if c == b'\n' {
                    line_start = true;
                }
                let ch = source[i..].chars().next().unwrap();
                i += ch.len_utf8();
            }
            if !closed {
                lexical_holes.push(LeanTcbHole {
                    kind: LeanTcbHoleKind::UnclosedLexicalConstruct,
                    token: "\"".to_string(),
                    enclosing_symbol: None,
                    span: LeanTcbSpan {
                        start_byte: str_start,
                        end_byte: len,
                    },
                });
            }
            tokens.push(LeanToken {
                kind: LeanTokKind::Punct,
                text: "\"\"",
                start: str_start,
                end: i,
                line_first: col0,
            });
            continue;
        }
        // Char literal `'a'`, `'α'`, `'«'`, `'\n'`, `'\x22'`, `'\u03b1'`
        if b == b'\'' && i + 1 < len {
            if bytes[i + 1] == b'\\' && i + 2 < len {
                let esc = source[i + 2..].chars().next().unwrap();
                if esc != '\n' {
                    let mut k = i + 2 + esc.len_utf8();
                    if esc == 'x' || esc == 'u' {
                        while k < len && bytes[k].is_ascii_hexdigit() && k - (i + 2) <= 6 {
                            k += 1;
                        }
                    }
                    if k < len && bytes[k] == b'\'' {
                        line_start = false;
                        i = k + 1;
                        continue;
                    }
                }
            } else {
                let ch = source[i + 1..].chars().next().unwrap();
                let end = i + 1 + ch.len_utf8();
                if ch != '\'' && ch != '\\' && ch != '\n' && end < len && bytes[end] == b'\'' {
                    line_start = false;
                    i = end + 1;
                    continue;
                }
            }
        }
        // Guillemet or hierarchical identifier (`«...»`, `«Sub».«val»`, `Foo.«bar»`, `_root_.Pkg.fn`)
        if source[i..].starts_with('«') {
            let start = i;
            let col0 = line_start;
            line_start = false;
            loop {
                if source[i..].starts_with('«') {
                    let g_start = i;
                    i += '«'.len_utf8();
                    let mut closed = false;
                    while i < len {
                        if source[i..].starts_with('»') {
                            i += '»'.len_utf8();
                            closed = true;
                            break;
                        }
                        let ch = source[i..].chars().next().unwrap();
                        if ch == '\n' {
                            line_start = true;
                        }
                        i += ch.len_utf8();
                    }
                    if !closed {
                        lexical_holes.push(LeanTcbHole {
                            kind: LeanTcbHoleKind::UnclosedLexicalConstruct,
                            token: "«".to_string(),
                            enclosing_symbol: None,
                            span: LeanTcbSpan {
                                start_byte: g_start,
                                end_byte: len,
                            },
                        });
                        break;
                    }
                } else {
                    while i < len {
                        let c = source[i..].chars().next().unwrap();
                        if c.is_alphanumeric() || c == '_' || c == '\'' || c == '?' || c == '!' {
                            i += c.len_utf8();
                        } else {
                            break;
                        }
                    }
                }
                if i + 1 < len && bytes[i] == b'.' {
                    let next = source[i + 1..].chars().next().unwrap();
                    if next == '«' || next.is_alphabetic() || next == '_' {
                        i += 1;
                        continue;
                    }
                }
                break;
            }
            tokens.push(LeanToken {
                kind: LeanTokKind::Ident,
                text: &source[start..i],
                start,
                end: i,
                line_first: col0,
            });
            continue;
        }
        // Attribute open `@[`
        if b == b'@' && i + 1 < len && bytes[i + 1] == b'[' {
            let col0 = line_start;
            line_start = false;
            tokens.push(LeanToken {
                kind: LeanTokKind::Punct,
                text: &source[i..i + 2],
                start: i,
                end: i + 2,
                line_first: col0,
            });
            i += 2;
            continue;
        }
        // `:=` assignment
        if b == b':' && i + 1 < len && bytes[i + 1] == b'=' {
            let col0 = line_start;
            line_start = false;
            tokens.push(LeanToken {
                kind: LeanTokKind::Punct,
                text: &source[i..i + 2],
                start: i,
                end: i + 2,
                line_first: col0,
            });
            i += 2;
            continue;
        }
        // Arrow `->`
        if b == b'-' && i + 1 < len && bytes[i + 1] == b'>' {
            let col0 = line_start;
            line_start = false;
            tokens.push(LeanToken {
                kind: LeanTokKind::Punct,
                text: &source[i..i + 2],
                start: i,
                end: i + 2,
                line_first: col0,
            });
            i += 2;
            continue;
        }

        // `#eval`, `#eval!`, `#print`, `#check`, `#reduce` command keywords
        if b == b'#' && i + 1 < len {
            let next = source[i + 1..].chars().next().unwrap();
            if next.is_alphabetic() || next == '_' {
                let start = i;
                let col0 = line_start;
                line_start = false;
                i += 1 + next.len_utf8();
                while i < len {
                    let c = source[i..].chars().next().unwrap();
                    if c.is_alphanumeric() || c == '_' || c == '\'' || c == '?' || c == '!' {
                        i += c.len_utf8();
                    } else {
                        break;
                    }
                }
                let text = &source[start..i];
                let kind = if is_lean_keyword(text) {
                    LeanTokKind::Keyword
                } else {
                    LeanTokKind::Ident
                };
                tokens.push(LeanToken {
                    kind,
                    text,
                    start,
                    end: i,
                    line_first: col0,
                });
                continue;
            }
        }

        let ch = source[i..].chars().next().unwrap();
        let col0 = line_start;
        line_start = false;

        if ch.is_alphabetic() || ch == '_' {
            let start = i;
            i += ch.len_utf8();
            while i < len {
                let c = source[i..].chars().next().unwrap();
                if c.is_alphanumeric() || c == '_' || c == '\'' || c == '?' || c == '!' {
                    i += c.len_utf8();
                } else if c == '.' && i + 1 < len {
                    let next = source[i + 1..].chars().next().unwrap();
                    if next.is_alphabetic() || next == '_' {
                        i += 1 + next.len_utf8();
                    } else if next == '«' {
                        let g_start = i + 1;
                        i += 1 + '«'.len_utf8();
                        let mut closed = false;
                        while i < len {
                            if source[i..].starts_with('»') {
                                i += '»'.len_utf8();
                                closed = true;
                                break;
                            }
                            let gc = source[i..].chars().next().unwrap();
                            if gc == '\n' {
                                line_start = true;
                            }
                            i += gc.len_utf8();
                        }
                        if !closed {
                            lexical_holes.push(LeanTcbHole {
                                kind: LeanTcbHoleKind::UnclosedLexicalConstruct,
                                token: "«".to_string(),
                                enclosing_symbol: None,
                                span: LeanTcbSpan {
                                    start_byte: g_start,
                                    end_byte: len,
                                },
                            });
                            break;
                        }
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            let text = &source[start..i];
            let kind = if !text.contains('«') && is_lean_keyword(text) {
                LeanTokKind::Keyword
            } else {
                LeanTokKind::Ident
            };
            tokens.push(LeanToken {
                kind,
                text,
                start,
                end: i,
                line_first: col0,
            });
        } else if ch.is_ascii_digit() {
            i += ch.len_utf8();
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
        } else {
            let end = i + ch.len_utf8();
            tokens.push(LeanToken {
                kind: LeanTokKind::Punct,
                text: &source[i..end],
                start: i,
                end,
                line_first: col0,
            });
            i = end;
        }
    }

    (tokens, lexical_holes)
}

fn has_extern_hook_attr(attrs: &[String]) -> bool {
    attrs.iter().any(|a| {
        let mut words = a.split_whitespace();
        let head = match words.next() {
            Some("scoped" | "local") => words.next(),
            other => other,
        };
        if let Some(h) = head {
            let leaf = strip_guillemets(h.rsplit('.').next().unwrap_or(h));
            if matches!(leaf, "extern" | "implemented_by" | "csimp") {
                return true;
            }
        }
        a.split_whitespace()
            .map(strip_guillemets)
            .any(|w| matches!(w, "extern" | "implemented_by" | "csimp"))
    })
}

fn find_metaprogramming_attr(attrs: &[String]) -> Option<&'static str> {
    for a in attrs {
        let mut words = a.split_whitespace();
        let head = match words.next() {
            Some("scoped" | "local") => words.next(),
            other => other,
        };
        if let Some(h) = head {
            let leaf = strip_guillemets(h.rsplit('.').next().unwrap_or(h));
            match leaf {
                "macro" => return Some("macro"),
                "command_elab" => return Some("command_elab"),
                "term_elab" => return Some("term_elab"),
                "tactic" => return Some("tactic"),
                "elab_without_expected_type" => return Some("elab_without_expected_type"),
                _ => {}
            }
        }
    }
    None
}

fn emit_metaprogramming_attrs(
    attrs: &[String],
    enclosing_symbol: Option<String>,
    span: LeanTcbSpan,
    holes: &mut Vec<LeanTcbHole>,
) {
    if let Some(attr_name) = find_metaprogramming_attr(attrs) {
        holes.push(LeanTcbHole {
            kind: LeanTcbHoleKind::MetaprogrammingEscape,
            token: format!("@[{attr_name}]"),
            enclosing_symbol,
            span,
        });
    }
}

fn parse_bracketed_attrs(tokens: &[LeanToken<'_>], idx: &mut usize, out: &mut Vec<String>) {
    let mut bracket_depth = 1usize;
    let mut cur_attr = String::new();
    while *idx < tokens.len() && bracket_depth > 0 {
        let t = &tokens[*idx];
        if t.text == "[" || t.text == "@[" {
            bracket_depth += 1;
        } else if t.text == "]" {
            bracket_depth -= 1;
            if bracket_depth == 0 {
                if !cur_attr.trim().is_empty() {
                    out.push(cur_attr.trim().to_string());
                }
                *idx += 1;
                break;
            }
        } else if t.text == "," && bracket_depth == 1 {
            if !cur_attr.trim().is_empty() {
                out.push(cur_attr.trim().to_string());
            }
            cur_attr.clear();
            *idx += 1;
            continue;
        }
        if !cur_attr.is_empty() {
            cur_attr.push(' ');
        }
        cur_attr.push_str(t.text);
        *idx += 1;
    }
}

/// Audit a Lean 4 source file's Trusted Computing Base (TCB) boundaries,
/// surfacing axioms, opaque witnesses, `@[extern]`/`@[implemented_by]`/`@[csimp]`
/// runtime substitutions (including standalone `attribute [...]` commands),
/// `unsafe` definitions/examples, `sorry`/`sorryAx`/`admit`/`native_decide`/
/// `Lean.ofReduceBool`/`Lean.trustCompiler` soundness holes, unclosed
/// comments/literals/guillemets, and byte-cap truncation.
pub fn analyze_lean_tcb(source: &str) -> LeanTcbReport {
    let (bounded, unread_bytes) = clamp_utf8_prefix(source, MAX_LEAN_BYTES);
    let (tokens, mut holes) = tokenize_lean(bounded);

    let mut imports = Vec::new();
    let mut declarations = Vec::new();

    #[derive(Clone)]
    struct ScopeEntry {
        is_namespace: bool,
        parts: Vec<String>,
    }
    let mut scope_stack: Vec<ScopeEntry> = Vec::new();
    let active_ns = |stack: &[ScopeEntry]| -> Vec<String> {
        let mut out = Vec::new();
        for entry in stack {
            if entry.is_namespace {
                out.extend(entry.parts.iter().cloned());
            }
        }
        out
    };

    let mut pending_attrs: Vec<String> = Vec::new();
    let mut pending_unsafe: Option<LeanTcbSpan> = None;
    let mut pending_partial = false;
    let mut decl_start_override: Option<usize> = None;

    let mut idx = 0usize;
    while idx < tokens.len() {
        let tok = &tokens[idx];

        if tok.text == "@[" {
            if decl_start_override.is_none() {
                decl_start_override = Some(tok.start);
            }
            idx += 1;
            parse_bracketed_attrs(&tokens, &mut idx, &mut pending_attrs);
            continue;
        }

        if matches!(
            tok.text,
            "private" | "protected" | "noncomputable" | "partial" | "unsafe" | "scoped" | "local"
        ) {
            if decl_start_override.is_none() {
                decl_start_override = Some(tok.start);
            }
            match tok.text {
                "unsafe" => {
                    pending_unsafe = Some(LeanTcbSpan {
                        start_byte: tok.start,
                        end_byte: tok.end,
                    });
                }
                "partial" => pending_partial = true,
                _ => {}
            }
            idx += 1;
            continue;
        }

        // Standalone `attribute [extern "c_fn", csimp] targetDecl`
        if tok.text == "attribute" {
            let attr_start = decl_start_override.unwrap_or(tok.start);
            idx += 1;
            let mut attrs = std::mem::take(&mut pending_attrs);
            if idx < tokens.len() && tokens[idx].text == "[" {
                idx += 1;
                parse_bracketed_attrs(&tokens, &mut idx, &mut attrs);
            }
            let mut attr_end = tok.end;
            let mut targets = Vec::new();
            while idx < tokens.len()
                && tokens[idx].kind == LeanTokKind::Ident
                && !tokens[idx].line_first
            {
                attr_end = tokens[idx].end;
                targets.push(tokens[idx].text.to_string());
                idx += 1;
            }
            let ns = active_ns(&scope_stack);
            let target_sym = targets.first().map(|raw| qualify_lean_name(&ns, raw).1);
            emit_metaprogramming_attrs(
                &attrs,
                target_sym,
                LeanTcbSpan {
                    start_byte: attr_start,
                    end_byte: attr_end,
                },
                &mut holes,
            );
            if has_extern_hook_attr(&attrs) {
                let target_raw = targets.first().map(String::as_str).unwrap_or("<attribute>");
                let (_, qualified_name) = qualify_lean_name(&ns, target_raw);
                declarations.push(LeanTcbDeclaration {
                    qualified_name,
                    keyword: "attribute".to_string(),
                    role: LeanTcbRole::ExternHook,
                    attributes: attrs,
                    is_unsafe: pending_unsafe.is_some(),
                    is_partial: pending_partial,
                    span: LeanTcbSpan {
                        start_byte: attr_start,
                        end_byte: attr_end,
                    },
                });
            }
            pending_unsafe = None;
            pending_partial = false;
            decl_start_override = None;
            continue;
        }

        if tok.text == "import" {
            let imp_start = tok.start;
            idx += 1;
            if idx < tokens.len() && tokens[idx].text == "all" {
                idx += 1;
            }
            while idx < tokens.len()
                && tokens[idx].kind == LeanTokKind::Ident
                && !tokens[idx].line_first
            {
                let mod_tok = &tokens[idx];
                imports.push(LeanTcbImport {
                    raw_import: format!("import {}", mod_tok.text),
                    module_specifier: mod_tok.text.to_string(),
                    span: LeanTcbSpan {
                        start_byte: imp_start,
                        end_byte: mod_tok.end,
                    },
                });
                idx += 1;
            }
            emit_metaprogramming_attrs(
                &pending_attrs,
                None,
                LeanTcbSpan {
                    start_byte: decl_start_override.unwrap_or(imp_start),
                    end_byte: imp_start,
                },
                &mut holes,
            );
            if has_extern_hook_attr(&pending_attrs) {
                let ns = active_ns(&scope_stack);
                let (_, qualified_name) = qualify_lean_name(&ns, "<extern_attr>");
                declarations.push(LeanTcbDeclaration {
                    qualified_name,
                    keyword: "attribute".to_string(),
                    role: LeanTcbRole::ExternHook,
                    attributes: std::mem::take(&mut pending_attrs),
                    is_unsafe: pending_unsafe.is_some(),
                    is_partial: pending_partial,
                    span: LeanTcbSpan {
                        start_byte: decl_start_override.unwrap_or(imp_start),
                        end_byte: imp_start,
                    },
                });
            } else {
                pending_attrs.clear();
            }
            pending_unsafe = None;
            pending_partial = false;
            decl_start_override = None;
            continue;
        }

        if tok.text == "namespace" || tok.text == "section" || tok.text == "mutual" {
            let is_ns = tok.text == "namespace";
            let is_mutual = tok.text == "mutual";
            let kw_start = tok.start;
            idx += 1;
            let mut parts = Vec::new();
            if !is_mutual
                && idx < tokens.len()
                && tokens[idx].kind == LeanTokKind::Ident
                && !tokens[idx].line_first
            {
                parts = tokens[idx]
                    .text
                    .split('.')
                    .map(strip_guillemets)
                    .map(str::to_string)
                    .collect();
                idx += 1;
            }
            emit_metaprogramming_attrs(
                &pending_attrs,
                None,
                LeanTcbSpan {
                    start_byte: decl_start_override.unwrap_or(kw_start),
                    end_byte: kw_start,
                },
                &mut holes,
            );
            if has_extern_hook_attr(&pending_attrs) {
                let ns = active_ns(&scope_stack);
                let (_, qualified_name) = qualify_lean_name(&ns, "<extern_attr>");
                declarations.push(LeanTcbDeclaration {
                    qualified_name,
                    keyword: "attribute".to_string(),
                    role: LeanTcbRole::ExternHook,
                    attributes: std::mem::take(&mut pending_attrs),
                    is_unsafe: pending_unsafe.is_some(),
                    is_partial: pending_partial,
                    span: LeanTcbSpan {
                        start_byte: decl_start_override.unwrap_or(kw_start),
                        end_byte: kw_start,
                    },
                });
            } else {
                pending_attrs.clear();
            }
            scope_stack.push(ScopeEntry {
                is_namespace: is_ns,
                parts,
            });
            pending_unsafe = None;
            pending_partial = false;
            decl_start_override = None;
            continue;
        }

        if tok.text == "end" {
            let kw_start = tok.start;
            idx += 1;
            if idx < tokens.len()
                && tokens[idx].kind == LeanTokKind::Ident
                && !tokens[idx].line_first
            {
                let closing: Vec<&str> =
                    tokens[idx].text.split('.').map(strip_guillemets).collect();
                idx += 1;
                let mut remaining = closing.len();
                while remaining > 0 && !scope_stack.is_empty() {
                    let popped = scope_stack.pop().unwrap();
                    remaining = remaining.saturating_sub(popped.parts.len().max(1));
                }
            } else {
                scope_stack.pop();
            }
            emit_metaprogramming_attrs(
                &pending_attrs,
                None,
                LeanTcbSpan {
                    start_byte: decl_start_override.unwrap_or(kw_start),
                    end_byte: kw_start,
                },
                &mut holes,
            );
            if has_extern_hook_attr(&pending_attrs) {
                let ns = active_ns(&scope_stack);
                let (_, qualified_name) = qualify_lean_name(&ns, "<extern_attr>");
                declarations.push(LeanTcbDeclaration {
                    qualified_name,
                    keyword: "attribute".to_string(),
                    role: LeanTcbRole::ExternHook,
                    attributes: std::mem::take(&mut pending_attrs),
                    is_unsafe: pending_unsafe.is_some(),
                    is_partial: pending_partial,
                    span: LeanTcbSpan {
                        start_byte: decl_start_override.unwrap_or(kw_start),
                        end_byte: kw_start,
                    },
                });
            } else {
                pending_attrs.clear();
            }
            pending_unsafe = None;
            pending_partial = false;
            decl_start_override = None;
            continue;
        }

        if matches!(
            tok.text,
            "theorem"
                | "lemma"
                | "def"
                | "abbrev"
                | "opaque"
                | "axiom"
                | "constant"
                | "inductive"
                | "coinductive"
                | "structure"
                | "class"
                | "instance"
                | "example"
        ) {
            let kw = tok.text;
            let decl_start = decl_start_override.unwrap_or(tok.start);
            let kw_end = tok.end;
            idx += 1;

            let raw_name = if kw == "example" {
                format!("example_{decl_start}")
            } else if idx < tokens.len() && tokens[idx].kind == LeanTokKind::Ident {
                let n = tokens[idx].text;
                idx += 1;
                n.to_string()
            } else if kw == "instance" {
                format!("inst_{decl_start}")
            } else {
                let was_unsafe = pending_unsafe.is_some();
                if let Some(unsafe_span) = pending_unsafe.take() {
                    holes.push(LeanTcbHole {
                        kind: LeanTcbHoleKind::UnsafeDecl,
                        token: "unsafe".to_string(),
                        enclosing_symbol: None,
                        span: unsafe_span,
                    });
                }
                emit_metaprogramming_attrs(
                    &pending_attrs,
                    None,
                    LeanTcbSpan {
                        start_byte: decl_start,
                        end_byte: kw_end,
                    },
                    &mut holes,
                );
                if has_extern_hook_attr(&pending_attrs) {
                    let ns = active_ns(&scope_stack);
                    let (_, qualified_name) = qualify_lean_name(&ns, "<extern_decl>");
                    declarations.push(LeanTcbDeclaration {
                        qualified_name,
                        keyword: kw.to_string(),
                        role: LeanTcbRole::ExternHook,
                        attributes: std::mem::take(&mut pending_attrs),
                        is_unsafe: was_unsafe,
                        is_partial: pending_partial,
                        span: LeanTcbSpan {
                            start_byte: decl_start,
                            end_byte: kw_end,
                        },
                    });
                } else {
                    pending_attrs.clear();
                }
                pending_partial = false;
                decl_start_override = None;
                continue;
            };

            let ns = active_ns(&scope_stack);
            let (_, qualified_name) = qualify_lean_name(&ns, &raw_name);

            let body_start_idx = idx;
            let mut decl_end = bounded[decl_start..]
                .find('\n')
                .map(|nl| decl_start + nl)
                .unwrap_or(bounded.len());
            while idx < tokens.len() && !is_top_level_boundary(&tokens[idx]) {
                decl_end = tokens[idx].end;
                idx += 1;
            }
            let body_tokens = &tokens[body_start_idx..idx];

            let is_unsafe = pending_unsafe.is_some();
            if pending_unsafe.take().is_some() {
                holes.push(LeanTcbHole {
                    kind: LeanTcbHoleKind::UnsafeDecl,
                    token: "unsafe".to_string(),
                    enclosing_symbol: if kw == "example" {
                        None
                    } else {
                        Some(qualified_name.clone())
                    },
                    span: LeanTcbSpan {
                        start_byte: decl_start,
                        end_byte: decl_end,
                    },
                });
            }

            let enclosing = if kw == "example" {
                None
            } else {
                Some(qualified_name.clone())
            };
            emit_metaprogramming_attrs(
                &pending_attrs,
                enclosing.clone(),
                LeanTcbSpan {
                    start_byte: decl_start,
                    end_byte: decl_end,
                },
                &mut holes,
            );
            let has_extern = has_extern_hook_attr(&pending_attrs);
            let role = if has_extern {
                LeanTcbRole::ExternHook
            } else if matches!(kw, "axiom" | "opaque" | "constant") {
                LeanTcbRole::TrustedAxiom
            } else if matches!(kw, "theorem" | "lemma") {
                LeanTcbRole::VerifiedSpec
            } else {
                LeanTcbRole::ExecutableDef
            };

            // Keep `example` out of named `declarations` unless it carries an `@[extern]` hook,
            // preserving exact declaration counts on named specs while still catching `@[extern] example`.
            if kw != "example" || has_extern {
                declarations.push(LeanTcbDeclaration {
                    qualified_name: qualified_name.clone(),
                    keyword: kw.to_string(),
                    role,
                    attributes: std::mem::take(&mut pending_attrs),
                    is_unsafe,
                    is_partial: pending_partial,
                    span: LeanTcbSpan {
                        start_byte: decl_start,
                        end_byte: decl_end,
                    },
                });
            } else {
                pending_attrs.clear();
            }

            let mut bi = 0usize;
            while bi < body_tokens.len() {
                let bt = &body_tokens[bi];
                if bt.text == "#"
                    && bi + 1 < body_tokens.len()
                    && matches!(strip_guillemets(body_tokens[bi + 1].text), "eval" | "eval!")
                {
                    let next = &body_tokens[bi + 1];
                    holes.push(LeanTcbHole {
                        kind: LeanTcbHoleKind::MetaprogrammingEscape,
                        token: format!("#{}", strip_guillemets(next.text)),
                        enclosing_symbol: enclosing.clone(),
                        span: LeanTcbSpan {
                            start_byte: bt.start,
                            end_byte: next.end,
                        },
                    });
                    bi += 2;
                    continue;
                }
                if let Some(hole_kind) = tcb_hole_for_token(bt.text) {
                    holes.push(LeanTcbHole {
                        kind: hole_kind,
                        token: bt.text.to_string(),
                        enclosing_symbol: enclosing.clone(),
                        span: LeanTcbSpan {
                            start_byte: bt.start,
                            end_byte: bt.end,
                        },
                    });
                }
                bi += 1;
            }

            pending_partial = false;
            decl_start_override = None;
            continue;
        }

        let was_unsafe = pending_unsafe.is_some();
        if let Some(unsafe_span) = pending_unsafe.take() {
            holes.push(LeanTcbHole {
                kind: LeanTcbHoleKind::UnsafeDecl,
                token: "unsafe".to_string(),
                enclosing_symbol: None,
                span: unsafe_span,
            });
        }
        emit_metaprogramming_attrs(
            &pending_attrs,
            if tok.kind == LeanTokKind::Ident {
                Some(qualify_lean_name(&active_ns(&scope_stack), tok.text).1)
            } else {
                None
            },
            LeanTcbSpan {
                start_byte: decl_start_override.unwrap_or(tok.start),
                end_byte: tok.end,
            },
            &mut holes,
        );
        if has_extern_hook_attr(&pending_attrs) {
            let ns = active_ns(&scope_stack);
            let raw_target = if tok.kind == LeanTokKind::Ident {
                tok.text
            } else {
                "<extern_attr>"
            };
            let (_, qualified_name) = qualify_lean_name(&ns, raw_target);
            declarations.push(LeanTcbDeclaration {
                qualified_name,
                keyword: if tok.kind == LeanTokKind::Ident {
                    "local_def".to_string()
                } else {
                    "attribute".to_string()
                },
                role: LeanTcbRole::ExternHook,
                attributes: std::mem::take(&mut pending_attrs),
                is_unsafe: was_unsafe,
                is_partial: pending_partial,
                span: LeanTcbSpan {
                    start_byte: decl_start_override.unwrap_or(tok.start),
                    end_byte: tok.end,
                },
            });
        } else {
            pending_attrs.clear();
        }
        if tok.text == "#"
            && idx + 1 < tokens.len()
            && matches!(strip_guillemets(tokens[idx + 1].text), "eval" | "eval!")
        {
            let next = &tokens[idx + 1];
            holes.push(LeanTcbHole {
                kind: LeanTcbHoleKind::MetaprogrammingEscape,
                token: format!("#{}", strip_guillemets(next.text)),
                enclosing_symbol: None,
                span: LeanTcbSpan {
                    start_byte: tok.start,
                    end_byte: next.end,
                },
            });
            pending_partial = false;
            decl_start_override = None;
            idx += 2;
            continue;
        }
        if let Some(hole_kind) = tcb_hole_for_token(tok.text) {
            holes.push(LeanTcbHole {
                kind: hole_kind,
                token: tok.text.to_string(),
                enclosing_symbol: None,
                span: LeanTcbSpan {
                    start_byte: tok.start,
                    end_byte: tok.end,
                },
            });
        }

        pending_partial = false;
        decl_start_override = None;
        idx += 1;
    }

    let trailing_unsafe = pending_unsafe.is_some();
    if let Some(unsafe_span) = pending_unsafe.take() {
        holes.push(LeanTcbHole {
            kind: LeanTcbHoleKind::UnsafeDecl,
            token: "unsafe".to_string(),
            enclosing_symbol: None,
            span: unsafe_span,
        });
    }
    emit_metaprogramming_attrs(
        &pending_attrs,
        None,
        LeanTcbSpan {
            start_byte: decl_start_override.unwrap_or(bounded.len()),
            end_byte: bounded.len(),
        },
        &mut holes,
    );
    if has_extern_hook_attr(&pending_attrs) {
        let ns = active_ns(&scope_stack);
        let (_, qualified_name) = qualify_lean_name(&ns, "<extern_attr>");
        declarations.push(LeanTcbDeclaration {
            qualified_name,
            keyword: "attribute".to_string(),
            role: LeanTcbRole::ExternHook,
            attributes: std::mem::take(&mut pending_attrs),
            is_unsafe: trailing_unsafe,
            is_partial: pending_partial,
            span: LeanTcbSpan {
                start_byte: decl_start_override.unwrap_or(bounded.len()),
                end_byte: bounded.len(),
            },
        });
    }

    LeanTcbReport {
        imports,
        declarations,
        holes,
        unread_bytes,
    }
}

/// Violation discovered while parsing Lean 4 `#print axioms` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeanAxiomViolation {
    pub declaration: String,
    pub detail: String,
    pub non_standard_axioms: Vec<String>,
}

fn normalize_lean_const_name(name: &str) -> String {
    name.split('.')
        .map(strip_guillemets)
        .collect::<Vec<_>>()
        .join(".")
}

fn format_lean_ident_for_print(qualified_name: &str) -> String {
    qualified_name
        .split('.')
        .map(|seg| {
            let clean = strip_guillemets(seg);
            let plain = !clean.is_empty()
                && !is_lean_keyword(clean)
                && clean
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic() || c == '_')
                && clean
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '\'' || c == '?' || c == '!');
            if plain {
                clean.to_string()
            } else {
                format!("«{clean}»")
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn decl_has_private_modifier(source: &str, decl: &LeanTcbDeclaration) -> bool {
    let start = decl.span.start_byte.min(source.len());
    let end = decl.span.end_byte.min(source.len()).max(start);
    let (toks, _) = tokenize_lean(&source[start..end]);
    for tok in toks {
        if tok.text == decl.keyword {
            break;
        }
        if tok.text == "private" {
            return true;
        }
    }
    false
}

/// Render a standalone Lean 4 driver module that `import`s `module_name` and runs
/// `#print axioms` on every publicly addressable theorem/declaration in `declarations`,
/// returning `(driver_source, expected_normalized_names)`.
pub fn render_lean_axiom_driver(
    module_name: &str,
    source: &str,
    declarations: &[LeanTcbDeclaration],
) -> (String, Vec<String>) {
    let mut driver = format!("import {module_name}\n");
    let mut expected = Vec::new();
    for decl in declarations {
        if decl.qualified_name.contains('<')
            || decl.keyword == "attribute"
            || decl.keyword == "example"
        {
            continue;
        }
        // Skip synthetic unnamed `instance` placeholders (`inst_<start_byte>`).
        if decl.keyword == "instance" {
            let short = decl
                .qualified_name
                .rsplit('.')
                .next()
                .unwrap_or(&decl.qualified_name);
            if short == format!("inst_{}", decl.span.start_byte) {
                continue;
            }
        }
        if decl_has_private_modifier(source, decl) {
            continue;
        }
        let norm = normalize_lean_const_name(&decl.qualified_name);
        if !expected.contains(&norm) {
            driver.push_str("#print axioms ");
            driver.push_str(&format_lean_ident_for_print(&decl.qualified_name));
            driver.push('\n');
            expected.push(norm);
        }
    }
    (driver, expected)
}

/// Parse `lean` stdout from a `#print axioms` driver, rejecting any missing expected
/// declaration and any axiom outside [`ALLOWED_LEAN_CORE_AXIOMS`] (`propext`,
/// `Classical.choice`, `Quot.sound`) — including `sorryAx`, `Lean.ofReduceBool`,
/// `Lean.trustCompiler`, and custom or elaborator-synthesized axioms.
pub fn verify_lean_axioms_output(
    stdout: &str,
    expected_declarations: &[String],
) -> Result<Vec<(String, Vec<String>)>, Vec<LeanAxiomViolation>> {
    const NO_AXIOMS_SUFFIX: &str = " does not depend on any axioms";
    const DEPENDS_PREFIX: &str = " depends on axioms: [";

    let mut observed: Vec<(String, Vec<String>)> = Vec::new();
    let mut violations: Vec<LeanAxiomViolation> = Vec::new();

    let mut rest = stdout;
    while !rest.is_empty() {
        let trimmed = rest.trim_start_matches([' ', '\t', '\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(after_quote) = trimmed.strip_prefix('\'')
            && let Some(end_quote) = after_quote.find('\'')
        {
            let raw_decl = &after_quote[..end_quote];
            let norm_decl = normalize_lean_const_name(raw_decl);
            let tail = &after_quote[end_quote + 1..];
            if let Some(after_no_ax) = tail.strip_prefix(NO_AXIOMS_SUFFIX) {
                observed.push((norm_decl, Vec::new()));
                rest = after_no_ax
                    .find('\n')
                    .map(|nl| &after_no_ax[nl + 1..])
                    .unwrap_or("");
                continue;
            }
            if let Some(after_dep) = tail.strip_prefix(DEPENDS_PREFIX) {
                if let Some(close_bracket) = after_dep.find(']') {
                    let list_body = &after_dep[..close_bracket];
                    let axioms: Vec<String> = list_body
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(normalize_lean_const_name)
                        .collect();
                    let bad: Vec<String> = axioms
                        .iter()
                        .filter(|ax| !ALLOWED_LEAN_CORE_AXIOMS.contains(&ax.as_str()))
                        .cloned()
                        .collect();
                    if !bad.is_empty() {
                        violations.push(LeanAxiomViolation {
                            declaration: norm_decl.clone(),
                            detail: format!(
                                "`{norm_decl}` depends on non-standard axiom(s) [{}]; only {{propext, Classical.choice, Quot.sound}} are permitted",
                                bad.join(", ")
                            ),
                            non_standard_axioms: bad,
                        });
                    }
                    observed.push((norm_decl, axioms));
                    let after_close = &after_dep[close_bracket + 1..];
                    rest = after_close
                        .find('\n')
                        .map(|nl| &after_close[nl + 1..])
                        .unwrap_or("");
                    continue;
                } else {
                    violations.push(LeanAxiomViolation {
                        declaration: norm_decl.clone(),
                        detail: format!(
                            "malformed `#print axioms` output for `{norm_decl}` (unclosed `[`)"
                        ),
                        non_standard_axioms: Vec::new(),
                    });
                    break;
                }
            }
        }
        rest = trimmed
            .find('\n')
            .map(|nl| &trimmed[nl + 1..])
            .unwrap_or("");
    }

    for exp in expected_declarations {
        if !observed.iter().any(|(d, _)| d == exp) {
            violations.push(LeanAxiomViolation {
                declaration: exp.clone(),
                detail: format!(
                    "expected `#print axioms` verification line for `{exp}` was missing from Lean output"
                ),
                non_standard_axioms: Vec::new(),
            });
        }
    }

    if violations.is_empty() {
        Ok(observed)
    } else {
        Err(violations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_strings_utf8_chars_and_where_extern_hooks_fail_closed() {
        let raw_escape_attack =
            "def p : String := r#\"C:\\\"#\ntheorem pwn : False := sorry\ndef q : String := \"\"\n";
        let r1 = analyze_lean_tcb(raw_escape_attack);
        assert!(!r1.is_sound_tcb());
        assert!(r1.holes.iter().any(|h| h.kind == LeanTcbHoleKind::Sorry));

        let utf8_char_attack =
            "def a : Char := '«'\ntheorem pwn : False := sorry\ndef b : Char := '»'\n";
        let r2 = analyze_lean_tcb(utf8_char_attack);
        assert!(!r2.is_sound_tcb());
        assert!(r2.holes.iter().any(|h| h.kind == LeanTcbHoleKind::Sorry));

        let hex_char_attack =
            "def a : Char := '\\x22'\ntheorem pwn : False := sorry\ndef b : Char := '\\x22'\n";
        let r3 = analyze_lean_tcb(hex_char_attack);
        assert!(!r3.is_sound_tcb());
        assert!(r3.holes.iter().any(|h| h.kind == LeanTcbHoleKind::Sorry));

        let where_extern_attack =
            "def outer : Nat := localEvil\nwhere\n  @[extern \"c_evil\"] localEvil : Nat := 0\n";
        let r4 = analyze_lean_tcb(where_extern_attack);
        assert!(!r4.is_sound_tcb());

        let mutual_in_ns = "namespace Foo\ninductive T where\n  | mk : T\n  deriving Repr\nmutual\n  def even : Nat → Bool\n    | 0 => true\n    | n + 1 => odd n\n  def odd : Nat → Bool\n    | 0 => false\n    | n + 1 => even n\nend\ntheorem even_zero : even 0 = true := rfl\nend Foo\n";
        let r5 = analyze_lean_tcb(mutual_in_ns);
        assert!(r5.is_sound_tcb());
        let names: Vec<&str> = r5
            .declarations
            .iter()
            .map(|d| d.qualified_name.as_str())
            .collect();
        assert_eq!(names, vec!["Foo.T", "Foo.even", "Foo.odd", "Foo.even_zero"]);
        assert!(
            r4.declarations
                .iter()
                .any(|d| d.role == LeanTcbRole::ExternHook && d.qualified_name == "localEvil")
        );

        let clean = "def rawSorry : String := r#\"\\\" sorry\"#\ndef ch : Char := 'α'\ntheorem ok : 1 = 1 := rfl\n";
        assert!(analyze_lean_tcb(clean).is_sound_tcb());
    }

    #[test]
    fn declare_syntax_cat_root_escape_and_segmented_guillemets_are_handled() {
        let cat_attack = "declare_syntax_cat forged_cat\ntheorem ok : True := trivial\n";
        let cat_rep = analyze_lean_tcb(cat_attack);
        assert!(
            !cat_rep.is_sound_tcb(),
            "declare_syntax_cat must convict as MetaprogrammingEscape"
        );
        assert!(cat_rep
            .holes
            .iter()
            .any(|h| h.kind == LeanTcbHoleKind::MetaprogrammingEscape));

        let scoped_src = "namespace Outer\ndef _root_.GlobalPkg.fn : Nat := 1\ndef «Sub».«val» : Nat := 2\ndef «mutual» : Nat := 3\nend Outer\n";
        let scoped_rep = analyze_lean_tcb(scoped_src);
        assert!(scoped_rep.is_sound_tcb());
        let qnames: Vec<&str> = scoped_rep
            .declarations
            .iter()
            .map(|d| d.qualified_name.as_str())
            .collect();
        assert_eq!(
            qnames,
            vec!["GlobalPkg.fn", "Outer.Sub.val", "Outer.mutual"]
        );
        let (driver, expected) =
            render_lean_axiom_driver("DcVerifyTargetSpec", scoped_src, &scoped_rep.declarations);
        assert!(driver.contains("#print axioms GlobalPkg.fn\n"), "{driver}");
        assert!(driver.contains("#print axioms Outer.Sub.val\n"), "{driver}");
        assert!(driver.contains("#print axioms Outer.«mutual»\n"), "{driver}");
        assert_eq!(
            expected,
            vec![
                "GlobalPkg.fn".to_string(),
                "Outer.Sub.val".to_string(),
                "Outer.mutual".to_string()
            ]
        );
    }
}
