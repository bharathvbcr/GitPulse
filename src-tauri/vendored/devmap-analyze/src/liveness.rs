use crate::model::*;
use devmap_extract::model::*;
use devmap_resolve::model::*;
use std::collections::{HashMap, HashSet};

/// Symbol identity relative to its file, so that `file_path` + `symbol_name`
/// reconstructs the graph id exactly. A method must report as `MyClass.execute`
/// rather than `execute`; the bare form cannot be joined back to a node and
/// collides with any same-named method on a different type in the same file.
fn dead_symbol_identity(symbol: &ExtractedSymbol, file_path: &str) -> String {
    symbol
        .qualified_name
        .strip_prefix(file_path)
        .and_then(|rest| rest.strip_prefix("::"))
        .map(str::to_string)
        .unwrap_or_else(|| symbol.name.clone())
}

/// Directory owning a path, used as half of a Go package identity. Two packages
/// with the same name in different directories are unrelated, so the name alone
/// cannot key the interface join.
fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("")
}

/// Go package identity of an extraction: `(directory, package clause)`.
fn go_package_key(ext: &Extraction) -> Option<(&str, &str)> {
    if ext.language != "go" {
        return None;
    }
    let package = ext.go_package.as_deref().filter(|name| !name.is_empty())?;
    Some((parent_dir(&ext.file_path), package))
}

/// Interface method specs grouped by declaring Go package.
///
/// Scoping to the package is exact, not merely conservative: an *unexported*
/// interface method name is qualified by the package that declared it, so no
/// type outside that package can ever satisfy it. Exported methods can be
/// satisfied cross-package, but an exported Go method already reports
/// `is_exported`, so it never needs this exemption. Widening to the whole corpus
/// would therefore buy nothing and would exempt every same-named method in every
/// unrelated package.
fn go_interface_specs_by_package(
    extractions: &[Extraction],
) -> HashMap<(&str, &str), Vec<&GoInterfaceMethod>> {
    let mut by_package: HashMap<(&str, &str), Vec<&GoInterfaceMethod>> = HashMap::new();
    for ext in extractions {
        let Some(key) = go_package_key(ext) else {
            continue;
        };
        if ext.go_interface_methods.is_empty() {
            continue;
        }
        by_package
            .entry(key)
            .or_default()
            .extend(ext.go_interface_methods.iter());
    }
    by_package
}

/// Grammar keys of the C family, as `Extraction::language` reports them. Metal
/// answers `cpp`, because it borrows that grammar.
fn is_c_family_language(language: &str) -> bool {
    matches!(language, "c" | "cpp" | "objc" | "cuda")
}

/// Whether `path` is a C-family header. Mirrors `is_c_header_path` in the
/// extractor, which decides both `is_exported` and which prototypes become
/// exports. The two must agree on every extension or the join silently half
/// fires: a file the extractor calls a header but this does not would publish
/// exports while its own symbols were still treated as private. The behavioural
/// consequence is pinned by `the_two_header_tables_agree_extension_by_extension`.
fn is_c_header_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    C_HEADER_EXTENSIONS
        .iter()
        .any(|extension| lower.ends_with(extension))
}

/// The C-family header extensions, taken from the frozen `LANGUAGE_SPECS`
/// rather than from what C projects can in principle be spelled with.
///
/// `.h` belongs to C, `.hh`/`.hpp`/`.hxx` to C++, `.cuh` to CUDA. Extensions the
/// registry does not list — `.h++`, `.inl`, `.tcc` — never reach a C-family
/// grammar at all, so listing them here would be configuration that can never
/// fire. An earlier draft did list them and the agreement test caught it.
const C_HEADER_EXTENSIONS: &[&str] = &[".h", ".hh", ".hpp", ".hxx", ".cuh"];

/// Every name published by a C-family header anywhere in the corpus.
///
/// The cross-file half of C-family visibility. A definition in a `.c`/`.cpp` is
/// not itself an export — the header publishes it — so a definition whose name a
/// header declares is public API and must never be a confident dead-code
/// candidate. Extraction cannot answer this because a header and its
/// implementation are different files; this is the same split SC6a used for Go
/// interface methods.
///
/// Measured on a 183-file first-party corpus, this is not a hypothetical: of 118
/// confident findings without it, 37 were libyaml's entire public API
/// (`yaml_emitter_delete`, `yaml_parser_set_input`, …) declared in `yaml.h` and
/// called only by the Swift package that wraps it, and a further block were
/// cgo's `x_cgo_*` entry points declared in `libcgo.h` and called from Go
/// assembly. Every one would have been a proposal to delete working code.
///
/// Scoped corpus-wide rather than per-directory because a C header can be
/// included from anywhere, unlike a Go package. The cost is that a header
/// declaring a very common name exempts same-named definitions elsewhere; that
/// errs toward missing a finding rather than toward proposing a deletion that
/// breaks a build, which is the direction SC6a settled on for the same trade.
fn c_header_exported_names(extractions: &[Extraction]) -> HashSet<&str> {
    let mut names = HashSet::new();
    for ext in extractions {
        if !is_c_family_language(&ext.language) || !is_c_header_path(&ext.file_path) {
            continue;
        }
        for export in &ext.exports {
            if !export.exported_name.is_empty() {
                names.insert(export.exported_name.as_str());
            }
        }
    }
    names
}

/// Why a build-variant finding is exempt rather than merely downgraded.
///
/// Named once so the analyzer and the tests that pin this behaviour cannot
/// drift into describing the same exemption two different ways.
pub const GO_BUILD_VARIANT_REASON: &str =
    "Go build-constrained variant — the call reaches whichever variant this build selects";

/// Symbol identities that exist in a Go package only as mutually exclusive
/// build variants, keyed by `(package, identity)`.
///
/// Go forbids two package-level declarations of one name. A package that
/// declares `configureProcessGroup` in both `procgroup_unix.go` and
/// `procgroup_other.go` therefore cannot compile unless those files are
/// mutually exclusive — and they are, by `//go:build unix` and `//go:build
/// !unix`. Exactly one reaches any given build, so a call naming that identity
/// reaches whichever one compiled. All of them are live.
///
/// The resolver cannot see this: it finds N definitions of one name, cannot
/// pick between them, and emits `AmbiguousGlobal`. Liveness then downgrades
/// every candidate to `only_ambiguous_callers` — which reads as "this might be
/// dead" about code that is guaranteed to be running. Measured on a Go-heavy
/// external corpus, this was **all 16** of its non-exempt findings.
///
/// The join is sound rather than merely convenient because of Go's own
/// visibility rule, which the resolver already enforces in
/// `go_symbol_visible_from`: an *unexported* name resolves only within its own
/// directory, and an exported one reports `is_exported` and never reaches this
/// branch at all. So the ambiguity behind one of these findings is necessarily
/// within a single package, which is precisely where the uniqueness rule bites.
///
/// Requiring **every** declaring file to carry a constraint is the part that
/// keeps this honest. Two unconstrained files declaring one name is not a build
/// variant — it is a package that does not compile, or an extraction bug, and
/// either way it is not evidence that the symbol is alive.
fn go_build_variant_identities(extractions: &[Extraction]) -> HashSet<(&str, &str, String)> {
    // (package key, identity) -> (files seen, files carrying a constraint)
    let mut seen: HashMap<(&str, &str, String), (usize, usize)> = HashMap::new();
    for ext in extractions {
        let Some((dir, package)) = go_package_key(ext) else {
            continue;
        };
        // One file declaring a name twice is not two files declaring it, and Go
        // would reject it anyway; count each file at most once per identity.
        let mut in_this_file: HashSet<String> = HashSet::new();
        for sym in &ext.symbols {
            if sym.kind == SymbolKind::File {
                continue;
            }
            let identity = dead_symbol_identity(sym, &ext.file_path);
            if !in_this_file.insert(identity.clone()) {
                continue;
            }
            let entry = seen.entry((dir, package, identity)).or_insert((0, 0));
            entry.0 += 1;
            entry.1 += usize::from(ext.go_build_constrained);
        }
    }
    seen.into_iter()
        .filter(|(_, (files, constrained))| *files >= 2 && files == constrained)
        .map(|(key, _)| key)
        .collect()
}

pub fn analyze_liveness(
    extractions: &[Extraction],
    resolution: &ResolutionResult,
) -> Vec<DeadSymbolReport> {
    let go_interface_specs = go_interface_specs_by_package(extractions);
    let c_header_exports = c_header_exported_names(extractions);
    let go_build_variants = go_build_variant_identities(extractions);

    // File-scoped called symbols: (target_file, symbol_name_or_qualified_name)
    let mut called_symbols: HashSet<(String, String)> = HashSet::new();
    let mut ambiguous_symbols: HashSet<(String, String)> = HashSet::new();

    for edge in &resolution.edges {
        // Structural edges describe where a symbol *lives*, not that anything
        // uses it. A file containing a symbol, or a type owning its method, is
        // not a call: counting it would mark every declared symbol as reached
        // and silently disable dead-code detection entirely.
        if matches!(
            edge.edge_kind,
            EdgeKind::Contains | EdgeKind::Defines | EdgeKind::MemberOf
        ) {
            continue;
        }
        // An ambiguous or explicitly unresolved edge is evidence that a
        // symbol may be called, not proof that any one candidate is called.
        // Do not turn speculative resolution into a false liveness negative.
        if matches!(
            edge.resolution.as_deref(),
            Some(Resolution::AmbiguousGlobal { .. })
        ) {
            ambiguous_symbols.insert((edge.target_file.clone(), edge.target_symbol.clone()));
            if let Some(short_name) = edge.target_symbol.rsplit("::").next() {
                ambiguous_symbols.insert((edge.target_file.clone(), short_name.to_string()));
                // …and the member name with its owner stripped.
                //
                // `ExtractedSymbol::name` is the bare `toJson`, while an edge
                // names `File::RemoteTaskEntity.toJson`, so without this the two
                // never meet and the ambiguity is recorded against nothing.
                //
                // This became load-bearing when Kotlin extension functions
                // gained their receiver: one Android file declares seven
                // `private fun <T>.toJson()` on seven different types and calls
                // every one of them as `it.toJson()` inside a `map { }`. The
                // receiver `it` cannot be typed, so the resolver emits an
                // *ambiguous* edge naming one candidate — and the other six,
                // each genuinely called, were reported dead at 0.9 confidence.
                // Before the receiver fix all seven collapsed into one symbol
                // and the question could not arise.
                //
                // Bounded to the ambiguous set on purpose: this can only move a
                // finding from 0.9 to 0.4 `only_ambiguous_callers`, never exempt
                // it, so it cannot hide a symbol nothing calls. Doing the same
                // for `called_symbols` would silently exempt every same-named
                // method in the file, which is a different and much worse trade.
                if let Some(member) = short_name.rsplit('.').next() {
                    ambiguous_symbols.insert((edge.target_file.clone(), member.to_string()));
                }
            }
            continue;
        }
        if matches!(
            edge.resolution.as_deref(),
            Some(Resolution::Unresolved { .. })
        ) {
            continue;
        }
        called_symbols.insert((edge.target_file.clone(), edge.target_symbol.clone()));
        if let Some(short_name) = edge.target_symbol.rsplit("::").next() {
            called_symbols.insert((edge.target_file.clone(), short_name.to_string()));
        }
    }

    let mut reports = Vec::new();

    for ext in extractions {
        // X6: Parse-failed files must NEVER be reported as confirmed dead code candidates
        let is_parse_failed = matches!(ext.parse_outcome, ParseOutcome::Failed { .. });

        // A wiring annotation is file-scoped only when it targets the file
        // itself. Symbol-scoped annotations must never be read as file-scoped:
        // one `#[test] fn` would otherwise exempt every symbol in the file,
        // which is the same over-exemption the file-level decorator rule
        // already suffers from.
        let (file_wiring, symbol_wiring): (Vec<_>, Vec<_>) = ext
            .wiring
            .iter()
            .partition(|w| w.target_symbol == ext.file_path);

        let is_file_exempt = is_parse_failed
            || file_wiring.iter().any(|w| {
                matches!(
                    w.kind,
                    WiringKind::Vendored
                        | WiringKind::TestFile
                        | WiringKind::GeneratedFile
                        | WiringKind::ScriptEntry
                        | WiringKind::StructuralExempt
                        | WiringKind::FrameworkDecorator
                        | WiringKind::Launcher
                        | WiringKind::ReExportPackage
                )
            });

        // Per-symbol exemptions: a runtime, framework, or harness reaches the
        // symbol without an explicit call site, or the language forbids the
        // symbol from ever being marked public. Keyed by `qualified_name`,
        // which is what the extractor writes into `target_symbol`.
        let symbol_exemptions: HashMap<&str, &str> = symbol_wiring
            .iter()
            .filter(|w| {
                matches!(
                    w.kind,
                    WiringKind::RuntimeEntryPoint | WiringKind::StructuralExempt
                )
            })
            .map(|w| (w.target_symbol.as_str(), w.details.as_str()))
            .collect();

        // Cross-file half of the Go interface exemption. Extraction closes the
        // same-file case as a wiring annotation; the package-wide join can only
        // happen here, where every file is in scope.
        let go_interface_exemptions: HashMap<&str, String> = go_package_key(ext)
            .and_then(|key| go_interface_specs.get(&key))
            .map(|specs| {
                let param_counts = ext.go_method_param_counts();
                go_interface_method_matches(&ext.symbols, &param_counts, specs.iter().copied())
                    .into_iter()
                    .map(|(symbol, interface_name)| {
                        (
                            symbol.qualified_name.as_str(),
                            go_interface_exemption_reason(interface_name),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();

        let file_reason = if is_parse_failed {
            Some("Parse failed — excluded from dead code candidates".to_string())
        } else {
            file_wiring.first().map(|w| w.details.clone())
        };

        for sym in &ext.symbols {
            // `starts_with("__")` was also tested here and is subsumed by the
            // single-underscore check — dead code that no mutant could kill.
            if sym.kind == SymbolKind::File || sym.name.starts_with('_') {
                continue; // File nodes and underscore-private symbols are exempt
            }

            let is_called = called_symbols.contains(&(ext.file_path.clone(), sym.name.clone()))
                || called_symbols.contains(&(ext.file_path.clone(), sym.qualified_name.clone()));
            let is_ambiguously_called = ambiguous_symbols
                .contains(&(ext.file_path.clone(), sym.name.clone()))
                || ambiguous_symbols.contains(&(ext.file_path.clone(), sym.qualified_name.clone()));

            let overlaps_parse_error = match &ext.parse_outcome {
                ParseOutcome::Partial { error_ranges } => error_ranges.iter().any(|range| {
                    sym.span.start_byte < range.end_byte && range.start_byte < sym.span.end_byte
                }),
                ParseOutcome::Clean | ParseOutcome::Failed { .. } => false,
            };

            let is_exported = sym.is_exported;
            let symbol_exemption: Option<&str> = symbol_exemptions
                .get(sym.qualified_name.as_str())
                .copied()
                .or_else(|| {
                    go_interface_exemptions
                        .get(sym.qualified_name.as_str())
                        .map(String::as_str)
                })
                // A definition whose name a header publishes is this unit's
                // public API, and its callers can lie outside the corpus
                // entirely — a library, a foreign-language binding, hand-written
                // assembly. Keyed on the bare name because that is what a
                // prototype declares; the qualified name belongs to the file
                // that defines it and no header could ever match it.
                .or_else(|| {
                    (is_c_family_language(&ext.language)
                        && !is_c_header_path(&ext.file_path)
                        && c_header_exports.contains(sym.name.as_str()))
                    .then_some("Declared in a C-family header — public interface")
                })
                // A spurious ambiguity, not a real one: the candidates the
                // resolver could not choose between are one identity compiled
                // for different platforms, so the call reached whichever one
                // this build selected.
                //
                // Gated on `is_ambiguously_called` deliberately. If *nothing*
                // calls the identity it is dead in every variant, and the
                // confident branch must keep saying so — a build constraint
                // explains an ambiguity, never an absence of callers.
                .or_else(|| {
                    (is_ambiguously_called
                        && go_package_key(ext)
                            .map(|(dir, package)| {
                                go_build_variants.contains(&(
                                    dir,
                                    package,
                                    dead_symbol_identity(sym, &ext.file_path),
                                ))
                            })
                            .unwrap_or(false))
                    .then_some(GO_BUILD_VARIANT_REASON)
                });

            if !is_called
                && is_ambiguously_called
                && !is_exported
                && !is_file_exempt
                && symbol_exemption.is_none()
                && !overlaps_parse_error
            {
                reports.push(DeadSymbolReport {
                    symbol_name: dead_symbol_identity(sym, &ext.file_path),
                    file_path: ext.file_path.clone(),
                    confidence: 0.4,
                    is_exempt: false,
                    exemption_reason: Some("only_ambiguous_callers".to_string()),
                });
            } else if !is_called
                && !is_exported
                && !is_file_exempt
                && symbol_exemption.is_none()
                && !overlaps_parse_error
            {
                reports.push(DeadSymbolReport {
                    symbol_name: dead_symbol_identity(sym, &ext.file_path),
                    file_path: ext.file_path.clone(),
                    confidence: 0.9,
                    is_exempt: false,
                    exemption_reason: None,
                });
            } else if !is_called {
                reports.push(DeadSymbolReport {
                    symbol_name: dead_symbol_identity(sym, &ext.file_path),
                    file_path: ext.file_path.clone(),
                    confidence: 0.3,
                    is_exempt: true,
                    // Most specific reason wins, so the report names the check
                    // that actually fired rather than a file-wide blanket.
                    exemption_reason: if overlaps_parse_error {
                        Some("Symbol overlaps a tree-sitter parse error".to_string())
                    } else {
                        symbol_exemption
                            .map(str::to_string)
                            .or_else(|| file_reason.clone())
                            .or_else(|| Some("Exported or exempt".to_string()))
                    },
                });
            }
        }
    }

    reports
}

#[cfg(all(test, feature = "parse"))]
mod tests {
    use super::*;

    /// The Go package join key is `(directory, package clause)`, and the
    /// directory half must be real.
    ///
    /// Mutation testing replaced `parent_dir` with a constant without any test
    /// noticing. A constant directory makes every Go file in the repository
    /// look like one package, so an interface declared anywhere would exempt a
    /// same-named method everywhere — silently disabling dead-method detection
    /// across the language.
    #[test]
    fn parent_dir_is_the_real_directory() {
        assert_eq!(parent_dir("pkg/svc/node.go"), "pkg/svc");
        assert_eq!(parent_dir("node.go"), "", "a root file has no directory");
        assert_eq!(parent_dir("a/b/c/d.go"), "a/b/c");
        // Two files in different directories must not share a key.
        assert_ne!(parent_dir("a/x.go"), parent_dir("b/x.go"));
    }

    fn symbol(name: &str, start: usize, end: usize) -> ExtractedSymbol {
        ExtractedSymbol {
            name: name.to_string(),
            qualified_name: format!("f.py::{name}"),
            kind: SymbolKind::Function,
            span: Span {
                start_byte: start,
                end_byte: end,
            },
            is_exported: false,
            docstring: None,
            signature: None,
            parent_symbol: None,
        }
    }

    fn extraction(symbols: Vec<ExtractedSymbol>, error: Option<(usize, usize)>) -> Extraction {
        let mut ext = devmap_extract::extract_file("f.py", "def x(): pass\n");
        ext.symbols = symbols;
        ext.wiring = Vec::new();
        ext.parse_outcome = match error {
            Some((start_byte, end_byte)) => ParseOutcome::Partial {
                error_ranges: vec![TextRange {
                    start_byte,
                    end_byte,
                }],
            },
            None => ParseOutcome::Clean,
        };
        ext
    }

    fn reports_with(ext: Extraction, edges: Vec<ResolvedEdge>) -> Vec<DeadSymbolReport> {
        let resolution = ResolutionResult {
            edges,
            receiver_types: std::collections::BTreeMap::new(),
            reexport_chains: std::collections::BTreeMap::new(),
            unresolved: Vec::new(),
        };
        analyze_liveness(std::slice::from_ref(&ext), &resolution)
    }

    fn call_edge(target_symbol: &str) -> ResolvedEdge {
        ResolvedEdge {
            source_file: "f.py".to_string(),
            target_file: "f.py".to_string(),
            source_symbol: "f.py::caller".to_string(),
            target_symbol: target_symbol.to_string(),
            edge_kind: devmap_extract::model::EdgeKind::Calls,
            confidence: devmap_extract::model::Confidence::DETERMINISTIC,
            resolution: None,
            details: None,
        }
    }

    /// A call marks a symbol live whether it names the bare or qualified form.
    ///
    /// The liveness lookup is a disjunction over both spellings, and flipping it
    /// to a conjunction survived: it would require an edge to name the symbol
    /// *both* ways at once, so essentially every symbol would report as dead.
    #[test]
    fn either_spelling_of_a_call_target_marks_a_symbol_live() {
        for target in ["helper", "f.py::helper"] {
            let out = reports_with(
                extraction(vec![symbol("helper", 0, 10)], None),
                vec![call_edge(target)],
            );
            assert!(
                out.iter()
                    .all(|r| !r.symbol_name.contains("helper") || r.is_exempt),
                "a call naming `{target}` must mark the symbol live: {out:?}"
            );
        }

        // And with no call at all it must still be reported, or the assertion
        // above would pass for the wrong reason.
        let uncalled = reports_with(extraction(vec![symbol("helper", 0, 10)], None), Vec::new());
        assert!(
            uncalled
                .iter()
                .any(|r| r.symbol_name.contains("helper") && !r.is_exempt),
            "an uncalled symbol must be reported: {uncalled:?}"
        );
    }

    /// An *ambiguous* call is recognised under either spelling too.
    ///
    /// `is_ambiguously_called` is a separate disjunction from `is_called`, and
    /// its `||` was still mutable after the `is_called` case was covered.
    /// Collapsing it to `&&` loses the `only_ambiguous_callers` tier: a symbol
    /// whose only inbound callers are ambiguous would be reported as
    /// *confidently* dead rather than at 0.4, which is precisely the
    /// mislabelling that tier exists to prevent.
    #[test]
    fn either_spelling_of_an_ambiguous_call_downgrades_the_verdict() {
        for target in ["helper", "f.py::helper"] {
            let mut ambiguous = call_edge(target);
            ambiguous.confidence = devmap_extract::model::Confidence::SPECULATIVE;
            ambiguous.resolution = Some(std::sync::Arc::new(
                devmap_resolve::model::Resolution::AmbiguousGlobal {
                    candidates: vec![("f.py".to_string(), "helper".to_string())],
                    family: devmap_resolve::model::LangFamily::Python,
                },
            ));
            let out = reports_with(
                extraction(vec![symbol("helper", 0, 10)], None),
                vec![ambiguous],
            );
            let report = out
                .iter()
                .find(|r| r.symbol_name.contains("helper"))
                .unwrap_or_else(|| panic!("helper must be reported for `{target}`: {out:?}"));
            assert!(
                report.confidence <= 0.4,
                "an ambiguously-called symbol must not be confidently dead under \
                 spelling `{target}`, got {report:?}"
            );
        }
    }

    fn reports(ext: Extraction) -> Vec<DeadSymbolReport> {
        let resolution = ResolutionResult {
            edges: Vec::new(),
            receiver_types: std::collections::BTreeMap::new(),
            reexport_chains: std::collections::BTreeMap::new(),
            unresolved: Vec::new(),
        };
        analyze_liveness(std::slice::from_ref(&ext), &resolution)
    }

    /// Underscore-private symbols are skipped; ordinary ones are not.
    ///
    /// The skip is a disjunction and both halves were mutable without a
    /// failure. Collapsing it either reports every private helper as dead or
    /// reports nothing at all.
    #[test]
    fn underscore_private_symbols_are_skipped_and_others_are_not() {
        let out = reports(extraction(
            vec![symbol("_private", 0, 10), symbol("visible", 20, 30)],
            None,
        ));
        assert!(
            !out.iter().any(|r| r.symbol_name.contains("_private")),
            "an underscore-private symbol must not be reported at all: {out:?}"
        );
        assert!(
            out.iter().any(|r| r.symbol_name.contains("visible")),
            "an uncalled public symbol must still be reported: {out:?}"
        );
    }

    /// Span overlap with a parse error is half-open, and the boundary matters.
    ///
    /// X6 exempts symbols overlapping an error range, because a symbol parsed
    /// out of broken source is not evidence of anything. Both `<` comparisons
    /// were mutable to `<=`, which would make a symbol merely *adjacent* to an
    /// error range exempt — quietly suppressing real findings next to any
    /// syntax error.
    #[test]
    fn parse_error_overlap_is_half_open_at_both_ends() {
        // Symbol [20,30) ends exactly where the error range begins: no overlap.
        let touching_before = reports(extraction(vec![symbol("before", 20, 30)], Some((30, 40))));
        assert!(
            touching_before
                .iter()
                .any(|r| r.symbol_name.contains("before") && !r.is_exempt),
            "a symbol ending exactly at an error range does not overlap it: {touching_before:?}"
        );

        // Symbol [40,50) begins exactly where the error range ends: no overlap.
        let touching_after = reports(extraction(vec![symbol("after", 40, 50)], Some((30, 40))));
        assert!(
            touching_after
                .iter()
                .any(|r| r.symbol_name.contains("after") && !r.is_exempt),
            "a symbol starting exactly at an error range end does not overlap it: {touching_after:?}"
        );

        // Genuine overlap must be exempt.
        let overlapping = reports(extraction(vec![symbol("inside", 32, 38)], Some((30, 40))));
        assert!(
            overlapping
                .iter()
                .all(|r| !r.symbol_name.contains("inside") || r.is_exempt),
            "a symbol inside an error range must be exempt: {overlapping:?}"
        );
    }
}
