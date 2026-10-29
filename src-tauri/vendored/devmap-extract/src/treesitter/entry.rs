use super::*;

/// Whether `path` is Metal, asked of the frozen registry rather than of `lang`.
///
/// `lang` is the *grammar* key and answers `"cpp"` for Metal and for C++ alike,
/// which is correct for choosing a parser and useless for any rule that holds
/// for one language and not the other.
pub(super) fn is_metal_path(path: &str) -> bool {
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
/// Five seconds is far above what a legitimate file costs on an idle machine
/// and far below the pathological case. Measured on the real corpora: the
/// largest source seen (624 KB, 12,353 lines) takes **875 ms** standalone at a
/// load average of 85, and most files take single-digit milliseconds.
///
/// It is a *wall-clock* bound, so it is not load-independent: the parallel
/// pass runs one file per rayon worker, and under heavy contention (load
/// 100-300) that 875 ms file stretches past five seconds and is refused. That
/// is a property of the clock, not of the file, so a budget refusal in the
/// parallel pass is not final: `extract_all_with_budgets` and
/// `retry_budget_refusals` re-run each such file serially, with no sibling
/// competing for the cores, under [`RETRY_BUDGET_FACTOR`] times this budget
/// (25 s). Only a file that overruns that too stays refused, and it is named
/// in the extraction report and in `devmap status` (`parse_failed`).
pub const DEFAULT_PARSE_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// How much larger the serial retry budget is than the parallel-pass budget.
///
/// Five: enough to absorb the 6x contention stretch measured on the 875 ms
/// file's worst case once the contention within the process is gone, while
/// keeping the retry bounded at 25 s per file against the 131 s pathological
/// C++ input.
pub const RETRY_BUDGET_FACTOR: u32 = 5;

/// At most this many files are retried per build, so a corpus full of
/// pathological inputs costs at most `MAX_BUDGET_RETRIES x 25 s` of serial
/// retry rather than `N x 25 s`. Files past the cap stay refused and are
/// counted in `BudgetRetryReport::not_retried`.
pub const MAX_BUDGET_RETRIES: usize = 16;

/// Stamp a budget-overrun reason so [`crate::model::Extraction::is_budget_refusal`]
/// can recognise it. The single constructor of such reasons.
pub(crate) fn budget_refusal_reason(detail: String) -> String {
    format!("{}{detail}", crate::model::BUDGET_REFUSAL_PREFIX)
}

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
pub(super) const UNSAFE_GRAMMARS: &[(&str, &str)] = &[(
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
pub(super) const GRAMMAR_LANGUAGE_KEYS: &[&str] = &[
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
pub(super) enum ParseAttempt {
    Parsed(tree_sitter::Tree),
    /// The parser exceeded its budget and was cancelled.
    Budget(std::time::Duration),
    /// `set_language` refused a grammar that *is* linked.
    GrammarLoadFailed,
    /// The parser returned no tree for a reason it did not name.
    NoTree,
}

/// Parse `source`, cancelled once `deadline` passes: `(tree, cancelled)`.
///
/// **Every parse in this crate goes through this.** The progress callback is
/// polled by tree-sitter during the parse and returning `true` cancels it; it
/// is the only way to bound a parse that has already entered a pathological
/// state, because a byte cap only bounds what is offered to the parser, not
/// how long the parser spends on it. The macro-body probes used to call
/// `parser.parse(text, None)` with no callback at all, after the file's own
/// parse had been bounded, inside the walk the daemon holds its writer lock
/// across.
///
/// A cancelled parse leaves the parser holding its partial state, and the next
/// `parse` on it would *resume* that state instead of starting on the new
/// text. The probes reuse one parser per thread, so a cancel resets it here
/// rather than relying on every caller to.
pub(crate) fn parse_until(
    parser: &mut Parser,
    source: &str,
    deadline: std::time::Instant,
) -> (Option<tree_sitter::Tree>, bool) {
    let mut cancelled = false;
    let tree = {
        let mut cancel = |_: &tree_sitter::ParseState| -> bool {
            if std::time::Instant::now() >= deadline {
                cancelled = true;
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
    if cancelled {
        parser.reset();
    }
    (tree, cancelled)
}

/// Parse an in-memory fragment — a macro body, a macro's expansion — against
/// the deadline of the file whose walk asked for it.
///
/// A probe is part of that file's extraction, not a separately funded one, so
/// it gets no budget of its own: it stops at the same instant
/// `parse_within_budget` and the walk do. A probe that crosses it latches
/// `WALK_OVERRAN`, so the file is refused rather than published with the calls
/// the probe never read. Outside an armed extraction (a unit test calling a
/// helper directly) it is held to [`DEFAULT_PARSE_BUDGET`] from now, never to
/// nothing.
pub(super) fn parse_probe(parser: &mut Parser, text: &str) -> Option<tree_sitter::Tree> {
    #[cfg(test)]
    PROBE_START_HOOK.with(|hook| {
        if let Some(start) = hook.take() {
            start();
        }
    });
    // A probe that starts after the deadline does not start: a small fragment
    // can finish before tree-sitter first polls the callback, and that parse
    // would still be work done past the point the file was promised to stop.
    if walk_overran() || walk_deadline_passed() {
        return None;
    }
    let deadline = WALK_DEADLINE
        .with(|slot| slot.get())
        .unwrap_or_else(|| std::time::Instant::now() + DEFAULT_PARSE_BUDGET);
    let (tree, cancelled) = parse_until(parser, text, deadline);
    if cancelled {
        WALK_OVERRAN.with(|slot| slot.set(true));
        return None;
    }
    tree
}

pub(super) fn parse_within_budget(
    parser: &mut Parser,
    source: &str,
    deadline: std::time::Instant,
) -> ParseAttempt {
    let started = std::time::Instant::now();
    // Against the *shared* deadline, not a budget of its own: the parse is the
    // first phase of the file's extraction, not a separately funded one.
    let (tree, over_budget) = parse_until(parser, source, deadline);
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
    pub(super) static EXTRACTION_FINISH_HOOK: Cell<Option<fn()>> = const { Cell::new(None) };
    /// Run once as the next macro-body probe starts, before its parse.
    pub(super) static PROBE_START_HOOK: Cell<Option<fn()>> = const { Cell::new(None) };
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
            budget_refusal_reason(format!(
                "extraction finalization exceeded the {budget:?} budget; no symbols are claimed for this file"
            )),
        )
    } else {
        extraction
    }
}

pub(super) fn extract_treesitter_before_deadline(
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
                    budget_refusal_reason(format!(
                        "parse of {} bytes exceeded the {:?} budget for grammar {grammar} \
                         (stopped at {elapsed:?}); no symbols are claimed for this file",
                        source.len(),
                        budget
                    )),
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
        // Kotlin only: a one-line class body is a whole-file grammar error
        // that a `;` written over a space repairs without moving a byte. The
        // rewritten tree replaces the failed one only if it is fully clean.
        let (attempt, semi_ranges, semi_note) = match attempt {
            // Only a tree `parse_outcome_of` calls `Partial`. That includes the
            // shapes the grammar recovers itself with a hidden zero-width
            // token: no node shows it, `has_error()` counts it, and
            // `parse_outcome_of` charges the node that carries it.
            ParseAttempt::Parsed(tree)
                if lang == "kotlin"
                    && tree.root_node().has_error()
                    && matches!(
                        parse_outcome_of(
                            tree.root_node(),
                            source,
                            false,
                            false,
                            true,
                            &crate::kotlinloss::LostScan::default(),
                        ),
                        ParseOutcome::Partial { .. }
                    ) =>
            {
                match crate::kotlinsemi::repair(&mut parser, tree, source, deadline) {
                    crate::kotlinsemi::Repair::Repaired {
                        tree,
                        ranges,
                        edits,
                        boundaries,
                    } => (
                        ParseAttempt::Parsed(tree),
                        ranges,
                        Some(format!(
                            "kotlin one-line class body{}: {edits} separator(s) written over \
                             spaces so the file parses; offsets are the file's own",
                            if boundaries {
                                " (declarations sharing a line)"
                            } else {
                                ""
                            }
                        )),
                    ),
                    crate::kotlinsemi::Repair::Unchanged { tree, why } => (
                        ParseAttempt::Parsed(tree),
                        Vec::new(),
                        Some(format!("kotlin one-line class body not repaired: {why}")),
                    ),
                }
            }
            other => (other, Vec::new(), None),
        };
        if let ParseAttempt::Parsed(tree) = attempt {
            return crate::parent_index::with_index(&tree, deadline, || {
                // The budget covers parse *and* walk *and* everything after it.
                // Measured: a 4,000-byte C++ file of 2,000 nested braces parses
                // in 3 ms and then spends 199 s in the walk, so bounding the
                // parse alone bounds nothing that actually hurts.
                let content_hash = content_hash(source);

                let root = tree.root_node();
                // Kotlin only: statements the grammar read as expressions,
                // which parse without an error and so never reach the walk
                // below that finds error nodes.
                let lost = if lang == "kotlin" {
                    crate::kotlinloss::detect(root, source)
                } else {
                    crate::kotlinloss::LostScan::default()
                };
                // The walks below leave out what the expression reading made of
                // a charged statement, instead of the settling that follows
                // taking it out by name.
                set_charged_regions(
                    lost.regions
                        .iter()
                        .map(|region| (region.range.start_byte, region.range.end_byte))
                        .collect(),
                );
                let parse_outcome = parse_outcome_of(
                    root,
                    source,
                    is_metal_path(path),
                    lang == "python",
                    lang == "kotlin",
                    &lost,
                );

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
                    return_type: None,
                });

                // File-level wiring first, so a file-scoped exemption always
                // precedes the symbol-scoped ones and stays the file's reason.
                let mut wiring = extract_wiring_annotations(path, source);

                // Every phase below shares one refusal, so a file cut short
                // in the tail cannot be reported differently from one cut short
                // in the walk.
                let over_budget = |phase: &str| {
                    budget_refusal_reason(format!(
                        "extraction of {} bytes exceeded the {budget:?} budget for grammar \
                         {grammar} while {phase}; no symbols are claimed for this file",
                        source.len()
                    ))
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
                // After the walk, because a stamped function is a declaration
                // the walk could not see: it exists only in a macro's
                // expansion. Its overrun is caught by the check below, the one
                // every phase between here and assembly shares.
                if is_c_family_grammar(lang) {
                    crate::cmacro::stamp_macro_instantiations(
                        root,
                        source,
                        lang,
                        &file_symbol_name,
                        is_metal_path(path),
                        &mut symbols,
                        &mut calls,
                        &mut references,
                        &mut wiring,
                        deadline,
                    );
                }
                // Ruby defines methods with no `def`: `attr_reader :tap` is the
                // declaration of `tap`. After the walk, so a `def` of the same
                // name anywhere in the class has already been emitted and wins.
                if lang == "ruby" {
                    let attributes = crate::langdecl::ruby::attribute_symbols(
                        root,
                        source,
                        &file_symbol_name,
                        &symbols,
                    );
                    symbols.extend(attributes);
                }
                if !crate::entry_names::collect(
                    root,
                    source,
                    lang,
                    &file_symbol_name,
                    &mut references,
                    deadline,
                ) {
                    return refused_extraction(
                        path,
                        lang,
                        source,
                        over_budget("reading name strings"),
                    );
                }

                let (go_interface_methods, go_method_params, go_member_names) = if lang == "go" {
                    let (interface_methods, method_params, mut member_names) =
                        go_method_sets(root, source, &file_symbol_name);
                    // A file tree-sitter could not parse cleanly may hold a
                    // declaration the walk never saw. Every declaration spells
                    // its name, so each unexported token in the text is a
                    // possible namesake: vetoing all of them keeps the
                    // unexported-selector rung sound for this package without
                    // switching it off for names this file never mentions. (One
                    // `Partial` file among scholarlm's 471 in `internal/api` had
                    // silenced the rung for the whole package.)
                    if !matches!(parse_outcome, ParseOutcome::Clean) {
                        member_names.extend(go_unexported_tokens(source));
                        member_names.sort();
                        member_names.dedup();
                    }
                    go_interface_method_exemptions(
                        &symbols,
                        &interface_methods,
                        &method_params,
                        &mut wiring,
                    );
                    (interface_methods, method_params, Some(member_names))
                } else {
                    (Vec::new(), Vec::new(), None)
                };
                if matches!(lang, "typescript" | "tsx") {
                    ts_ambient_exemptions(path, root, &symbols, &mut wiring);
                }
                if lang == "rust" {
                    rust_crate_private_scopes(root, source, &symbols, &mut wiring);
                }
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
                    // Every module-scope binding is kept. A Python module's
                    // names are its attributes, and `from mod import NAME`
                    // imports any of them: `__all__` governs only
                    // `from mod import *`. Keeping only the names `__all__`
                    // listed dropped `REQUIRED_CONTROLS` from a module whose
                    // `__all__` omitted it while a test imported it by name,
                    // and dropped every constant of a module with no `__all__`
                    // at all — unsearchable, with no target for its importers
                    // and no blast radius. `__all__` still decides
                    // `is_exported`, exactly as it does for functions and
                    // classes, so liveness judges a constant as it judges a
                    // function: by its edges.
                    //
                    // One symbol per name. `try: X = a` / `except: X = b`
                    // binds one module attribute, not two, and `f = wrap(f)`
                    // rebinds the attribute `def f` already declares.
                    let mut seen: BTreeSet<String> = symbols
                        .iter()
                        .filter(|symbol| symbol.kind != SymbolKind::Variable)
                        .map(|symbol| symbol.qualified_name.clone())
                        .collect();
                    symbols.retain(|symbol| {
                        symbol.kind != SymbolKind::Variable
                            || seen.insert(symbol.qualified_name.clone())
                    });
                }

                if matches!(lang, "javascript" | "typescript" | "tsx") {
                    // A binding exported by `export { local }` or
                    // `export default local` is exported as surely as one
                    // whose declaration says `export`; mark it, then keep the
                    // module-level bindings that are exported by either route.
                    // Private module constants stay out, as they do for Rust
                    // and Go.
                    let clause_exported = js_locally_exported_names(root, source, &exports);
                    for symbol in symbols.iter_mut() {
                        if symbol.kind != SymbolKind::File
                            && symbol.parent_symbol.as_deref() == Some(file_symbol_name.as_str())
                            && clause_exported.contains(&symbol.name)
                        {
                            symbol.is_exported = true;
                        }
                    }
                    // An unexported object keeps its symbol when it owns a member
                    // symbol: the member's `parent_symbol` and its qualified name
                    // both name it, and a parent that is no node is a containment
                    // edge from nothing.
                    let owners: BTreeSet<String> = symbols
                        .iter()
                        .filter(|symbol| symbol.kind == SymbolKind::Method)
                        .filter_map(|symbol| symbol.parent_symbol.clone())
                        .collect();
                    symbols.retain(|symbol| {
                        symbol.kind != SymbolKind::Variable
                            || symbol.is_exported
                            || owners.contains(&symbol.qualified_name)
                    });
                }

                publish_exports(&symbols, &mut exports);
                if lang == "rust" {
                    rust_expand_local_macros(
                        root,
                        source,
                        &file_symbol_name,
                        &mut symbols,
                        &mut calls,
                        &mut references,
                    );
                    rust_calls_through_fn_pointer_items(root, source, &mut calls);
                }
                // Ties on a span break by name, except for a Python base: its
                // segments share the base's one span and their emission order
                // (member, then the qualifier from the root down) is what says which
                // segment is the class, so the sort keeps it.
                fn tie_name<'r>(lang: &str, reference: &'r ExtractedReference) -> &'r str {
                    if lang == "python" && reference.kind == ReferenceKind::Heritage {
                        ""
                    } else {
                        reference.name.as_str()
                    }
                }
                references.sort_by(|a, b| {
                    (a.span.start_byte, a.span.end_byte, tie_name(lang, a)).cmp(&(
                        b.span.start_byte,
                        b.span.end_byte,
                        tie_name(lang, b),
                    ))
                });

                // Framework matching is an auxiliary extractor. A matcher
                // failure cannot retroactively turn a successful grammar
                // parse into a failed parse outcome, but it must remain
                // visible to downstream diagnostics.
                let (routes, mut diagnostics) = match extract_framework_routes(lang, source) {
                    Ok(routes) => (routes, Vec::new()),
                    Err(reason) => (Vec::new(), vec![reason]),
                };
                diagnostics.extend(semi_note);

                // Body signatures are stamped here, after every language arm
                // has finished pushing symbols, rather than inside `walk_tree`.
                // The walk emits symbols from more than thirty grammar-specific
                // branches; hashing in each is thirty chances to add a grammar
                // later and silently omit its signatures.
                crate::clonesig::stamp_signatures(&mut symbols, root, source);
                crate::clonesig::stamp_declaration_hashes(&mut symbols, root, source);
                crate::returns::stamp_return_types(&mut symbols, root, source);

                let rust_implicit_calls = if lang == "rust" {
                    rust_collect_implicit_calls(root, source, &file_symbol_name)
                } else {
                    Vec::new()
                };
                // The implicit calls name receivers the resolver types from the
                // same site bindings an explicit call's receiver does.
                let binding_calls: std::borrow::Cow<[ExtractedCall]> =
                    if rust_implicit_calls.is_empty() {
                        std::borrow::Cow::Borrowed(&calls)
                    } else {
                        std::borrow::Cow::Owned(
                            calls.iter().chain(&rust_implicit_calls).cloned().collect(),
                        )
                    };
                let local_bindings = collect_site_bindings(
                    root,
                    source,
                    lang,
                    &file_symbol_name,
                    &binding_calls,
                    &references,
                    &imports,
                );
                // Python modules loaded by file path (`spec_from_file_location`
                // and friends) — see `pyload`. Before the gate below, so the
                // walk's deadline bounds it like every other stage.
                if lang == "python" {
                    imports.extend(crate::pyload::python_path_loads(
                        root,
                        source,
                        &file_symbol_name,
                    ));
                }

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

                let (rust_inline_modules, rust_deref_targets, rust_blanket_impl_params) =
                    if lang == "rust" {
                        collect_rust_item_facts(root, source)
                    } else {
                        (Vec::new(), Vec::new(), Vec::new())
                    };
                // One call per fact family, each answering for its own
                // languages and returning an empty set for the rest, so a new
                // fact is added in `jvmfacts` or `pyfacts` and never here.
                let jvm_facts = crate::jvmfacts::collect(
                    root,
                    source,
                    lang,
                    &symbols,
                    &parse_outcome,
                    deadline,
                );
                let py_facts = crate::pyfacts::collect(root, source, lang, &symbols, deadline);
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
                    jvm_package: matches!(lang, "kotlin" | "java")
                        .then(|| jvm_package_name(root, source))
                        .flatten(),
                    scope_receivers: if lang == "kotlin" {
                        crate::langcalls::kotlin::scope_receivers(root, source)
                    } else {
                        Vec::new()
                    },
                    swift_type_aliases: if lang == "swift" {
                        crate::swiftscope::declared_aliases(root, source)
                    } else {
                        Vec::new()
                    },
                    go_build_constrained: lang == "go" && go_build_constrained(path, source),
                    go_interface_methods,
                    go_method_params,
                    go_member_names,
                    go_facts: (lang == "go")
                        .then(|| crate::gofacts::extract(root, source, &file_symbol_name)),
                    // After `walk_tree`, so the per-scope cache it warmed is
                    // reused rather than every callable's subtree being walked
                    // a second time.
                    scope_locals: collect_scope_locals(root, source, &file_symbol_name, lang),
                    rust_inline_modules,
                    rust_deref_targets,
                    rust_blanket_impl_params,
                    rust_implicit_calls,
                    rust_try_conversions: if lang == "rust" {
                        rust_collect_try_conversions(root, source, &file_symbol_name)
                    } else {
                        Vec::new()
                    },
                    local_bindings,
                    source_code: Some(source.to_string()),
                    literals: Vec::new(),
                    js_objects: if matches!(lang, "javascript" | "typescript" | "tsx") {
                        crate::jsobjects::collect(root, source)
                    } else {
                        Vec::new()
                    },
                    jvm_decls: jvm_facts.jvm_decls,
                    package_scopes: jvm_facts.package_scopes,
                    local_extents: jvm_facts.local_extents,
                    decl_gaps: jvm_facts.decl_gaps,
                    recovered_decl_ranges: semi_ranges,
                    dynamic_dispatch: py_facts.dynamic_dispatch,
                };

                // Kotlin only, and only for a file the loss detector charged:
                // re-derive the declarations the grammar read as expressions.
                // After the extraction is assembled, because the recovered
                // symbols carry their own stamps from the re-parse, and before
                // everything below, which reads `extraction.symbols`.
                if lang == "kotlin" {
                    crate::kotlinloss::recover(&mut extraction, source, &lost, deadline);
                }

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
                // After the merge, so an embedded `<script>`'s references are
                // deduplicated against its own calls too, and once for the
                // whole file rather than per language arm.
                drop_duplicate_callee_names(&mut extraction.references);
                // The other half of a template file: its markup, and its
                // `<style>` block. After the script merge because a selector
                // string is attributed to the function it sits inside, and that
                // function only exists in `symbols` once the merge above has
                // run; after the dedup because a `ReferenceKind::Selector` is
                // not in the callee-name namespace that pass is about. Returns
                // after one registry lookup for every language whose entry
                // permits neither `css` nor `html` inside it.
                crate::markup::merge_markup(&mut extraction, root, source, lang);
                let (literals, mut notes) = crate::literals::index_literals(
                    root,
                    source,
                    lang,
                    &extraction.symbols,
                    deadline,
                );
                extraction.literals = literals;
                extraction.diagnostics.append(&mut notes);
                extraction.diagnostics.extend(take_nesting_cap_notes());
                extraction
                    .diagnostics
                    .extend(take_swift_unspelled_operator_notes());
                extraction
                    .diagnostics
                    .extend(take_swift_bare_cast_member_notes());
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
pub(super) fn push_children<'tree>(node: Node<'tree>, worklist: &mut Vec<Node<'tree>>) {
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
pub(super) fn push_children_reversed<'tree>(node: Node<'tree>, worklist: &mut Vec<Node<'tree>>) {
    let mark = worklist.len();
    push_children(node, worklist);
    worklist[mark..].reverse();
}

/// [`push_children`] over named children only.
pub(crate) fn push_named_children<'tree>(node: Node<'tree>, worklist: &mut Vec<Node<'tree>>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        worklist.push(child);
    }
}

/// Publish every exported symbol under its own name unless the module already
/// publishes that name, and put `exports` in its published order.
///
/// The one owner of the answer, called after the walk and again after a Kotlin
/// loss recovery has settled `symbols`: an export is derived from the symbols
/// that survive, never patched on the way, so a symbol that is not published
/// cannot hold the slot of one that is.
pub(crate) fn publish_exports(symbols: &[ExtractedSymbol], exports: &mut Vec<ExtractedExport>) {
    // The names the exports so far publish, kept as sets so each
    // exported symbol asks one lookup instead of scanning every
    // export: a type with tens of thousands of exported members made
    // this loop quadratic (30,000 Swift properties spent most of
    // their extraction here).
    let mut published_as: std::collections::HashSet<String> = exports
        .iter()
        .map(|export| export.exported_name.clone())
        .collect();
    let mut published_from: std::collections::HashSet<String> = exports
        .iter()
        .filter(|export| export.module_specifier.is_none() && export.exported_name != "default")
        .filter_map(|export| export.local_name.clone())
        .collect();
    for symbol in symbols.iter().filter(|symbol| symbol.is_exported) {
        // A symbol already published under another name —
        // `export { local as Public }` — is not also exported as
        // itself. Matching on `exported_name` alone fabricated a
        // second export, `local`, that the module never declared.
        if !published_as.contains(&symbol.name) && !published_from.contains(&symbol.name) {
            published_as.insert(symbol.name.clone());
            if symbol.name != "default" {
                published_from.insert(symbol.name.clone());
            }
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
}

/// The outcome of a parse: `Clean`, or `Partial` with the ranges that are not
/// to be trusted.
///
/// `lost` is the Kotlin loss detector's answer (see [`crate::kotlinloss`]):
/// statements the grammar read as an expression without reporting an error. Its
/// regions are charged as error ranges, so a file that silently dropped a
/// declaration is `Partial` and every consumer that already treats a partial
/// parse as unable to prove absence does so here. It is applied in this one
/// place so the answer has one owner. A detector that did not finish is not a
/// detector that found nothing: a deadline fails the outcome as this walk's own
/// deadline does, and a node cap charges the whole file.
pub(super) fn parse_outcome_of(
    root: Node,
    source: &str,
    is_metal: bool,
    is_python: bool,
    is_kotlin: bool,
    lost: &crate::kotlinloss::LostScan,
) -> ParseOutcome {
    if lost.deadline_passed {
        return ParseOutcome::Failed {
            reason: budget_refusal_reason(
                "parse outcome validation exceeded the extraction budget".to_string(),
            ),
        };
    }
    if !root.has_error() && !is_python && lost.regions.is_empty() && !lost.capped {
        return ParseOutcome::Clean;
    }
    let mut error_ranges = Vec::new();
    let mut stack = vec![root];
    let mut cursor = root.walk();
    let mut since_check = 0u32;
    while let Some(node) = stack.pop() {
        since_check += 1;
        if since_check >= DEADLINE_CHECK_STRIDE {
            since_check = 0;
            if walk_deadline_passed() {
                return ParseOutcome::Failed {
                    reason: budget_refusal_reason(
                        "parse outcome validation exceeded the extraction budget".to_string(),
                    ),
                };
            }
        }
        // tree-sitter-python permits an empty `block`, although Python
        // requires a statement after a compound header. This is a suite node,
        // not the optional body of an empty module. Comments are extras and
        // do not satisfy it; `pass` and a docstring do.
        let empty_python_suite = is_python && node.kind() == "block" && {
            let mut children = node.walk();
            let empty = !node
                .named_children(&mut children)
                .any(|child| child.kind() != "comment");
            empty
        };
        if empty_python_suite {
            let owner = bounded_parent(node).unwrap_or(node);
            error_ranges.push(TextRange {
                start_byte: owner.start_byte(),
                end_byte: owner.end_byte(),
            });
        }
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
        let mut child_has_error = false;
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                child_has_error |= child.has_error();
                if child.has_error() || is_python {
                    stack.push(child);
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        // Kotlin only: a node that carries an error no node shows. The parser
        // inserted a zero-width token that is hidden from the tree (a
        // grammar's `_`-prefixed token), so the cost is on this node and on
        // none of its children, and the search for an ERROR or MISSING node
        // above comes back empty. Measured on tree-sitter-kotlin-ng, `class A {
        // fun f() = if (c) { a() } else { b() } }` is `has_error()` on the
        // file, the class and its body and on nothing below, and reading what
        // the hidden token should have separated made the `if` branch a
        // lambda: phantom `$0` and `it` locals. The file is not clean and no
        // consumer may be told it is, so the node's own range is charged.
        //
        // Not for other grammars: they report `has_error()` with no visible
        // child in valid code (Swift `b as? String ?? "x"`, a C directive or a
        // Go declaration at the end of a file with no newline), and charging
        // those made 4 Vani Swift files and 1 Curio file Partial with no graph
        // change. Another grammar earns the rule by a measured wrong reading,
        // not by sharing the blind spot.
        if is_kotlin
            && node.has_error()
            && !node.is_error()
            && !node.is_missing()
            && !child_has_error
        {
            error_ranges.push(TextRange {
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
            });
        }
    }
    if lost.capped {
        error_ranges.push(TextRange {
            start_byte: 0,
            end_byte: source.len(),
        });
    }
    if !lost.regions.is_empty() || lost.capped {
        error_ranges.extend(lost.ranges());
        error_ranges.sort_by_key(|range| (range.start_byte, range.end_byte));
        error_ranges.dedup();
    }
    if error_ranges.is_empty() {
        ParseOutcome::Clean
    } else {
        let nameless_ranges = error_ranges
            .iter()
            .filter(|range| range_is_nameless(source, range))
            .cloned()
            .collect();
        ParseOutcome::Partial {
            error_ranges,
            nameless_ranges,
        }
    }
}

/// Whether an error region of nonzero width holds no identifier character.
///
/// ASCII letters, digits, `_` and `$` count, and so does every non-ASCII byte,
/// since an identifier may be spelled in any script. A range that does not lie
/// on the source is not nameless: what cannot be read is assumed to hold a name.
pub(super) fn range_is_nameless(source: &str, range: &TextRange) -> bool {
    range.end_byte > range.start_byte
        && source
            .as_bytes()
            .get(range.start_byte..range.end_byte)
            .is_some_and(|bytes| {
                !bytes
                    .iter()
                    .any(|&b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || !b.is_ascii())
            })
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
pub(super) const METAL_DECLARATION_QUALIFIERS: &[&str] = &[
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
pub(super) fn is_benign_metal_qualifier(node: Node, source: &str) -> bool {
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
pub(super) fn template_group_start(source: &str, close: usize) -> Option<usize> {
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
pub(super) fn preceding_token(source: &str, at: usize) -> Option<(usize, &str)> {
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

pub(super) fn is_benign_jsx_ampersand(node: Node, source: &str) -> bool {
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
pub(super) fn jsx_tag_is_component(tag: &str) -> bool {
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
pub(super) fn go_composite_literal_type<'tree>(
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

/// Whether `node` is the **field-name key** of a Go struct composite literal.
///
/// `&Command{run: nil}` parses the key as an ordinary `identifier`, so it
/// reached `maybe_push_name_reference` as a bare `ReferenceKind::Name` and was
/// indistinguishable from naming a package-level `func run`. That cost nothing
/// while the reference ladder declined every bare `Name`; once the Go
/// package-scope rung began answering them, the same spelling produced a
/// `SamePackage` edge at DETERMINISTIC — a fabricated caller for a function
/// nothing calls, which is exactly the defect the receiver gates elsewhere in
/// this ladder exist to prevent.
///
/// A struct key is not a reference to anything the graph holds: it names a
/// field of the literal's own type. A **map** key is the opposite — `m[k]` and
/// `map[string]int{k: 1}` both name a real value — so the two are separated by
/// the literal's declared type rather than by position alone.
///
/// The type is read from the nearest enclosing `composite_literal`, and when
/// that one is implicit (`[]Command{{run: nil}}`, `map[string]Command{"a":
/// {run: nil}}` — the inner literal carries no type at all) from the one above
/// it, whose element/value type is what the inner literal constructs.
/// [`go_composite_literal_type`] already performs that unwrapping, so this
/// asks it rather than repeating the rules.
pub(super) fn go_struct_literal_field_key(node: Node, source: &str) -> bool {
    // Key position: `keyed_element` holds `literal_element : literal_element`,
    // and only the first is the key.
    let Some(element) = node.parent().filter(|p| p.kind() == "literal_element") else {
        return false;
    };
    let Some(keyed) = element.parent().filter(|p| p.kind() == "keyed_element") else {
        return false;
    };
    if keyed.named_child(0).map(|first| first.id()) != Some(element.id()) {
        return false;
    }

    // The literal this key belongs to, then outwards while the type is implicit.
    let mut literal = keyed.parent().and_then(|value| value.parent());
    for _ in 0..16 {
        let Some(current) = literal.filter(|c| c.kind() == "composite_literal") else {
            return false;
        };
        match current.child_by_field_name("type") {
            // A map literal's key is an expression, not a field name. Checked
            // before unwrapping, because `go_composite_literal_type` reduces
            // `map[K]V` to `V` and the distinction would be gone.
            Some(declared) if declared.kind() == "map_type" => return false,
            Some(declared) => {
                return go_composite_literal_type(declared, source, 0).is_some();
            }
            // Implicit type: this literal is an element of the one outside it.
            None => {
                literal = current
                    .parent()
                    .and_then(|value| value.parent())
                    .filter(|outer| outer.kind() == "composite_literal");
            }
        }
    }
    false
}

/// Go's predeclared type names, from the language specification.
///
/// None of these can be a node in the graph — they are declared by the language,
/// not by any indexed file — so emitting a call to `string` only produces an
/// unresolvable row, and risks joining to an unrelated user symbol of that name.
pub(super) const GO_PREDECLARED_TYPES: &[&str] = &[
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
