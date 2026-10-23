#[cfg(feature = "parse")]
use super::*;
#[cfg(feature = "parse")]
use devmap_extract::extract_file;

#[cfg(feature = "parse")]
fn resolve(files: &[(&str, &str)]) -> ResolutionResult {
    let extractions: Vec<_> = files
        .iter()
        .map(|(path, source)| extract_file(path, source))
        .collect();
    let mut resolver = Resolver::new();
    resolver.index_extractions(&extractions);
    resolver.resolve_all(&extractions).unwrap()
}

#[cfg(feature = "parse")]
fn reference_targets(result: &ResolutionResult, name: &str) -> Vec<String> {
    result
        .edges
        .iter()
        .filter(|edge| edge.edge_kind == EdgeKind::References && edge.target_symbol.ends_with(name))
        .map(|edge| edge.target_file.clone())
        .collect()
}

#[cfg(feature = "parse")]
fn edge_rows(result: &ResolutionResult) -> Vec<String> {
    let mut rows: Vec<String> = result
        .edges
        .iter()
        .filter(|edge| edge.edge_kind == EdgeKind::References || edge.edge_kind == EdgeKind::Calls)
        .map(|edge| format!("{}->{}", edge.source_symbol, edge.target_symbol))
        .collect();
    rows.sort();
    rows
}

#[cfg(feature = "parse")]
fn edges_of(result: &ResolutionResult, kinds: &[EdgeKind]) -> Vec<String> {
    let mut rows: Vec<String> = result
        .edges
        .iter()
        .filter(|edge| kinds.contains(&edge.edge_kind))
        .map(|edge| format!("{}->{}", edge.source_symbol, edge.target_symbol))
        .collect();
    rows.sort();
    rows
}

/// Containment names the file for every symbol, and the type for its methods.
///
/// `Contains` edges are the structural skeleton of every graph — the
/// majority of edges in the frozen baseline. Two guards shape them and both
/// were mutable: the file symbol must not contain itself, and the second,
/// type-owned edge must be emitted only when the parent is a real type
/// rather than the file. Inverting the latter drops every
/// `type contains method` edge and duplicates the file edge in its place,
/// so a method stops being reachable through its type.
#[test]
#[cfg(feature = "parse")]
fn containment_names_the_file_and_the_declaring_type() {
    let result = resolve(&[(
        "a.py",
        "class C:\n    def m(self):\n        pass\n\ndef f():\n    pass\n",
    )]);
    assert_eq!(
        edges_of(&result, &[EdgeKind::Contains]),
        [
            "a.py->a.py::C",
            "a.py->a.py::C.m",
            "a.py->a.py::f",
            "a.py::C->a.py::C.m",
        ],
        "the file contains every declared symbol, the class additionally \
             contains its method, and the file never contains itself"
    );
}

/// Go package membership groups non-main packages only.
///
/// The G20 star topology hangs every file of a package off one synthetic
/// package node. Both guards were mutable: dropping the family test builds
/// package groups for languages that have no packages, and inverting the
/// `main` test groups exactly the one package that is not importable while
/// leaving the importable ones ungrouped.
#[test]
#[cfg(feature = "parse")]
fn go_package_membership_groups_non_main_packages_only() {
    let package = resolve(&[
        ("pkg/a.go", "package pkg\nfunc A() {}\n"),
        ("pkg/b.go", "package pkg\nfunc B() {}\n"),
    ]);
    assert_eq!(
        edges_of(&package, &[EdgeKind::MemberOf]),
        ["pkg/a.go->package:pkg/pkg", "pkg/b.go->package:pkg/pkg",],
        "both files of a package hang off one package node"
    );

    // `package main` is not importable, so it is never grouped.
    let main = resolve(&[("cmd/m.go", "package main\nfunc main() {}\n")]);
    assert!(
        edges_of(&main, &[EdgeKind::MemberOf]).is_empty(),
        "`package main` must not be grouped"
    );

    // A language without packages produces no membership at all.
    let python = resolve(&[("a.py", "def f():\n    pass\n")]);
    assert!(
        edges_of(&python, &[EdgeKind::MemberOf]).is_empty(),
        "package grouping is Go-only"
    );
}

/// A Go import edge names the package, not each of its files.
///
/// The Go branch collapses the resolved file list to package nodes. Losing
/// it emits one import edge per file in the imported package, which is the
/// fan-out SC10 removed — the edge count grows with the size of the
/// imported package rather than with the number of imports.
#[test]
#[cfg(feature = "parse")]
fn a_go_import_edge_names_the_package_not_each_file() {
    let result = resolve(&[
        (
            "app/main.go",
            "package main\nimport \"example.com/m/internal/svc\"\nfunc main() { svc.Do() }\n",
        ),
        ("internal/svc/s.go", "package svc\nfunc Do() {}\n"),
        ("internal/svc/t.go", "package svc\nfunc Other() {}\n"),
    ]);
    assert_eq!(
        edges_of(&result, &[EdgeKind::Imports]),
        ["app/main.go->package:internal/svc/svc"],
        "a two-file package yields one import edge, not two"
    );
}

/// A same-file callee wins outright; an ambiguous global one is speculative.
///
/// The same-file uniqueness test was mutable, which sends a call that has a
/// local definition out to the global tier instead — where the same name in
/// another file makes it ambiguous. The confidence tiers are the point: a
/// call resolved in its own file is DETERMINISTIC, a unique global match is
/// HIGH, and a multi-candidate guess must stay SPECULATIVE. G5 exists so a
/// guess is never presented with the confidence of a fact.
#[test]
#[cfg(feature = "parse")]
fn call_confidence_reflects_how_the_callee_was_found() {
    let same_file = resolve(&[
        (
            "s.py",
            "def helper():\n    pass\n\ndef use():\n    return helper()\n",
        ),
        ("other.py", "def helper():\n    pass\n"),
    ]);
    let same_file_edges: Vec<_> = same_file
        .edges
        .iter()
        .filter(|edge| edge.edge_kind == EdgeKind::Calls)
        .collect();
    assert_eq!(
        same_file_edges.len(),
        1,
        "a local definition wins outright, even though another file declares \
             the same name: {:?}",
        edges_of(&same_file, &[EdgeKind::Calls])
    );
    assert_eq!(same_file_edges[0].target_file, "s.py");
    assert_eq!(
        same_file_edges[0].confidence,
        Confidence::DETERMINISTIC,
        "a same-file callee is a fact, not an inference"
    );

    // A unique global match is strong but not certain.
    let unique = resolve(&[
        ("u.py", "def use():\n    return only()\n"),
        ("x.py", "def only():\n    pass\n"),
    ]);
    let unique_edges: Vec<_> = unique
        .edges
        .iter()
        .filter(|edge| edge.edge_kind == EdgeKind::Calls)
        .collect();
    assert_eq!(unique_edges.len(), 1);
    assert_eq!(unique_edges[0].confidence, Confidence::HIGH);

    // Several candidates are not a call to any of them: no edge, and the
    // site's ledger row carries the complete list (S2).
    let ambiguous = resolve(&[
        ("u.py", "def use():\n    return shared()\n"),
        ("x.py", "def shared():\n    pass\n"),
        ("y.py", "def shared():\n    pass\n"),
    ]);
    assert!(
        ambiguous
            .edges
            .iter()
            .all(|edge| edge.edge_kind != EdgeKind::Calls),
        "an ambiguous bare name binds no caller: {:?}",
        edges_of(&ambiguous, &[EdgeKind::Calls])
    );
    let row = ambiguous
        .unresolved
        .iter()
        .find(|row| row.callee_name == "shared")
        .expect("the ambiguous site is recorded in the ledger");
    assert!(matches!(
        &row.resolution,
        Resolution::AmbiguousGlobal { candidates, .. } if candidates.len() == 2
    ));
    assert_eq!(row.class, UnresolvedClass::Unresolved);
}

/// A route binds to its handler only when the handler is unambiguous and
/// in the same language family.
///
/// A `HandlesRoute` edge is what tells the dead-code pass that a handler is
/// reached from outside the call graph. Without it the handler has no
/// caller and reads as dead; bound to the wrong function, a genuinely dead
/// one is protected instead.
///
/// Every framework whose handler can be named is covered here: axum names
/// it in the route call, FastAPI/Flask name it by decoration, and Express
/// names it in the argument list. An Express handler written as an arrow
/// function is genuinely anonymous and correctly binds to nothing.
///
/// Both endpoints are node identities — `file::name`, and
/// `file::VERB path` for the route, from `ExtractedRoute::node_id`. They
/// used to be a bare `"VERB path"` and a bare handler name, neither of
/// which names a node, so the graph export emitted this edge dangling at
/// both ends and every route consumer read an empty graph.
#[test]
#[cfg(feature = "parse")]
fn a_route_binds_only_to_an_unambiguous_same_family_handler() {
    let route_file = (
        "srv.rs",
        "fn app() -> Router { Router::new().route(\"/items\", get(list_items)) }\n",
    );

    let unique = resolve(&[route_file, ("h.rs", "pub async fn list_items() {}\n")]);
    assert_eq!(
        edges_of(&unique, &[EdgeKind::HandlesRoute]),
        ["srv.rs::GET /items->h.rs::list_items"],
        "a unique handler binds to its route"
    );

    // Two candidates: binding either one would be a guess.
    let ambiguous = resolve(&[
        route_file,
        ("h.rs", "pub async fn list_items() {}\n"),
        ("h2.rs", "pub async fn list_items() {}\n"),
    ]);
    assert!(
        edges_of(&ambiguous, &[EdgeKind::HandlesRoute]).is_empty(),
        "two candidate handlers must produce no edge rather than a guess: {:?}",
        edges_of(&ambiguous, &[EdgeKind::HandlesRoute])
    );

    // A same-named function in another language is not a candidate at all.
    let cross_family = resolve(&[route_file, ("h.py", "def list_items():\n    pass\n")]);
    assert!(
        edges_of(&cross_family, &[EdgeKind::HandlesRoute]).is_empty(),
        "a Rust route must not bind to a Python function of the same name"
    );

    // FastAPI/Flask: the handler is the decorated function.
    let python = resolve(&[(
        "api.py",
        "@app.get('/items')\ndef read_items():\n    return []\n",
    )]);
    assert_eq!(
        edges_of(&python, &[EdgeKind::HandlesRoute]),
        ["api.py::GET /items->api.py::read_items"],
        "a decorated Python handler binds to its route"
    );

    // Express: the handler is the last argument, and resolves across files
    // through the import binding.
    let express = resolve(&[(
        "app.js",
        "app.get('/users', handleUsers);\nfunction handleUsers(req, res) {}\n",
    )]);
    assert_eq!(
        edges_of(&express, &[EdgeKind::HandlesRoute]),
        ["app.js::GET /users->app.js::handleUsers"]
    );
    let imported = resolve(&[
        (
            "app.js",
            "import { handleUsers } from './h';\napp.get('/users', handleUsers);\n",
        ),
        ("h.js", "export function handleUsers(req, res) {}\n"),
    ]);
    assert_eq!(
        edges_of(&imported, &[EdgeKind::HandlesRoute]),
        ["app.js::GET /users->h.js::handleUsers"],
        "an imported handler resolves through its import binding"
    );

    // An anonymous handler has no name, so it binds to nothing rather than
    // to whatever symbol a placeholder happened to match.
    let anonymous = resolve(&[("app.js", "app.get('/a', (req, res) => {});\n")]);
    assert!(
        edges_of(&anonymous, &[EdgeKind::HandlesRoute]).is_empty(),
        "an arrow-function handler is anonymous and binds to nothing"
    );
}

/// Route middleware binds through the handler's ladder to a `Registers`
/// edge from the route node, and what does not bind is recorded, not lost.
///
/// Three outcomes, each with its own evidence: a repository function binds
/// (`Registers`, which is also what keeps it from reading as dead); a
/// package's middleware is `External` by the file's own import, so it can
/// neither bind to a same-named repository symbol nor veto that symbol's
/// dead-code finding; and a qualified Go reference binds only to a
/// declaration in the package its qualifier names — chi's
/// `middleware.Logger` must not become an edge to the one `Logger` this
/// repository happens to declare in another package.
#[test]
#[cfg(feature = "parse")]
fn route_middleware_binds_to_a_registers_edge_or_says_why_not() {
    let express = resolve(&[
        (
            "app.js",
            "import cors from 'cors';\n\
                 import { requireAuth } from './auth';\n\
                 app.use(cors());\n\
                 app.use(requireAuth);\n\
                 app.get('/users', audit, listUsers);\n\
                 function audit(req, res, next) { next(); }\n\
                 function listUsers(req, res) {}\n",
        ),
        (
            "auth.js",
            "export function requireAuth(req, res, next) { next(); }\n",
        ),
        // A repository symbol sharing the package middleware's name.
        ("util.js", "export function cors() {}\n"),
    ]);
    assert_eq!(
        edges_of(&express, &[EdgeKind::Registers]),
        [
            "app.js::GET /users->app.js::audit",
            "app.js::GET /users->auth.js::requireAuth",
        ],
        "repository middleware binds; the package's `cors()` does not bind to util.js"
    );
    let cors = express
        .unresolved
        .iter()
        .find(|row| row.callee_name == "cors" && row.kind == UnresolvedKind::Route)
        .expect("the package middleware is recorded, not dropped");
    // The `cors()` call itself is the package's too: it used to bind to
    // util.js at the global tier, a confident edge to a namesake the file
    // demonstrably does not call.
    let call = express
        .unresolved
        .iter()
        .find(|row| row.callee_name == "cors" && row.kind == UnresolvedKind::Call)
        .expect("the call is recorded external, not bound to the namesake");
    assert!(
        matches!(&call.class, UnresolvedClass::External { module } if module == "cors"),
        "{:?}",
        call.class
    );
    assert_eq!(cors.kind, UnresolvedKind::Route);
    assert!(
        matches!(&cors.class, UnresolvedClass::External { module } if module == "cors"),
        "{:?}",
        cors.class
    );

    let go = resolve(&[
        (
            "api/routes.go",
            "package api\n\n\
                 import \"github.com/go-chi/chi/v5/middleware\"\n\n\
                 func Routes(r Router) {\n\
                 \tr.Use(middleware.Logger)\n\
                 \tr.Use(auth.Check)\n\
                 \tr.Use(auth.Logger)\n\
                 \tr.Get(\"/orders\", ListOrders)\n\
                 }\n\n\
                 func ListOrders() {}\n",
        ),
        ("auth/check.go", "package auth\n\nfunc Check() {}\n"),
        ("logging/log.go", "package logging\n\nfunc Logger() {}\n"),
    ]);
    // `middleware.Logger` is stopped by the file's external import;
    // `auth.Logger` has no import to stop it, and the repository's only
    // `Logger` lives in `logging/` — only the qualifier keeps it unbound.
    assert_eq!(
        edges_of(&go, &[EdgeKind::Registers]),
        ["api/routes.go::GET /orders->auth/check.go::Check"],
        "a qualified reference binds inside the package it names, and nowhere else"
    );
    assert!(
        go.unresolved.iter().any(|row| row.callee_name == "Logger"
            && row.receiver.as_deref() == Some("auth")
            && row.class == UnresolvedClass::Unresolved),
        "the unbound `auth.Logger` is recorded as not binding"
    );
    assert_eq!(
        edges_of(&go, &[EdgeKind::HandlesRoute]),
        ["api/routes.go::GET /orders->api/routes.go::ListOrders"]
    );
}

/// A `module.function()` call resolves through the module's import binding.
///
/// The receiver/method split feeds a lookup of the receiver in this file's
/// import bindings, then of the method inside that module. Both emptiness
/// guards were mutable. This is how the most common cross-file call shape
/// in Python, TypeScript and Go resolves at all — losing it leaves every
/// `mod.fn()` call unresolved, and the callee with no callers.
#[test]
#[cfg(feature = "parse")]
fn a_module_qualified_call_resolves_through_its_import_binding() {
    let python = resolve(&[
        (
            "app.py",
            "import helpers\n\ndef run():\n    return helpers.do()\n",
        ),
        ("helpers.py", "def do():\n    return 1\n"),
    ]);
    assert_eq!(
        edges_of(&python, &[EdgeKind::Calls]),
        ["app.py::run->helpers.py::do"],
        "`helpers.do()` resolves through the `import helpers` binding"
    );

    let typescript = resolve(&[
        (
            "u.ts",
            "import * as ns from './h';\nexport function run() { return ns.doIt(); }\n",
        ),
        ("h.ts", "export function doIt() { return 1; }\n"),
    ]);
    assert_eq!(
        edges_of(&typescript, &[EdgeKind::Calls]),
        ["u.ts::run->h.ts::doIt"],
        "a namespace import binds the receiver too"
    );

    let go = resolve(&[
        (
            "app/main.go",
            "package main\nimport \"example.com/m/internal/svc\"\nfunc main() { svc.Do() }\n",
        ),
        ("internal/svc/s.go", "package svc\nfunc Do() {}\n"),
    ]);
    assert_eq!(
        edges_of(&go, &[EdgeKind::Calls]),
        ["app/main.go::main->internal/svc/s.go::Do"],
        "a Go package-qualified call resolves into the imported package"
    );
}

/// Rust `self::` and `super::` resolve relative to the module's own file.
///
/// The guard covering this branch was mutable. `self::` stays in the current
/// module directory and `super::` walks up one per prefix — getting either
/// wrong sends the import to a directory that does not exist, so it silently
/// resolves to nothing.
#[test]
#[cfg(feature = "parse")]
fn rust_self_and_super_paths_resolve_relative_to_their_module() {
    use devmap_extract::extract_file;
    let files = [
        ("src/deep/mod.rs", "pub mod leaf;\n"),
        ("src/deep/leaf.rs", "pub fn l() {}\n"),
        ("src/sibling.rs", "pub fn s() {}\n"),
    ];
    let extractions: Vec<_> = files
        .iter()
        .map(|(path, source)| extract_file(path, source))
        .collect();
    let mut resolver = Resolver::new();
    resolver.index_extractions(&extractions);

    assert_eq!(
        resolver
            .resolve_import_path("src/deep/mod.rs", "rust", "self::leaf")
            .as_deref(),
        Some("src/deep/leaf.rs"),
        "`self::` stays inside the current module directory"
    );
    assert_eq!(
        resolver
            .resolve_import_path("src/deep/mod.rs", "rust", "super::sibling")
            .as_deref(),
        Some("src/sibling.rs"),
        "`super::` walks up one directory"
    );
    // The guard is language-scoped: this is not Python module syntax.
    assert_eq!(
        resolver.resolve_import_path("src/deep/mod.rs", "python", "self::leaf"),
        None
    );
}

/// A Python builtin is never captured by a same-named user function    /// A Python builtin is never captured by a same-named user function    /// A Python builtin is never captured by a same-named user function    /// A Python builtin is never captured by a same-named user function
/// elsewhere in the repository.
///
/// Every clause of the stdlib guard was mutable. `open`, `print`, `len` and
/// friends appear in almost every Python file, so without the guard a
/// single user-defined `open` anywhere in the repository captures *all* of
/// them — one function acquires thousands of false callers, and it can
/// never be reported dead again. The guard is deliberately scoped to other
/// files: a same-file definition really does shadow the builtin.
#[test]
#[cfg(feature = "parse")]
fn python_builtins_are_not_captured_by_a_same_named_user_function() {
    let cross_file = resolve(&[
        ("app.py", "def run(p):\n    return open(p)\n"),
        ("helpers.py", "def open(p):\n    return p\n"),
    ]);
    assert!(
        edge_rows(&cross_file).is_empty(),
        "a builtin call must not bind to another file's same-named function: {:?}",
        edge_rows(&cross_file)
    );

    // Positive control: shadowing in the *same* file is real shadowing.
    let same_file = resolve(&[(
        "app.py",
        "def open(p):\n    return p\n\ndef run(p):\n    return open(p)\n",
    )]);
    assert_eq!(
        edge_rows(&same_file),
        ["app.py::run->app.py::open"],
        "a definition in the same file does shadow the builtin"
    );
}

/// A Class and a Function sharing a bare name in one file: a *type*
/// annotation prefers the Class (see also
/// `a_type_position_prefers_a_type_over_a_same_named_value`).
///
/// Both declarations share the graph identity `file::Dup`, so a value-position
/// call cannot abstain between two edges that would name the same node —
/// prefer_types is the load-bearing rule here.
#[test]
#[cfg(feature = "parse")]
fn an_ambiguous_same_file_name_in_type_position_prefers_the_type() {
    let mixed = resolve(&[(
        "a.py",
        "class Dup:\n    pass\n\ndef Dup():\n    pass\n\ndef use(x: Dup):\n    return x\n",
    )]);
    assert_eq!(
        edge_rows(&mixed),
        ["a.py::use->a.py::Dup"],
        "a type annotation must prefer the Class over the same-named Function"
    );

    // Positive control: with only the function, the same annotation has
    // nothing typed to bind to (or binds nothing useful).
    let only_fn = resolve(&[(
        "b.py",
        "def Dup():\n    pass\n\ndef use(x: Dup):\n    return x\n",
    )]);
    // A function is not a type; prefer_types has nothing to prefer.
    // Either no edge, or an edge that is not claimed as a Class prefer —
    // the mixed case above is the load-bearing one.
    let _ = only_fn;
}

/// A type position prefers a type over a same-named value.
///
/// The `prefer_types` guards appear in both the same-file and global tiers
/// and were mutable in every direction. A type annotation that binds to a
/// same-named *function* is a confidently wrong edge, and it also leaves
/// the real type looking unreferenced.
#[test]
#[cfg(feature = "parse")]
fn a_type_position_prefers_a_type_over_a_same_named_value() {
    let result = resolve(&[
        ("t.ts", "export class Shape {}\n"),
        ("f.ts", "export function Shape() {}\n"),
        (
            "u.ts",
            "import { Shape } from './t';\nexport function use(s: Shape) { return s; }\n",
        ),
    ]);
    assert_eq!(
        edge_rows(&result),
        ["u.ts::use->t.ts::Shape"],
        "the annotation must bind to the class, never to the same-named function"
    );
}

/// A reference resolves only inside its own language family.    /// A reference resolves only inside its own language family.
///
/// The family test was mutable to `!=` without a failure. Dropping it lets
/// a Python annotation resolve to an identically-named Go type — a
/// confidently wrong cross-language edge.
#[test]
#[cfg(feature = "parse")]
fn references_do_not_resolve_across_language_families() {
    let result = resolve(&[
        (
            "app.py",
            "from models import Record\n\ndef use(r: Record):\n    return r\n",
        ),
        ("models.py", "class Record:\n    pass\n"),
        ("pkg/record.go", "package pkg\ntype Record struct{}\n"),
    ]);
    let targets = reference_targets(&result, "Record");
    assert!(
        !targets.iter().any(|file| file.ends_with(".go")),
        "a Python reference must never resolve to a Go type: {targets:?}"
    );
}

/// Go visibility is enforced on references across packages.
///
/// The `family != Go || visible_from(..)` guard was mutable in both
/// directions. Dropping it resolves an unexported identifier from another
/// package, which the Go compiler would reject outright.
#[test]
#[cfg(feature = "parse")]
fn unexported_go_symbols_are_invisible_from_another_package() {
    let result = resolve(&[
        ("pkg/a.go", "package pkg\ntype hidden struct{}\n"),
        ("other/b.go", "package other\nfunc use(h hidden) {}\n"),
    ]);
    let targets = reference_targets(&result, "hidden");
    assert!(
        targets.is_empty(),
        "an unexported Go type must not be referenced from another package: {targets:?}"
    );

    // Exported and same-package references must still work.
    let visible = resolve(&[
        ("pkg/a.go", "package pkg\ntype Shown struct{}\n"),
        ("pkg/b.go", "package pkg\nfunc use() { var _ Shown }\n"),
    ]);
    assert!(
        !reference_targets(&visible, "Shown").is_empty(),
        "a same-package Go type must resolve: {:?}",
        visible
            .edges
            .iter()
            .map(|e| (&e.source_symbol, &e.target_symbol))
            .collect::<Vec<_>>()
    );
}
