//! Dedicated reader and Trusted Computing Base (TCB) boundary adapter for Lean 4 (`.lean`).
//!
//! No `tree-sitter-lean` C grammar is linked in `vendor/grammars`, so Lean 4
//! rides on DevMap's tier-2 dedicated-reader seam (`fallback::scan_declarations_in`,
//! alongside `crate::markup` for HTML/CSS) and honestly reports
//! `ExtractionEngine::RegexFallback` + `ParseOutcome::Fallback` rather than
//! claiming a tree-sitter grammar ran.
//!
//! Lexical tokenization and TCB soundness boundary classification live in the
//! single canonical owner [`dc_evidence::lean_tcb`] so `dcverify`'s `.lean`
//! rigor gate and DevMap's symbol/TCB surfaces never drift.

use crate::fallback::{FallbackScan, MAX_FALLBACK_SYMBOLS};
use crate::model::{
    ExtractedCall, ExtractedImport, ExtractedReference, ExtractedSymbol, ReferenceKind, Span,
    SymbolKind,
};
pub use dc_evidence::lean_tcb::{
    byte_to_line_1based, LeanKernelMode, LeanTcbHoleKind, LeanTcbRole, LeanVerificationTier,
    MAX_LEAN_BYTES,
};
use dc_evidence::lean_tcb::{
    clamp_utf8_prefix, is_top_level_boundary, qualify_lean_name, render_lean_axiom_driver,
    strip_guillemets, tcb_hole_for_token, tokenize_lean, verify_lean_axioms_output, LeanTcbSpan,
    LeanTokKind,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Maximum non-call references emitted per `.lean` file.
pub const MAX_LEAN_REFERENCES: usize = 4_000;

/// Maximum call-shaped sites emitted per `.lean` file.
pub const MAX_LEAN_CALLS: usize = 4_000;

/// One soundness hole or TCB escape located in a Lean 4 source file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeanTcbHole {
    pub kind: LeanTcbHoleKind,
    pub token: String,
    pub enclosing_symbol: Option<String>,
    pub span: Span,
}

/// Summary of a Lean 4 declaration's TCB status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeanTcbDeclaration {
    pub qualified_name: String,
    pub keyword: String,
    pub role: LeanTcbRole,
    pub attributes: Vec<String>,
    pub is_unsafe: bool,
    pub is_partial: bool,
    pub span: Span,
}

/// Complete Trusted Computing Base boundary audit of a Lean 4 file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeanTcbReport {
    pub imports: Vec<ExtractedImport>,
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

fn to_model_span(span: LeanTcbSpan) -> Span {
    Span {
        start_byte: span.start_byte,
        end_byte: span.end_byte,
    }
}

fn is_builtin_scalar(word: &str) -> bool {
    matches!(
        word,
        "Nat"
            | "Int"
            | "Bool"
            | "String"
            | "Char"
            | "UInt8"
            | "UInt16"
            | "UInt32"
            | "UInt64"
            | "USize"
            | "Float"
            | "Unit"
            | "Empty"
            | "Option"
            | "List"
            | "Array"
            | "ByteArray"
            | "BitVec"
            | "Fin"
            | "IO"
            | "Except"
            | "StateM"
    )
}

fn decl_symbol_kind(keyword: &str) -> SymbolKind {
    match keyword {
        "structure" => SymbolKind::Struct,
        "inductive" | "coinductive" => SymbolKind::Enum,
        "class" => SymbolKind::Trait,
        "axiom" | "constant" => SymbolKind::Variable,
        _ => SymbolKind::Function,
    }
}

/// Dedicated tier-2 fallback reader invoked by [`crate::fallback::scan_declarations_in`].
pub fn scan_lean(_file_path: &str, source: &str) -> FallbackScan {
    let (bounded, unread_bytes) = clamp_utf8_prefix(source, MAX_LEAN_BYTES);
    let (tokens, _lexical_holes) = tokenize_lean(bounded);

    let mut symbols: Vec<ExtractedSymbol> = Vec::new();
    let mut truncated = 0usize;
    let mut references: Vec<ExtractedReference> = Vec::new();
    let mut calls: Vec<ExtractedCall> = Vec::new();
    let mut truncated_calls = 0usize;

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

    let mut pending_private = false;
    let mut decl_start_override: Option<usize> = None;

    let mut idx = 0usize;
    while idx < tokens.len() {
        let tok = &tokens[idx];

        if tok.text == "@[" {
            if decl_start_override.is_none() {
                decl_start_override = Some(tok.start);
            }
            idx += 1;
            let mut bracket_depth = 1usize;
            while idx < tokens.len() && bracket_depth > 0 {
                if tokens[idx].text == "[" || tokens[idx].text == "@[" {
                    bracket_depth += 1;
                } else if tokens[idx].text == "]" {
                    bracket_depth -= 1;
                }
                idx += 1;
            }
            continue;
        }

        if matches!(
            tok.text,
            "private" | "protected" | "noncomputable" | "partial" | "unsafe" | "scoped" | "local"
        ) {
            if decl_start_override.is_none() {
                decl_start_override = Some(tok.start);
            }
            if tok.text == "private" {
                pending_private = true;
            }
            idx += 1;
            continue;
        }

        if tok.text == "import" {
            idx += 1;
            if idx < tokens.len() && tokens[idx].text == "all" {
                idx += 1;
            }
            while idx < tokens.len()
                && tokens[idx].kind == LeanTokKind::Ident
                && !tokens[idx].line_first
            {
                idx += 1;
            }
            pending_private = false;
            decl_start_override = None;
            continue;
        }

        if tok.text == "namespace" || tok.text == "section" || tok.text == "mutual" {
            let is_ns = tok.text == "namespace";
            let is_mutual = tok.text == "mutual";
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
            scope_stack.push(ScopeEntry {
                is_namespace: is_ns,
                parts,
            });
            pending_private = false;
            decl_start_override = None;
            continue;
        }

        if tok.text == "end" {
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
            pending_private = false;
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
        ) {
            let kw = tok.text;
            let decl_start = decl_start_override.unwrap_or(tok.start);
            idx += 1;

            let raw_name = if idx < tokens.len() && tokens[idx].kind == LeanTokKind::Ident {
                let n = tokens[idx].text;
                idx += 1;
                n.to_string()
            } else if kw == "instance" {
                format!("inst_{decl_start}")
            } else {
                pending_private = false;
                decl_start_override = None;
                continue;
            };

            let ns = active_ns(&scope_stack);
            let parent_symbol = if ns.is_empty() {
                None
            } else {
                Some(ns.join("."))
            };
            let (short_name, qualified_name) = qualify_lean_name(&ns, &raw_name);

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

            if symbols.len() < MAX_FALLBACK_SYMBOLS {
                symbols.push(ExtractedSymbol {
                    name: short_name,
                    qualified_name: qualified_name.clone(),
                    kind: decl_symbol_kind(kw),
                    span: Span {
                        start_byte: decl_start,
                        end_byte: decl_end,
                    },
                    is_exported: !pending_private,
                    docstring: None,
                    signature: Some(kw.to_string()),
                    parent_symbol,
                    body_signature: None,
                    declaration_hash: None,
                    return_type: None,
                });
            } else {
                truncated += 1;
            }

            let mut in_extends = false;
            let mut in_body = false;
            let mut local_params: BTreeSet<&str> = BTreeSet::new();
            let mut bi = 0usize;
            while bi < body_tokens.len() {
                let bt = &body_tokens[bi];

                if !in_body && matches!(bt.text, ":=" | "where" | "|") {
                    if bt.text != "where" || !in_extends {
                        in_body = true;
                    }
                    in_extends = false;
                    bi += 1;
                    continue;
                }

                if !in_body && bt.text == "extends" && matches!(kw, "structure" | "class") {
                    in_extends = true;
                    bi += 1;
                    continue;
                }

                if in_extends {
                    if matches!(bt.text, "where" | ":=" | "deriving" | "(" | "[" | "{") {
                        in_extends = false;
                        if bt.text == "where" || bt.text == ":=" {
                            in_body = true;
                        }
                    } else if bt.kind == LeanTokKind::Ident && !is_builtin_scalar(bt.text) {
                        let base_name = bt.text.rsplit('.').next().unwrap_or(bt.text);
                        if references.len() < MAX_LEAN_REFERENCES {
                            references.push(ExtractedReference {
                                name: strip_guillemets(base_name).to_string(),
                                kind: ReferenceKind::Heritage,
                                span: Span {
                                    start_byte: bt.start,
                                    end_byte: bt.end,
                                },
                                enclosing_symbol: Some(qualified_name.clone()),
                                assigned_to: None,
                                receiver_expr: None,
                            });
                        }
                        bi += 1;
                        while bi < body_tokens.len()
                            && !matches!(body_tokens[bi].text, "," | "where" | ":=" | "deriving")
                        {
                            bi += 1;
                        }
                        continue;
                    }
                }

                if !in_body
                    && matches!(bt.text, "(" | "{" | "[")
                    && bi + 2 < body_tokens.len()
                    && body_tokens[bi + 1].kind == LeanTokKind::Ident
                {
                    let mut look = bi + 1;
                    let mut candidates = Vec::new();
                    while look < body_tokens.len() && body_tokens[look].kind == LeanTokKind::Ident {
                        candidates.push(body_tokens[look].text);
                        look += 1;
                    }
                    if look < body_tokens.len() && body_tokens[look].text == ":" {
                        for c in candidates {
                            if !c.contains('.') {
                                local_params.insert(c);
                            }
                        }
                    }
                }

                if !in_body && bt.text == ":" && bi + 1 < body_tokens.len() {
                    let mut ti = bi + 1;
                    while ti < body_tokens.len()
                        && !matches!(
                            body_tokens[ti].text,
                            ":=" | "where" | ")" | "}" | "]" | "extends"
                        )
                    {
                        let tt = &body_tokens[ti];
                        if tt.kind == LeanTokKind::Ident
                            && !local_params.contains(tt.text)
                            && !is_builtin_scalar(tt.text)
                            && tcb_hole_for_token(tt.text).is_none()
                        {
                            let (receiver_expr, bare_str) = match tt.text.rsplit_once('.') {
                                Some((recv, leaf)) => (
                                    Some(strip_guillemets(recv).to_string()),
                                    strip_guillemets(leaf).to_string(),
                                ),
                                None => (None, strip_guillemets(tt.text).to_string()),
                            };
                            if bare_str.chars().next().is_some_and(|c| c.is_uppercase()) {
                                if references.len() < MAX_LEAN_REFERENCES {
                                    references.push(ExtractedReference {
                                        name: bare_str,
                                        kind: ReferenceKind::Type,
                                        span: Span {
                                            start_byte: tt.start,
                                            end_byte: tt.end,
                                        },
                                        enclosing_symbol: Some(qualified_name.clone()),
                                        assigned_to: None,
                                        receiver_expr,
                                    });
                                }
                            } else {
                                let followed_by_arg = ti + 1 < body_tokens.len()
                                    && !body_tokens[ti + 1].line_first
                                    && (body_tokens[ti + 1].kind == LeanTokKind::Ident
                                        || matches!(body_tokens[ti + 1].text, "("));
                                if followed_by_arg {
                                    if calls.len() < MAX_LEAN_CALLS {
                                        calls.push(ExtractedCall {
                                            caller_symbol: Some(qualified_name.clone()),
                                            callee_name: bare_str,
                                            receiver_expr,
                                            span: Span {
                                                start_byte: tt.start,
                                                end_byte: tt.end,
                                            },
                                        });
                                    } else {
                                        truncated_calls += 1;
                                    }
                                } else if references.len() < MAX_LEAN_REFERENCES {
                                    references.push(ExtractedReference {
                                        name: bare_str,
                                        kind: ReferenceKind::Name,
                                        span: Span {
                                            start_byte: tt.start,
                                            end_byte: tt.end,
                                        },
                                        enclosing_symbol: Some(qualified_name.clone()),
                                        assigned_to: None,
                                        receiver_expr,
                                    });
                                }
                            }
                        }
                        ti += 1;
                    }
                }

                if in_body
                    && bt.kind == LeanTokKind::Ident
                    && !local_params.contains(bt.text)
                    && !is_builtin_scalar(bt.text)
                    && tcb_hole_for_token(bt.text).is_none()
                {
                    let is_field_lhs = matches!(kw, "structure" | "class")
                        && bi + 1 < body_tokens.len()
                        && body_tokens[bi + 1].text == ":";
                    if !is_field_lhs {
                        let (receiver_expr, callee_bare) = match bt.text.rsplit_once('.') {
                            Some((recv, leaf)) => (
                                Some(strip_guillemets(recv).to_string()),
                                strip_guillemets(leaf).to_string(),
                            ),
                            None => (None, strip_guillemets(bt.text).to_string()),
                        };
                        let followed_by_arg = bi + 1 < body_tokens.len()
                            && !body_tokens[bi + 1].line_first
                            && (body_tokens[bi + 1].kind == LeanTokKind::Ident
                                || matches!(body_tokens[bi + 1].text, "("));
                        if followed_by_arg {
                            if calls.len() < MAX_LEAN_CALLS {
                                calls.push(ExtractedCall {
                                    caller_symbol: Some(qualified_name.clone()),
                                    callee_name: callee_bare,
                                    receiver_expr,
                                    span: Span {
                                        start_byte: bt.start,
                                        end_byte: bt.end,
                                    },
                                });
                            } else {
                                truncated_calls += 1;
                            }
                        } else if references.len() < MAX_LEAN_REFERENCES {
                            references.push(ExtractedReference {
                                name: callee_bare,
                                kind: ReferenceKind::Name,
                                span: Span {
                                    start_byte: bt.start,
                                    end_byte: bt.end,
                                },
                                enclosing_symbol: Some(qualified_name.clone()),
                                assigned_to: None,
                                receiver_expr,
                            });
                        }
                    }
                }

                bi += 1;
            }

            pending_private = false;
            decl_start_override = None;
            continue;
        }

        pending_private = false;
        decl_start_override = None;
        idx += 1;
    }

    FallbackScan {
        symbols,
        references,
        truncated,
        skipped_long_lines: 0,
        unread_bytes,
        calls,
        truncated_calls,
    }
}

/// Audit a Lean 4 source file's Trusted Computing Base (TCB) boundaries by
/// delegating to the canonical [`dc_evidence::lean_tcb::analyze_lean_tcb`] engine.
pub fn analyze_lean_tcb(source: &str) -> LeanTcbReport {
    let raw = dc_evidence::lean_tcb::analyze_lean_tcb(source);
    LeanTcbReport {
        imports: raw
            .imports
            .into_iter()
            .map(|imp| ExtractedImport {
                raw_import: imp.raw_import,
                module_specifier: imp.module_specifier,
                imported_names: Vec::new(),
                local_names: Vec::new(),
                alias: None,
                span: to_model_span(imp.span),
                path_load: None,
            })
            .collect(),
        declarations: raw
            .declarations
            .into_iter()
            .map(|d| LeanTcbDeclaration {
                qualified_name: d.qualified_name,
                keyword: d.keyword,
                role: d.role,
                attributes: d.attributes,
                is_unsafe: d.is_unsafe,
                is_partial: d.is_partial,
                span: to_model_span(d.span),
            })
            .collect(),
        holes: raw
            .holes
            .into_iter()
            .map(|h| LeanTcbHole {
                kind: h.kind,
                token: h.token,
                enclosing_symbol: h.enclosing_symbol,
                span: to_model_span(h.span),
            })
            .collect(),
        unread_bytes: raw.unread_bytes,
    }
}

fn map_proc_tier(t: dc_proc::lean_kernel::LeanKernelTier) -> LeanVerificationTier {
    match t {
        dc_proc::lean_kernel::LeanKernelTier::LexicalOnly => LeanVerificationTier::LexicalOnly,
        dc_proc::lean_kernel::LeanKernelTier::LeanKernel => LeanVerificationTier::LeanKernel,
        dc_proc::lean_kernel::LeanKernelTier::KernelAndLean4checker => {
            LeanVerificationTier::KernelAndLean4checker
        }
    }
}

/// Probe external `lean` + `lean4checker` availability outside `repo_root`.
pub fn probe_lean_kernel_toolchain(
    repo_root: &std::path::Path,
    mode: LeanKernelMode,
) -> Result<(LeanVerificationTier, Option<String>), String> {
    if mode == LeanKernelMode::Off {
        return Ok((LeanVerificationTier::LexicalOnly, None));
    }
    dc_proc::lean_kernel::probe_lean_toolchain(repo_root, mode == LeanKernelMode::Require)
        .map(|(tier, note)| (map_proc_tier(tier), note))
        .map_err(|fail| match fail {
            dc_proc::lean_kernel::LeanKernelFailure::Unavailable(msg)
            | dc_proc::lean_kernel::LeanKernelFailure::KernelRejected(msg)
            | dc_proc::lean_kernel::LeanKernelFailure::Lean4CheckerRejected(msg) => msg,
        })
}

/// Audit a `.lean` file lexically AND (when `mode != Off`) replay it through `lean`,
/// `#print axioms`, and `lean4checker --fresh` in an isolated scratch directory.
pub fn analyze_lean_tcb_with_kernel(
    repo_root: &std::path::Path,
    spec_path: Option<&std::path::Path>,
    source: &str,
    mode: LeanKernelMode,
) -> (LeanTcbReport, LeanVerificationTier, Option<String>) {
    let raw = dc_evidence::lean_tcb::analyze_lean_tcb(source);
    let mut holes: Vec<LeanTcbHole> = raw
        .holes
        .iter()
        .map(|h| LeanTcbHole {
            kind: h.kind,
            token: h.token.clone(),
            enclosing_symbol: h.enclosing_symbol.clone(),
            span: to_model_span(h.span),
        })
        .collect();

    let (tier, note) = if mode == LeanKernelMode::Off {
        (LeanVerificationTier::LexicalOnly, None)
    } else {
        let (driver, expected_decls) = render_lean_axiom_driver(
            dc_proc::lean_kernel::target_module_name(),
            source,
            &raw.declarations,
        );
        let outcome = dc_proc::lean_kernel::run_lean_kernel_check(
            repo_root,
            spec_path,
            source,
            &driver,
            mode == LeanKernelMode::Require,
            dc_proc::lean_kernel::configured_lean_kernel_timeout(),
        );
        if let Some(stdout) = &outcome.print_axioms_stdout {
            if let Err(violations) = verify_lean_axioms_output(stdout, &expected_decls) {
                for v in violations {
                    let decl_span = raw
                        .declarations
                        .iter()
                        .find(|d| d.qualified_name == v.declaration)
                        .map(|d| to_model_span(d.span))
                        .unwrap_or(Span {
                            start_byte: 0,
                            end_byte: 0,
                        });
                    let tok = if v.non_standard_axioms.is_empty() {
                        v.detail
                    } else {
                        v.non_standard_axioms.join(", ")
                    };
                    holes.push(LeanTcbHole {
                        kind: LeanTcbHoleKind::NonStandardAxiom,
                        token: tok,
                        enclosing_symbol: Some(v.declaration),
                        span: decl_span,
                    });
                }
            }
        }
        if let Some(fail) = outcome.failure {
            let (kind, msg) = match fail {
                dc_proc::lean_kernel::LeanKernelFailure::Unavailable(m) => {
                    (LeanTcbHoleKind::KernelUnavailable, m)
                }
                dc_proc::lean_kernel::LeanKernelFailure::KernelRejected(m) => {
                    (LeanTcbHoleKind::KernelRejected, m)
                }
                dc_proc::lean_kernel::LeanKernelFailure::Lean4CheckerRejected(m) => {
                    (LeanTcbHoleKind::Lean4CheckerRejected, m)
                }
            };
            holes.push(LeanTcbHole {
                kind,
                token: msg,
                enclosing_symbol: None,
                span: Span {
                    start_byte: 0,
                    end_byte: 0,
                },
            });
        }
        (map_proc_tier(outcome.tier), outcome.note)
    };

    let report = LeanTcbReport {
        imports: raw
            .imports
            .into_iter()
            .map(|imp| ExtractedImport {
                raw_import: imp.raw_import,
                module_specifier: imp.module_specifier,
                imported_names: Vec::new(),
                local_names: Vec::new(),
                alias: None,
                span: to_model_span(imp.span),
                path_load: None,
            })
            .collect(),
        declarations: raw
            .declarations
            .into_iter()
            .map(|d| LeanTcbDeclaration {
                qualified_name: d.qualified_name,
                keyword: d.keyword,
                role: d.role,
                attributes: d.attributes,
                is_unsafe: d.is_unsafe,
                is_partial: d.is_partial,
                span: to_model_span(d.span),
            })
            .collect(),
        holes,
        unread_bytes: raw.unread_bytes,
    };
    (report, tier, note)
}
