//! Exact traversal starts for impact/trace.
//!
//! Substring `contains` on symbol *or* file path made `impact "e"` saturate the
//! graph and report Available. Matching is exact: a qualified `file::symbol`,
//! `module::symbol`, `Type::method`, a path, or a symbol name (including
//! `Type.method`).

pub fn traversal_start_matches(query: &str, symbol: &str, file: &str) -> bool {
    let trimmed = query.trim();
    match classify(trimmed) {
        StartQuery::Nothing => false,
        StartQuery::Qualified { .. } => {
            qualified_file_or_module_matches(trimmed, symbol, file)
                || scoped_symbol_matches(symbol, trimmed)
        }
        StartQuery::Path(path) => path_matches(file, path),
        StartQuery::Symbol(name) => symbol_matches(symbol, name),
    }
}

/// True when a `::`-qualified query matches `(symbol, file)` by matching a
/// `::`-delimited prefix against `file` (exact/suffix file path or module path)
/// and the remaining suffix against `symbol`.
pub(crate) fn qualified_file_or_module_matches(query: &str, symbol: &str, file: &str) -> bool {
    if has_empty_qualified_segment(query) {
        return false;
    }
    let bytes = query.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b':' && bytes[i + 1] == b':' {
            let prefix = &query[..i];
            let suffix = &query[i + 2..];
            if !prefix.is_empty()
                && !suffix.is_empty()
                && (path_matches(file, prefix) || module_path_matches(file, prefix))
                && symbol_matches(symbol, suffix)
            {
                return true;
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    false
}

/// Rejects leading `::`, trailing `::`, or empty interior segments (`a::::b`).
fn has_empty_qualified_segment(query: &str) -> bool {
    query.split("::").any(str::is_empty)
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
    /// `path::symbol`, `module::symbol`, or `Type::method` — split at the last
    /// `::` so `symbol` is always the leaf name that every matching symbol must
    /// satisfy.
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

pub(crate) fn looks_like_path(query: &str) -> bool {
    query.contains('/')
        || query.contains('\\')
        || (query.contains('.') && has_source_extension(query))
}

pub(crate) fn has_source_extension(query: &str) -> bool {
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

fn is_module_source_extension(ext: &str) -> bool {
    matches!(
        ext,
        "go" | "py"
            | "rs"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "mts"
            | "cts"
            | "c"
            | "h"
            | "cc"
            | "cpp"
            | "hpp"
            | "cxx"
            | "cs"
            | "java"
            | "kt"
            | "kts"
            | "swift"
            | "rb"
            | "php"
            | "vue"
            | "svelte"
            | "astro"
            | "scala"
            | "dart"
            | "lua"
            | "sh"
            | "bash"
            | "sql"
            | "ex"
            | "exs"
            | "zig"
            | "m"
            | "mm"
    )
}

/// Whether `file` belongs to the module path `query` (e.g. `webcontent` or
/// `sysload::webcontent` for `src-tauri/src/sysload/webcontent.rs`, or
/// `sysload` for `src-tauri/src/sysload/mod.rs`).
///
/// Zero heap allocations: scans segments right-to-left over UTF-8 slices.
pub fn module_path_matches(file: &str, query: &str) -> bool {
    if query.is_empty() {
        return false;
    }
    let Some((stem_path, ext)) = strip_module_extension(file) else {
        return false;
    };
    if module_segments_match_suffix(stem_path, query) {
        return true;
    }
    // Directory modules: `pkg/mod.rs`, `pkg/index.ts`, `pkg/__init__.py`.
    if let Some(dir_path) = directory_module_parent(stem_path, ext) {
        return module_segments_match_suffix(dir_path, query);
    }
    false
}

fn strip_module_extension(file: &str) -> Option<(&str, &str)> {
    let (stem, ext) = file.rsplit_once('.')?;
    if stem.is_empty() || ext.contains(['/', '\\']) || !is_module_source_extension(ext) {
        return None;
    }
    Some((stem, ext))
}

fn directory_module_parent<'a>(stem_path: &'a str, ext: &str) -> Option<&'a str> {
    let (parent, base) = match stem_path.rfind(['/', '\\']) {
        Some(sep) => (&stem_path[..sep], &stem_path[sep + 1..]),
        None => ("", stem_path),
    };
    if parent.is_empty() {
        return None;
    }
    let is_dir_module = (ext == "rs" && base == "mod")
        || (matches!(
            ext,
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts"
        ) && base == "index")
        || (ext == "py" && base == "__init__");
    is_dir_module.then_some(parent)
}

fn module_segments_match_suffix(candidate_path: &str, query: &str) -> bool {
    let mut q_rest = query;
    let mut p_rest = candidate_path;
    let mut matched_any = false;
    while let Some(q_seg) = next_module_query_segment_back(&mut q_rest) {
        let Ok(q_seg) = q_seg else {
            return false;
        };
        let Some(p_seg) = next_path_segment_back(&mut p_rest) else {
            return false;
        };
        if p_seg != q_seg {
            return false;
        }
        matched_any = true;
    }
    matched_any
}

fn next_module_query_segment_back<'a>(rest: &mut &'a str) -> Option<Result<&'a str, ()>> {
    if rest.is_empty() {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut i = bytes.len();
    while i > 0 {
        let b = bytes[i - 1];
        if b == b'/' || b == b'\\' {
            let seg = &rest[i..];
            if seg.is_empty() || i == 1 {
                return Some(Err(()));
            }
            *rest = &rest[..i - 1];
            return Some(Ok(seg));
        }
        if b == b':' && i >= 2 && bytes[i - 2] == b':' {
            let seg = &rest[i..];
            if seg.is_empty() || i == 2 {
                return Some(Err(()));
            }
            *rest = &rest[..i - 2];
            return Some(Ok(seg));
        }
        i -= 1;
    }
    let seg = *rest;
    *rest = "";
    Some(Ok(seg))
}

fn next_path_segment_back<'a>(rest: &mut &'a str) -> Option<&'a str> {
    if rest.is_empty() {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut i = bytes.len();
    while i > 0 {
        let b = bytes[i - 1];
        if b == b'/' || b == b'\\' {
            let seg = &rest[i..];
            *rest = &rest[..i - 1];
            return Some(seg);
        }
        i -= 1;
    }
    let seg = *rest;
    *rest = "";
    Some(seg)
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

/// Whether `symbol` is the `File` node of a template component whose file stem
/// is `name`: `src/lib/MarkdownBody.svelte` for `MarkdownBody`.
///
/// A component has no symbol of its own. It is its file, so the name an agent
/// writes for it ("who uses MarkdownBody?") matches nothing by symbol and the
/// answer was an empty `Available` one. Only a file node qualifies (it has no
/// `::`), only the template-component extensions, and only the whole stem, so
/// `Foo.test.svelte` is not `Foo`.
pub fn component_file_matches(symbol: &str, name: &str) -> bool {
    component_stem(symbol) == Some(name)
}

/// The stem of a template component's `File` node (`MarkdownBody` for
/// `src/lib/MarkdownBody.svelte`), and `None` for anything else.
pub fn component_stem(symbol: &str) -> Option<&str> {
    if symbol.contains("::") {
        return None;
    }
    let file = symbol.rsplit(['/', '\\']).next().unwrap_or(symbol);
    let (stem, extension) = file.rsplit_once('.')?;
    matches!(extension, "svelte" | "vue" | "astro").then_some(stem)
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
    //
    // Only for a file node, which has no `::`. In `a.go::Type.go` the part
    // after `::` is a symbol name by construction — a method that happens to be
    // called `go` — and `impact go` / `impact Type.go` must be able to start
    // from it.
    let is_file_node = !symbol.contains("::");
    if is_file_node && looks_like_path(tail) {
        return false;
    }
    if let Some((_, method)) = tail.rsplit_once('.') {
        if method == query && (!is_file_node || !has_source_extension(method)) {
            return true;
        }
    }
    scoped_symbol_tail_matches(tail, query, is_file_node)
}

/// Matches a `::`-scoped symbol query (`Ring::dump`, `Outer::Inner::method`)
/// against the symbol's tail (`Ring.dump`, `Outer.Inner.method`), never
/// bypassing the file check by comparing against the full `file::symbol` ID.
pub(crate) fn scoped_symbol_matches(symbol: &str, query: &str) -> bool {
    let is_file_node = !symbol.contains("::");
    let tail = symbol.rsplit("::").next().unwrap_or(symbol);
    if is_file_node && looks_like_path(tail) {
        return false;
    }
    scoped_symbol_tail_matches(tail, query, is_file_node)
}

fn scoped_symbol_tail_matches(tail: &str, query: &str, is_file_node: bool) -> bool {
    if query.is_empty()
        || (!query.contains("::") && !query.contains('.'))
        || query.contains(['/', '\\'])
    {
        return false;
    }
    let mut q_rest = query;
    let mut t_rest = tail;
    let mut segments = 0usize;
    while let Some(q_seg) = next_symbol_scope_segment_back(&mut q_rest) {
        let Ok(q_seg) = q_seg else {
            return false;
        };
        if segments == 0 && is_file_node && has_source_extension(q_seg) {
            return false;
        }
        let Some(t_seg) = next_dot_segment_back(&mut t_rest) else {
            return false;
        };
        if t_seg != q_seg {
            return false;
        }
        segments += 1;
    }
    segments >= 2
}

fn next_symbol_scope_segment_back<'a>(rest: &mut &'a str) -> Option<Result<&'a str, ()>> {
    if rest.is_empty() {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut i = bytes.len();
    while i > 0 {
        let b = bytes[i - 1];
        if b == b'.' {
            let seg = &rest[i..];
            if seg.is_empty() || i == 1 {
                return Some(Err(()));
            }
            *rest = &rest[..i - 1];
            return Some(Ok(seg));
        }
        if b == b':' && i >= 2 && bytes[i - 2] == b':' {
            let seg = &rest[i..];
            if seg.is_empty() || i == 2 {
                return Some(Err(()));
            }
            *rest = &rest[..i - 2];
            return Some(Ok(seg));
        }
        i -= 1;
    }
    let seg = *rest;
    *rest = "";
    Some(Ok(seg))
}

fn next_dot_segment_back<'a>(rest: &mut &'a str) -> Option<&'a str> {
    if rest.is_empty() {
        return None;
    }
    match rest.rfind('.') {
        Some(dot) => {
            let seg = &rest[dot + 1..];
            *rest = &rest[..dot];
            Some(seg)
        }
        None => {
            let seg = *rest;
            *rest = "";
            Some(seg)
        }
    }
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

    /// A method may be called `go`: past `::` the tail is a symbol name, so the
    /// source-extension guard that keeps `impact go` off every Go file does not
    /// apply to it.
    #[test]
    fn a_method_named_like_an_extension_is_still_a_symbol() {
        assert!(symbol_matches("a.ts::Thing.go", "go"));
        assert!(symbol_matches("a.ts::Thing.go", "Thing.go"));
        assert!(!symbol_matches("a.ts::Thing.go", "Other.go"));
        // The file node itself is still not a method named `go`.
        assert!(!symbol_matches("pkg/foo.go", "go"));
        assert!(!symbol_matches("foo.go", "go"));
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

    #[test]
    fn module_path_matches_stems_and_directory_modules() {
        let file = "src-tauri/src/sysload/webcontent.rs";
        assert!(module_path_matches(file, "webcontent"));
        assert!(module_path_matches(file, "sysload::webcontent"));
        assert!(module_path_matches(file, "src::sysload::webcontent"));
        assert!(module_path_matches(
            file,
            "src-tauri::src::sysload::webcontent"
        ));
        // Windows separator parity.
        assert!(module_path_matches(
            r"src-tauri\src\sysload\webcontent.rs",
            "sysload::webcontent"
        ));
        // Partial stems, parent dirs of non-index files, wrong chains, and empties are rejected.
        assert!(!module_path_matches(file, "content"));
        assert!(!module_path_matches(file, "sysload"));
        assert!(!module_path_matches(file, "other::webcontent"));
        assert!(!module_path_matches(file, ""));
        assert!(!module_path_matches(file, "::webcontent"));
        assert!(!module_path_matches(file, "webcontent::"));
        assert!(!module_path_matches(file, "sysload::::webcontent"));

        // Directory module files (`mod.rs`, `index.*`, `__init__.py`) match their enclosing directory.
        assert!(module_path_matches("src/sysload/mod.rs", "sysload"));
        assert!(module_path_matches("src/sysload/mod.rs", "src::sysload"));
        assert!(module_path_matches("src/sysload/mod.rs", "sysload::mod"));
        assert!(module_path_matches("src/ui/panel/index.tsx", "panel"));
        assert!(module_path_matches("src/ui/panel/index.tsx", "ui::panel"));
        assert!(module_path_matches("pkg/auth/__init__.py", "auth"));
        assert!(module_path_matches("pkg/auth/__init__.py", "pkg::auth"));
        // Root-level index/mod files have no parent directory module.
        assert!(!module_path_matches("mod.rs", ""));
        assert!(module_path_matches("mod.rs", "mod"));
    }

    #[test]
    fn scoped_symbol_matches_double_colon_member_tails() {
        assert!(scoped_symbol_matches(
            "src-tauri/src/sysload/webcontent.rs::Ring.dump",
            "Ring::dump"
        ));
        assert!(scoped_symbol_matches("a.rs::Outer.Inner.run", "Inner::run"));
        assert!(scoped_symbol_matches(
            "a.rs::Outer.Inner.run",
            "Outer::Inner::run"
        ));
        // Boundary and structural rejections.
        assert!(!scoped_symbol_matches(
            "src-tauri/src/sysload/webcontent.rs::Ring.dump",
            "ing::dump"
        ));
        assert!(!scoped_symbol_matches(
            "src-tauri/src/sysload/webcontent.rs::Ring.dump",
            "Other::dump"
        ));
        assert!(!scoped_symbol_matches(
            "src-tauri/src/sysload/webcontent.rs::Ring.dump",
            "Ring::other"
        ));
        assert!(!scoped_symbol_matches("a.rs::Ring.dump", "Ring::"));
        assert!(!scoped_symbol_matches("a.rs::Ring.dump", "::dump"));
        assert!(!scoped_symbol_matches("a.rs::Ring.dump", "Ring::::dump"));
        // File nodes (no `::`) are never scoped symbols.
        assert!(!scoped_symbol_matches("pkg/Ring.dump", "Ring::dump"));
    }

    #[test]
    fn traversal_start_matches_all_qualified_module_and_type_shapes() {
        let file = "src-tauri/src/sysload/webcontent.rs";
        let fn_node = "src-tauri/src/sysload/webcontent.rs::snapshot";
        let method_node = "src-tauri/src/sysload/webcontent.rs::Ring.dump";

        for query in [
            "webcontent::snapshot",
            "sysload::webcontent::snapshot",
            "webcontent.rs::snapshot",
            "src-tauri/src/sysload/webcontent.rs::snapshot",
        ] {
            assert!(
                traversal_start_matches(query, fn_node, file),
                "expected {query:?} to match {fn_node:?}"
            );
        }

        for query in [
            "Ring::dump",
            "webcontent::Ring::dump",
            "sysload::webcontent::Ring::dump",
            "webcontent.rs::Ring::dump",
            "src-tauri/src/sysload/webcontent.rs::Ring::dump",
            "webcontent::Ring.dump",
        ] {
            assert!(
                traversal_start_matches(query, method_node, file),
                "expected {query:?} to match {method_node:?}"
            );
        }

        assert!(traversal_start_matches(
            "sysload::init",
            "src/sysload/mod.rs::init",
            "src/sysload/mod.rs"
        ));
        assert!(traversal_start_matches(
            "panel::render",
            "src/ui/panel/index.ts::render",
            "src/ui/panel/index.ts"
        ));
        assert!(traversal_start_matches(
            "auth::login",
            "pkg/auth/__init__.py::login",
            "pkg/auth/__init__.py"
        ));

        for bad in [
            "other::snapshot",
            "Other::dump",
            "other.rs::Ring::dump",
            "::snapshot",
            "webcontent::",
            "webcontent::::snapshot",
        ] {
            assert!(
                !traversal_start_matches(bad, fn_node, file),
                "expected {bad:?} not to match {fn_node:?}"
            );
            assert!(
                !traversal_start_matches(bad, method_node, file),
                "expected {bad:?} not to match {method_node:?}"
            );
        }
    }
}
