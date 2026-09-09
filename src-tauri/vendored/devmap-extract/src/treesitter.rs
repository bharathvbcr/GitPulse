use crate::content_hash;
use crate::frameworks::extract_framework_routes;
use crate::model::*;
use crate::wiring::extract_wiring_annotations;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use tree_sitter::{Language, Node, Parser};

/// Grammars compiled from vendored C, bound directly to our tree-sitter.
///
/// The published Rust wrappers for these languages pin tree-sitter 0.20, whose
/// `Language` is a distinct type from ours — they cannot be linked at all. The
/// generated parser is fine (ABI 15, which 0.25 accepts), so the wrapper was
/// the only obstacle. Declaring the entry point here keeps these grammars on
/// exactly the same runtime as every other one, with no second tree-sitter in
/// the dependency graph.
pub mod vendored {
    use tree_sitter::Language;
    use tree_sitter_language::LanguageFn;

    unsafe extern "C" {
        fn tree_sitter_vue() -> *const ();
        fn tree_sitter_liquid() -> *const ();
    }

    /// SAFETY: `tree_sitter_vue` is the entry point generated into the vendored
    /// `parser.c`, compiled into this binary by `build.rs`, and returns a
    /// pointer to a `'static` grammar table.
    pub const VUE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_vue) };

    /// SAFETY: as `VUE`.
    pub const LIQUID: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_liquid) };

    pub fn vue() -> Language {
        VUE.into()
    }

    pub fn liquid() -> Language {
        LIQUID.into()
    }
}

/// Whether `path` is Metal, asked of the frozen registry rather than of `lang`.
///
/// `lang` is the *grammar* key and answers `"cpp"` for Metal and for C++ alike,
/// which is correct for choosing a parser and useless for any rule that holds
/// for one language and not the other.
fn is_metal_path(path: &str) -> bool {
    crate::languages::detect_extractor_id(std::path::Path::new(path))
        == Some(crate::languages::ExtractorId::Metal)
}

/// How long one file may spend inside tree-sitter before the parse is abandoned.
///
/// Measured, not guessed. A 4,000-byte C++ source consisting of 2,000 nested
/// braces takes **131 seconds** to parse (`tree-sitter-cpp` 0.23, release
/// build); the same input costs Ruby 8.8 s and Python 58 ms. Generated,
/// minified and machine-emitted sources hit exactly this shape, and until now a
/// single such file stalled the whole build with no diagnostic — a `dev map` on
/// a repository containing one would look like a hang, and a daemon rebuild
/// would hold its lock for the duration.
///
/// Five seconds is far above any legitimate file measured on the corpora in
/// this repository (the slowest real source parses in single-digit
/// milliseconds) and far below the pathological case.
pub const DEFAULT_PARSE_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// Grammars deliberately not routed to, and why.
///
/// `cobol`: the vendored grammar **does not terminate** on malformed input —
/// `"a\0b\0c\n"` (six bytes) and `"\u{feff}????\n"` each ran past three minutes,
/// while the same 14 hostile inputs across the other 34 grammars complete in
/// 5.03 s total. No in-process bound stops it: tree-sitter's progress callback
/// is never reached from inside a scanner that is spinning, and a spinning
/// thread cannot be killed. One `.cbl` file with a stray NUL or a mangled
/// header hung `dev map` outright and left a daemon holding its writer lock.
///
/// Unlinking costs nothing measurable, which is what settled it: on a realistic
/// COBOL program the grammar parsed `Clean` and yielded **only the File node**
/// — zero declarations, zero calls — and the bounded fallback scanner recovers
/// nothing either. COBOL was already one of the twelve languages with no call
/// extraction. Files still get a File node and stay addressable as edge
/// targets.
///
/// Re-linking requires a grammar that terminates, proven against
/// `devmap-extract/tests/adversarial_corpus.rs`.
const UNSAFE_GRAMMARS: &[(&str, &str)] = &[(
    "cobol",
    "the vendored tree-sitter-cobol grammar does not terminate on malformed \
     input and cannot be bounded in-process; it yielded no declarations or \
     calls even on well-formed source, so it is not linked",
)];

/// The grammar this build parses `lang` with, and the key it records.
///
/// Hoisted out of `extract_treesitter_with_budget` so it has a name and a
/// second caller. `langimports` needs the same table to prove, per language,
/// that an import node kind it matches is a kind the linked grammar actually
/// produces — a claim that was previously only checkable by running the whole
/// extractor and inferring the answer from what came out.
///
/// The returned `&'static str` is the *grammar* key, which is not always the
/// language key: ArkTS answers `typescript` and Metal answers `cpp`, because
/// they reuse those grammars.
pub(crate) fn grammar_for(lang: &str) -> Option<(&'static str, Language)> {
    match lang {
        "python" => Some(("python", tree_sitter_python::LANGUAGE.into())),
        "javascript" => Some(("javascript", tree_sitter_javascript::LANGUAGE.into())),
        "typescript" => Some((
            "typescript",
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        )),
        "tsx" => Some(("tsx", tree_sitter_typescript::LANGUAGE_TSX.into())),
        "rust" => Some(("rust", tree_sitter_rust::LANGUAGE.into())),
        "go" => Some(("go", tree_sitter_go::LANGUAGE.into())),
        "hcl" => Some(("hcl", tree_sitter_hcl::LANGUAGE.into())),
        "vue" => Some(("vue", vendored::vue())),
        "liquid" => Some(("liquid", vendored::liquid())),
        "astro" => Some(("astro", tree_sitter_astro_next::LANGUAGE.into())),
        "kotlin" => Some(("kotlin", tree_sitter_kotlin_ng::LANGUAGE.into())),
        "svelte" => Some(("svelte", tree_sitter_svelte_ng::LANGUAGE.into())),
        "java" => Some(("java", tree_sitter_java::LANGUAGE.into())),
        "csharp" => Some(("csharp", tree_sitter_c_sharp::LANGUAGE.into())),
        "php" => Some(("php", tree_sitter_php::LANGUAGE_PHP.into())),
        "ruby" => Some(("ruby", tree_sitter_ruby::LANGUAGE.into())),
        "c" => Some(("c", tree_sitter_c::LANGUAGE.into())),
        "cpp" => Some(("cpp", tree_sitter_cpp::LANGUAGE.into())),
        "objc" => Some(("objc", tree_sitter_objc::LANGUAGE.into())),
        "cuda" => Some(("cuda", tree_sitter_cuda::LANGUAGE.into())),
        "swift" => Some(("swift", tree_sitter_swift::LANGUAGE.into())),
        "scala" => Some(("scala", tree_sitter_scala::LANGUAGE.into())),
        "dart" => Some(("dart", tree_sitter_dart::LANGUAGE.into())),
        "pascal" => Some(("pascal", tree_sitter_pascal::LANGUAGE.into())),
        "lua" => Some(("lua", tree_sitter_lua::LANGUAGE.into())),
        "luau" => Some(("luau", tree_sitter_luau::LANGUAGE.into())),
        "r" => Some(("r", tree_sitter_r::LANGUAGE.into())),
        "cfml" => Some(("cfml", tree_sitter_cfml::LANGUAGE_CFML.into())),
        "erlang" => Some(("erlang", tree_sitter_erlang::LANGUAGE.into())),
        "solidity" => Some(("solidity", tree_sitter_solidity::LANGUAGE.into())),
        "nix" => Some(("nix", tree_sitter_nix::LANGUAGE.into())),
        // Not in the frozen Python 35 and so not in LANGUAGE_SPECS, but
        // `detect_language` already produces these keys from its fallback table.
        "shell" => Some(("shell", tree_sitter_bash::LANGUAGE.into())),
        "sql" => Some(("sql", tree_sitter_sequel::LANGUAGE.into())),
        _ => None,
    }
}

/// Language keys that have a [`grammar_for`] arm.
///
/// Kept beside the match so a new arm without an entry here fails
/// [`linked_grammar_keys_cover_every_grammar_for_arm`]. Hosts compare
/// [`linked_grammar_count`] against a vendored expectation; the count is
/// derived from this list rather than asserted, so it cannot silently drift.
const GRAMMAR_LANGUAGE_KEYS: &[&str] = &[
    "python",
    "javascript",
    "typescript",
    "tsx",
    "rust",
    "go",
    "hcl",
    "vue",
    "liquid",
    "astro",
    "kotlin",
    "svelte",
    "java",
    "csharp",
    "php",
    "ruby",
    "c",
    "cpp",
    "objc",
    "cuda",
    "swift",
    "scala",
    "dart",
    "pascal",
    "lua",
    "luau",
    "r",
    "cfml",
    "erlang",
    "solidity",
    "nix",
    "shell",
    "sql",
];

/// Distinct tree-sitter grammar keys this binary can load.
///
/// Excludes languages refused by [`UNSAFE_GRAMMARS`]: those still have a
/// generated parser in the tree, but this build will not route to them.
/// Hosts (GitPulse, the Python seam) compare this number against the count
/// they expect from the same revision, so a binary built without a grammar
/// cannot claim the same capability as one that has it.
pub fn linked_grammar_count() -> usize {
    linked_grammar_keys().len()
}

/// The grammar keys behind [`linked_grammar_count`], sorted and deduplicated.
pub fn linked_grammar_keys() -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = GRAMMAR_LANGUAGE_KEYS
        .iter()
        .filter(|lang| {
            !UNSAFE_GRAMMARS
                .iter()
                .any(|(unsafe_lang, _)| unsafe_lang == *lang)
        })
        .filter_map(|lang| grammar_for(lang).map(|(grammar, _)| grammar))
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

pub fn extract_treesitter(path: &str, lang: &str, source: &str) -> Extraction {
    extract_treesitter_with_budget(path, lang, source, DEFAULT_PARSE_BUDGET)
}

/// What a bounded parse attempt produced.
///
/// Distinguishing these is the point: before, "no grammar arm for this
/// language", "the grammar is linked but refused to load", and "the parser gave
/// up" all fell through to the same `unavailable_extraction`, which then
/// asserted *"no linked tree-sitter grammar for {lang}"* — false for the last
/// two, and in the grammar-load case exactly what a tree-sitter ABI regression
/// would look like while the build stayed green.
enum ParseAttempt {
    Parsed(tree_sitter::Tree),
    /// The parser exceeded its budget and was cancelled.
    Budget(std::time::Duration),
    /// `set_language` refused a grammar that *is* linked.
    GrammarLoadFailed,
    /// The parser returned no tree for a reason it did not name.
    NoTree,
}

fn parse_within_budget(
    parser: &mut Parser,
    source: &str,
    deadline: std::time::Instant,
) -> ParseAttempt {
    let started = std::time::Instant::now();
    let mut over_budget = false;
    let tree = {
        // The progress callback is polled by tree-sitter during the parse and
        // returning `true` cancels it; this is the only way to bound a parse
        // that has already entered a pathological state.
        //
        // Against the *shared* deadline, not a budget of its own: the parse is
        // the first phase of the file's extraction, not a separately funded
        // one.
        let mut cancel = |_: &tree_sitter::ParseState| -> bool {
            if std::time::Instant::now() >= deadline {
                over_budget = true;
                return true;
            }
            false
        };
        let options = tree_sitter::ParseOptions::new().progress_callback(&mut cancel);
        let bytes = source.as_bytes();
        parser.parse_with_options(
            &mut |offset: usize, _| {
                if offset < bytes.len() {
                    &bytes[offset..]
                } else {
                    &[][..]
                }
            },
            None,
            Some(options),
        )
    };
    match tree {
        Some(tree) if !over_budget => ParseAttempt::Parsed(tree),
        // A cancelled parse can still hand back a partial tree. It describes a
        // prefix of the file, so accepting it would publish a truncated symbol
        // set as a complete one — the shape this codebase treats as worse than
        // a visible failure.
        _ if over_budget => ParseAttempt::Budget(started.elapsed()),
        _ => ParseAttempt::NoTree,
    }
}

#[cfg(test)]
thread_local! {
    static EXTRACTION_FINISH_HOOK: Cell<Option<fn()>> = const { Cell::new(None) };
}

pub fn extract_treesitter_with_budget(
    path: &str,
    lang: &str,
    source: &str,
    budget: std::time::Duration,
) -> Extraction {
    // One deadline for the whole call, taken once.
    //
    // It used to be two: the parse was given `budget`, and then the walk was
    // given `Instant::now() + budget` *after* the parse had already spent its
    // own, so a file could legitimately consume twice the budget its caller
    // asked for and still be published `Clean`. Everything after the walk —
    // `go_method_sets`, the Python passes, the framework matcher, `clonesig` —
    // had no deadline at all. `budget` is what the caller is promised; this is
    // the only clock that decides whether the promise was kept.
    let started = std::time::Instant::now();
    let deadline = started + budget;
    let _budget = BudgetGuard::arm(deadline);
    let extraction = extract_treesitter_before_deadline(path, lang, source, budget, deadline);
    // All tree, parser and parent-index temporaries have been released. A
    // helper in the final scope/embedded pass can still have stopped early, or
    // cleanup can have crossed the deadline after the last phase check.
    if !matches!(
        extraction.parse_outcome,
        ParseOutcome::Failed { .. } | ParseOutcome::Skipped { .. }
    ) && extraction_overran(deadline)
    {
        refused_extraction(
            path,
            lang,
            source,
            format!(
                "extraction finalization exceeded the {budget:?} budget; no symbols are claimed for this file"
            ),
        )
    } else {
        extraction
    }
}

fn extract_treesitter_before_deadline(
    path: &str,
    lang: &str,
    source: &str,
    budget: std::time::Duration,
    deadline: std::time::Instant,
) -> Extraction {
    // Before anything else: a language whose grammar is deliberately not linked
    // must be refused here, not fall through to `unavailable_extraction`. That
    // path's reason — "no linked tree-sitter grammar for {lang}" — is true but
    // useless: it reads as "upstream has no grammar", which is why VB.NET is
    // absent, and would leave a maintainer free to re-link a grammar that hangs.
    if let Some(why) = UNSAFE_GRAMMARS
        .iter()
        .find(|(unsafe_lang, _)| *unsafe_lang == lang)
        .map(|(_, why)| *why)
    {
        return refused_extraction(path, lang, source, why.to_string());
    }

    // And a *file* whose shape makes the budget the thing that decides the
    // answer. `UNSAFE_GRAMMARS` above refuses by language; this refuses by
    // path, and for the same underlying reason — a parse whose cost is not
    // bounded by anything the extractor controls.
    //
    // A minified bundle is the one shape that reliably reaches the boundary: it
    // is dense syntax with no line structure, it is large, and it is committed
    // outside the `node_modules/` and `dist/` directories discovery already
    // refuses. The real one in this project's tree — 177,599 bytes over five
    // lines — cost 4.7–5.0 s against a 5 s budget, so which of `Clean`,
    // `Partial` and `Failed` it published depended on the load average. That is
    // 449 symbols appearing and disappearing between builds, and a dead-code
    // verdict changing with them.
    //
    // Declining costs nothing, because there was nothing to lose. Every
    // identifier in the file is a minifier's single letter: `t`, `e`, `n`. As
    // search results they are noise, as call edges they resolve to nothing, and
    // as dead-code candidates they are already exempt — `is_vendored_path`
    // matches the same file and hangs the annotation that exempts it.
    if crate::wiring::is_minified_bundle(path) {
        return skipped_extraction(
            path,
            lang,
            source,
            "not parsed: minified bundle, whose only declarations are a \
             minifier's mangled names"
                .to_string(),
        );
    }

    let mut parser = Parser::new();

    let ts_lang: Option<(&str, Language)> = grammar_for(lang);

    if let Some((grammar, ts_l)) = ts_lang {
        // A NUL byte means this is not source text, and some grammars do not
        // merely mis-parse it — they hang. Measured: `"a\0b\0c\n"`, six bytes,
        // ran for over three minutes in `tree-sitter-cobol` with no sign of
        // terminating, and the parse budget could not stop it because the
        // progress callback is never reached from inside a scanner that is
        // spinning. A `.cbl` file with a stray NUL would hang `dev map`
        // outright, and a daemon would hold its lock forever.
        //
        // Rejecting at the boundary is the only bound that holds for every
        // grammar, including a vendored one whose scanner this repository does
        // not control. It is also correct on its own terms: a file containing a
        // NUL is binary, and every extractor here assumes text.
        if source.as_bytes().contains(&0) {
            return refused_extraction(
                path,
                lang,
                source,
                format!(
                    "source contains a NUL byte at offset {} and is not text; refused before \
                     parsing because some grammars do not terminate on it",
                    source.as_bytes().iter().position(|b| *b == 0).unwrap_or(0)
                ),
            );
        }
        let attempt = if parser.set_language(&ts_l).is_ok() {
            parse_within_budget(&mut parser, source, deadline)
        } else {
            ParseAttempt::GrammarLoadFailed
        };
        match &attempt {
            ParseAttempt::Budget(elapsed) => {
                return refused_extraction(
                    path,
                    lang,
                    source,
                    format!(
                        "parse of {} bytes exceeded the {:?} budget for grammar {grammar} \
                         (stopped at {elapsed:?}); no symbols are claimed for this file",
                        source.len(),
                        budget
                    ),
                );
            }
            ParseAttempt::GrammarLoadFailed => {
                return refused_extraction(
                    path,
                    lang,
                    source,
                    format!(
                        "grammar {grammar} for {lang} is linked but failed to load; this is a \
                         grammar/ABI fault, not an absent grammar"
                    ),
                );
            }
            ParseAttempt::NoTree => {
                // The variant that exists to name "the parser returned no tree
                // and did not say why" was the one not routed to the refusal
                // path. Falling through published every file of the language as
                // `RegexFallback` with the reason "no linked tree-sitter grammar
                // for {lang}" — false, cache-admitted under a real content hash,
                // and exempting the whole language from dead-code analysis while
                // the build stayed green. That is the exact scenario
                // `refused_extraction`'s own doc warns about.
                return refused_extraction(
                    path,
                    lang,
                    source,
                    format!(
                        "grammar {grammar} for {lang} returned no tree and named no reason; \
                         no symbols are claimed for this file"
                    ),
                );
            }
            ParseAttempt::Parsed(_) => {}
        }
        if let ParseAttempt::Parsed(tree) = attempt {
            return crate::parent_index::with_index(&tree, deadline, || {
                // The budget covers parse *and* walk *and* everything after it.
                // Measured: a 4,000-byte C++ file of 2,000 nested braces parses
                // in 3 ms and then spends 199 s in the walk, so bounding the
                // parse alone bounds nothing that actually hurts.
                let content_hash = content_hash(source);

                let root = tree.root_node();
                let parse_outcome = parse_outcome_of(root, source, is_metal_path(path));

                let mut symbols = Vec::new();
                let mut imports = Vec::new();
                let mut calls = Vec::new();
                let mut exports = Vec::new();
                let mut references = Vec::new();

                let file_symbol_name = path.to_string();
                symbols.push(ExtractedSymbol {
                    name: path.rsplit('/').next().unwrap_or(path).to_string(),
                    qualified_name: file_symbol_name.clone(),
                    kind: SymbolKind::File,
                    span: Span {
                        start_byte: 0,
                        end_byte: source.len(),
                    },
                    is_exported: true,
                    docstring: None,
                    signature: None,
                    parent_symbol: None,
                    body_signature: None,
                    declaration_hash: None,
                });

                // File-level wiring first, so a file-scoped exemption always
                // precedes the symbol-scoped ones and stays the file's reason.
                let mut wiring = extract_wiring_annotations(path, source);

                // Every phase below shares one refusal, so a file cut short
                // in the tail cannot be reported differently from one cut short
                // in the walk.
                let over_budget = |phase: &str| {
                    format!(
                        "extraction of {} bytes exceeded the {budget:?} budget for grammar \
                         {grammar} while {phase}; no symbols are claimed for this file",
                        source.len()
                    )
                };
                let walk_completed = walk_tree(
                    root,
                    source,
                    lang,
                    &file_symbol_name,
                    &mut symbols,
                    &mut imports,
                    &mut calls,
                    &mut exports,
                    &mut references,
                    &mut wiring,
                    deadline,
                );
                if !walk_completed {
                    // The partial `symbols`/`calls` collected so far describe a
                    // prefix of the tree. Publishing them would be a truncated
                    // extraction wearing a clean outcome, and every consumer
                    // reads an absent symbol as one that does not exist.
                    return refused_extraction(
                        path,
                        lang,
                        source,
                        over_budget("walking the syntax tree"),
                    );
                }

                let (go_interface_methods, go_method_params) = if lang == "go" {
                    let sets = go_method_sets(root, source, &file_symbol_name);
                    go_interface_method_exemptions(&symbols, &sets.0, &sets.1, &mut wiring);
                    sets
                } else {
                    (Vec::new(), Vec::new())
                };
                if extraction_overran(deadline) {
                    // Labelled by what actually ran for *this* language. The
                    // block above is gated on `lang == "go"`, so naming it
                    // unconditionally reported a Python file as having spent
                    // its budget "deriving Go method sets" — a stage that did
                    // not execute, for a language that has no method sets. The
                    // refusal was right and the accounting was not, which sends
                    // a maintainer after a pass that never ran.
                    //
                    // For every other language the only work between
                    // `walk_tree`'s last deadline check and here is the walk's
                    // own tail: the clock is read every `DEADLINE_CHECK_STRIDE`
                    // nodes, so a walk can return `true` having crossed the
                    // deadline within the following stride.
                    return refused_extraction(
                        path,
                        lang,
                        source,
                        over_budget(if lang == "go" {
                            "deriving Go method sets"
                        } else {
                            "finishing the syntax-tree walk"
                        }),
                    );
                }

                if lang == "python" {
                    let declared = python_all_exports(source);
                    if !declared.is_empty() {
                        for symbol in symbols.iter_mut() {
                            if symbol.kind != SymbolKind::File && declared.contains(&symbol.name) {
                                symbol.is_exported = true;
                            }
                        }
                    }
                    // Module-level bindings survive when `__all__` names
                    // them, or when they are a second name for something
                    // already referenceable. Emitting the rest would add a
                    // symbol for every private module constant in the
                    // repository, whose only graph effect is dead-code noise;
                    // dropping an alias, on the other hand, deletes a name other
                    // modules import.
                    let aliases = python_module_aliases(root, source);
                    symbols.retain(|symbol| {
                        symbol.kind != SymbolKind::Variable
                            || declared.contains(&symbol.name)
                            || aliases.contains(&symbol.name)
                    });
                }

                for symbol in symbols.iter().filter(|symbol| symbol.is_exported) {
                    if !exports
                        .iter()
                        .any(|export| export.exported_name == symbol.name)
                    {
                        exports.push(ExtractedExport {
                            exported_name: symbol.name.clone(),
                            local_name: Some(symbol.name.clone()),
                            module_specifier: None,
                            span: symbol.span.clone(),
                        });
                    }
                }
                exports.sort_by(|a, b| {
                    (&a.exported_name, &a.module_specifier, a.span.start_byte).cmp(&(
                        &b.exported_name,
                        &b.module_specifier,
                        b.span.start_byte,
                    ))
                });
                references.sort_by(|a, b| {
                    (a.span.start_byte, a.span.end_byte, &a.name).cmp(&(
                        b.span.start_byte,
                        b.span.end_byte,
                        &b.name,
                    ))
                });

                // Framework matching is an auxiliary extractor. A matcher
                // failure cannot retroactively turn a successful grammar
                // parse into a failed parse outcome, but it must remain
                // visible to downstream diagnostics.
                let (routes, diagnostics) = match extract_framework_routes(lang, source) {
                    Ok(routes) => (routes, Vec::new()),
                    Err(reason) => (Vec::new(), vec![reason]),
                };

                // Body signatures are stamped here, after every language arm
                // has finished pushing symbols, rather than inside `walk_tree`.
                // The walk emits symbols from more than thirty grammar-specific
                // branches; hashing in each is thirty chances to add a grammar
                // later and silently omit its signatures.
                crate::clonesig::stamp_signatures(&mut symbols, root, source);
                crate::clonesig::stamp_declaration_hashes(&mut symbols, root, source);

                let local_bindings = collect_site_bindings(
                    root,
                    source,
                    &file_symbol_name,
                    &calls,
                    &references,
                    &imports,
                );

                // The last gate before the file is published. Everything above
                // has either finished or latched `WALK_OVERRAN`; this is what
                // stops a run that went past its budget from being handed out
                // as a complete read of the file.
                if extraction_overran(deadline) {
                    return refused_extraction(
                        path,
                        lang,
                        source,
                        over_budget("stamping signatures"),
                    );
                }

                // Bound rather than returned: the embedded-script merge below
                // still has to run, and it mutates this value in place.
                let mut extraction = Extraction {
                    file_path: path.to_string(),
                    language: lang.to_string(),
                    content_hash,
                    engine: ExtractionEngine::TreeSitter {
                        grammar: grammar.to_string(),
                        grammar_version: ts_l.abi_version() as u32,
                    },
                    parse_outcome,
                    symbols,
                    imports,
                    calls,
                    exports,
                    references,
                    diagnostics,
                    routes,
                    wiring,
                    go_package: (lang == "go")
                        .then(|| go_package_name(root, source))
                        .flatten(),
                    go_build_constrained: lang == "go" && go_build_constrained(path, source),
                    go_interface_methods,
                    go_method_params,
                    // After `walk_tree`, so the per-scope cache it warmed is
                    // reused rather than every callable's subtree being walked
                    // a second time.
                    scope_locals: collect_scope_locals(root, source, &file_symbol_name),
                    local_bindings,
                    source_code: Some(source.to_string()),
                };

                // The code inside a template language's `<script>` blocks, in
                // the outer file's own coordinates. Last, and after
                // `collect_scope_locals`, for two reasons: each inner parse
                // clears the per-scope local cache that call reuses, and the
                // merge restores the orderings established just above. For
                // every language whose registry entry declares no embedded
                // languages — all but Svelte, Vue, Astro and Liquid — this
                // returns after one registry lookup.
                // The one clock, not a second one started here. This used to
                // pass a deadline computed inside the arm as
                // `Instant::now() + budget`, which restarted the budget after
                // the parse had already spent part of it. `deadline` is the
                // clock `BudgetGuard` is armed with and the one every other
                // stage checks.
                crate::embedded::merge_embedded_scripts(
                    &mut extraction,
                    root,
                    source,
                    lang,
                    deadline,
                );
                #[cfg(test)]
                EXTRACTION_FINISH_HOOK.with(|hook| {
                    if let Some(finish) = hook.take() {
                        finish();
                    }
                });
                extraction
            });
        }
    }

    unavailable_extraction(path, lang, source)
}

/// Grammar false-errors on a lone `&` in JSX text (valid TSX; still open
/// upstream in tree-sitter-javascript#366). Those must not mark the file
/// Partial or downstream will treat a successful extract as degraded.
///
/// `is_metal` opts a `.metal` file into the second benign class — see
/// [`is_benign_metal_qualifier`]. It is a language fact, not a grammar fact, so
/// it cannot be read off the grammar key: Metal and C++ share `"cpp"`.
/// Push every child of `node` onto a traversal worklist.
///
/// **Never iterate children by index.** `Node::child(i)` walks the sibling
/// chain from the first child on every call, so `for i in 0..node.child_count()`
/// is O(n^2) in the number of children. That is not a theoretical cost: a
/// degenerate parse gives the root one child per token, and a 140 KB file of
/// `"fn (((("` produced a root with 100,000 children. Walking it by index took
/// **37 s**; walking the same tree with a `TreeCursor` visited the same nodes in
/// **2.5 ms**. The whole 873x parse-budget overrun measured in
/// `tests/budget_is_a_real_bound.rs` was this, not tree-sitter.
///
/// A `TreeCursor` steps to the next sibling in O(1), so these helpers are
/// linear. They exist so the fast form has one owner and the slow form has one
/// place to be warned about.
fn push_children<'tree>(node: Node<'tree>, worklist: &mut Vec<Node<'tree>>) {
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            worklist.push(cursor.node());
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// [`push_children`], but pushed last-child-first.
///
/// Callers that `pop()` from the worklist get first-child-first — document
/// order — which several walks depend on for deterministic output (R4).
fn push_children_reversed<'tree>(node: Node<'tree>, worklist: &mut Vec<Node<'tree>>) {
    let mark = worklist.len();
    push_children(node, worklist);
    worklist[mark..].reverse();
}

/// [`push_children`] over named children only.
fn push_named_children<'tree>(node: Node<'tree>, worklist: &mut Vec<Node<'tree>>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        worklist.push(child);
    }
}

fn parse_outcome_of(root: Node, source: &str, is_metal: bool) -> ParseOutcome {
    if !root.has_error() {
        return ParseOutcome::Clean;
    }
    let mut error_ranges = Vec::new();
    let mut stack = vec![root];
    let mut cursor = root.walk();
    while let Some(node) = stack.pop() {
        if (node.is_error() || node.is_missing())
            && !is_benign_jsx_ampersand(node, source)
            && !(is_metal && is_benign_metal_qualifier(node, source))
        {
            error_ranges.push(TextRange {
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
            });
        }
        cursor.reset(node);
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.has_error() {
                    stack.push(child);
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }
    if error_ranges.is_empty() {
        ParseOutcome::Clean
    } else {
        ParseOutcome::Partial { error_ranges }
    }
}

/// Metal declaration qualifiers that the C++ grammar has no production for.
///
/// Metal Shading Language is C++14 plus two keyword sets that appear exactly
/// where C++ expects a declaration specifier: the function qualifiers
/// (`kernel`, `vertex`, `fragment`) and the address spaces (`device`,
/// `constant`, `threadgroup`, `threadgroup_imageblock`, `thread`, `ray_data`,
/// `object_data`). tree-sitter-cpp consumes the qualifier as though it were the
/// type and then reports the *real* type — or the declarator that follows it —
/// as an ERROR node.
///
/// Metal's `[[...]]` attributes are deliberately absent: `[[buffer(0)]]`,
/// `[[stage_in]]`, `[[position]]`, `[[attribute(n)]]` and
/// `[[thread_position_in_grid]]` are C++11 attribute syntax and were measured to
/// parse cleanly, in argument position included. They are not a source of error
/// nodes and need no exemption.
const METAL_DECLARATION_QUALIFIERS: &[&str] = &[
    "kernel",
    "vertex",
    "fragment",
    "device",
    "constant",
    "threadgroup",
    "threadgroup_imageblock",
    "thread",
    "ray_data",
    "object_data",
];

/// Whether an ERROR node exists only because a Metal declaration qualifier sits
/// where the C++ grammar expected a type.
///
/// This is a *measured* equivalence, not an assumption that errors are
/// harmless. Neutralising every qualifier in a realistic 111-line Metal shader
/// and re-extracting it as C++ yields the identical 13 symbols with identical
/// names and kinds — the qualifier costs a `Partial` outcome and nothing else.
/// Across 55 real `.metal` files (11,274 lines) the rule takes the corpus from
/// 54 of 55 files `Partial` with 2,584 error ranges to **0 and 0**, while the
/// recovered declaration count is unchanged at 287 and no file's symbol set
/// differs — so it clears the outcome without moving a single symbol.
///
/// Left unclassified, that outcome is charged against the file forever: it
/// makes the diagnostic worthless (every Metal file is permanently degraded, so
/// a real grammar regression is invisible against the background) and it arms
/// `liveness`'s `overlaps_parse_error` to exempt every shader in the corpus from
/// dead-code analysis.
///
/// Two guards keep it fail-closed, and they do different jobs.
///
/// **Single token.** Real breakage is reported by tree-sitter as one wide ERROR
/// spanning whitespace, statements and braces; the measured qualifier errors are
/// always exactly one token — `void`, `float4`, `Uniforms`, `kA`, `*`. Requiring
/// one token is therefore the guard that actually stops a broken file from being
/// absorbed, and `real_metal_breakage_still_reports_partial` pins it with a case
/// (junk immediately after a bare `kernel`) that reports Clean without it.
///
/// **Adjacency.** The qualifier must be reachable by stepping back over
/// whitespace and at most one intervening *word* token — which separates
/// `kernel void f` (nothing between) from `constant float kA` and
/// `device float *p` (one type token between). Note the real reach here is the
/// word chain, not the step count: the walk stops at the first punctuation, so
/// it cannot cross out of a declaration specifier list. No source could be
/// constructed where a larger step count changes the verdict, so the bound of
/// two is a conservative tightening rather than a measured boundary; it is
/// pinned from below instead, by the three tests that fail if it drops to one.
fn is_benign_metal_qualifier(node: Node, source: &str) -> bool {
    let text = get_node_text(node, source);

    // The qualifier standing alone as the error, rather than displacing the
    // token after it. Metal's address-space *cast* — `(device bfloat *)O`, which
    // is how a shader reinterprets a pointer — puts the keyword somewhere C++
    // has no production for it at all, so the grammar rejects the keyword
    // itself. An exact match against the closed table is as tight a signal as
    // this rule ever gets.
    if METAL_DECLARATION_QUALIFIERS.contains(&text.as_str()) {
        return true;
    }

    // A zero-width MISSING `::`, which is how the grammar reports an atomic in
    // an address space: given `device atomic_float *d`, it consumes `device` as
    // the type, then reads `atomic_float` as the start of a qualified name and
    // inserts the scope operator it expected next. Such a node has no text, so
    // the single-token guard below cannot judge it and the adjacency walk is
    // the whole test. Restricted to `::` deliberately — real breakage inserts a
    // MISSING `}` or `)`, and admitting those by their emptiness alone would
    // hand the exemption exactly the files it must never cover.
    let missing_scope_operator = node.is_missing() && node.kind() == "::";

    if !missing_scope_operator
        && (text.is_empty()
            || text.chars().any(char::is_whitespace)
            || text.contains(['{', '}', ';']))
    {
        return false;
    }
    let mut cursor = node.start_byte();
    for _ in 0..2 {
        let Some((start, word)) = preceding_token(source, cursor) else {
            return false;
        };
        // `device atomic<float> *p` is the same declaration as
        // `device atomic_uint *p` with a template argument list on the type, so
        // the token before the declarator is `>`. Resolving the group back to
        // its type name makes the two read alike; leaving it out would fix one
        // spelling of the construct and not the other.
        let (start, word) = if word == ">" {
            let Some(open) = template_group_start(source, start) else {
                return false;
            };
            let Some(before) = preceding_token(source, open) else {
                return false;
            };
            before
        } else {
            (start, word)
        };
        if METAL_DECLARATION_QUALIFIERS.contains(&word) {
            return true;
        }
        // Only a bare type token may stand between the qualifier and the error.
        if !word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return false;
        }
        cursor = start;
    }
    false
}

/// Byte offset of the `<` matching a `>` that ends at `close`, or `None`.
///
/// Bounded and fail-closed: a template argument list in a declaration specifier
/// is short, and `>` is far more often a comparison, so the scan gives up at
/// the first statement boundary and at a fixed distance rather than searching
/// the file for a character that will always eventually appear.
fn template_group_start(source: &str, close: usize) -> Option<usize> {
    const MAX_SPAN: usize = 256;
    let bytes = source.as_bytes();
    let floor = close.saturating_sub(MAX_SPAN);
    let mut depth = 1usize;
    let mut index = close;
    while index > floor {
        index -= 1;
        match bytes[index] {
            b'>' => depth += 1,
            b'<' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            b';' | b'{' | b'}' | b'(' | b')' => return None,
            _ => {}
        }
    }
    None
}

/// The token immediately before `at`, as `(start_byte, text)`, skipping
/// whitespace.
///
/// Returns `None` at the start of the file, and for any slice that is not a
/// character boundary — a non-ASCII byte in a preceding comment or string
/// literal therefore fails its caller closed rather than panicking.
fn preceding_token(source: &str, at: usize) -> Option<(usize, &str)> {
    let bytes = source.as_bytes();
    let mut end = at.min(bytes.len());
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    if end == 0 {
        return None;
    }
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let start = if is_word(bytes[end - 1]) {
        let mut start = end;
        while start > 0 && is_word(bytes[start - 1]) {
            start -= 1;
        }
        start
    } else {
        end - 1
    };
    source.get(start..end).map(|text| (start, text))
}

fn is_benign_jsx_ampersand(node: Node, source: &str) -> bool {
    let text = get_node_text(node, source);
    if !text.contains('&') || text.contains('<') || text.contains('{') || text.contains('}') {
        return false;
    }
    let mut current = bounded_parent(node);
    while let Some(parent) = current {
        if matches!(
            parent.kind(),
            "jsx_element"
                | "jsx_text"
                | "jsx_fragment"
                | "jsx_opening_element"
                | "jsx_self_closing_element"
        ) {
            return true;
        }
        current = bounded_parent(parent);
    }
    false
}

/// Whether a JSX tag names a component binding rather than an intrinsic host
/// element.
///
/// This is the JSX transform's own rule, not a heuristic. A `JSXIdentifier`
/// beginning with a lowercase letter compiles to a *string* (`"div"`), so it
/// references no binding and can never join to a symbol; a capitalized one
/// compiles to the identifier itself. A `JSXMemberExpression` (`<Foo.Bar/>`) is
/// always a value. A `JSXNamespacedName` (`<svg:circle/>`) is always intrinsic.
///
/// `_` and `$` are identifier starts that are not lowercase letters, and the
/// transform treats them as value references, so they count as components here.
fn jsx_tag_is_component(tag: &str) -> bool {
    if tag.contains(':') {
        return false;
    }
    if tag.contains('.') {
        return true;
    }
    match tag.chars().next() {
        Some(first) => !first.is_lowercase(),
        None => false,
    }
}

/// The named type a Go composite literal constructs, unwrapping the type
/// constructors that wrap it.
///
/// Deliberately **not** folded into `go_type_name`, which answers a different
/// question — "what is this value's type, for receiver dispatch". Unwrapping
/// there would bind a `[]Foo` parameter to `Foo` and let `x.Method()` resolve
/// against `Foo`'s method set, which is the SC9 class of confidently-wrong edge.
/// Here the wrapper is irrelevant: `[]*Foo{...}` really does construct `Foo`.
///
/// Fail-closed: a literal with no named type at its core — `[]string{...}`,
/// `struct{...}{}` — yields `None` and emits no call, rather than recording the
/// type expression text (`[]string`) as a callee that matches no symbol.
fn go_composite_literal_type<'tree>(
    node: Node<'tree>,
    source: &str,
    depth: usize,
) -> Option<Node<'tree>> {
    if depth > 16 {
        return None;
    }
    match node.kind() {
        // `qualified_type` (`pkg.T`) is never predeclared, so it is always kept.
        "type_identifier" => {
            (!GO_PREDECLARED_TYPES.contains(&get_node_text(node, source).as_str())).then_some(node)
        }
        "qualified_type" => Some(node),
        // `[]T`, `[N]T` and `*T` all construct T.
        "slice_type" | "array_type" => node
            .child_by_field_name("element")
            .and_then(|inner| go_composite_literal_type(inner, source, depth + 1)),
        "pointer_type" => node
            .named_child(0)
            .and_then(|inner| go_composite_literal_type(inner, source, depth + 1)),
        // `map[K]V{...}` constructs V values; the key is not constructed.
        "map_type" => node
            .child_by_field_name("value")
            .and_then(|inner| go_composite_literal_type(inner, source, depth + 1)),
        "generic_type" => node
            .child_by_field_name("type")
            .and_then(|inner| go_composite_literal_type(inner, source, depth + 1)),
        _ => None,
    }
}

/// Go's predeclared type names, from the language specification.
///
/// None of these can be a node in the graph — they are declared by the language,
/// not by any indexed file — so emitting a call to `string` only produces an
/// unresolvable row, and risks joining to an unrelated user symbol of that name.
const GO_PREDECLARED_TYPES: &[&str] = &[
    "any",
    "bool",
    "byte",
    "comparable",
    "complex64",
    "complex128",
    "error",
    "float32",
    "float64",
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "rune",
    "string",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "uintptr",
];

/// Names a Python module declares public via `__all__`.
///
/// `__all__` is the module's explicit export list, so a symbol named in it is
/// public API by definition and must never be reported as dead code. Handles
/// both `__all__ = [...]` and the `__all__ += [...]` accumulation form, and
/// spans multiple lines because real modules wrap long lists.
///
/// Three things must *not* be read as a declaration, because each would publish
/// a name the module never exported and so suppress a real dead-code finding:
/// a longer identifier that merely ends in `__all__` (`my__all__ = [...]`), an
/// occurrence inside a comment, and any occurrence not followed by an
/// assignment. The first two were claimed by this comment but not implemented —
/// the token was matched anywhere in the source, so `my__all__ = ["nope"]`
/// exported `nope`.
///
/// Known limit: the comment test looks for `#` earlier on the same line, so a
/// `#` inside a string literal preceding `__all__` on that same line would hide
/// a real declaration. `__all__` sits on its own top-level line in practice.
fn python_all_exports(source: &str) -> std::collections::BTreeSet<String> {
    let mut exported = std::collections::BTreeSet::new();
    let mut line_start = 0usize;
    for line in source.split_inclusive('\n') {
        // `match_indices` walks non-overlapping matches left to right, so the
        // scan cannot fail to advance. A hand-rolled cursor here previously made
        // termination depend on the increment arithmetic being right — mutating
        // that one expression hung the extractor instead of producing a wrong
        // answer, which is a worse failure than any it was guarding.
        for (at, matched) in line.match_indices("__all__") {
            let searched = at + matched.len();
            let before = &line[..at];

            // Stand-alone token only: not the tail of a longer identifier, and
            // not inside a comment.
            if before
                .chars()
                .next_back()
                .is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
                || before.contains('#')
            {
                continue;
            }

            // The list itself may wrap across lines, so it is read from the
            // whole source rather than from this line.
            let after = &source[line_start + searched..];
            let Some(open) = after.find(['[', '(']) else {
                continue;
            };
            let assignment = after[..open].trim();
            if !(assignment == "=" || assignment == "+=") {
                continue;
            }

            let list = &after[open..];
            let end = list.find([']', ')']).unwrap_or(list.len());
            let mut chars = list[..end].chars();
            while let Some(quote) = chars.next() {
                if quote != '"' && quote != '\'' {
                    continue;
                }
                let mut name = String::new();
                for inner in chars.by_ref() {
                    if inner == quote {
                        break;
                    }
                    name.push(inner);
                }
                if !name.is_empty() {
                    exported.insert(name);
                }
            }
        }
        line_start += line.len();
    }
    exported
}

/// Module-level names a Python file declares as another name for something
/// already referenceable — `TestEvidence = VerificationEvidence`.
///
/// Python has no `export` keyword, so a re-export alias is written as a plain
/// module-level assignment and is indistinguishable by node kind from a private
/// constant. Every module-level binding is emitted as a candidate and then
/// dropped unless `__all__` names it, which is right for constants and wrong for
/// aliases: an alias is a *symbol other files import by that name*.
/// `src/devcouncil/domain/evidence.py` declares no `__all__`, so `TestEvidence`
/// existed nowhere in the graph, and the twenty import bindings that name it
/// resolved to a file which — as far as the resolver could see — does not
/// declare it, and fell off the resolution ladder entirely.
///
/// The rule is deliberately narrow: the right-hand side must be a bare name or a
/// dotted name, so this admits aliases and re-exports and nothing else. Keeping
/// every module-level assignment instead would add a node for every private
/// constant in the repository, whose only graph effect is dead-code noise, which
/// is the reason the `__all__` filter exists at all.
fn python_module_aliases(root: Node, source: &str) -> std::collections::BTreeSet<String> {
    let mut aliases = std::collections::BTreeSet::new();
    let mut cursor = root.walk();
    for statement in root.children(&mut cursor) {
        // Module level only. A class-body or function-body assignment is a
        // field or a local, neither of which another module can import.
        let assignment = match statement.kind() {
            "assignment" => statement,
            "expression_statement" => match statement.named_child(0) {
                Some(inner) if inner.kind() == "assignment" => inner,
                _ => continue,
            },
            _ => continue,
        };
        let (Some(left), Some(right)) = (
            assignment.child_by_field_name("left"),
            assignment.child_by_field_name("right"),
        ) else {
            continue;
        };
        if left.kind() != "identifier" || !matches!(right.kind(), "identifier" | "attribute") {
            continue;
        }
        let name = get_node_text(left, source);
        if !name.is_empty() && name != "__all__" {
            aliases.insert(name);
        }
    }
    aliases
}

/// Extraction for a language with no linked grammar.
///
/// Emits the `File` node, then tries tier-2 pattern recovery for declarations.
///
/// A file that reached here used to contribute *no nodes whatsoever*: the
/// `File` node was pushed only on the tree-sitter path above, so there was no
/// symbol, no span, and nothing for an edge to point at. On two real trees that
/// silently swallowed every `.proto`, `.ps1`, `.vb`, `.vue` and `.metal` file
/// in them. Both halves are closed now — the file is always addressable, and
/// its declarations are recovered when the language has any to recover.
///
/// The two *declaration* outcomes stay labelled differently on purpose. When
/// the scanner finds declarations the file reports `RegexFallback` /
/// `ParseOutcome::Fallback`, so no consumer can mistake a pattern-matched
/// symbol for a parsed one. When it finds none the outcome stays `Failed`,
/// because no declaration was recovered and the `File` node is not a
/// declaration — reporting success on the strength of it would be the lie the
/// labelling exists to prevent.
///
/// A parse this build *refused to complete*, reported as such.
///
/// Distinct from `unavailable_extraction`, which answers "there is no grammar
/// for this language". Here a grammar exists and something went wrong with it —
/// a budget overrun or a load fault — and saying "no linked tree-sitter grammar
/// for {lang}" would be false. It matters most for the case that looks like
/// nothing: a tree-sitter ABI break would otherwise downgrade every file of a
/// language to regex fallback, be cache-admitted, exempt them all from
/// dead-code analysis, and leave the build green.
///
/// No declarations are recovered by pattern here. A file whose parse was
/// abandoned has an unknown structure, and a pattern scan over it would produce
/// a plausible-looking symbol set that nothing verified.
pub(crate) fn refused_extraction(
    path: &str,
    lang: &str,
    source: &str,
    reason: String,
) -> Extraction {
    unparsed_extraction(
        path,
        lang,
        source,
        ExtractionEngine::Unavailable {
            requested_language: lang.to_string(),
        },
        ParseOutcome::Failed { reason },
    )
}

/// A parse this build **chose not to attempt**, reported as such.
///
/// Distinct from `refused_extraction` in both fields, and both differences are
/// the same claim: nothing went wrong here. The engine is `NotApplicable`
/// rather than `Unavailable` because no grammar was wanted — one exists and is
/// linked, and asking it was simply not worth the clock — and the outcome is
/// `Skipped` rather than `Failed` because "we did not try" is not a symptom a
/// maintainer should be sent to investigate, and because a decision about a
/// file's name is a stable verdict that may be cached.
///
/// Shares its body with the refusal path deliberately. Both emit the `File`
/// node and the wiring annotations and claim nothing else; when that shape was
/// two copies, one of them was fixed and the other was not.
fn skipped_extraction(path: &str, lang: &str, source: &str, reason: String) -> Extraction {
    unparsed_extraction(
        path,
        lang,
        source,
        ExtractionEngine::NotApplicable {
            language: lang.to_string(),
        },
        ParseOutcome::Skipped { reason },
    )
}

/// The `File`-node-only extraction both no-parse paths publish.
fn unparsed_extraction(
    path: &str,
    lang: &str,
    source: &str,
    engine: ExtractionEngine,
    parse_outcome: ParseOutcome,
) -> Extraction {
    Extraction {
        file_path: path.to_string(),
        language: lang.to_string(),
        content_hash: content_hash(source),
        engine,
        parse_outcome,
        // The File node is still emitted: being unable to parse a file is not a
        // reason to deny it exists, and every edge that targets it needs a node.
        symbols: vec![ExtractedSymbol {
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            qualified_name: path.to_string(),
            kind: SymbolKind::File,
            span: Span {
                start_byte: 0,
                end_byte: source.len(),
            },
            is_exported: true,
            docstring: None,
            signature: None,
            parent_symbol: None,
            body_signature: None,
            declaration_hash: None,
        }],
        imports: Vec::new(),
        calls: Vec::new(),
        exports: Vec::new(),
        references: Vec::new(),
        diagnostics: Vec::new(),
        routes: Vec::new(),
        wiring: extract_wiring_annotations(path, source),
        go_package: None,
        go_build_constrained: false,
        go_interface_methods: Vec::new(),
        go_method_params: Vec::new(),
        scope_locals: Vec::new(),
        local_bindings: Vec::new(),
        source_code: Some(source.to_string()),
    }
}

fn unavailable_extraction(path: &str, lang: &str, source: &str) -> Extraction {
    // Whether a grammar was ever expected for this format. A `.proto` or `.ps1`
    // with no linked grammar is a gap in coverage; a `.md` is not, and K5 is
    // the record of what conflating them cost — 294 of 1,310 files reported as
    // parse failures, all prose and data, hiding the 16 real ones.
    let declarative = crate::fallback::applies_to(lang);
    let scan = if declarative {
        crate::fallback::scan_declarations(path, source)
    } else {
        // Prose and data formats declare nothing; see
        // `fallback::NON_DECLARATIVE_LANGUAGES` for what scanning them produced.
        crate::fallback::FallbackScan {
            symbols: Vec::new(),
            truncated: 0,
            skipped_long_lines: 0,
        }
    };
    let recovered_count = scan.symbols.len();
    let recovered = recovered_count > 0;

    // The `File` node, which this path used to omit entirely.
    //
    // It is pushed on the tree-sitter path above as the first symbol of every
    // parsed file, but that push sits *inside* the `if let Some(ts_lang)` block,
    // so a file with no linked grammar was recorded in `generation_files` and
    // contributed nothing to the graph: not addressable by a file-level query,
    // and unusable as the target of any edge. That is placement, not design —
    // nothing downstream wants a nodeless file.
    //
    // Emitted for *every* grammarless file, including the prose and data
    // formats that get no declaration scan. Being unable to recover a file's
    // declarations is not a reason to deny that the file exists; a Markdown
    // document that something links to still has to be a valid edge target.
    //
    // Safe against the dead-code surface by construction: `File` nodes are
    // exempt in `liveness.rs` (both the Go duplicate-identity count and the
    // dead-symbol sweep) and are skipped as `Contains` sources in the resolver,
    // so this widens what the graph can address without adding a single
    // dead-symbol candidate.
    let mut symbols = Vec::with_capacity(scan.symbols.len() + 1);
    symbols.push(ExtractedSymbol {
        name: path.rsplit('/').next().unwrap_or(path).to_string(),
        qualified_name: path.to_string(),
        kind: SymbolKind::File,
        span: Span {
            start_byte: 0,
            end_byte: source.len(),
        },
        is_exported: true,
        docstring: None,
        signature: None,
        parent_symbol: None,
        // A file has no body to fingerprint; clone detection is over symbol
        // bodies, and `None` means "not computed", never "no duplicates".
        body_signature: None,
        declaration_hash: None,
    });
    symbols.extend(scan.symbols);
    let mut diagnostics: Vec<String> = Vec::new();
    if scan.truncated > 0 {
        // Reported, not silently dropped: the symbol list is a prefix, and a
        // consumer that reads it as complete would conclude the rest of the
        // file declares nothing.
        diagnostics.push(format!(
            "fallback declaration scan stopped at {} symbols; {} more were dropped",
            crate::fallback::MAX_FALLBACK_SYMBOLS,
            scan.truncated
        ));
    }
    Extraction {
        file_path: path.to_string(),
        language: lang.to_string(),
        content_hash: content_hash(source),
        engine: match (recovered, declarative) {
            (true, _) => ExtractionEngine::RegexFallback {
                requested_language: lang.to_string(),
            },
            // A grammar was wanted for this language and was not there: a real
            // gap, and `Extraction::is_parse_failure` counts it.
            (false, true) => ExtractionEngine::Unavailable {
                requested_language: lang.to_string(),
            },
            // Prose or data. No grammar was ever expected, so this is not a
            // failure to report — see `ExtractionEngine::NotApplicable`.
            (false, false) => ExtractionEngine::NotApplicable {
                language: lang.to_string(),
            },
        },
        parse_outcome: match (recovered, declarative) {
            // The dropped count rides on the *reason*, not on `diagnostics`.
            // `for_durable_store` clears `diagnostics` before the payload is
            // written to `generation_files.extraction_json` and the extraction
            // cache, and nothing in the workspace reads that field in
            // production — so the truncation the scanner correctly computed was
            // erased on the way to storage, and the stored record of a 2,500
            // declaration file read as a complete recovery of 2,000. A
            // truncation is part of the result, not a note about it.
            (true, _) => ParseOutcome::Fallback {
                reason: {
                    // Two different ways to lose a declaration, and they are
                    // not interchangeable. The scan cap drops declarations the
                    // scanner *counted*, so the true total is known exactly.
                    // A skipped line was never pattern-matched at all, so how
                    // many declarations it held is unknown — which makes the
                    // total a lower bound, and saying so is the whole point.
                    let counted_total = recovered_count + scan.truncated;
                    let mut caveats = Vec::new();
                    if scan.truncated > 0 {
                        caveats.push(format!("{} dropped at the scan cap", scan.truncated));
                    }
                    if scan.skipped_long_lines > 0 {
                        caveats.push(format!(
                            "{} line(s) over {} bytes never scanned, so the total is a lower \
                             bound",
                            scan.skipped_long_lines,
                            crate::fallback::MAX_LINE_BYTES
                        ));
                    }
                    // The "X of Y" form is used only when Y is genuinely
                    // known — that is, when every declaration was counted and
                    // some were dropped afterwards. If a line was never
                    // scanned, printing "2 of 2" would state a total the
                    // scanner cannot know, which is the same overclaim in a
                    // smaller font.
                    let counted = if scan.truncated > 0 {
                        format!("{recovered_count} of {counted_total} declaration(s)")
                    } else {
                        format!("{recovered_count} declaration(s)")
                    };
                    if caveats.is_empty() {
                        format!(
                            "no linked tree-sitter grammar for {lang}; {counted} recovered by \
                             pattern"
                        )
                    } else {
                        format!(
                            "no linked tree-sitter grammar for {lang}; {counted} recovered by \
                             pattern, {} — the symbol list is a prefix, not a set",
                            caveats.join(" and ")
                        )
                    }
                },
            },
            (false, true) => ParseOutcome::Failed {
                reason: format!("no linked tree-sitter grammar for {lang}"),
            },
            (false, false) => ParseOutcome::Failed {
                reason: format!(
                    "{lang} is a prose/data format with no declarations to parse; \
                     indexed as a File node"
                ),
            },
        },
        symbols,
        imports: Vec::new(),
        calls: Vec::new(),
        exports: Vec::new(),
        references: Vec::new(),
        diagnostics,
        routes: Vec::new(),
        wiring: extract_wiring_annotations(path, source),
        go_package: None,
        go_build_constrained: false,
        go_interface_methods: Vec::new(),
        go_method_params: Vec::new(),
        scope_locals: Vec::new(),
        local_bindings: Vec::new(),
        source_code: Some(source.to_string()),
    }
}

// The explicit worklist avoids stack overflow on adversarially deep syntax
// while preserving tree-sitter's deterministic pre-order traversal.
#[allow(clippy::too_many_arguments)]
fn walk_tree(
    root: Node,
    source: &str,
    lang: &str,
    file_symbol_name: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    calls: &mut Vec<ExtractedCall>,
    exports: &mut Vec<ExtractedExport>,
    references: &mut Vec<ExtractedReference>,
    wiring: &mut Vec<WiringAnnotation>,
    deadline: std::time::Instant,
) -> bool {
    let mut worklist = vec![root];
    let mut since_check = 0u32;
    while let Some(node) = worklist.pop() {
        // A `Cell` read, cheap enough for every node, unlike a clock read.
        if walk_overran() {
            return false;
        }
        // The clock is read every `DEADLINE_CHECK_STRIDE` nodes rather than
        // every node: an ordinary file walks millions of nodes and each read is
        // a real cost, while the pathological case that needs bounding takes
        // orders of magnitude longer than the stride can hide.
        since_check += 1;
        if since_check >= DEADLINE_CHECK_STRIDE {
            since_check = 0;
            if std::time::Instant::now() >= deadline {
                return false;
            }
        }
        extract_node(
            node,
            source,
            lang,
            file_symbol_name,
            symbols,
            imports,
            calls,
            exports,
            references,
            wiring,
        );
        // Beside `extract_node` rather than inside it: heritage is one decision
        // that fifteen languages spell differently, and putting it in the
        // per-language match would scatter it across every arm.
        crate::heritage::push_heritage_references(node, source, lang, file_symbol_name, references);
        push_children_reversed(node, &mut worklist);
    }
    true
}

/// Nodes walked between deadline checks. See `walk_tree`.
const DEADLINE_CHECK_STRIDE: u32 = 256;

/// The `{ a, b as c }` clause of a JavaScript/TypeScript import or export
/// statement, or `None` when the statement has no binding clause at all.
///
/// Located structurally because the text scan it replaces was wrong in both
/// directions on ordinary source. `text.find('{')` and `text.find('}')` were
/// taken independently with no ordering check, so
/// `export const isClose = (c) => c === '}' || c === '{';` — valid, idiomatic
/// JavaScript — produced an inverted slice range and panicked, aborting the
/// whole build (`extract_all` runs under rayon and release sets
/// `panic = "abort"`). With the two literals the other way round the same scan
/// did not panic; it fabricated an import *and* an export of a name spelled
/// `'`, which is the quieter half of the same defect. `export default function
/// f() { … }` was read the same way, with the function body parsed as a binding
/// list.
///
/// The grammar already answers the question exactly: a binding clause is an
/// `export_clause` (`export { … }`) or a `named_imports` (`import { … }`,
/// reached through the statement's `import_clause`), and a statement that has
/// neither binds no names. Both are direct or once-nested children, so this
/// looks no deeper and cannot mistake a nested object literal for a clause.
fn js_binding_clause(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    let mut inner_cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "export_clause" | "named_imports" => return Some(child),
            "import_clause" => {
                for inner in child.named_children(&mut inner_cursor) {
                    if inner.kind() == "named_imports" {
                        return Some(inner);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn find_string_child(node: Node, source: &str) -> Option<String> {
    if let Some(s) = node.child_by_field_name("source") {
        return Some(
            get_node_text(s, source)
                .trim_matches('"')
                .trim_matches('\'')
                .to_string(),
        );
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == "string" {
            return Some(
                get_node_text(c, source)
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string(),
            );
        }
    }
    None
}

/// Graph identity of the function or method lexically containing `node`.
///
/// Without this every call edge is attributed to the file rather than to the
/// caller, which collapses `app.py::my_func -> app.py::open` into
/// `app.py -> app.py::open` and erases caller granularity from impact and
/// trace. Returns `None` at file scope, where the file itself is the caller.
pub(crate) fn enclosing_callable_qualified(
    node: Node,
    source: &str,
    file_symbol_name: &str,
) -> Option<String> {
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        // Python defaults are evaluated by the enclosing scope, before the
        // function's parameters exist. Calls in the body keep the callable.
        if parent.kind() == "function_definition"
            && node.kind() == "call"
            && parent.child_by_field_name("name").is_some()
            && field_contains(parent, "parameters", node)
        {
            ancestor = bounded_parent(parent);
            continue;
        }
        // The C family derives its name and its owner together: no C-family
        // declaration has a `name` field for `callable_binding_name` to read,
        // and an out-of-line definition names its owner inside its own
        // declarator rather than through any ancestor. Both come from the same
        // helper the symbol emitter uses, so the scope string and the node
        // identity cannot drift apart — which is precisely how they drifted
        // before: `callable_binding_name` returned `None` for every C-family
        // function, so every reference and every call made inside one was
        // attributed to the *file*. On the measurement corpus all 117 C-family
        // `References` edges had the file as their source.
        if is_c_family_callable(parent) {
            if let Some((owner, name)) = c_callable_identity(parent, source) {
                return Some(match owner {
                    Some(type_name) => format!("{file_symbol_name}::{type_name}.{name}"),
                    None => format!("{file_symbol_name}::{name}"),
                });
            }
        }
        if let Some(name) = callable_binding_name(parent, source) {
            // A Go method is owned by its receiver type, which is a field of
            // the `method_declaration` itself rather than an enclosing node, so
            // the ancestor walk in `enclosing_type_name` cannot find it.
            //
            // Without this, every method in a file reported the bare
            // `file::name`, so `func (a *A) Read()` and `func (b *B) Read()`
            // were indistinguishable as scopes — which is what let two types'
            // receiver bindings collide (SC9). It also left Go method call
            // edges naming a source symbol (`file::Read`) that matches no
            // node's qualified name (`file::A.Read`), making those edges
            // unjoinable to the node they come from.
            let owner = (parent.kind() == "method_declaration")
                .then(|| go_receiver(parent, source).map(|(_, type_name)| type_name))
                .flatten()
                .or_else(|| enclosing_type_name(parent, source));
            // Must produce exactly the identity the symbol itself carries, or
            // every call made inside a nested function names a source no node
            // has — the same unjoinable-edge failure as SC9/SC10. `parent` is
            // the callable we just matched, so recursing from it walks the rest
            // of the enclosing scope chain.
            return Some(match owner {
                Some(type_name) => format!("{file_symbol_name}::{type_name}.{name}"),
                None => scoped_qualified_name(parent, source, file_symbol_name, &name),
            });
        }
        ancestor = bounded_parent(parent);
    }
    None
}

/// Whether `node` is a C-family callable.
///
/// Told apart from the identically-named nodes in other grammars by structure,
/// because this walk has no language key: Python's `function_definition` and
/// JavaScript's `method_definition` both carry a `name` field, while the
/// C-family ones never do and hang their name off a `declarator` chain or an
/// Objective-C selector instead.
fn is_c_family_callable(node: Node) -> bool {
    match node.kind() {
        "function_definition" => {
            node.child_by_field_name("name").is_none()
                && node.child_by_field_name("declarator").is_some()
        }
        "method_definition" => node.child_by_field_name("name").is_none(),
        _ => false,
    }
}

/// Binding name of `node` if it is a callable. Unnamed arrows keep walking so
/// `useEffect(() => { persist() })` still attributes `persist` to the outer
/// named function instead of dropping to file scope.
fn callable_binding_name(node: Node, source: &str) -> Option<String> {
    if matches!(
        node.kind(),
        "function_definition"
            | "async_function_definition"
            | "function_declaration"
            | "method_definition"
            | "function_item"
            | "method_declaration"
            | "generator_function_declaration"
            | "generator_function"
    ) {
        return get_child_text(node, "name", source);
    }
    if !matches!(node.kind(), "arrow_function" | "function_expression") {
        return None;
    }
    // Name a function *expression* by the binding it is assigned to, never by
    // its own internal name.
    //
    // A named function expression's name is only in scope inside itself, so it
    // is deliberately not emitted as a symbol. Returning it here anyway made
    // `dns.lookup = function patchedLookup() { … }` attribute its calls to
    // `file::patchedLookup`, which matches no node — the same unjoinable-edge
    // defect as SC9, and the residual behind SC10. It also disagreed with the
    // symbol emitter for `const f = function inner() { … }`, where the emitted
    // symbol is `f`. Falling through to `None` lets the walk continue to an
    // enclosing symbol that does exist, exactly as unnamed arrows already do.
    let parent = bounded_parent(node)?;
    if parent.kind() == "variable_declarator" {
        return get_child_text(parent, "name", source);
    }
    None
}

/// The object half of a member access, when `node` is the member half.
///
/// `cfg` for `cfg.enabled`, `cmd` for `cmd.baseline`, `this.backend` for
/// `this.backend.hydrate`. `None` for a bare identifier, and `None` for the
/// object half itself — `cfg` in `cfg.enabled` is a use of `cfg`, not of
/// anything owned by it.
///
/// Recording this is what lets the resolver tell a *member* name, which names
/// something another symbol owns, from a *local* name, which names a binding in
/// the current scope. It refuses to resolve the latter globally on purpose, so
/// that `except Exception as e` cannot bind to an unrelated `def e`; without
/// the distinction that refusal swallowed every property read and every
/// callback passed by attribute along with it.
fn member_access_receiver(node: Node, source: &str) -> Option<String> {
    let parent = bounded_parent(node)?;
    // The grammars spell the same shape three ways: `attribute` in Python,
    // `member_expression` in JS/TS, `selector_expression` in Go.
    let object_field = match parent.kind() {
        "attribute" => "object",
        "member_expression" => "object",
        "selector_expression" => "operand",
        _ => return None,
    };
    // Only the member half has a receiver. The object half is a use in its own
    // right and must keep resolving as one.
    let member = parent
        .child_by_field_name("attribute")
        .or_else(|| parent.child_by_field_name("property"))
        .or_else(|| parent.child_by_field_name("field"))?;
    if member.id() != node.id() {
        return None;
    }
    let object = parent.child_by_field_name(object_field)?;
    // X44. The object's *identity*, not its source text: `runner.invoke(app,
    // ["init"]).output` names the call it reads from, and a receiver that is a
    // block copied into a column groups with nothing.
    let text = receiver_identity(object, source, 0);
    (!text.is_empty()).then_some(text)
}

fn extracted_reference(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    name: String,
    kind: ReferenceKind,
    assigned_to: Option<String>,
) -> ExtractedReference {
    ExtractedReference {
        name,
        kind,
        span: node_span(node),
        enclosing_symbol: enclosing_callable_qualified(node, source, file_symbol_name),
        assigned_to,
        // The object half of a member access, so the resolver can tell
        // `cfg.enabled` from a bare local named `enabled`.
        receiver_expr: member_access_receiver(node, source),
    }
}

fn enclosing_type_name(node: Node, source: &str) -> Option<String> {
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        match parent.kind() {
            "class_definition"
            | "class_declaration"
            | "abstract_class_declaration"
            | "trait_item" => {
                return get_child_text(parent, "name", source);
            }
            // An impl's methods belong to the TYPE, including in
            // `impl Trait for Type` (SC11).
            //
            // Qualifying them by the trait gave every implementor the same
            // identity. That is not an exotic case: a single `impl` overriding
            // a *defaulted* trait method already produced `Trait.method` twice
            // — the declaration and the override — so the graph held two nodes
            // no edge could tell apart. A qualified name is the graph's join
            // key; two things sharing one is a broken key, not a naming
            // preference. The type is what makes an implementation unique.
            //
            // The trait relationship is not lost: `rust_method_structural_reason`
            // reads the `trait` field directly to exempt trait-impl methods from
            // dead-code reporting, and does not depend on this name.
            //
            // This diverges from the Python incumbent, which also names these by
            // the trait. The divergence is deliberate and recorded in
            // DIVERGENCES.md; correctness of the join key outranks bug-for-bug
            // parity here.
            "impl_item" => {
                let owner = parent
                    .child_by_field_name("type")
                    .or_else(|| parent.child_by_field_name("trait"))?;
                let text = get_node_text(owner, source);
                return Some(text.split('<').next().unwrap_or(&text).trim().to_string());
            }
            // A nested function is not a class method merely because a class
            // occurs farther up the ancestor chain.
            "function_definition"
            | "async_function_definition"
            | "function_declaration"
            | "function_expression"
            | "arrow_function"
            | "method_definition" => return None,
            _ => ancestor = bounded_parent(parent),
        }
    }
    None
}

/// Attribute paths attached to `node`, innermost first.
///
/// tree-sitter-rust emits `#[...]` as an `attribute_item` *sibling* preceding
/// the item it decorates, never as a child. Reading only the item's own text —
/// which is all the `pub` export check does — is therefore structurally blind
/// to every attribute, including `#[test]` and `#[no_mangle]`.
fn rust_attribute_paths(node: Node, source: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut sibling = node.prev_sibling();
    while let Some(candidate) = sibling {
        match candidate.kind() {
            "line_comment" | "block_comment" => {}
            "attribute_item" => {
                if let Some(attribute) = candidate.named_child(0) {
                    if let Some(path) = attribute.named_child(0) {
                        paths.push(get_node_text(path, source).replace(char::is_whitespace, ""));
                    }
                }
            }
            _ => break,
        }
        sibling = candidate.prev_sibling();
    }
    paths
}

/// Why a Rust `fn` can never be observed as `pub` regardless of its liveness.
///
/// A trait-impl method and a defaulted trait method both reject `pub`, so
/// `is_exported` is a constant `false` for them and carries no information.
/// Treating that constant as evidence of deadness is the single highest-volume
/// Rust false positive.
fn rust_method_structural_reason(node: Node) -> Option<&'static str> {
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        match parent.kind() {
            "impl_item" => {
                return parent
                    .child_by_field_name("trait")
                    .map(|_| "implements a trait method; Rust forbids `pub` on trait-impl items")
            }
            "trait_item" => {
                // A bare signature declares the contract; a defaulted method
                // also supplies a body. Both are exempt for the same structural
                // reason, but the reason string is user-facing and should not
                // call a declaration a default.
                return Some(if node.kind() == "function_signature_item" {
                    "declared trait method; Rust forbids `pub` on trait items"
                } else {
                    "defaulted trait method; Rust forbids `pub` on trait items"
                });
            }
            // A nested `fn` is not a method merely because an impl block is
            // farther up the ancestor chain.
            "function_item" | "closure_expression" => return None,
            _ => ancestor = bounded_parent(parent),
        }
    }
    None
}

/// Root of the syntax tree containing `node`.
fn ast_root(node: Node) -> Node {
    let mut current = node;
    while let Some(parent) = bounded_parent(current) {
        current = parent;
    }
    current
}

/// Whether a JS/TS declaration is externally reachable.
///
/// **Single owner of this test.** Six declaration forms need it — function,
/// method, `const` arrow, class, enum, interface/type alias, and namespace —
/// and five of them had inlined their own copy of
/// `parent().kind() == "export_statement" || …`. Two of those copies were
/// subtly different, and every one of them was independently mutable, so a
/// broken copy would silently mark one *kind* of declaration unexported while
/// the others stayed right. `is_exported` is direct evidence for dead code, so
/// that reads as "this enum is private and unused" rather than as a bug.
///
/// Two forms are not their own owner:
/// - `bounded_parent(method_definition)` is always `class_body`, so a direct parent
///   check can never be true — a method is exported exactly when its class is.
/// - `bounded_parent(variable_declarator)` is the `lexical_declaration`; the export
///   statement wraps that, not the declarator.
fn js_symbol_is_exported(node: Node, source: &str) -> bool {
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        match parent.kind() {
            "export_statement" | "export_declaration" => return true,
            // The value escapes to the host runtime. `globalThis.ResizeObserver
            // = class { observe() {} }` publishes `observe` to anything that
            // constructs a `ResizeObserver` — the browser, a test harness, a
            // library — and none of those call sites is in the corpus. That is
            // the same claim `export` makes, made a different way.
            "assignment_expression" => {
                if parent
                    .child_by_field_name("left")
                    .is_some_and(|left| js_target_is_global(left, source))
                {
                    return true;
                }
                ancestor = bounded_parent(parent);
            }
            // A value bound inside a callable does not escape by being written.
            // Proving that a returned object reaches a caller needs escape
            // analysis, which a syntax-directed extractor does not do, so stop
            // here and report what is actually evident: not exported.
            "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "arrow_function"
            | "method_definition" => return false,
            _ => ancestor = bounded_parent(parent),
        }
    }
    false
}

/// Whether an assignment target names a property of the global object.
///
/// The four spellings are the ones a JavaScript program can actually reach:
/// `globalThis` is the standard, `window` and `self` are the web ones, `global`
/// is Node's. A bare identifier is deliberately excluded — `x = class {…}`
/// assigns to a binding, not to the runtime.
fn js_target_is_global(left: Node, source: &str) -> bool {
    if left.kind() != "member_expression" {
        return false;
    }
    left.child_by_field_name("object")
        .map(|object| get_node_text(object, source))
        .is_some_and(|root| matches!(root.as_str(), "globalThis" | "window" | "global" | "self"))
}

/// Declared parameter count of a `parameter_list`.
///
/// A grouped declaration binds several names to one type (`m(x, y int)` is two
/// parameters, not one), and an interface spec may omit names entirely
/// (`m(int)` is one). Counting `parameter_list` children would get both wrong,
/// so count the `name` fields and fall back to one for an unnamed declaration.
fn go_param_count(parameters: Node) -> usize {
    let mut cursor = parameters.walk();
    let mut total = 0;
    let mut param_cursor = parameters.walk();
    for declaration in parameters.named_children(&mut param_cursor) {
        if !matches!(
            declaration.kind(),
            "parameter_declaration" | "variadic_parameter_declaration"
        ) {
            continue;
        }
        total += declaration
            .children_by_field_name("name", &mut cursor)
            .count()
            .max(1);
    }
    total
}

/// Interface method specs and method-declaration arities declared in this file.
///
/// Both halves of the Go interface-satisfaction join are per-file facts, but the
/// two halves routinely live in different files of the same package, so they are
/// carried on the `Extraction` and joined in `devmap-analyze`. Embedded
/// interfaces (`interface { io.Reader }`) parse as `type_elem`, not
/// `method_elem`, so their methods are not represented here.
fn go_method_sets(
    root: Node,
    source: &str,
    file_symbol_name: &str,
) -> (Vec<GoInterfaceMethod>, Vec<GoMethodParams>) {
    let mut interface_methods = Vec::new();
    let mut method_params = Vec::new();
    let mut worklist = vec![root];
    // Same stride idiom as `walk_tree`. This pass runs *after* the walk has
    // returned, over the whole tree again, and until now had no deadline at
    // all: a tree that the walk finished just inside its budget could spend
    // unbounded time here and still be published. The caller re-reads the latch
    // this sets and refuses the file.
    let mut since_check = 0u32;
    while let Some(node) = worklist.pop() {
        since_check += 1;
        if since_check >= DEADLINE_CHECK_STRIDE {
            since_check = 0;
            if walk_deadline_passed() {
                break;
            }
        }
        match node.kind() {
            "type_spec" => {
                if let (Some(name), Some(declared)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("type"),
                ) {
                    if declared.kind() == "interface_type" {
                        let interface_name = get_node_text(name, source);
                        let mut member_cursor = declared.walk();
                        for member in declared.named_children(&mut member_cursor) {
                            if !matches!(member.kind(), "method_elem" | "method_spec") {
                                continue;
                            }
                            let Some(method) = member.child_by_field_name("name") else {
                                continue;
                            };
                            interface_methods.push(GoInterfaceMethod {
                                interface_name: interface_name.clone(),
                                method: get_node_text(method, source),
                                param_count: member
                                    .child_by_field_name("parameters")
                                    .map(go_param_count)
                                    .unwrap_or(0),
                            });
                        }
                    }
                }
            }
            "method_declaration" => {
                if let (Some(name), Some((_, type_name))) = (
                    get_child_text(node, "name", source),
                    go_receiver(node, source),
                ) {
                    method_params.push(GoMethodParams {
                        qualified_name: format!("{file_symbol_name}::{type_name}.{name}"),
                        param_count: node
                            .child_by_field_name("parameters")
                            .map(go_param_count)
                            .unwrap_or(0),
                    });
                }
            }
            _ => {}
        }
        push_children(node, &mut worklist);
    }
    interface_methods.sort_by(|a, b| {
        (&a.method, a.param_count, &a.interface_name).cmp(&(
            &b.method,
            b.param_count,
            &b.interface_name,
        ))
    });
    method_params.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));
    (interface_methods, method_params)
}

/// Same-file half of the interface exemption, as a symbol-scoped annotation.
///
/// The cross-file half cannot be decided here — extraction never sees another
/// file — so `devmap-analyze` re-runs the same join over the whole Go package.
/// Both use `go_interface_method_matches`, so one match cannot yield two
/// different verdicts.
fn go_interface_method_exemptions(
    symbols: &[ExtractedSymbol],
    interface_methods: &[GoInterfaceMethod],
    method_params: &[GoMethodParams],
    wiring: &mut Vec<WiringAnnotation>,
) {
    let param_counts: std::collections::BTreeMap<&str, usize> = method_params
        .iter()
        .map(|entry| (entry.qualified_name.as_str(), entry.param_count))
        .collect();
    for (symbol, interface_name) in
        go_interface_method_matches(symbols, &param_counts, interface_methods)
    {
        wiring.push(WiringAnnotation {
            kind: WiringKind::StructuralExempt,
            target_symbol: symbol.qualified_name.clone(),
            details: go_interface_exemption_reason(interface_name),
        });
    }
}

/// Whether a declaration sits at module/package level rather than inside a
/// callable or type body.
///
/// Module-level bindings are public API surface; a constant declared inside a
/// function is a local and belongs in nobody's graph.
fn is_module_level(node: Node) -> bool {
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        if is_callable_node(parent) {
            return false;
        }
        if matches!(
            parent.kind(),
            "class_declaration"
                | "class_definition"
                | "class_body"
                | "abstract_class_declaration"
                | "impl_item"
                | "trait_item"
                | "block"
                | "statement_block"
        ) {
            return false;
        }
        ancestor = bounded_parent(parent);
    }
    true
}

/// Emit an exported module-level binding as a `Variable` symbol.
///
/// The frozen Python baseline emits these too (misclassified as `function`);
/// devmap records them with the correct kind. Without them an exported
/// constant is invisible: it cannot be searched, nothing can reference it, and
/// it can be neither confirmed live nor reported dead — it simply is not in
/// the map. Only *exported* bindings are emitted, because a private
/// module-local constant is an implementation detail whose main effect on the
/// graph would be dead-code noise.
#[allow(clippy::too_many_arguments)]
fn push_module_binding(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    name: String,
    is_exported: bool,
    span: Span,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    if !is_exported || name.is_empty() || !is_module_level(node) {
        return;
    }
    let _ = source;
    symbols.push(ExtractedSymbol {
        qualified_name: format!("{file_symbol_name}::{name}"),
        name,
        kind: SymbolKind::Variable,
        span,
        is_exported,
        docstring: None,
        signature: None,
        parent_symbol: Some(file_symbol_name.to_string()),
        body_signature: None,
        declaration_hash: None,
    });
}

/// Terraform address for an HCL block: `resource.aws_s3_bucket.b`.
///
/// A block is its type identifier followed by zero or more quoted labels.
/// Nested blocks (a `lifecycle` inside a `resource`) have no labels and are
/// attributes of their parent rather than addressable units, so they are
/// skipped rather than emitted under a misleading name.
fn hcl_block_address(node: Node, source: &str) -> Option<String> {
    let block_type = node.named_child(0)?;
    if block_type.kind() != "identifier" {
        return None;
    }
    let mut parts = vec![get_node_text(block_type, source)];
    for index in 1..node.named_child_count() {
        let child = node.named_child(index)?;
        if child.kind() != "string_lit" {
            break;
        }
        parts.push(get_node_text(child, source).trim_matches('"').to_string());
    }
    if parts.len() < 2 || parts.iter().any(|part| part.is_empty()) {
        return None;
    }
    Some(parts.join("."))
}

/// Declaration node kinds shared across the broad grammar matrix.
///
/// The five specialised languages have their own arms; this table covers every
/// other linked grammar. Kinds were read from each grammar's own parse tree
/// rather than assumed — C in particular does not put a `name` field on
/// `function_definition`, so the name is recovered through its declarator.
pub(crate) fn generic_symbol_kind(kind: &str) -> Option<SymbolKind> {
    Some(match kind {
        "class_declaration" | "class_definition" | "class_specifier" | "class"
        | "object_definition" | "singleton_class" => SymbolKind::Class,
        // A Solidity contract is the unit that owns functions, like a class.
        "contract_declaration" | "library_declaration" => SymbolKind::Class,
        "interface_declaration" | "protocol_declaration" => SymbolKind::Interface,
        "struct_specifier" | "struct_declaration" | "struct_item" => SymbolKind::Struct,
        "enum_declaration" | "enum_specifier" | "enum_item" => SymbolKind::Enum,
        "trait_item" | "trait_declaration" => SymbolKind::Trait,
        "module" | "namespace_declaration" | "package_declaration" => SymbolKind::Module,
        // SQL. A relation is a named set of typed columns, which is what
        // `Struct` already means here, and a stored function/trigger is a
        // callable body. Only schema objects a code map can act on are mapped:
        // indexes, sequences, roles, databases and extensions are deliberately
        // left out because they name no callable and own no fields, so they
        // would add nodes nothing can traverse to.
        "create_table" | "create_view" | "create_materialized_view" | "create_type" => {
            SymbolKind::Struct
        }
        "create_function" | "create_trigger" => SymbolKind::Function,
        "create_schema" => SymbolKind::Module,
        "function_definition"
        | "function_declaration"
        | "function_item"
        | "method_declaration"
        | "method_definition"
        | "method"
        | "singleton_method"
        | "constructor_declaration"
        | "subroutine_declaration"
        | "function_declarator" => SymbolKind::Function,
        _ => return None,
    })
}

/// `(kind, name)` for a declaration node, or `None` when it is not one.
pub(crate) fn generic_declaration(node: Node, source: &str) -> Option<(SymbolKind, String)> {
    let kind = generic_symbol_kind(node.kind())?;
    // `function_declarator` exists to name a C-family function whose
    // `function_definition` carries no name field; taking both would emit the
    // same function twice.
    if node.kind() == "function_declarator"
        && bounded_parent(node).is_some_and(|parent| parent.kind() == "function_definition")
    {
        return None;
    }
    let name = generic_declaration_name(node, source)?;
    (!name.is_empty()).then_some((kind, name))
}

fn generic_declaration_name(node: Node, source: &str) -> Option<String> {
    if let Some(name) = get_child_text(node, "name", source).filter(|name| !name.is_empty()) {
        return Some(name);
    }
    // SQL: a `create_*` node carries no `name` field. The name lives in an
    // `object_reference` child, which itself has a required `name` field —
    // taking that rather than the reference's whole text keeps a
    // schema-qualified `analytics.events` from becoming part of the identity.
    let mut ref_cursor = node.walk();
    for child in node.named_children(&mut ref_cursor) {
        if child.kind() == "object_reference" {
            if let Some(name) = get_child_text(child, "name", source).filter(|n| !n.is_empty()) {
                return Some(name);
            }
        }
    }
    // C family: the name lives inside the declarator.
    let declarator = node.child_by_field_name("declarator")?;
    if let Some(name) =
        get_child_text(declarator, "declarator", source).filter(|name| !name.is_empty())
    {
        return Some(name);
    }
    let mut declarator_cursor = declarator.walk();
    for child in declarator.named_children(&mut declarator_cursor) {
        if matches!(
            child.kind(),
            "identifier" | "field_identifier" | "type_identifier"
        ) {
            let text = get_node_text(child, source);
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

/// The shader-entry reason for a Metal declaration, read from its own leading
/// qualifier.
///
/// Both spellings occur and both are accepted: the bare qualifier
/// (`kernel void reduce(...)`) and the attribute form (`[[kernel]] void f()`,
/// `[[kernel, max_total_threads_per_threadgroup(256)]] void f()`). Only the
/// declaration's *leading* tokens are read, within a bounded window, so a
/// `kernel`-typed parameter or an address space further along the signature can
/// never promote an ordinary helper to an entry point.
fn metal_shader_entry_reason_of(node: Node, source: &str) -> Option<&'static str> {
    const HEAD_WINDOW: usize = 256;
    let start = node.start_byte();
    let end = node.end_byte().min(start + HEAD_WINDOW);
    let head = source.get(start..end)?.trim_start();
    if let Some(rest) = head.strip_prefix("[[") {
        let list = rest.split("]]").next().unwrap_or(rest);
        return list
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .find_map(crate::wiring::metal_shader_entry_reason);
    }
    let word: String = head
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect();
    crate::wiring::metal_shader_entry_reason(&word)
}

/// Nearest enclosing type-like declaration, so a method is owned by its type.
pub(crate) fn generic_enclosing_type(node: Node, source: &str) -> Option<String> {
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        if let Some(kind) = generic_symbol_kind(parent.kind()) {
            if !matches!(kind, SymbolKind::Function) {
                return get_child_text(parent, "name", source).filter(|name| !name.is_empty());
            }
            // A function inside a function is not owned by a type.
            return None;
        }
        ancestor = bounded_parent(parent);
    }
    None
}

/// The declaration's own header: everything before its body.
///
/// A visibility modifier precedes the name in every language that has one, so
/// this is the region a modifier can legally occupy. Returns the whole node
/// when no body can be identified, which is the previous behaviour and is safe
/// for declarations that have no body to confuse it with.
fn declaration_header<'a>(node: Node, source: &'a str) -> &'a str {
    let body_start = node
        .child_by_field_name("body")
        .map(|body| body.start_byte())
        .or_else(|| {
            let mut cursor = node.walk();
            let found = node
                .children(&mut cursor)
                .find(|child| {
                    let kind = child.kind();
                    kind.ends_with("_body")
                        || kind == "block"
                        || kind == "declaration_list"
                        || kind == "field_declaration_list"
                        || kind == "template_body"
                })
                .map(|body| body.start_byte());
            found
        })
        .unwrap_or_else(|| node.end_byte());
    source
        .get(node.start_byte()..body_start.max(node.start_byte()))
        .unwrap_or("")
}

/// Visibility for grammars without a single export keyword.
///
/// Falls back to the leading-character convention only where a language
/// actually uses one; otherwise a declaration is treated as visible, which is
/// the safe direction — treating a public symbol as private would make it a
/// dead-code candidate on no evidence.
///
/// The scan is bounded to [`declaration_header`] because it previously read
/// `get_node_text(node, source)` — the whole subtree, bodies included — and so
/// answered a question about the declaration from text belonging to its
/// members. One `private val` inside a public Scala class returned `false`, and
/// `devmap dead` then reported that class at the 0.90 tier with no exemption
/// reason. Java escaped it only because `"public "` is tested first and Java
/// spells the modifier; that is an accident of one keyword set, not a rule.
/// The doc contract above — every `false` rests on evidence the language
/// actually provides — is only true once the evidence is the declaration's own.
pub(crate) fn generic_is_exported(node: Node, source: &str, name: &str) -> bool {
    let text = declaration_header(node, source);
    if text.starts_with("pub ") || text.contains("public ") || text.contains("export ") {
        return true;
    }
    if text.contains("private ") || text.contains("protected ") {
        return false;
    }
    !name.starts_with('_')
}

// Keep the node handler pure with respect to traversal state; the explicit
// accumulators make ownership and mutation sites visible to the caller.
#[allow(clippy::too_many_arguments)]
fn extract_node(
    node: Node,
    source: &str,
    lang: &str,
    file_symbol_name: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
    calls: &mut Vec<ExtractedCall>,
    exports: &mut Vec<ExtractedExport>,
    references: &mut Vec<ExtractedReference>,
    wiring: &mut Vec<WiringAnnotation>,
) {
    let kind = node.kind();
    let span = node_span(node);

    match lang {
        "python" => match kind {
            // A module-level binding is a candidate public constant. Python has
            // no export keyword, so the only principled test is `__all__`
            // membership — applied after the walk, once the whole file has been
            // seen. Candidates not declared there are dropped, so a module
            // without `__all__` contributes none.
            "assignment" if is_module_level(node) => {
                if let Some(target) = node.child_by_field_name("left") {
                    if target.kind() == "identifier" {
                        let name = get_node_text(target, source);
                        if name != "__all__" && !name.is_empty() {
                            symbols.push(ExtractedSymbol {
                                qualified_name: format!("{file_symbol_name}::{name}"),
                                name,
                                kind: SymbolKind::Variable,
                                span,
                                is_exported: false,
                                docstring: None,
                                signature: None,
                                parent_symbol: Some(file_symbol_name.to_string()),
                                body_signature: None,
                                declaration_hash: None,
                            });
                        }
                    }
                }
            }
            "function_definition" | "async_function_definition" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    let enclosing_type = enclosing_type_name(node, source);
                    let is_method = enclosing_type.is_some();
                    let parent_symbol = enclosing_type
                        .as_ref()
                        .map(|type_name| format!("{}::{}", file_symbol_name, type_name))
                        .or_else(|| enclosing_callable_qualified(node, source, file_symbol_name))
                        .unwrap_or_else(|| file_symbol_name.to_string());
                    let qualified_name = enclosing_type
                        .map(|type_name| format!("{}::{}.{}", file_symbol_name, type_name, n))
                        .unwrap_or_else(|| {
                            scoped_qualified_name(node, source, file_symbol_name, &n)
                        });
                    if let Some(reason) = crate::wiring::python_harness_entry_reason(&n) {
                        wiring.push(WiringAnnotation {
                            kind: WiringKind::RuntimeEntryPoint,
                            target_symbol: qualified_name.clone(),
                            details: reason.to_string(),
                        });
                    }
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name,
                        kind: if is_method {
                            SymbolKind::Method
                        } else {
                            SymbolKind::Function
                        },
                        span,
                        is_exported: false,
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(parent_symbol),
                        body_signature: None,
                        declaration_hash: None,
                    });
                }
            }
            "class_definition" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name: scoped_qualified_name(node, source, file_symbol_name, &n),
                        kind: SymbolKind::Class,
                        span,
                        is_exported: false,
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(file_symbol_name.to_string()),
                        body_signature: None,
                        declaration_hash: None,
                    });
                }
            }
            "import_statement" | "import_from_statement" => {
                let text = get_node_text(node, source);
                let mut module_specifier = "".to_string();
                let mut imported_names = vec![];
                let mut local_names = vec![];
                let mut alias = None;

                if text.starts_with("from ") {
                    if let Some(import_idx) = text.find(" import ") {
                        module_specifier = text[5..import_idx].trim().to_string();
                        let names_str = &text[import_idx + 8..];
                        let (names, locals) = crate::model::parse_import_bindings(names_str);
                        imported_names = names;
                        local_names = locals;
                    }
                } else if let Some(stripped) = text.strip_prefix("import ") {
                    module_specifier = stripped.trim().to_string();
                    if let Some(as_idx) = module_specifier.find(" as ") {
                        alias = Some(module_specifier[as_idx + 4..].trim().to_string());
                        module_specifier = module_specifier[..as_idx].trim().to_string();
                    }
                }

                imports.push(ExtractedImport {
                    raw_import: text,
                    module_specifier,
                    imported_names,
                    local_names,
                    alias,
                    span,
                });
            }
            "call" => {
                // A curried call — `app.command(name="apply-patch")(fn)`,
                // `_default_runner(5)(argv, cwd)` — has a `call` in its
                // `function` field and so no callee name of its own.
                // `split_call_target` refuses it rather than recording the
                // inner call's whole source text as a callee name. The inner
                // call is its own `call` node, so `command` and
                // `_default_runner` keep the edges they always had.
                if let Some(f) = node.child_by_field_name("function") {
                    if let Some((callee_name, receiver_expr)) = split_call_target(f, source) {
                        references.push(extracted_reference(
                            f,
                            source,
                            file_symbol_name,
                            callee_name.clone(),
                            ReferenceKind::Call,
                            assignment_binding(node, source),
                        ));
                        calls.push(ExtractedCall {
                            caller_symbol: enclosing_callable_qualified(
                                node,
                                source,
                                file_symbol_name,
                            ),
                            callee_name,
                            receiver_expr,
                            span,
                        });
                    }
                }
            }
            _ => {}
        },
        "javascript" | "typescript" | "tsx" => match kind {
            // `generator_function_declaration` covers `function*` and
            // `async function*`. It is as much a declaration as
            // `function_declaration` — and frequently an exported API — but was
            // omitted here while `callable_binding_name` still named it as a
            // call scope, so its calls pointed at a source symbol no node had
            // (SC10).
            "function_declaration"
            | "generator_function_declaration"
            | "method_definition"
            | "arrow_function" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    let is_exported = js_symbol_is_exported(node, source);
                    let enclosing_type = (kind == "method_definition")
                        .then(|| enclosing_type_name(node, source))
                        .flatten();
                    let parent_symbol = enclosing_type
                        .as_ref()
                        .map(|type_name| format!("{}::{}", file_symbol_name, type_name))
                        .or_else(|| enclosing_callable_qualified(node, source, file_symbol_name))
                        .unwrap_or_else(|| file_symbol_name.to_string());
                    let qualified_name = enclosing_type
                        .map(|type_name| format!("{}::{}.{}", file_symbol_name, type_name, n))
                        .unwrap_or_else(|| {
                            scoped_qualified_name(node, source, file_symbol_name, &n)
                        });
                    if kind == "method_definition" {
                        if let Some(reason) = crate::wiring::js_lifecycle_hook_reason(&n) {
                            wiring.push(WiringAnnotation {
                                kind: WiringKind::RuntimeEntryPoint,
                                target_symbol: qualified_name.clone(),
                                details: reason.to_string(),
                            });
                        }
                    }
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name,
                        kind: if kind == "method_definition" {
                            SymbolKind::Method
                        } else {
                            SymbolKind::Function
                        },
                        span,
                        is_exported,
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(parent_symbol),
                        body_signature: None,
                        declaration_hash: None,
                    });
                }
            }
            "variable_declarator" => {
                if let Some(val) = node.child_by_field_name("value") {
                    let vk = val.kind();
                    if !matches!(vk, "arrow_function" | "function_expression") {
                        if let Some(n) = get_child_text(node, "name", source) {
                            push_module_binding(
                                node,
                                source,
                                file_symbol_name,
                                n,
                                js_symbol_is_exported(node, source),
                                span.clone(),
                                symbols,
                            );
                        }
                    }
                    if vk == "arrow_function" || vk == "function_expression" {
                        if let Some(n) = get_child_text(node, "name", source) {
                            let is_exported = js_symbol_is_exported(node, source);
                            symbols.push(ExtractedSymbol {
                                name: n.clone(),
                                qualified_name: scoped_qualified_name(
                                    node,
                                    source,
                                    file_symbol_name,
                                    &n,
                                ),
                                kind: SymbolKind::Function,
                                span,
                                is_exported,
                                docstring: None,
                                signature: None,
                                parent_symbol: Some(file_symbol_name.to_string()),
                                body_signature: None,
                                declaration_hash: None,
                            });
                        }
                    }
                }
            }
            "class_declaration" | "abstract_class_declaration" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    let is_exported = js_symbol_is_exported(node, source);
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name: scoped_qualified_name(node, source, file_symbol_name, &n),
                        kind: SymbolKind::Class,
                        span,
                        is_exported,
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(file_symbol_name.to_string()),
                        body_signature: None,
                        declaration_hash: None,
                    });
                }
            }
            "enum_declaration" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    let is_exported = js_symbol_is_exported(node, source);
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name: scoped_qualified_name(node, source, file_symbol_name, &n),
                        kind: SymbolKind::Enum,
                        span,
                        is_exported,
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(file_symbol_name.to_string()),
                        body_signature: None,
                        declaration_hash: None,
                    });
                }
            }
            "interface_declaration" | "type_alias_declaration" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    let is_exported = js_symbol_is_exported(node, source);
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name: scoped_qualified_name(node, source, file_symbol_name, &n),
                        kind: SymbolKind::Interface,
                        span,
                        is_exported,
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(file_symbol_name.to_string()),
                        body_signature: None,
                        declaration_hash: None,
                    });
                }
            }
            "internal_module" | "module" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    let is_exported = js_symbol_is_exported(node, source);
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name: scoped_qualified_name(node, source, file_symbol_name, &n),
                        kind: SymbolKind::Module,
                        span,
                        is_exported,
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(file_symbol_name.to_string()),
                        body_signature: None,
                        declaration_hash: None,
                    });
                }
            }
            "import_statement" | "export_statement" => {
                let text = get_node_text(node, source);
                let mut alias = None;
                let mut imported_names = vec![];
                let mut local_names = vec![];
                let mod_spec = find_string_child(node, source).unwrap_or_default();

                if text.contains("import * as ") {
                    if let Some(idx) = text.find("import * as ") {
                        let rest = &text[idx + 12..];
                        if let Some(from_idx) = rest.find(" from") {
                            let ns_alias = rest[..from_idx].trim().to_string();
                            alias = Some(ns_alias.clone());
                            imported_names.push("*".to_string());
                            local_names.push(ns_alias);
                        }
                    }
                } else if text.contains("export *") {
                    imported_names.push("*".to_string());
                    local_names.push("*".to_string());
                    exports.push(ExtractedExport {
                        exported_name: "*".to_string(),
                        local_name: None,
                        module_specifier: (!mod_spec.is_empty()).then(|| mod_spec.clone()),
                        span: span.clone(),
                    });
                } else if let Some(clause) = js_binding_clause(node) {
                    // `parse_import_bindings` strips the braces itself.
                    let inner = get_node_text(clause, source);
                    let (names, locals) = crate::model::parse_import_bindings(&inner);
                    imported_names = names;
                    local_names = locals;
                    if text.trim_start().starts_with("export") {
                        for (local, exported) in imported_names.iter().zip(local_names.iter()) {
                            exports.push(ExtractedExport {
                                exported_name: exported.clone(),
                                local_name: Some(local.clone()),
                                module_specifier: (!mod_spec.is_empty()).then(|| mod_spec.clone()),
                                span: span.clone(),
                            });
                        }
                    }
                } else if text.trim_start().starts_with("import ") {
                    let rest = text.trim_start().trim_start_matches("import ");
                    if let Some(from_index) = rest.find(" from ") {
                        let candidate = rest[..from_index].trim();
                        if !candidate.is_empty()
                            && candidate.chars().all(|character| {
                                character.is_ascii_alphanumeric() || matches!(character, '_' | '$')
                            })
                        {
                            alias = Some(candidate.to_string());
                        }
                    }
                }

                // A module edge needs a module. Gating on `imported_names`
                // instead let `export { a as b };` — a purely local re-export
                // with no `from` — push an import whose `module_specifier` was
                // `""`, an endpoint no resolver can ever bind, and it did the
                // same for every function body the old text scan mis-sliced.
                if !mod_spec.is_empty() {
                    imports.push(ExtractedImport {
                        raw_import: text,
                        module_specifier: mod_spec,
                        imported_names,
                        local_names,
                        alias,
                        span,
                    });
                }
            }
            "jsx_opening_element" | "jsx_self_closing_element" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let tag_name = get_node_text(name_node, source);
                    references.push(extracted_reference(
                        name_node,
                        source,
                        file_symbol_name,
                        tag_name.clone(),
                        ReferenceKind::JsxTag,
                        None,
                    ));
                    // Only a *component* tag is a reference to a symbol. A
                    // lowercase tag is an intrinsic host element: JSX compiles
                    // `<div/>` to the string "div", never to a binding, so a
                    // call edge for it can never resolve and is pure noise.
                    // The JsxTag reference above still records every tag.
                    if jsx_tag_is_component(&tag_name) {
                        // `<motion.div/>` is a member access, not a component
                        // named `motion.div`. Split it like any other member
                        // call so the namespace lands in the receiver, where
                        // import evidence can recognise it — otherwise the tag
                        // names a symbol nothing declares and joins to nothing.
                        let (callee_name, receiver_expr) = match tag_name.rsplit_once('.') {
                            Some((object, property)) if !property.is_empty() => {
                                (property.to_string(), Some(object.to_string()))
                            }
                            _ => (tag_name, None),
                        };
                        calls.push(ExtractedCall {
                            caller_symbol: enclosing_callable_qualified(
                                node,
                                source,
                                file_symbol_name,
                            ),
                            callee_name,
                            receiver_expr,
                            span,
                        });
                    }
                }
            }
            "call_expression" => {
                if let Some(f) = node.child_by_field_name("function") {
                    let callee = get_node_text(f, source);
                    if callee == "require" {
                        if let Some(args) = node.child_by_field_name("arguments") {
                            if let Some(arg) = args.named_child(0) {
                                let mod_spec = get_node_text(arg, source)
                                    .trim_matches('"')
                                    .trim_matches('\'')
                                    .to_string();
                                imports.push(ExtractedImport {
                                    raw_import: get_node_text(node, source),
                                    module_specifier: mod_spec,
                                    imported_names: vec![],
                                    local_names: vec![],
                                    alias: None,
                                    span: span.clone(),
                                });
                            }
                        }
                    } else {
                        // Bundled JavaScript is full of immediately-invoked
                        // literals — `(function(t){if(Array.isArray(t))return t})(x)`
                        // — whose `function` field is the literal itself. SC26
                        // stopped the Go `defer func(){…}()` form at the Go arm
                        // alone; here the whole minified function body was still
                        // being recorded as a callee name.
                        if let Some((callee_name, receiver_expr)) = split_call_target(f, source) {
                            references.push(extracted_reference(
                                f,
                                source,
                                file_symbol_name,
                                callee_name.clone(),
                                ReferenceKind::Call,
                                assignment_binding(node, source),
                            ));
                            calls.push(ExtractedCall {
                                caller_symbol: enclosing_callable_qualified(
                                    node,
                                    source,
                                    file_symbol_name,
                                ),
                                callee_name,
                                receiver_expr,
                                span,
                            });
                        }
                    }
                }
            }
            "new_expression" => {
                // A constructor is split exactly like a call target, and for
                // the same reason. This arm read the constructor's raw source
                // text, so `new pkg.Widget()` named a symbol `pkg.Widget` that
                // nothing declares while discarding the receiver import
                // evidence recognises, and `new (cond ? A : B)()` recorded a
                // ternary as a constructed type. The same defect as the JSX
                // member tag SC26 fixed, in the one JS shape that never went
                // through the splitter.
                if let Some(f) = node.child_by_field_name("constructor") {
                    if let Some((callee_name, receiver_expr)) = split_call_target(f, source) {
                        references.push(extracted_reference(
                            f,
                            source,
                            file_symbol_name,
                            callee_name.clone(),
                            ReferenceKind::Constructor,
                            assignment_binding(node, source),
                        ));
                        calls.push(ExtractedCall {
                            caller_symbol: enclosing_callable_qualified(
                                node,
                                source,
                                file_symbol_name,
                            ),
                            callee_name,
                            receiver_expr,
                            span,
                        });
                    }
                }
            }
            _ => {}
        },
        "rust" => match kind {
            "function_item" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    let text = get_node_text(node, source);
                    // Single owner for "function declared inside a type body":
                    // a `fn` in a trait or impl block is that type's method, not
                    // a second top-level function with the same bare name.
                    let owner = enclosing_type_name(node, source);
                    let qualified_name = match &owner {
                        Some(type_name) => format!("{}::{}.{}", file_symbol_name, type_name, n),
                        None => scoped_qualified_name(node, source, file_symbol_name, &n),
                    };
                    // Attribute first: `#[test] fn helper` inside a trait impl
                    // is reached by the harness, which is the sharper reason.
                    // Bind each parameter name to its declared type, scoped
                    // to this function so two functions using the same
                    // parameter name cannot collide (the SC9 lesson).
                    for (param, type_name, qualifier) in param_type_bindings(node, source, lang) {
                        let scope = node.child_by_field_name("body").and_then(|body| {
                            enclosing_callable_qualified(body, source, file_symbol_name)
                        });
                        // The qualifying module, when the type was written with
                        // one. Carried beside the type rather than inside it so
                        // dispatch keeps the bare name (SC25).
                        if let Some(qualifier) = qualifier {
                            references.push(ExtractedReference {
                                name: qualifier,
                                kind: ReferenceKind::TypeQualifier,
                                span: node_span(node),
                                enclosing_symbol: scope.clone(),
                                assigned_to: Some(param.clone()),
                                // The mirrored call already carries the receiver; repeating it here would be a second copy of one fact.
                                receiver_expr: None,
                            });
                        }
                        references.push(ExtractedReference {
                            name: type_name,
                            kind: ReferenceKind::Type,
                            span: node_span(node),
                            enclosing_symbol: scope,
                            assigned_to: Some(param),
                            // The mirrored call already carries the receiver; repeating it here would be a second copy of one fact.
                            receiver_expr: None,
                        });
                    }
                    let entry_reason = rust_attribute_paths(node, source)
                        .iter()
                        .find_map(|path| crate::wiring::rust_attribute_entry_reason(path))
                        .map(|reason| (WiringKind::RuntimeEntryPoint, reason.to_string()))
                        .or_else(|| {
                            rust_method_structural_reason(node)
                                .map(|reason| (WiringKind::StructuralExempt, reason.to_string()))
                        })
                        .or_else(|| {
                            (owner.is_none()
                                && n == "main"
                                && crate::wiring::rust_path_declares_main(file_symbol_name))
                            .then(|| {
                                (
                                    WiringKind::RuntimeEntryPoint,
                                    "binary entry point invoked by the toolchain".to_string(),
                                )
                            })
                        });
                    if let Some((wiring_kind, details)) = entry_reason {
                        wiring.push(WiringAnnotation {
                            kind: wiring_kind,
                            target_symbol: qualified_name.clone(),
                            details,
                        });
                    }
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name,
                        kind: if owner.is_some() {
                            SymbolKind::Method
                        } else {
                            SymbolKind::Function
                        },
                        span,
                        is_exported: text.starts_with("pub"),
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(match &owner {
                            Some(type_name) => format!("{}::{}", file_symbol_name, type_name),
                            None => enclosing_callable_qualified(node, source, file_symbol_name)
                                .unwrap_or_else(|| file_symbol_name.to_string()),
                        }),
                        body_signature: None,
                        declaration_hash: None,
                    });
                }
            }
            // Calls hidden inside a macro body (SC13). The token tree is
            // re-parsed; nothing is emitted when it is not a valid argument
            // list, so a declarative macro body contributes no fabricated call.
            "macro_invocation" => {
                let caller = enclosing_callable_qualified(node, source, file_symbol_name);
                for (callee_name, receiver_expr) in rust_macro_calls(node, source, file_symbol_name)
                {
                    references.push(extracted_reference(
                        node,
                        source,
                        file_symbol_name,
                        callee_name.clone(),
                        ReferenceKind::Call,
                        None,
                    ));
                    calls.push(ExtractedCall {
                        caller_symbol: caller.clone(),
                        callee_name,
                        receiver_expr,
                        span: span.clone(),
                    });
                }
            }
            // A bare trait method signature (`fn greet(&self) -> String;`) is a
            // `function_signature_item`, not a `function_item`, and was never
            // extracted (SC6b). A defaulted method — which has a body — was, so
            // a trait's declared surface was represented only where it happened
            // to carry an implementation. The signature IS the trait's contract;
            // omitting it left the declaration invisible to the graph while the
            // implementations pointed at nothing.
            //
            // This depends on SC11: while impl methods were qualified by the
            // trait, adding the signature would have produced a third source of
            // the same name. With impls qualified by their type, `Trait.method`
            // now belongs to the declaration alone.
            "function_signature_item" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    let owner = enclosing_type_name(node, source);
                    let qualified_name = match &owner {
                        Some(type_name) => format!("{}::{}.{}", file_symbol_name, type_name, n),
                        None => scoped_qualified_name(node, source, file_symbol_name, &n),
                    };
                    // Rust forbids `pub` on trait items, so exportedness comes
                    // from the trait, not the signature's own text. Treating the
                    // absent `pub` as evidence of deadness is the false positive
                    // `rust_method_structural_reason` exists to prevent, and it
                    // already covers `trait_item` ancestors.
                    if let Some(reason) = rust_method_structural_reason(node) {
                        wiring.push(WiringAnnotation {
                            kind: WiringKind::StructuralExempt,
                            target_symbol: qualified_name.clone(),
                            details: reason.to_string(),
                        });
                    }
                    symbols.push(ExtractedSymbol {
                        name: n,
                        qualified_name,
                        kind: SymbolKind::Method,
                        span,
                        is_exported: false,
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(match &owner {
                            Some(type_name) => format!("{}::{}", file_symbol_name, type_name),
                            None => enclosing_callable_qualified(node, source, file_symbol_name)
                                .unwrap_or_else(|| file_symbol_name.to_string()),
                        }),
                        body_signature: None,
                        declaration_hash: None,
                    });
                }
            }
            "struct_item" | "enum_item" | "trait_item" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    let text = get_node_text(node, source);
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name: scoped_qualified_name(node, source, file_symbol_name, &n),
                        kind: match kind {
                            "struct_item" => SymbolKind::Struct,
                            "enum_item" => SymbolKind::Enum,
                            _ => SymbolKind::Trait,
                        },
                        span,
                        is_exported: text.starts_with("pub"),
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(file_symbol_name.to_string()),
                        body_signature: None,
                        declaration_hash: None,
                    });
                }
            }
            "const_item" | "static_item" => {
                if let Some(name) = get_child_text(node, "name", source) {
                    let exported = node.children(&mut node.walk()).any(|child| {
                        child.kind() == "visibility_modifier"
                            && get_node_text(child, source).starts_with("pub")
                    });
                    push_module_binding(
                        node,
                        source,
                        file_symbol_name,
                        name,
                        exported,
                        span.clone(),
                        symbols,
                    );
                }
            }
            // Methods inside an `impl` block are emitted by the `function_item`
            // arm, which resolves its owning type via `enclosing_type_name`.
            // Emitting them here as well produced the same method twice.
            "impl_item" => {}
            "use_declaration" => {
                rust_use_imports(node, source, span, imports);
            }
            "call_expression" => {
                if let Some(f) = node.child_by_field_name("function") {
                    // Split `receiver.method()` the way every other language
                    // already does. Emitting the whole dotted chain as one
                    // callee with no receiver meant Rust method calls could
                    // never reach receiver-type resolution at all: the call was
                    // recorded, but with a name no symbol has and no receiver
                    // to type, so it resolved to nothing (SC12).
                    //
                    // The text fallback that used to sit here is gone: an
                    // immediately-invoked closure (`(|| { … })()`) has no callee
                    // to name, and falling back turned its whole body into one —
                    // the shape SC26 removed from the Go arm and left live here.
                    if let Some((callee, receiver_expr)) = split_call_target(f, source) {
                        references.push(extracted_reference(
                            f,
                            source,
                            file_symbol_name,
                            callee.clone(),
                            ReferenceKind::Call,
                            // `let w = Worker::new()` binds `w` to `Worker` exactly
                            // as the Python, TS and Go arms record it. This arm
                            // passed `None`, so a Rust value that was constructed
                            // locally had a receiver with no type behind it and
                            // `w.go()` resolved to nothing — the SC12 failure mode
                            // reached by assignment rather than by parameter.
                            assignment_binding(node, source),
                        ));
                        calls.push(ExtractedCall {
                            caller_symbol: enclosing_callable_qualified(
                                node,
                                source,
                                file_symbol_name,
                            ),
                            callee_name: callee,
                            receiver_expr,
                            span,
                        });
                    }
                }
            }
            _ => {}
        },
        "go" => match kind {
            "function_declaration" | "method_declaration" => {
                if let Some(n) = get_child_text(node, "name", source) {
                    let receiver = (kind == "method_declaration")
                        .then(|| go_receiver(node, source))
                        .flatten();
                    let parent_symbol = receiver
                        .as_ref()
                        .map(|(_, type_name)| format!("{file_symbol_name}::{type_name}"))
                        .unwrap_or_else(|| file_symbol_name.to_string());
                    let qualified_name = receiver
                        .as_ref()
                        .map(|(_, type_name)| format!("{file_symbol_name}::{type_name}.{n}"))
                        .unwrap_or_else(|| format!("{file_symbol_name}::{n}"));
                    if let Some(reason) = crate::wiring::go_runtime_entry_reason(
                        &n,
                        go_package_name(ast_root(node), source).as_deref(),
                        receiver.is_some(),
                    ) {
                        wiring.push(WiringAnnotation {
                            kind: WiringKind::RuntimeEntryPoint,
                            target_symbol: qualified_name.clone(),
                            details: reason.to_string(),
                        });
                    }
                    // Bind each parameter name to its declared type, scoped
                    // to this function so two functions using the same
                    // parameter name cannot collide (the SC9 lesson).
                    for (param, type_name, qualifier) in param_type_bindings(node, source, lang) {
                        let scope = node.child_by_field_name("body").and_then(|body| {
                            enclosing_callable_qualified(body, source, file_symbol_name)
                        });
                        // The qualifying module, when the type was written with
                        // one. Carried beside the type rather than inside it so
                        // dispatch keeps the bare name (SC25).
                        if let Some(qualifier) = qualifier {
                            references.push(ExtractedReference {
                                name: qualifier,
                                kind: ReferenceKind::TypeQualifier,
                                span: node_span(node),
                                enclosing_symbol: scope.clone(),
                                assigned_to: Some(param.clone()),
                                // The mirrored call already carries the receiver; repeating it here would be a second copy of one fact.
                                receiver_expr: None,
                            });
                        }
                        references.push(ExtractedReference {
                            name: type_name,
                            kind: ReferenceKind::Type,
                            span: node_span(node),
                            enclosing_symbol: scope,
                            assigned_to: Some(param),
                            // The mirrored call already carries the receiver; repeating it here would be a second copy of one fact.
                            receiver_expr: None,
                        });
                    }
                    if let Some((recv_name, type_name)) = &receiver {
                        if !recv_name.is_empty() {
                            // The receiver binding is scoped to THIS method, not
                            // to the file (SC9).
                            //
                            // `extracted_reference` derives `enclosing_symbol` by
                            // walking *up* from the node it is handed. Passing the
                            // `method_declaration` started that walk at its parent,
                            // skipping straight past the method to file scope and
                            // yielding `None`. The binding then looked file-wide, so
                            // two types using the same receiver name in one file —
                            // `func (s *A)` and `func (s *B)`, ordinary Go — collided
                            // on one key and every `s.method()` in the file resolved
                            // against whichever type was indexed last, at full
                            // confidence.
                            //
                            // Deriving the scope from the body makes the walk stop at
                            // this method and produces exactly the string a call made
                            // inside it reports as `caller_symbol`, which is what lets
                            // the resolver join the two. The span still covers the
                            // declaration, so parse-error overlap is unchanged.
                            references.push(ExtractedReference {
                                name: type_name.clone(),
                                kind: ReferenceKind::Type,
                                span: node_span(node),
                                enclosing_symbol: node.child_by_field_name("body").and_then(
                                    |body| {
                                        enclosing_callable_qualified(body, source, file_symbol_name)
                                    },
                                ),
                                assigned_to: Some(recv_name.clone()),
                                // The mirrored call already carries the receiver; repeating it here would be a second copy of one fact.
                                receiver_expr: None,
                            });
                        }
                    }
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name,
                        kind: if kind == "method_declaration" {
                            SymbolKind::Method
                        } else {
                            SymbolKind::Function
                        },
                        span,
                        is_exported: n.chars().next().is_some_and(|c| c.is_uppercase()),
                        docstring: None,
                        signature: receiver.as_ref().map(|(recv, ty)| {
                            if recv.is_empty() {
                                format!("({ty})")
                            } else {
                                format!("({recv} *{ty})")
                            }
                        }),
                        parent_symbol: Some(parent_symbol),
                        body_signature: None,
                        declaration_hash: None,
                    });
                }
            }
            "type_declaration" | "type_spec" => {
                let node_to_check = if kind == "type_declaration" {
                    node.child(0).unwrap_or(node)
                } else {
                    node
                };
                if let Some(n) = get_child_text(node_to_check, "name", source) {
                    symbols.push(ExtractedSymbol {
                        name: n.clone(),
                        qualified_name: scoped_qualified_name(node, source, file_symbol_name, &n),
                        kind: SymbolKind::Struct,
                        span,
                        is_exported: n.chars().next().is_some_and(|c| c.is_uppercase()),
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(file_symbol_name.to_string()),
                        body_signature: None,
                        declaration_hash: None,
                    });
                }
            }
            "const_spec" | "var_spec" => {
                if let Some(name) = get_child_text(node, "name", source) {
                    let exported = name.chars().next().is_some_and(char::is_uppercase);
                    push_module_binding(
                        node,
                        source,
                        file_symbol_name,
                        name,
                        exported,
                        span.clone(),
                        symbols,
                    );
                }
            }
            "import_spec" => {
                if let Some(p) = node.child_by_field_name("path") {
                    let mod_spec = get_node_text(p, source).trim_matches('"').to_string();
                    let alias = node
                        .child_by_field_name("name")
                        .map(|name| get_node_text(name, source))
                        .filter(|name| !name.is_empty());
                    imports.push(ExtractedImport {
                        raw_import: get_node_text(node, source),
                        module_specifier: mod_spec,
                        imported_names: vec![],
                        local_names: vec![],
                        alias,
                        span,
                    });
                }
            }
            "call_expression" | "composite_literal" => {
                let callee_node = if kind == "call_expression" {
                    node.child_by_field_name("function")
                } else {
                    // `composite_literal` declares `type` as a required field
                    // (tree-sitter-go node-types.json), so this cannot be None
                    // for a well-formed node. A child-scanning fallback used to
                    // sit here and was measured unreachable: zero invocations
                    // across 2,386 real Go files and across every literal shape
                    // (plain, addressed, sliced, mapped, nested, anonymous
                    // struct, implicit-length array). If a future grammar makes
                    // the field optional this yields None and the literal is
                    // skipped, which is the safe direction — no fabricated call.
                    //
                    // The type is unwrapped to the named type it constructs, so
                    // `[]*Foo{...}` references `Foo` rather than recording the
                    // literal text `[]*Foo` as a callee no symbol can match.
                    node.child_by_field_name("type")
                        .and_then(|t| go_composite_literal_type(t, source, 0))
                };
                // An immediately-invoked function literal — `defer func(){…}()`
                // — has no callee to name, and neither does any other
                // expression that is not a name. Both the literal filter that
                // stood here and the text fallback that followed it now live in
                // `split_call_target`, the one place every grammar's callee name
                // is built and therefore the only place the rule cannot be
                // forgotten by a new arm.
                if let Some(f) = callee_node {
                    if let Some((callee_name, receiver_expr)) = split_call_target(f, source) {
                        references.push(extracted_reference(
                            f,
                            source,
                            file_symbol_name,
                            callee_name.clone(),
                            if kind == "composite_literal" {
                                ReferenceKind::Constructor
                            } else {
                                ReferenceKind::Call
                            },
                            assignment_binding(node, source),
                        ));
                        calls.push(ExtractedCall {
                            caller_symbol: enclosing_callable_qualified(
                                node,
                                source,
                                file_symbol_name,
                            ),
                            callee_name,
                            receiver_expr,
                            span,
                        });
                    }
                }
            }
            _ => {}
        },
        // HCL/Terraform. Its declarations are `block` nodes rather than
        // functions or types, so the generic declaration table below cannot see
        // them: a Terraform file would index with a File node and nothing else.
        // Blocks are named by their Terraform address (`resource.aws_s3_bucket.b`,
        // `variable.region`), which is the identity operators already use and
        // the one `terraform plan` prints.
        "hcl" => {
            if kind == "block" {
                if let Some(name) = hcl_block_address(node, source) {
                    let block_type = node
                        .named_child(0)
                        .map(|child| get_node_text(child, source))
                        .unwrap_or_default();
                    symbols.push(ExtractedSymbol {
                        name: name.clone(),
                        qualified_name: format!("{file_symbol_name}::{name}"),
                        kind: match block_type.as_str() {
                            "variable" | "output" | "locals" => SymbolKind::Variable,
                            _ => SymbolKind::Module,
                        },
                        span,
                        // Terraform has no private declarations; every block is
                        // addressable from outside the file.
                        is_exported: true,
                        docstring: None,
                        signature: None,
                        parent_symbol: Some(file_symbol_name.to_string()),
                        body_signature: None,
                        declaration_hash: None,
                    });
                }
            }
        }

        // Every other linked grammar. The specialised arms above encode
        // language-specific call, import and receiver semantics; this one
        // recovers the declaration skeleton — types and functions — which is
        // what a code map needs first and what the frozen Python baseline
        // produces for these languages through its own generic extractor.
        _ => {
            let c_family = is_c_family_grammar(lang);
            if c_family {
                extract_c_family_call(node, source, file_symbol_name, calls, references);
                extract_c_header_export(node, source, file_symbol_name, exports);
            } else {
                // Every other language reaching this arm gets declarations only
                // unless `langcalls` has a module for it (SC34). Five of them —
                // Ruby, Swift, PHP, Scala, Lua — were measured recovering calls
                // under the Python implementation this port replaces, so their
                // absence here was a migration regression, not a shared gap.
                crate::langcalls::extract_calls(
                    lang,
                    node,
                    source,
                    file_symbol_name,
                    calls,
                    references,
                );
            }
            // Declaration identity, kind and visibility all come from the
            // language's own module in `langdecl`, which is the single owner of
            // the answer `langcalls::scope` mirrors for caller attribution.
            if let Some(declaration) = crate::langdecl::declaration_of(lang, node, source) {
                let qualified = declaration.qualified(file_symbol_name);
                let name = declaration.name.clone();
                if declaration.declared_kind == SymbolKind::Function
                    && is_metal_path(file_symbol_name)
                {
                    if let Some(reason) = metal_shader_entry_reason_of(node, source) {
                        wiring.push(WiringAnnotation {
                            kind: WiringKind::RuntimeEntryPoint,
                            target_symbol: qualified.clone(),
                            details: reason.to_string(),
                        });
                    }
                }
                if c_family && declaration.declared_kind == SymbolKind::Function {
                    if let Some(reason) =
                        c_family_entry_point_reason(node, source, file_symbol_name, &name)
                    {
                        wiring.push(WiringAnnotation {
                            kind: WiringKind::RuntimeEntryPoint,
                            target_symbol: qualified.clone(),
                            details: reason.to_string(),
                        });
                    }
                }
                // Reading visibility is what makes an exemption necessary: until
                // a symbol can report `is_exported = false` nothing could ever
                // be dead, so nothing needed exempting.
                if let Some((wiring_kind, reason)) =
                    crate::langdecl::exemption(lang, node, source, &declaration)
                {
                    wiring.push(WiringAnnotation {
                        kind: wiring_kind,
                        target_symbol: qualified.clone(),
                        details: reason.to_string(),
                    });
                }
                symbols.push(ExtractedSymbol {
                    name,
                    qualified_name: qualified,
                    kind: declaration.emitted_kind(),
                    span,
                    is_exported: crate::langdecl::is_exported_of(
                        lang,
                        node,
                        source,
                        &declaration.name,
                        file_symbol_name,
                    ),
                    docstring: None,
                    signature: None,
                    parent_symbol: Some(declaration.parent_symbol(file_symbol_name)),
                    body_signature: None,
                    declaration_hash: None,
                });
            }
        }
    }
    // Import extraction for the languages whose specifier names a file
    // (W0.3 move 2), after the `match` rather than inside its generic arm.
    //
    // Two of the languages served here reach `extract_node` through a
    // *specialised* arm — `hcl` has one of its own, and the C family takes the
    // `c_family` branch — so a call wired beside `langcalls::extract_calls`
    // would have missed both, including the one family that had no import
    // handler anywhere. This position is also the one that cannot rot: a
    // language gaining a specialised arm later keeps its imports, where the
    // inner position would have taken them away silently.
    //
    // Languages whose arm already pushes imports — Python, JS/TS, Rust, Go —
    // are absent from the dispatcher's match, so nothing is counted twice.
    crate::langimports::extract_imports(lang, node, source, imports);
    maybe_push_name_reference(node, source, lang, file_symbol_name, references);
}

/// Grammar keys that parse the C family.
///
/// Metal has no grammar of its own — its spec routes it to `cpp` — so `"cpp"`
/// covers both, exactly as `is_metal_path` exists to tell them apart again when
/// a rule holds for one and not the other.
pub(crate) fn is_c_family_grammar(lang: &str) -> bool {
    matches!(lang, "c" | "cpp" | "objc" | "cuda")
}

/// Whether `path` is a C-family header rather than an implementation file.
///
/// In C, C++ and Objective-C the header *is* the declaration surface: a
/// definition in a `.c`/`.cpp`/`.m` is an implementation detail unless a header
/// publishes it. This is the only structural evidence of visibility the family
/// has, and `c_family_is_exported` is its only consumer.
fn is_c_header_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    C_HEADER_EXTENSIONS
        .iter()
        .any(|extension| lower.ends_with(extension))
}

/// The C-family header extensions, taken from the frozen `LANGUAGE_SPECS`:
/// `.h` is C's, `.hh`/`.hpp`/`.hxx` are C++'s, `.cuh` is CUDA's. An extension
/// the registry does not list never reaches a C-family grammar, so adding one
/// here would be configuration that can never fire. `devmap-analyze` keeps a
/// mirror of this list, and the two are pinned to agree by
/// `the_two_header_tables_agree_extension_by_extension`.
const C_HEADER_EXTENSIONS: &[&str] = &[".h", ".hh", ".hpp", ".hxx", ".cuh"];

/// A declaration's own leading tokens: the text before its declarator, bounded.
///
/// Two C-family facts are absent from the parse tree and can only be read here.
/// tree-sitter-cuda drops `__global__`/`__device__` entirely — probing
/// `__global__ void kern(int*){}` yields a `function_definition` carrying no
/// trace of the qualifier — and Metal's `kernel`/`vertex`/`fragment` are parsed
/// by the C++ grammar, which does not model them either. Stopping at the
/// declarator means a parameter, an attribute on an argument, or a body token
/// can never promote an ordinary helper, and the window bounds the scan on a
/// large declaration.
fn c_declaration_head<'a>(node: Node, source: &'a str) -> &'a str {
    const HEAD_WINDOW: usize = 256;
    let start = node.start_byte();
    let end = node
        .child_by_field_name("declarator")
        .map_or_else(|| node.end_byte(), |declarator| declarator.start_byte())
        .min(start + HEAD_WINDOW)
        .max(start);
    // The window edge is an arbitrary byte offset, so it lands mid-character
    // whenever the head contains any multi-byte text — a comment, an identifier
    // in a non-ASCII script. `.get()` then yielded `None` and the whole head
    // became `""`, so `head_has_word("__global__")` answered `false` for a file
    // whose first ten bytes are `__global__`, and the kernel lost the
    // entry-point exemption that keeps it out of the dead-code report.
    let end = crate::langcalls::scope::floor_char_boundary(source, end).max(start);
    source.get(start..end).unwrap_or_default()
}

/// Whether `head` contains `word` as a whole token rather than as a substring.
///
/// `static` must not be found inside `staticAssert`, and `kernel` must not be
/// found inside `kernelSize`.
fn head_has_word(head: &str, word: &str) -> bool {
    head.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .any(|token| token == word)
}

/// Whether a C-family declaration carries explicit evidence of an external
/// interface.
///
/// Only markers that *say so* count. `extern "C"` exists so another language can
/// call the symbol, and `dllexport`/`visibility` exist so a shared object can;
/// each is direct evidence that a call site may lie outside the indexed corpus.
/// A bare non-`static` definition is deliberately not evidence: it has external
/// linkage, but so does every ordinary helper in every `.c` file, which is
/// exactly why linkage alone cannot serve as the visibility test.
fn c_declaration_is_explicitly_external(node: Node, source: &str) -> bool {
    let head = c_declaration_head(node, source);
    if head.contains("dllexport") || head.contains("visibility") {
        return true;
    }
    // `extern "C" { … }` and `extern "C" int f()` both wrap the declaration in a
    // linkage specification. Stop at the first enclosing body so a declaration
    // merely nested somewhere inside one cannot inherit it.
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        match parent.kind() {
            "linkage_specification" => return true,
            "function_definition" | "compound_statement" | "translation_unit" => return false,
            _ => ancestor = bounded_parent(parent),
        }
    }
    false
}

/// Visibility of a C-family declaration.
///
/// Anything declared in a header is public API and must never become a
/// dead-code candidate. A definition in an implementation file is not itself an
/// export — the header publishes it — so it is visible only on the explicit
/// evidence above. That puts C on the same footing as Python, where a
/// module-level function is likewise not "exported" and an uncalled one is a
/// candidate.
///
/// This replaces `generic_is_exported`, whose fallback is `!name.starts_with('_')`
/// and therefore answered `true` for 1,393 of the 1,464 C-family symbols on the
/// measurement corpus. Every one of them was exempt, so the C family had no
/// dead-code analysis at all: 1,094 reports, all of them at confidence 0.3
/// "Exported or exempt".
pub(crate) fn c_family_is_exported(node: Node, source: &str, path: &str) -> bool {
    is_c_header_path(path) || c_declaration_is_explicitly_external(node, source)
}

/// The node a C-family declarator chain ultimately names.
///
/// `function_definition -> function_declarator -> identifier` is the common
/// shape; pointer, reference and parenthesized declarators sit in between for
/// anything returning or holding a pointer.
fn c_declarator_name_node(declarator: Node) -> Option<Node> {
    let mut current = declarator;
    for _ in 0..16 {
        match current.kind() {
            "identifier"
            | "field_identifier"
            | "type_identifier"
            | "qualified_identifier"
            | "destructor_name"
            | "operator_name" => return Some(current),
            _ => {
                current = current
                    .child_by_field_name("declarator")
                    .or_else(|| current.named_child(0))?;
            }
        }
    }
    None
}

/// Whether a C-family declarator chain declares a parameter list.
///
/// This is what makes a definition a *function* rather than something the
/// grammar could not parse. The walk follows `declarator` links and the
/// parenthesised/pointer wrappers that sit between, so
/// `int (*handler(int))(char)` still answers yes.
fn c_declarator_declares_parameters(declarator: Node) -> bool {
    let mut current = declarator;
    for _ in 0..16 {
        if current.kind() == "function_declarator" {
            return true;
        }
        let Some(next) = current
            .child_by_field_name("declarator")
            .or_else(|| current.named_child(0))
        else {
            return false;
        };
        current = next;
    }
    false
}

/// Splits a C++ qualified name into `(innermost scope, final name)`.
///
/// tree-sitter-cpp nests these to the right — `a::b::c` is `a::(b::c)` — so the
/// scope that owns the final name is the *innermost* one. `void ns::S::m()`
/// therefore yields `(Some("S"), "m")`, which is the type the in-class
/// declaration would have given, rather than the namespace.
fn split_qualified_identifier(node: Node, source: &str) -> (Option<String>, String) {
    let mut scope = None;
    let mut current = node;
    for _ in 0..16 {
        if current.kind() != "qualified_identifier" {
            break;
        }
        let Some(name) = current.child_by_field_name("name") else {
            break;
        };
        scope = current
            .child_by_field_name("scope")
            .map(|node| get_node_text(node, source))
            .filter(|text| !text.is_empty());
        current = name;
    }
    (scope, get_node_text(current, source))
}

/// The full Objective-C selector a method definition declares: `run`, `setX:`,
/// `a:b:`.
///
/// The whole selector, not its first part, is the method's identity — `-a:b:`
/// and `-a:c:` are different methods. The call site is built by the matching
/// rule in `objc_message_selector`, so definition and call join.
fn objc_selector(node: Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    let mut selector = String::new();
    for (index, child) in children.iter().enumerate() {
        if child.kind() != "identifier" {
            continue;
        }
        let text = get_node_text(*child, source);
        if text.is_empty() {
            continue;
        }
        selector.push_str(&text);
        if children
            .get(index + 1)
            .is_some_and(|next| next.kind() == "method_parameter")
        {
            selector.push(':');
        }
    }
    (!selector.is_empty()).then_some(selector)
}

/// The selector a `message_expression` sends: `[self run]` is `run`,
/// `[self a:1 b:2]` is `a:b:`.
///
/// tree-sitter-objc gives a keyword message one `method` field *per* selector
/// part, so `child_by_field_name` sees only the first and would report `a:b:`
/// as `a` — an identity that no two-part method carries.
fn objc_message_selector(node: Node, source: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut argument_count = 0usize;
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let field = cursor.field_name();
            let child = cursor.node();
            match field {
                Some("method") => parts.push(get_node_text(child, source)),
                Some("receiver") => {}
                _ => {
                    // Anything else named, once a selector part has been seen,
                    // is an argument to it.
                    if child.is_named() && !parts.is_empty() {
                        argument_count += 1;
                    }
                }
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    if parts.is_empty() || parts.iter().any(String::is_empty) {
        return None;
    }
    Some(if argument_count == 0 {
        parts.concat()
    } else {
        parts
            .iter()
            .map(|part| format!("{part}:"))
            .collect::<String>()
    })
}

/// Name of an Objective-C `@interface`/`@implementation` container.
///
/// tree-sitter-objc puts the class name in an unnamed `identifier` child rather
/// than in a `name` field, so the generic `name`-field lookup finds nothing.
fn objc_container_name(node: Node, source: &str) -> Option<String> {
    get_child_text(node, "name", source)
        .filter(|name| !name.is_empty())
        .or_else(|| {
            let mut cursor = node.walk();
            let found = node
                .named_children(&mut cursor)
                .find(|child| child.kind() == "identifier")
                .map(|child| get_node_text(child, source))
                .filter(|name| !name.is_empty());
            found
        })
}

/// Whether this Objective-C `@interface` is also implemented in the same file.
///
/// `@interface Foo` and `@implementation Foo` are two halves of one class, so
/// emitting a symbol for each puts two nodes carrying one qualified name into
/// the graph — the duplicate-identity defect SC14 tracks, and a broken join key.
/// The implementation wins because it is the definition; a header carrying only
/// the `@interface` still yields the class, and a `.h`/`.m` pair are different
/// files whose qualified names never collided in the first place.
fn objc_interface_is_implemented_here(node: Node, source: &str) -> bool {
    let implementation_kind = match node.kind() {
        "class_interface" => "class_implementation",
        "category_interface" => "category_implementation",
        _ => return false,
    };
    let Some(name) = objc_container_name(node, source) else {
        return false;
    };
    let mut root = node;
    while let Some(parent) = bounded_parent(root) {
        root = parent;
    }
    let mut cursor = root.walk();
    let found = root.named_children(&mut cursor).any(|sibling| {
        sibling.kind() == implementation_kind
            && objc_container_name(sibling, source).as_deref() == Some(name.as_str())
    });
    found
}

/// Whether `node` is the name of a C-family type specifier that has no body,
/// i.e. a *use* of an already-declared type rather than a declaration of one.
///
/// C spells `struct chunk *prev` and `struct chunk { … }` with the same node
/// kind and the same `name` field; only the definition carries a body.
fn is_bodyless_type_specifier_name(node: Node) -> bool {
    let Some(parent) = bounded_parent(node) else {
        return false;
    };
    matches!(
        parent.kind(),
        "struct_specifier" | "union_specifier" | "enum_specifier" | "class_specifier"
    ) && parent.child_by_field_name("body").is_none()
        && parent
            .child_by_field_name("name")
            .is_some_and(|name| name.id() == node.id())
}

/// Whether `node` is the identifier an Objective-C declaration names itself
/// with.
///
/// Objective-C hangs both a container's name and a method's selector parts off
/// *unnamed* `identifier` children, so none of them is reachable through the
/// `name`-field tests in `is_defining_name` and every declaration read as a
/// reference to itself — the same shape as the C-family declarator defect, in
/// the one C-family grammar that does not use declarators.
///
/// A superclass or an adopted protocol is deliberately still a reference: only
/// the container's *first* identifier is its own name.
fn is_objc_declaring_identifier(node: Node) -> bool {
    if node.kind() != "identifier" {
        return false;
    }
    let Some(parent) = bounded_parent(node) else {
        return false;
    };
    match parent.kind() {
        "class_interface"
        | "class_implementation"
        | "category_interface"
        | "category_implementation"
        | "protocol_declaration" => {
            let mut cursor = parent.walk();
            let first = parent
                .named_children(&mut cursor)
                .find(|child| child.kind() == "identifier")
                .map(|child| child.id());
            first == Some(node.id())
        }
        // Go also spells a method `method_declaration` and JavaScript spells one
        // `method_definition`, but both name it in a `name` field. Requiring the
        // field to be absent keeps this rule to the grammar that has no other
        // way of naming a method.
        "method_definition" | "method_declaration" => parent.child_by_field_name("name").is_none(),
        _ => false,
    }
}

/// `(owner, name)` a C-family callable carries as a graph node.
///
/// **One canonical owner for C-family identity**, read by the symbol emitter and
/// by `enclosing_callable_qualified` alike. SC14 recorded that deriving member
/// identity and call-scope identity through two independent walks is what makes
/// them silently disagree; for the C family they did disagree in two ways at
/// once — `callable_binding_name` looks for a `name` field no C-family
/// declaration has, and `enclosing_type_name` knows nothing of
/// `struct_specifier` — so an edge out of any C-family function named a source
/// no node carried. Both now come from here.
///
/// `Some(owner)` for a member, `None` for a free function. An out-of-line
/// definition (`int S::m(…)`) resolves to the same `(S, m)` its in-class
/// declaration would, so the two are one symbol rather than two.
fn c_callable_identity(node: Node, source: &str) -> Option<(Option<String>, String)> {
    if node.kind() == "method_definition" {
        // Objective-C. The JS grammar reuses this node kind but puts the name in
        // a `name` field, so a JS method never reaches the selector rule.
        if node.child_by_field_name("name").is_none() {
            let selector = objc_selector(node, source)?;
            return Some((objc_enclosing_container(node, source), selector));
        }
        return None;
    }
    if is_c_macro_invocation(node) {
        return None;
    }
    let declarator = node.child_by_field_name("declarator")?;
    // A function definition declares parameters. Anything else that a C-family
    // grammar reports as a `function_definition` is a construct it could not
    // parse, and there are two common ones — both of which appear thousands of
    // times in a real header tree.
    //
    // `.h` is ambiguous between C and C++ and routes to the C grammar, which
    // has no notion of a namespace, so `namespace at { … }` parses as a
    // definition with `namespace` as the return type and `at` as the whole
    // declarator. LibTorch declares `namespace at` in 2,486 of 3,028 sampled
    // headers, and taking each as a function put 2,486 nodes named `at` into
    // one corpus. `struct TORCH_API Foo { … }` misparses the same way, the
    // macro standing where the grammar expects the declarator.
    //
    // Neither has a parameter list, and every real definition does — including
    // constructors, destructors, conversion operators and trailing-return-type
    // functions, all of which keep a `function_declarator` in the chain.
    if !c_declarator_declares_parameters(declarator) {
        return None;
    }
    let name_node = c_declarator_name_node(declarator)?;
    let (scope, name) = if name_node.kind() == "qualified_identifier" {
        split_qualified_identifier(name_node, source)
    } else {
        (None, get_node_text(name_node, source))
    };
    if name.is_empty() {
        return None;
    }
    // A qualified declarator names its own owner; otherwise the owner is the
    // type whose body lexically encloses the definition.
    Some((scope.or_else(|| c_enclosing_type(node, source)), name))
}

/// Whether a C-family `function_definition` is really a function-like macro
/// invocation that happens to be followed by a brace-enclosed block.
///
/// `PYBIND11_MODULE(NAME, m) { … }`, `TORCH_LIBRARY(ops, m) { … }` and
/// `CMARK_DEFINE_LOCK(arena)` are macros, but each has a function's *shape*, and
/// the grammar runs no preprocessor, so each is reported as a
/// `function_definition`. The discriminator is the return type: a C or C++
/// function definition carries one, and the only definitions that legitimately
/// do not are constructors, destructors and conversion operators, which either
/// name their type in a qualified declarator or sit inside a class body.
///
/// Suppressed here, in the single canonical owner of C-family identity, rather
/// than at the symbol emitter: a macro that stops being a symbol must also stop
/// being a *call scope*, or the calls inside its block would name a source
/// symbol no node carries — the orphaned-edge failure of SC9/SC10. Attributing
/// them to the file is what the code did before any of this and is truthful.
///
/// Accepted cost: a K&R-era definition relying on implicit `int` is suppressed
/// too. That errs toward omitting a symbol rather than toward inventing one,
/// which is the safe direction for dead-code reporting.
fn is_c_macro_invocation(node: Node) -> bool {
    if node.kind() != "function_definition" || node.child_by_field_name("type").is_some() {
        return false;
    }
    let Some(name_node) = node
        .child_by_field_name("declarator")
        .and_then(c_declarator_name_node)
    else {
        return false;
    };
    if name_node.kind() != "identifier" {
        return false;
    }
    // An in-class constructor is a bare `identifier` with no return type too, so
    // anything inside a type body is left alone.
    !matches!(
        bounded_parent(node).map(|parent| parent.kind()),
        Some("field_declaration_list")
    )
}

/// Nearest enclosing Objective-C class container.
fn objc_enclosing_container(node: Node, source: &str) -> Option<String> {
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        if matches!(
            parent.kind(),
            "class_implementation"
                | "class_interface"
                | "category_implementation"
                | "category_interface"
        ) {
            return objc_container_name(parent, source);
        }
        ancestor = bounded_parent(parent);
    }
    None
}

/// Nearest enclosing C-family type whose body contains `node`.
///
/// Stops at a function body so a type declared inside a function does not adopt
/// a definition that merely follows it, and stops at a `function_definition` so
/// a nested lambda is not made a member.
fn c_enclosing_type(node: Node, source: &str) -> Option<String> {
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        match parent.kind() {
            "struct_specifier" | "class_specifier" | "union_specifier" => {
                return get_child_text(parent, "name", source).filter(|name| !name.is_empty());
            }
            "class_implementation"
            | "class_interface"
            | "category_implementation"
            | "category_interface" => return objc_container_name(parent, source),
            "function_definition" | "compound_statement" => return None,
            _ => ancestor = bounded_parent(parent),
        }
    }
    None
}

/// Whether `node` sits inside a C-family attribute.
///
/// `__attribute__((visibility("default")))` parses its contents as a real
/// `call_expression`, so without this guard every such attribute would emit a
/// call to `visibility` — a callee that can never resolve and that names
/// nothing in the program.
fn is_inside_c_attribute(node: Node) -> bool {
    let mut ancestor = bounded_parent(node);
    for _ in 0..16 {
        let Some(parent) = ancestor else {
            return false;
        };
        if matches!(
            parent.kind(),
            "attribute_specifier"
                | "attribute_declaration"
                | "attribute"
                | "ms_declspec_modifier"
                | "alignas_qualifier"
        ) {
            return true;
        }
        ancestor = bounded_parent(parent);
    }
    false
}

/// `(kind, owner, name)` for a C-family declaration node.
///
/// Diverges from `generic_declaration` in exactly the places where the C family
/// does: functions take their identity from `c_callable_identity`, Objective-C
/// containers name themselves in an unnamed child, and a bare prototype is not a
/// declaration this graph records.
pub(crate) fn c_family_declaration(
    node: Node,
    source: &str,
) -> Option<crate::langdecl::Declaration> {
    match node.kind() {
        "class_implementation"
        | "class_interface"
        | "category_implementation"
        | "category_interface" => {
            if objc_interface_is_implemented_here(node, source) {
                return None;
            }
            crate::langdecl::Declaration::new(
                SymbolKind::Class,
                None,
                objc_container_name(node, source)?,
            )
        }
        "function_definition" | "method_definition" => {
            let (owner, name) = c_callable_identity(node, source)?;
            crate::langdecl::Declaration::new(SymbolKind::Function, owner, name)
        }
        // A function-like macro is a callable in C, and SC31 made that
        // observable: once C-family calls are extracted, `ACTIONS(1)` is
        // recorded as a call, and with no symbol behind the `#define` the graph
        // holds a call whose target it never emitted. On this repository that
        // asymmetry alone put 13,630 rows into the defect tier — 7,372 of them
        // `ACTIONS` inside one generated `parser.c` table — which is precisely
        // the drowning-out that SC18 and SC30 exist to prevent.
        //
        // Only the *function-like* form. `preproc_def` (`#define MAX 10`) is a
        // constant: it carries no `parameters`, it is never the callee of a
        // call, and emitting it would add a node per object-like macro for no
        // resolution benefit. Recording the call while refusing the definition
        // is the asymmetry being fixed here, so the test pins both directions.
        "preproc_function_def" => {
            let name = get_child_text(node, "name", source)?;
            if name.is_empty() || node.child_by_field_name("parameters").is_none() {
                return None;
            }
            crate::langdecl::Declaration::new(SymbolKind::Function, None, name)
        }
        // A prototype declares; it does not define. Emitting it alongside the
        // definition would put two nodes carrying one name into the graph, and
        // an ambiguous resolution downgrades every call to that name to the
        // speculative tier — which `analyze_liveness` reports as
        // `only_ambiguous_callers` rather than as a resolved call. The header
        // stays the visibility evidence (`c_family_is_exported`) without
        // becoming a second symbol.
        //
        // This is also what the code already did, by accident:
        // `generic_declaration_name` looks one level too deep for a declarator
        // that is a bare identifier and returned `None` for every prototype.
        // Making it explicit keeps the new identity rule from silently
        // reversing it.
        "function_declarator" => None,
        // `struct arena_chunk *prev` is a *use* of a type, not a declaration of
        // one, and C spells the use with the same node kind as the definition —
        // the difference is that only a definition has a `body`.
        //
        // Without this every mention of an elaborated type emitted another
        // symbol carrying that type's name: `arena.c` alone produced eight
        // `arena_chunk` nodes for one struct. Duplicate qualified names are a
        // broken join key (SC14), and with C-family visibility now meaningful
        // each copy also became its own dead-code candidate.
        "struct_specifier" | "union_specifier" | "enum_specifier" | "class_specifier" => {
            node.child_by_field_name("body")?;
            let kind = generic_symbol_kind(node.kind())?;
            let name = generic_declaration_name(node, source).filter(|name| !name.is_empty())?;
            crate::langdecl::Declaration::new(kind, generic_enclosing_type(node, source), name)
        }
        _ => {
            let kind = generic_symbol_kind(node.kind())?;
            let name = generic_declaration_name(node, source).filter(|name| !name.is_empty())?;
            crate::langdecl::Declaration::new(kind, generic_enclosing_type(node, source), name)
        }
    }
}

/// Why a C-family function is reached without an observable call site.
///
/// Each is evidence from the declaration itself, never a guess from the name
/// alone: `main` is the program entry point the C standard names, a CUDA
/// `__global__` function is launched from host code by name, and an
/// Objective-C method is reachable through the runtime's selector dispatch —
/// delegates, target/action and protocol conformance all invoke methods that no
/// call expression in the corpus mentions. Metal's shader qualifiers are handled
/// by `metal_shader_entry_reason_of`, which already owns that rule.
fn c_family_entry_point_reason(
    node: Node,
    source: &str,
    path: &str,
    name: &str,
) -> Option<&'static str> {
    if node.kind() == "method_definition" {
        return Some("Objective-C method reachable by runtime selector dispatch");
    }
    if name == "main" && !is_c_header_path(path) {
        return Some("Program entry point");
    }
    // libFuzzer defines these two names as the harness's entry points and calls
    // them by name from its own driver, exactly as the C standard defines
    // `main`. Neither is ever called from the corpus that declares it.
    if matches!(name, "LLVMFuzzerTestOneInput" | "LLVMFuzzerInitialize") {
        return Some("libFuzzer harness entry point");
    }
    if head_has_word(c_declaration_head(node, source), "__global__") {
        return Some("CUDA kernel launched by name from host code");
    }
    None
}

/// The interface a C-family header publishes.
///
/// A prototype in a header is precisely an export declaration: it says this
/// unit's interface contains this name, and it is the only place C, C++ and
/// Objective-C state that. `analyze_liveness` joins these against definitions in
/// implementation files, which is the cross-file half of C-family visibility —
/// the same split SC6a used for Go interfaces, where extraction records the
/// per-file evidence and analysis performs the join because only it holds every
/// file at once.
///
/// Recorded as an *export* rather than as a symbol deliberately. A prototype and
/// its definition are one function, so emitting a symbol for each would put two
/// nodes carrying one name into the graph and make every call to that name
/// resolve ambiguously — measured on this corpus as the difference between one
/// `cmark_node` node and nine, where the nine made 841 real type references
/// unresolvable. `Extraction::exports` feeds no edge and no symbol index, so
/// this adds evidence without adding graph.
fn extract_c_header_export(
    node: Node,
    source: &str,
    path: &str,
    exports: &mut Vec<ExtractedExport>,
) {
    if !is_c_header_path(path) {
        return;
    }
    let exported_name = match node.kind() {
        // C and C++: `int yaml_emitter_delete(yaml_emitter_t *emitter);`
        "function_declarator" => {
            // A definition's own declarator is not a declaration of an
            // interface; the definition is already the symbol.
            if bounded_parent(node).is_some_and(|parent| parent.kind() == "function_definition") {
                return;
            }
            let Some(name_node) = c_declarator_name_node(node) else {
                return;
            };
            if name_node.kind() == "qualified_identifier" {
                split_qualified_identifier(name_node, source).1
            } else {
                get_node_text(name_node, source)
            }
        }
        // Objective-C: a method spec inside `@interface`/`@protocol`.
        "method_declaration" => match objc_selector(node, source) {
            Some(selector) => selector,
            None => return,
        },
        _ => return,
    };
    if exported_name.is_empty() {
        return;
    }
    exports.push(ExtractedExport {
        exported_name,
        local_name: None,
        module_specifier: None,
        span: node_span(node),
    });
}

/// Calls made by C, C++, Objective-C, CUDA and Metal code.
///
/// The C family reached `extract_node`'s generic arm, which emits declarations
/// and nothing else, so **not one C-family call was extracted** — measured as 0
/// `Calls` edges across 183 real C-family files and 9,082 C++ headers. Every
/// consumer that reads the call graph (`impact`, `trace`, dead code, the PDG)
/// was answering for the whole family from nothing.
///
/// The shapes are taken from each grammar's own parse tree rather than assumed:
/// a plain call, a call through a function pointer and a CUDA `<<<…>>>` launch
/// are all `call_expression` with a `function` field, so they need no separate
/// handling, while `new` and Objective-C messages are distinct node kinds.
fn extract_c_family_call(
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    if is_inside_c_attribute(node) {
        return;
    }
    let span = node_span(node);
    let (callee_name, receiver_expr, target, reference_kind) = match node.kind() {
        "call_expression" => {
            let Some(function) = node.child_by_field_name("function") else {
                return;
            };
            if is_anonymous_callable(function.kind()) {
                return;
            }
            let Some((callee, receiver)) = split_call_target(function, source) else {
                return;
            };
            (callee, receiver, function, ReferenceKind::Call)
        }
        // `new S(…)` constructs `S`. Fail closed on `new int[10]`: a predeclared
        // type names no user symbol, so recording it would manufacture a callee
        // that can never resolve — the rule SC17 established for Go composite
        // literals.
        "new_expression" => {
            let Some(type_node) = node.child_by_field_name("type") else {
                return;
            };
            if type_node.kind() == "primitive_type" {
                return;
            }
            let Some((callee, receiver)) = split_call_target(type_node, source) else {
                return;
            };
            (callee, receiver, type_node, ReferenceKind::Constructor)
        }
        "message_expression" => {
            let Some(selector) = objc_message_selector(node, source) else {
                return;
            };
            let receiver = node
                .child_by_field_name("receiver")
                .map(|receiver| get_node_text(receiver, source))
                .filter(|text| is_user_ident(text));
            (selector, receiver, node, ReferenceKind::Call)
        }
        _ => return,
    };
    if callee_name.is_empty() || !is_user_ident(&callee_name.replace(':', "")) {
        return;
    }
    references.push(extracted_reference(
        target,
        source,
        file_symbol_name,
        callee_name.clone(),
        reference_kind,
        assignment_binding(node, source),
    ));
    calls.push(ExtractedCall {
        caller_symbol: enclosing_callable_qualified(node, source, file_symbol_name),
        callee_name,
        receiver_expr,
        span,
    });
}

/// The callee's identity — its name, and the receiver expression it is reached
/// through — or `None` when the target names nothing.
///
/// **Returning `None` is the point of this signature.** Every arm below can fail
/// to find a name, and the historical answer was to record the target's own
/// source text instead. That is the SC26 defect class, and the original fix
/// reached five of its shapes while the same text fallback survived in every
/// other arm. Measured on the DevCouncil repository, it was still recording a
/// Python curried call (`app.command(name="apply-patch")(fn)`), a minified JS
/// immediately-invoked literal (`(function(t){…})(x)`) and a Rust
/// immediately-invoked closure as callee *names* — rows that can never join to
/// a symbol, crowding the one tier reserved for genuine resolution defects.
///
/// Two rungs, structural then lexical. An inline function literal has no callee
/// identity by construction. Anything else must be identifier-shaped: every
/// grammar that reaches here names a callee with a single identifier token, so
/// text carrying an argument list, an operator or a brace is an *expression*
/// that fell through to the catch-all below.
///
/// Nothing real is lost by refusing them. The inner call of a curried
/// expression is itself a call node extracted on its own — `command` and
/// `_default_runner` keep the edges they always had — and an inline literal has
/// no symbol to point at. The rule is fail-closed in the direction that
/// matters: it can only drop a name no symbol could carry.
pub(crate) fn split_call_target(
    function_node: Node,
    source: &str,
) -> Option<(String, Option<String>)> {
    if is_anonymous_callable(function_node.kind()) {
        return None;
    }
    let (name, receiver) = split_call_target_inner(function_node, source, 0);
    is_callee_identity(&name).then_some((name, receiver))
}

/// Whether text can be the *name* of a callee, as opposed to an expression.
///
/// Deliberately not `is_user_ident`, which is ASCII-only and rejects `#`. Both
/// would cost real edges here: Python, Go and Rust all admit non-ASCII
/// identifiers, and a JavaScript private method is declared *and* called as
/// `#name`, so the symbol and the callee agree and the edge resolves. Rust raw
/// identifiers (`r#type`) carry the same character for the same reason.
///
/// What it excludes is everything an expression brings with it — whitespace,
/// brackets, operators, quotes, `.`, `,`, `;` — which is the signature of source
/// text that reached a fallback instead of a name.
pub(crate) fn is_callee_identity(name: &str) -> bool {
    let Some(first) = name.chars().next() else {
        return false;
    };
    (first.is_alphabetic() || matches!(first, '_' | '$' | '#'))
        && name
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '$' | '#'))
}

/// Whether a call target is an inline function literal rather than a name.
///
/// An immediately-invoked literal is a call with no callee *identity*: there is
/// no symbol to join to, and recording its source text as a callee name — which
/// is what a text fallback does — manufactures a row that can never match.
pub(crate) fn is_anonymous_callable(kind: &str) -> bool {
    matches!(
        kind,
        "func_literal" | "closure_expression" | "function_expression" | "arrow_function" | "lambda"
    )
}

/// The longest a receiver expression may be recorded as.
///
/// X44. `receiver_expr` is documented as existing "so the classification can be
/// audited rather than trusted", which means being grouped and read. A receiver
/// that is a unique 38,644-character string — the measured maximum on this
/// repository, the whole body of one function, stored as the "receiver" of a
/// method called on the end of it — groups with nothing and answers no
/// question, while costing the store a megabyte of duplicated source.
///
/// 64 characters holds every receiver that is genuinely a path of names, which
/// is what this field is for. Past it the value is cut and **marked** cut, so a
/// truncated string can never be read as a whole expression.
const MAX_RECEIVER_CHARS: usize = 64;

/// A receiver expression reduced to something a reader can group by.
///
/// One line, bounded, and marked when it was cut. Collapsing whitespace is part
/// of the identity and not cosmetic: 4,973 receivers on this repository contain
/// a newline, and every one of them is a block that was copied into a column
/// whose job is to name a value.
fn bound_receiver_text(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX_RECEIVER_CHARS {
        return flat;
    }
    let mut out: String = flat.chars().take(MAX_RECEIVER_CHARS).collect();
    out.push('\u{2026}');
    out
}

/// Grammar keys for a call, across the languages this crate splits receivers
/// for. A call's identity is the callee it names, never its argument list.
fn is_call_node(kind: &str) -> bool {
    matches!(
        kind,
        "call"
            | "call_expression"
            | "function_call_expression"
            | "invocation_expression"
            | "macro_invocation"
            | "member_call_expression"
            | "method_call"
            | "method_invocation"
            | "new_expression"
            | "object_creation_expression"
            | "scoped_call_expression"
    )
}

/// Grammar keys for member access — the same three spellings
/// `member_access_receiver` already reconciles, plus Rust's and C's.
fn member_access_fields(kind: &str) -> Option<(&'static str, &'static str)> {
    match kind {
        "attribute" => Some(("object", "attribute")),
        "member_expression" => Some(("object", "property")),
        "selector_expression" => Some(("operand", "field")),
        "field_expression" => Some(("value", "field")),
        _ => None,
    }
}

/// What a receiver expression *is*, rather than what it says.
///
/// X44. The receiver used to be `get_node_text` of the receiver node, whole, so
/// `runner.invoke(app, ["init"]).output.strip()` recorded its entire left-hand
/// side. The classifier reads only the receiver's leftmost segment, and a
/// reader auditing the ledger needs rows that group — neither is served by a
/// copy of the source.
///
/// The reduction is structural, not textual: a receiver that is a **call** is
/// named by that call's callee, and a member access is `<object identity>.
/// <property>`. Anything this walk does not recognise keeps its text, bounded.
pub(crate) fn receiver_identity(node: Node, source: &str, depth: usize) -> String {
    if depth > 16 {
        return bound_receiver_text(&get_node_text(node, source));
    }
    let fallback = || bound_receiver_text(&get_node_text(node, source));
    let kind = node.kind();
    if matches!(
        kind,
        "await_expression" | "parenthesized_expression" | "non_null_expression"
    ) {
        return match node.named_child(0) {
            Some(inner) => receiver_identity(inner, source, depth + 1),
            None => fallback(),
        };
    }
    if is_call_node(kind) {
        return node
            .child_by_field_name("function")
            .or_else(|| node.child_by_field_name("constructor"))
            .map(|target| {
                // The inner call's callee **with its own receiver**, not the
                // callee alone. Measured: reducing `runner.invoke(app, [...])`
                // to `invoke` cost 1,235 `External` classifications on this
                // repository, because the classifier roots its answer at the
                // receiver's leftmost segment and `runner` is where the
                // declared type `CliRunner` — and the import that proves it
                // external — is recorded. `runner.invoke` keeps that root and
                // still drops the argument list, which is the part that made
                // the string unique.
                let (name, receiver) = split_call_target_inner(target, source, depth + 1);
                match receiver.filter(|receiver| !receiver.is_empty()) {
                    Some(receiver) if !name.is_empty() => format!("{receiver}.{name}"),
                    _ => name,
                }
            })
            .map(|identity| bound_receiver_text(&identity))
            .filter(|identity| !identity.is_empty())
            .unwrap_or_else(fallback);
    }
    if let Some((object_field, member_field)) = member_access_fields(kind) {
        let member = node
            .child_by_field_name(member_field)
            .map(|child| get_node_text(child, source))
            .filter(|text| !text.is_empty());
        let object = node
            .child_by_field_name(object_field)
            .or_else(|| node.child_by_field_name("argument"))
            .map(|child| receiver_identity(child, source, depth + 1))
            .filter(|text| !text.is_empty());
        return match (object, member) {
            (Some(object), Some(member)) => bound_receiver_text(&format!("{object}.{member}")),
            _ => fallback(),
        };
    }
    fallback()
}

/// The receiver named by `field` on `node`, as an identity.
fn receiver_from_field(node: Node, field: &str, source: &str, depth: usize) -> Option<String> {
    node.child_by_field_name(field)
        .map(|child| receiver_identity(child, source, depth + 1))
        .filter(|text| !text.is_empty())
}

fn split_call_target_inner(
    function_node: Node,
    source: &str,
    depth: usize,
) -> (String, Option<String>) {
    // The unwrapping arms below recurse. Bounded so a pathological nesting
    // cannot exhaust the stack; past the bound the raw text is returned, which
    // is the pre-existing behaviour and joins to nothing.
    if depth > 16 {
        return (get_node_text(function_node, source), None);
    }
    match function_node.kind() {
        // Wrappers that stand between a call and its real callee.
        //
        // `await invoke<Raw>('x')` is the motivating case: with a type argument
        // present, tree-sitter makes the *await expression* the call's
        // `function` field, so the whole text `await invoke` became the callee
        // and matched no symbol. Plain `await invoke('x')` was never affected,
        // which is why this hid — it needs the generic argument to appear.
        "await_expression" | "parenthesized_expression" | "non_null_expression" => {
            match function_node.named_child(0) {
                Some(inner) => split_call_target_inner(inner, source, depth + 1),
                None => (get_node_text(function_node, source), None),
            }
        }
        // Turbofish: `row.get::<_, String>(0)`, `serde_json::from_str::<V>(s)`.
        // The type arguments are not part of the callee's identity, and leaving
        // them in produced a name no symbol can carry.
        "generic_function" => match function_node.child_by_field_name("function") {
            Some(inner) => split_call_target_inner(inner, source, depth + 1),
            None => (get_node_text(function_node, source), None),
        },
        // Rust path call: `Vec::new()`, `MyType::create()`, `std::fs::write()`.
        //
        // Without this the entire path was the callee, so a user type's
        // associated function was invisible to the graph — `MyType::create` is
        // a real edge this was dropping, not just library noise. The path
        // becomes the receiver, matching how `x.method()` already splits.
        "scoped_identifier" => {
            let name = get_child_text(function_node, "name", source);
            let path = get_child_text(function_node, "path", source);
            match (name, path) {
                (Some(name), path) if !name.is_empty() => (name, path),
                _ => (get_node_text(function_node, source), None),
            }
        }
        "attribute" => (
            get_child_text(function_node, "attribute", source).unwrap_or_default(),
            receiver_from_field(function_node, "object", source, depth),
        ),
        "member_expression" => (
            get_child_text(function_node, "property", source).unwrap_or_default(),
            receiver_from_field(function_node, "object", source, depth),
        ),
        // C++ scope resolution: `ns::fn()`, `S::sm()`, `a::b::c()`.
        //
        // The whole path was previously the callee, so a namespaced or static
        // member call named a symbol nothing can carry — the C++ shape of the
        // Rust `scoped_identifier` gap SC26 closed. tree-sitter-cpp nests these
        // to the *right* (`a::(b::c)`), so the walk recurses through `name` and
        // the receiver is the innermost scope — `b` for `a::b::c()`, which is
        // the type or namespace that actually owns `c`.
        "qualified_identifier" => match function_node.child_by_field_name("name") {
            Some(name) => {
                let (callee, inner_scope) = split_call_target_inner(name, source, depth + 1);
                let scope = inner_scope
                    .or_else(|| get_child_text(function_node, "scope", source))
                    .filter(|scope| !scope.is_empty());
                (callee, scope)
            }
            None => (get_node_text(function_node, source), None),
        },
        // C++ template call: `tmplFn<int>(5)`, `std::make_unique<S>()`. The
        // type arguments are not part of the callee's identity, exactly as
        // SC26 established for Rust's turbofish.
        "template_function" => match function_node.child_by_field_name("name") {
            Some(name) => split_call_target_inner(name, source, depth + 1),
            None => (get_node_text(function_node, source), None),
        },
        // C call through a dereferenced function pointer: `(*fp)(3)`. The
        // surrounding `parenthesized_expression` is already unwrapped above,
        // which leaves the `pointer_expression` between the call and the name.
        "pointer_expression" => match function_node.child_by_field_name("argument") {
            Some(inner) => split_call_target_inner(inner, source, depth + 1),
            None => (get_node_text(function_node, source), None),
        },
        // Rust method call: `receiver.method()` is a `field_expression`
        // whose `value` is the receiver and whose `field` is the method.
        //
        // tree-sitter-c/cpp use the same node kind for `s.m()` and `p->m()` but
        // name the receiver `argument` rather than `value`, so the receiver was
        // silently dropped for the entire C family. Rust has no `argument`
        // field here, so the fallback cannot change a Rust split.
        "field_expression" => {
            let field = get_child_text(function_node, "field", source);
            let value = receiver_from_field(function_node, "value", source, depth)
                .or_else(|| receiver_from_field(function_node, "argument", source, depth));
            match (field, value) {
                (Some(field), value) if !field.is_empty() => (field, value),
                _ => (get_node_text(function_node, source), None),
            }
        }
        "selector_expression" => {
            let field = get_child_text(function_node, "field", source)
                .or_else(|| get_child_text(function_node, "selector", source));
            let operand = receiver_from_field(function_node, "operand", source, depth);
            match (field, operand) {
                (Some(field), Some(operand)) if !field.is_empty() => (field, Some(operand)),
                _ => (get_node_text(function_node, source), None),
            }
        }
        // Go package-qualified type, reachable only as a composite literal's
        // type (`genai.Part{}`) — a call's `function` field is a
        // `selector_expression`, never this. Split it the same way, so the
        // package lands in the receiver instead of producing a callee named
        // `genai.Part`, which matches how `go_type_name` names a qualified type.
        "qualified_type" => {
            let name = get_child_text(function_node, "name", source);
            let package = get_child_text(function_node, "package", source);
            match (name, package) {
                (Some(name), package) if !name.is_empty() => (name, package),
                _ => (get_node_text(function_node, source), None),
            }
        }
        _ => (get_node_text(function_node, source), None),
    }
}

fn go_receiver(node: Node, source: &str) -> Option<(String, String)> {
    let receiver = node.child_by_field_name("receiver")?;
    let mut receiver_cursor = receiver.walk();
    for param in receiver.named_children(&mut receiver_cursor) {
        if param.kind() != "parameter_declaration" {
            continue;
        }
        let recv_name = get_child_text(param, "name", source).unwrap_or_default();
        let type_node = param.child_by_field_name("type")?;
        let type_name = go_type_name(type_node, source, 0)?;
        return Some((recv_name, type_name));
    }
    None
}

thread_local! {
    /// One parser reused for macro-body probes. Creating a `Parser` per macro
    /// invocation would dominate the cost of the recovery below.
    static MACRO_PROBE_PARSER: RefCell<Option<Parser>> = const { RefCell::new(None) };
}

/// Recover calls hidden inside a Rust macro invocation (SC13).
///
/// tree-sitter parses macro arguments as an unstructured `token_tree`, so
/// `println!("{}", s.work())` flattens to loose `identifier` tokens and the call
/// disappears from the graph entirely — not resolved weakly, absent. In real
/// Rust (`println!`, `assert!`, `format!`, `vec!`) that hides a large share of
/// calls, and any symbol reached only from inside a macro reads as dead.
///
/// The token text is re-parsed with the real grammar rather than pattern-matched
/// out of the flat token stream, so nesting, chained calls and closures come out
/// correctly. Wrapping the arguments in a synthetic call makes a comma-separated
/// argument list a valid expression position.
///
/// Fail-closed: a token tree that is not a valid argument list (a declarative
/// macro body, a pattern-matching macro) yields nothing rather than a guess.
/// Spans point at the macro invocation, which is where the call textually is.
fn rust_macro_calls(
    node: Node,
    source: &str,
    file_symbol_name: &str,
) -> Vec<(String, Option<String>)> {
    let _ = file_symbol_name;
    let mut token_cursor = node.walk();
    let Some(tokens) = node
        .named_children(&mut token_cursor)
        .find(|child| child.kind() == "token_tree")
    else {
        return Vec::new();
    };
    let Some(inner) = macro_token_body(&get_node_text(tokens, source)) else {
        return Vec::new();
    };

    // Macros nest — `assert_eq!(a, format!("{}", b.c()))` hides a call two
    // levels down — so inner macro bodies are queued and probed in turn rather
    // than recursed into, which keeps the thread-local parser borrow scoped to
    // one parse at a time. The depth bound stops a pathological nest from
    // running away.
    const MAX_MACRO_DEPTH: usize = 4;
    let mut pending = vec![(inner, 0usize)];
    let mut found = Vec::new();
    while let Some((body, depth)) = pending.pop() {
        if depth > MAX_MACRO_DEPTH {
            continue;
        }
        let (calls, nested) = probe_macro_body(&body);
        found.extend(calls);
        pending.extend(nested.into_iter().map(|body| (body, depth + 1)));
    }
    found.sort();
    found.dedup();
    found
}

/// Strip a token tree's outer delimiter pair, if it has one worth probing.
fn macro_token_body(raw: &str) -> Option<String> {
    let inner = raw
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
        .or_else(|| {
            raw.strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
        })
        .or_else(|| {
            raw.strip_prefix('{')
                .and_then(|rest| rest.strip_suffix('}'))
        })?;
    // No parenthesis means no call to recover; skip the parse entirely.
    (inner.contains('(') && !inner.trim().is_empty()).then(|| inner.to_string())
}

/// Parse one macro body and return `(calls, nested macro bodies)`.
///
/// Returns nothing at all when the body is not a valid argument list — a
/// declarative macro definition, a pattern-matching macro. A fabricated call is
/// worse than a missing one.
fn probe_macro_body(inner: &str) -> (Vec<(String, Option<String>)>, Vec<String>) {
    const PROBE: &str = "__devmap_macro_probe";
    let probe_source = format!("fn {PROBE}() {{ {PROBE}({inner}); }}");

    let parsed = MACRO_PROBE_PARSER.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            let mut parser = Parser::new();
            if parser
                .set_language(&tree_sitter_rust::LANGUAGE.into())
                .is_err()
            {
                return None;
            }
            *slot = Some(parser);
        }
        slot.as_mut()
            .and_then(|parser| parser.parse(&probe_source, None))
    });

    let Some(tree) = parsed else {
        return (Vec::new(), Vec::new());
    };
    let root = tree.root_node();
    if root.has_error() {
        return (Vec::new(), Vec::new());
    }

    let mut calls = Vec::new();
    let mut nested = Vec::new();
    let mut stack = vec![root];
    while let Some(current) = stack.pop() {
        match current.kind() {
            "call_expression" => {
                if let Some(function) = current.child_by_field_name("function") {
                    if let Some((callee, receiver)) = split_call_target(function, &probe_source) {
                        if callee != PROBE {
                            calls.push((callee, receiver));
                        }
                    }
                }
            }
            "macro_invocation" => {
                let mut nested_cursor = current.walk();
                // Bound in its own statement so the borrowing iterator is
                // dropped here rather than living to the end of the `if let`.
                let tokens = current
                    .named_children(&mut nested_cursor)
                    .find(|child| child.kind() == "token_tree");
                if let Some(tokens) = tokens {
                    if let Some(body) = macro_token_body(&get_node_text(tokens, &probe_source)) {
                        nested.push(body);
                    }
                }
            }
            _ => {}
        }
        push_named_children(current, &mut stack);
    }
    (calls, nested)
}

/// Qualified name for a symbol that has no enclosing *type*.
///
/// A definition inside a function body belongs to that function, not to the
/// file. Qualifying it `file::name` made every same-named local definition in a
/// file collapse to one identity (SC14): `run` declared inside 15 different
/// test functions became one node, `struct Out` declared in three different
/// functions became one node. A qualified name is the graph's join key, so
/// collisions are not a cosmetic naming issue — they make distinct code
/// indistinguishable to every edge that names it.
///
/// Top-level definitions are unaffected: with no enclosing callable this is
/// exactly the previous `file::name`.
fn scoped_qualified_name(node: Node, source: &str, file_symbol_name: &str, name: &str) -> String {
    match enclosing_callable_qualified(node, source, file_symbol_name) {
        Some(scope) => format!("{scope}.{name}"),
        None => format!("{file_symbol_name}::{name}"),
    }
}

/// Strip references, pointers and generics down to a Rust type's bare name.
///
/// Bounded depth for the same reason as `rust_type_qualifier` below: the
/// unwrapping arms recurse once per layer, and `&&&…&T` written deeply enough
/// exhausts the stack. A stack overflow aborts the process, so one hostile or
/// generated file would take the whole build with it. Past the bound the type
/// is simply not recovered, which costs a receiver binding and guesses nothing.
fn rust_type_name(node: Node, source: &str, depth: usize) -> Option<String> {
    if depth > 16 {
        return None;
    }
    match node.kind() {
        "reference_type" | "pointer_type" => node
            .child_by_field_name("type")
            .and_then(|inner| rust_type_name(inner, source, depth + 1)),
        "generic_type" => node
            .child_by_field_name("type")
            .and_then(|inner| rust_type_name(inner, source, depth + 1)),
        "type_identifier" | "scoped_type_identifier" => {
            let text = get_node_text(node, source);
            let bare = text.rsplit("::").next().unwrap_or(&text);
            Some(bare.split('<').next().unwrap_or(bare).trim().to_string())
        }
        _ => None,
    }
}

/// One leaf of a `use` tree: the module it comes from and the name it binds.
struct RustUseLeaf {
    /// The path segments before the imported name — `["std", "collections"]`
    /// for `std::collections::BTreeMap`.
    module: Vec<String>,
    /// The name imported from that module, or `None` for a glob.
    name: Option<String>,
    /// The local binding, when `as` renamed it.
    alias: Option<String>,
}

/// How deeply a `use` tree may nest before recovery stops.
///
/// A `use` group is a tree and this walk recurses per level, so an adversarially
/// nested statement is a stack-overflow shape — the same reason
/// [`rust_type_name`] is bounded. There is deliberately **no cap on the number
/// of leaves**: leaves cost source bytes, which `MAX_SOURCE_BYTES` already
/// bounds, and a leaf cap would silently truncate an import list — presenting a
/// capped sample as the file's complete set of imports, which is exactly the
/// shape that makes "nothing imports this" mean two different things.
const RUST_USE_MAX_DEPTH: usize = 32;

/// The path segments of a `use` path node, appended to `out`.
///
/// Returns `false` for a node shape this does not recognise, and the caller
/// abandons the leaf rather than recording a partial path — half a module path
/// resolves to a *different* module, which is worse than not resolving.
fn rust_path_segments(node: Node, source: &str, depth: usize, out: &mut Vec<String>) -> bool {
    if depth > RUST_USE_MAX_DEPTH {
        return false;
    }
    match node.kind() {
        "scoped_identifier" | "scoped_type_identifier" => {
            if let Some(path) = node.child_by_field_name("path") {
                if !rust_path_segments(path, source, depth + 1, out) {
                    return false;
                }
            }
            match node.child_by_field_name("name") {
                Some(name) => rust_path_segments(name, source, depth + 1, out),
                None => false,
            }
        }
        "identifier" | "type_identifier" | "primitive_type" | "super" | "crate" | "self"
        | "metavariable" => {
            let text = get_node_text(node, source);
            if text.is_empty() {
                return false;
            }
            out.push(text);
            true
        }
        _ => false,
    }
}

/// Flatten a `use` tree into one leaf per name it binds.
fn collect_rust_use_leaves(
    node: Node,
    source: &str,
    prefix: &[String],
    depth: usize,
    out: &mut Vec<RustUseLeaf>,
) {
    if depth > RUST_USE_MAX_DEPTH {
        return;
    }
    match node.kind() {
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_rust_use_leaves(child, source, prefix, depth + 1, out);
            }
        }
        "scoped_use_list" => {
            let mut nested = prefix.to_vec();
            if let Some(path) = node.child_by_field_name("path") {
                if !rust_path_segments(path, source, 0, &mut nested) {
                    return;
                }
            }
            if let Some(list) = node.child_by_field_name("list") {
                collect_rust_use_leaves(list, source, &nested, depth + 1, out);
            }
        }
        "use_as_clause" => {
            let mut segments = prefix.to_vec();
            let Some(path) = node.child_by_field_name("path") else {
                return;
            };
            if !rust_path_segments(path, source, 0, &mut segments) {
                return;
            }
            let alias = node
                .child_by_field_name("alias")
                .map(|child| get_node_text(child, source))
                .filter(|alias| !alias.is_empty());
            if let Some(name) = segments.pop() {
                out.push(RustUseLeaf {
                    module: segments,
                    name: Some(name),
                    alias,
                });
            }
        }
        "use_wildcard" => {
            let mut segments = prefix.to_vec();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if !rust_path_segments(child, source, 0, &mut segments) {
                    return;
                }
            }
            out.push(RustUseLeaf {
                module: segments,
                name: None,
                alias: None,
            });
        }
        _ => {
            let mut segments = prefix.to_vec();
            if !rust_path_segments(node, source, 0, &mut segments) {
                return;
            }
            if let Some(name) = segments.pop() {
                out.push(RustUseLeaf {
                    module: segments,
                    name: Some(name),
                    alias: None,
                });
            }
        }
    }
}

/// How many **inline** `mod { … }` blocks enclose this node.
///
/// Rust's `super` is relative to the module, not to the file, and an inline
/// module is one module deeper without being one file deeper. `mod tests { use
/// super::*; }` therefore names the *file it is written in*; the same statement
/// at file level names the parent directory's module. This repository contains
/// 87 of the first spelling and none of the resolver's rungs could tell them
/// apart.
fn rust_inline_module_depth(node: Node) -> usize {
    let mut depth = 0usize;
    let mut current = bounded_parent(node);
    while let Some(parent) = current {
        if parent.kind() == "mod_item" && parent.child_by_field_name("body").is_some() {
            depth += 1;
        }
        current = bounded_parent(parent);
    }
    depth
}

/// The module specifier a leaf's path denotes, with `super` resolved against
/// the inline-module nesting it was written inside.
///
/// `super` spent against an inline module does not leave the file, so once the
/// nesting is used up the target is this file — spelled `self`, which the
/// resolver reads as "the file this import is written in".
///
/// Known limit, stated rather than papered over: where the nesting absorbs every
/// `super` and segments remain (`mod tests { use super::helpers::thing; }`),
/// the remainder is emitted as `self::helpers`, which resolves to a sibling
/// *file* module. That is right when `helpers` is `mod helpers;` and wrong when
/// it is `mod helpers { … }` in this same file — and telling those apart needs
/// the file's own inline-module table, which this function does not have. The
/// wrong case resolves to nothing, which is where it already sat.
fn rust_use_specifier(module: &[String], inline_depth: usize) -> String {
    let leading_super = module
        .iter()
        .take_while(|segment| *segment == "super")
        .count();
    let spent = leading_super.min(inline_depth);
    let remaining = leading_super - spent;
    let mut segments: Vec<&str> = Vec::with_capacity(module.len());
    if leading_super > 0 && remaining == 0 {
        segments.push("self");
    } else {
        segments.extend(std::iter::repeat_n("super", remaining));
    }
    segments.extend(module[leading_super..].iter().map(String::as_str));
    segments.join("::")
}

/// Read a `use_declaration` into one [`ExtractedImport`] per module it names.
///
/// The arm this replaces stored the statement's own text as the module
/// specifier: `use tree_sitter::{Language, Node, Parser};` became one import
/// whose module was the literal string `"tree_sitter::{Language, Node,
/// Parser}"`, with an empty `imported_names`. Both fields are what every
/// consumer of an import reads, so nothing downstream worked at all — measured
/// on this repository, no `.rs` file produced a single `Imports` edge and
/// `UnresolvedClass::External` never fired once for Rust, while the same ladder
/// produced 8,734 External rows for Python from the same evidence shape.
///
/// A glob is emitted as the `.` alias rather than as a name. That spelling
/// already exists for Go's dot-import and means exactly this — bind everything
/// this module exports — so the resolver needs no second rung for it.
fn rust_use_imports(
    node: Node,
    source: &str,
    span: Span,
    imports: &mut Vec<ExtractedImport>,
) -> Option<()> {
    let raw = get_node_text(node, source);
    let argument = node.child_by_field_name("argument")?;
    let inline_depth = rust_inline_module_depth(node);
    let mut leaves: Vec<RustUseLeaf> = Vec::new();
    collect_rust_use_leaves(argument, source, &[], 0, &mut leaves);

    // Grouped by module so `use std::{fmt, io}` is one import of `std` naming
    // two symbols, the shape every other language's extractor produces. Ordered
    // by module for determinism (R4); names keep source order within a module.
    let mut named: BTreeMap<String, (Vec<String>, Vec<String>)> = BTreeMap::new();
    let mut whole_module: BTreeMap<String, Option<String>> = BTreeMap::new();
    for leaf in leaves {
        // `use serde;` and `use super::{self, thing};` import the module
        // itself, not a name out of it.
        let names_the_module = leaf.module.is_empty() || leaf.name.as_deref() == Some("self");
        if names_the_module {
            let mut module = leaf.module.clone();
            if leaf.name.as_deref() != Some("self") {
                if let Some(name) = leaf.name.clone() {
                    module.push(name);
                }
            }
            if module.is_empty() {
                continue;
            }
            whole_module.insert(rust_use_specifier(&module, inline_depth), leaf.alias);
            continue;
        }
        let specifier = rust_use_specifier(&leaf.module, inline_depth);
        match leaf.name {
            // A glob binds the module's whole surface, which is what the
            // resolver's `.` alias means.
            None => {
                whole_module.insert(specifier, Some(".".to_string()));
            }
            Some(name) => {
                let local = leaf.alias.unwrap_or_else(|| name.clone());
                let entry = named.entry(specifier).or_default();
                entry.0.push(name);
                entry.1.push(local);
            }
        }
    }

    for (specifier, (imported_names, local_names)) in named {
        imports.push(ExtractedImport {
            raw_import: raw.clone(),
            module_specifier: specifier,
            imported_names,
            local_names,
            alias: None,
            span: span.clone(),
        });
    }
    for (specifier, alias) in whole_module {
        imports.push(ExtractedImport {
            raw_import: raw.clone(),
            module_specifier: specifier,
            imported_names: vec![],
            local_names: vec![],
            alias,
            span: span.clone(),
        });
    }
    Some(())
}

/// Parameter name → declared type, for languages whose parameters carry one.
///
/// A parameter's declared type binds its name to that type just as firmly as a
/// constructor assignment does, and it is the *only* binding available to a
/// function operating on a value it did not construct. Without it,
/// `fn run(g: &English) { g.greet() }` produced no call edge at all: receiver
/// resolution had nothing to look up, so the call vanished from the graph
/// rather than merely being uncertain (SC12).
/// Parameter bindings as `(name, bare type, qualifying module)`.
///
/// The qualifier is carried separately rather than folded into the type name,
/// because the two answer different questions. The bare name is the dispatch
/// key — it is what `symbol_index` is keyed by, so `pkg.Widget` must still look
/// up as `Widget`. The qualifier is the *provenance*: `*testing.T` names a type
/// no indexed file declares, and `testing` is the import that proves it (SC25).
fn param_type_bindings(
    node: Node,
    source: &str,
    lang: &str,
) -> Vec<(String, String, Option<String>)> {
    let Some(params) = node.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = params.walk();
    for child in params.named_children(&mut cursor) {
        let binding = match (lang, child.kind()) {
            ("rust", "parameter") => {
                let name = child
                    .child_by_field_name("pattern")
                    .map(|pattern| get_node_text(pattern, source));
                let ty_node = child.child_by_field_name("type");
                let ty = ty_node.and_then(|ty| rust_type_name(ty, source, 0));
                let qualifier = ty_node.and_then(|ty| rust_type_qualifier(ty, source, 0));
                name.zip(ty).map(|(name, ty)| (name, ty, qualifier))
            }
            ("go", "parameter_declaration") => {
                let name = get_child_text(child, "name", source);
                let ty_node = child.child_by_field_name("type");
                let ty = ty_node.and_then(|ty| go_type_name(ty, source, 0));
                let qualifier = ty_node.and_then(|ty| go_type_qualifier(ty, source, 0));
                name.zip(ty).map(|(name, ty)| (name, ty, qualifier))
            }
            _ => None,
        };
        if let Some((name, ty, qualifier)) = binding {
            if !name.is_empty() && !ty.is_empty() && name != "_" {
                out.push((name, ty, qualifier));
            }
        }
    }
    out
}

/// The package a Go type is qualified by, unwrapping the type constructors
/// around it: `[]*testing.T` is qualified by `testing`.
///
/// Separate from `go_type_name` for the SC17 reason — that function answers
/// "what type is this value, for dispatch", and must keep returning the bare
/// name. Bounded depth so a pathological nesting cannot recurse without end.
fn go_type_qualifier(node: Node, source: &str, depth: usize) -> Option<String> {
    if depth > 16 {
        return None;
    }
    match node.kind() {
        "qualified_type" => get_child_text(node, "package", source).filter(|p| !p.is_empty()),
        "pointer_type" => node
            .named_child(0)
            .and_then(|inner| go_type_qualifier(inner, source, depth + 1)),
        "slice_type" | "array_type" => node
            .child_by_field_name("element")
            .and_then(|inner| go_type_qualifier(inner, source, depth + 1)),
        "map_type" => node
            .child_by_field_name("value")
            .and_then(|inner| go_type_qualifier(inner, source, depth + 1)),
        "generic_type" => node
            .child_by_field_name("type")
            .and_then(|inner| go_type_qualifier(inner, source, depth + 1)),
        _ => None,
    }
}

/// The crate or module root a Rust type path is rooted at: `&reqwest::Client`
/// is qualified by `reqwest`.
///
/// Takes the *first* segment because that is what a `use` binds and therefore
/// what an import can be matched against. `crate::`, `self::` and `super::`
/// roots are returned as written and simply never match an external import,
/// which is the correct outcome for an in-corpus path.
fn rust_type_qualifier(node: Node, source: &str, depth: usize) -> Option<String> {
    if depth > 16 {
        return None;
    }
    match node.kind() {
        "reference_type" | "pointer_type" | "generic_type" => node
            .child_by_field_name("type")
            .and_then(|inner| rust_type_qualifier(inner, source, depth + 1)),
        "scoped_type_identifier" => {
            let text = get_node_text(node, source);
            let root = text.split("::").next().unwrap_or_default().trim();
            (!root.is_empty()).then(|| root.to_string())
        }
        _ => None,
    }
}

/// The bare name of a Go type, unwrapping the pointers around it.
///
/// Bounded depth for the same reason as `go_type_qualifier` above, and measured
/// rather than assumed: `func (r **…*T) M() {}` at 10,000 levels — a ~10 KB
/// file, four orders of magnitude under `MAX_SOURCE_BYTES` — overflowed the
/// stack and aborted `devmap build` with exit 134. The tree walk itself is an
/// explicit worklist and never recurses, so this function and `rust_type_name`
/// were the whole of that overflow. Past the bound the receiver is not
/// recovered and the method is named `file::M` rather than `file::T.M` — a lost
/// qualification, never a guessed one.
fn go_type_name(node: Node, source: &str, depth: usize) -> Option<String> {
    if depth > 16 {
        return None;
    }
    match node.kind() {
        "pointer_type" => {
            let mut pointer_cursor = node.walk();
            for child in node.named_children(&mut pointer_cursor) {
                if let Some(name) = go_type_name(child, source, depth + 1) {
                    return Some(name);
                }
            }
            None
        }
        "type_identifier" | "identifier" | "field_identifier" => {
            let text = get_node_text(node, source);
            (!text.is_empty()).then_some(text)
        }
        "qualified_type" => get_child_text(node, "name", source).or_else(|| {
            let text = get_node_text(node, source);
            text.rsplit('.').next().map(str::to_string)
        }),
        _ => None,
    }
}

fn go_package_name(root: Node, source: &str) -> Option<String> {
    let mut package_cursor = root.walk();
    let mut identifier_cursor = root.walk();
    for child in root.named_children(&mut package_cursor) {
        if child.kind() != "package_clause" {
            continue;
        }
        let name = get_child_text(child, "name", source).or_else(|| {
            child
                .named_children(&mut identifier_cursor)
                .find(|node| node.kind() == "package_identifier")
                .map(|node| get_node_text(node, source))
        })?;
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

/// Every `GOOS` value Go recognises, from `go/build/syslist.go`.
///
/// A filename ending `_<GOOS>.go` carries an implicit build constraint even
/// with no `//go:build` line, so the list is part of the constraint test rather
/// than decoration. Held as a sorted constant so membership is a fact about the
/// toolchain rather than a guess about what an OS name looks like — a
/// name-shaped heuristic would read `_other.go` and `_test.go` as constraints.
const GO_OS_VALUES: &[&str] = &[
    "aix",
    "android",
    "darwin",
    "dragonfly",
    "freebsd",
    "hurd",
    "illumos",
    "ios",
    "js",
    "linux",
    "nacl",
    "netbsd",
    "openbsd",
    "plan9",
    "solaris",
    "wasip1",
    "windows",
    "zos",
];

/// Every `GOARCH` value Go recognises, from `go/build/syslist.go`.
const GO_ARCH_VALUES: &[&str] = &[
    "386",
    "amd64",
    "amd64p32",
    "arm",
    "arm64",
    "arm64be",
    "armbe",
    "loong64",
    "mips",
    "mips64",
    "mips64le",
    "mips64p32",
    "mips64p32le",
    "mipsle",
    "ppc",
    "ppc64",
    "ppc64le",
    "riscv",
    "riscv64",
    "s390",
    "s390x",
    "sparc",
    "sparc64",
    "wasm",
];

/// Whether a Go file is excluded from some builds — by a `//go:build` or
/// `// +build` directive, or by an implicit `_GOOS`/`_GOARCH` filename suffix.
///
/// Both halves are needed and neither subsumes the other. `procgroup_other.go`
/// has no recognisable suffix and is constrained only by `//go:build !unix`;
/// a `foo_windows.go` with no directive is constrained only by its name.
///
/// The directive scan stops at the package clause because that is where Go
/// stops looking: a `//go:build` line below it is an ordinary comment, and
/// treating it as a constraint would let a comment anywhere in a file suppress
/// a real finding.
fn go_build_constrained(path: &str, source: &str) -> bool {
    go_has_build_directive(source) || go_filename_is_constrained(path)
}

fn go_has_build_directive(source: &str) -> bool {
    for line in source.lines() {
        let trimmed = line.trim();
        // The header ends at the package clause; nothing below it constrains
        // the build.
        if trimmed.starts_with("package ") || trimmed == "package" {
            return false;
        }
        if trimmed.starts_with("//go:build") {
            return true;
        }
        // The legacy form is `// +build`, with the space required.
        if let Some(rest) = trimmed.strip_prefix("//") {
            if rest.trim_start().starts_with("+build") {
                return true;
            }
        }
    }
    false
}

/// Whether the `_GOOS` / `_GOARCH` / `_GOOS_GOARCH` filename suffix applies.
///
/// `_test` is stripped first: Go reads `foo_linux_test.go` as the linux-only
/// test file for `foo`, so the constraint sits one component further left.
fn go_filename_is_constrained(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    let Some(stem) = file.strip_suffix(".go") else {
        return false;
    };
    let stem = stem.strip_suffix("_test").unwrap_or(stem);
    let mut parts = stem.split('_').rev();
    let Some(last) = parts.next() else {
        return false;
    };
    // Go only honours the suffix when something precedes it: a file *named*
    // `linux.go` is not constrained, `net_linux.go` is.
    let has_prefix = parts.clone().next().is_some();
    if !has_prefix {
        return false;
    }
    if GO_OS_VALUES.contains(&last) {
        return true;
    }
    if GO_ARCH_VALUES.contains(&last) {
        // `_GOARCH` alone constrains; `_GOOS_GOARCH` does too, and the OS half
        // is only meaningful when it really is an OS.
        return true;
    }
    false
}

fn maybe_push_name_reference(
    node: Node,
    source: &str,
    lang: &str,
    file_symbol_name: &str,
    references: &mut Vec<ExtractedReference>,
) {
    let ref_kind = match node.kind() {
        "type_identifier" | "nested_type_identifier" => ReferenceKind::Type,
        "identifier"
        | "shorthand_property_identifier"
        | "property_identifier"
        | "field_identifier"
        // tree-sitter-swift spells every value-position identifier
        // `simple_identifier`, so without this arm Swift recorded no name
        // references at all — only type positions, which are
        // `type_identifier`. A Swift type or function used as a *value* was
        // therefore invisible: `Const.firebaseTimeoutMs`,
        // `self[SlideUpDismissKey.self]`, `fields.map(csvField)` and
        // `AXObserverCreate(pid, selectionObserverCallback, …)` all name a
        // symbol the graph had no edge for, and each of those symbols was
        // reported dead at 0.9 confidence on real code once Swift visibility
        // became readable. The existing gates still apply: a declaration site
        // (`is_defining_name`), a callee position (`is_call_callee`) and a name
        // shadowed by a local binding are all still refused.
        | "simple_identifier" => ReferenceKind::Name,
        _ => return,
    };
    if is_defining_name(node) || is_inside_import_or_export(node) || is_call_callee(node) {
        return;
    }
    let name = get_node_text(node, source);
    if !is_user_ident(&name) {
        return;
    }
    // X40. `_` in **type position** is the inferred-type placeholder — Rust's
    // `row.get::<_, f64>(1)`, Go's blank identifier — and it references
    // nothing, so there is no attribution to attempt and no honest tier to file
    // the failure under. Measured on this repository: 275 of the 9,790 rows in
    // the tier documented as "the only tier that indicates a defect" were this
    // placeholder, every one of them a turbofish.
    //
    // Restricted to type position on purpose. `_` is a perfectly ordinary
    // value-position identifier in JavaScript (lodash) and Python (gettext), so
    // refusing it everywhere would drop real references; no language names a
    // *type* `_`.
    if ref_kind == ReferenceKind::Type && name.chars().all(|character| character == '_') {
        return;
    }
    if ref_kind == ReferenceKind::Name && name_is_shadowed_by_local(node, source, &name) {
        return;
    }
    references.push(ExtractedReference {
        name,
        kind: ref_kind,
        span: node_span(node),
        enclosing_symbol: enclosing_emitted_symbol_for(node, source, lang, file_symbol_name),
        assigned_to: None,
        // The object half of a member access, so the resolver can tell
        // `cfg.enabled` from a bare local named `enabled`.
        receiver_expr: member_access_receiver(node, source),
    });
}

/// Grammar keys with a language-specific arm in `extract_node`.
///
/// Everything else reaches the generic arm, whose declaration identity comes
/// from `langdecl` — so a reference made inside one of those languages must be
/// attributed through `langdecl` too, or it names a symbol the emitter never
/// produced. Measured: with Kotlin extension functions owned by their receiver,
/// `enclosing_callable_qualified` still answered `Main.kt::extra` for a
/// reference inside `fun Person.extra()`, where the emitted symbol is
/// `Main.kt::Person.extra` — an orphaned edge of exactly the SC9/SC10 shape.
///
/// Not trusted: `every_language_attributes_its_references_to_an_emitted_symbol`
/// in `tests/declarations.rs` runs a declaration-plus-use snippet through every
/// language named here **and** through the languages served by the generic arm,
/// and fails if any reference names a symbol the emitter did not produce.
/// Demonstrated in both directions rather than asserted: adding `"kotlin"` here
/// makes a reference inside `fun Person.extra()` report `a.kt::extra`, and
/// removing `"rust"` makes a reference inside an `impl` block report
/// `a.rs::outer` where the emitter said `a.rs::S.outer`. Both are orphans.
const SPECIALISED_ARM_LANGUAGES: &[&str] = &[
    "go",
    "hcl",
    "javascript",
    "python",
    "rust",
    "tsx",
    "typescript",
];

/// The enclosing symbol a reference belongs to, asked of whichever path emitted
/// the enclosing declaration.
fn enclosing_emitted_symbol_for(
    node: Node,
    source: &str,
    lang: &str,
    file_symbol_name: &str,
) -> Option<String> {
    if SPECIALISED_ARM_LANGUAGES.contains(&lang) {
        return enclosing_callable_qualified(node, source, file_symbol_name);
    }
    crate::langcalls::scope::enclosing_emitted_symbol(node, source, lang, file_symbol_name)
}

/// Whether `node` is the name an R assignment binds.
///
/// `<-`, `<<-` and `=` bind their right operand to their left; `->` and `->>`
/// bind the other way. Every other `binary_operator` — arithmetic, comparison,
/// a pipe — has operands that are uses, and this must not suppress them.
fn is_r_binding_target(node: Node) -> bool {
    let Some(parent) = bounded_parent(node) else {
        return false;
    };
    if parent.kind() != "binary_operator" {
        return false;
    }
    let Some(operator) = parent.child_by_field_name("operator") else {
        return false;
    };
    let operator = operator.kind();
    match operator {
        "<-" | "<<-" | "=" => field_contains(parent, "lhs", node),
        "->" | "->>" => field_contains(parent, "rhs", node),
        _ => false,
    }
}

fn is_user_ident(name: &str) -> bool {
    let Some(first) = name.chars().next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_' || first == '$')
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
}

/// Whether `needle` lies inside the first child of `parent` on `field`.
///
/// Deliberately the *first*, not all of them. Several grammars put more than one
/// child on `name`, and they do not agree on what the extra ones are:
/// tree-sitter-dart's `constructor_signature` puts the class, a `.` and the
/// constructor name there, all of them declaration sites, while
/// tree-sitter-swift's `parameter` puts the parameter name **and its type**
/// there. Widening this to every `name` child was measured and reverted: it
/// suppressed 796 real type references across 342 `.swift` files, because every
/// parameter type stopped counting as a use. The grammars that genuinely need
/// the wider rule get a named clause in `is_defining_name` instead.
fn field_contains(parent: Node, field: &str, needle: Node) -> bool {
    parent
        .child_by_field_name(field)
        .is_some_and(|haystack| node_contains(haystack, needle))
}

fn node_contains(haystack: Node, needle: Node) -> bool {
    if haystack.id() == needle.id() {
        return true;
    }
    let mut cursor = haystack.walk();
    for child in haystack.children(&mut cursor) {
        if node_contains(child, needle) {
            return true;
        }
    }
    false
}

/// The declaration a C-family `declarator` chain binds, when `node` is the
/// identifier it names.
///
/// C, C++, Objective-C, CUDA and Metal do not put a `name` field on a
/// declaration: the name hangs off a `declarator` chain
/// (`function_definition -> function_declarator -> identifier`). The
/// `name`-field tests in `is_defining_name` therefore never recognised a
/// C-family declaration's own identifier, so every declaration emitted a
/// `Name` reference to itself, `analyze_liveness` counted that self-reference
/// as a use, and no C-family symbol could ever be reported dead.
///
/// The walk follows `declarator` *identity* links, never `field_contains`
/// subtree containment: `function_declarator` holds the signature under
/// `parameters` and `array_declarator` holds its bound under `size`, so a
/// containment test would also swallow parameter types and array bounds, which
/// are genuine references. Returns the outermost node of the chain — the
/// declaration itself — so callers can tell a function definition from a
/// variable or parameter binding.
fn c_declarator_declaration(node: Node) -> Option<Node> {
    let mut current = node;
    let mut climbed = false;
    while let Some(parent) = bounded_parent(current) {
        if parent
            .child_by_field_name("declarator")
            .is_none_or(|declarator| declarator.id() != current.id())
        {
            break;
        }
        climbed = true;
        current = parent;
    }
    climbed.then_some(current)
}

fn is_defining_name(node: Node) -> bool {
    // A grammar's `name` field is not necessarily a declaration. Java/Lua
    // invocation nodes use it for the callee; recording that use as a local
    // binding suppresses the very call it names (RA1).
    if bounded_parent(node).is_some_and(|parent| {
        matches!(
            parent.kind(),
            "method_invocation"
                | "function_call"
                | "call"
                | "call_expression"
                | "invocation_expression"
        )
    }) {
        return false;
    }
    if c_declarator_declaration(node).is_some() || is_objc_declaring_identifier(node) {
        return true;
    }
    // `struct render_state *state` names the type in the same `name` field that
    // `struct render_state { … }` uses, so the generic test below calls both a
    // definition and suppresses the reference. Only the form with a body
    // defines; the other is a use, and it is frequently the *only* use, because
    // C code that never typedefs a struct mentions it exclusively this way.
    //
    // The mirror of the rule in `c_family_declaration`, which is why they are
    // stated in the same terms: a bodyless specifier neither declares a symbol
    // nor suppresses a reference.
    if is_bodyless_type_specifier_name(node) {
        return false;
    }
    // A Kotlin `enum_entry` names its constant in an unlabelled `identifier`
    // child, so the `name`-field test below cannot see it. Once enum entries
    // became symbols, the declaration site was being recorded as a *use* of the
    // symbol it declares — `Main.kt::Mode.FAST` referencing `Main.kt::Mode.FAST`
    // — which is the self-reference shape
    // `c_family_declarations_do_not_reference_themselves` already pins for C.
    if bounded_parent(node).is_some_and(|parent| parent.kind() == "enum_entry") {
        return true;
    }
    // R spells every function declaration as an assignment — `helper <-
    // function(a) …` — so the name being declared sits on `lhs` of a
    // `binary_operator`, a node kind that also covers `a + 1`. Gated on the
    // operator actually binding, so an arithmetic operand is never suppressed.
    // Without it a four-function R file recorded four file-scoped references to
    // its own declarations, and every uncalled R function looked used.
    if is_r_binding_target(node) {
        return true;
    }
    // tree-sitter-dart spells a named constructor `Widget.named` as three
    // children on the `name` field — `Widget`, `.`, `named` — so the generic
    // first-child test below sees only `Widget` and reported the constructor's
    // own name as a *use* of the symbol it declares:
    // `app.dart::Widget.named` referencing `app.dart::Widget.named`, the
    // self-reference shape `c_family_declarations_do_not_reference_themselves`
    // pins for C. A parameter's identifiers hang off `formal_parameter_list`
    // rather than off the signature, so this reaches only the name.
    if bounded_parent(node).is_some_and(|parent| parent.kind() == "constructor_signature") {
        return true;
    }
    let mut current = node;
    loop {
        let Some(parent) = bounded_parent(current) else {
            return false;
        };
        if field_contains(parent, "name", node)
            || field_contains(parent, "alias", node)
            || field_contains(parent, "parameter", node)
            || field_contains(parent, "pattern", node)
        {
            return true;
        }
        if matches!(
            parent.kind(),
            "assignment"
                | "assignment_expression"
                | "assignment_statement"
                | "augmented_assignment"
                | "augmented_assignment_expression"
                | "short_var_declaration"
                | "range_clause"
                | "for_statement"
                | "for_in_statement"
                | "for_of_statement"
                | "for_in_clause"
                | "for_of_clause"
                | "var_spec"
                | "const_spec"
        ) && field_contains(parent, "left", node)
        {
            return true;
        }
        // A Rust closure parameter is a binding site. `closure_parameters`
        // has no fields, so neither the `pattern` test above nor the wrapper
        // climb reaches an untyped `|handler|`; `|handler: H|` was recognised
        // only because the `parameter` node in between does carry `pattern`.
        // Half a rule is worse than none: the typed form counted as a local and
        // the untyped form did not.
        //
        // This one belongs here rather than in `collect_parameter_names`, which
        // only sees a callable's own signature: a closure is not an
        // `is_callable_node`, so `collect_non_symbol_locals` walks straight
        // through it and its parameters are the enclosing function's locals —
        // which is also where its calls are attributed. Unlike a pytest fixture
        // parameter, a closure parameter refers to nothing, so suppressing the
        // `Name` reference it used to emit loses no signal.
        if parent.kind() == "closure_parameters" {
            return true;
        }
        if parent.kind() == "pair" {
            if let Some(key) = parent.child_by_field_name("key") {
                if key.id() == node.id() {
                    return true;
                }
            }
        }
        if is_binding_wrapper(parent.kind()) {
            current = parent;
            continue;
        }
        return false;
    }
}

/// Nodes that wrap a binding without being one, so the defining-name walk
/// climbs through them.
///
/// The `_target` suffix is load-bearing and easy to mistake for dead breadth:
/// Python's `with … as handle` puts `handle` in an `as_pattern_target`, which
/// appears in `node-types.json` only as a *field type* of `as_pattern` and not
/// as a top-level entry, so a scan of top-level node kinds does not find it.
/// Without the clause the walk stops there, `handle` stops being a binding
/// site, and both it and every later mention become module-level uses.
fn is_binding_wrapper(kind: &str) -> bool {
    matches!(
        kind,
        "expression_list"
            | "identifier_list"
            | "pattern_list"
            | "parenthesized_expression"
            | "spread_element"
            | "list_splat_pattern"
            | "pattern"
    ) || kind.ends_with("_pattern")
        || kind.ends_with("_target")
}

thread_local! {
    static SCOPE_LOCALS: RefCell<HashMap<usize, HashSet<String>>> =
        RefCell::new(HashMap::new());
    /// The deadline for the current file's extraction, readable by helpers that
    /// the walk calls but does not pass arguments to.
    static WALK_DEADLINE: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
    /// Set once a helper has stopped early because that deadline passed.
    static WALK_OVERRAN: Cell<bool> = const { Cell::new(false) };
    /// Ancestor steps taken since the clock was last read. See `bounded_parent`.
    static PARENT_STEPS: Cell<u32> = const { Cell::new(0) };
    #[cfg(test)]
    static NATIVE_PARENT_READS: Cell<usize> = const { Cell::new(0) };
}

/// Arms the extraction deadline for exactly as long as one file is being
/// extracted, and disarms it on the way out of *every* path — including the
/// early returns that refuse a file.
///
/// The deadline used to be armed inside `walk_tree` and never cleared, so it
/// outlived the walk it belonged to: every phase that runs afterwards
/// (`go_method_sets`, the Python passes, `clonesig`) saw a live deadline
/// belonging to the *previous* file, and the next file's pre-walk phases saw
/// one that had already expired. Nothing consulted it there yet, which is the
/// only reason it was invisible; extending the bound past the walk — the point
/// of this type — makes that stale value load-bearing. Tying arm and disarm to
/// a scope means the invariant cannot be broken by adding a `return`.
struct BudgetGuard;

impl BudgetGuard {
    fn arm(deadline: std::time::Instant) -> Self {
        reset_scope_locals();
        WALK_DEADLINE.with(|slot| slot.set(Some(deadline)));
        WALK_OVERRAN.with(|slot| slot.set(false));
        PARENT_STEPS.with(|slot| slot.set(0));
        BudgetGuard
    }
}

impl Drop for BudgetGuard {
    fn drop(&mut self) {
        WALK_DEADLINE.with(|slot| slot.set(None));
        // Retain the incomplete-walk signal until the next extraction arms
        // its budget. The publication gate must not lose a helper's refusal.
    }
}

/// `bounded_parent(node)`, bounded by the extraction deadline.
///
/// **Every ancestor walk in this crate must go through this**, because
/// `Node::parent()` is not O(1): tree-sitter reconstructs the parent by
/// descending from the tree root, so one call costs O(depth) and climbing to
/// the root costs **O(depth^2)**. Measured 2026-09-05 (release, macOS) on
/// `func (r *…*T) M() {}`: one `parent()` from the deepest node takes 27.8 us
/// at depth 1,000 and 434 us at depth 16,000, and a full ancestor walk takes
/// 13.1 ms / 52.7 ms / 204.7 ms / 821.7 ms / 3.46 s at depth 1k / 2k / 4k / 8k
/// / 16k — four times the cost for twice the depth, which is the signature.
///
/// That quadratic is why the stride in `walk_tree` was not a bound. The whole
/// 13.15 s debug extraction of a 10 KB file was **two ancestor walks on a
/// single node** — `is_inside_import_or_export` at 1.166 s and
/// `enclosing_callable_qualified` at 1.145 s in release — so the walk's
/// per-node clock check had no opportunity to fire between them, and the file
/// was published `Clean` after running 2.6x past its 5 s budget. A stride is
/// not a bound if one step is unbounded; this is where that step is bounded.
///
/// Returning `None` early reports "no further ancestors", which is a *wrong*
/// answer, and that is safe only because it is never published: the same check
/// latches `WALK_OVERRAN`, and `extract_treesitter_with_budget` turns that latch
/// into a refusal for the whole file. This is the contract
/// `collect_non_symbol_locals` already relies on.
///
/// The clock is read every `PARENT_CHECK_STRIDE` steps rather than every step
/// because `Instant::now()` costs more than an ordinary ancestor hop on the
/// happy path, where files nest tens of levels deep, not thousands.
pub(crate) fn bounded_parent(node: Node<'_>) -> Option<Node<'_>> {
    let steps = PARENT_STEPS.with(|slot| {
        let next = slot.get() + 1;
        slot.set(next);
        next
    });
    if steps >= PARENT_CHECK_STRIDE {
        PARENT_STEPS.with(|slot| slot.set(0));
        if walk_deadline_passed() {
            return None;
        }
    }
    // A walk that has already overrun must not start another climb: the latch
    // is set for the rest of the file, so every remaining ancestor walk stops
    // at its first step instead of paying O(depth) apiece to reach the same
    // refusal.
    if walk_overran() {
        return None;
    }
    crate::parent_index::parent(node).unwrap_or_else(|| {
        #[cfg(test)]
        NATIVE_PARENT_READS.with(|count| count.set(count.get() + 1));
        node.parent()
    })
}

/// Ancestor steps taken between clock reads. See `bounded_parent`.
///
/// Smaller than `DEADLINE_CHECK_STRIDE` because the steps are not comparable: a
/// node visit is O(1) while an ancestor hop is O(depth). At the 1 MiB
/// `MAX_SOURCE_BYTES` ceiling the deepest reachable nesting is around a million
/// levels, where one hop measures ~27 ms, so 64 steps bounds the overshoot at
/// roughly 1.7 s — inside the 5 s `DEFAULT_PARSE_BUDGET` and far inside the 10x
/// tolerance `tests/budget_is_a_real_bound.rs` allows. A stride of 256 would put
/// it at 6.9 s, which is past the budget it exists to enforce.
const PARENT_CHECK_STRIDE: u32 = 64;

/// True once a bounded helper gave up because the walk's deadline passed.
///
/// `walk_tree` checks the clock every `DEADLINE_CHECK_STRIDE` nodes, which
/// bounds the *number* of nodes between checks but not the *work* inside any
/// one of them. `collect_non_symbol_locals` walks a whole scope subtree per
/// call, so a single `extract_node` could run for a minute while the stride
/// counter sat at 1 — a stride is not a bound if one step is unbounded. This
/// flag is how a helper that stopped early tells the walk to stop too, and it
/// is a plain `Cell` read so the loop can consult it on every node.
fn walk_overran() -> bool {
    WALK_OVERRAN.with(|slot| slot.get())
}

/// Whether this file's extraction may still publish what it has.
///
/// Two ways to fail, and they are not the same question: a helper may have
/// stopped early and latched `WALK_OVERRAN` — in which case the answers it
/// returned are incomplete even though the clock may since have been reset —
/// or the phase just finished may simply have run past the deadline while
/// checking nothing. Either one means the result is not a complete read of the
/// file, and a check that could not run must never report what a check that ran
/// and passed reports.
fn extraction_overran(deadline: std::time::Instant) -> bool {
    walk_overran() || std::time::Instant::now() >= deadline
}

/// Deadline check for a helper running inside the walk. Latches the flag.
fn walk_deadline_passed() -> bool {
    match WALK_DEADLINE.with(|slot| slot.get()) {
        Some(deadline) if std::time::Instant::now() >= deadline => {
            WALK_OVERRAN.with(|slot| slot.set(true));
            true
        }
        _ => false,
    }
}

/// Drop the per-scope local-name cache between files.
///
/// The cache is keyed by `Node::id()`, which is the node's address in its
/// tree's arena — those addresses are reused once a tree is dropped, so a key
/// from one file can collide with an unrelated scope in the next. Without the
/// clear, that serves one file's local-variable set to another file, silently
/// suppressing genuine references there. Extraction of a file must never depend
/// on what was extracted before it.
fn reset_scope_locals() {
    SCOPE_LOCALS.with(|cache| cache.borrow_mut().clear());
}

/// Number of cached scopes. Test-only: the clear has no other observable effect
/// until an address happens to be reused, which is exactly what makes a missing
/// clear so hard to catch in the wild.
#[cfg(test)]
fn scope_locals_len() -> usize {
    SCOPE_LOCALS.with(|cache| cache.borrow().len())
}

fn is_callable_node(node: Node) -> bool {
    matches!(
        node.kind(),
        "function_definition"
            | "async_function_definition"
            | "function_declaration"
            | "method_definition"
            | "function_item"
            | "method_declaration"
            | "generator_function_declaration"
            | "generator_function"
            | "arrow_function"
            | "function_expression"
            | "lambda"
    )
}

fn enclosing_scope_node(node: Node) -> Node {
    let mut current = node;
    let mut ancestor = bounded_parent(node);
    while let Some(parent) = ancestor {
        if is_callable_node(parent) {
            return parent;
        }
        current = parent;
        ancestor = bounded_parent(parent);
    }
    current
}

fn is_symbol_binding(node: Node) -> bool {
    // A C-family function name binds a symbol, not a local. Without this the
    // `declarator` arm of `is_defining_name` would file every function's own
    // identifier into `collect_non_symbol_locals`, and `name_is_shadowed_by_local`
    // would then drop non-call references to it from inside its own body.
    if c_declarator_declaration(node).is_some_and(|declaration| {
        matches!(declaration.kind(), "function_definition" | "function_item")
    }) {
        return true;
    }
    // Same reasoning for Objective-C, whose class names and selector parts are
    // unnamed children rather than declarator chains.
    if is_objc_declaring_identifier(node) {
        return true;
    }
    let Some(parent) = bounded_parent(node) else {
        return false;
    };
    if matches!(
        parent.kind(),
        "function_definition"
            | "async_function_definition"
            | "function_declaration"
            | "method_definition"
            | "function_item"
            | "method_declaration"
            | "generator_function_declaration"
            | "class_definition"
            | "class_declaration"
            | "abstract_class_declaration"
            | "type_spec"
            | "type_declaration"
            | "struct_item"
            | "enum_item"
            | "trait_item"
            | "enum_declaration"
    ) && field_contains(parent, "name", node)
    {
        return true;
    }
    if parent.kind() == "variable_declarator" && field_contains(parent, "name", node) {
        return parent.child_by_field_name("value").is_some_and(|value| {
            matches!(
                value.kind(),
                "arrow_function" | "function_expression" | "generator_function"
            )
        });
    }
    false
}

fn collect_non_symbol_locals(scope: Node, source: &str) -> HashSet<String> {
    let mut locals = HashSet::new();
    let mut worklist = Vec::new();
    push_children(scope, &mut worklist);
    let mut since_check = 0u32;
    while let Some(node) = worklist.pop() {
        // Same stride idiom as `walk_tree`: this loop is per-scope and a
        // pathological tree makes it the dominant cost of the whole
        // extraction, so it has to be interruptible on its own.
        since_check += 1;
        if since_check >= DEADLINE_CHECK_STRIDE {
            since_check = 0;
            if walk_deadline_passed() {
                break;
            }
        }
        if is_callable_node(node) {
            continue;
        }
        if matches!(
            node.kind(),
            "identifier"
                | "shorthand_property_identifier"
                | "shorthand_property_identifier_pattern"
        ) && is_defining_name(node)
            && !is_symbol_binding(node)
        {
            let name = get_node_text(node, source);
            if is_user_ident(&name) {
                locals.insert(name);
            }
        }
        push_children_reversed(node, &mut worklist);
    }
    locals
}

/// Run `visit` over one scope's local-name set, computing it once per scope.
fn with_scope_locals<R>(scope: Node, source: &str, visit: impl FnOnce(&HashSet<String>) -> R) -> R {
    let id = scope.id();
    let known = SCOPE_LOCALS.with(|cache| cache.borrow().contains_key(&id));
    if !known {
        let locals = collect_non_symbol_locals(scope, source);
        if walk_overran() {
            // A set the deadline cut short is not this scope's local-name set.
            // The file is refused either way, but caching it would serve the
            // wrong answer to every later lookup of the same scope in the
            // meantime, so it is used once and not kept.
            return visit(&locals);
        }
        SCOPE_LOCALS.with(|cache| cache.borrow_mut().insert(id, locals));
    }
    SCOPE_LOCALS.with(|cache| {
        let borrowed = cache.borrow();
        match borrowed.get(&id) {
            Some(locals) => visit(locals),
            // Unreachable: inserted just above when absent, and nothing removes
            // an entry within a file. Answering "no locals" rather than
            // panicking keeps a cache defect from failing extraction.
            None => visit(&HashSet::new()),
        }
    })
}

/// Module fixtures with an explicit import from pytest. A bare decorator of
/// the same spelling, without that import, cannot establish framework binding.
fn python_fixture_names(root: Node, source: &str, imports: &[ExtractedImport]) -> BTreeSet<String> {
    if root.kind() != "module" {
        return BTreeSet::new();
    }
    let mut decorators = BTreeSet::new();
    for import in imports
        .iter()
        .filter(|import| import.module_specifier == "pytest")
    {
        if import.imported_names.is_empty() {
            decorators.insert(format!(
                "{}.fixture",
                import.alias.as_deref().unwrap_or("pytest")
            ));
        }
        for (index, name) in import
            .imported_names
            .iter()
            .enumerate()
            .filter(|(_, name)| *name == "fixture")
        {
            decorators.insert(
                import
                    .local_names
                    .get(index)
                    .or(import.alias.as_ref())
                    .unwrap_or(name)
                    .clone(),
            );
        }
    }
    if decorators.is_empty() {
        return BTreeSet::new();
    }
    let mut fixtures = BTreeSet::new();
    let mut cursor = root.walk();
    for decorated in root
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "decorated_definition")
    {
        if walk_deadline_passed() {
            break;
        }
        let Some(definition) = decorated
            .child_by_field_name("definition")
            .filter(|node| node.kind() == "function_definition")
        else {
            continue;
        };
        let mut cursor = decorated.walk();
        let registered = decorated
            .named_children(&mut cursor)
            .filter(|node| node.kind() == "decorator")
            .any(|node| {
                let text = get_node_text(node, source);
                let name = text
                    .trim()
                    .trim_start_matches('@')
                    .split('(')
                    .next()
                    .unwrap_or("")
                    .trim();
                decorators.contains(name)
            });
        if registered {
            if let Some(name) = definition.child_by_field_name("name") {
                fixtures.insert(get_node_text(name, source));
            }
        }
    }
    fixtures
}

/// Establish binding facts while the syntax tree still exists. A named graph
/// caller may own many anonymous scopes; it cannot stand in for lexical scope.
fn collect_site_bindings(
    root: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &[ExtractedCall],
    references: &[ExtractedReference],
    imports: &[ExtractedImport],
) -> Vec<LocalBinding> {
    let fixtures = python_fixture_names(root, source, imports);
    let mut sites = BTreeSet::new();
    let mut parameters: HashMap<usize, BTreeSet<String>> = HashMap::new();
    let inputs = calls
        .iter()
        .map(|call| {
            (
                &call.span,
                call.receiver_expr.as_deref().unwrap_or(&call.callee_name),
                false,
            )
        })
        .chain(
            references
                .iter()
                .filter(|r| {
                    matches!(
                        r.kind,
                        ReferenceKind::Name | ReferenceKind::Call | ReferenceKind::Constructor
                    )
                })
                .map(|r| {
                    (
                        &r.span,
                        r.receiver_expr.as_deref().unwrap_or(&r.name),
                        r.kind == ReferenceKind::Name,
                    )
                }),
        );
    for (span, name, is_name_reference) in inputs {
        if walk_deadline_passed() {
            break;
        }
        let Some(node) = root.descendant_for_byte_range(span.start_byte, span.end_byte) else {
            continue;
        };
        let mut ancestor = bounded_parent(node);
        while let Some(scope) = ancestor {
            if is_callable_node(scope) {
                // Python defaults run in the enclosing scope. The parameter's
                // own declaration is still a binding site, but its value is not.
                let in_default = scope.kind() == "function_definition"
                    && field_contains(scope, "parameters", node)
                    && (node.kind() == "call" || {
                        let mut cursor = bounded_parent(node);
                        let mut value = false;
                        while let Some(parent) = cursor {
                            if parent.id() == scope.id() {
                                break;
                            }
                            if field_contains(parent, "value", node)
                                || field_contains(parent, "type", node)
                            {
                                value = true;
                                break;
                            }
                            cursor = bounded_parent(parent);
                        }
                        value
                    });
                if !in_default {
                    let names = parameters.entry(scope.id()).or_insert_with(|| {
                        let mut names = BTreeSet::new();
                        collect_parameter_names(scope, source, &mut names);
                        // A JavaScript arrow can have a single unparenthesized parameter.
                        if let Some(parameter) = scope.child_by_field_name("parameter") {
                            if parameter.kind() == "identifier" {
                                names.insert(get_node_text(parameter, source));
                            }
                        }
                        names
                    });
                    if names.contains(name)
                        || with_scope_locals(scope, source, |locals| locals.contains(name))
                    {
                        // A pytest signature requests a declared fixture. Preserve
                        // that dependency at the signature only; uses in its body
                        // refer to the injected value and are still local bindings.
                        let fixture_request = is_name_reference
                            && fixtures.contains(name)
                            && scope.kind() == "function_definition"
                            && field_contains(scope, "parameters", node)
                            && scope.child_by_field_name("name").is_some_and(|n| {
                                let consumer = get_node_text(n, source);
                                consumer.starts_with("test_") || fixtures.contains(&consumer)
                            });
                        if !fixture_request {
                            let named_scope = callable_binding_name(scope, source).is_some()
                                || is_c_family_callable(scope);
                            sites.insert(LocalBinding {
                                start_byte: span.start_byte,
                                name: name.to_string(),
                                scope: named_scope
                                    .then(|| {
                                        scope.child(0).and_then(|child| {
                                            enclosing_callable_qualified(
                                                child,
                                                source,
                                                file_symbol_name,
                                            )
                                        })
                                    })
                                    .flatten(),
                            });
                        }
                        break;
                    }
                }
            }
            ancestor = bounded_parent(scope);
        }
    }
    sites.into_iter().collect()
}

fn name_is_shadowed_by_local(node: Node, source: &str, name: &str) -> bool {
    let scope = enclosing_scope_node(node);
    with_scope_locals(scope, source, |locals| locals.contains(name))
}

/// Every value a callable binds itself, keyed by the identity a call made inside
/// it reports as its caller.
///
/// The pairing is what makes this usable. `ExtractedCall::caller_symbol` comes
/// from `enclosing_callable_qualified`, and that walk deliberately *skips*
/// unnamed callbacks — `useEffect(() => { … })` attributes to the enclosing
/// named function rather than dropping to file scope. Naming the scopes here by
/// the callable node instead would file an arrow's parameters under a scope no
/// call ever names, and the resolver's lookup would never hit. So each callable
/// is named by running the same walk from one of its own children, and callables
/// that resolve to the same name — a function and every unnamed closure inside
/// it — merge into one set.
///
/// A callable at file scope resolves to `None` and is skipped: a call there
/// reports the file as its caller, and `scope_declares_local` refuses that key
/// outright rather than matching on a string shape.
///
/// `BTreeMap`/`BTreeSet` rather than the hashed containers `collect_non_symbol_locals`
/// returns, because this is serialized and the determinism gate digests it.
/// Nodes that hold a callable's parameters directly, one per grammar family.
const PARAMETER_LIST_KINDS: [&str; 5] = [
    // Python `def`/`class`, Rust `fn`.
    "parameters",
    // JavaScript, TypeScript.
    "formal_parameters",
    "lambda_parameters",
    "closure_parameters",
    // Go.
    "parameter_list",
];

/// Names a callable's own signature binds.
///
/// Deliberately **not** folded into `collect_non_symbol_locals`, which answers a
/// different question — "does a local here shadow a reference to a symbol" — and
/// whose answers are consumed by reference emission. Merging them looked
/// tempting and was measured wrong: pytest's `def test_x(mapper)` names a
/// module-level fixture, so that parameter is genuinely *both* a binding and a
/// use, and suppressing it as a shadow deleted 231 real `References` edges,
/// several of them the only inbound edge a fixture had. Same data, two
/// questions, two owners — the rule SC17 and SC25 both landed on.
///
/// A parameter's declared type and default value are uses, not bindings, so the
/// `type` and `value` fields are not descended into: `def f(x: Widget)` binds
/// `x` and refers to `Widget`.
fn collect_parameter_names(callable: Node, source: &str, out: &mut BTreeSet<String>) {
    let mut params_cursor = callable.walk();
    let Some(params) = callable
        .children(&mut params_cursor)
        .find(|child| PARAMETER_LIST_KINDS.contains(&child.kind()))
    else {
        return;
    };
    let mut worklist = vec![params];
    while let Some(node) = worklist.pop() {
        if node.kind() == "identifier" {
            let name = get_node_text(node, source);
            if is_user_ident(&name) {
                out.insert(name);
            }
        }
        let mut cursor = node.walk();
        for (index, child) in node.children(&mut cursor).enumerate() {
            if matches!(
                node.field_name_for_child(index as u32),
                Some("type") | Some("value")
            ) {
                continue;
            }
            worklist.push(child);
        }
    }
}
/// The **type parameters** the callable declares on itself: `T` and `E` in
/// `fn read<T, E>(…)`, `T` in `func Map[T any](…)`, `K` in
/// `function pick<K extends string>(…)`.
///
/// X40. A type parameter is a name the enclosing item binds in its own
/// signature, which is precisely what `UnresolvedClass::LocalBinding` is
/// defined as — "a bare call to a name the enclosing symbol itself declares".
/// Before this, a use of `T` in the body or the parameter list matched no
/// indexed symbol and landed in the tier documented as the one that indicates a
/// defect, which is not what a generic parameter is.
///
/// Read through the grammar's `type_parameters` node rather than from a naming
/// convention: `T`-shaped single letters are the *style*, not the rule, and a
/// parameter called `Item` is no less bound by the signature that declares it.
/// Every grammar this crate links spells the list `type_parameters`; a language
/// whose grammar does not simply contributes nothing here, which leaves its
/// generics exactly where they are today rather than guessing.
fn collect_type_parameter_names(callable: Node, source: &str, out: &mut BTreeSet<String>) {
    let mut cursor = callable.walk();
    let Some(params) = callable
        .children(&mut cursor)
        .find(|child| child.kind() == "type_parameters")
    else {
        return;
    };
    let mut worklist = vec![params];
    while let Some(node) = worklist.pop() {
        // The declared name is the *first* identifier of each entry; a bound
        // (`T: Display`, `T any`) is a use of another type and must not be
        // recorded as though this signature declared it.
        if matches!(node.kind(), "type_parameter" | "constrained_type_parameter") {
            if let Some(name) = node
                .child_by_field_name("name")
                .or_else(|| node.named_child(0))
                .map(|child| get_node_text(child, source))
                .filter(|name| is_user_ident(name))
            {
                out.insert(name);
            }
            continue;
        }
        if matches!(node.kind(), "type_identifier" | "identifier")
            && bounded_parent(node).is_some_and(|parent| parent.kind() == "type_parameters")
        {
            let name = get_node_text(node, source);
            if is_user_ident(&name) {
                out.insert(name);
            }
            continue;
        }
        let mut children = node.walk();
        for child in node.named_children(&mut children) {
            worklist.push(child);
        }
    }
}

fn collect_scope_locals(root: Node, source: &str, file_symbol_name: &str) -> Vec<(String, String)> {
    let mut by_scope: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut worklist = vec![root];
    while let Some(node) = worklist.pop() {
        if is_callable_node(node) {
            if let Some(scope) = node
                .child(0)
                .and_then(|child| enclosing_callable_qualified(child, source, file_symbol_name))
            {
                let entry = by_scope.entry(scope).or_default();
                with_scope_locals(node, source, |locals| {
                    entry.extend(locals.iter().cloned());
                });
                // `collect_non_symbol_locals` recognises a parameter only when
                // the grammar wraps it in a node carrying `name` or `pattern`,
                // so `def f(a)`, `def f(a: T)`, `function f(a)` and `|handler|`
                // all fell through. The same declaration counted as a binding or
                // not depending on whether the author wrote a default or a type,
                // which is why `next_gap_id: Callable[…]` and `cls` were the two
                // largest remaining unattributed callees on this repository.
                collect_parameter_names(node, source, entry);
                // X40. A type parameter is bound by this signature exactly as a
                // value parameter is, and the resolver reads both from the same
                // per-scope set.
                collect_type_parameter_names(node, source, entry);
            }
        }
        push_children(node, &mut worklist);
    }
    by_scope
        .into_iter()
        .flat_map(|(scope, locals)| locals.into_iter().map(move |local| (scope.clone(), local)))
        .collect()
}

fn is_inside_import_or_export(node: Node) -> bool {
    let mut current = bounded_parent(node);
    while let Some(parent) = current {
        if matches!(
            parent.kind(),
            "import_specifier"
                | "export_specifier"
                | "import_spec"
                | "export_clause"
                | "named_imports"
                | "namespace_import"
                | "import_clause"
                | "use_declaration"
                | "use_list"
        ) {
            return true;
        }
        // A declaration body is a use site. Stop before `export function`
        // / `export const` would otherwise hide every identifier in the
        // exported item.
        if matches!(
            parent.kind(),
            "statement_block"
                | "class_body"
                | "function_body"
                | "block"
                | "compound_statement"
                | "function_declaration"
                | "function_definition"
                | "async_function_definition"
                | "arrow_function"
                | "function_expression"
                | "method_definition"
                | "class_declaration"
                | "lexical_declaration"
                | "variable_declaration"
        ) {
            return false;
        }
        if matches!(
            parent.kind(),
            "import_statement" | "import_declaration" | "import_from_statement"
        ) {
            return true;
        }
        current = bounded_parent(parent);
    }
    false
}

fn is_call_callee(node: Node) -> bool {
    let Some(parent) = bounded_parent(node) else {
        return false;
    };
    // An Objective-C keyword message has one `method` field per selector part,
    // so the single-field lookup below would only ever recognise the first.
    // Each part is the callee's name, never a use of some same-named symbol.
    if parent.kind() == "message_expression" {
        let mut cursor = parent.walk();
        if cursor.goto_first_child() {
            loop {
                if cursor.field_name() == Some("method") && cursor.node().id() == node.id() {
                    return true;
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }
    if matches!(
        parent.kind(),
        "call" | "call_expression" | "new_expression" | "composite_literal"
    ) {
        let callee = parent
            .child_by_field_name("function")
            .or_else(|| parent.child_by_field_name("constructor"))
            .or_else(|| parent.child_by_field_name("type"));
        if let Some(callee) = callee {
            // A walk up from `node` used to sit here, looking for the callee
            // among its ancestors. It was dead by construction: this branch is
            // only entered when `bounded_parent(node)` *is* the call, so the first
            // ancestor examined is always that same parent and the walk broke
            // immediately. Instrumented over 4,421 files in five languages it
            // never once reached the callee or iterated twice.
            if callee.id() == node.id() {
                return true;
            }
        }
    }
    if matches!(
        parent.kind(),
        "selector_expression" | "member_expression" | "attribute"
    ) {
        // Only the *member* half is the callee. `console.print(…)` names two
        // things: the method, which the call record already carries, and the
        // receiver, which is an ordinary use of a different symbol and the only
        // evidence that symbol is alive.
        //
        // Refusing both made every method receiver invisible to the graph. A
        // module-level singleton used the way singletons are used —
        // `console = _common.console` at the top of a file and `console.print`
        // in every function below — had no inbound edge at all and was reported
        // dead at 0.9 in eleven files of this repository at once.
        let is_member_half = parent
            .child_by_field_name("attribute")
            .or_else(|| parent.child_by_field_name("property"))
            .or_else(|| parent.child_by_field_name("field"))
            .is_some_and(|member| member.id() == node.id());
        if is_member_half {
            if let Some(grand) = bounded_parent(parent) {
                // The `function` check is redundant in every linked grammar —
                // arguments are wrapped in `arguments`/`argument_list`, so a
                // member expression that is a direct child of a call is always
                // its callee, measured zero counterexamples over the same
                // corpus. It is kept as a guard: without it, a grammar that
                // nests arguments differently would silently classify an
                // argument as the callee.
                if matches!(grand.kind(), "call" | "call_expression")
                    && grand
                        .child_by_field_name("function")
                        .is_some_and(|function| function.id() == parent.id())
                {
                    return true;
                }
            }
        }
    }
    false
}

fn assignment_binding(mut node: Node, source: &str) -> Option<String> {
    for _ in 0..4 {
        node = bounded_parent(node)?;
        let binding = match node.kind() {
            "assignment"
            | "assignment_expression"
            | "augmented_assignment"
            | "short_var_declaration"
            | "assignment_statement" => node
                .child_by_field_name("left")
                .or_else(|| node.child_by_field_name("name")),
            "variable_declarator" => node.child_by_field_name("name"),
            "let_declaration" => node
                .child_by_field_name("pattern")
                .or_else(|| node.child_by_field_name("name")),
            _ => None,
        };
        if let Some(binding) = binding {
            return simple_binding_name(binding, source);
        }
        if matches!(
            node.kind(),
            "expression_statement"
                | "return_statement"
                | "function_definition"
                | "function_declaration"
                | "method_definition"
        ) {
            break;
        }
    }
    None
}

/// The single local name a binding node introduces, if it introduces exactly one.
///
/// Go wraps a `:=` target in an `expression_list` even when there is only one
/// target, so `worker := NewWorker()` reached this as a list rather than an
/// identifier and bound nothing — every Go value constructed the idiomatic way
/// had a receiver with no type behind it.
///
/// A list with more than one target is deliberately *not* bound: in
/// `value, err := New()` nothing here says which target receives the
/// constructed value, and a guess would bind a real name to the wrong type,
/// which is worse than leaving it unresolved.
fn simple_binding_name(node: Node, source: &str) -> Option<String> {
    if matches!(node.kind(), "expression_list" | "pattern_list") {
        if node.named_child_count() != 1 {
            return None;
        }
        return node
            .named_child(0)
            .and_then(|only| simple_binding_name(only, source));
    }
    if matches!(
        node.kind(),
        "identifier" | "variable_name" | "shorthand_property_identifier_pattern"
    ) {
        let name = get_node_text(node, source);
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

pub(crate) fn get_child_text(node: Node, field: &str, source: &str) -> Option<String> {
    node.child_by_field_name(field)
        .map(|n| get_node_text(n, source))
}

pub(crate) fn get_node_text(node: Node, source: &str) -> String {
    let start = node.start_byte();
    let end = node.end_byte();
    if start <= end && end <= source.len() {
        source[start..end].to_string()
    } else {
        String::new()
    }
}

pub(crate) fn node_span(node: Node) -> Span {
    Span {
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_finalization_refused(hook: fn()) {
        struct ClearHook;
        impl Drop for ClearHook {
            fn drop(&mut self) {
                EXTRACTION_FINISH_HOOK.with(|hook| hook.set(None));
            }
        }
        let _clear = ClearHook;
        EXTRACTION_FINISH_HOOK.with(|slot| slot.set(Some(hook)));
        let extraction = extract_treesitter_with_budget(
            "finish.py",
            "python",
            "def work():\n    return 1\ndef caller():\n    return work()\n",
            std::time::Duration::from_secs(2),
        );
        assert!(
            EXTRACTION_FINISH_HOOK.with(|slot| slot.get().is_none()),
            "the finalization boundary must be reached"
        );
        assert!(
            matches!(&extraction.parse_outcome, ParseOutcome::Failed { reason } if reason.contains("budget")),
            "late finalization published {:?}",
            extraction.parse_outcome
        );
        assert_eq!(extraction.symbols.len(), 1, "only the File node may remain");
        assert!(extraction.calls.is_empty());
    }

    #[test]
    fn finalization_cannot_publish_after_deadline_expiry() {
        fn expire() {
            let deadline = WALK_DEADLINE.with(|slot| slot.get().expect("armed extraction"));
            while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
                std::thread::sleep(remaining);
            }
        }
        assert_finalization_refused(expire);
    }

    #[test]
    fn finalization_cannot_publish_after_a_late_incomplete_walk() {
        fn incomplete() {
            WALK_OVERRAN.with(|slot| slot.set(true));
        }
        assert_finalization_refused(incomplete);
    }

    /// Every discovered file is addressable, whether or not it parsed. (K1)
    ///
    /// The `File` node used to be pushed only inside the tree-sitter branch, so
    /// a language with no linked grammar produced an `Extraction` with an empty
    /// `symbols` vector: the file was recorded in `generation_files` and was
    /// absent from the graph entirely — not returnable by a file-level query
    /// and not usable as the target of any edge.
    ///
    /// All three cases below reached that same nodeless state, and they are
    /// kept apart because they fail for different reasons: a grammarless
    /// language whose declarations *are* recoverable, one whose declarations
    /// are not, and a prose format that is deliberately never scanned. Only
    /// the first would be fixed by improving the fallback scanner, which is
    /// why "tier-2 recovers declarations" does not subsume this test.
    #[test]
    fn a_grammarless_file_still_gets_a_file_node() {
        for (path, source, why) in [
            (
                "api/user.proto",
                "message User {\n  string id = 1;\n}\n",
                "grammarless source with recoverable declarations",
            ),
            (
                "scripts/manifest.psd1",
                "@{ ModuleVersion = '1.0' }\n",
                "grammarless source with nothing to recover",
            ),
            (
                "docs/design.md",
                "# Design\n\nProse, never declaration-scanned.\n",
                "prose format excluded from tier-2 by NON_DECLARATIVE_LANGUAGES",
            ),
        ] {
            let extraction = crate::extract_file(path, source);
            let files: Vec<&ExtractedSymbol> = extraction
                .symbols
                .iter()
                .filter(|symbol| symbol.kind == SymbolKind::File)
                .collect();
            assert_eq!(
                files.len(),
                1,
                "{path} ({why}) must contribute exactly one File node, got {:?}",
                extraction.symbols
            );
            let file = files[0];
            assert_eq!(
                file.qualified_name, path,
                "the File node is addressed by full path, like every parsed file"
            );
            assert_eq!(
                file.span.end_byte,
                source.len(),
                "the File node spans the whole file"
            );
        }
    }

    /// The `File` node is not a declaration, so it must not be read as one.
    ///
    /// Emitting it unconditionally would be an easy way to make a nodeless file
    /// *look* recovered. It must not move `parse_outcome`: a file whose
    /// declarations could not be recovered still reports `Failed`, and only a
    /// real declaration recovery reports `Fallback`.
    #[test]
    fn the_file_node_alone_is_never_reported_as_a_recovery() {
        let bare = crate::extract_file("scripts/manifest.psd1", "@{ ModuleVersion = '1.0' }\n");
        assert!(
            matches!(bare.parse_outcome, ParseOutcome::Failed { .. }),
            "a File node is not a recovered declaration, got {:?}",
            bare.parse_outcome
        );
        assert!(
            matches!(bare.engine, ExtractionEngine::Unavailable { .. }),
            "engine must stay Unavailable, got {:?}",
            bare.engine
        );

        let recovered =
            crate::extract_file("api/user.proto", "message User {\n  string id = 1;\n}\n");
        assert!(
            matches!(recovered.parse_outcome, ParseOutcome::Fallback { .. }),
            "a real declaration recovery still reports Fallback, got {:?}",
            recovered.parse_outcome
        );
        assert!(
            recovered
                .symbols
                .iter()
                .any(|symbol| symbol.name == "User" && symbol.kind != SymbolKind::File),
            "the recovered declaration must survive alongside the File node"
        );
    }

    /// A real syntax error must never be reported as a clean parse.
    ///
    /// `is_benign_jsx_ampersand` suppresses a known upstream grammar
    /// false-error on a lone `&` in JSX text. Suppression is the dangerous
    /// direction: if it over-matches, genuinely broken source reports
    /// `Clean`, and X6's rule — that symbols overlapping a parse error are
    /// never dead-code evidence — stops firing, because there is no recorded
    /// error to overlap. `treesitter.rs` had no tests at all, so nothing held
    /// this boundary.
    #[test]
    fn benign_jsx_ampersand_is_suppressed_but_real_errors_are_not() {
        // A lone `&` in JSX text is valid TSX the grammar mis-flags.
        let jsx = extract_treesitter(
            "ok.tsx",
            "tsx",
            "export function A() {\n  return <div>Tom & Jerry</div>;\n}\n",
        );
        assert!(
            matches!(jsx.parse_outcome, ParseOutcome::Clean),
            "a lone & in JSX text must not degrade the outcome: {:?}",
            jsx.parse_outcome
        );

        // Genuinely broken source must stay Partial, with ranges recorded.
        let broken = extract_treesitter("bad.tsx", "tsx", "export function A( {\n");
        match &broken.parse_outcome {
            ParseOutcome::Partial { error_ranges } => {
                assert!(
                    !error_ranges.is_empty(),
                    "a partial parse must record ranges"
                );
            }
            other => panic!("broken source must not report as {other:?}"),
        }

        // A plain-text error *inside* a JSX element must stay Partial. The
        // suppression's first operand is a veto on non-ampersand text, and
        // because `&&` binds tighter than `||` its removal turns the whole
        // guard into "no braces, no angle bracket" — so any ordinary syntax
        // error inside JSX (here `{a b}`) is silently reported Clean. Verified
        // to produce exactly one error range, `"b"`, carrying none of the four
        // characters the guard tests.
        let plain_error_in_jsx = extract_treesitter(
            "plain.tsx",
            "tsx",
            "export function A() {\n  return <div>{a b}</div>;\n}\n",
        );
        assert!(
            matches!(
                plain_error_in_jsx.parse_outcome,
                ParseOutcome::Partial { .. }
            ),
            "an ordinary syntax error inside JSX must not be suppressed by the \
             ampersand rule: {:?}",
            plain_error_in_jsx.parse_outcome
        );

        // A stray brace *inside* a JSX element, alongside an ampersand, must
        // stay Partial. This is the only shape that separates the first guard
        // from the rest: the error node contains both `&` and `}` and does have
        // a JSX ancestor, so if `!contains('&')` stops being a veto the node is
        // suppressed and a genuine JSX error reports as Clean. Verified to
        // produce exactly one error range, `"& b }"`, so nothing else can carry
        // the assertion.
        let stray_brace = extract_treesitter(
            "brace.tsx",
            "tsx",
            "export function A() {\n  return <div>a & b }</div>;\n}\n",
        );
        assert!(
            matches!(stray_brace.parse_outcome, ParseOutcome::Partial { .. }),
            "a stray `}}` in JSX is a real error and must not be suppressed by the \
             ampersand rule: {:?}",
            stray_brace.parse_outcome
        );

        // Broken source that also contains an ampersand must still be Partial:
        // the suppression must not generalise from "contains &" to "is fine".
        let broken_with_amp = extract_treesitter(
            "bad2.tsx",
            "tsx",
            "export function A( {\n  const x = 1 & 2;\n",
        );
        assert!(
            !matches!(broken_with_amp.parse_outcome, ParseOutcome::Clean),
            "an ampersand must not launder a real syntax error: {:?}",
            broken_with_amp.parse_outcome
        );
    }

    /// A macro body is only probed when it could contain a call.
    ///
    /// `macro_token_body` strips the delimiter pair and skips bodies with no
    /// parenthesis. The guard is what keeps SC13's recovery from re-parsing
    /// every `vec![1, 2, 3]` in a repository, and the delimiter handling is
    /// what makes `[]` and `{}` macros work at all.
    #[test]
    fn macro_bodies_are_unwrapped_and_skipped_when_they_hold_no_call() {
        assert_eq!(
            macro_token_body("(\"{}\", s.work())").as_deref(),
            Some("\"{}\", s.work()"),
            "parenthesised bodies unwrap"
        );
        assert_eq!(
            macro_token_body("[a.b()]").as_deref(),
            Some("a.b()"),
            "bracket macros unwrap too"
        );
        assert_eq!(
            macro_token_body("{a.b()}").as_deref(),
            Some("a.b()"),
            "brace macros unwrap too"
        );

        // No parenthesis inside means no call to recover.
        assert_eq!(macro_token_body("[1, 2, 3]"), None);
        assert_eq!(macro_token_body("()"), None, "an empty body holds nothing");
        // Unbalanced or undelimited text is not a token tree body.
        assert_eq!(macro_token_body("no delimiters"), None);
    }

    /// Rust type names strip references, pointers and generics.
    ///
    /// This feeds parameter-type receiver bindings (SC12). Leaving `&` or a
    /// generic argument attached makes the binding name a type that no symbol
    /// has, so the call silently fails to resolve.
    #[test]
    fn rust_type_names_reduce_to_the_bare_identifier() {
        let probe = |source: &str| -> Option<String> {
            let ext = extract_treesitter("t.rs", "rust", source);
            ext.references
                .iter()
                .find(|reference| reference.assigned_to.as_deref() == Some("value"))
                .map(|reference| reference.name.clone())
        };
        assert_eq!(probe("fn f(value: &Store) {}").as_deref(), Some("Store"));
        assert_eq!(
            probe("fn f(value: &mut Store) {}").as_deref(),
            Some("Store")
        );
        assert_eq!(probe("fn f(value: Vec<Store>) {}").as_deref(), Some("Vec"));
        assert_eq!(
            probe("fn f(value: crate::inner::Store) {}").as_deref(),
            Some("Store"),
            "a scoped path reduces to its last segment"
        );
    }

    /// A non-callee identifier is still recorded as a name use.
    ///
    /// Positive control for the suppression in `maybe_push_name_reference`:
    /// `is_call_callee` was replaceable with a constant in *both* directions,
    /// and the `false` direction is caught by the duplicate-count assertion
    /// above. This covers the `true` direction — a suppression that widens to
    /// every identifier erases the non-call uses that liveness relies on, so a
    /// symbol used only as a value starts reading as dead.
    #[test]
    fn identifiers_used_as_values_are_still_name_references() {
        let extraction = extract_treesitter(
            "f.py",
            "python",
            "import os\ndef run():\n    os.path.join('a')\n",
        );
        let names: Vec<&str> = extraction
            .references
            .iter()
            .filter(|r| r.kind == ReferenceKind::Name)
            .map(|r| r.name.as_str())
            .collect();
        assert!(
            names.contains(&"os"),
            "the receiver of a call is a name use, not a callee: {names:?}"
        );
        assert!(
            names.contains(&"path"),
            "an intermediate attribute is a name use: {names:?}"
        );
        assert!(
            !names.contains(&"join"),
            "the callee itself must not also be a name use: {names:?}"
        );
    }

    /// Only identifier-shaped text is treated as a user identifier.
    ///
    /// This predicate decides whether a token becomes a graph node at all.
    /// Every operator in it was mutable: with `&&` -> `||` a leading digit is
    /// accepted, with `all` -> `any` any punctuation-bearing text is accepted,
    /// and with either `||` collapsed the `_`/`$` prefixes stop being
    /// identifiers. Loosened, operators and literals become symbols and every
    /// reference count is inflated; tightened, `_private` and `$el` disappear
    /// from the graph and read as dead.
    #[test]
    fn only_identifier_shaped_text_is_a_user_identifier() {
        for accepted in [
            "foo", "_foo", "$foo", "_", "$", "a1", "foo$bar", "foo_bar9", "A", "_9",
        ] {
            assert!(
                is_user_ident(accepted),
                "{accepted:?} is identifier-shaped and must be accepted"
            );
        }

        for rejected in [
            "",        // no first character at all
            "9foo",    // a leading digit is not an identifier start
            "1",       //
            "foo-bar", // punctuation is not an identifier character
            "foo bar",
            "foo.bar",
            "foo()",
            "&",
            "...",
            "\u{e9}t\u{e9}", // non-ASCII
            "\u{4f60}\u{597d}",
        ] {
            assert!(
                !is_user_ident(rejected),
                "{rejected:?} is not identifier-shaped and must be rejected"
            );
        }
    }

    fn name_refs(extraction: &Extraction) -> Vec<&str> {
        extraction
            .references
            .iter()
            .filter(|r| r.kind == ReferenceKind::Name)
            .map(|r| r.name.as_str())
            .collect()
    }

    /// A C-family declaration must not reference the name it declares.
    ///
    /// C, C++, Objective-C, CUDA and Metal name a declaration through a
    /// `declarator` chain, not a `name` field, so `is_defining_name` did not
    /// recognise a function's own identifier and every declaration emitted a
    /// `Name` reference to itself. `analyze_liveness` skips only
    /// `Contains`/`Defines`/`MemberOf`, so that self-reference resolved to a
    /// `file -> file::symbol` `References` edge and counted as a call: no
    /// C-family symbol could ever be `!is_called`, which disables dead-code
    /// detection for the whole language family rather than degrading it.
    #[test]
    fn c_family_declarations_do_not_reference_themselves() {
        let c = extract_treesitter(
            "a.c",
            "c",
            "int helper(int x) { return x + 1; }\nint caller(void) { return helper(2); }\n",
        );
        assert!(
            !name_refs(&c).contains(&"helper"),
            "a C function's own declarator is a definition, not a use: {:?}",
            name_refs(&c)
        );
        assert!(!name_refs(&c).contains(&"caller"), "{:?}", name_refs(&c));

        let cpp = extract_treesitter(
            "b.cpp",
            "cpp",
            "float scale(float v) { return v * 2.0f; }\n",
        );
        assert!(!name_refs(&cpp).contains(&"scale"), "{:?}", name_refs(&cpp));

        // The symbols themselves must survive: the suppression removes the
        // bogus reference, not the declaration it was attached to.
        let qualified: Vec<&str> = c
            .symbols
            .iter()
            .map(|s| s.qualified_name.as_str())
            .collect();
        assert!(qualified.contains(&"a.c::helper"), "{qualified:?}");
        assert!(qualified.contains(&"a.c::caller"), "{qualified:?}");
    }

    /// The declarator suppression follows `declarator` identity, not subtree
    /// containment.
    ///
    /// `field_contains` — the test every other arm of `is_defining_name` uses —
    /// is a *subtree* test, and a C declarator subtree also holds the signature
    /// (`function_declarator.parameters`) and the array bound
    /// (`array_declarator.size`). Widening to containment therefore silently
    /// erases parameter types and array bounds, which are genuine cross-symbol
    /// references, and the symbols they name start reading as dead. These are
    /// the identifiers that must survive on the other side of the fix.
    #[test]
    fn c_declarator_suppression_spares_real_references() {
        let c = extract_treesitter(
            "b.c",
            "c",
            concat!(
                "int table[LIMIT];\n",
                "int total = origin;\n",
                "void takes(Store *s);\n",
            ),
        );
        let names = name_refs(&c);
        assert!(
            names.contains(&"LIMIT"),
            "an array bound sits under `size`, not the declarator chain: {names:?}"
        );
        assert!(
            names.contains(&"origin"),
            "an initializer sits under `value`, not the declarator chain: {names:?}"
        );
        let types: Vec<&str> = c
            .references
            .iter()
            .filter(|r| r.kind == ReferenceKind::Type)
            .map(|r| r.name.as_str())
            .collect();
        assert!(
            types.contains(&"Store"),
            "a parameter type is inside the function declarator but is a use: {types:?}"
        );

        // The bindings on the near side stay suppressed.
        for declared in ["table", "total", "takes", "s"] {
            assert!(
                !names.contains(&declared),
                "{declared:?} is declared here, not used: {names:?}"
            );
        }
    }

    /// A C function name is a symbol binding, not a scope-local.
    ///
    /// `collect_non_symbol_locals` files every `is_defining_name` that is not an
    /// `is_symbol_binding` into a per-scope shadow set, and
    /// `name_is_shadowed_by_local` then drops `Name` references matching it. Once
    /// the declarator arm makes a function's own identifier a defining name, the
    /// function would shadow itself inside its own body and non-call uses of it —
    /// taking its address, passing it as a callback — would vanish.
    #[test]
    fn c_function_names_are_symbol_bindings_not_locals() {
        let c = extract_treesitter(
            "c.c",
            "c",
            concat!(
                "int helper(int x) { return x + 1; }\n",
                "void install(void) { register_cb(helper); }\n",
            ),
        );
        let names = name_refs(&c);
        assert!(
            names.contains(&"helper"),
            "a function passed as a value is a real use: {names:?}"
        );
        assert_eq!(
            names.iter().filter(|name| **name == "helper").count(),
            1,
            "only the callback use, never the declarator: {names:?}"
        );
    }

    /// Every call syntax splits into the right `(callee, receiver)` pair.
    ///
    /// `split_call_target` has one arm per grammar's member-call node, and each
    /// arm was independently deletable — deleting the `member_expression` arm
    /// makes JS `obj.method()` fall through to the catch-all, which reports the
    /// callee as the literal text `obj.method` with no receiver. Nothing errors:
    /// the call is still recorded, it just names a symbol that does not exist,
    /// so the edge never joins and `method` looks uncalled. This is the same
    /// shape as SC9/SC10, where unjoinable call edges produced 74,312 orphans.
    ///
    /// Each expected pair below was read off the extractor before being pinned.
    #[test]
    fn member_calls_split_into_callee_and_receiver_in_every_grammar() {
        let cases: [(&str, &str, &str, &str, Option<&str>); 5] = [
            // (file, grammar, source, expected callee, expected receiver)
            (
                "f.py",
                "python",
                "import os\ndef run():\n    os.path.join('a')\n",
                "join",
                Some("os.path"),
            ),
            (
                "f.js",
                "javascript",
                "function run() {\n  obj.method(1);\n}\n",
                "method",
                Some("obj"),
            ),
            (
                "f.rs",
                "rust",
                "fn run() {\n    let x = Thing::new();\n    x.method();\n}\n",
                "method",
                Some("x"),
            ),
            (
                "f.go",
                "go",
                "package main\nfunc run() {\n\tsvc.Method()\n}\n",
                "Method",
                Some("svc"),
            ),
            (
                "f.ts",
                "typescript",
                "function run(): void {\n  this.svc.doIt();\n}\n",
                "doIt",
                Some("this.svc"),
            ),
        ];

        for (path, grammar, source, callee, receiver) in cases {
            let extraction = extract_treesitter(path, grammar, source);
            let call = extraction
                .calls
                .iter()
                .find(|call| call.callee_name == callee)
                .unwrap_or_else(|| {
                    panic!(
                        "{grammar}: no call named {callee:?}; got {:?}",
                        extraction
                            .calls
                            .iter()
                            .map(|c| (&c.callee_name, &c.receiver_expr))
                            .collect::<Vec<_>>()
                    )
                });
            assert_eq!(
                call.receiver_expr.as_deref(),
                receiver,
                "{grammar}: wrong receiver for {callee}"
            );
            assert!(
                !call.callee_name.contains('.'),
                "{grammar}: the callee must be the bare method name, not the whole \
                 member expression, or the edge cannot join: {:?}",
                call.callee_name
            );
        }
    }

    /// A plain call has no receiver, and a definition name is not a reference.
    ///
    /// The catch-all arm of `split_call_target` must leave `receiver_expr`
    /// `None` — a fabricated receiver turns an ordinary free-function call into
    /// a method call on a type that does not exist. Separately, `is_defining_name`
    /// decides whether an identifier at a declaration site is a *use*: inverted,
    /// every function would reference itself, which is precisely the shape that
    /// makes a dead symbol look live.
    #[test]
    fn free_calls_have_no_receiver_and_definitions_are_not_uses() {
        for (path, grammar, source) in [
            ("f.py", "python", "def run():\n    helper()\n"),
            ("f.js", "javascript", "function run() {\n  helper();\n}\n"),
            ("f.rs", "rust", "fn run() {\n    helper();\n}\n"),
            ("f.go", "go", "package main\nfunc run() {\n\thelper()\n}\n"),
        ] {
            let extraction = extract_treesitter(path, grammar, source);

            let call = extraction
                .calls
                .iter()
                .find(|call| call.callee_name == "helper")
                .unwrap_or_else(|| panic!("{grammar}: `helper()` must be recorded as a call"));
            assert_eq!(
                call.receiver_expr, None,
                "{grammar}: a free call must not invent a receiver"
            );
            assert!(
                call.caller_symbol
                    .as_deref()
                    .is_some_and(|c| c.ends_with("run")),
                "{grammar}: the call must be attributed to its enclosing function, or it \
                 cannot make `run` reach `helper`: {:?}",
                call.caller_symbol
            );

            // The callee is a Call-kind reference, so liveness sees an invocation
            // rather than a bare name occurrence.
            assert!(
                extraction
                    .references
                    .iter()
                    .any(|r| r.name == "helper" && r.kind == ReferenceKind::Call),
                "{grammar}: the callee must be a Call reference: {:?}",
                extraction
                    .references
                    .iter()
                    .map(|r| (&r.name, r.kind))
                    .collect::<Vec<_>>()
            );

            // ...and *only* a Call reference. `is_call_callee` exists to stop a
            // callee being pushed a second time as a bare Name; inverted, every
            // call is double-counted as both an invocation and a name use.
            assert_eq!(
                extraction
                    .references
                    .iter()
                    .filter(|r| r.name == "helper")
                    .count(),
                1,
                "{grammar}: the callee must be recorded once, not as both Call and Name: {:?}",
                extraction
                    .references
                    .iter()
                    .map(|r| (&r.name, r.kind))
                    .collect::<Vec<_>>()
            );

            // The declaration site of `run` is not a use of `run`.
            assert!(
                !extraction.references.iter().any(|r| r.name == "run"),
                "{grammar}: a function's own declaration name must not be recorded as a \
                 reference to itself: {:?}",
                extraction
                    .references
                    .iter()
                    .map(|r| (&r.name, r.kind))
                    .collect::<Vec<_>>()
            );
        }
    }

    /// Definition sites are suppressed; the values they bind are not.
    ///
    /// Four separate guards decide whether an identifier is a *use*, and each
    /// was mutable without a failure. They all fail in the same direction —
    /// a use stops being recorded, so the symbol it names loses an incoming
    /// edge and moves toward `dead` — except the object-key case, which fails
    /// the other way and invents a use of whatever the key is named.
    ///
    /// Every expectation below was read off the extractor before being pinned.
    #[test]
    fn definition_sites_are_suppressed_but_their_values_are_recorded() {
        // Type annotations are Type references. Deleting the `type_identifier`
        // arm drops every annotation, which is how a type used only in
        // signatures starts reading as unreferenced.
        let types = extract_treesitter(
            "f.ts",
            "typescript",
            "function f(x: Foo): Bar {\n  return x as Baz;\n}\n",
        );
        let type_refs: Vec<&str> = types
            .references
            .iter()
            .filter(|r| r.kind == ReferenceKind::Type)
            .map(|r| r.name.as_str())
            .collect();
        for expected in ["Foo", "Bar", "Baz"] {
            assert!(
                type_refs.contains(&expected),
                "parameter, return and cast annotations are all type uses: {type_refs:?}"
            );
        }

        // The right-hand side of an assignment is a use even though the
        // statement as a whole is a binding. Collapsing the `matches!(..) &&
        // field == left` conjunction to `||` swallows every RHS identifier.
        let assign = extract_treesitter(
            "f.py",
            "python",
            "def run():\n    x = helper_value\n    return x\n",
        );
        let assign_names: Vec<&str> = assign.references.iter().map(|r| r.name.as_str()).collect();
        assert!(
            assign_names.contains(&"helper_value"),
            "the value assigned is a use of that value: {assign_names:?}"
        );
        assert!(
            !assign_names.contains(&"x"),
            "the assignment target is a binding, not a use: {assign_names:?}"
        );

        // In an object literal the key is a binding and the value is a use.
        // Inverting the `pair` test makes the key a reference to whatever
        // symbol shares its name.
        let pair = extract_treesitter(
            "f.js",
            "javascript",
            "function run() {\n  return { key: value };\n}\n",
        );
        let pair_names: Vec<&str> = pair.references.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            pair_names,
            ["value"],
            "an object literal records its value and not its key"
        );

        // A member expression passed as an *argument* is a use, not a callee.
        // Widening the callee test to `||` suppresses it, so `b` in `f(a.b)`
        // disappears from the graph entirely.
        let argument = extract_treesitter("f.js", "javascript", "function run() {\n  f(a.b);\n}\n");
        let arg_names: Vec<&str> = argument
            .references
            .iter()
            .filter(|r| r.kind == ReferenceKind::Name)
            .map(|r| r.name.as_str())
            .collect();
        assert!(
            arg_names.contains(&"a") && arg_names.contains(&"b"),
            "both halves of an argument member expression are uses: {arg_names:?}"
        );
        assert!(
            argument
                .references
                .iter()
                .any(|r| r.name == "f" && r.kind == ReferenceKind::Call),
            "the actual callee is still a Call reference"
        );
    }

    /// The exact symbol set for each linked grammar.
    ///
    /// `extract_node` is one large dispatch and carried 58 surviving mutants:
    /// individual node kinds could be dropped, exportedness flipped, and
    /// qualified names rebuilt, all without a failure, because no test asserted
    /// the *whole* set — only that particular symbols were present. A missing
    /// symbol is the dangerous direction: it is not an error anywhere, the file
    /// still extracts "successfully", and the symbol simply never exists to be
    /// called, so nothing downstream can notice.
    ///
    /// This pins each tuple as `(kind, qualified_name, is_exported)`. The
    /// qualified names are load-bearing: they are the join keys the call graph
    /// resolves against, and two of them encode fixes that were expensive to
    /// get right — `f.rs::Thing.go` vs `f.rs::Doer.go` is SC11's type-first
    /// qualification of impl items, and `f.go::Client.Run` is SC9's receiver
    /// ownership, without which Go method edges do not join at all.
    ///
    /// This is a record of current behavior, not an endorsement of it. Two
    /// entries are parity artifacts rather than intent: a TS `type` alias is
    /// reported as `Interface`, and a Go `interface` type as `Struct`. They are
    /// pinned so that changing them is a visible decision instead of a silent
    /// drift.
    ///
    /// Exported module-level bindings *are* emitted now (`f.go::Limit`); this
    /// golden previously pinned their absence as deliberate, which is how the
    /// omission survived unnoticed. Python module constants and class fields
    /// still produce no symbol — Python has no export marker, so the principled
    /// rule is `__all__` membership, and that is not implemented yet.
    #[test]
    fn each_grammar_extracts_its_exact_symbol_set() {
        /// `(kind, qualified_name, is_exported)`.
        type Symbol<'a> = (SymbolKind, &'a str, bool);
        /// `(path, grammar, source, expected symbols)`.
        type Case<'a> = (&'a str, &'a str, &'a str, &'a [Symbol<'a>]);

        let cases: [Case<'_>; 4] = [
            (
                "f.py",
                "python",
                "import os\n\nCONST = 1\n\nclass Widget:\n    attr: int = 0\n                     def render(self):\n        return helper()\n\ndef helper():\n    return 1\n",
                &[
                    (SymbolKind::File, "f.py", true),
                    (SymbolKind::Class, "f.py::Widget", false),
                    (SymbolKind::Method, "f.py::Widget.render", false),
                    (SymbolKind::Function, "f.py::helper", false),
                ],
            ),
            (
                "f.ts",
                "typescript",
                "export interface Shape { area(): number }\nexport type Alias = string;\n                 export class Box implements Shape {\n  private size = 1;\n                   area(): number { return this.size; }\n}\n                 export function make(): Box { return new Box(); }\nconst local = 5;\n",
                &[
                    (SymbolKind::File, "f.ts", true),
                    (SymbolKind::Interface, "f.ts::Shape", true),
                    (SymbolKind::Interface, "f.ts::Alias", true),
                    (SymbolKind::Class, "f.ts::Box", true),
                    (SymbolKind::Method, "f.ts::Box.area", true),
                    (SymbolKind::Function, "f.ts::make", true),
                ],
            ),
            (
                "f.rs",
                "rust",
                "pub struct Thing { pub id: u32 }\npub enum Mode { On, Off }\n                 pub trait Doer { fn go(&self); }\nimpl Doer for Thing { fn go(&self) {} }\n                 impl Thing { pub fn new() -> Self { Thing { id: 0 } } }\n                 pub fn top() {}\nfn private_fn() {}\n",
                &[
                    (SymbolKind::File, "f.rs", true),
                    (SymbolKind::Struct, "f.rs::Thing", true),
                    (SymbolKind::Enum, "f.rs::Mode", true),
                    (SymbolKind::Trait, "f.rs::Doer", true),
                    (SymbolKind::Method, "f.rs::Doer.go", false),
                    (SymbolKind::Method, "f.rs::Thing.go", false),
                    (SymbolKind::Method, "f.rs::Thing.new", true),
                    (SymbolKind::Function, "f.rs::top", true),
                    (SymbolKind::Function, "f.rs::private_fn", false),
                ],
            ),
            (
                "f.go",
                "go",
                "package svc\n\ntype Client struct { ID int }\n\n                 type Runner interface { Run() error }\n\n                 func (c *Client) Run() error { return nil }\n\n                 func New() *Client { return &Client{} }\n\nfunc helper() {}\n\n                 const Limit = 10\n",
                &[
                    (SymbolKind::File, "f.go", true),
                    (SymbolKind::Struct, "f.go::Client", true),
                    (SymbolKind::Struct, "f.go::Runner", true),
                    (SymbolKind::Method, "f.go::Client.Run", true),
                    (SymbolKind::Function, "f.go::New", true),
                    (SymbolKind::Function, "f.go::helper", false),
                    // An exported package-level constant is public API surface.
                    (SymbolKind::Variable, "f.go::Limit", true),
                ],
            ),
        ];

        for (path, grammar, source, expected) in cases {
            let extraction = extract_treesitter(path, grammar, source);
            assert!(
                matches!(extraction.parse_outcome, ParseOutcome::Clean),
                "{grammar}: the fixture must parse cleanly, or the symbol set below is \
                 measuring a broken parse: {:?}",
                extraction.parse_outcome
            );

            let actual: Vec<Symbol<'_>> = extraction
                .symbols
                .iter()
                .map(|symbol| {
                    (
                        symbol.kind,
                        symbol.qualified_name.as_str(),
                        symbol.is_exported,
                    )
                })
                .collect();
            assert_eq!(actual, expected.to_vec(), "{grammar}: symbol set drifted");

            // Every symbol's parent chain resolves inside this file, so the
            // containment edges the graph is built from cannot dangle.
            for symbol in &extraction.symbols {
                let Some(parent) = symbol.parent_symbol.as_deref() else {
                    assert_eq!(
                        symbol.kind,
                        SymbolKind::File,
                        "{grammar}: only the file symbol may be parentless"
                    );
                    continue;
                };
                assert!(
                    extraction
                        .symbols
                        .iter()
                        .any(|candidate| candidate.qualified_name == parent),
                    "{grammar}: {} names parent {parent:?}, which is not a symbol in this file",
                    symbol.qualified_name
                );
            }
        }
    }

    /// The exact import and export set for each linked grammar.
    ///
    /// The companion to the symbol golden above, aimed at the other half of
    /// `extract_node`'s surviving mutants: whole `import_statement`,
    /// `import_spec` and `variable_declarator` arms were deletable, and the
    /// export-detection conditions were invertible in every direction.
    ///
    /// Imports are what make a symbol reachable from another file. Drop an
    /// import arm and the file's edges to its dependencies vanish — the
    /// extraction still succeeds, so the only visible effect is that the
    /// imported symbols stop having callers and drift toward `dead`. Invert an
    /// export test and the reverse happens: private declarations are published
    /// as public API, which suppresses genuine dead-code findings.
    ///
    /// `(module_specifier, imported_names, local_names, alias)` is pinned
    /// because the resolver joins on all four — SC12's binding work depends on
    /// `imported_names` and `local_names` staying aligned and in order.
    #[test]
    fn each_grammar_extracts_its_exact_imports_and_exports() {
        /// `(module_specifier, imported_names, local_names, alias)`.
        type Import<'a> = (&'a str, Vec<&'a str>, Vec<&'a str>, Option<&'a str>);
        /// `(path, grammar, source, expected imports, expected exports)`.
        type Case<'a> = (&'a str, &'a str, &'a str, Vec<Import<'a>>, Vec<&'a str>);

        let cases: [Case<'_>; 4] = [
            (
                "f.py",
                "python",
                "import os\nimport os.path as osp\nfrom .rel import thing\n                 from pkg.mod import a, b as c\n",
                vec![
                    ("os", vec![], vec![], None),
                    ("os.path", vec![], vec![], Some("osp")),
                    (".rel", vec!["thing"], vec!["thing"], None),
                    // `b as c` must keep the imported and local halves aligned:
                    // the resolver binds `c` in this file to `b` in `pkg.mod`.
                    ("pkg.mod", vec!["a", "b"], vec!["a", "c"], None),
                ],
                vec!["f.py"],
            ),
            (
                "f.ts",
                "typescript",
                "import def from 'm';\nimport { x, y as z } from './local';\n                 import * as ns from 'pkg';\nexport const val = 1;\nexport { x };\n                 export default function d() {}\n",
                vec![
                    ("m", vec![], vec![], Some("def")),
                    ("./local", vec!["x", "y"], vec!["x", "z"], None),
                    // A namespace import records the `*` sentinel, which is how
                    // `ns.anything` stays resolvable without enumerating names.
                    ("pkg", vec!["*"], vec!["ns"], Some("ns")),
                    // `export { x };` is deliberately absent. It used to appear
                    // here as `("", ["x"], ["x"], None)` — an import with an
                    // empty module specifier — and this expectation pinned the
                    // defect rather than the intent: a local re-export with no
                    // `from` is an export and nothing else, and `""` is an
                    // endpoint no resolver can ever bind. It still appears in
                    // the export set below, which is where it belongs.
                ],
                vec!["d", "f.ts", "val", "x"],
            ),
            (
                "f.go",
                "go",
                "package svc\n\nimport (\n\t\"fmt\"\n\t                 alias \"example.com/pkg/sub\"\n)\n\nimport \"single\"\n",
                vec![
                    ("fmt", vec![], vec![], None),
                    // Both the block form and the single form must be read, and
                    // a Go import alias is the package's local name.
                    ("example.com/pkg/sub", vec![], vec![], Some("alias")),
                    ("single", vec![], vec![], None),
                ],
                vec!["f.go"],
            ),
            (
                "f.rs",
                "rust",
                "use std::collections::BTreeMap;\nuse crate::thing::{One, Two as Three};\n                 pub use inner::Exported;\n",
                vec![
                    // X41. Each `use` names a module and the names it takes out
                    // of it, the shape every other grammar here produces. This
                    // expectation used to read
                    // `("crate::thing::{One, Two as Three}", [], [], None)` and
                    // said so in a comment — "pinned as current behavior, not
                    // intent". The intent is this: a module specifier that is
                    // the statement's own source text matches no file and binds
                    // no name, so nothing downstream could use it.
                    ("std::collections", vec!["BTreeMap"], vec!["BTreeMap"], None),
                    ("crate::thing", vec!["One", "Two"], vec!["One", "Three"], None),
                    ("inner", vec!["Exported"], vec!["Exported"], None),
                ],
                vec!["f.rs"],
            ),
        ];

        for (path, grammar, source, expected_imports, expected_exports) in cases {
            let extraction = extract_treesitter(path, grammar, source);
            assert!(
                matches!(extraction.parse_outcome, ParseOutcome::Clean),
                "{grammar}: fixture must parse cleanly: {:?}",
                extraction.parse_outcome
            );

            let actual: Vec<Import<'_>> = extraction
                .imports
                .iter()
                .map(|import| {
                    (
                        import.module_specifier.as_str(),
                        import.imported_names.iter().map(String::as_str).collect(),
                        import.local_names.iter().map(String::as_str).collect(),
                        import.alias.as_deref(),
                    )
                })
                .collect();
            assert_eq!(actual, expected_imports, "{grammar}: import set drifted");

            let mut exports: Vec<&str> = extraction
                .exports
                .iter()
                .map(|export| export.exported_name.as_str())
                .collect();
            exports.sort_unstable();
            assert_eq!(exports, expected_exports, "{grammar}: export set drifted");

            // No import names an empty module.
            //
            // The loop below already refused an empty binding *name*, and this
            // test still passed while every `export { x };` in the corpus
            // pushed an import whose module was `""` — the check was one field
            // short of the class it was written for.
            for import in &extraction.imports {
                assert!(
                    !import.module_specifier.is_empty(),
                    "{grammar}: an import must name a module, got {:?}",
                    import.raw_import
                );
            }

            // Every import's binding pairs are well formed, so the resolver
            // cannot be handed a binding keyed on an empty name.
            for import in &extraction.imports {
                for (local, imported) in import.binding_pairs() {
                    assert!(
                        !local.is_empty() && !imported.is_empty(),
                        "{grammar}: {import:?} produced an empty binding"
                    );
                }
            }
        }
    }

    /// The macro probe recovers real calls and fabricates none.
    ///
    /// `probe_macro_body` is a pure string function and every one of its 18
    /// possible return values was substitutable without a failure, including
    /// `("xyzzy", Some("xyzzy"))`. It is how SC13 recovers calls hidden inside
    /// macro expansions: `println!("{}", helper())` is the only place `helper`
    /// is called in a great deal of Rust, so an empty return makes those callees
    /// look uncalled, and a fabricated return invents edges to symbols that do
    /// not exist. Both were unobservable.
    ///
    /// Every expectation below was read off the function before being pinned.
    #[test]
    fn the_macro_probe_recovers_real_calls_and_fabricates_none() {
        // A direct call inside a macro body is recovered, and the synthetic
        // probe wrapper never leaks out as a call of its own.
        assert_eq!(
            probe_macro_body("\"{}\", helper()"),
            (vec![("helper".to_string(), None)], vec![]),
            "a call inside a macro body is a real call"
        );

        // A nested macro is queued as a body rather than recursed into, so the
        // parser borrow stays scoped to one parse.
        assert_eq!(
            probe_macro_body("a, format!(\"{}\", b.c())"),
            (vec![], vec!["\"{}\", b.c()".to_string()]),
            "an inner macro is deferred, not expanded in place"
        );

        // Receivers survive the probe, so a macro-hidden method call still
        // joins to its type.
        let (calls, nested) = probe_macro_body("thing.go(), other()");
        assert!(nested.is_empty());
        assert_eq!(
            calls,
            vec![
                ("other".to_string(), None),
                ("go".to_string(), Some("thing".to_string())),
            ],
            "a method call inside a macro keeps its receiver"
        );

        // A declarative macro definition is not an argument list. Returning
        // nothing is required: a fabricated call is worse than a missing one.
        assert_eq!(
            probe_macro_body("$x:expr) => { $x "),
            (vec![], vec![]),
            "a macro_rules body must not be read as calls"
        );
    }

    /// A macro body is unwrapped from any delimiter, and skipped when it holds
    /// no call.
    #[test]
    fn macro_bodies_unwrap_every_delimiter_and_skip_callless_ones() {
        for wrapped in ["(a())", "[a()]", "{a()}"] {
            assert_eq!(
                macro_token_body(wrapped).as_deref(),
                Some("a()"),
                "`{wrapped}` must unwrap: `vec![..]` and `matches!{{..}}` are as common as \
                 parenthesised macros"
            );
        }

        // Without a parenthesis there is no call to recover, so the parse is
        // skipped entirely rather than run and discarded.
        for skipped in ["(nocall)", "()", "raw"] {
            assert_eq!(
                macro_token_body(skipped),
                None,
                "`{skipped}` holds no call and must not be probed"
            );
        }
    }

    /// Nested macros are followed to a bounded depth, and the bound holds.
    ///
    /// The depth comparison was mutable in every direction and the `depth + 1`
    /// increment to `-` and `*`. Loosened, a pathological nest runs away inside
    /// extraction; tightened, ordinary two- and three-level nests such as
    /// `assert_eq!(a, format!("{}", b.c()))` stop yielding their calls. The
    /// boundary is measured, not assumed: five levels resolve, six do not.
    #[test]
    fn nested_macros_are_followed_to_a_bounded_depth() {
        let calls_at = |depth: usize| -> Vec<String> {
            let mut body = String::from("deep()");
            for _ in 0..depth {
                body = format!("m!({body})");
            }
            extract_treesitter("f.rs", "rust", &format!("fn run() {{ {body}; }}\n"))
                .calls
                .iter()
                .map(|call| call.callee_name.clone())
                .collect()
        };

        for depth in 1..=5 {
            assert!(
                calls_at(depth).contains(&"deep".to_string()),
                "a call nested {depth} macro levels deep must still be recovered"
            );
        }
        assert!(
            !calls_at(6).contains(&"deep".to_string()),
            "the depth bound must actually stop: six levels is past it"
        );
    }

    /// Attribute scanning walks past comments and stops at real code.
    ///
    /// `rust_attribute_paths` was replaceable with an empty vector and with a
    /// fabricated one, and both of its match arms were deletable. It feeds the
    /// runtime-entry-point exemption, so an empty result marks `#[tokio::main]`
    /// as ordinary dead code — the highest-volume Rust false positive the
    /// exemption exists to prevent. Dropping the comment arm breaks the walk at
    /// the first doc comment, which is where attributes usually sit.
    #[test]
    fn attribute_scanning_walks_past_comments_to_reach_the_entry_attribute() {
        let extraction = extract_treesitter(
            "f.rs",
            "rust",
            "#[tokio::main]\n// a comment between the attributes\n             #[allow(dead_code)]\nfn main() {}\n",
        );
        assert!(
            extraction.wiring.iter().any(|annotation| {
                annotation.kind == WiringKind::RuntimeEntryPoint
                    && annotation.target_symbol == "f.rs::main"
            }),
            "`#[tokio::main]` must still be found with a comment and another attribute \
             between it and the function: {:?}",
            extraction.wiring
        );

        // A function with no attributes at all gets no fabricated annotation.
        let plain = extract_treesitter("g.rs", "rust", "fn helper() {}\n");
        assert!(
            plain.wiring.is_empty(),
            "no attribute means no annotation, not an invented one: {:?}",
            plain.wiring
        );
    }

    /// Rust's structural exemptions name their exact reason, and stop at the
    /// right boundary.
    ///
    /// `rust_method_structural_reason` was replaceable with `None`, `Some("")`
    /// and `Some("xyzzy")`, and each of its three match arms was deletable.
    /// It exists because a trait-impl method and a trait method both *reject*
    /// `pub`, so `is_exported == false` carries no information about them —
    /// treating that constant as evidence of deadness is the highest-volume
    /// Rust false positive. `None` brings every one of those back; a constant
    /// string makes all three indistinguishable to whoever reads the reason.
    #[test]
    fn rust_structural_exemptions_name_their_reason_and_stop_at_nested_fns() {
        let extraction = extract_treesitter(
            "f.rs",
            "rust",
            "pub trait Doer {\n    fn declared(&self);\n    fn defaulted(&self) {}\n}\n             pub struct Thing;\nimpl Doer for Thing {\n    fn declared(&self) {\n                     fn nested_helper() {}\n        nested_helper();\n    }\n}\n             impl Thing {\n    pub fn inherent(&self) {}\n}\n",
        );

        let reason_for = |symbol: &str| -> Option<String> {
            extraction
                .wiring
                .iter()
                .find(|annotation| annotation.target_symbol == symbol)
                .map(|annotation| annotation.details.clone())
        };

        // A bare signature and a defaulted method are exempt for the same
        // structural reason, but the wording must not call a declaration a
        // default — the reason is what a human reads before deleting the code.
        assert_eq!(
            reason_for("f.rs::Doer.declared").as_deref(),
            Some("declared trait method; Rust forbids `pub` on trait items")
        );
        assert_eq!(
            reason_for("f.rs::Doer.defaulted").as_deref(),
            Some("defaulted trait method; Rust forbids `pub` on trait items")
        );
        assert_eq!(
            reason_for("f.rs::Thing.declared").as_deref(),
            Some("implements a trait method; Rust forbids `pub` on trait-impl items")
        );

        // An inherent method *can* be `pub`, so its exportedness is real
        // evidence and it must not be exempted.
        assert_eq!(
            reason_for("f.rs::Thing.inherent"),
            None,
            "an inherent impl method can be `pub`, so it gets no structural pass"
        );

        // A `fn` nested inside a trait-impl method is not itself a method: the
        // walk must stop at the enclosing `function_item` rather than inherit
        // the impl block's exemption from farther up the ancestor chain.
        assert_eq!(
            reason_for("f.rs::Thing.nested_helper"),
            None,
            "a nested fn must not inherit its enclosing method's exemption"
        );

        // Every exemption is a StructuralExempt, not a runtime entry point.
        for annotation in &extraction.wiring {
            assert_eq!(annotation.kind, WiringKind::StructuralExempt);
            assert!(!annotation.details.is_empty(), "an exemption must say why");
        }
    }

    /// A Go file records its package clause and its qualified type uses.
    ///
    /// `go_package_name` was replaceable with `None`, `Some("")` and
    /// `Some("xyzzy")`. The package name survives durable persist precisely so
    /// package-level import edges and G20 stars can be rebuilt after
    /// `source_code` is stripped, so losing it silently breaks those edges on
    /// reload rather than at extraction time. The `qualified_type` arm is what
    /// records `pkg.Thing` as a use of `Thing`.
    #[test]
    fn go_records_its_package_and_qualified_type_uses() {
        let extraction = extract_treesitter(
            "f.go",
            "go",
            "package svc\n\ntype Client struct{}\n\n             func (c *Client) Do(x pkg.Thing) error { return nil }\n",
        );

        assert_eq!(
            extraction.go_package.as_deref(),
            Some("svc"),
            "the package clause is the file's package identity"
        );

        let type_refs: Vec<&str> = extraction
            .references
            .iter()
            .filter(|reference| reference.kind == ReferenceKind::Type)
            .map(|reference| reference.name.as_str())
            .collect();
        assert!(
            type_refs.contains(&"Thing"),
            "a qualified type `pkg.Thing` is a use of `Thing`: {type_refs:?}"
        );
        assert!(
            type_refs.contains(&"Client"),
            "the receiver type is a use: {type_refs:?}"
        );

        // A file with no package clause reports none rather than inventing one.
        let bare = extract_treesitter("g.go", "go", "type Loose struct{}\n");
        assert_eq!(bare.go_package, None);
    }

    fn exported_names(source: &str) -> Vec<(String, bool)> {
        extract_treesitter("m.js", "javascript", source)
            .symbols
            .into_iter()
            .filter(|symbol| symbol.kind != SymbolKind::File)
            .map(|symbol| (symbol.name, symbol.is_exported))
            .collect()
    }

    fn is_exported(source: &str, name: &str) -> bool {
        exported_names(source)
            .into_iter()
            .find(|(symbol, _)| symbol == name)
            .unwrap_or_else(|| panic!("{name} must be extracted from: {source}"))
            .1
    }

    /// A member of an object or class *expression* is exported when the value
    /// holding it escapes the module.
    ///
    /// The export check used to look only for a `class_declaration` ancestor
    /// with an `export_statement` parent, so every member of an object literal
    /// or an anonymous class read as private. Nothing in the corpus calls them
    /// — the caller is the bundler, the browser, or the test harness — so each
    /// one became a *confident* dead-code finding on code that is certainly
    /// running. Measured on a first-party web corpus, this shape was
    /// `manualChunks` (Rollup's chunking callback) plus the whole
    /// `ResizeObserver`/`IntersectionObserver` test polyfill.
    #[test]
    fn a_member_of_an_escaping_object_or_class_expression_is_exported() {
        assert!(
            is_exported("export const handlers = { onClick() {} };\n", "onClick"),
            "an exported const's object members are public API"
        );
        assert!(
            is_exported(
                "export default { build: { output: { manualChunks(id) { return id; } } } };\n",
                "manualChunks"
            ),
            "a callback nested in the default export is reached through it"
        );
        for global in ["globalThis", "window", "global", "self"] {
            let source = format!("{global}.ResizeObserver = class {{ observe() {{}} }};\n");
            assert!(
                is_exported(&source, "observe"),
                "assigning to `{global}` publishes the member to the runtime"
            );
        }
    }

    /// A value that does not escape keeps every member detectable.
    ///
    /// This is the half that stops the rule from being a blanket amnesty for
    /// object literals. Losing it would silently disable dead-code detection
    /// for every method-shaped property in JavaScript and TypeScript.
    #[test]
    fn members_of_a_value_that_never_escapes_stay_private() {
        for (source, name) in [
            ("const handlers = { onDead() {} };\n", "onDead"),
            ("class Hidden { neverUsed() {} }\n", "neverUsed"),
            ("const anon = class { alsoDead() {} };\n", "alsoDead"),
            ("let obj = { deepDead() {} };\n", "deepDead"),
            // A bare identifier target is a binding, not the global object.
            ("ResizeObserver = class { observe() {} };\n", "observe"),
            // Returned from a function is not *syntactically* an escape;
            // proving it reaches a caller needs escape analysis this extractor
            // does not do, so it reports what is evident.
            ("export function make() { return { get() {} }; }\n", "get"),
            // A nested declaration does not inherit its enclosing function's
            // export.
            (
                "export function outer() { function inner() {} return inner; }\n",
                "inner",
            ),
        ] {
            assert!(
                !is_exported(source, name),
                "`{name}` does not escape and must stay detectable: {source}"
            );
        }
    }

    /// The ordinary declaration forms are unchanged.
    ///
    /// The walk that follows escapes replaced a hand-rolled per-kind match, and
    /// these are the cases that match had to keep getting right.
    #[test]
    fn ordinary_javascript_export_forms_are_unchanged() {
        assert!(is_exported("export class W { render() {} }\n", "render"));
        assert!(is_exported("export class W { render() {} }\n", "W"));
        assert!(!is_exported("class W { render() {} }\n", "render"));
        assert!(is_exported("export const helper = () => {};\n", "helper"));
        assert!(!is_exported("const helper = () => {};\n", "helper"));
        assert!(is_exported("export function fn() {}\n", "fn"));
        assert!(!is_exported("function fn() {}\n", "fn"));
    }

    /// A Go build constraint is recorded, from either the directive or the name.
    ///
    /// The flag is what lets `analyze_liveness` tell a *spurious* ambiguity
    /// between platform variants of one identity from a genuine one between two
    /// unrelated symbols. Reporting no constraint makes every `_unix.go` pair a
    /// dead-code finding on live code; reporting one everywhere silently
    /// disables detection for the whole language.
    #[test]
    fn a_go_build_constraint_is_recorded_from_the_directive_or_the_filename() {
        let directive = extract_treesitter(
            "procgroup_other.go",
            "go",
            "//go:build !unix\n\npackage store\nfunc configure() {}\n",
        );
        assert!(
            directive.go_build_constrained,
            "`//go:build !unix` constrains the build"
        );

        let legacy = extract_treesitter(
            "old.go",
            "go",
            "// +build linux\n\npackage store\nfunc configure() {}\n",
        );
        assert!(
            legacy.go_build_constrained,
            "the legacy `// +build` form too"
        );

        // `unix` is a legal build *tag* but not a GOOS, so this file is
        // constrained by its directive and never by its name.
        let by_name = extract_treesitter("sock_linux.go", "go", "package net\nfunc opt() {}\n");
        assert!(
            by_name.go_build_constrained,
            "Go applies the `_GOOS` filename constraint with no directive present"
        );
        let arch = extract_treesitter("asm_amd64.go", "go", "package net\nfunc opt() {}\n");
        assert!(arch.go_build_constrained, "`_GOARCH` constrains too");
        let goos_arch =
            extract_treesitter("asm_linux_amd64.go", "go", "package net\nfunc opt() {}\n");
        assert!(goos_arch.go_build_constrained, "`_GOOS_GOARCH` constrains");
        let suffixed_test =
            extract_treesitter("net_linux_test.go", "go", "package net\nfunc opt() {}\n");
        assert!(
            suffixed_test.go_build_constrained,
            "`_test` is stripped first, so the constraint sits one component left"
        );
    }

    /// Nothing that merely resembles a constraint is read as one.
    ///
    /// Each of these was a way for the flag to be true everywhere, which is the
    /// direction that silently disables Go dead-code detection rather than the
    /// one that reports it loudly.
    #[test]
    fn a_go_file_without_a_real_constraint_reports_none() {
        let plain = extract_treesitter("store.go", "go", "package store\nfunc configure() {}\n");
        assert!(
            !plain.go_build_constrained,
            "an ordinary file is unconstrained"
        );

        // The header ends at the package clause; below it, `//go:build` is an
        // ordinary comment. Reading it would let a comment anywhere in a file
        // suppress a real finding.
        let below = extract_treesitter(
            "late.go",
            "go",
            "package store\n\n//go:build unix\nfunc configure() {}\n",
        );
        assert!(
            !below.go_build_constrained,
            "a `//go:build` line below the package clause is a comment, not a constraint"
        );

        for name in [
            "helpers_other.go", // `other` is not a GOOS
            "raw_unix.go",      // `unix` is a build tag, not a GOOS
            "linux.go",         // Go needs something before the suffix
            "config_test.go",   // `_test` alone is not a platform
        ] {
            let extraction = extract_treesitter(name, "go", "package p\nfunc f() {}\n");
            assert!(
                !extraction.go_build_constrained,
                "{name} carries no GOOS/GOARCH suffix and no directive"
            );
        }

        // The flag is Go-only: a Python file named like a Go platform variant
        // must never set it.
        let python = extract_treesitter("sock_linux.py", "python", "def f():\n    pass\n");
        assert!(!python.go_build_constrained, "the flag is Go-only");
    }

    /// `__all__` is read only where it is actually assigned.
    ///
    /// The slice offset past the `__all__` token was mutable from `+` to `-`
    /// and `*`. `__all__` is a module's explicit public API, so a name in it can
    /// never be dead; reading the wrong slice either loses the list (every
    /// exported name becomes deletable) or reads an arbitrary window of source
    /// as export names.
    #[test]
    fn python_all_is_read_only_where_it_is_assigned() {
        let exports = python_all_exports("__all__ = [\"alpha\", 'beta']\n__all__ += [\"gamma\"]\n");
        assert_eq!(
            exports.iter().map(String::as_str).collect::<Vec<_>>(),
            ["alpha", "beta", "gamma"],
            "both the assignment and the accumulation form declare exports, in \
             either quote style"
        );

        // A name that merely contains `__all__` is not an `__all__` assignment.
        assert!(
            python_all_exports("my__all__ = [\"nope\"]\n").is_empty(),
            "a longer identifier must not be read as `__all__`"
        );

        // An `__all__` inside a comment is not a declaration.
        assert!(
            python_all_exports("# __all__ = [\"ignored\"]\n").is_empty(),
            "a commented-out `__all__` must not export"
        );
        assert!(
            python_all_exports("x = 1  # see __all__ = [\"ignored\"]\n").is_empty(),
            "a trailing comment must not export"
        );

        // A real declaration after a decoy on an earlier line is still found,
        // so skipping a decoy must not abandon the scan.
        assert_eq!(
            python_all_exports(
                "my__all__ = [\"nope\"]\n# __all__ = [\"ignored\"]\n__all__ = [\"real\"]\n"
            )
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
            ["real"],
            "decoys are skipped without abandoning the scan"
        );

        // An indented `__all__` (a conditional or class-level export list) is
        // still a declaration: the character before it is whitespace, which is
        // neither an identifier character nor a comment.
        assert_eq!(
            python_all_exports("if TYPE_CHECKING:\n    __all__ = [\"scoped\"]\n")
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["scoped"],
            "an indented `__all__` must still be read"
        );

        // A decoy and the real declaration on the *same* line: the scan must
        // resume just past the decoy token, not overshoot it.
        assert_eq!(
            python_all_exports("mod__all__ = 1; __all__ = [\"real\"]\n")
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["real"],
            "a decoy earlier on the same line must not hide the declaration"
        );

        // No `__all__` at all yields nothing.
        assert!(python_all_exports("def f(): pass\n").is_empty());

        // A truncated list does not panic and does not invent names.
        assert!(python_all_exports("__all__ = [\n").is_empty());
        assert!(python_all_exports("__all__").is_empty());
    }

    /// A function nested inside a method belongs to the method, not the class.
    ///
    /// `enclosing_type_name` walks up for an owning type and must stop at the
    /// first enclosing callable — deleting that arm lets a nested helper
    /// inherit the class from farther up the chain. The qualified name is the
    /// call graph's join key, so `f.py::C.inner` collides with a real method of
    /// `C` named `inner` if one exists, and fails to join with the actual
    /// nested definition either way.
    #[test]
    fn a_function_nested_in_a_method_is_owned_by_the_method() {
        let python = extract_treesitter(
            "f.py",
            "python",
            "class C:\n    def m(self):\n        def inner():\n            pass\n                     return inner\n",
        );
        let names: Vec<&str> = python
            .symbols
            .iter()
            .map(|symbol| symbol.qualified_name.as_str())
            .collect();
        assert_eq!(
            names,
            ["f.py", "f.py::C", "f.py::C.m", "f.py::C.m.inner"],
            "the nested function is qualified through its enclosing method"
        );

        let typescript = extract_treesitter(
            "f.ts",
            "typescript",
            "class B {\n  go() {\n    function helper() {}\n    return helper;\n  }\n}\n",
        );
        let ts_names: Vec<&str> = typescript
            .symbols
            .iter()
            .map(|symbol| symbol.qualified_name.as_str())
            .collect();
        assert_eq!(
            ts_names,
            ["f.ts", "f.ts::B", "f.ts::B.go", "f.ts::B.go.helper"],
            "the same rule holds for a function nested in a class method"
        );
    }

    /// A constructed value binds its local name to its type, in every grammar.
    ///
    /// `assignment_binding` was replaceable with `None`, `Some("")` and
    /// `Some("xyzzy")`, and each of its three arms was deletable. It is what
    /// records `worker = Worker()` so that a later `worker.go()` knows what
    /// `worker` is — without it a receiver has a name and no type, so receiver
    /// resolution has nothing to look up and the call resolves to nothing
    /// rather than resolving uncertainly (SC12).
    ///
    /// Writing this found the Rust arm passing a literal `None` where the
    /// Python, TypeScript and Go arms all passed the binding, so Rust values
    /// constructed with `let` were the one case with no type behind the
    /// receiver. All four are asserted together here so the arms cannot drift
    /// apart again.
    #[test]
    fn a_constructed_value_binds_its_local_name_in_every_grammar() {
        for (path, grammar, source, constructor) in [
            (
                "f.py",
                "python",
                "def run():\n    worker = Worker()\n    worker.go()\n",
                "Worker",
            ),
            (
                "f.js",
                "javascript",
                "function run() {\n  const worker = new Worker();\n  worker.go();\n}\n",
                "Worker",
            ),
            (
                "f.ts",
                "typescript",
                "function run() {\n  let worker = new Worker();\n  worker.go();\n}\n",
                "Worker",
            ),
            (
                "f.rs",
                "rust",
                "fn run() {\n    let worker = Worker::new();\n    worker.go();\n}\n",
                // SC26 split `Worker::new` into callee `new` on receiver
                // `Worker`. The binding this test guards is unchanged, and
                // dispatch demonstrably still works — see
                // `rust_associated_function_calls_resolve_and_keep_receiver_typing`
                // in devmap-resolve, which asserts both `Worker.go` and the
                // newly recovered `Worker.new` edge.
                "new",
            ),
            (
                "f.go",
                "go",
                "package p\nfunc run() {\n\tworker := NewWorker()\n\tworker.Go()\n}\n",
                "NewWorker",
            ),
        ] {
            let extraction = extract_treesitter(path, grammar, source);
            let bound = extraction
                .references
                .iter()
                .find(|reference| reference.name == constructor)
                .unwrap_or_else(|| {
                    panic!(
                        "{grammar}: no reference named {constructor}: {:?}",
                        extraction
                            .references
                            .iter()
                            .map(|r| &r.name)
                            .collect::<Vec<_>>()
                    )
                });
            assert_eq!(
                bound.assigned_to.as_deref(),
                Some("worker"),
                "{grammar}: the constructed value must bind to the name it is assigned to"
            );

            // And the later method call carries that same name as its receiver,
            // which is the join the binding exists to enable.
            assert!(
                extraction
                    .calls
                    .iter()
                    .any(|call| call.receiver_expr.as_deref() == Some("worker")),
                "{grammar}: the method call must name `worker` as its receiver: {:?}",
                extraction
                    .calls
                    .iter()
                    .map(|c| (&c.callee_name, &c.receiver_expr))
                    .collect::<Vec<_>>()
            );
        }
    }

    /// A Rust tuple-struct constructor binds its local name to the struct.
    ///
    /// This is the shape that makes the Rust arm's binding usable downstream:
    /// the callee is the bare type name `Worker`, which the resolver can find
    /// as a `Struct`. Measured end to end, it upgrades `w.go()` from a
    /// speculative 0.2 name guess among every `go` in the corpus to a
    /// deterministic 1.0 resolution.
    ///
    /// The associated-function form `Worker::new()` still does **not** bind:
    /// its callee name is a scoped path that matches no symbol. That gap is
    /// real and recorded in STATUS.md rather than papered over here.
    #[test]
    fn a_rust_tuple_struct_constructor_binds_its_local_name() {
        let extraction = extract_treesitter(
            "f.rs",
            "rust",
            "pub struct Worker(u32);\nimpl Worker { pub fn go(&self) {} }\n             fn run() {\n    let w = Worker(1);\n    w.go();\n}\n",
        );
        let constructor = extraction
            .references
            .iter()
            .find(|reference| reference.name == "Worker" && reference.kind == ReferenceKind::Call)
            .expect("the tuple-struct constructor is a call");
        assert_eq!(
            constructor.assigned_to.as_deref(),
            Some("w"),
            "the constructed struct must bind the local name it is assigned to"
        );
    }

    /// A declared parameter type binds the parameter name, and `_` binds nothing.
    ///
    /// `param_type_bindings` is the other half of SC12: for a function that
    /// operates on a value it did not construct, the declared type is the only
    /// binding available. The Go arm was deletable and both emptiness guards
    /// were collapsible, which would bind the throwaway name `_` to a type and
    /// let every `_` parameter in the package collide on one binding.
    #[test]
    fn a_declared_parameter_type_binds_its_name_but_never_underscore() {
        let go = extract_treesitter(
            "f.go",
            "go",
            "package p\nfunc run(g *English, n int) {\n\tg.Greet()\n}\n",
        );
        let go_bindings: Vec<(&str, Option<&str>)> = go
            .references
            .iter()
            .filter(|reference| reference.kind == ReferenceKind::Type)
            .map(|reference| (reference.name.as_str(), reference.assigned_to.as_deref()))
            .collect();
        assert!(
            go_bindings.contains(&("English", Some("g"))),
            "a pointer parameter type binds its name: {go_bindings:?}"
        );
        assert!(
            go_bindings.contains(&("int", Some("n"))),
            "a plain parameter type binds its name too: {go_bindings:?}"
        );

        let rust = extract_treesitter("f.rs", "rust", "fn run(g: &English) {\n    g.greet();\n}\n");
        assert!(
            rust.references.iter().any(|reference| {
                reference.name == "English" && reference.assigned_to.as_deref() == Some("g")
            }),
            "a Rust reference parameter binds through its type: {:?}",
            rust.references
                .iter()
                .map(|r| (&r.name, &r.assigned_to))
                .collect::<Vec<_>>()
        );

        // A multi-target assignment binds nothing: `value, err := New()` does
        // not say here which target receives the constructed value, and a wrong
        // binding is worse than an absent one.
        let multi = extract_treesitter(
            "m.go",
            "go",
            "package p\nfunc run() {\n\tvalue, err := NewWorker()\n\t_ = err\n}\n",
        );
        let bound = multi
            .references
            .iter()
            .find(|reference| reference.name == "NewWorker")
            .expect("the constructor is still a call");
        assert_eq!(
            bound.assigned_to, None,
            "a two-target `:=` must not guess which name receives the value"
        );

        // `_` is a throwaway, not a name worth binding.
        let anonymous = extract_treesitter("g.go", "go", "package p\nfunc run(_ *English) {}\n");
        assert!(
            anonymous
                .references
                .iter()
                .all(|reference| reference.assigned_to.is_none()),
            "`_` must not be bound to a type: {:?}",
            anonymous
                .references
                .iter()
                .map(|r| (&r.name, &r.assigned_to))
                .collect::<Vec<_>>()
        );
        let rust_anonymous = extract_treesitter("g.rs", "rust", "fn run(_: &English) {}\n");
        assert!(
            rust_anonymous
                .references
                .iter()
                .all(|reference| reference.assigned_to.is_none()),
            "the same holds in Rust"
        );
    }

    /// Every JS/TS declaration form reports its exportedness, both ways.
    ///
    /// Five of the six forms had inlined their own copy of the export test and
    /// each copy was independently mutable — inverting one marks that whole
    /// *kind* of declaration unexported while the rest stay correct, which
    /// reads as "this enum is private and unused" rather than as a bug.
    /// `is_exported` is direct evidence for dead code, so a wrong answer here
    /// produces confident false positives; in the other direction it hides real
    /// ones. The copies are now one helper, and this covers every form through
    /// it with a matching unexported control so neither direction can be
    /// satisfied by a constant.
    #[test]
    fn every_js_declaration_form_reports_its_exportedness() {
        let extraction = extract_treesitter(
            "f.ts",
            "typescript",
            "export const arrow = () => {};\nconst plain = () => {};\n\
             export class Klass { m() {} }\nclass Priv { p() {} }\n\
             export enum Color { A }\nenum Hidden { B }\n\
             export interface Shape { x: number }\ninterface Inner { y: number }\n\
             export type Alias = string;\ntype Local = number;\n\
             export namespace Outer { export const q = 1; }\nnamespace Quiet { }\n",
        );

        let exported_of = |qualified: &str| -> bool {
            extraction
                .symbols
                .iter()
                .find(|symbol| symbol.qualified_name == qualified)
                .unwrap_or_else(|| {
                    panic!(
                        "{qualified} was not extracted at all; got {:?}",
                        extraction
                            .symbols
                            .iter()
                            .map(|s| &s.qualified_name)
                            .collect::<Vec<_>>()
                    )
                })
                .is_exported
        };

        // Each pair is (exported form, unexported form of the same kind), so a
        // constant `true` and a constant `false` both fail.
        for (exported, private) in [
            // `export const f = () => {}` — the declarator's owner is the
            // lexical_declaration, not the declarator itself.
            ("f.ts::arrow", "f.ts::plain"),
            ("f.ts::Klass", "f.ts::Priv"),
            // A method is exported exactly when its class is: its own parent is
            // always `class_body`, which is never an export statement.
            ("f.ts::Klass.m", "f.ts::Priv.p"),
            ("f.ts::Color", "f.ts::Hidden"),
            ("f.ts::Shape", "f.ts::Inner"),
            ("f.ts::Alias", "f.ts::Local"),
            ("f.ts::Outer", "f.ts::Quiet"),
        ] {
            assert!(
                exported_of(exported),
                "{exported} is exported and must report so"
            );
            assert!(
                !exported_of(private),
                "{private} is not exported and must not report so"
            );
        }
    }

    /// A re-export names its source module; a local re-export names none.
    ///
    /// The brace slice and the `!mod_spec.is_empty()` guard were both mutable.
    /// `export { x }` and `export { x } from './m'` are different facts — the
    /// first republishes a local symbol, the second forwards another module's —
    /// and giving the local form an empty module specifier makes the resolver
    /// look for a module named `""`, so the export binds to nothing.
    #[test]
    fn re_exports_carry_their_source_module_only_when_they_have_one() {
        let extraction = extract_treesitter(
            "f.ts",
            "typescript",
            "export { a, b as c } from './m';\nexport { local };\n",
        );

        let export = |name: &str| {
            extraction
                .exports
                .iter()
                .find(|export| export.exported_name == name)
                .unwrap_or_else(|| panic!("no export named {name}"))
        };

        // `b as c` publishes `c` and points at the local name `b`; getting the
        // brace slice wrong loses the rename and publishes the wrong name.
        assert_eq!(export("a").local_name.as_deref(), Some("a"));
        assert_eq!(export("c").local_name.as_deref(), Some("b"));
        assert_eq!(export("a").module_specifier.as_deref(), Some("./m"));
        assert_eq!(export("c").module_specifier.as_deref(), Some("./m"));

        // A local re-export forwards nothing, so it must carry no module at all
        // rather than an empty one.
        assert_eq!(
            export("local").module_specifier,
            None,
            "a local re-export must have no module specifier, not an empty one"
        );

        // The brace contents are parsed as bindings, aligned and in order.
        let braced = extraction
            .imports
            .iter()
            .find(|import| import.module_specifier == "./m")
            .expect("the re-export is recorded as an import of `./m`");
        assert_eq!(braced.imported_names, ["a", "b"]);
        assert_eq!(braced.local_names, ["a", "c"]);
    }

    /// A default import takes an alias; a braced import does not.
    ///
    /// The `!candidate.is_empty() && candidate.chars().all(..)` guard decides
    /// whether the text before `from` is a single default binding. Collapsed to
    /// `||`, the braced form `{ x }` passes and becomes the module's alias, so
    /// every later `{` -prefixed reference resolves through a binding named
    /// `{ x }` that no symbol has.
    #[test]
    fn only_a_bare_default_import_becomes_an_alias() {
        let extraction = extract_treesitter(
            "f.ts",
            "typescript",
            "import def from 'pkg';\nimport { x } from 'q';\n",
        );

        let import = |module: &str| {
            extraction
                .imports
                .iter()
                .find(|import| import.module_specifier == module)
                .unwrap_or_else(|| panic!("no import of {module}"))
        };
        assert_eq!(
            import("pkg").alias.as_deref(),
            Some("def"),
            "a bare default import binds its name as the alias"
        );
        assert_eq!(
            import("q").alias,
            None,
            "a braced import list is not an alias"
        );
    }

    /// A React lifecycle hook is an entry point only as a class method.
    ///
    /// `kind == "method_definition"` was invertible. The annotation says "the
    /// renderer calls this, so never report it dead"; applied to a plain
    /// function that merely shares the name, it suppresses a real dead-code
    /// finding, and withheld from the actual method it produces a confident
    /// false positive on every component in the tree.
    #[test]
    fn a_lifecycle_hook_is_an_entry_point_only_as_a_method() {
        let extraction = extract_treesitter(
            "f.jsx",
            "javascript",
            "class Comp {\n  componentDidMount() {}\n}\nfunction componentDidMount() {}\n",
        );
        let targets: Vec<&str> = extraction
            .wiring
            .iter()
            .filter(|annotation| annotation.kind == WiringKind::RuntimeEntryPoint)
            .map(|annotation| annotation.target_symbol.as_str())
            .collect();
        assert_eq!(
            targets,
            ["f.jsx::Comp.componentDidMount"],
            "only the class method is a renderer entry point"
        );
    }

    /// A named Go receiver binds its type; an anonymous one binds nothing.
    ///
    /// The `!recv_name.is_empty()` guard was deletable, which inverts SC9's
    /// method-scoped receiver binding: named receivers stop binding and only
    /// anonymous ones would, so `c.help()` inside `func (c *Client) Do()` loses
    /// the type of `c` and the intra-type call edge disappears.
    #[test]
    fn a_named_go_receiver_binds_its_type_and_an_anonymous_one_does_not() {
        let extraction = extract_treesitter(
            "f.go",
            "go",
            "package p\ntype Client struct{}\nfunc (c *Client) Do() { c.help() }\n             func (*Client) Anon() {}\nfunc (c *Client) help() {}\n             func mk() *Client { return &Client{} }\n",
        );

        // The binding is scoped to the method it was declared in (SC9), not to
        // the file, so two methods each get their own.
        assert!(
            extraction.references.iter().any(|reference| {
                reference.name == "Client"
                    && reference.assigned_to.as_deref() == Some("c")
                    && reference.enclosing_symbol.as_deref() == Some("f.go::Client.Do")
            }),
            "a named receiver binds its type within its own method: {:?}",
            extraction
                .references
                .iter()
                .map(|r| (&r.name, &r.assigned_to, &r.enclosing_symbol))
                .collect::<Vec<_>>()
        );

        // An anonymous receiver has no name to bind.
        assert!(
            extraction.references.iter().all(|reference| {
                reference.enclosing_symbol.as_deref() != Some("f.go::Client.Anon")
                    || reference.assigned_to.is_none()
            }),
            "an anonymous receiver must bind nothing"
        );

        // A composite literal resolves its type through the child scan, so
        // `&Client{}` is a construction of `Client`.
        assert!(
            extraction.references.iter().any(|reference| {
                reference.name == "Client" && reference.kind == ReferenceKind::Constructor
            }),
            "a composite literal is a constructor reference to its type: {:?}",
            extraction
                .references
                .iter()
                .map(|r| (&r.name, r.kind))
                .collect::<Vec<_>>()
        );
    }

    /// Only a bare identifier before `from` is a default binding.
    ///
    /// The guard sits on the *brace-less* import path, so the discriminating
    /// shape is a modified default import: `import type Foo from 'm'` has no
    /// braces, and its pre-`from` text is two words. Collapsed to `||`, that
    /// whole string becomes the module's alias and every reference resolved
    /// through it looks for a binding named `type Foo` that no symbol has.
    #[test]
    fn a_multi_word_import_clause_is_not_an_alias() {
        let extraction = extract_treesitter(
            "f.ts",
            "typescript",
            "import type Foo from 'm';\nimport def from 'pkg';\n",
        );
        let alias_of = |module: &str| {
            extraction
                .imports
                .iter()
                .find(|import| import.module_specifier == module)
                .unwrap_or_else(|| panic!("no import of {module}"))
                .alias
                .clone()
        };
        assert_eq!(
            alias_of("m"),
            None,
            "`type Foo` is not a single default binding and must not become an alias"
        );
        assert_eq!(
            alias_of("pkg").as_deref(),
            Some("def"),
            "positive control: a bare default import still aliases"
        );
    }

    /// The braced binding list starts *after* the brace.
    ///
    /// `text[idx1 + 1..idx2]` was mutable to `idx1 - 1`, which pulls in the
    /// character before the brace. Every spaced form survives it because the
    /// binding parser trims whitespace and braces, so the discriminating shape
    /// is the unspaced one a minifier emits: with `- 1` the slice starts inside
    /// `export`, and the imported name comes out as `t{a` rather than `a`.
    ///
    /// (`idx1 * 1` is an equivalent mutant: it yields `idx1`, and the leading
    /// brace is trimmed by the binding parser, so the result is unchanged.)
    #[test]
    fn a_braced_binding_list_starts_after_its_brace() {
        let extraction = extract_treesitter("g.ts", "typescript", "export{a}from'./m';\n");
        let braced = extraction
            .imports
            .iter()
            .find(|import| import.module_specifier == "./m")
            .expect("the unspaced re-export is still recorded");
        assert_eq!(
            braced.imported_names,
            ["a"],
            "the binding is `a`, not the text around the brace"
        );
        assert_eq!(braced.local_names, ["a"]);
        assert!(
            extraction
                .exports
                .iter()
                .any(|export| export.exported_name == "a"),
            "and it is published under that same name: {:?}",
            extraction
                .exports
                .iter()
                .map(|e| &e.exported_name)
                .collect::<Vec<_>>()
        );
    }

    /// A Rust binary entry point is a free `main` in a file that declares one.
    ///
    /// All three conjuncts were mutable. The annotation means "the toolchain
    /// calls this, so it can never be dead"; widened, it exempts every function
    /// in `main.rs` and every method named `main`, silently suppressing real
    /// dead-code findings across the whole binary crate. Narrowed, `main`
    /// itself is reported dead, which is the most conspicuous false positive a
    /// Rust dead-code pass can produce.
    #[test]
    fn a_rust_binary_entry_point_is_a_free_main_in_an_entry_file() {
        let entry_points = |path: &str, source: &str| -> Vec<String> {
            extract_treesitter(path, "rust", source)
                .wiring
                .iter()
                .filter(|annotation| annotation.kind == WiringKind::RuntimeEntryPoint)
                .map(|annotation| annotation.target_symbol.clone())
                .collect()
        };

        // `main.rs` declares an entry point, but only the free `main` is one:
        // not a sibling function, and not a method that shares the name.
        assert_eq!(
            entry_points(
                "src/main.rs",
                "fn main() {}\nfn helper() {}\nstruct S;\nimpl S { fn main(&self) {} }\n",
            ),
            ["src/main.rs::main"],
            "only the free `main` in an entry file is a toolchain entry point"
        );

        // `bin/` files declare entry points too.
        assert_eq!(
            entry_points("src/bin/tool.rs", "fn main() {}\n"),
            ["src/bin/tool.rs::main"]
        );

        // A `main` in a file that declares no entry point is an ordinary
        // function and a real dead-code candidate.
        assert!(
            entry_points("src/lib.rs", "fn main() {}\n").is_empty(),
            "`main` in a library file is not an entry point"
        );
        assert!(entry_points("src/other.rs", "fn main() {}\n").is_empty());
    }

    /// A function expression is named by the binding it is assigned to.
    ///
    /// `callable_binding_name` decides what a call inside an arrow or function
    /// expression is attributed to. Losing it leaves the call attributed to an
    /// enclosing symbol or to nothing, so `f` never reaches `helper` — the
    /// unjoinable-edge shape behind SC9/SC10, where the edge exists but names
    /// something no node has.
    #[test]
    fn a_function_expression_is_named_by_its_binding() {
        let extraction =
            extract_treesitter("f.js", "javascript", "const f = () => { helper(); };\n");
        let call = extraction
            .calls
            .iter()
            .find(|call| call.callee_name == "helper")
            .expect("the call inside the arrow is recorded");
        assert_eq!(
            call.caller_symbol.as_deref(),
            Some("f.js::f"),
            "the call belongs to the binding `f`, which is the symbol that exists"
        );
        assert!(
            extraction
                .symbols
                .iter()
                .any(|symbol| symbol.qualified_name == "f.js::f"),
            "and that symbol is emitted, so the edge joins"
        );
    }

    /// A Go method that satisfies an interface is annotated as exempt.
    ///
    /// The whole `go_interface_method_exemptions` body was replaceable with
    /// nothing. It is the wiring half of the interface-satisfaction join: the
    /// pure matcher in `model.rs` is tested there, but nothing asserted that
    /// its results were ever turned into annotations. Without them, every method
    /// reached only through an interface reads as dead.
    #[test]
    fn a_go_method_satisfying_an_interface_is_annotated_exempt() {
        let extraction = extract_treesitter(
            "f.go",
            "go",
            "package p\ntype Runner interface { Run() error }\ntype Impl struct{}\n             func (i *Impl) Run() error { return nil }\n",
        );
        let exemption = extraction
            .wiring
            .iter()
            .find(|annotation| annotation.target_symbol == "f.go::Impl.Run")
            .unwrap_or_else(|| {
                panic!(
                    "Impl.Run satisfies Runner and must be exempt: {:?}",
                    extraction.wiring
                )
            });
        assert_eq!(exemption.kind, WiringKind::StructuralExempt);
        assert!(
            exemption.details.contains("Runner"),
            "the reason must name the interface: {}",
            exemption.details
        );
    }

    /// An imported name is bound once, not counted again as a use.
    ///
    /// `is_inside_import_or_export` suppresses the identifier *inside* the
    /// import specifier, which is a binding site rather than a use. Forced to
    /// `false`, every import contributes a phantom reference to the symbol it
    /// imports, so an imported-but-never-used symbol looks used and its real
    /// dead-code finding disappears.
    #[test]
    fn an_import_specifier_is_a_binding_not_a_use() {
        let extraction = extract_treesitter(
            "f.ts",
            "typescript",
            "import { x } from 'm';\nexport function run() { return x; }\n",
        );
        let uses: Vec<&str> = extraction
            .references
            .iter()
            .filter(|reference| reference.name == "x")
            .map(|reference| reference.enclosing_symbol.as_deref().unwrap_or("<file>"))
            .collect();
        assert_eq!(
            uses,
            ["f.ts::run"],
            "`x` is used exactly once, inside `run`; the import specifier is not a use"
        );
    }

    /// A local variable shadows the name for its own scope only.
    ///
    /// Three guards cooperate here. `is_callable_node` bounds the scope: forced
    /// to `false` the scope becomes the whole file, so one function's local
    /// silently suppresses another function's genuine use of that name.
    /// `is_symbol_binding` keeps a named function binding out of the local set,
    /// so `const f = () => {}` stays a symbol and references to it survive.
    /// `is_binding_wrapper` lets the defining-name walk climb through the list
    /// and pattern wrappers that Go and Python put around assignment targets.
    #[test]
    fn a_local_shadows_its_own_scope_only() {
        // Scope is the function, not the file.
        let cross = extract_treesitter(
            "f.js",
            "javascript",
            "function a() { const shared = 1; return shared; }\n             function b() { return shared; }\n",
        );
        let shared_uses: Vec<&str> = cross
            .references
            .iter()
            .filter(|reference| reference.name == "shared")
            .map(|reference| reference.enclosing_symbol.as_deref().unwrap_or("<file>"))
            .collect();
        assert_eq!(
            shared_uses,
            ["f.js::b"],
            "`shared` is a local in `a` and a real use in `b`; a file-wide scope \
             would suppress both"
        );

        // A function binding is a symbol, not a shadowing local.
        let binding = extract_treesitter(
            "g.js",
            "javascript",
            "function run() { const f = () => {}; return f; }\n",
        );
        assert!(
            binding
                .symbols
                .iter()
                .any(|symbol| symbol.qualified_name == "g.js::run.f"),
            "the arrow binding is emitted as a symbol"
        );
        assert!(
            binding
                .references
                .iter()
                .any(|reference| reference.name == "f" && reference.kind == ReferenceKind::Name),
            "and referring to it is a use, not a shadowed local: {:?}",
            binding
                .references
                .iter()
                .map(|r| (&r.name, r.kind))
                .collect::<Vec<_>>()
        );

        // Assignment targets wrapped in a list are still binding sites.
        let go = extract_treesitter(
            "f.go",
            "go",
            "package p\nfunc run() {\n\tvar a, b int\n\ta, b = one(), two()\n\t             _ = a\n\t_ = b\n}\n",
        );
        assert!(
            !go.references
                .iter()
                .any(|reference| reference.kind == ReferenceKind::Name
                    && (reference.name == "a" || reference.name == "b")),
            "a multi-target assignment binds its names rather than using them: {:?}",
            go.references
                .iter()
                .map(|r| (&r.name, r.kind))
                .collect::<Vec<_>>()
        );

        // A Python augmented-assignment target is a binding too.
        let python = extract_treesitter(
            "f.py",
            "python",
            "def run():\n    total = 0\n    total += step\n    return total\n",
        );
        let python_names: Vec<&str> = python
            .references
            .iter()
            .map(|reference| reference.name.as_str())
            .collect();
        assert_eq!(
            python_names,
            ["step"],
            "only the value read is a use; the accumulator is a binding"
        );
    }

    /// The scope-local cache is cleared between files.
    ///
    /// `reset_scope_locals` was replaceable with nothing. Its keys are
    /// `Node::id()` — arena addresses that are reused after a tree is dropped —
    /// so a stale entry can be served for an unrelated scope in the next file,
    /// suppressing that file's genuine references. Address reuse is
    /// nondeterministic, so this asserts the contract directly rather than
    /// hoping to observe a collision.
    #[test]
    fn the_scope_local_cache_is_cleared_between_files() {
        // Extraction populates the cache: this source has a local that shadows,
        // which is what forces a scope's locals to be collected.
        extract_treesitter(
            "f.js",
            "javascript",
            "function run() { const helper = 1; return helper; }\n",
        );
        assert!(
            scope_locals_len() > 0,
            "the fixture must actually populate the cache, or this test is vacuous"
        );

        reset_scope_locals();
        assert_eq!(
            scope_locals_len(),
            0,
            "the cache must be empty before the next file is extracted"
        );

        // And extraction itself performs the clear, so two files in sequence
        // cannot leak into one another.
        extract_treesitter(
            "f.js",
            "javascript",
            "function run() { const helper = 1; return helper; }\n",
        );
        let after_first = scope_locals_len();
        extract_treesitter("g.js", "javascript", "function other() { return 1; }\n");
        assert!(
            scope_locals_len() <= after_first,
            "extracting a second file must start from a cleared cache, not accumulate"
        );
    }

    /// Alias and parameter binding sites are definitions, not uses.
    ///
    /// `is_defining_name` tests four fields, and each disjunction was mutable.
    /// The `alias` field covers `with … as handle` and `except … as e`; the
    /// `parameter` field covers an unparenthesised arrow parameter and a catch
    /// clause. Losing either turns the binding site into a *use* of whatever
    /// module-level symbol shares that name, which both inflates that symbol's
    /// reference count and hides its real dead-code finding.
    #[test]
    fn alias_and_parameter_binding_sites_are_not_uses() {
        // `parameter` field: `x` binds, `offset` is the only real use.
        let arrow = extract_treesitter("f.js", "javascript", "const f = x => x + offset;\n");
        assert_eq!(
            arrow
                .references
                .iter()
                .map(|reference| reference.name.as_str())
                .collect::<Vec<_>>(),
            ["offset"],
            "an arrow parameter is a binding, not a use"
        );

        // `alias` field: `handle` binds and is then a local, so neither the
        // binding nor the later mention is a module-level use.
        let with_stmt = extract_treesitter(
            "f.py",
            "python",
            "def run():\n    with open(path) as handle:\n        return handle\n",
        );
        let with_names: Vec<&str> = with_stmt
            .references
            .iter()
            .map(|reference| reference.name.as_str())
            .collect();
        assert!(
            !with_names.contains(&"handle"),
            "`as handle` is a binding site: {with_names:?}"
        );
        assert!(
            with_names.contains(&"open") && with_names.contains(&"path"),
            "positive control: the call and its argument are still uses: {with_names:?}"
        );

        // The same field carries `except … as e`.
        let except = extract_treesitter(
            "g.py",
            "python",
            "def run():\n    try:\n        go()\n    except Err as e:\n        return e\n",
        );
        let except_names: Vec<&str> = except
            .references
            .iter()
            .map(|reference| reference.name.as_str())
            .collect();
        assert!(
            !except_names.contains(&"e"),
            "`as e` is a binding site: {except_names:?}"
        );
        assert!(
            except_names.contains(&"Err"),
            "positive control: the exception type is a use: {except_names:?}"
        );
    }

    /// Names inside a Rust `use` list are imports, not uses.
    ///
    /// `is_inside_import_or_export` is what keeps a `use` list from counting as
    /// a reference to everything it names. Forced to `false`, every imported
    /// name gains a phantom use, so an item that is imported and never called
    /// looks live and its dead-code finding disappears. A Rust `use` list is the
    /// clean case: unlike a TS import specifier, its names are not also caught
    /// by the defining-name check ahead of it.
    #[test]
    fn names_in_a_rust_use_list_are_not_uses() {
        let extraction = extract_treesitter(
            "f.rs",
            "rust",
            "use crate::thing::{One, Two};\nfn run() -> One { One }\n",
        );
        let two_uses = extraction
            .references
            .iter()
            .filter(|reference| reference.name == "Two")
            .count();
        assert_eq!(
            two_uses,
            0,
            "`Two` is only imported, never used, and must have no reference: {:?}",
            extraction
                .references
                .iter()
                .map(|r| (&r.name, r.kind))
                .collect::<Vec<_>>()
        );
        assert!(
            extraction
                .references
                .iter()
                .any(|reference| reference.name == "One"),
            "positive control: `One` is genuinely used and does have references"
        );
    }

    /// The defining-name walk climbs wrappers but still stops.
    ///
    /// `is_binding_wrapper` forced to `true` makes the walk climb out of every
    /// expression until it finds *some* ancestor that looks like a binding, so
    /// ordinary uses are reclassified as definitions and disappear. Measured on
    /// a five-language corpus that mutation silently drops 86 edges, among them
    /// real `References` between methods of the same class. The shape below is
    /// the minimal reproduction: attribute uses and parameter occurrences stop
    /// being recorded, so a method reached only through `self` loses its
    /// incoming edge and moves toward `dead`.
    #[test]
    fn the_defining_name_walk_climbs_wrappers_but_still_stops() {
        let extraction = extract_treesitter(
            "f.py",
            "python",
            "class C:\n    def client(self):\n        return 1\n    def reset(self):\n                     self._c = self.client\n",
        );
        let mut names: Vec<&str> = extraction
            .references
            .iter()
            .map(|reference| reference.name.as_str())
            .collect();
        names.sort_unstable();
        assert_eq!(
            names,
            ["_c", "client", "self", "self", "self", "self"],
            "every `self` occurrence and both attribute names are uses; a walk that \
             never stops reclassifies them as definitions and drops them"
        );
    }

    /// Exported module-level bindings are symbols; private and local ones are not.
    ///
    /// An exported constant is public API surface. Without a symbol it is
    /// invisible to the map: unsearchable, unreferenceable, and neither
    /// confirmable as live nor reportable as dead. The frozen Python baseline
    /// emits these (misclassified as `function`); devmap records them as
    /// `Variable`. Scope is deliberate — private and function-local bindings
    /// stay out, since their only graph effect would be dead-code noise.
    #[test]
    fn exported_module_level_bindings_are_symbols() {
        let rust = extract_treesitter(
            "a.rs",
            "rust",
            "pub const LIMIT: u32 = 10;\nstatic PRIV: u32 = 1;\npub static G: u32 = 2;\n             fn f() { const INNER: u32 = 3; }\n",
        );
        let rust_names: Vec<&str> = rust
            .symbols
            .iter()
            .filter(|symbol| symbol.kind == SymbolKind::Variable)
            .map(|symbol| symbol.name.as_str())
            .collect();
        assert_eq!(
            rust_names,
            ["LIMIT", "G"],
            "only `pub` module-level bindings; not private, not function-local"
        );

        let go = extract_treesitter(
            "b.go",
            "go",
            "package p\nconst Limit = 10\nvar Global = 1\nconst lower = 2\n             func f() { const Inner = 4 }\n",
        );
        let go_names: Vec<&str> = go
            .symbols
            .iter()
            .filter(|symbol| symbol.kind == SymbolKind::Variable)
            .map(|symbol| symbol.name.as_str())
            .collect();
        assert_eq!(
            go_names,
            ["Limit", "Global"],
            "Go exports by capitalisation; `lower` and the function-local are out"
        );

        let typescript = extract_treesitter(
            "c.ts",
            "typescript",
            "export const VALUE = 42;\nconst local = 1;\nexport const fn = () => {};\n",
        );
        let ts_variables: Vec<&str> = typescript
            .symbols
            .iter()
            .filter(|symbol| symbol.kind == SymbolKind::Variable)
            .map(|symbol| symbol.name.as_str())
            .collect();
        assert_eq!(
            ts_variables,
            ["VALUE"],
            "only the exported non-function const"
        );
        assert!(
            typescript
                .symbols
                .iter()
                .any(|symbol| symbol.name == "fn" && symbol.kind == SymbolKind::Function),
            "an exported arrow is still a Function, not a Variable"
        );
    }

    /// Terraform blocks are symbols addressed the way Terraform addresses them.
    #[test]
    fn terraform_blocks_are_symbols_with_terraform_addresses() {
        let extraction = extract_treesitter(
            "main.tf",
            "hcl",
            "resource \"aws_s3_bucket\" \"b\" {\n  bucket = \"x\"\n               lifecycle {\n    prevent_destroy = true\n  }\n}\n\n             variable \"region\" {\n  type = string\n}\n\n             module \"vpc\" {\n  source = \"./vpc\"\n}\n",
        );
        let rows: Vec<String> = extraction
            .symbols
            .iter()
            .filter(|symbol| symbol.kind != SymbolKind::File)
            .map(|symbol| format!("{:?}:{}", symbol.kind, symbol.name))
            .collect();
        assert_eq!(
            rows,
            [
                "Module:resource.aws_s3_bucket.b",
                "Variable:variable.region",
                "Module:module.vpc",
            ],
            "a nested `lifecycle` block is an attribute of its resource, not an \
             addressable unit, and must not be emitted"
        );
    }

    /// A Python module constant is public exactly when `__all__` says so.
    ///
    /// Python has no export keyword, so `__all__` is the only declaration of
    /// public surface the language offers. Emitting every module-level binding
    /// would add a symbol for every private constant in a repository, whose
    /// only graph effect is dead-code noise; emitting none leaves genuinely
    /// public constants invisible. Membership is the principled line.
    #[test]
    fn a_python_module_constant_is_public_exactly_when_all_declares_it() {
        let declared = extract_treesitter(
            "m.py",
            "python",
            "__all__ = [\"CONST\", \"Widget\"]\nCONST = 1\nOTHER = 2\n_priv = 3\n             class Widget:\n    ATTR = 5\ndef f():\n    LOCAL = 6\n",
        );
        let variables: Vec<(&str, bool)> = declared
            .symbols
            .iter()
            .filter(|symbol| symbol.kind == SymbolKind::Variable)
            .map(|symbol| (symbol.name.as_str(), symbol.is_exported))
            .collect();
        assert_eq!(
            variables,
            [("CONST", true)],
            "only the `__all__` member is a symbol: `OTHER` and `_priv` are private, \
             `ATTR` is a class attribute and `LOCAL` is a function local"
        );

        // A module with no `__all__` declares no public surface, so it
        // contributes no constants rather than all of them.
        let undeclared = extract_treesitter("n.py", "python", "CONST = 1\nOTHER = 2\n");
        assert!(
            !undeclared
                .symbols
                .iter()
                .any(|symbol| symbol.kind == SymbolKind::Variable),
            "no `__all__` means no declared constants: {:?}",
            undeclared
                .symbols
                .iter()
                .map(|s| &s.name)
                .collect::<Vec<_>>()
        );
    }

    /// SC17a. Every JSX tag used to emit a call edge, including intrinsic host
    /// elements. `<div/>` compiles to the string `"div"`, so that edge names a
    /// symbol that cannot exist — on the 12,817-file corpus `div` alone was the
    /// 5th most common unresolved callee at 10,307 rows, and `button` 2,280.
    /// The noise is not merely cosmetic: it buries real resolution failures.
    ///
    /// The tag must still be recorded as a `JsxTag` reference — dropping the
    /// call must not cost the graph its knowledge that the tag was used.
    #[test]
    fn jsx_intrinsic_tags_emit_no_call_but_components_still_do() {
        let source = concat!(
            "export function View() {\n",
            "  return (\n",
            "    <div>\n",
            "      <Widget />\n",
            "      <Panel.Header />\n",
            "      <svg:circle />\n",
            "      <button onClick={go} />\n",
            "    </div>\n",
            "  );\n",
            "}\n",
        );
        let extraction = extract_treesitter("v.tsx", "tsx", source);
        let callees: Vec<&str> = extraction
            .calls
            .iter()
            .map(|call| call.callee_name.as_str())
            .collect();

        for component in ["Widget", "Header"] {
            assert!(
                callees.contains(&component),
                "component tag {component:?} must still emit a call; got {callees:?}"
            );
        }
        // SC26: a member tag is a member access, so the namespace is the
        // receiver rather than part of the name.
        assert!(
            extraction
                .calls
                .iter()
                .any(|c| c.callee_name == "Header" && c.receiver_expr.as_deref() == Some("Panel")),
            "`<Panel.Header/>` must split into receiver `Panel`; got {:?}",
            extraction
                .calls
                .iter()
                .map(|c| (&c.callee_name, &c.receiver_expr))
                .collect::<Vec<_>>()
        );
        for intrinsic in ["div", "button", "svg:circle"] {
            assert!(
                !callees.contains(&intrinsic),
                "intrinsic tag {intrinsic:?} must not emit a call; got {callees:?}"
            );
        }

        // The tag itself is still known to the graph.
        assert!(
            extraction
                .references
                .iter()
                .any(|r| r.name == "div" && r.kind == ReferenceKind::JsxTag),
            "the `div` JsxTag reference must survive: {:?}",
            extraction
                .references
                .iter()
                .map(|r| (&r.name, &r.kind))
                .collect::<Vec<_>>()
        );
    }

    /// SC22. Shell and SQL had no grammar, so every `.sh` and `.sql` file
    /// returned `ParseOutcome::Failed` and contributed nothing but a File node —
    /// 86 shell and 19 SQL files on the personal corpus. Neither is in the
    /// frozen Python 35, so neither has a `LanguageSpec`; they reach a grammar
    /// through `detect_language`'s fallback table.
    ///
    /// Asserts the symbols, not merely that the parse succeeded: a Clean parse
    /// that yields nothing is what SQL did before `generic_symbol_kind` and
    /// `generic_declaration_name` learned its node shapes.
    #[test]
    fn shell_and_sql_extract_their_declarations() {
        let shell = extract_treesitter(
            "deploy.sh",
            "shell",
            "#!/bin/bash\nhelper() {\n  echo hi\n}\nmain() {\n  helper\n}\nmain\n",
        );
        assert_eq!(shell.parse_outcome, ParseOutcome::Clean);
        let shell_names: Vec<&str> = shell
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Function)
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(
            shell_names,
            vec!["helper", "main"],
            "shell function definitions must be symbols"
        );

        let sql = extract_treesitter(
            "schema.sql",
            "sql",
            concat!(
                "CREATE TABLE analytics.users (id INT);\n",
                "CREATE VIEW active AS SELECT id FROM users;\n",
                "CREATE FUNCTION tally() RETURNS INT AS $$ SELECT 1 $$ LANGUAGE SQL;\n",
                "CREATE INDEX users_id_idx ON users(id);\n",
            ),
        );
        assert_eq!(sql.parse_outcome, ParseOutcome::Clean);
        let sql_symbols: Vec<(&str, &SymbolKind)> = sql
            .symbols
            .iter()
            .filter(|s| s.kind != SymbolKind::File)
            .map(|s| (s.name.as_str(), &s.kind))
            .collect();
        assert_eq!(
            sql_symbols,
            vec![
                // Schema-qualified: the identity is the name, not `analytics.users`.
                ("users", &SymbolKind::Struct),
                ("active", &SymbolKind::Struct),
                ("tally", &SymbolKind::Function),
            ],
            "an index names no callable and owns no fields, so it must not \
             become a symbol"
        );
    }

    /// SC26. Three call shapes recorded a whole expression as the callee, so
    /// each named a symbol nothing declares and joined to nothing. Found by
    /// reading the `unresolved` tier once SC18/SC25 had cleared the expected
    /// rows out of it — together they were 11,847 of its 34,013 rows.
    ///
    /// The Rust case is not merely noise: `MyType::create()` is a *real edge* to
    /// a user-defined associated function that the graph was dropping.
    #[test]
    fn call_targets_split_instead_of_recording_whole_expressions() {
        // Rust associated functions and module paths.
        let rust = extract_treesitter(
            "b.rs",
            "rust",
            concat!(
                "fn run() {\n",
                "    let v = MyType::create();\n",
                "    std::fs::write(\"a\", \"b\");\n",
                "}\n",
            ),
        );
        let rust_calls: Vec<(&str, Option<&str>)> = rust
            .calls
            .iter()
            .map(|c| (c.callee_name.as_str(), c.receiver_expr.as_deref()))
            .collect();
        assert!(
            rust_calls.contains(&("create", Some("MyType"))),
            "a Rust associated function must split into callee + type; got {rust_calls:?}"
        );
        assert!(
            rust_calls.contains(&("write", Some("std::fs"))),
            "a Rust path call must split into callee + module path; got {rust_calls:?}"
        );
        assert!(
            !rust_calls.iter().any(|(callee, _)| callee.contains("::")),
            "no callee may still carry a path separator; got {rust_calls:?}"
        );

        // `await` only breaks the callee when a type argument is present, which
        // is why plain awaited calls never showed the defect.
        let ts = extract_treesitter(
            "a.ts",
            "typescript",
            concat!(
                "async function run() {\n",
                "  const r = await invoke<Raw>('x');\n",
                "  const q = await api.post<Raw>('y');\n",
                "}\n",
            ),
        );
        let ts_calls: Vec<(&str, Option<&str>)> = ts
            .calls
            .iter()
            .map(|c| (c.callee_name.as_str(), c.receiver_expr.as_deref()))
            .collect();
        assert!(
            ts_calls.contains(&("invoke", None)),
            "an awaited generic call must name the function; got {ts_calls:?}"
        );
        assert!(
            ts_calls.contains(&("post", Some("api"))),
            "an awaited generic member call must split; got {ts_calls:?}"
        );
        assert!(
            !ts_calls
                .iter()
                .any(|(callee, _)| callee.starts_with("await")),
            "`await` must never be part of a callee name; got {ts_calls:?}"
        );

        // A JSX member tag is a member access, not a component of that name.
        let tsx = extract_treesitter(
            "v.tsx",
            "tsx",
            "export const V = () => <motion.div><Plain /></motion.div>;\n",
        );
        let tsx_calls: Vec<(&str, Option<&str>)> = tsx
            .calls
            .iter()
            .map(|c| (c.callee_name.as_str(), c.receiver_expr.as_deref()))
            .collect();
        assert!(
            tsx_calls.contains(&("div", Some("motion"))),
            "a JSX member tag must split into namespace + property; got {tsx_calls:?}"
        );
        assert!(
            tsx_calls.contains(&("Plain", None)),
            "a plain component tag must keep its bare name; got {tsx_calls:?}"
        );
    }

    /// SC26b. Two more whole-expression callees, found by re-reading the
    /// `unresolved` tier after the first round of splits cleared it further.
    #[test]
    fn turbofish_and_inline_literals_do_not_become_callee_names() {
        // Type arguments are not part of a callee's identity.
        let rust = extract_treesitter(
            "q.rs",
            "rust",
            concat!(
                "fn run(row: Row) {\n",
                "    let a = row.get::<_, String>(0);\n",
                "    let b = serde_json::from_str::<Value>(\"{}\");\n",
                "}\n",
            ),
        );
        let rust_calls: Vec<(&str, Option<&str>)> = rust
            .calls
            .iter()
            .map(|c| (c.callee_name.as_str(), c.receiver_expr.as_deref()))
            .collect();
        assert!(
            rust_calls.contains(&("get", Some("row"))),
            "a turbofish method call must split to its method; got {rust_calls:?}"
        );
        assert!(
            rust_calls.contains(&("from_str", Some("serde_json"))),
            "a turbofish path call must split to its function; got {rust_calls:?}"
        );
        assert!(
            !rust_calls.iter().any(|(callee, _)| callee.contains('<')),
            "no callee may carry type arguments; got {rust_calls:?}"
        );

        // An immediately-invoked literal has no callee identity at all.
        let go = extract_treesitter(
            "d.go",
            "go",
            concat!(
                "package p\n",
                "func run() {\n",
                "\tdefer func() { _ = recover() }()\n",
                "\tnamed()\n",
                "}\n",
            ),
        );
        let go_calls: Vec<&str> = go.calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            go_calls.contains(&"named"),
            "ordinary calls must survive; got {go_calls:?}"
        );
        assert!(
            !go_calls.iter().any(|c| c.starts_with("func(")),
            "an inline function literal must not become a callee name; got {go_calls:?}"
        );
    }

    /// SC17b. A Go composite literal recorded its whole *type expression* as the
    /// callee, so `[]*genai.Part{...}` produced a call to the literal text
    /// `[]*genai.Part`. No symbol is ever named that, so the edge could not
    /// join, and the real construction of `Part` was lost. 24,382 such rows on
    /// the corpus.
    ///
    /// Unwrapping recovers the real reference. A literal whose core is a
    /// predeclared type constructs nothing indexable and must stay silent
    /// rather than emit a call to `string`.
    #[test]
    fn go_composite_literals_reference_the_named_type_they_construct() {
        let source = concat!(
            "package main\n",
            "func build() {\n",
            "\t_ = []*Hypothesis{}\n",
            "\t_ = map[string]*Finding{}\n",
            "\t_ = Session{}\n",
            "\t_ = []genai.Part{}\n",
            "\t_ = []string{}\n",
            "\t_ = map[string]int{}\n",
            "}\n",
        );
        let extraction = extract_treesitter("b.go", "go", source);
        let callees: Vec<&str> = extraction
            .calls
            .iter()
            .map(|call| call.callee_name.as_str())
            .collect();

        for named in ["Hypothesis", "Finding", "Session", "Part"] {
            assert!(
                callees.contains(&named),
                "literal must reference the named type {named:?}; got {callees:?}"
            );
        }
        // The wrapper text must never be the callee.
        for literal_text in ["[]*Hypothesis", "map[string]*Finding", "[]string"] {
            assert!(
                !callees.contains(&literal_text),
                "type expression {literal_text:?} must not be a callee; got {callees:?}"
            );
        }
        // Predeclared cores construct nothing indexable.
        for predeclared in ["string", "int"] {
            assert!(
                !callees.contains(&predeclared),
                "predeclared type {predeclared:?} must not be a callee; got {callees:?}"
            );
        }
    }

    /// Every `grammar_for` arm is counted, and every counted key has an arm.
    ///
    /// `linked_grammar_count` is what hosts compare against a vendored
    /// expectation. If a new language grows an arm without joining
    /// `GRAMMAR_LANGUAGE_KEYS`, the count stays stale and the host cannot
    /// detect the mismatch; if a key is listed without an arm, the count
    /// under-reports silently the other way.
    #[test]
    fn linked_grammar_keys_cover_every_grammar_for_arm() {
        for lang in GRAMMAR_LANGUAGE_KEYS {
            assert!(
                grammar_for(lang).is_some()
                    || UNSAFE_GRAMMARS
                        .iter()
                        .any(|(unsafe_lang, _)| unsafe_lang == lang),
                "{lang} is listed in GRAMMAR_LANGUAGE_KEYS but has no grammar_for arm \
                 and is not in UNSAFE_GRAMMARS"
            );
        }
        // Spot-check: a language that is not listed must not suddenly grow an
        // arm without updating the list. Probe a few ids that are deliberately
        // absent from the table.
        for absent in ["vb", "cobol", "fortran", "haskell"] {
            if GRAMMAR_LANGUAGE_KEYS.contains(&absent) {
                continue;
            }
            assert!(
                grammar_for(absent).is_none()
                    || UNSAFE_GRAMMARS
                        .iter()
                        .any(|(unsafe_lang, _)| *unsafe_lang == absent),
                "{absent} has a grammar_for arm but is missing from GRAMMAR_LANGUAGE_KEYS"
            );
        }
        let count = linked_grammar_count();
        assert!(
            count >= 30,
            "linked_grammar_count()={count} is too low for this workspace's grammar set"
        );
        assert_eq!(count, linked_grammar_keys().len());
    }
}

#[cfg(test)]
mod parent_work_regression {
    use super::*;

    #[test]
    fn repeated_ancestor_questions_do_not_reconstruct_the_root_walk() {
        let source: String = (0..500)
            .map(|i| format!("fn f{i}(a: i32) -> i32 {{ external(a) + {i} }}\n"))
            .collect();
        NATIVE_PARENT_READS.with(|count| count.set(0));
        let extraction = extract_treesitter("wide.rs", "rust", &source);
        assert_eq!(extraction.parse_outcome, ParseOutcome::Clean);
        assert_eq!(
            extraction
                .symbols
                .iter()
                .filter(|s| s.kind == SymbolKind::Function)
                .count(),
            500
        );
        assert_eq!(
            extraction
                .calls
                .iter()
                .filter(|c| c.callee_name == "external")
                .count(),
            500
        );
        let reads = NATIVE_PARENT_READS.with(Cell::get);
        // This bounds actual expensive native work, independently of CPU speed
        // and debug/release profiles. Every ancestor is in this small tree.
        assert!(
            reads < 500,
            "reconstructed {reads} root walks for 500 functions"
        );
    }
}
