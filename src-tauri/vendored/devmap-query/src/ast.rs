//! Structural symbol search: name, kind and language, from the parsed index.
//!
//! Ported from `indexing/ast_matcher.py`, which walks the working tree and
//! matches with `ast.parse` for Python and four hand-written regexes for
//! TypeScript, JavaScript, Go and Rust. Everything else it cannot see.
//!
//! This answers the same question from the committed generation, so it covers
//! every language the kernel parses rather than four, and a hit is a symbol the
//! extractor produced rather than a line that matched a pattern.
//!
//! It also fixes the defect that module's own comment admits: `_engine()`
//! reports `"tree-sitter-optional"` whenever tree-sitter is *installed*, even
//! for a file that fell back to regex — so a pattern-matched symbol and a
//! parsed one arrive under the same label. Here the engine is read per result
//! from the file's recorded extraction outcome.

use std::collections::BTreeMap;

use devmap_store::{Store, StoredSymbol};
use serde_json::{json, Value};

/// What to match.
#[derive(Debug, Clone, Default)]
pub struct AstFilter {
    /// Case-insensitive substring of the name or qualified name. Empty matches all.
    pub query: String,
    /// Exact symbol kind, case-insensitive.
    pub kind: Option<String>,
    /// Exact language id, case-insensitive.
    pub language: Option<String>,
    /// Rows returned. The total is reported whatever this is.
    pub limit: usize,
}

fn language_of(path: &str) -> &'static str {
    devmap_extract::languages::detect_language(std::path::Path::new(path))
}

fn matches(symbol: &StoredSymbol, filter: &AstFilter, query: &str) -> bool {
    if let Some(kind) = &filter.kind {
        if !symbol.kind.eq_ignore_ascii_case(kind) {
            return false;
        }
    }
    if let Some(language) = &filter.language {
        if !language_of(&symbol.path).eq_ignore_ascii_case(language) {
            return false;
        }
    }
    if query.is_empty() {
        return true;
    }
    symbol.name.to_lowercase().contains(query)
        || symbol.qualified_name.to_lowercase().contains(query)
}

/// Answer a structural query against the committed generation.
///
/// The whole symbol table is filtered before anything is cut, so `total` is
/// exact rather than a page length. The Python returns `matches[:limit]` with
/// no total at all, which makes a truncated answer and a complete one identical
/// to the caller.
pub fn ast_query(store: &Store, filter: &AstFilter) -> anyhow::Result<Value> {
    let limit = filter.limit.max(1);
    let query = filter.query.to_lowercase();
    let symbols = store.all_symbols()?;
    let corpus = symbols.len();

    let mut hits: Vec<&StoredSymbol> = symbols
        .iter()
        .filter(|symbol| matches(symbol, filter, &query))
        .collect();
    hits.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.span_start.cmp(&right.span_start))
            .then_with(|| left.name.cmp(&right.name))
    });
    let total = hits.len();
    hits.truncate(limit);

    // Only for the rows actually returned, and once per file rather than once
    // per symbol.
    let mut engines: BTreeMap<&str, Value> = BTreeMap::new();
    for hit in &hits {
        if engines.contains_key(hit.path.as_str()) {
            continue;
        }
        let file = store.latest_file(&hit.path)?;
        engines.insert(
            hit.path.as_str(),
            match file {
                Some(file) => json!({
                    "engine": format!("{:?}", file.engine),
                    "parse_outcome": format!("{:?}", file.parse_outcome),
                }),
                // The symbol is in the generation and its file row is not. Say
                // so rather than defaulting to the parsed label.
                None => json!({"engine": Value::Null, "parse_outcome": Value::Null}),
            },
        );
    }

    let rows: Vec<Value> = hits
        .iter()
        .map(|hit| {
            let provenance = &engines[hit.path.as_str()];
            json!({
                "path": hit.path,
                "language": language_of(&hit.path),
                "kind": hit.kind,
                "name": hit.name,
                "qualified_name": hit.qualified_name,
                "span_start": hit.span_start,
                "span_end": hit.span_end,
                "exported": hit.is_exported,
                // The file's real extraction outcome, not whether a parser
                // happened to be installed.
                "engine": provenance["engine"],
                "parse_outcome": provenance["parse_outcome"],
            })
        })
        .collect();

    // A filter naming a kind or language nothing carries returns the same empty
    // list as a query that genuinely matches nothing. They call for different
    // reactions — fix the filter, or accept the answer — so they are told apart.
    let mut unmatched_filters = Vec::new();
    if total == 0 {
        if let Some(kind) = &filter.kind {
            if !symbols.iter().any(|s| s.kind.eq_ignore_ascii_case(kind)) {
                unmatched_filters.push(json!({
                    "filter": "kind", "value": kind,
                    "detail": "no symbol in this generation has that kind",
                }));
            }
        }
        if let Some(language) = &filter.language {
            if !symbols
                .iter()
                .any(|s| language_of(&s.path).eq_ignore_ascii_case(language))
            {
                unmatched_filters.push(json!({
                    "filter": "language", "value": language,
                    "detail": "no file in this generation is that language",
                }));
            }
        }
    }

    Ok(json!({
        "matches": rows,
        "shown": rows.len(),
        "total": total,
        "hidden": total.saturating_sub(rows.len()),
        "truncated": total > rows.len(),
        "corpus_symbols": corpus,
        "unmatched_filters": unmatched_filters,
    }))
}

/// Kinds and languages this generation actually holds.
///
/// So a caller can pick a filter that exists rather than guess one.
pub fn ast_facets(store: &Store) -> anyhow::Result<Value> {
    let symbols = store.all_symbols()?;
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut languages: BTreeMap<&str, usize> = BTreeMap::new();
    for symbol in &symbols {
        *kinds.entry(symbol.kind.clone()).or_default() += 1;
        *languages.entry(language_of(&symbol.path)).or_default() += 1;
    }
    Ok(json!({
        "kinds": kinds,
        "languages": languages,
        "corpus_symbols": symbols.len(),
    }))
}
