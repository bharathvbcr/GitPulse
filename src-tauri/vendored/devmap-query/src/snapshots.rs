//! Budgeted semantic snapshots (closes X16 — no arbitrary 500-symbol cap).

use devmap_extract::model::{ExtractedSymbol, Extraction, SymbolKind};
use serde::{Deserialize, Serialize};

use crate::model::Response;
use crate::{budget_take, Request};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticSnapshotSymbol {
    pub name: String,
    pub qualified_name: String,
    pub file_path: String,
    pub kind: String,
    pub is_exported: bool,
    pub is_public: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticSnapshot {
    pub file_path: String,
    pub language: String,
    pub symbols: Vec<SemanticSnapshotSymbol>,
    pub shown: u32,
    pub total: u32,
    pub truncated: bool,
}

fn is_public_symbol(sym: &ExtractedSymbol, ext: &Extraction) -> bool {
    match ext.language.as_str() {
        "python" => !sym.name.starts_with('_'),
        "rust" => sym.is_exported,
        "go" => sym.is_exported,
        "typescript" | "javascript" | "tsx" => sym.is_exported,
        _ => sym.is_exported || !sym.name.starts_with('_'),
    }
}

/// Build a budgeted semantic snapshot for one file (X16, X18 per-language public rules).
pub fn semantic_snapshot_for_file(
    ext: &Extraction,
    token_budget: u32,
    public_only: bool,
) -> SemanticSnapshot {
    let mut symbols: Vec<SemanticSnapshotSymbol> = Vec::new();
    for sym in &ext.symbols {
        if sym.kind == SymbolKind::File {
            continue;
        }
        let public = is_public_symbol(sym, ext);
        if public_only && !public {
            continue;
        }
        symbols.push(SemanticSnapshotSymbol {
            name: sym.name.clone(),
            qualified_name: sym.qualified_name.clone(),
            file_path: ext.file_path.clone(),
            kind: format!("{:?}", sym.kind),
            is_exported: sym.is_exported,
            is_public: public,
        });
    }
    symbols.sort_by(|a, b| a.name.cmp(&b.name));
    let resp: Response<SemanticSnapshotSymbol> = budget_take(symbols, token_budget, |_| 12);
    SemanticSnapshot {
        file_path: ext.file_path.clone(),
        language: ext.language.clone(),
        symbols: resp.items,
        shown: resp.shown,
        total: resp.total,
        truncated: resp.truncated,
    }
}

pub fn semantic_snapshots(
    extractions: &[Extraction],
    req: Request<String>,
) -> Response<SemanticSnapshot> {
    let path = req.query.trim();
    let mut out = Vec::new();
    for ext in extractions {
        if !path.is_empty() && ext.file_path != path {
            continue;
        }
        out.push(semantic_snapshot_for_file(ext, req.token_budget, true));
    }
    out.sort_by(|a, b| a.file_path.cmp(&b.file_path));
    let total = out.len() as u32;
    let resp = budget_take(out, req.token_budget.saturating_mul(10), |_| 50);
    Response {
        total,
        shown: resp.shown,
        hidden: total.saturating_sub(resp.shown),
        truncated: resp.truncated || resp.shown < total,
        tokens_used: resp.tokens_used,
        items: resp.items,
        resolution: resp.resolution,
    }
}

#[cfg(all(test, feature = "parse"))]
mod tests {
    use super::*;
    use devmap_extract::extract_file;

    fn symbol(name: &str, is_exported: bool) -> ExtractedSymbol {
        ExtractedSymbol {
            name: name.to_string(),
            qualified_name: format!("f::{name}"),
            kind: SymbolKind::Function,
            span: devmap_extract::model::Span {
                start_byte: 0,
                end_byte: 1,
            },
            is_exported,
            docstring: None,
            signature: None,
            parent_symbol: None,
        }
    }

    fn extraction_in(language: &str) -> Extraction {
        let mut ext = extract_file("f.py", "def x(): pass\n");
        ext.language = language.to_string();
        ext
    }

    /// Each language's public-symbol rule asserted where it *differs* from the
    /// fallback.
    ///
    /// Mutation testing deleted every arm of this match without a single
    /// failure, because the surrounding tests only used inputs where the arm
    /// and the `_` fallback happen to agree. An arm that can be deleted
    /// unnoticed is an arm no test actually pins: each case below is chosen so
    /// the arm and the fallback disagree.
    #[test]
    fn per_language_public_rules_differ_from_the_fallback() {
        // Python is name-based, not export-based: a dunder/underscore name is
        // private even when the extractor marked it exported. The fallback
        // (`is_exported || !starts_with('_')`) would call this public.
        assert!(!is_public_symbol(
            &symbol("_private", true),
            &extraction_in("python")
        ));
        assert!(is_public_symbol(
            &symbol("visible", false),
            &extraction_in("python")
        ));

        // Rust, Go and TS/JS are export-based only: an unexported name without
        // a leading underscore is private, where the fallback would call it
        // public.
        for language in ["rust", "go", "typescript", "javascript", "tsx"] {
            assert!(
                !is_public_symbol(&symbol("helper", false), &extraction_in(language)),
                "{language}: an unexported symbol must not be public merely for \
                 lacking a leading underscore"
            );
            assert!(
                is_public_symbol(&symbol("Helper", true), &extraction_in(language)),
                "{language}: an exported symbol must be public"
            );
        }

        // The fallback itself is a disjunction, and both halves matter.
        assert!(is_public_symbol(
            &symbol("_odd", true),
            &extraction_in("ruby")
        ));
        assert!(is_public_symbol(
            &symbol("plain", false),
            &extraction_in("ruby")
        ));
        assert!(!is_public_symbol(
            &symbol("_hidden", false),
            &extraction_in("ruby")
        ));
    }

    /// Path filtering and truncation accounting in `semantic_snapshots`.
    ///
    /// Mutation testing flipped every operator in this function's filter and
    /// its truncation arithmetic without a failure: `&&` to `||`, the `!` on
    /// the empty-path check deleted, `!=` to `==`, and `<` to `>`/`==`/`<=`.
    /// The filter decides *which* file a caller gets back and the arithmetic
    /// decides whether a partial answer is labelled partial — both are
    /// wrong-answer surfaces, not cosmetic ones.
    #[test]
    fn snapshot_filtering_and_truncation_accounting() {
        let a = extract_file("a.py", "def alpha(): pass\n");
        let b = extract_file("b.py", "def beta(): pass\n");
        let all = vec![a, b];

        // An empty query means "every file"; a non-empty one means exactly that
        // file. Flipping either half of the guard breaks one of these.
        let every = semantic_snapshots(
            &all,
            Request {
                query: String::new(),
                token_budget: 10_000,
                min_confidence: 0.0,
                max_depth: 1,
            },
        );
        assert_eq!(every.total, 2, "an empty query must return every file");
        assert!(
            !every.truncated,
            "a complete answer must not claim truncation"
        );

        let one = semantic_snapshots(
            &all,
            Request {
                query: "b.py".to_string(),
                token_budget: 10_000,
                min_confidence: 0.0,
                max_depth: 1,
            },
        );
        assert_eq!(one.total, 1, "a path query must select exactly that file");
        assert_eq!(one.items.len(), 1);
        assert_eq!(
            one.items[0].file_path, "b.py",
            "and it must be the right one"
        );

        // A budget too small to hold every file must report the shortfall
        // honestly: shown below total, hidden making up the difference, and
        // `truncated` set. A capped sample presented as complete is the failure
        // this pins.
        let squeezed = semantic_snapshots(
            &all,
            Request {
                query: String::new(),
                token_budget: 1,
                min_confidence: 0.0,
                max_depth: 1,
            },
        );
        assert_eq!(squeezed.total, 2);
        assert!(
            squeezed.shown < squeezed.total,
            "a 1-token budget cannot show both files"
        );
        assert!(
            squeezed.truncated,
            "an answer that dropped items must be labelled truncated"
        );
        assert_eq!(
            squeezed.hidden,
            squeezed.total - squeezed.shown,
            "hidden must account for exactly the items not shown"
        );
    }

    #[test]
    fn test_x16_budgeted_truncation_not_hard_cap() {
        // closes X16
        let mut src = String::new();
        for i in 0..600 {
            src.push_str(&format!("def sym_{i}(): pass\n"));
        }
        let ext = extract_file("big.py", &src);
        let snap = semantic_snapshot_for_file(&ext, 200, true);
        assert_eq!(snap.total, 600);
        assert!(snap.truncated);
        assert!(snap.shown < snap.total);
        assert!(snap.shown > 0);
    }

    #[test]
    fn test_x18_python_private_filtered_when_public_only() {
        // closes X18
        let ext = extract_file("mod.py", "def public_fn(): pass\ndef _private(): pass\n");
        let snap = semantic_snapshot_for_file(&ext, 2000, true);
        assert!(snap.symbols.iter().any(|s| s.name == "public_fn"));
        assert!(!snap.symbols.iter().any(|s| s.name == "_private"));
    }
}
