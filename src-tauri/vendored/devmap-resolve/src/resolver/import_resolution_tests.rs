use super::*;
use devmap_extract::GoModule;

fn module(dir: &str, prefix: &str, replaces: &[(&str, &str)]) -> GoModule {
    GoModule {
        prefix: prefix.to_string(),
        dir: dir.to_string(),
        replaces: replaces
            .iter()
            .map(|(from, to)| (from.to_string(), to.to_string()))
            .collect(),
    }
}

/// A receiver binds once; a conflicting bind poisons the key permanently.
///
/// This is SC9's fix. Two different types bound to the same receiver name
/// used to be last-write-wins, so the loser still resolved — at
/// DETERMINISTIC confidence — to the wrong type. That is a *confidently
/// wrong* answer rather than a missing one, which is the failure a code
/// map must never produce. Every part of the conflict test was mutable:
/// forced false it restores last-write-wins, forced true it poisons on a
/// harmless repeat, and inverted it poisons agreement while accepting
/// conflict.
#[test]
fn a_conflicting_receiver_bind_poisons_the_key_permanently() {
    let mut map = BTreeMap::new();
    let mut poisoned = BTreeSet::new();
    let key = || "file.go:scope:w".to_string();

    // First bind wins.
    Resolver::bind_receiver(&mut map, &mut poisoned, key(), "Worker");
    assert_eq!(map.get(&key()).map(String::as_str), Some("Worker"));
    assert!(poisoned.is_empty());

    // Re-binding the *same* type is agreement, not conflict.
    Resolver::bind_receiver(&mut map, &mut poisoned, key(), "Worker");
    assert_eq!(
        map.get(&key()).map(String::as_str),
        Some("Worker"),
        "binding the same type twice must not poison the key"
    );
    assert!(poisoned.is_empty(), "agreement is not a conflict");

    // A different type is a conflict: the binding is withdrawn, not
    // overwritten, so neither type is reported.
    Resolver::bind_receiver(&mut map, &mut poisoned, key(), "Other");
    assert_eq!(
        map.get(&key()),
        None,
        "a conflicting bind must withdraw the binding, not overwrite it"
    );
    assert!(poisoned.contains(&key()));

    // And the poison is permanent — a later bind cannot resurrect it, in
    // either direction.
    Resolver::bind_receiver(&mut map, &mut poisoned, key(), "Worker");
    assert_eq!(
        map.get(&key()),
        None,
        "a poisoned key must stay unbound even when re-offered the original type"
    );

    // Poisoning is per key: an unrelated receiver is unaffected.
    let other = "file.go:scope:z".to_string();
    Resolver::bind_receiver(&mut map, &mut poisoned, other.clone(), "Thing");
    assert_eq!(map.get(&other).map(String::as_str), Some("Thing"));
}

/// Only Python's dotted relative form names its child module in the list.
///
/// `from . import sibling` puts the *module* in the import list, so the
/// specifier to resolve is `.sibling`; every other language keeps the
/// specifier as written. All three clauses were mutable, and each failure
/// mode rewrites an import to a module path that does not exist — the
/// import silently resolves to nothing.
#[test]
fn only_dotted_relative_specifiers_absorb_the_imported_name() {
    // Python relative imports name the child module.
    assert_eq!(Resolver::import_spec_for_name(".", "sibling"), ".sibling");
    assert_eq!(Resolver::import_spec_for_name("..", "parent"), "..parent");

    // A real module specifier is kept as written.
    assert_eq!(
        Resolver::import_spec_for_name("pkg.sub", "Name"),
        "pkg.sub",
        "a named import from a real module keeps its specifier"
    );
    assert_eq!(Resolver::import_spec_for_name("./local", "x"), "./local");

    // A wildcard names no module, and neither does an absent name.
    assert_eq!(
        Resolver::import_spec_for_name(".", "*"),
        ".",
        "`from . import *` names no child module"
    );
    assert_eq!(Resolver::import_spec_for_name(".", ""), ".");
}

/// Every language maps to its own resolution family.
///
/// The family is what stops a Python annotation resolving to an
/// identically-named Go type. Deleting an arm drops that language to
/// `Generic`, which shares a family with every other unmapped language —
/// so the guard that prevents cross-language edges silently stops
/// separating them.
#[test]
fn every_language_maps_to_its_own_resolution_family() {
    use devmap_resolve_family_assertions::*;
    assert_family("python", LangFamily::Python);
    for lang in ["javascript", "typescript", "tsx", "jsx"] {
        assert_family(lang, LangFamily::JsTs);
    }
    assert_family("go", LangFamily::Go);
    assert_family("rust", LangFamily::Rust);
    for lang in ["c", "cpp", "csharp", "java", "objc"] {
        assert_family(lang, LangFamily::CStyle);
    }
    // An unmapped language falls back rather than joining someone else.
    assert_family("cobol", LangFamily::Generic);
    assert_family("", LangFamily::Generic);

    // The families must actually be distinct, or none of the above proves
    // anything.
    let families = [
        LangFamily::from_lang("python"),
        LangFamily::from_lang("javascript"),
        LangFamily::from_lang("go"),
        LangFamily::from_lang("rust"),
        LangFamily::from_lang("c"),
        LangFamily::from_lang("cobol"),
    ];
    for (index, left) in families.iter().enumerate() {
        for right in &families[index + 1..] {
            assert_ne!(left, right, "two languages share a resolution family");
        }
    }
}

mod devmap_resolve_family_assertions {
    use super::LangFamily;
    pub fn assert_family(lang: &str, expected: LangFamily) {
        assert_eq!(
            LangFamily::from_lang(lang),
            expected,
            "{lang} must map to {expected:?}"
        );
    }
}

/// Fixture spanning the per-language import-path resolvers.
#[cfg(feature = "parse")]
fn path_fixture() -> Resolver {
    use devmap_extract::extract_file;
    let files = [
        ("web/app.ts", "export const a = 1;\n"),
        ("web/sibling.ts", "export const b = 2;\n"),
        ("web/widget.tsx", "export const c = 3;\n"),
        ("web/nested/index.ts", "export const d = 4;\n"),
        ("web/plain.js", "export const e = 5;\n"),
        ("pkg/mod.py", "def f():\n    pass\n"),
        ("pkg/__init__.py", "\n"),
        ("pkg/sub/leaf.py", "def g():\n    pass\n"),
        ("src/lib.rs", "pub mod thing;\n"),
        ("src/thing.rs", "pub fn t() {}\n"),
        ("src/deep/mod.rs", "pub fn d() {}\n"),
    ];
    let extractions: Vec<_> = files
        .iter()
        .map(|(path, source)| extract_file(path, source))
        .collect();
    let mut resolver = Resolver::new();
    resolver.index_extractions(&extractions);
    resolver
}

/// Each language resolves import paths by its own rules.
///
/// `resolve_import_path` is the only thing that turns an import specifier
/// into a file, so every mutant here breaks import edges — and always by
/// resolving to *nothing*, never by erroring. The language guards were
/// mutable in both directions: dropped, one language's path rules are
/// applied to another's specifiers; inverted, the language that owns the
/// rule stops using it.
///
/// Every expectation was read off the resolver before being pinned.
#[test]
#[cfg(feature = "parse")]
fn each_language_resolves_import_paths_by_its_own_rules() {
    let resolver = path_fixture();
    let resolve =
        |file: &str, lang: &str, spec: &str| resolver.resolve_import_path(file, lang, spec);

    // TypeScript: a relative specifier is tried against each extension and
    // then against an index file.
    assert_eq!(
        resolve("web/app.ts", "typescript", "./sibling").as_deref(),
        Some("web/sibling.ts")
    );
    assert_eq!(
        resolve("web/app.ts", "typescript", "./nested").as_deref(),
        Some("web/nested/index.ts"),
        "a directory import resolves to its index file"
    );
    assert_eq!(
        resolve("web/app.ts", "typescript", "./plain").as_deref(),
        Some("web/plain.js"),
        "a TS file may import a JS sibling"
    );

    // The specifier arrives quoted from the grammar, in either quote style,
    // and both must be stripped before the path is built.
    assert_eq!(
        resolve("web/app.ts", "typescript", "\"./sibling\"").as_deref(),
        Some("web/sibling.ts"),
        "a double-quoted specifier resolves"
    );
    assert_eq!(
        resolve("web/app.ts", "typescript", "'./widget'").as_deref(),
        Some("web/widget.tsx"),
        "a single-quoted specifier resolves"
    );

    // A bare package specifier is not a path and must not be resolved
    // against the repository.
    assert_eq!(
        resolve("web/app.ts", "typescript", "react"),
        None,
        "a node package is not a local file"
    );

    // Python: an absolute dotted module, and the relative forms, where the
    // leading dot count decides how far up to walk.
    assert_eq!(
        resolve("pkg/mod.py", "python", "pkg.sub.leaf").as_deref(),
        Some("pkg/sub/leaf.py"),
        "an absolute dotted module maps onto directories"
    );
    assert_eq!(
        resolve("pkg/sub/leaf.py", "python", "..").as_deref(),
        Some("pkg/__init__.py"),
        "`from .. import x` resolves to the parent package"
    );
    assert_eq!(
        resolve("pkg/sub/leaf.py", "python", "..mod").as_deref(),
        Some("pkg/mod.py"),
        "the dots walk up and the tail names the module"
    );

    // Rust: `crate::` maps onto `src/`, trying a file then a module
    // directory, and popping segments until something exists.
    assert_eq!(
        resolve("src/lib.rs", "rust", "crate::thing").as_deref(),
        Some("src/thing.rs")
    );
    assert_eq!(
        resolve("src/lib.rs", "rust", "crate::deep").as_deref(),
        Some("src/deep/mod.rs"),
        "a module directory resolves through its mod.rs"
    );
    assert_eq!(
        resolve("src/lib.rs", "rust", "crate::thing::t").as_deref(),
        Some("src/thing.rs"),
        "a path to an item inside a module resolves to that module's file"
    );

    // The language guards hold: another language's specifier syntax does
    // not resolve through these rules.
    assert_eq!(
        resolve("src/lib.rs", "python", "crate::thing"),
        None,
        "Rust path syntax is not resolved for Python"
    );
    assert_eq!(
        resolve("web/app.ts", "python", "./sibling"),
        None,
        "a relative TS specifier is not a Python module path"
    );
}

/// A Python relative import is answered from the importer's own package and
/// nowhere else (ft-bd1bfac9).
///
/// `src/pkg/util.py` and `pkg/pkg/util.py` are present while `pkg/util.py` is
/// not, which is exactly the shape the old hand-rolled arm bound by probing
/// `src/{path}` and `{dir}/{path}` after the real candidates missed.
#[test]
#[cfg(feature = "parse")]
fn python_relative_imports_resolve_in_the_importing_package_only() {
    use devmap_extract::extract_file;
    let files = [
        ("main.py", "def m():\n    pass\n"),
        ("util.py", "def f():\n    pass\n"),
        ("pkg/__init__.py", "\n"),
        ("pkg/a.py", "def a():\n    pass\n"),
        ("src/pkg/util.py", "def f():\n    pass\n"),
        ("pkg/pkg/util.py", "def f():\n    pass\n"),
        // A namespace package: no `__init__.py` anywhere under it.
        ("ns/inner/leaf.py", "def leaf():\n    pass\n"),
        ("ns/sibling.py", "def s():\n    pass\n"),
    ];
    let extractions: Vec<_> = files
        .iter()
        .map(|(path, source)| extract_file(path, source))
        .collect();
    let mut resolver = Resolver::new();
    resolver.index_extractions(&extractions);
    let resolve = |file: &str, spec: &str| resolver.resolve_import_path(file, "python", spec);

    assert_eq!(
        resolve("main.py", ".util").as_deref(),
        Some("util.py"),
        "at the repo root the importing package is the root directory itself"
    );
    assert_eq!(
        resolve("main.py", "."),
        None,
        "`from . import x` at a root with no __init__.py names no module file"
    );
    assert_eq!(
        resolve("main.py", ".missing"),
        None,
        "a missing root sibling abstains"
    );
    assert_eq!(
        resolve("pkg/a.py", "."),
        Some("pkg/__init__.py".to_string()),
        "`from . import x` is the importer's own package"
    );
    assert_eq!(
        resolve("pkg/a.py", "..util").as_deref(),
        Some("util.py"),
        "two dots climb one package level, to the root"
    );
    assert_eq!(
        resolve("pkg/a.py", "..."),
        None,
        "three dots from a depth-one package climb above the root"
    );
    assert_eq!(
        resolve("main.py", "..util"),
        None,
        "two dots at the root climb above it: nothing, never the clamped root file"
    );
    assert_eq!(
        resolve("main.py", "...util"),
        None,
        "further climbs past the root stay unresolved"
    );
    assert_eq!(
        resolve("pkg/a.py", ".util"),
        None,
        "pkg/util.py is absent: not src/pkg/util.py, not pkg/pkg/util.py"
    );
    assert_eq!(
        resolve("pkg/a.py", ".pkg.util").as_deref(),
        Some("pkg/pkg/util.py"),
        "a dotted tail after the leading dots names a real sub-package"
    );
    assert_eq!(
        resolve("ns/inner/leaf.py", "..sibling").as_deref(),
        Some("ns/sibling.py"),
        "a namespace package (no __init__.py) still counts as a package"
    );
    assert_eq!(
        resolve("ns/inner/leaf.py", "..inner.leaf").as_deref(),
        Some("ns/inner/leaf.py")
    );
}

/// Fixture spanning every tier of the Go import ladder.    /// Fixture spanning every tier of the Go import ladder.
#[cfg(feature = "parse")]
fn go_fixture() -> Resolver {
    use devmap_extract::extract_file;
    let files = [
        ("internal/svc/a.go", "package svc\nfunc A() {}\n"),
        ("internal/svc/b.go", "package svc\nfunc B() {}\n"),
        // A test file is not part of the importable surface.
        ("internal/svc/a_test.go", "package svc\nfunc T() {}\n"),
        ("vendor/example.com/ext/e.go", "package ext\nfunc E() {}\n"),
        // A vendored directory whose name collides with cgo's pseudo-package.
        ("vendor/C/c.go", "package c\nfunc C() {}\n"),
        ("local/lib/l.go", "package lib\nfunc L() {}\n"),
        ("deep/nested/pkg/n.go", "package pkg\nfunc N() {}\n"),
        ("cmd/tool/main.go", "package main\nfunc main() {}\n"),
        // A single-component directory that the suffix tier is too weak
        // to claim, so only the module tier can resolve it.
        ("x/y.go", "package x\nfunc Y() {}\n"),
        // A nested module whose directory deliberately differs from the
        // path the outer module's prefix would produce.
        ("othersub/inner/i.go", "package inner\nfunc I() {}\n"),
        // A non-Go file whose directory is a longer suffix than the real
        // Go package's.
        ("a/deep/nested/pkg/z.py", "def z():\n    pass\n"),
    ];
    let extractions: Vec<_> = files
        .iter()
        .map(|(path, source)| extract_file(path, source))
        .collect();
    let mut resolver = Resolver::new();
    resolver.index_extractions(&extractions);
    resolver.index_go_modules(&[
        module("", "example.com/app", &[("example.com/lib", "./local/lib")]),
        module("othersub", "example.com/app/sub", &[]),
        // A duplicate prefix of equal length, declared second.
        module("dup", "example.com/app/sub", &[]),
    ]);
    resolver
}

#[test]
#[cfg(feature = "parse")]
fn probe_extended() {
    let r = go_fixture();
    for spec in [
        "example.com/app",
        "example.com/app/x",
        "example.com/app/sub/inner",
        "example.com/app/internal/svc",
        "other.com/a/deep/nested/pkg",
        "other.com/q/x",
    ] {
        println!("spec {spec:?} -> {:?}", r.resolve_go_import(spec));
    }
    println!("dir_root -> {:?}", r.go_files_in_dir(""));
    println!("dir_internal -> {:?}", r.go_files_in_dir("internal"));
}

/// The Go import ladder resolves each tier, in order, and abstains at the end.
///
/// `resolve_go_import` carried 23 surviving mutants — the largest single
/// cluster in the workspace — including whole-body replacements with
/// `vec![]` and `vec!["xyzzy"]`. It maps an import specifier to the files
/// that satisfy it, so it is what makes a Go import edge point anywhere at
/// all. Returning nothing severs every cross-package edge in a Go repo and
/// leaves the imported package looking uncalled; returning the wrong files
/// points the edge at a package that merely shares a path suffix, which is
/// worse because it still looks resolved.
///
/// Every expectation was read off the resolver before being pinned.
#[test]
#[cfg(feature = "parse")]
fn the_go_import_ladder_resolves_each_tier_in_order() {
    let resolver = go_fixture();

    // Tier 0 — abstention. `C` is cgo's pseudo-package and has no files.
    assert!(resolver.resolve_go_import("").is_empty());
    assert!(resolver.resolve_go_import("   ").is_empty());
    assert!(
        resolver.resolve_go_import("C").is_empty(),
        "cgo's pseudo-package must resolve to nothing, not to a directory named C"
    );

    // A specifier arrives quoted from the Go grammar and must be trimmed.
    assert_eq!(
        resolver.resolve_go_import("\"example.com/app/internal/svc\""),
        ["internal/svc/a.go", "internal/svc/b.go"],
        "a quoted specifier resolves exactly as its bare form does"
    );

    // Tier 2 — module prefix. Test files are excluded from the surface.
    assert_eq!(
        resolver.resolve_go_import("example.com/app/internal/svc"),
        ["internal/svc/a.go", "internal/svc/b.go"],
        "an import under the module prefix resolves to that directory"
    );

    // Tier 1 — a `replace` directive outranks the module prefix.
    assert_eq!(
        resolver.resolve_go_import("example.com/lib"),
        ["local/lib/l.go"],
        "a replace directive redirects the import"
    );

    // Tier 3 — vendored copy.
    assert_eq!(
        resolver.resolve_go_import("example.com/ext"),
        ["vendor/example.com/ext/e.go"],
        "an import with no module match falls back to vendor/"
    );

    // Tier 4 — directory suffix, for repositories whose module path is not
    // reflected on disk.
    assert_eq!(
        resolver.resolve_go_import("other.com/x/deep/nested/pkg"),
        ["deep/nested/pkg/n.go"],
        "a directory that is a suffix of the import resolves it"
    );

    // Tier 4 requires at least two path components, so a single-segment
    // directory can never be claimed by an unrelated import.
    assert!(
        resolver.resolve_go_import("other.com/x/tool").is_empty(),
        "a one-component directory is too weak a signal to match on"
    );
    assert!(
        resolver
            .resolve_go_import("other.com/x/nested/pkg")
            .is_empty(),
        "a partial suffix must not match"
    );

    // A bare specifier with no slash is a standard-library package.
    assert!(
        resolver.resolve_go_import("fmt").is_empty(),
        "a stdlib import has no files in this repository"
    );
    assert!(resolver.resolve_go_import("pkg").is_empty());

    // The module root itself holds no Go files here.
    assert!(resolver.resolve_go_import("example.com/app").is_empty());
    assert!(resolver
        .resolve_go_import("unknown.com/nope/deep")
        .is_empty());
}

/// The module tier resolves what no weaker tier can.
///
/// `x/` is a single-component directory, so the suffix tier is forbidden
/// from claiming it and vendor holds nothing — the module prefix is the
/// only tier that can resolve this import. That makes it the case that
/// separates a working module tier from one that merely looks like it works
/// because a later tier happens to reach the same files: both halves of the
/// prefix test and the "did it find anything" check are only observable here.
#[test]
#[cfg(feature = "parse")]
fn the_module_tier_resolves_what_no_weaker_tier_reaches() {
    let resolver = go_fixture();
    assert_eq!(
        resolver.resolve_go_import("example.com/app/x"),
        ["x/y.go"],
        "a one-component package under the module prefix resolves only via the \
             module tier"
    );
    assert!(
        resolver.resolve_go_import("other.com/q/x").is_empty(),
        "and without the prefix it must not resolve at all"
    );
}

/// The longest matching module prefix wins.
///
/// Go repositories nest modules, and the inner module's directory need not
/// match the path the outer module's prefix would produce. All three
/// comparisons in the "is this a better match" test were mutable. If a
/// shorter prefix wins, every import under the inner module is rewritten
/// against the outer module's layout and lands on a directory that does not
/// exist — the import silently resolves to nothing.
#[test]
#[cfg(feature = "parse")]
fn the_longest_matching_module_prefix_wins() {
    let resolver = go_fixture();
    assert_eq!(
        resolver.resolve_go_import("example.com/app/sub/inner"),
        ["othersub/inner/i.go"],
        "the nested module maps `sub/` to `othersub/`; the outer module would \
             have produced `sub/inner`, which holds nothing"
    );
}

/// Only Go files contribute directories to the suffix tier.
///
/// The suffix tier scans indexed paths for candidate directories, filtered
/// to non-test `.go` files. Relaxing that filter to `||` lets any file's
/// directory become a candidate — and because the tier keeps the *longest*
/// match, a deeper directory holding no Go code at all outranks the real
/// package and the import resolves to nothing. The fixture places a Python
/// file at `a/deep/nested/pkg/`, a strictly longer suffix than the Go
/// package at `deep/nested/pkg/`.
#[test]
#[cfg(feature = "parse")]
fn only_go_files_contribute_suffix_tier_candidates() {
    let resolver = go_fixture();
    assert_eq!(
        resolver.resolve_go_import("other.com/a/deep/nested/pkg"),
        ["deep/nested/pkg/n.go"],
        "a longer non-Go directory must not outrank the real Go package"
    );
}

/// A file's Go package survives having its source stripped.
///
/// `go_package_name_of` prefers the extracted `go_package` field and falls
/// back to scanning source text. The whole function was replaceable with
/// `None`, `Some("")` and `Some("xyzzy")`, and both emptiness filters were
/// deletable. The fallback hides the important case: `for_durable_store`
/// strips `source_code`, so after a reload the field is the *only* record of
/// the package, and losing it breaks every package-level import edge on
/// exactly the path a restarted daemon takes.
#[test]
#[cfg(feature = "parse")]
fn a_go_package_name_survives_source_stripping() {
    use devmap_extract::extract_file;

    let extraction = extract_file("pkg/a.go", "package svc\nfunc A() {}\n");
    assert_eq!(
        go_package_name_of(&extraction).as_deref(),
        Some("svc"),
        "the package is read from the extracted field"
    );

    let durable = extraction.for_durable_store();
    assert!(durable.source_code.is_none(), "fixture precondition");
    assert_eq!(
        go_package_name_of(&durable).as_deref(),
        Some("svc"),
        "the package must survive a reload, where no source text remains"
    );

    let mut blank = extraction.clone();
    blank.go_package = None;
    blank.source_code = None;
    assert_eq!(go_package_name_of(&blank), None);

    let mut empty_clause = extraction.clone();
    empty_clause.go_package = Some(String::new());
    empty_clause.source_code = Some("package \npackage real\n".to_string());
    assert_eq!(
        go_package_name_of(&empty_clause).as_deref(),
        Some("real"),
        "an empty clause is skipped in favour of a real one, not returned as an \
             empty package name"
    );
}

/// cgo's pseudo-package never binds to a real directory.
///
/// `import "C"` is not a package — it is the cgo directive, and the Go
/// toolchain synthesises it. The early return is what guarantees that, and
/// it is only observable when a directory of that name exists: the fixture
/// vendors `vendor/C/`, which the vendor tier would otherwise happily
/// resolve to. Binding every cgo import in a repository to one unrelated
/// package is a confidently wrong edge, not a missing one.
#[test]
#[cfg(feature = "parse")]
fn cgos_pseudo_package_never_binds_to_a_real_directory() {
    let resolver = go_fixture();
    assert_eq!(
        resolver.go_files_in_dir("vendor/C"),
        ["vendor/C/c.go"],
        "fixture precondition: a directory named C really is vendored"
    );
    assert!(
        resolver.resolve_go_import("C").is_empty(),
        "`import \"C\"` must resolve to nothing even when `vendor/C/` exists"
    );
    assert!(resolver.resolve_go_import("\"C\"").is_empty());
}

/// Equal-length module prefixes resolve deterministically, first declared.
///
/// The "is this a better match" comparison was mutable from `>` to `>=`,
/// which hands ties to the last module instead of the first. A monorepo can
/// declare the same prefix twice with different directories; whichever rule
/// applies, resolution must not depend on the order `go.mod` files happen
/// to be walked in. `>` keeps the first declaration, and this pins it.
#[test]
#[cfg(feature = "parse")]
fn equal_length_module_prefixes_resolve_deterministically() {
    let resolver = go_fixture();
    assert_eq!(
        resolver.resolve_go_import("example.com/app/sub/inner"),
        ["othersub/inner/i.go"],
        "on a tie the first declared module wins, not the last"
    );
}

/// A package's file set is exactly its own directory, minus tests.    /// A package's file set is exactly its own directory, minus tests.
#[test]
#[cfg(feature = "parse")]
fn go_files_in_dir_is_exact_and_excludes_tests() {
    let resolver = go_fixture();

    assert_eq!(
        resolver.go_files_in_dir("internal/svc"),
        ["internal/svc/a.go", "internal/svc/b.go"],
        "`_test.go` files are not part of the importable package"
    );
    // A trailing slash names the same directory.
    assert_eq!(
        resolver.go_files_in_dir("internal/svc/"),
        resolver.go_files_in_dir("internal/svc")
    );
    // Matching is on the exact parent directory, not a path prefix, or a
    // parent package would absorb all of its children's files.
    assert!(
        resolver.go_files_in_dir("internal").is_empty(),
        "a parent directory holds no files of its own here"
    );
    assert!(resolver.go_files_in_dir("").is_empty());
}

/// The directory index carries the current snapshot and nothing older.
///
/// `index_extractions` resets every other map for exactly this reason: a
/// caller may reuse the object for a rebuild. An index that accumulated
/// would answer a Go import with a file the snapshot no longer contains,
/// and the resulting edge is stamped with the same confidence as a real
/// one — a deleted file reappearing as a live import target.
///
/// Fails against an index that is built but never cleared.
#[test]
#[cfg(feature = "parse")]
fn the_directory_index_holds_only_the_current_snapshot() {
    use devmap_extract::extract_file;
    let mut resolver = Resolver::new();

    let first = [extract_file(
        "internal/svc/a.go",
        "package svc\nfunc A() {}\n",
    )];
    resolver.index_extractions(&first);
    assert_eq!(
        resolver.go_files_in_dir("internal/svc"),
        ["internal/svc/a.go"]
    );

    let second = [extract_file(
        "internal/other/b.go",
        "package other\nfunc B() {}\n",
    )];
    resolver.index_extractions(&second);
    assert!(
        resolver.go_files_in_dir("internal/svc").is_empty(),
        "a directory absent from the new snapshot must not survive the rebuild"
    );
    assert_eq!(
        resolver.go_files_in_dir("internal/other"),
        ["internal/other/b.go"]
    );
}

/// Grouping *every* indexed file by directory moves the `.go` filter from
/// the scan to the reader, so the reader has to still apply it.
///
/// A real package directory holds a README, a generated `.json` and often a
/// helper script. Returning those as package files makes each one an import
/// target for `example.com/svc`, and the first of them wins the edge.
///
/// Fails against a reader that returns the directory's files unfiltered.
#[test]
#[cfg(feature = "parse")]
fn go_files_in_dir_returns_no_non_go_sibling() {
    use devmap_extract::extract_file;
    let files = [
        ("pkg/svc/a.go", "package svc\nfunc A() {}\n"),
        ("pkg/svc/README.md", "# svc\n"),
        ("pkg/svc/helper.py", "def helper():\n    pass\n"),
        ("pkg/svc/a_test.go", "package svc\nfunc T() {}\n"),
    ];
    let extractions: Vec<_> = files
        .iter()
        .map(|(path, source)| extract_file(path, source))
        .collect();
    let mut resolver = Resolver::new();
    resolver.index_extractions(&extractions);

    assert_eq!(
        resolver.go_files_in_dir("pkg/svc"),
        ["pkg/svc/a.go"],
        "only the importable Go file belongs to the package"
    );
}

/// The suffix fallback ranks directories that hold importable Go.
///
/// The index holds every directory, including ones with no Go in them at
/// all. Handing that set to the fallback unfiltered lets `docs/api` claim
/// an `example.com/api` import, and a directory holding only `_test.go`
/// claim its package — both resolve to an empty file set, so the import
/// binds to nothing while reporting that it matched.
///
/// Fails against a fallback that walks the index keys directly.
#[test]
#[cfg(feature = "parse")]
fn go_package_dirs_names_only_directories_holding_importable_go() {
    use devmap_extract::extract_file;
    let files = [
        ("pkg/real/a.go", "package real\nfunc A() {}\n"),
        ("pkg/testonly/a_test.go", "package testonly\nfunc T() {}\n"),
        ("docs/api/guide.md", "# guide\n"),
    ];
    let extractions: Vec<_> = files
        .iter()
        .map(|(path, source)| extract_file(path, source))
        .collect();
    let mut resolver = Resolver::new();
    resolver.index_extractions(&extractions);

    assert_eq!(
        resolver.go_package_dirs().collect::<Vec<_>>(),
        ["pkg/real"],
        "a test-only directory and a docs directory are not Go packages"
    );
}

/// A `doc.go` carrying only a package clause is still a package file, and
/// answers come back sorted.
///
/// This pins membership and ordering, not a symbol-free path. The extractor
/// gives every file a file-level symbol named after its basename — measured,
/// including for an empty `.go` and for a `.txt` — so `file_symbols` never
/// holds an empty entry, and no mutation of the index's *population* can be
/// caught here. It is a contract test, and the mutation harness records it
/// as one rather than counting it as mutation-covered.
#[test]
#[cfg(feature = "parse")]
fn a_package_file_that_declares_nothing_is_still_in_the_package() {
    use devmap_extract::extract_file;
    let files = [
        ("pkg/svc/z.go", "package svc\nfunc Z() {}\n"),
        ("pkg/svc/doc.go", "package svc\n"),
        ("pkg/svc/a.go", "package svc\nfunc A() {}\n"),
    ];
    let extractions: Vec<_> = files
        .iter()
        .map(|(path, source)| extract_file(path, source))
        .collect();
    let mut resolver = Resolver::new();
    resolver.index_extractions(&extractions);

    assert_eq!(
        resolver.go_files_in_dir("pkg/svc"),
        ["pkg/svc/a.go", "pkg/svc/doc.go", "pkg/svc/z.go"],
        "a symbol-free file belongs to the package, and answers are sorted"
    );
}

/// An empty extension matches every path through `ends_with`.
///
/// The guard refusing it is what stops a rule that names no extension from
/// claiming every file in the directory.
#[test]
#[cfg(feature = "parse")]
fn an_empty_extension_claims_no_file() {
    use devmap_extract::extract_file;
    let files = [
        ("infra/main.tf", "resource \"null_resource\" \"a\" {}\n"),
        ("infra/notes.md", "# notes\n"),
    ];
    let extractions: Vec<_> = files
        .iter()
        .map(|(path, source)| extract_file(path, source))
        .collect();
    let mut resolver = Resolver::new();
    resolver.index_extractions(&extractions);

    assert!(
        resolver
            .files_in_dir_with_extensions("infra", &[""])
            .is_empty(),
        "an empty extension must not match by `ends_with`"
    );
    assert_eq!(
        resolver.files_in_dir_with_extensions("infra", &[".tf"]),
        ["infra/main.tf"]
    );
    // A trailing slash names the same directory here too.
    assert_eq!(
        resolver.files_in_dir_with_extensions("infra/", &[".tf"]),
        resolver.files_in_dir_with_extensions("infra", &[".tf"])
    );
}

/// An import edge points at the package node, except for `package main`.
///
/// `go_import_edge_targets` was replaceable with `vec![]` and with a
/// fabricated name. A Go import names a *package*, not a file, so the edge
/// target is the synthetic package node — collapsing that to per-file edges
/// multiplies every import edge by the package's file count, which is the
/// fan-out SC10 removed. `main` is not importable, so it stays a file.
#[test]
#[cfg(feature = "parse")]
fn go_import_edges_target_the_package_not_its_files() {
    let resolver = go_fixture();

    assert_eq!(
        resolver.go_import_edge_targets(&[
            "internal/svc/a.go".to_string(),
            "internal/svc/b.go".to_string(),
        ]),
        ["package:internal/svc/svc"],
        "two files of one package collapse to a single package node"
    );
    assert_eq!(
        resolver.go_import_edge_targets(&["cmd/tool/main.go".to_string()]),
        ["cmd/tool/main.go"],
        "`package main` is not importable and stays a file target"
    );
    assert!(resolver.go_import_edge_targets(&[]).is_empty());
}

/// Go visibility is decided by the leading character of the name.
#[test]
fn go_export_visibility_follows_the_leading_character() {
    assert!(Resolver::go_name_is_exported("A"));
    assert!(Resolver::go_name_is_exported("Exported"));
    assert!(!Resolver::go_name_is_exported("a"));
    assert!(!Resolver::go_name_is_exported("unexported"));
    assert!(!Resolver::go_name_is_exported("_private"));
    assert!(!Resolver::go_name_is_exported(""));
}

/// An import's local binding name is taken per-language.
///
/// Go binds the last path segment (`github.com/x/y/pkg` → `pkg`); the
/// dotted languages bind the last dotted segment. The `lang == "go"` test
/// was mutable to `!=` without a failure, which swaps the two rules and
/// makes every Go import bind to the whole specifier.
#[test]
fn import_local_names_follow_the_language() {
    assert_eq!(
        Resolver::import_local_name("go", "github.com/org/repo/pkg"),
        "pkg"
    );
    assert_eq!(Resolver::import_local_name("python", "a.b.c"), "c");
    assert_eq!(Resolver::import_local_name("typescript", "x.y"), "y");
    // The two rules must actually differ, or this test proves nothing.
    assert_ne!(
        Resolver::import_local_name("go", "a.b.c"),
        Resolver::import_local_name("python", "a.b.c")
    );
}

/// `replace` directives resolve longest-prefix-first.
///
/// All three comparisons in the "is this a better match" test were mutable
/// without a failure. A shorter prefix winning sends every import under the
/// longer one to the wrong module — silently, since both targets exist.
#[test]
fn go_replace_prefers_the_longest_matching_prefix() {
    let mut resolver = Resolver::new();
    resolver.index_go_modules(&[module(
        "",
        "example.com/main",
        &[
            ("example.com/lib", "./vendor/lib"),
            ("example.com/lib/inner", "./vendor/inner"),
        ],
    )]);

    // `apply_go_replace` returns (original spec, replacement target); the
    // replacement is the second element.
    let (_, specific) = resolver
        .apply_go_replace("example.com/lib/inner/pkg")
        .expect("replacement stays in the repository");
    assert_eq!(
        specific.as_deref(),
        Some("vendor/inner/pkg"),
        "the longer prefix must win"
    );

    let (_, general) = resolver
        .apply_go_replace("example.com/lib/other")
        .expect("replacement stays in the repository");
    assert_eq!(
        general.as_deref(),
        Some("vendor/lib/other"),
        "a path only the shorter prefix matches must still resolve"
    );

    // A specifier matching neither prefix yields no replacement at all.
    let (spec, untouched) = resolver
        .apply_go_replace("other.com/thing")
        .expect("replacement stays in the repository");
    assert_eq!(spec, "other.com/thing");
    assert_eq!(untouched, None, "a non-matching spec must not be rewritten");
}

/// Equal-length prefixes resolve deterministically to the first declared.
///
/// The longest-prefix comparison is strict (`>`), so a later replace of the
/// *same* path does not displace an earlier one. Relaxing it to `>=` made
/// the winner depend on module iteration order — two `go.mod` files
/// replacing the same module path would resolve differently between builds,
/// which R4 forbids outright.
#[test]
fn equal_length_replace_prefixes_resolve_to_the_first_declared() {
    let mut resolver = Resolver::new();
    resolver.index_go_modules(&[
        module("", "m1", &[("example.com/dup", "./first")]),
        module("sub", "m2", &[("example.com/dup", "./second")]),
    ]);

    let (_, target) = resolver
        .apply_go_replace("example.com/dup")
        .expect("replacement stays in the repository");
    assert_eq!(
        target.as_deref(),
        Some("first"),
        "the first declared replace of a path must win, not the last"
    );

    // Repeat: the answer must not depend on iteration order across calls.
    for _ in 0..3 {
        let (_, again) = resolver
            .apply_go_replace("example.com/dup")
            .expect("replacement stays in the repository");
        assert_eq!(again.as_deref(), Some("first"), "resolution must be stable");
    }
}

/// A local `replace` target is recognised by `./` or `/`, and both matter.
///
/// The `starts_with('.') || starts_with('/')` test was mutable to `&&`,
/// which requires a target to begin with both characters at once — so every
/// local replacement would be treated as a remote module path instead.
#[test]
fn local_replace_targets_are_recognised_by_either_marker() {
    let mut dot = Resolver::new();
    dot.index_go_modules(&[module("", "m", &[("example.com/a", "./local/a")])]);
    let (_, dot_target) = dot
        .apply_go_replace("example.com/a")
        .expect("replacement stays in the repository");
    assert_eq!(
        dot_target.as_deref(),
        Some("local/a"),
        "a `./` target must resolve to a repo-relative path"
    );

    let mut slash = Resolver::new();
    slash.index_go_modules(&[module("", "m", &[("example.com/b", "/abs/b")])]);
    assert_eq!(
        slash.apply_go_replace("example.com/b"),
        None,
        "an absolute target has no repository-relative identity"
    );

    // A module-path target (neither marker) stays a module path.
    let mut remote = Resolver::new();
    remote.index_go_modules(&[module("", "m", &[("example.com/c", "other.com/c")])]);
    let (_, remote_target) = remote
        .apply_go_replace("example.com/c")
        .expect("replacement stays in the repository");
    assert_eq!(
        remote_target.as_deref(),
        Some("other.com/c"),
        "a module-path target must remain a module path"
    );
}
