//! Exact traversal starts for impact/trace.
//!
//! Substring `contains` on symbol *or* file path made `impact "e"` saturate the
//! graph and report Available. Matching is exact: a qualified `file::symbol`,
//! a path, or a symbol name (including `Type.method`).

pub fn traversal_start_matches(query: &str, symbol: &str, file: &str) -> bool {
    match classify(query) {
        StartQuery::Nothing => false,
        StartQuery::Qualified {
            file: wanted_file,
            symbol: wanted_symbol,
        } => path_matches(file, wanted_file) && symbol_matches(symbol, wanted_symbol),
        StartQuery::Path(path) => path_matches(file, path),
        StartQuery::Symbol(name) => symbol_matches(symbol, name),
    }
}

/// What a traversal-start query is asking about.
///
/// The classification is separated from the per-edge test because an index can
/// answer the three shapes very differently — a symbol query needs only the
/// distinct symbols, a path query only the distinct files — and a second
/// hand-written copy of "is this a path or a name" would be a second thing to
/// keep in step with [`traversal_start_matches`]. There is one classifier, and
/// both the scan and the index go through it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartQuery<'a> {
    /// `path::symbol` — both halves must match.
    Qualified { file: &'a str, symbol: &'a str },
    /// A file path.
    Path(&'a str),
    /// A symbol name, possibly `Type.method`.
    Symbol(&'a str),
    /// Empty or blank: matches nothing, rather than everything.
    Nothing,
}

pub fn classify(query: &str) -> StartQuery<'_> {
    let query = query.trim();
    if query.is_empty() {
        return StartQuery::Nothing;
    }
    if let Some((file, symbol)) = split_qualified(query) {
        return StartQuery::Qualified { file, symbol };
    }
    if looks_like_path(query) {
        return StartQuery::Path(query);
    }
    StartQuery::Symbol(query)
}

fn split_qualified(query: &str) -> Option<(&str, &str)> {
    query
        .rsplit_once("::")
        .filter(|(file, symbol)| !file.is_empty() && !symbol.is_empty())
}

fn looks_like_path(query: &str) -> bool {
    query.contains('/')
        || query.contains('\\')
        || (query.contains('.') && has_source_extension(query))
}

fn has_source_extension(query: &str) -> bool {
    matches!(
        query.rsplit('.').next().unwrap_or(""),
        "go" | "py"
            | "rs"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "c"
            | "h"
            | "cc"
            | "cpp"
            | "cs"
            | "java"
            | "kt"
            | "swift"
            | "rb"
            | "php"
            | "vue"
            | "svelte"
    )
}

/// `file` names `query`, exactly or as a segment-aligned suffix, with `\\` and
/// `/` treated as the same separator.
///
/// Byte-wise rather than `file.replace('\\', "/") == query.replace(...) ||
/// file.ends_with(&format!("/{query}"))`. That spelling allocated to answer a
/// boolean — measured at four heap operations per call — and
/// `traversal_starts` calls this once per edge in the generation, so one
/// `impact` on a 660,000-edge store discarded ~2.6M of them. Over that store
/// the scan cost 37.3 ms and costs 3.4 ms here; see
/// `examples/query_phase_ab.rs`, hypothesis H1.
///
/// Equivalent by construction, not by resemblance: `/` (0x2F) and `\\` (0x5C)
/// are ASCII, so they never occur inside a multi-byte UTF-8 sequence, and
/// folding one to the other is a length-preserving byte map. Comparing the
/// folded bytes is therefore the same predicate as comparing the folded
/// strings. `path_matching_agrees_with_the_spelling_it_replaced`, below,
/// differential-tests it against that spelling, and
/// `tests/query_work_is_bounded_by_the_answer.rs` pins the allocation count.
pub fn path_matches(file: &str, query: &str) -> bool {
    let file = file.as_bytes();
    let query = query.as_bytes();
    if separator_insensitive_eq(file, query) {
        return true;
    }
    // The `ends_with("/{query}")` half: the separator has to be there too, so
    // `abc.go` does not match a query of `bc.go`.
    match file.len().checked_sub(query.len() + 1) {
        Some(boundary) => {
            as_separator(file[boundary]) == b'/'
                && separator_insensitive_eq(&file[boundary + 1..], query)
        }
        None => false,
    }
}

fn as_separator(byte: u8) -> u8 {
    if byte == b'\\' {
        b'/'
    } else {
        byte
    }
}

fn separator_insensitive_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(a, b)| as_separator(*a) == as_separator(*b))
}

pub fn symbol_matches(symbol: &str, query: &str) -> bool {
    if symbol == query {
        return true;
    }
    let tail = symbol.rsplit("::").next().unwrap_or(symbol);
    if tail == query {
        return true;
    }
    // File-path node ids (`pkg/foo.go`) are not `Type.method`. Treating the
    // extension as a method made `impact go` start from every Go file.
    if looks_like_path(tail) {
        return false;
    }
    if let Some((_, method)) = tail.rsplit_once('.') {
        if method == query && !has_source_extension(method) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substring_traps_do_not_match() {
        assert!(!traversal_start_matches("go", "Alpha", "alpha.go"));
        assert!(!traversal_start_matches(
            "go",
            "go_orchestrator/internal/search/arxiv.go",
            "go_orchestrator/internal/search/arxiv.go"
        ));
        assert!(!traversal_start_matches("e", "eval", "eval.go"));
        assert!(!traversal_start_matches(
            "hallucinations.go::segment",
            "other.go::segmenterResponse",
            "other.go"
        ));
    }

    /// Each predicate asserted directly, one case per surviving mutant.
    ///
    /// Mutation testing found every helper in this file replaceable without a
    /// failure: `path_matches -> true`, `has_source_extension -> false`,
    /// `split_qualified -> None`, and both `||` in `looks_like_path` flipped to
    /// `&&`. The two end-to-end tests here exercise the composed behaviour, but
    /// a composed assertion cannot pin which branch produced it — an
    /// always-true `path_matches` still yields the right answer whenever the
    /// symbol half of the conjunction is also true. These target the branches.
    #[test]
    fn split_qualified_requires_both_halves() {
        assert_eq!(split_qualified("a.go::sym"), Some(("a.go", "sym")));
        // Both guards are load-bearing: an empty half must reject, or a bare
        // `::sym` would be treated as a file-qualified query against no file.
        assert_eq!(split_qualified("::sym"), None);
        assert_eq!(split_qualified("a.go::"), None);
        assert_eq!(split_qualified("plain"), None);
        // rsplit: the LAST `::` separates, so a nested identity keeps its head.
        assert_eq!(
            split_qualified("a.go::Type::method"),
            Some(("a.go::Type", "method"))
        );
    }

    #[test]
    fn looks_like_path_accepts_separators_and_extensions_independently() {
        // Separator alone, no extension.
        assert!(looks_like_path("pkg/sub"));
        assert!(looks_like_path("pkg\\sub"));
        // Extension alone, no separator.
        assert!(looks_like_path("main.go"));
        // Neither.
        assert!(!looks_like_path("plainName"));
        // A dot that is not a source extension is a `Type.method`, not a path.
        assert!(!looks_like_path("Type.method"));
    }

    #[test]
    fn has_source_extension_discriminates() {
        assert!(has_source_extension("main.go"));
        assert!(has_source_extension("app.tsx"));
        assert!(!has_source_extension("Type.method"));
        assert!(!has_source_extension("noextension"));
    }

    /// The allocation-free matcher answers exactly what the `replace`/`format!`
    /// spelling answered.
    ///
    /// The rewrite is justified by an argument — `/` and `\\` are ASCII, so
    /// folding one to the other is a length-preserving byte map and comparing
    /// folded bytes is comparing folded strings — and an argument is not
    /// evidence. This is the evidence: the old spelling, kept here as an
    /// oracle, run against the new one over the inputs that could separate
    /// them. Multi-byte text is in the table because a byte-wise comparison is
    /// where a naive rewrite breaks; the empty query is there because
    /// `checked_sub(query.len() + 1)` is the guard that keeps it from
    /// underflowing.
    #[test]
    fn path_matching_agrees_with_the_spelling_it_replaced() {
        fn oracle(file: &str, query: &str) -> bool {
            let file = file.replace('\\', "/");
            let query = query.replace('\\', "/");
            file == query || file.ends_with(&format!("/{query}"))
        }

        let fragments = [
            "",
            "/",
            "\\",
            "a",
            "a.go",
            "b/c.go",
            "a/b/c.go",
            "a\\b\\c.go",
            "a/b\\c.go",
            "abc.go",
            "bc.go",
            "/b/c.go",
            "a/b/c.go/",
            "a//b.go",
            // Multi-byte: 0xC3 0xA9 and a 4-byte emoji. A byte-wise fold must
            // not mistake a continuation byte for a separator.
            "caf\u{e9}/m\u{f3}dulo.go",
            "caf\u{e9}\\m\u{f3}dulo.go",
            "m\u{f3}dulo.go",
            "\u{1F980}/ferris.rs",
            "ferris.rs",
            // A name whose bytes end the same way another one does, which is
            // what the boundary check exists to reject.
            "xxbc.go",
        ];

        let mut checked = 0usize;
        for file in fragments {
            for query in fragments {
                assert_eq!(
                    path_matches(file, query),
                    oracle(file, query),
                    "path_matches({file:?}, {query:?}) diverged from the spelling it replaced"
                );
                checked += 1;
            }
        }
        // The pair count is asserted so a future edit that empties the table
        // cannot leave a green test that checked nothing.
        assert_eq!(checked, fragments.len() * fragments.len());
        assert_eq!(checked, 400);
    }

    #[test]
    fn path_matches_is_exact_or_suffix_on_a_boundary() {
        assert!(path_matches("a/b/c.go", "a/b/c.go"));
        assert!(path_matches("a/b/c.go", "b/c.go"));
        assert!(path_matches("a\\b\\c.go", "b/c.go"));
        // Must reject, or every query would match every file.
        assert!(!path_matches("a/b/c.go", "d.go"));
        // Suffix must start at a separator: `bc.go` is not `b/c.go`.
        assert!(!path_matches("a/abc.go", "bc.go"));
    }

    #[test]
    fn symbol_matches_requires_a_real_method_tail() {
        assert!(symbol_matches("a.go::Type.method", "method"));
        assert!(symbol_matches("a.go::Type.method", "Type.method"));
        assert!(symbol_matches("plain", "plain"));
        // A different method name must not match merely by having a tail.
        assert!(!symbol_matches("a.go::Type.method", "other"));
        // A file-path node id is not a `Type.method`, so its extension is not
        // a method name — otherwise `impact go` starts from every Go file.
        assert!(!symbol_matches("pkg/foo.go", "go"));
    }

    #[test]
    fn a_qualified_query_requires_both_file_and_symbol_to_match() {
        // Right file, wrong symbol.
        assert!(!traversal_start_matches("a.go::sym", "other", "a.go"));
        // Right symbol, wrong file.
        assert!(!traversal_start_matches("a.go::sym", "sym", "b.go"));
        // Both right.
        assert!(traversal_start_matches("a.go::sym", "sym", "a.go"));
    }

    #[test]
    fn qualified_and_exact_symbol_match() {
        assert!(traversal_start_matches(
            "hallucinations.go::segment",
            "hallucinations.go::segment",
            "hallucinations.go"
        ));
        assert!(traversal_start_matches("c", "c", "c.py"));
        assert!(traversal_start_matches(
            "headers",
            "client.go::paperclipClient.headers",
            "client.go"
        ));
    }
}
