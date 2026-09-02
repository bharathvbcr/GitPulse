//! Exact traversal starts for impact/trace.
//!
//! Substring `contains` on symbol *or* file path made `impact "e"` saturate the
//! graph and report Available. Matching is exact: a qualified `file::symbol`,
//! a path, or a symbol name (including `Type.method`).

pub fn traversal_start_matches(query: &str, symbol: &str, file: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return false;
    }
    if let Some((file_part, symbol_part)) = split_qualified(query) {
        return path_matches(file, file_part) && symbol_matches(symbol, symbol_part);
    }
    if looks_like_path(query) {
        return path_matches(file, query);
    }
    symbol_matches(symbol, query)
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

fn path_matches(file: &str, query: &str) -> bool {
    let file = file.replace('\\', "/");
    let query = query.replace('\\', "/");
    file == query || file.ends_with(&format!("/{query}"))
}

fn symbol_matches(symbol: &str, query: &str) -> bool {
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
