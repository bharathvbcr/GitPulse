#[cfg(feature = "parse")]
use super::*;
#[cfg(feature = "parse")]
use devmap_extract::extract_file;

#[cfg(feature = "parse")]
fn indexed(files: &[(&str, &str)]) -> (Resolver, Vec<devmap_extract::model::Extraction>) {
    let extractions: Vec<_> = files
        .iter()
        .map(|(path, source)| extract_file(path, source))
        .collect();
    let mut resolver = Resolver::new();
    resolver.index_extractions(&extractions);
    (resolver, extractions)
}

/// Go same-package lookup is scoped to one directory, excludes tests, and
/// abstains when ambiguous.
///
/// Every clause of the filter was mutable without a failure. Widening any
/// of them resolves a call to a same-named function in an unrelated package
/// — a confidently wrong edge, since nothing downstream can tell the
/// difference.
#[test]
#[cfg(feature = "parse")]
fn go_package_lookup_is_directory_scoped_and_abstains_when_ambiguous() {
    let (resolver, _) = indexed(&[
        ("pkg/a.go", "package pkg\nfunc Helper() {}\n"),
        ("pkg/b.go", "package pkg\nfunc Caller() {}\n"),
        ("other/c.go", "package other\nfunc Elsewhere() {}\n"),
        ("pkg/a_test.go", "package pkg\nfunc OnlyInTest() {}\n"),
    ]);

    // A sibling file in the same directory resolves.
    assert_eq!(
        resolver.lookup_in_package("pkg/b.go", "Helper"),
        Some(("pkg/a.go".to_string(), "pkg/a.go::Helper".to_string())),
        "a same-package sibling must resolve"
    );

    // A different directory is a different package and must not.
    assert_eq!(
        resolver.lookup_in_package("pkg/b.go", "Elsewhere"),
        None,
        "a symbol in another directory is a different package"
    );

    // Test files are not part of the importable package surface.
    assert_eq!(
        resolver.lookup_in_package("pkg/b.go", "OnlyInTest"),
        None,
        "a symbol declared only in a _test.go file must not resolve"
    );

    // Non-Go files get no package lookup at all.
    assert_eq!(
        resolver.lookup_in_package("app.py", "Helper"),
        None,
        "package lookup is Go-only"
    );
}

#[test]
#[cfg(feature = "parse")]
fn go_package_lookup_abstains_when_two_files_declare_the_name() {
    let (resolver, _) = indexed(&[
        ("pkg/a.go", "package pkg\nfunc Dup() {}\n"),
        ("pkg/b.go", "package pkg\nfunc Dup() {}\n"),
        ("pkg/c.go", "package pkg\nfunc Caller() {}\n"),
    ]);
    assert_eq!(
        resolver.lookup_in_package("pkg/c.go", "Dup"),
        None,
        "two candidates in one package must abstain, not pick one"
    );
}

/// A symbol declared in the querying file wins outright.
#[test]
#[cfg(feature = "parse")]
fn go_package_lookup_prefers_the_querying_file() {
    let (resolver, _) = indexed(&[
        ("pkg/a.go", "package pkg\nfunc Same() {}\n"),
        ("pkg/b.go", "package pkg\nfunc Same() {}\n"),
    ]);
    assert_eq!(
        resolver.lookup_in_package("pkg/a.go", "Same"),
        Some(("pkg/a.go".to_string(), "pkg/a.go::Same".to_string())),
        "the querying file's own symbol must win before any package scan"
    );
}

/// The Python stdlib guard is scoped to Python and to cross-file targets.
///
/// `open`, `len`, `type` and friends are overwhelmingly the builtins, not a
/// same-named helper in another module, so resolving them across files
/// manufactures edges. Both halves of the guard were mutable: dropping the
/// language test applies it to Go and TypeScript too, and dropping the
/// `target != self` test suppresses a *genuine* same-file definition.
#[test]
#[cfg(feature = "parse")]
fn python_stdlib_names_do_not_resolve_across_files_but_do_within_one() {
    // Cross-file: `open` defined in another module must NOT be reached.
    let (cross, cross_exts) = indexed(&[
        (
            "caller.py",
            "import helpers\n\ndef go():\n    return open('x')\n",
        ),
        ("helpers.py", "def open(path):\n    return path\n"),
    ]);
    let result = cross.resolve_all(&cross_exts).unwrap();
    assert!(
        !result.edges.iter().any(|edge| {
            edge.source_file == "caller.py"
                && edge.target_file == "helpers.py"
                && edge.target_symbol.ends_with("open")
                && edge.confidence == Confidence::DETERMINISTIC
        }),
        "a Python builtin name must not resolve confidently to another \
             module's same-named function: {:?}",
        result
            .edges
            .iter()
            .map(|e| (&e.source_symbol, &e.target_symbol, e.confidence.0))
            .collect::<Vec<_>>()
    );

    // Same file: a real local definition of `open` must still resolve.
    let (same, same_exts) = indexed(&[(
        "local.py",
        "def open(path):\n    return path\n\ndef go():\n    return open('x')\n",
    )]);
    let local = same.resolve_all(&same_exts).unwrap();
    assert!(
        local.edges.iter().any(|edge| {
            edge.edge_kind == EdgeKind::Calls && edge.target_symbol.ends_with("open")
        }),
        "a same-file definition must still resolve, guard or not: {:?}",
        local
            .edges
            .iter()
            .map(|e| (&e.source_symbol, &e.target_symbol))
            .collect::<Vec<_>>()
    );

    // The guard is Python-only: a Go function named `open` is ordinary.
    let (go, go_exts) = indexed(&[
        (
            "pkg/a.go",
            "package pkg\nfunc open(p string) string { return p }\n",
        ),
        (
            "pkg/b.go",
            "package pkg\nfunc use() string { return open(\"x\") }\n",
        ),
    ]);
    let go_result = go.resolve_all(&go_exts).unwrap();
    assert!(
        go_result
            .edges
            .iter()
            .any(|edge| edge.edge_kind == EdgeKind::Calls && edge.target_symbol.ends_with("open")),
        "the stdlib guard must not apply outside Python: {:?}",
        go_result
            .edges
            .iter()
            .map(|e| (&e.source_symbol, &e.target_symbol))
            .collect::<Vec<_>>()
    );
}
