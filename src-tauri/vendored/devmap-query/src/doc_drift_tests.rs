use super::*;

fn refs(text: &str) -> Vec<(u32, RefSpec)> {
    extract_references(text)
        .references
        .into_iter()
        .map(|r| (r.line, r.spec))
        .collect()
}

fn link(target: &str) -> RefSpec {
    RefSpec::Link {
        target: target.to_string(),
        symbols: Vec::new(),
    }
}

fn link_with(target: &str, symbols: &[&str]) -> RefSpec {
    RefSpec::Link {
        target: target.to_string(),
        symbols: symbols.iter().map(|s| (*s).to_string()).collect(),
    }
}

fn symbol(qualifier: &[&str], name: &str, written: &str) -> RefSpec {
    RefSpec::Symbol {
        qualifier: qualifier.iter().map(|s| (*s).to_string()).collect(),
        name: name.to_string(),
        written: written.to_string(),
    }
}

fn path(path: &str, lines: Option<(u32, u32)>, ext: bool) -> RefSpec {
    RefSpec::PathSpan {
        path: path.to_string(),
        lines,
        has_known_extension: ext,
    }
}

#[test]
fn fenced_blocks_are_not_scanned_in_either_fence_style() {
    let text = "[a](a.md)\n```rust\n[b](b.md) `VerifyTask`\n```\n~~~\n[c](c.md)\n~~~\n[d](d.md)\n";
    let found = refs(text);
    assert_eq!(found, vec![(1, link("a.md")), (8, link("d.md"))]);
}

#[test]
fn a_longer_fence_is_not_closed_by_a_shorter_one() {
    let text = "````\n```\n[x](x.md)\n```\n````\n[y](y.md)\n";
    assert_eq!(refs(text), vec![(6, link("y.md"))]);
}

#[test]
fn an_unterminated_fence_swallows_the_rest_of_the_document() {
    let text = "[a](a.md)\n```\n[b](b.md)\n[c](c.md)\n";
    assert_eq!(refs(text), vec![(1, link("a.md"))]);
}

#[test]
fn a_code_span_at_line_start_is_not_a_fence() {
    let text = "```not a fence``` and `VerifyTask`\n";
    assert_eq!(
        refs(text),
        vec![(1, symbol(&[], "VerifyTask", "VerifyTask"))]
    );
}

#[test]
fn indented_code_blocks_are_skipped_but_nested_list_items_are_not() {
    let text = "para\n\n    [code](code.md)\n    `VerifyTask`\n\n- item\n    - [nested](nested.md)\n\nback [b](b.md)\n";
    assert_eq!(refs(text), vec![(7, link("nested.md")), (9, link("b.md"))]);
}

#[test]
fn html_comments_are_skipped_single_line_and_multi_line() {
    let text = "a <!-- [x](x.md) --> [y](y.md)\n<!--\n[z](z.md)\n--> [w](w.md)\n";
    assert_eq!(refs(text), vec![(1, link("y.md")), (4, link("w.md"))]);
}

#[test]
fn a_comment_marker_inside_a_code_span_does_not_open_a_comment() {
    let text = "write `<!--` to open one\n[kept](kept.md)\n";
    assert_eq!(refs(text), vec![(2, link("kept.md"))]);
}

#[test]
fn link_forms_are_recognised() {
    let text =
        "[a](docs/a.md) ![img](img/p.png) [t](<with space.md>) [u](x.md \"title\") [p](a(b).md)\n";
    let found: Vec<RefSpec> = refs(text).into_iter().map(|(_, s)| s).collect();
    assert_eq!(
        found,
        vec![
            link("docs/a.md"),
            link("img/p.png"),
            link("with space.md"),
            link("x.md"),
            link("a(b).md"),
        ]
    );
}

#[test]
fn reference_style_definitions_are_links_and_footnotes_are_not() {
    let text = "[guide]: docs/guide.md \"Guide\"\n[other]: <docs/o.md>\n[^1]: a note\n[web]: https://example.com\n";
    assert_eq!(
        refs(text),
        vec![(1, link("docs/guide.md")), (2, link("docs/o.md"))]
    );
}

#[test]
fn external_absolute_and_same_document_targets_are_skipped_with_reasons() {
    let text = "[a](https://x.dev) [b](mailto:a@b.c) [c](#top) [d](/abs/path) [e](//cdn.x/y) [f]() [g](foo/{id}.md)\n";
    let extraction = extract_references(text);
    assert!(extraction.references.is_empty(), "{extraction:?}");
    assert_eq!(extraction.skipped.get("external_link"), Some(&3));
    assert_eq!(extraction.skipped.get("same_document_anchor"), Some(&1));
    assert_eq!(extraction.skipped.get("absolute_link"), Some(&1));
    assert_eq!(extraction.skipped.get("empty_link"), Some(&1));
    assert_eq!(extraction.skipped.get("placeholder"), Some(&1));
}

#[test]
fn link_anchors_stay_on_the_target() {
    let text = "[a](src/lib.rs#L10) [b](src/lib.rs#L10-L20) [c](guide.md#some-heading)\n";
    let found: Vec<RefSpec> = refs(text).into_iter().map(|(_, s)| s).collect();
    assert_eq!(
        found,
        vec![
            link("src/lib.rs#L10"),
            link("src/lib.rs#L10-L20"),
            link("guide.md#some-heading"),
        ]
    );
    assert_eq!(parse_line_anchor("L10"), Some((10, 10)));
    assert_eq!(parse_line_anchor("L10-L20"), Some((10, 20)));
    assert_eq!(parse_line_anchor("L20-L10"), Some((10, 20)));
    assert_eq!(parse_line_anchor("L0"), None);
    assert_eq!(parse_line_anchor("Lines"), None);
    assert_eq!(parse_line_anchor("heading"), None);
}

#[test]
fn symbol_spans_in_link_text_bind_to_the_link() {
    let text = "[`VerifyTask` and `plain` and `src/x.rs`](src/verify.go)\n";
    assert_eq!(
        refs(text),
        vec![(1, link_with("src/verify.go", &["VerifyTask"]))]
    );
}

#[test]
fn an_image_inside_a_link_is_still_a_link() {
    let text = "[![badge](img/b.svg)](docs/ci.md)\n";
    let found: Vec<RefSpec> = refs(text).into_iter().map(|(_, s)| s).collect();
    assert_eq!(found, vec![link("docs/ci.md"), link("img/b.svg")]);
}

#[test]
fn path_spans_carry_line_suffixes() {
    let text = "see `src/lib.rs:12` and `src/lib.rs:12-30` and `docs/a.md` and `src/lib.rs`\n";
    let found: Vec<RefSpec> = refs(text).into_iter().map(|(_, s)| s).collect();
    assert_eq!(
        found,
        vec![
            path("src/lib.rs", Some((12, 12)), true),
            path("src/lib.rs", Some((12, 30)), true),
            path("docs/a.md", None, true),
            path("src/lib.rs", None, true),
        ]
    );
}

#[test]
fn a_directory_path_is_a_path_without_an_extension() {
    assert_eq!(
        refs("the `rust/devmap-query/` crate\n"),
        vec![(1, path("rust/devmap-query", None, false))]
    );
}

#[test]
fn spans_that_are_not_paths_are_not_references() {
    let text = "`--flag a/b` `cargo run -p x/y` `a/*.rs` `<dir>/x.rs` `{a,b}/c.rs` `$HOME/x.rs` `a|b/c.rs` `-x/y.rs` `a\\b/c.rs`\n";
    let extraction = extract_references(text);
    assert!(extraction.references.is_empty(), "{extraction:?}");
}

#[test]
fn urls_and_placeholders_are_named_skips_not_paths() {
    let text = "`https://x.dev/a.md` `github.com/org/repo/x.go` `/etc/hosts` `~/Code/x.md` `path/to/file.rs`\n";
    let extraction = extract_references(text);
    assert!(extraction.references.is_empty(), "{extraction:?}");
    assert_eq!(extraction.skipped.get("external_reference"), Some(&2));
    assert_eq!(extraction.skipped.get("absolute_path"), Some(&1));
    assert_eq!(extraction.skipped.get("home_path"), Some(&1));
    assert_eq!(extraction.skipped.get("placeholder_path"), Some(&1));
}

#[test]
fn symbol_shapes_are_checked_and_ambiguous_names_are_skipped() {
    let text = "`VerifyTask` `runRigorGates` `run()` `Store::latest_file` `store.Requirements` `pkg.Func()` `HTTPServer`\n";
    let found: Vec<RefSpec> = refs(text).into_iter().map(|(_, s)| s).collect();
    assert_eq!(
        found,
        vec![
            symbol(&[], "VerifyTask", "VerifyTask"),
            symbol(&[], "runRigorGates", "runRigorGates"),
            symbol(&[], "run", "run()"),
            symbol(&["Store"], "latest_file", "Store::latest_file"),
            symbol(&["store"], "Requirements", "store.Requirements"),
            symbol(&["pkg"], "Func", "pkg.Func()"),
            symbol(&[], "HTTPServer", "HTTPServer"),
        ]
    );

    let skipped_text = "`word` `Store` `ALL_CAPS` `snake_case` `foo.bar` `Cargo.toml` `main.rs` `std::mem::take` `fmt.Println` `GitHub` `FOO_BAR()`\n";
    let extraction = extract_references(skipped_text);
    let kept: Vec<&RefSpec> = extraction.references.iter().map(|r| &r.spec).collect();
    // `FOO_BAR()` ends in `()`, so it is a call and is checked. `fmt.Println`
    // has an ambiguous standard root: whether it is the standard library's or
    // the repository's is the resolver's call, so it is a reference here.
    assert_eq!(
        kept,
        vec![
            &symbol(&["fmt"], "Println", "fmt.Println"),
            &symbol(&[], "FOO_BAR", "FOO_BAR()")
        ]
    );
    assert_eq!(extraction.skipped.get("ambiguous_identifier"), Some(&5));
    assert_eq!(extraction.skipped.get("bare_filename"), Some(&2));
    assert_eq!(extraction.skipped.get("external_qualifier"), Some(&1));
    assert_eq!(extraction.skipped.get("known_term"), Some(&1));
}

/// Found by running the report over this repository: git config keys, Kotlin
/// properties, namespaces and lint names were all reported as missing symbols.
#[test]
fn dotted_config_keys_namespaces_and_lint_names_are_not_symbols() {
    let text = "`receive.denyCurrentBranch` `Habit.statusLabel` `Microsoft.Extensions.DependencyInjection` `clippy::useless_concat` `extensions.worktreeConfig`\n";
    let extraction = extract_references(text);
    assert!(extraction.references.is_empty(), "{extraction:?}");
    assert_eq!(extraction.skipped.get("ambiguous_identifier"), Some(&4));
    assert_eq!(extraction.skipped.get("external_qualifier"), Some(&1));
}

#[test]
fn generated_paths_are_recognised() {
    for generated in [
        "tmp/x.sh",
        "target/debug/devmap",
        "node_modules/a/b.js",
        ".devcouncil/repo_map.json",
        ".cursor/mcp.json",
        "benchmarks/run/verify.log",
    ] {
        assert!(is_generated_path(generated), "{generated}");
    }
    for kept in [
        "docs/a.md",
        ".github/workflows/ci.yml",
        "../x.md",
        "rust/target-notes.md",
    ] {
        assert!(!is_generated_path(kept), "{kept}");
    }
}

#[test]
fn extraction_counts_candidates_as_references_plus_skips() {
    let extraction = extract_references("`VerifyTask` `word` [a](a.md) [b](https://x.dev)\n");
    assert_eq!(extraction.references.len(), 2);
    assert_eq!(extraction.candidates(), 4);
}

#[test]
fn multi_backtick_spans_and_padding() {
    let text = "``a `VerifyTask` b`` and ` runRigor `\n";
    // The double-backtick span's content has spaces, so it is prose; the padded
    // single span unwraps to an identifier.
    assert_eq!(refs(text), vec![(1, symbol(&[], "runRigor", "runRigor"))]);
}

#[test]
fn headings_produce_github_slugs() {
    let text = "# Title Here\n## What's New? (v2)\n### `code` & Co.\n## Under_score-Dash\n## Ünïcode Straße\n";
    let slugs = heading_slugs(text);
    for want in [
        "title-here",
        "whats-new-v2",
        "code--co",
        "under_score-dash",
        "ünïcode-straße",
    ] {
        assert!(slugs.contains(want), "{want} missing from {slugs:?}");
    }
}

#[test]
fn duplicate_headings_get_numeric_suffixes() {
    let slugs = heading_slugs("# Usage\n## Usage\n## Usage\n");
    assert_eq!(
        slugs.into_iter().collect::<Vec<_>>(),
        vec!["usage", "usage-1", "usage-2"]
    );
}

#[test]
fn headings_in_fences_and_comments_do_not_count_and_links_keep_their_text() {
    let text = "```\n# Not A Heading\n```\n<!--\n# Hidden\n-->\n## See [the docs](x.md) and <b>bold</b>\n## Closed ##\n";
    let slugs = heading_slugs(text);
    assert!(!slugs.contains("not-a-heading"), "{slugs:?}");
    assert!(!slugs.contains("hidden"), "{slugs:?}");
    assert!(slugs.contains("see-the-docs-and-bold"), "{slugs:?}");
    assert!(slugs.contains("closed"), "{slugs:?}");
}

#[test]
fn setext_headings_are_headings_and_a_leading_rule_is_not() {
    let slugs = heading_slugs("---\nTitle\n=====\n\nSub head\n--------\n");
    assert!(slugs.contains("title"), "{slugs:?}");
    assert!(slugs.contains("sub-head"), "{slugs:?}");
    assert_eq!(slugs.len(), 2, "{slugs:?}");
}

#[test]
fn an_atx_marker_without_a_space_is_not_a_heading() {
    assert!(heading_slugs("#hashtag\n#######seven\n").is_empty());
}

#[test]
fn relative_resolution_normalises_and_refuses_escapes() {
    assert_eq!(
        resolve_relative("docs/a.md", "../src/x.rs").as_deref(),
        Some("src/x.rs")
    );
    assert_eq!(
        resolve_relative("README.md", "./docs/./b.md").as_deref(),
        Some("docs/b.md")
    );
    assert_eq!(
        resolve_relative("docs/deep/a.md", "../../x.md").as_deref(),
        Some("x.md")
    );
    assert_eq!(resolve_relative("docs/a.md", "../../x.md"), None);
    assert_eq!(resolve_relative("README.md", "../x.md"), None);
    assert_eq!(resolve_relative("a.md", "x/../../y"), None);
}

#[test]
fn percent_encoding_decodes_only_valid_utf8() {
    assert_eq!(percent_decode("my%20file.md"), "my file.md");
    assert_eq!(percent_decode("100%"), "100%");
    assert_eq!(percent_decode("%zz"), "%zz");
    assert_eq!(percent_decode("%ff"), "%ff");
    assert_eq!(percent_decode("é%20ü"), "é ü");
}

#[test]
fn qualifier_rule_uses_the_qualified_name_then_the_path() {
    let symbol = |qualified: &str, path: &str, name: &str| StoredSymbol {
        name: name.to_string(),
        qualified_name: qualified.to_string(),
        kind: "Method".to_string(),
        path: path.to_string(),
        span_start: 1,
        span_end: 2,
        is_exported: true,
        content_hash: 0,
    };
    let method = symbol(
        "rust/devmap-store/src/db/query.rs::Store::latest_file",
        "rust/devmap-store/src/db/query.rs",
        "latest_file",
    );
    let q = |parts: &[&str]| parts.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    assert!(qualifier_matches(&method, &[]));
    assert!(qualifier_matches(&method, &q(&["Store"])));
    assert!(!qualifier_matches(&method, &q(&["Other"])));
    // A directory, with `-` read as `_`.
    assert!(qualifier_matches(&method, &q(&["devmap_store"])));
    // The file stem.
    assert!(qualifier_matches(&method, &q(&["query"])));
    assert!(!qualifier_matches(&method, &q(&["queries"])));
    // A Go function qualified by its package directory.
    let go = symbol(
        "pkg/store/req.go::Requirements",
        "pkg/store/req.go",
        "Requirements",
    );
    assert!(qualifier_matches(&go, &q(&["store"])));
    assert!(!qualifier_matches(&go, &q(&["web"])));
}

#[test]
fn path_suffixes_name_every_tail_of_an_indexed_path() {
    let suffixes = path_suffixes(["a/b/c.rs"].into_iter());
    assert!(suffixes.contains("b/c.rs"));
    assert!(suffixes.contains("c.rs"));
    assert!(!suffixes.contains("a/b/c.rs"));
}

#[test]
fn prefixes_refuse_escapes_and_whole_repo_spellings() {
    let owned = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    assert_eq!(
        clean_prefixes(&owned(&["./docs/", "README.md"]), "path").unwrap(),
        vec!["docs", "README.md"]
    );
    for bad in ["../x", "/abs", "a/../b", "", "  ", ".", "./", "a\\b"] {
        assert!(
            clean_prefixes(&owned(&[bad]), "path").is_err(),
            "{bad:?} must be refused"
        );
    }
    assert!(under_prefix("docs/a.md", "docs"));
    assert!(under_prefix("docs", "docs"));
    assert!(!under_prefix("docsx/a.md", "docs"));
}

// Hostile input -------------------------------------------------------------

#[test]
fn a_huge_line_is_counted_and_not_scanned() {
    let long = format!("[a](a.md) {}\n[b](b.md)\n", "x".repeat(MAX_LINE_BYTES + 10));
    let extraction = extract_references(&long);
    assert_eq!(extraction.skipped.get("line_too_long"), Some(&1));
    let found: Vec<RefSpec> = extraction.references.into_iter().map(|r| r.spec).collect();
    assert_eq!(found, vec![link("b.md")]);
}

#[test]
fn unbalanced_brackets_and_backticks_on_a_maximal_line_stay_fast() {
    let line = "[`(".repeat(MAX_LINE_BYTES / 3 - 1);
    let started = std::time::Instant::now();
    let extraction = extract_references(&format!("{line}\n[ok](ok.md)\n"));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(extraction.references.len(), 1);
    let alternating = "`a``b```c````".repeat(300);
    let _ = extract_references(&alternating[..alternating.len().min(MAX_LINE_BYTES)]);
}

#[test]
fn nested_brackets_and_escapes() {
    let text = "[a [b] c](nest.md) [esc\\]](x.md) \\[no](y.md)\n";
    let found: Vec<RefSpec> = refs(text).into_iter().map(|(_, s)| s).collect();
    assert_eq!(found, vec![link("nest.md"), link("x.md")]);
}

#[test]
fn unicode_and_multibyte_text_around_markers_does_not_panic() {
    let text = "日本語 [リンク](docs/日本.md) `Ünï::Cödé` é`VerifyTask`é [é](é\n";
    let found: Vec<RefSpec> = refs(text).into_iter().map(|(_, s)| s).collect();
    assert_eq!(
        found,
        vec![
            link("docs/日本.md"),
            symbol(&[], "VerifyTask", "VerifyTask")
        ]
    );
}

#[test]
fn a_destination_that_runs_to_the_lookahead_edge_is_not_a_link() {
    let text = format!("[a]({}\u{e9}\n", "x".repeat(LINK_LOOKAHEAD - 4));
    assert!(extract_references(&text).references.is_empty());
}

#[test]
fn the_reference_cap_stops_the_scan_and_says_so() {
    let line = "`VerifyTask` ".repeat(10);
    let text = format!("{line}\n").repeat(MAX_REFERENCES_PER_DOC / 10 + 5);
    let extraction = extract_references(&text);
    assert_eq!(extraction.references.len(), MAX_REFERENCES_PER_DOC);
    assert!(extraction.truncated);
    assert!(!extract_references("`VerifyTask`\n").truncated);
}

#[test]
fn crlf_and_missing_final_newline() {
    let text = "[a](a.md)\r\n```\r\n[b](b.md)\r\n```\r\n[c](c.md)";
    assert_eq!(refs(text), vec![(1, link("a.md")), (5, link("c.md"))]);
}

#[test]
fn line_suffix_edge_cases() {
    assert_eq!(split_line_suffix("a.rs:3"), ("a.rs", Some((3, 3))));
    assert_eq!(split_line_suffix("a.rs:9-3"), ("a.rs", Some((3, 9))));
    assert_eq!(split_line_suffix("a.rs:0"), ("a.rs:0", None));
    assert_eq!(split_line_suffix("a.rs:x"), ("a.rs:x", None));
    assert_eq!(split_line_suffix("a::b"), ("a::b", None));
    assert_eq!(split_line_suffix(":3"), (":3", None));
    assert_eq!(
        split_line_suffix("a.rs:99999999999"),
        ("a.rs:99999999999", None)
    );
}
