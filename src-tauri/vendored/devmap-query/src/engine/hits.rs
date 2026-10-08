use super::{BYTES_PER_TOKEN, SEARCH_HIT_OVERHEAD_TOKENS};
use crate::model::{LiteralSite, SkeletonSymbol, SymbolHit};
use devmap_extract::model::Span;
use devmap_store::Store;
use std::path::Path;

/// Token cost of one search hit: its source span plus a fixed per-row overhead.
///
/// Shared by keyword and semantic search so the two spend the budget at the
/// same rate; two copies of this arithmetic would let the same result cost
/// different amounts depending on which command asked for it.
/// Share of an evidence pack's budget held for its related-test list: a
/// quarter, so the hits keep most of the budget and a few test files still
/// fit beside them.
pub const EVIDENCE_TEST_BUDGET_SHARE: u32 = 4;

/// Inbound depth of the related-test walk: the `devmap affected` default.
pub const EVIDENCE_TEST_DEPTH: usize = 3;

pub(crate) fn search_hit_tokens(hit: &SymbolHit) -> u32 {
    u32::try_from(hit.source_span.len() / BYTES_PER_TOKEN as usize)
        .unwrap_or(u32::MAX)
        .saturating_add(SEARCH_HIT_OVERHEAD_TOKENS)
}

/// Token cost of one skeleton row: the signature text (or the "not extracted"
/// note) plus a fixed overhead for the name and span fields.
pub(super) fn skeleton_symbol_tokens(item: &SkeletonSymbol) -> u32 {
    let body = item
        .signature
        .as_deref()
        .or(item.signature_note.as_deref())
        .unwrap_or("");
    let bytes = item.qualified_name.len() + item.kind.len() + body.len() + 24;
    u32::try_from(bytes / BYTES_PER_TOKEN as usize)
        .unwrap_or(u32::MAX)
        .saturating_add(8)
}

/// Repository-relative path spelling the store uses as a key.
///
/// Absolute paths under the indexed root, Windows separators and a leading
/// `./` all reach the same row; without this a Cursor absolute `file_path`
/// would answer "not in index" for a file the generation holds under a
/// relative key.
pub(super) fn resolve_skeleton_path(store: &Store, path: &str) -> anyhow::Result<String> {
    let unified = path.trim().replace('\\', "/");
    let stripped = unified.strip_prefix("./").unwrap_or(&unified);
    let raw = Path::new(stripped);
    if raw.is_absolute() {
        if let Some(root) = store.latest_repo_root()? {
            let root = Path::new(&root);
            if let Ok(relative) = raw.strip_prefix(root) {
                return Ok(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    Ok(stripped.to_string())
}

#[cfg(test)]
thread_local! {
    /// Source-span reads attempted on this thread.
    ///
    /// Reading a file per scored symbol is the cost `search_semantic` used to
    /// pay for the entire corpus before the budget was applied, and "how many
    /// files did this query open" is not observable from the response. Counted
    /// per thread rather than globally so tests running in parallel in one
    /// binary cannot contaminate each other's count.
    pub(crate) static SOURCE_SPAN_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };

    /// Bytes those reads pulled off disk on this thread.
    ///
    /// The count above says how many files were opened; it says nothing about
    /// how much of each was read, and `read_to_string` read all of it. A 50 MB
    /// vendored bundle answered a one-line span with 50 MB of I/O and 50 MB of
    /// resident string, per hit, and neither the response nor the read counter
    /// showed it.
    pub(crate) static SOURCE_SPAN_BYTES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Verify the complete source identity before applying stored byte coordinates.
/// A bounded prefix alone can belong to a different file revision. Reads stay
/// within discovery's source ceiling and only cover files selected for hits.
pub(super) fn read_verified_source(
    repo_root: Option<&str>,
    path: &str,
    span: std::ops::Range<usize>,
    expected_hash: u64,
) -> std::io::Result<String> {
    let source = devmap_extract::safe_fs::read_repo_source(
        repo_root.map(std::path::Path::new),
        path,
        devmap_extract::MAX_SOURCE_BYTES,
    )?;
    #[cfg(test)]
    SOURCE_SPAN_BYTES.with(|bytes| bytes.set(bytes.get().saturating_add(source.len() as u64)));
    if devmap_extract::content_hash(&source) != expected_hash {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "source changed since this symbol was indexed",
        ));
    }
    if source.get(span).is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "stored span is invalid for the verified source",
        ));
    }
    Ok(source)
}

/// Build a hit from a stored symbol row, reading its source span from disk.
///
/// One owner for the read, the line-range conversion, the span cap and the
/// unavailability reason. When the file cannot be read the reason is recorded
/// on the hit rather than dropped, so an empty `source_span` is never mistaken
/// for a symbol with no body.
pub(super) fn hit_from_stored(
    row: devmap_store::StoredSymbol,
    repo_root: Option<&str>,
    token_budget: u32,
    score: f32,
) -> SymbolHit {
    #[cfg(test)]
    SOURCE_SPAN_READS.with(|reads| reads.set(reads.get().saturating_add(1)));
    let source_result = read_verified_source(
        repo_root,
        &row.path,
        row.span_start..row.span_end,
        row.content_hash,
    );
    let source_unavailable_reason = source_result.as_ref().err().map(|error| {
        format!(
            "source unavailable at query time for {:?}: {error}",
            row.path
        )
    });
    let source = source_result.ok();
    let source_span = source
        .as_deref()
        .and_then(|text| text.get(row.span_start..row.span_end))
        .unwrap_or("")
        .to_string();
    let span = source
        .as_deref()
        .map(|text| {
            Span {
                start_byte: row.span_start,
                end_byte: row.span_end,
            }
            .line_range(text)
        })
        .unwrap_or((0, 0));
    let source_indent = source
        .as_deref()
        .and_then(|text| line_indent_before(text, row.span_start));
    let (source_span, source_span_omitted_bytes) = cap_source_span(source_span, token_budget);
    SymbolHit {
        symbol_name: row.name,
        file_path: row.path,
        kind: row.kind,
        span,
        source_span,
        source_unavailable_reason,
        source_span_omitted_bytes,
        source_indent,
        score,
    }
}

/// Longest indentation [`SymbolHit::source_indent`] carries. Past this the
/// prefix is not indentation anyone reads, and it is not worth its bytes.
pub(super) const MAX_SOURCE_INDENT: usize = 256;

/// The spaces and tabs between the start of `offset`'s line and `offset`, or
/// `None` at column zero or when anything else precedes it on the line.
pub(super) fn line_indent_before(text: &str, offset: usize) -> Option<String> {
    let before = text.get(..offset)?;
    let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
    let prefix = &before[line_start..];
    (!prefix.is_empty()
        && prefix.len() <= MAX_SOURCE_INDENT
        && prefix.bytes().all(|byte| byte == b' ' || byte == b'\t'))
    .then(|| prefix.to_string())
}

/// Cap a hit's source span so one hit can never exceed the whole token budget.
///
/// A search hit costs `source_span.len() / 4 + 20` tokens, and `source_span` is
/// the symbol's entire body. One 8 KB function therefore outweighed the 2,000
/// token default on its own, and the caller enforces the budget as a hard
/// contract — `DevMapClient._budgeted` raises on an over-budget response — so
/// an uncapped hit is not merely large, it is unreturnable. `devmap search
/// "resolve calls"` on this repository matched exactly one symbol,
/// `resolve_calls`, and answered with nothing.
///
/// Returns the (possibly capped) span and the number of bytes dropped, which
/// the caller records in `source_span_omitted_bytes`. A capped span is never
/// passed off as the verbatim body R2 promises.
/// Largest share of a request's budget one hit's source span may take.
///
/// Capping at the *whole* budget — which this did — is enough to keep a single
/// oversized item from being withheld, but it lets that item crowd out every
/// other result. A `File` symbol's span is its entire file, so a search whose
/// best matches are files returned two hits against a 4,000-token budget and
/// reported 512 more withheld. A quarter guarantees at least three results
/// survive alongside any one of them.
pub(super) const MAX_HIT_BUDGET_SHARE: u32 = 4;

pub(super) fn cap_source_span(source_span: String, token_budget: u32) -> (String, Option<u32>) {
    let max_bytes = (token_budget / MAX_HIT_BUDGET_SHARE)
        .saturating_sub(SEARCH_HIT_OVERHEAD_TOKENS)
        .saturating_mul(BYTES_PER_TOKEN) as usize;
    if source_span.len() <= max_bytes {
        return (source_span, None);
    }
    // Truncate on a char boundary: `String` slicing panics mid-codepoint, and
    // source files contain non-ASCII in strings, comments and identifiers.
    let mut end = max_bytes;
    while end > 0 && !source_span.is_char_boundary(end) {
        end -= 1;
    }
    let omitted = u32::try_from(source_span.len() - end).unwrap_or(u32::MAX);
    let mut capped = source_span;
    capped.truncate(end);
    (capped, Some(omitted))
}

/// Rows loaded for one literal query before the token budget is applied.
pub(super) const LITERAL_PAGE_CAP: usize = 1000;

pub(super) fn scope_report_from_narrowing(
    narrowing: &devmap_store::SearchNarrowing,
    analysis: Option<&devmap_analyze::AnalysisDisclosure>,
) -> crate::scope::ScopeReport {
    let corpus_files = match analysis {
        Some(disclosure) if disclosure.total_files > 0 => {
            u32::try_from(disclosure.total_files).unwrap_or(u32::MAX)
        }
        _ => narrowing.corpus_files,
    };
    let corpus_symbols = match analysis {
        Some(disclosure) if disclosure.total_symbols > 0 => {
            u32::try_from(disclosure.total_symbols).unwrap_or(u32::MAX)
        }
        _ => narrowing.corpus_symbols,
    };
    crate::scope::ScopeReport {
        paths: narrowing.paths.clone(),
        languages: narrowing.languages.clone(),
        kinds: narrowing.kinds.clone(),
        files: narrowing.files,
        symbols: narrowing.symbols,
        corpus_files,
        corpus_symbols,
        related_tests_outside_scope: 0,
    }
}

pub(super) fn literal_site_tokens(site: &LiteralSite) -> u32 {
    let bytes = site.file_path.len()
        + site.value.len()
        + site.qualified_name.len()
        + site.symbol_name.len()
        + 8;
    u32::try_from(bytes / 4).unwrap_or(u32::MAX).max(1)
}
