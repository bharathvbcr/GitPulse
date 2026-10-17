use crate::model::*;
use devmap_extract::languages::{capabilities_for_language, Capability};
use devmap_extract::model::*;
use devmap_resolve::model::*;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Symbol identity relative to its file, so that `file_path` + `symbol_name`
/// reconstructs the graph id exactly. A method must report as `MyClass.execute`
/// rather than `execute`; the bare form cannot be joined back to a node and
/// collides with any same-named method on a different type in the same file.
/// Kinds that are never dead-code candidates, whatever the graph says about
/// them.
///
/// `File` is here because file reachability is a different analysis with its own
/// edges. The two markup kinds are here because **this index can prove a DOM or
/// stylesheet identity is used and cannot prove one is not.** A class reaches an
/// element through `class:{expr}`, through `classList.add(name)` where `name` is
/// computed, through a `:global()` rule another component relies on, or through a
/// template string — and `crate::markup` deliberately reads none of those, on the
/// grounds that guessing at them would invent edges. Reporting `.card` dead on
/// that evidence would be the exact error the honesty rules in this crate exist
/// to prevent: a check that *could not run* returning the same answer as a check
/// that ran and found nothing.
///
/// The consequence is stated rather than hidden: DevMap does not report unused
/// CSS. Svelte's own compiler does, from inside the compilation that knows the
/// answer, and that is the tool for it.
///
/// `Macro` is here for the same reason: a `macro_rules!` macro is linked only to
/// its invocations in the file that defines it. One invoked from another file
/// through `#[macro_use]`, a `use` of the macro or `$crate::name!` leaves no
/// edge, so an unreferenced macro is not evidence of a dead one.
fn is_never_dead_candidate(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::File | SymbolKind::MarkupAnchor | SymbolKind::StyleRule | SymbolKind::Macro
    )
}

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

/// Why a Go function named as a value is exempt rather than reported.
///
/// Worded as a statement about the evidence, not about the function. This
/// exemption does not claim the function runs — it says the index holds a
/// value-position mention of it that the reference ladder declines to attribute,
/// so the index cannot show that nothing uses it.
pub const GO_VALUE_MENTION_REASON: &str =
    "Named as a value in its own Go package — the mention is a bare identifier the ladder declines";

/// Bare value-position identifiers named in a Go package, keyed by
/// `(directory, package clause, name)` and carrying the files that named them.
///
/// `Command{Run: runStatus}` is the shape. The mention is a
/// `ReferenceKind::Name`, and `resolve_reference` returns `None` for a bare
/// `Name` rather than continuing down the ladder — deliberately, because the
/// unique-global rung would bind `except Exception as e` to some unrelated
/// `def e`. The Go package rung is written *below* that refusal and its comment
/// says so in as many words: a bare identifier mention is the one shape that
/// function declines rather than fails, and a package-scope rung must not be the
/// thing that widens it. So the edge does not exist, and liveness sees a function
/// nothing calls.
///
/// The declaring file is excluded at the use site, not here, because it is only
/// knowable per candidate. That exclusion is what keeps this exemption from
/// claiming credit for the same-file rung's work — and, in the same stroke, stops
/// a function's own body from being the evidence that spares it.
///
/// Scoping to the package is exact rather than merely conservative, by the same
/// rule as [`go_interface_specs_by_package`]: an unexported Go name resolves only
/// within its own directory, and an exported one reports `is_exported` and is
/// spared earlier without ever reaching this branch.
///
/// **Known over-approximation, tested and named.** Go spells a composite-literal
/// field key as a bare identifier with no receiver, so `Command{run: nil}` is
/// indistinguishable here from naming a package-level `func run`. An unexported
/// function colliding with a field key is therefore exempted although nothing
/// uses it. That is the direction an exemption may be wrong in — withholding a
/// finding it cannot prove, never asserting one it cannot support. Narrowing it
/// belongs in the extractor, where a field key could carry its composite type as
/// a receiver; at this layer the only available answer would be to guess from the
/// spelling.
fn go_value_mentioned_names(
    extractions: &[Extraction],
) -> HashMap<(&str, &str, &str), HashSet<&str>> {
    let mut mentions: HashMap<(&str, &str, &str), HashSet<&str>> = HashMap::new();
    for ext in extractions {
        let Some((dir, package)) = go_package_key(ext) else {
            continue;
        };
        for reference in &ext.references {
            // A receiver makes this a member reference — `cfg.runDoctor` names a
            // struct field, and the member rungs run for it in full, so its
            // failure is already recorded as a failure rather than declined.
            if reference.kind != ReferenceKind::Name || reference.receiver_expr.is_some() {
                continue;
            }
            mentions
                .entry((dir, package, reference.name.as_str()))
                .or_default()
                .insert(ext.file_path.as_str());
        }
    }
    mentions
}

/// Exported interface method specs anywhere in the corpus, by `(method, arity)`.
///
/// The corpus-wide counterpart of [`go_interface_specs_by_package`]. An
/// exported method name is not qualified by its package, so a type in one
/// package satisfies an interface declared in any other. Read only for
/// exported Go methods; see the arm in [`symbol_exemption_index`].
fn go_exported_interface_specs(extractions: &[Extraction]) -> HashMap<(&str, usize), &str> {
    let mut specs: HashMap<(&str, usize), &str> = HashMap::new();
    for ext in extractions {
        if ext.language != "go" {
            continue;
        }
        for spec in &ext.go_interface_methods {
            if spec.method.starts_with(|c: char| c.is_uppercase()) {
                specs
                    .entry((spec.method.as_str(), spec.param_count))
                    .or_insert(spec.interface_name.as_str());
            }
        }
    }
    specs
}

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

/// How much of the corpus the extraction tier could not read.
///
/// The cross-file half of X6. `analyze_liveness` already refuses to call a
/// parse-failed file's *own* symbols dead, because "nothing calls it" is only
/// evidence when calls were looked for. The same sentence is true one hop out:
/// a file that contributed no call edges was also the only possible caller of
/// somebody else's symbol, and nothing downstream knew that the reachability
/// scan had a hole in it.
///
/// Kept as two numbers rather than one because they are different claims. A
/// `Failed` file contributed nothing at all; a `Fallback` file contributed
/// names and spans but, by construction, no calls and no imports. Both lose
/// edges, and a reader deciding whether to act on a dead-code finding wants to
/// know which kind of blindness they are looking at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractionCoverage {
    /// Files a grammar was wanted for and did not get to read.
    ///
    /// Counted through [`Extraction::is_parse_failure`], the canonical owner,
    /// **not** through a bare `matches!(parse_outcome, Failed { .. })`. Prose
    /// and data formats report `Failed` for want of a grammar that does not
    /// exist and never will; on this repository that is 294 of 1,310 files, all
    /// Markdown, JSON, YAML, config and HTML. Counting those would report every
    /// build of every real repository as degraded, and a degraded flag that is
    /// always on carries no information at all.
    pub parse_failed_files: usize,
    /// Files whose declarations were recovered by line pattern.
    ///
    /// A `.proto` or `.ps1` this build cannot parse is a genuine gap in call
    /// coverage — see [`ExtractionEngine::NotApplicable`]'s own docs drawing
    /// exactly this line against a `.md`. Inventory still charges these into
    /// `coverage_gaps`; whether they *cap* findings is language-scoped — a
    /// PowerShell recovery cannot hide callers of a Rust symbol.
    pub pattern_recovered_files: usize,
    /// Files discovery refused before any extractor saw them — oversized,
    /// unreadable, or a non-UTF-8 path.
    ///
    /// **Not derivable from `extractions`**, which is precisely why this gap
    /// outlived the parse-failure one it otherwise resembles: a refused file has
    /// no `Extraction` at all, so every coverage check computed from that slice
    /// reported a complete corpus. The count has to be carried in from
    /// discovery, and the two production build paths do that.
    ///
    /// The cost of missing it, measured: `lib.py` defines `helper()`, its only
    /// caller `app.py` is over `MAX_SOURCE_BYTES`, and `devmap dead` proposed
    /// deleting `helper` at 0.9 — the confident tier — because the file that
    /// calls it was never read.
    pub discovery_refused_files: usize,
    /// Files a grammar read cleanly whose language has no call extractor.
    ///
    /// The gap the other three could not represent, because it is not a
    /// failure of any kind: the parse succeeded. `CALL_EXTRACTION_LANGUAGES`
    /// was supposed to be read by "the coverage report" so this would be a
    /// stated fact — no such reader existed, and a CFML or Terraform file
    /// therefore reported full coverage while contributing not one call edge.
    pub call_blind_files: usize,
    /// Files a grammar read cleanly whose language has no import extractor.
    ///
    /// Counted, published, and deliberately kept out of `is_complete()` — see
    /// the note there. Its consumer is `unwired_candidates`, whose entire
    /// question is "does an inbound `Imports` edge exist", and which for 24 of
    /// 35 languages was answering it from an absence the extractor created.
    pub import_blind_files: usize,
    /// Files whose calls a grammar actually read.
    ///
    /// The denominator the other five counters never had. Without it `cap()`
    /// could only ask *whether* the scan had a hole, never *how big* — so one
    /// unreadable vendored file and a corpus that is 90% unreadable produced
    /// the identical verdict, and the whole confidence ladder collapsed onto
    /// [`COVERAGE_LOSS_CONFIDENCE_CAP`].
    ///
    /// Counted the same way `call_blind_files` is charged, from
    /// [`a_grammar_read_this_file`] and the language's `Calls` capability, so
    /// the two are two sides of one partition and cannot drift into describing
    /// different corpora. Prose and data formats are in neither: no grammar
    /// read them and none ever will, so they are not part of the question.
    pub files_with_call_extraction: usize,
    /// Files the extractor chose not to parse.
    ///
    /// Counted and published so the decision is visible, and — like
    /// `import_blind_files` — deliberately kept out of `is_complete()`. The
    /// files this counts are minified bundles: third-party output already
    /// exempt from liveness through `WiringKind::Vendored`, whose every
    /// identifier is a minifier's `t`, `e` or `n`. Folding them in would cap
    /// every dead-code finding in every repository that vendors one bundle,
    /// which is the trade `import_blind_files` was kept out of `is_complete()`
    /// to avoid.
    ///
    /// Published rather than dropped because the alternative is the silence
    /// `discovery_refused_files` documents: a file nothing read, counted as a
    /// file whose calls were looked for.
    pub not_parsed_files: usize,
    /// Call-coverage holes that can demote findings, keyed by the gap file's
    /// language.
    ///
    /// Inventory counters above stay corpus-wide so `coverage_gaps` still names
    /// every hole. Capping asks a narrower question: does this hole hide
    /// callers of *this* finding's language? A pattern-recovered `.ps1` lands
    /// in `pattern_recovered_files` and here under `"powershell"`, and
    /// [`language_can_reference`] refuses to let it affect Rust or Go.
    ///
    /// Empty when a test constructs coverage from aggregate counters alone —
    /// [`Self::blind_files`] then falls back to those aggregates so the graded
    /// ceiling tests keep exercising the curve without building a language map.
    pub blind_by_language: BTreeMap<String, usize>,
    /// Files whose calls were extracted, keyed by language — the per-language
    /// half of [`Self::files_with_call_extraction`].
    pub covered_by_language: BTreeMap<String, usize>,
    /// Source files discovery skipped for exceeding the size ceiling whose
    /// language extracts calls and which are not vendored.
    ///
    /// The ones that can hold a caller, and so the only ones charged: a
    /// 30 MB vendored `parser.c` cannot call this repository's code, and a
    /// `.json` blob has no calls to miss, so neither demotes a finding — which
    /// is the "permanently partial" trap that made `Oversized` an ordinary skip
    /// in the first place. A 1.7 MB generated `gen.py` calling `helper` forty
    /// thousand times is exactly a caller, and was invisible.
    ///
    /// Also entered in [`Self::blind_by_language`] under its language, so the
    /// language-scoped cap sees it; counted here for the aggregate fallback
    /// and for `degraded_reason`'s sentence.
    pub over_limit_files: usize,
    /// Files a grammar read with `ERROR`/`MISSING` nodes in the tree that are
    /// charged: in a call-extracting language and not vendored.
    ///
    /// Entered in [`Self::blind_by_language`] and kept out of
    /// [`Self::covered_by_language`]. A hole in a file whose calls were never
    /// looked for is already charged as `call_blind`, and a vendored file
    /// cannot hold a caller; both are still *listed* in the `partial_parse`
    /// inventory, which `extraction_gaps` derives without this filter.
    pub partial_parse_files: usize,
}

/// What discovery refused, for the analysis that cannot see it.
///
/// A separate type rather than a bare `usize` so a caller cannot pass the wrong
/// count positionally, and so the one place that means "no discovery step ran"
/// is spelled [`DiscoveryCoverage::none`] rather than `0`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveryCoverage {
    /// `None` means no discovery result is being reported — which is **not**
    /// the same as a discovery step that ran and refused nothing. Keeping the
    /// two apart is the whole reason this is an `Option` and not a `usize`: a
    /// summary that records `0` for the first case tells a later reader the
    /// tree was fully walked when nobody walked it.
    refused_files: Option<usize>,
    /// Source files skipped for exceeding the size ceiling, sorted by path.
    /// Meaningful only beside a measured `refused_files`: the same walk
    /// produces both.
    over_limit: Vec<crate::model::OverLimitFile>,
}

impl DiscoveryCoverage {
    /// No discovery step ran, so there is no refusal count to report.
    ///
    /// Correct for a caller that supplies its own corpus directly — a test, or
    /// the single-file preview path. Wrong for anything that walked a tree, and
    /// that is the distinction this exists to keep visible.
    pub fn none() -> Self {
        Self {
            refused_files: None,
            over_limit: Vec::new(),
        }
    }

    /// A discovery step ran and refused this many files. `refused(0)` is a
    /// measurement, and says more than [`DiscoveryCoverage::none`] does.
    pub fn refused(refused_files: usize) -> Self {
        Self {
            refused_files: Some(refused_files),
            over_limit: Vec::new(),
        }
    }

    /// The same measurement, with the files the walk skipped for size.
    ///
    /// Sorted and de-duplicated by path here, so two callers handing the same
    /// facts in different orders persist the same summary.
    pub fn with_over_limit(mut self, mut files: Vec<crate::model::OverLimitFile>) -> Self {
        files.sort();
        files.dedup_by(|left, right| left.path == right.path);
        self.over_limit = files;
        self
    }

    /// The over-limit files as persisted: `None` when discovery was not
    /// measured, for the reason [`Self::refused_files`] gives.
    pub fn over_limit_files(&self) -> Option<Vec<crate::model::OverLimitFile>> {
        self.refused_files.map(|_| self.over_limit.clone())
    }

    /// The over-limit files that can hide a caller, with their language.
    ///
    /// A call-extracting language — by the same capability bit
    /// `extraction_coverage` counts covered files from, so the two sides of
    /// the share describe one partition — and not vendored, by
    /// `devmap_extract::wiring::is_vendored_path`, the predicate liveness
    /// already exempts vendored symbols by.
    pub fn charged_over_limit(&self) -> impl Iterator<Item = (&str, &'static str)> {
        self.over_limit.iter().filter_map(|file| {
            let language = devmap_extract::detect_language(std::path::Path::new(&file.path));
            let calls = devmap_extract::languages::capabilities_for_language(language)
                .contains(Capability::Calls);
            (calls && !devmap_extract::wiring::is_vendored_path(&file.path))
                .then_some((file.path.as_str(), language))
        })
    }

    /// The measurement, or `None` when none was taken. Persisted verbatim so a
    /// later generation can tell "measured, nothing refused" from "never
    /// measured" instead of rounding both to zero.
    pub fn refused_files(&self) -> Option<usize> {
        self.refused_files
    }

    /// What to charge against coverage.
    ///
    /// An unmeasured discovery contributes nothing, deliberately. Charging it
    /// would put every caller that builds its own corpus — every test, and the
    /// single-file preview path — permanently in a degraded state, and a marker
    /// that is always on is worth exactly as much as one that is never on.
    pub fn charged(&self) -> usize {
        self.refused_files.unwrap_or(0)
    }
}

impl ExtractionCoverage {
    /// Whether every file in the corpus had its calls looked for.
    ///
    /// `import_blind_files` is deliberately **not** here. It is a hole in a
    /// different claim: no `Imports` edge is evidence about
    /// `unwired_candidates`, which is where W0.3 charges it, and folding it in
    /// would cap every dead-code finding in every Java, C++, Ruby, Swift, C#
    /// and PHP repository at `ambiguous` — that is most of the world's code,
    /// demoted for a blindness that is not the one the verdict rests on. The
    /// existing two counters are kept apart for exactly this reason ("different
    /// claims"), and a third that means something else again gets the same
    /// treatment.
    /// Derived from [`Self::blind_files`] rather than re-listing its counters.
    ///
    /// They were two hand-written sums over the same fields and they had already
    /// drifted: `not_parsed_files` was in the numerator of `blind_share` and not
    /// in this verdict, so 500 vendored bundles alone left the corpus "complete"
    /// — every finding at 0.9 — and adding one `.proto` made it *50% blind* in
    /// one step. `degraded_reason` then sized the hole from a third list again,
    /// so the sentence a reader saw could not match what `cap` had charged.
    /// One owner ends all three disagreements.
    pub fn is_complete(&self) -> bool {
        self.blind_files() == 0
    }

    /// Whether call extraction covering callers of `language` is complete.
    ///
    /// The single-symbol pass asks this of the finding's own language so a
    /// PowerShell pattern-recovery cannot demote a Rust orphan.
    pub fn is_complete_for(&self, language: &str) -> bool {
        self.blind_affecting(language) == 0
    }

    /// Files that contributed no call edges, of any kind.
    ///
    /// Saturating rather than wrapping. Every counter here is bounded by a file
    /// count in production, but `discovery_refused_files` is folded in from
    /// *outside* — `DiscoveryCoverage::refused(n)` takes whatever a caller
    /// passes — and this feeds a division. A wrap would turn a huge blind count
    /// into a small one and hand a confident ceiling to a corpus nothing read,
    /// which is the flattering direction and therefore the one to bound.
    ///
    /// A count of fact, where [`Self::is_complete`] is a verdict, which is why
    /// `not_parsed_files` is in this one and not in that one: a skipped file
    /// really did contribute no call edges, and saying otherwise would make the
    /// method's name false, but it is not a reason to distrust the corpus.
    pub fn files_without_call_extraction(&self) -> usize {
        self.parse_failed_files
            .saturating_add(self.pattern_recovered_files)
            .saturating_add(self.call_blind_files)
            .saturating_add(self.not_parsed_files)
    }

    /// Why the corpus-level scan is incomplete, or `None` when it is complete.
    ///
    /// `None` on a clean corpus is the whole point: this string is what
    /// `analyze()` folds into `AnalysisStatus::Partial`, which drives
    /// `graph_degraded` and `analysis_status`. A repository whose every file
    /// parsed must keep reporting `ok`.
    pub fn degraded_reason(&self) -> Option<String> {
        if self.is_complete() {
            return None;
        }
        let mut reason = format!(
            "call extraction did not cover the whole corpus: {} file(s) failed to parse, \
             {} recovered by pattern (no calls extracted), {} refused by discovery and never \
             read at all — dead-code and unwired findings are a lower bound and are capped \
             below the confident tier",
            self.parse_failed_files, self.pattern_recovered_files, self.discovery_refused_files
        );
        // Appended rather than folded into the sentence above, because it is a
        // different kind of fact and a permanent one. The other three describe
        // this run — a file that happened to fail, a walk that happened to
        // refuse. This one describes the build: no amount of re-running
        // extracts a call from a `.cfm`, and a reader deciding whether to
        // re-index needs to know which of the two they are looking at.
        if self.call_blind_files > 0 {
            reason.push_str(&format!(
                "; {} file(s) in a language with no call extractor at all \
                 (permanent for this build, not a transient failure)",
                self.call_blind_files
            ));
        }
        // Its own clause for the same reason: the fix is to split or shrink
        // the file (or accept the hole), not to re-run anything.
        if self.partial_parse_files > 0 {
            reason.push_str(&format!(
                "; {} file(s) parsed with ERROR/MISSING regions, so declarations and calls \
                 inside those regions are absent",
                self.partial_parse_files
            ));
        }
        if self.over_limit_files > 0 {
            reason.push_str(&format!(
                "; {} source file(s) that can hold calls were skipped for exceeding the \
                 source size limit, so any caller in them is absent from the graph",
                self.over_limit_files
            ));
        }
        Some(reason)
    }

    /// Files this scan had no call edges from, of any cause.
    ///
    /// The numerator of [`Self::blind_share`]. `import_blind_files` is
    /// deliberately absent for the same reason it is absent from
    /// `is_complete()`: it is a hole in a different claim.
    fn blind_files(&self) -> usize {
        // **Not** `files_without_call_extraction()`, which is a count of *fact*
        // and includes `not_parsed_files`. This is the count of *hole*, and the
        // two differ by exactly the files `is_complete()` deliberately forgives:
        // minified bundles the extractor chose not to parse, already exempt from
        // liveness through `WiringKind::Vendored`, whose own field doc says
        // folding them in "would cap every dead-code finding in every repository
        // that vendors one bundle".
        //
        // Language-scoped when `blind_by_language` is populated: a PowerShell
        // recovery beside Rust does not demote the corpus. Aggregate-only
        // records (tests that set counters by hand) keep the pre-scoped sum.
        let aggregate = self
            .parse_failed_files
            .saturating_add(self.pattern_recovered_files)
            .saturating_add(self.call_blind_files)
            .saturating_add(self.discovery_refused_files)
            .saturating_add(self.over_limit_files);
        if self.blind_by_language.is_empty() && self.covered_by_language.is_empty() {
            return aggregate;
        }
        let mut blind = self.discovery_refused_files;
        if self.covered_by_language.is_empty() {
            for count in self.blind_by_language.values() {
                blind = blind.saturating_add(*count);
            }
            return blind;
        }
        for (gap_lang, count) in &self.blind_by_language {
            if self
                .covered_by_language
                .keys()
                .any(|covered| language_can_reference(gap_lang, covered))
            {
                blind = blind.saturating_add(*count);
            }
        }
        blind
    }

    /// Gaps that can hide callers of symbols in `language`.
    fn blind_affecting(&self, language: &str) -> usize {
        if self.blind_by_language.is_empty() && self.covered_by_language.is_empty() {
            return self.blind_files();
        }
        let mut blind = self.discovery_refused_files;
        for (gap_lang, count) in &self.blind_by_language {
            if language_can_reference(gap_lang, language) {
                blind = blind.saturating_add(*count);
            }
        }
        blind
    }

    /// Readable call-extraction files that could produce callers of `language`.
    fn covered_affecting(&self, language: &str) -> usize {
        if self.covered_by_language.is_empty() {
            return self.files_with_call_extraction;
        }
        self.covered_by_language
            .iter()
            .filter(|(lang, _)| language_can_reference(lang, language))
            .map(|(_, count)| *count)
            .fold(0usize, usize::saturating_add)
    }

    /// The blind share this record is *charged* at, which is never smaller than
    /// [`MIN_CHARGED_BLIND_SHARE`].
    ///
    /// `None` carries the same meaning it does in [`Self::blind_share`]:
    /// nothing was measured, so nothing is known.
    fn charged_blind_share(&self) -> Option<f32> {
        Some(self.blind_share()?.max(MIN_CHARGED_BLIND_SHARE))
    }

    fn charged_blind_share_for(&self, language: &str) -> Option<f32> {
        Some(self.blind_share_for(language)?.max(MIN_CHARGED_BLIND_SHARE))
    }

    /// The ceiling for a claim whose strength compounds over `extra` extra
    /// members, or `None` when no corpus was measured.
    ///
    /// One implementation for [`Self::cap`] and [`Self::cap_cluster`], which
    /// were the same five lines with one term different — and therefore two
    /// places for a future change to the curve, the floor or the minimum charge
    /// to land in only one of.
    fn ceiling(&self, extra: i32) -> Option<f32> {
        let charged = self.charged_blind_share()?;
        Some(
            (1.0 - charged)
                .powi(COVERAGE_CEILING_EXPONENT.saturating_add(extra))
                .clamp(COVERAGE_LOSS_CONFIDENCE_CAP, HIGHEST_DEGRADED_CONFIDENCE),
        )
    }

    fn ceiling_for(&self, language: &str, extra: i32) -> Option<f32> {
        let charged = self.charged_blind_share_for(language)?;
        Some(
            (1.0 - charged)
                .powi(COVERAGE_CEILING_EXPONENT.saturating_add(extra))
                .clamp(COVERAGE_LOSS_CONFIDENCE_CAP, HIGHEST_DEGRADED_CONFIDENCE),
        )
    }

    /// The share of the corpus whose calls were never extracted, in `[0, 1]`.
    ///
    /// `None` when nothing was measured at all — which is not zero. A
    /// [`Self::default`] record with no file counts behind it has not observed
    /// a complete corpus; it has observed nothing, and the two must not read
    /// alike.
    ///
    /// Saturating throughout, and the ratio is clamped after the division. Both
    /// guards were written after `graded_cap_under_hostile_input.rs` found the
    /// plain adds: a debug build panics on the overflow, and a **release** build
    /// wraps a saturated blind count round to a small one and hands a near-
    /// `extracted` ceiling to a corpus nothing read. Saturating turns the same
    /// input into "entirely blind", which is the answer it should have had.
    fn blind_share(&self) -> Option<f32> {
        let blind = self.blind_files();
        let considered = blind.saturating_add(self.files_with_call_extraction);
        if considered == 0 {
            return None;
        }
        // `as f32` on a saturated `usize` is lossy but monotone, and both sides
        // lose the same way, so the ratio survives. Clamped anyway: a ratio
        // outside `[0, 1]` would put `powi`'s base outside it too.
        Some((blind as f32 / considered as f32).clamp(0.0, 1.0))
    }

    fn blind_share_for(&self, language: &str) -> Option<f32> {
        let blind = self.blind_affecting(language);
        let considered = blind.saturating_add(self.covered_affecting(language));
        if considered == 0 {
            return None;
        }
        Some((blind as f32 / considered as f32).clamp(0.0, 1.0))
    }

    /// Ceiling applied to a non-exempt dead-code confidence while the scan has
    /// a hole in it, leaving anything below it untouched.
    ///
    /// **Graded, not binary.** The old rule asked one question — is any counter
    /// non-zero — and answered every finding in the generation with
    /// [`COVERAGE_LOSS_CONFIDENCE_CAP`]. Measured on this repository: 1,502
    /// files, 10 of them blind (0.67%), and all 214 dead findings came back at
    /// exactly 0.35. `generation_dead_symbols` is read `ORDER BY confidence
    /// DESC, file_path`, so with every confidence tied the ranked list an agent
    /// reads degenerated to alphabetical order, and the default budget showed
    /// it the first ~66 filenames rather than the strongest evidence.
    ///
    /// Worse than the ranking: the three tiers below became one number. "No
    /// edge names this symbol" (0.9) is evidence *for* death; "something calls
    /// it and the resolver could not say which" (0.4) and "an unresolved site
    /// names it" (0.4) are evidence *against*. Collapsing opposite evidence
    /// into one value is not conservatism — conservatism lowers the ceiling and
    /// keeps the order.
    ///
    /// So the ceiling now tracks the size of the hole. A caller's claim is "no
    /// file in the corpus calls this", and the risk it is wrong scales with the
    /// share of the corpus that was never read — not with whether that share is
    /// non-zero.
    ///
    /// Three properties, each pinned in `coverage_cap_is_graded.rs`:
    ///
    /// * **Never `extracted`.** [`HIGHEST_DEGRADED_CONFIDENCE`] sits below
    ///   `EXTRACTED_FLOOR_MILLIS`, so a check that could not run cannot reach
    ///   the tier whose contract is "safe to act on" — at *any* blind share.
    ///   That rule is absolute and does not scale.
    /// * **Never below the old floor.** A mostly-blind corpus lands on
    ///   [`COVERAGE_LOSS_CONFIDENCE_CAP`] exactly as it does today, so the
    ///   constant keeps its meaning instead of becoming decorative.
    /// * **Monotone, and never raises.** `cap` is non-decreasing in its input
    ///   and never returns more than it was given, so no finding is promoted by
    ///   this change — only separated from findings it was never equal to.
    ///
    /// Public because the cluster pass applies the same ceiling for a stronger
    /// reason: a cluster finding is wrong outright if one call edge into the
    /// component was missed. Two implementations of one ceiling is how the two
    /// would come to disagree about what "degraded" costs.
    pub fn cap(&self, confidence: f32) -> f32 {
        if self.is_complete() {
            return confidence;
        }
        // No measured corpus behind the record: nothing was read, so nothing is
        // known, and the floor is the only honest answer. Reachable — a build
        // whose discovery refused every file it found produces exactly this.
        let Some(ceiling) = self.ceiling(0) else {
            return confidence.min(COVERAGE_LOSS_CONFIDENCE_CAP);
        };
        confidence.min(ceiling)
    }

    /// Ceiling for a finding in `language`, ignoring holes that language cannot
    /// be reached from. A pattern-recovered `.ps1` must not demote Rust.
    pub fn cap_for(&self, language: &str, confidence: f32) -> f32 {
        if self.is_complete_for(language) {
            return confidence;
        }
        let Some(ceiling) = self.ceiling_for(language, 0) else {
            return confidence.min(COVERAGE_LOSS_CONFIDENCE_CAP);
        };
        confidence.min(ceiling)
    }

    /// The ceiling for a finding whose **own file** contributed no call edges.
    ///
    /// [`Self::cap`] prices a corpus-wide ratio, and for a file that is itself
    /// blind that ratio is the wrong denominator by construction. Measured: in
    /// a 99-Python / 1-Terraform corpus the `.tf` symbols took the corpus
    /// ceiling — 1% blind — and published at the top of `inferred` carrying
    /// `CALL_BLIND_REASON`, which is the string "this language has no call
    /// extractor in this build" priced as a one-percent risk. The local blind
    /// share for that file is 1.0: no call in it was read, and the files most
    /// likely to call it are the other files of the same language, every one of
    /// them equally unread. `(1 - 1.0)^n` is zero, so the floor is what the same
    /// curve returns for it, and the floor is what it gets.
    ///
    /// The worse of the two, not the local one alone, so a call-blind file in an
    /// *also* badly degraded corpus cannot come back better than its neighbours.
    ///
    /// Distinct from the wholesale exemption `is_parse_failed` applies. A
    /// call-blind file's declarations are real — a grammar read them cleanly —
    /// so the finding stays visible and non-exempt; only its confidence says
    /// that the evidence behind it could never have been gathered.
    pub fn cap_call_blind_file(&self, confidence: f32) -> f32 {
        self.cap(confidence).min(COVERAGE_LOSS_CONFIDENCE_CAP)
    }

    /// Language-aware form of [`Self::cap_call_blind_file`].
    pub fn cap_call_blind_file_for(&self, language: &str, confidence: f32) -> f32 {
        self.cap_for(language, confidence)
            .min(COVERAGE_LOSS_CONFIDENCE_CAP)
    }

    /// The ceiling for a **whole-graph** claim, which falls faster than
    /// [`Self::cap`]'s.
    ///
    /// `dead_clusters` states the difference and this is where it is priced: a
    /// missed call edge into a component makes the entire cluster finding
    /// wrong, where the same missed edge costs a single-symbol finding only
    /// itself. So the claim is not "this one symbol has no caller in the blind
    /// region" but "**none of the `size` members** does", and the ceiling
    /// compounds accordingly.
    ///
    /// The consequence is the one the cluster pass already argues for: a
    /// forty-symbol cluster in a corpus that is five percent blind is a much
    /// weaker claim than a two-symbol cluster in a corpus that is one file
    /// short, and before this the two were priced identically.
    ///
    /// `size` is clamped rather than trusted: `powi` on a 400,000-member
    /// component would underflow to zero, which the floor would catch anyway,
    /// but the clamp says so rather than relying on it.
    ///
    /// The clamp's *lower* bound is 1, so `cap_cluster(x, 0)` prices a
    /// zero-member component as a one-member one. No such component exists —
    /// Tarjan emits no empty component and the pass discards single nodes that
    /// do not self-loop — so this is a total function over an input the
    /// producer cannot supply, not a rounding of a real case. It is 1 rather
    /// than 0 because `(1 - s)^8` is the *single-symbol* ceiling, and a claim
    /// about nothing must not be priced more cheaply than a claim about
    /// something.
    pub fn cap_cluster(&self, confidence: f32, size: usize) -> f32 {
        if self.is_complete() {
            return confidence;
        }
        let members = size.clamp(1, CLUSTER_COMPOUNDING_MEMBER_CAP) as i32;
        let Some(ceiling) = self.ceiling(members) else {
            return confidence.min(COVERAGE_LOSS_CONFIDENCE_CAP);
        };
        confidence.min(ceiling)
    }
}

/// Whether a file in `from` can contribute call evidence about symbols in `to`.
///
/// Same language always can. A language with no call extractor cannot reference
/// anything in the graph sense — that is why PowerShell / protobuf pattern
/// recovery must not demote Rust or Go findings. Cross-language pairs share a
/// family only when an FFI or shared runtime makes that reference ordinary.
pub fn language_can_reference(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    if !capabilities_for_language(from).contains(Capability::Calls) {
        return false;
    }
    match (language_family(from), language_family(to)) {
        (Some(left), Some(right)) => left == right,
        // Unknown languages only reference themselves, via the `from == to` arm.
        _ => false,
    }
}

fn language_family(language: &str) -> Option<&'static str> {
    Some(match language {
        "javascript" | "typescript" | "tsx" | "arkts" | "svelte" | "vue" | "astro" | "liquid" => {
            "js"
        }
        "c" | "cpp" | "objc" | "cuda" | "metal" => "c",
        "java" | "kotlin" | "scala" => "jvm",
        "csharp" | "vbnet" => "clr",
        "python" => "python",
        "go" => "go",
        "rust" => "rust",
        "swift" => "swift",
        "ruby" => "ruby",
        "php" => "php",
        "dart" => "dart",
        _ => return None,
    })
}

/// How many members the cluster ceiling compounds over before it stops caring.
///
/// **This clamp is load-bearing, not a rounding convenience.** An earlier
/// draft of this comment claimed the ceiling "has long since hit the floor for
/// any non-trivial blind share" by this point, which is false where it matters:
/// measured on a hostile corpus of 411 files with 2 refused (0.49% blind), a
/// 5,000-member abandoned ring compounds to `0.99513^5008` — about 2.5e-11, so
/// the floor — while the same corpus's three-member cycle keeps 0.5. Without
/// the clamp, *every* large component in *any* corpus with one unreadable file
/// lands on `ambiguous`, which is the flattening this whole mechanism exists to
/// undo, reintroduced for clusters.
///
/// So the clamp is where the compounding stops being informative and starts
/// being a size penalty. A 64-member component is already a much weaker claim
/// than a 2-member one and is priced as such; past that, the extra members say
/// more about how the subsystem was written than about how likely the scan is
/// to have missed an edge into it.
///
/// It also keeps `powi` away from an exponent that would underflow silently.
const CLUSTER_COMPOUNDING_MEMBER_CAP: usize = 64;

/// How sharply the coverage ceiling falls as the corpus goes blind.
///
/// **A policy dial, stated as one rather than dressed up as derived.** There is
/// no probability model here that anyone can defend to three decimal places;
/// what there is, is a shape the answer has to have, and one number that fixes
/// it. The shape:
///
/// * A handful of unreadable files in a thousand must barely move the ceiling,
///   because otherwise the ladder collapses. That is the defect this exponent
///   exists to fix: 10 blind files of 1,502 on this repository put all 214 dead
///   findings on the same number.
/// * A corpus that is *substantially* unread must land on
///   [`COVERAGE_LOSS_CONFIDENCE_CAP`] exactly as it does today, because at that
///   point "nothing calls this" really is a statement about the scan.
///
/// At 8, the ceiling reaches the floor at a blind share of **12.3%**
/// (`0.35^(1/8) = 0.877`), which is the crossover
/// `the_ceiling_reaches_the_floor_well_before_the_corpus_is_half_unread` pins.
/// Every fixture in this crate's older coverage tests is a two- or three-file
/// corpus — 33% to 50% blind — so all of them sit past the crossover and keep
/// the exact behaviour they were written to assert. That is not a coincidence
/// to rely on quietly; it is why those tests still pass unchanged, and it is
/// checked rather than assumed.
const COVERAGE_CEILING_EXPONENT: i32 = 8;

/// The smallest blind share a degraded scan may be charged at.
///
/// **The denominator measures the wrong thing, and this is what is done about
/// it.** `blind_share` is a ratio over *file counts*, and file counts are not
/// what a dead-code claim rests on — call sites are. The two diverge in one
/// direction and only one:
///
/// * Discovery refuses a file precisely for exceeding `MAX_SOURCE_BYTES`
///   (`db.rs`), so the refused class is *by definition* the largest files in the
///   tree.
/// * Parse failures and `Skipped` cluster on the same tail: generated clients,
///   vendored bundles, machine-written protocol code.
///
/// So the files that go blind are systematically the files holding the most
/// calls, and a ratio over counts prices them as *average*. One 4 MB generated
/// client in a 1,000-file repository is 0.1% by file count and plausibly fifteen
/// percent of the corpus's call edges; the formula saw 0.1%.
///
/// **Weighting is not available, and pretending otherwise would be worse.** A
/// discovery-refused file has no `Extraction` at all — no symbols, no bytes,
/// nothing to weigh — so a symbol-weighted or byte-weighted share would give the
/// one class we *know* is huge a weight of zero. That is the same error with an
/// arithmetic alibi.
///
/// What is left is to state the uncertainty instead of pricing it at zero. A
/// corpus with any hole in it is charged at least this much, whatever the file
/// count says.
///
/// **The value, and the shape it is chosen for.** At 5%, `0.95^8 = 0.663`. That
/// is the middle of `inferred` — the tier whose contract is "unconfirmed" — with
/// 0.237 of headroom below `EXTRACTED_FLOOR_MILLIS` and 0.263 above
/// `INFERRED_FLOOR_MILLIS`. The rule it encodes is one sentence: **a check with
/// a known hole in it reports in the middle of "unconfirmed", never at its
/// edge.**
///
/// Without it the ceiling for a small hole was `HIGHEST_DEGRADED_CONFIDENCE`
/// itself, 0.89, so the entire distance between "the caller of this symbol was
/// never read" and "safe to delete" was ten thousandths and a rounding rule.
/// The audit's Q-1 — `lib.py::helper` whose only caller sits in a file over
/// `MAX_SOURCE_BYTES` — scored 0.35 before the grading landed and 0.89 after,
/// which is a claim that is *literally false* published one tier below the
/// act-on threshold. It now scores 0.663.
///
/// **It costs the grading nothing.** What the grading is for is separation: with
/// every confidence tied at 0.35, `ORDER BY confidence DESC, file_path`
/// degenerated to alphabetical and an agent read the first 66 filenames instead
/// of the strongest evidence. Separation needs the three tiers to differ, not to
/// approach `extracted` — and at 0.663 against 0.4 they differ by more than they
/// did at 0.89 against 0.4 in every way that a ranked read can use.
const MIN_CHARGED_BLIND_SHARE: f32 = 0.05;

/// The highest confidence a finding from an incomplete scan may carry.
///
/// `confidence_millis` rounds, and `EXTRACTED_FLOOR_MILLIS` is 900, so this has
/// to sit far enough below 0.9 that no rounding can reach it: 0.89 renders 890
/// and therefore `inferred`. A value of 0.8995 would round *up* into
/// `extracted` and quietly retire the one rule this whole mechanism exists to
/// enforce.
pub const HIGHEST_DEGRADED_CONFIDENCE: f32 = 0.89;

/// Confidence ceiling for a dead-code finding made against a partially read
/// corpus.
///
/// Sits below `INFERRED_FLOOR_MILLIS` (400) so `code_graph.rs::confidence_label`
/// renders `ambiguous` rather than `inferred` or `extracted`, and above the
/// 0.3 the exempt tier uses so the two stay distinguishable. `extracted` is the
/// tier `CLAUDE.md` tells agents to act on; a check that could not run must
/// never reach it.
pub const COVERAGE_LOSS_CONFIDENCE_CAP: f32 = 0.35;

/// Reason carried by a confident finding that the coverage cap demoted.
///
/// The unqualified branch previously carried `None`, which `code_graph.rs`
/// renders as "no inbound call edges and not exported" — a claim the run was
/// not entitled to make. `only_ambiguous_callers` is deliberately left alone:
/// it is a machine token three tests match exactly, and its own tier already
/// reads as unconfirmed.
pub const COVERAGE_LOSS_REASON: &str =
    "no inbound call edges, but call extraction did not cover every file — not evidence of death";

/// Languages whose confident (0.9) dead tier has been earned by measurement.
///
/// **Set by a check, not by assignment.** `devmap-cli/tests/dead_tier_eligibility.rs`
/// builds every labelled fixture under `testdata/accuracy/` and fails if a
/// language is listed here while any of its confident-branch rows is not
/// labelled `truly_dead`, and fails if a language whose fixture is clean — at
/// least one confident row, no false one, every truly-dead marker in the
/// confident branch — and whose corpus record (`record.json`) has labelled
/// rows, at most 5% false on every corpus, is *not* listed. A language the
/// record never measured, or measured with no row, is unproven: a missing or
/// unreadable record fails the test. So the table cannot rot in either direction: a
/// regression that puts one false row in a listed language's confident tier
/// fails CI, and a fix that earns a language the tier fails CI until it is
/// listed.
///
/// Labels are [`Extraction::code_language`] values, so `typescript` and `tsx`
/// are earned separately — they are separate grammars with separate extractors.
///
/// A language not listed here never publishes a row at 0.9. Its confident
/// rows are capped at [`UNPROVEN_TIER_CONFIDENCE`] carrying
/// [`confident_tier_unproven_reason`], which names the language. The audit of
/// 2026-10-09 measured the confident tier wrong in every language but Python
/// (Go 2/2, TypeScript 13/28, Rust 1/1, Swift 1/1, Kotlin 2/11), each through a
/// silent miss that bypassed the namesake veto — and `CLAUDE.md` tells agents
/// that tier is the one to act on.
///
/// **What listing rests on today, stated so nobody reads more into it.**
/// (2026-10-10.) Each listed language has zero confident-tier false positives
/// on its committed fixture, every fixture `truly_dead` marker in the confident
/// branch, at least one labelled row in the corpus record, and at most 5% false
/// on every corpus of it (`testdata/accuracy/record.json`: 20 local
/// repositories, every language each holds listed — `rows: 0` where its
/// confident branch was empty). Measured there: Go 0/17 (all from one corpus),
/// TypeScript 0/29, TSX 0/14, Rust 0/3.
///
/// Off the list: **Kotlin**, whose only corpus proof was nine ChronosFlow rows
/// (0/9, all coverage-capped at 0.66) scored on 2026-10-15 (26c5139b). Their
/// dead labels are lost, so the rows are carried and unverified in
/// `record.json` (`carried_dead_evidence`) and no longer count; re-labelling
/// is tracked by ft-8ed75897. **Swift**, measured on ten corpora with no confident-branch
/// row on any — its examined population is `private` members something
/// reaches — so nothing beyond one fixture row has ever tested it; **Python**,
/// 17 of 58 corpus rows false (string dispatch, an aliased import, module
/// constants read from other files, calls through a Protocol), causes the
/// L-Python and interface-dispatch cards own. Each comes back when the record
/// says so.
pub const CONFIDENT_TIER_ELIGIBLE: &[&str] = &["go", "rust", "tsx", "typescript"];

/// Whether `language` may publish dead rows in the confident tier.
pub fn confident_tier_eligible(language: &str) -> bool {
    CONFIDENT_TIER_ELIGIBLE.contains(&language)
}

/// The machine token every row capped by [`CONFIDENT_TIER_ELIGIBLE`] starts with.
pub const CONFIDENT_TIER_UNPROVEN: &str = "confident_tier_unproven";

/// Where a confident row of an unlisted language lands: the unconfirmed tier,
/// beside `only_ambiguous_callers`, so it stays visible and is never acted on.
pub const UNPROVEN_TIER_CONFIDENCE: f32 = 0.4;

/// Reason carried by a confident row whose language has not earned the tier.
///
/// Same shape as [`CALL_BLIND_REASON`] and the coverage cap: the evidence for
/// death is the same "no inbound edge" it always was, and the reason says why
/// that evidence is not yet trusted for this language.
pub fn confident_tier_unproven_reason(language: &str) -> String {
    format!(
        "{CONFIDENT_TIER_UNPROVEN}: no inbound edge names this symbol, but the {language} \
         confident tier has not been proven against its labelled fixture and the corpus \
         record — capped below the act-on tier until CONFIDENT_TIER_ELIGIBLE lists {language}"
    )
}

/// Whether a dead row's evidence earned the confident tier: it was published
/// there, or it carries [`CONFIDENT_TIER_UNPROVEN`] because its language has not
/// earned the tier yet.
///
/// The one question a caller asking "did the analysis call this confidently
/// dead?" means. Reading `confidence >= 0.9` alone answers a different one —
/// "is this language listed?" — and a check that a C function *reached* the
/// confident branch would start failing, or worse passing, for that reason.
pub fn earned_confident_tier(report: &DeadSymbolReport) -> bool {
    !report.is_exempt
        && (report.confidence > HIGHEST_DEGRADED_CONFIDENCE
            || report
                .exemption_reason
                .as_deref()
                .is_some_and(|reason| reason.starts_with(CONFIDENT_TIER_UNPROVEN)))
}

/// The machine token an `exported_unreferenced` row starts with.
pub const EXPORTED_UNREFERENCED: &str = "exported_unreferenced";

/// Reason carried by an exported symbol the manifest proves module-private.
fn exported_unreferenced_reason(proof: &str) -> String {
    format!(
        "{EXPORTED_UNREFERENCED}: exported, but {proof}, and nothing in the corpus reaches it — \
         unconfirmed until this language's entry-point model is proven"
    )
}

/// The machine token a row starts with when its class inherits from a type the
/// corpus does not declare.
pub const EXTERNAL_SUPERTYPE: &str = "external_supertype";

/// The machine token a row starts with when it is a member of an object literal.
pub const OBJECT_LITERAL_MEMBER: &str = "object_literal_member";

/// Languages where a subclass overrides a base method with no keyword saying so.
///
/// Kotlin, Java, Swift, C# and Scala require `override` (or `@Override`), and
/// the extractor already exempts what carries it; a method there without one
/// cannot be what a base class calls. Python and TypeScript/JavaScript say
/// nothing at the override, so an uncalled method on a subclass of a library
/// type may be exactly what the library calls — CodeMirror's `WidgetType.toDOM`,
/// `HTMLParser.handle_starttag`, watchdog's `on_any_event`. The 2026-10-10
/// corpus record measured those as confident false positives in both languages.
fn overrides_without_keyword(language: &str) -> bool {
    matches!(language, "python" | "typescript" | "tsx" | "javascript")
}

/// Supertypes that call no subclass method by name, so inheriting from one is
/// no evidence that an uncalled method is reached.
///
/// `object`, the abstract-base and generic markers, enums and record shapes
/// (pydantic's `BaseModel` among them: it calls decorated validators and
/// named hooks, never a plain method a model adds), and exception types (by name, including a library's `HTTPException` or a
/// `ValidationError`): none of them dispatches to a method a subclass adds.
/// Dunder hooks are already outside the population (leading underscore).
///
/// For Swift, the value-semantics and marker protocols whose requirements are
/// operators or compiler-synthesised members (`Equatable`, `Hashable`,
/// `Codable`, `Sendable`, `Identifiable`, …) and `NSObject` itself: conforming
/// to one says nothing about who calls the type's other members.
fn supertype_dispatches_nothing(name: &str) -> bool {
    matches!(
        name,
        "object"
            | "ABC"
            | "Generic"
            | "Enum"
            | "IntEnum"
            | "StrEnum"
            | "Flag"
            | "NamedTuple"
            | "TypedDict"
            | "BaseModel"
            | "NSObject"
            | "Equatable"
            | "Hashable"
            | "Comparable"
            | "Codable"
            | "Encodable"
            | "Decodable"
            | "Sendable"
            | "Identifiable"
            | "CaseIterable"
            | "RawRepresentable"
            | "ObservableObject"
            | "AnyObject"
    ) || name.ends_with("Error")
        || name.ends_with("Exception")
}

/// `(file, declaring type)` → the first supertype it names that the corpus does
/// not declare, for the languages [`overrides_without_keyword`] covers.
///
/// A supertype is in the corpus when a resolved `Extends` edge from the type
/// names it ([`supertypes_by_type`]); a heritage reference with no such edge
/// is a library type, or one this build could not find — either way something
/// outside the graph may call the type's methods. A base whose contract the
/// wiring registry knows (`known_dispatch_contract`: `torch.autograd.Function`,
/// `torch.nn.Module`, `HTMLElement`, React class components), matched on the
/// base as [`import_qualified`] names it, is not counted: its dispatched
/// methods are exempt by name and the rest stay ordinary candidates.
fn external_supertypes<'e>(
    extractions: &'e [Extraction],
    supertypes: &HashMap<(&str, &str), Vec<(&str, &str)>>,
) -> HashMap<(&'e str, &'e str), &'e str> {
    let mut out = HashMap::new();
    for ext in extractions {
        // Swift is tracked for the conformance half: a framework calls a
        // protocol requirement's witness by name (`swift_member_may_witness`).
        if !overrides_without_keyword(ext.code_language()) && ext.code_language() != "swift" {
            continue;
        }
        // A dotted base arrives as one `Heritage` reference per segment over
        // one span, innermost first (`Function`, `autograd`, `torch`), so the
        // written base is the segments reversed; the bare name is the first.
        let mut bases: Vec<(&str, usize, Vec<&str>)> = Vec::new();
        for reference in &ext.references {
            // `implements` names an interface, which has no code to call with.
            if reference.kind != ReferenceKind::Heritage {
                continue;
            }
            let Some(declarer) = reference
                .enclosing_symbol
                .as_deref()
                .and_then(|owner| owner.rsplit("::").next())
            else {
                continue;
            };
            let start = reference.span.start_byte;
            match bases
                .iter_mut()
                .find(|(owner, at, _)| *owner == declarer && *at == start)
            {
                Some((_, _, segments)) => segments.push(reference.name.as_str()),
                None => bases.push((declarer, start, vec![reference.name.as_str()])),
            }
        }
        for (declarer, _, segments) in bases {
            let base = segments[0].rsplit('.').next().unwrap_or(segments[0]);
            if base.is_empty() || supertype_dispatches_nothing(base) {
                continue;
            }
            let written: Vec<&str> = segments.iter().rev().copied().collect();
            if devmap_extract::wiring::known_dispatch_contract(&import_qualified(ext, &written)) {
                continue;
            }
            let in_corpus = supertypes
                .get(&(ext.file_path.as_str(), declarer))
                .is_some_and(|supers| supers.iter().any(|(_, name)| *name == base));
            if !in_corpus {
                out.entry((ext.file_path.as_str(), declarer))
                    .or_insert(segments[0]);
            }
        }
    }
    out
}

/// A written base (`["nn", "Module"]`) with its root segment replaced by what
/// the file's imports bind it to (`torch.nn.Module` under `import torch.nn as
/// nn`), so the dispatch registry matches the library and not a spelling.
///
/// * `from M import X as L` / `import { X as L } from 'M'` binds `L` to `M.X`.
/// * `import M as L` / `import L from 'M'` / `import * as L from 'M'` binds `L`
///   to `M` (a JS default import is the module's own export).
/// * `import torch.nn` binds `torch`, the module's first segment, to itself.
///
/// A root no import binds is returned as written: a global (`HTMLElement`) or
/// a name declared in the file.
fn import_qualified(ext: &Extraction, written: &[&str]) -> String {
    let Some((root, rest)) = written.split_first() else {
        return String::new();
    };
    let bound = ext.imports.iter().find_map(|import| {
        let spec = import.module_specifier.as_str();
        let named = import
            .local_names
            .iter()
            .zip(&import.imported_names)
            .find(|(local, _)| local.as_str() == *root)
            .map(|(_, imported)| {
                if imported == "*" {
                    spec.to_string()
                } else {
                    format!("{spec}.{imported}")
                }
            });
        named
            .or_else(|| (import.alias.as_deref() == Some(*root)).then(|| spec.to_string()))
            .or_else(|| {
                (import.alias.is_none()
                    && import.imported_names.is_empty()
                    && spec.split('.').next() == Some(*root))
                .then(|| root.to_string())
            })
    });
    let mut qualified = bound.unwrap_or_else(|| root.to_string());
    for segment in rest {
        qualified.push('.');
        qualified.push_str(segment);
    }
    qualified
}

/// The external supertype `declaring_type` inherits from, directly or through
/// in-corpus ancestors, walked as [`reached_through_a_supertype`] walks them.
fn external_supertype_of<'e>(
    file: &str,
    declaring_type: &str,
    supertypes: &HashMap<(&str, &str), Vec<(&str, &str)>>,
    external: &HashMap<(&'e str, &'e str), &'e str>,
) -> Option<&'e str> {
    let mut frontier = vec![(file, declaring_type)];
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    for _ in 0..HERITAGE_WALK_MAX_DEPTH {
        let mut next = Vec::new();
        for key in frontier {
            if !seen.insert(key) {
                continue;
            }
            if let Some(base) = external.get(&key) {
                return Some(base);
            }
            next.extend(supertypes.get(&key).into_iter().flatten().copied());
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    None
}

/// Why an uncalled method that would otherwise be confident is unconfirmed:
/// something outside the call graph dispatches to it by name.
///
/// * **`object_literal_member`** — a TypeScript/JavaScript method declared in
///   an object literal (`{ startState() {…}, token() {…} }`,
///   `components={{ pre() {…} }}`) is reached through the object, by key, by
///   whatever the object is handed to. The extractor parents it on the file,
///   or on the module-level `const` that holds the object (`parser.startState`),
///   never on a class, which is how it is told from a class method.
/// * **`external_supertype`** — a method on a class whose supertype chain
///   reaches a type the corpus does not declare, in a language where an
///   override is not marked.
///
/// Visible at 0.4 with the reason, never exempt: either can also be dead, and
/// the reader is told which check to make.
fn unconfirmed_dispatch_reason(
    ext: &Extraction,
    sym: &ExtractedSymbol,
    identity: &str,
    supertypes: &HashMap<(&str, &str), Vec<(&str, &str)>>,
    external: &HashMap<(&str, &str), &str>,
) -> Option<String> {
    let language = ext.code_language();
    if sym.kind != SymbolKind::Method || !overrides_without_keyword(language) {
        return None;
    }
    if matches!(language, "typescript" | "tsx" | "javascript")
        && sym.parent_symbol.as_deref().is_none_or(|parent| {
            parent == ext.file_path
                || ext.symbols.iter().any(|owner| {
                    owner.qualified_name == parent && owner.kind == SymbolKind::Variable
                })
        })
    {
        return Some(format!(
            "{OBJECT_LITERAL_MEMBER}: a method of an object literal, reached through the object \
             by key wherever the object is passed — no call names it, which is not evidence \
             nothing calls it"
        ));
    }
    let (declaring_type, _) = identity.rsplit_once('.')?;
    let base = external_supertype_of(&ext.file_path, declaring_type, supertypes, external)?;
    Some(format!(
        "{EXTERNAL_SUPERTYPE}: its class inherits from `{base}`, which the corpus does not \
         declare, and a base class calls a subclass's methods by name — no call names it, \
         which is not evidence nothing calls it"
    ))
}

/// Why an uncalled symbol was not examined at all: it is exported.
///
/// The visibility, not any evidence, keeps it off the list. Counted per
/// language in the `excluded` disclosure, because a `dead` answer of zero over
/// a corpus whose every uncalled symbol is exported is not a finding.
pub const EXPORTED_NOT_EXAMINED_REASON: &str =
    "exported — not examined: its visibility, not any evidence, keeps it off the dead list";

/// Why an uncalled, unexported symbol was exempted with its file when the file
/// carried no more specific reason.
pub const FILE_EXEMPT_REASON: &str =
    "exempt with its file — the file is outside the dead-code population";

/// The stable token for a stored exemption reason, for counting.
///
/// Reasons are sentences, and several embed a name (`implements interface
/// \`Reader\``), so counting by the raw string would split one cause across as
/// many rows as it has names. This is the one owner of the mapping, read by the
/// `excluded` disclosure; a reason this does not recognise is a wiring
/// annotation's details — a runtime entry point, a framework hook, a test or
/// generated file — and is counted as `wiring`.
pub fn exemption_token(reason: &str) -> &'static str {
    // The second spelling is the fallback generations before 2026-10-09
    // stored for the same case, so an old store still counts it.
    if reason == EXPORTED_NOT_EXAMINED_REASON || reason == "Exported or exempt" {
        "exported"
    } else if reason == "Member of an exported type — public interface" {
        "exported_owner"
    } else if reason.starts_with("Overrides a method reached through") {
        "heritage_override"
    } else if reason.starts_with("implements interface `") {
        "interface_implementation"
    } else if reason == GO_STD_INTERFACE_REASON {
        "std_interface_implementation"
    } else if reason == "Declared in a C-family header — public interface" {
        "c_header"
    } else if reason == GO_BUILD_VARIANT_REASON {
        "go_build_variant"
    } else if reason == GO_VALUE_MENTION_REASON {
        "go_value_mention"
    } else if reason == "Symbol overlaps a tree-sitter parse error" {
        "parse_error_overlap"
    } else if reason == FILE_EXEMPT_REASON {
        "file_exempt"
    } else if reason.ends_with("excluded from dead code candidates") {
        "not_parsed"
    } else {
        "wiring"
    }
}

/// Exempt-reason tokens that are visibility rather than evidence.
///
/// What the `excluded` disclosure sums as "not examined": nothing reached these
/// symbols; a keyword or a language default kept them off the list.
pub const VISIBILITY_TOKENS: &[&str] = &["exported", "exported_owner"];

/// Why an exported Go method is exempt when a standard-library interface could
/// dispatch to it.
pub const GO_STD_INTERFACE_REASON: &str =
    "satisfies a standard-library interface by name and arity (fmt.Stringer, error, io.Reader, \
     http.Handler, json.Marshaler, sort.Interface, …); calls reach it through the interface";

/// `(method, parameter count)` of the standard-library interfaces a type in a
/// module-private package most often satisfies without a single in-corpus call.
///
/// Only consulted when widening exported Go symbols into the unconfirmed tier,
/// where it errs toward withholding: a `String()` on an `internal/` type is
/// called by `fmt`, never by the corpus.
const GO_STD_INTERFACE_METHODS: &[(&str, usize)] = &[
    ("String", 0),
    ("GoString", 0),
    ("Error", 0),
    ("Unwrap", 0),
    ("Is", 1),
    ("As", 1),
    ("Format", 2),
    ("Read", 1),
    ("Write", 1),
    ("Close", 0),
    ("Seek", 2),
    ("ReadFrom", 1),
    ("WriteTo", 1),
    ("ServeHTTP", 2),
    ("RoundTrip", 1),
    ("MarshalJSON", 0),
    ("UnmarshalJSON", 1),
    ("MarshalText", 0),
    ("UnmarshalText", 1),
    ("MarshalYAML", 0),
    ("UnmarshalYAML", 1),
    ("MarshalBinary", 0),
    ("UnmarshalBinary", 1),
    ("Scan", 1),
    ("Value", 0),
    ("Len", 0),
    ("Less", 2),
    ("Swap", 2),
    ("Lock", 0),
    ("Unlock", 0),
];

/// Whether a Go file's package is provably private to its module.
///
/// Go's own rules, nothing inferred: an `internal/` directory is importable
/// only from the tree rooted at its parent, and nothing can import `package
/// main`. Either way an exported name proves nothing about callers outside the
/// corpus, so an uncalled one is a candidate — at the unconfirmed tier, until
/// Go's interface dispatch is modelled.
fn go_module_private_proof(ext: &Extraction) -> Option<&'static str> {
    if ext.language != "go" {
        return None;
    }
    if ext.go_package.as_deref() == Some("main") {
        return Some("nothing can import Go `package main`");
    }
    let dir = parent_dir(&ext.file_path);
    if dir == "internal"
        || dir.starts_with("internal/")
        || dir.contains("/internal/")
        || dir.ends_with("/internal")
    {
        return Some(
            "Go permits importing an `internal/` package only from inside its parent tree",
        );
    }
    None
}

/// Which kind of hole one file leaves in call coverage.
///
/// The two are not interchangeable and are never folded into one number: a
/// `ParseFailed` file contributed nothing at all, a `PatternRecovered` one
/// contributed names and spans but, by construction, no calls and no imports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractionGap {
    ParseFailed,
    PatternRecovered,
    /// A grammar read the file cleanly and this build has no call extractor for
    /// its language.
    ///
    /// The hole the other two could not see. A `.cfm` or a `.tf` parses
    /// `Clean`, so `is_parse_failure` is false and `Fallback` never matches —
    /// the file sailed past both gaps, `is_complete()` stayed true, and every
    /// top-level symbol in it was published at the `extracted` tier, the one
    /// `CLAUDE.md` tells agents is safe to act on. "Nothing calls it" was a
    /// statement about the extractor and read as a statement about the code.
    CallBlind,
    /// A grammar read the file cleanly and this build extracts no imports for
    /// its language.
    ///
    /// Charged separately from [`Self::CallBlind`] and **not** folded into
    /// `is_complete()`: it undermines `unwired_candidates`, not the dead-symbol
    /// verdict. See `ExtractionCoverage::is_complete`.
    ImportBlind,
    /// The extractor decided not to parse the file at all.
    ///
    /// Its own kind because the alternative is silence. A skipped file is not a
    /// parse failure (nothing failed), is not pattern-recovered (nothing was
    /// matched) and no grammar read it — so it matched none of the four arms
    /// above and fell out of the inventory entirely, reported as a file whose
    /// calls were looked for and found to be none. `discovery_refused_files`
    /// exists because that exact silence, for a file discovery dropped, cost a
    /// `helper()` a confident delete verdict.
    NotParsed,
    /// A grammar read the file and its tree holds `ERROR` or `MISSING` nodes.
    ///
    /// Not `ParseFailed` — the file contributed a `File` node and whatever was
    /// declared outside the damage — and not clean either: tree-sitter's error
    /// recovery can swallow everything after the fault. A `.tsx` holding
    /// `type R<T> = import('m').Ref<T>;` loses every later declaration
    /// (ScholarLM's `AuthContext.tsx` lost `AuthProvider` and `useAuth`), and a
    /// Kotlin `suspend { … }` lambda opens an error region that swallows the
    /// rest of the file. None of that appeared in any coverage inventory, so a
    /// caller inside the damage was a caller nobody could know was missing.
    PartialParse,
}

impl ExtractionGap {
    /// Every kind, so the store's read side can be checked against the write
    /// side rather than trusted.
    ///
    /// The write path stores whatever `label()` returns; the read path matches
    /// a fixed list. A variant in the first and not the second is a row written
    /// on every build and never read back — which is precisely what happened to
    /// `CallBlind` and `ImportBlind` between their introduction and the test
    /// that now iterates this.
    pub const ALL: &'static [ExtractionGap] = &[
        ExtractionGap::ParseFailed,
        ExtractionGap::PatternRecovered,
        ExtractionGap::CallBlind,
        ExtractionGap::ImportBlind,
        ExtractionGap::NotParsed,
        ExtractionGap::PartialParse,
    ];

    /// The stored spelling, and the one a consumer reads back. One owner, so a
    /// persisted inventory and an in-memory count cannot disagree about what a
    /// gap is called.
    pub fn label(self) -> &'static str {
        match self {
            ExtractionGap::ParseFailed => "parse_failed",
            ExtractionGap::PatternRecovered => "pattern_recovered",
            ExtractionGap::CallBlind => "call_blind",
            ExtractionGap::ImportBlind => "import_blind",
            ExtractionGap::NotParsed => "not_parsed",
            ExtractionGap::PartialParse => "partial_parse",
        }
    }
}

/// How far the override join walks a heritage chain.
///
/// `Derived -> Middle -> Base` is two hops and ordinary; a bound past this is
/// either generated code or a cycle, and a cycle in a heritage graph is not
/// expressible in any language here but is expressible in a *graph*, which is
/// what this walks. Bounded rather than trusted.
const HERITAGE_WALK_MAX_DEPTH: usize = 8;

/// Reason carried by an override the base type's call reaches.
///
/// Names the supertype, because the exemption is only as good as the edge
/// behind it and a reader deciding whether to trust it needs to see which
/// relation fired.
fn heritage_override_reason(supertype: &str) -> String {
    format!(
        "Overrides a method reached through `{supertype}` — polymorphic dispatch, \
         matched by name on a resolved heritage edge"
    )
}

/// `(file, TypeName)` -> the supertypes it declares, from `Extends`/`Implements`.
///
/// These edges exist at all only since W1.2: `ReferenceKind::Heritage` and both
/// edge kinds were declared with no producer, so a method reached only through
/// its base type had no inbound edge and every override was a candidate
/// `extracted` false positive.
///
/// Ambiguous edges are excluded for the same reason the call join excludes
/// them: an unresolved supertype is evidence that a base *might* exist, not
/// proof of which one, and a speculative edge must not exempt a symbol.
fn supertypes_by_type(resolution: &ResolutionResult) -> HashMap<(&str, &str), Vec<(&str, &str)>> {
    let mut by_type: HashMap<(&str, &str), Vec<(&str, &str)>> = HashMap::new();
    for edge in &resolution.edges {
        if !matches!(edge.edge_kind, EdgeKind::Extends | EdgeKind::Implements) {
            continue;
        }
        if matches!(
            edge.resolution.as_deref(),
            Some(Resolution::AmbiguousGlobal { .. })
                | Some(Resolution::LanguageServerDispatch { .. })
                | Some(Resolution::Unresolved { .. })
        ) {
            continue;
        }
        let Some(declarer) = edge.source_symbol.rsplit("::").next() else {
            continue;
        };
        let Some(supertype) = edge.target_symbol.rsplit("::").next() else {
            continue;
        };
        by_type
            .entry((edge.source_file.as_str(), declarer))
            .or_default()
            .push((edge.target_file.as_str(), supertype));
    }
    by_type
}

/// Whether a call to a supertype's same-named method reaches this override.
///
/// The generalisation of the Go interface pre-pass, which matches on name plus
/// arity within one package and produces a wiring exemption. This one runs on a
/// real resolved edge, so it works across files and across languages, and it
/// still matches the *method* by name only — a supertype's `render` and an
/// override's `render` are joined because they are spelled the same, which is
/// what an override is.
/// Whether a member's visibility is a naming convention rather than a keyword.
///
/// The distinction decides whether a member may inherit its owner's export.
///
/// Java, C#, TypeScript, PHP, Kotlin and Swift all have `private`, and the
/// extractor reads it: a `private void used()` arrives with
/// `is_exported: false` and a `public void run()` with `true`. For those, a
/// member that reaches the dead-symbol branch has already said it is not public
/// and must keep being reported — measured against
/// `extraction_coverage_liveness.rs`, whose Java fixture this rule wrongly
/// exempted on the first attempt.
///
/// Python has no such keyword. Every method arrives `is_exported: false`
/// whatever its intent, so the flag carries no information and the leading
/// underscore is the whole convention — already applied before this point. What
/// is left is a public member, and if its class is named in `__all__` then
/// deleting it breaks consumers outside the corpus.
///
/// Deliberately a list of one. Adding a language here is a claim that its
/// extractor cannot mark a public member exported, which is checkable and
/// should be checked rather than assumed.
fn member_visibility_is_conventional(language: &str) -> bool {
    language == "python"
}

fn reached_through_a_supertype(
    file: &str,
    identity: &str,
    supertypes: &HashMap<(&str, &str), Vec<(&str, &str)>>,
    called: &HashSet<(String, String)>,
) -> Option<String> {
    // Only a method can be an override: `Type.method` is the identity shape
    // `dead_symbol_identity` produces, and a bare name is a free function.
    let (declaring_type, method) = identity.rsplit_once('.')?;

    let mut frontier = vec![(file, declaring_type)];
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    for _ in 0..HERITAGE_WALK_MAX_DEPTH {
        let mut next = Vec::new();
        for key in frontier {
            if !seen.insert(key) {
                continue;
            }
            for (super_file, super_name) in supertypes.get(&key).into_iter().flatten() {
                // Either spelling of the call target: the qualified
                // `Base.render` an edge names, or the bare `render` the short
                // name is also recorded under.
                let qualified = format!("{super_name}.{method}");
                if called.contains(&(super_file.to_string(), qualified))
                    || called.contains(&(super_file.to_string(), method.to_string()))
                {
                    return Some(heritage_override_reason(super_name));
                }
                next.push((*super_file, *super_name));
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    None
}

/// Reason carried by a finding an unresolved call site vetoed.
///
/// States the imprecision in the reason itself rather than in a code comment,
/// because the reason is what a reader acts on. `UnresolvedReference` carries
/// no `target_file`, so the join is name-only and corpus-wide — the same trade
/// `c_header_exported_names` already makes for C headers, and for the same
/// reason: the alternative is a confident verdict resting on a resolver's
/// failure.
pub const UNRESOLVED_NAMESAKE_REASON: &str =
    "an unresolved call site names this symbol — the resolver could not bind that site to \
     anything, so \"nothing calls this\" is a statement about the resolver, not the code \
     (matched by name across the whole corpus; the ledger records no target file)";

/// Names that some call site meant and the resolver could not bind.
///
/// The kernel keeps a six-tier ledger of every site the resolution ladder gave
/// up on, and the dead-code pass never read it. If an unresolved site names
/// `foo`, then "nothing calls `foo`" describes the resolver rather than the
/// code, and publishing it at the `extracted` tier — whose contract is "safe to
/// act on" — is the unattributed tier silently manufacturing confident
/// findings.
///
/// **Three classes qualify for the veto, not every non-builtin.** Classes
/// with affirmative evidence that the site meant something else — `Builtin`,
/// `HostGlobal`, `External`, `NoNamesake`, `ModulePath` — must not veto. What is
/// left is the three the ledger itself calls unattributed
/// ([`UNATTRIBUTED_LABELS`]): `UninferredReceiver`, `UnresolvedMember`, `Unresolved`, and
/// `LocalBinding`.
///
/// `LocalBinding` joined on 2026-10-09. It was excluded on the theory that a
/// call to a name the enclosing scope binds is a callback, never a symbol —
/// and the audit measured the theory wrong in exactly the shapes where it
/// mattered: `let (n, shadow_b) = (1, opt.and_then(|p| shadow_b(p)))` (Rust
/// C48) calls the *function* inside the initializer of the pattern that binds
/// the name, `{ cwd = repoRoot() }` (a TypeScript destructured default) calls
/// it while binding `cwd`, and a Go function-local `type entry` shadowed by a
/// later `for _, entry := range` loses its only reference the same way. Each
/// was published at 0.9 and each was live. The classifier is right that the
/// scope binds the name; it cannot see *which* of the name's bindings a given
/// site reached, so the honest reading is the one the other two classes get:
/// "the resolver could not prove which", not "nothing calls this".
///
/// `Route` joins `Call` and `Reference` as an admitted kind because a route
/// handler that failed to bind is the strongest version of this case: the
/// `HandlesRoute` edge is what tells liveness a handler is reached from outside
/// the call graph at all, so an unbound one leaves a live handler looking dead.
/// `Import` is excluded because its `callee_name` is a *module specifier*, not
/// a symbol name, and matching specifiers against symbols is noise.
pub(crate) fn unresolved_namesake_names(resolution: &ResolutionResult) -> HashSet<&str> {
    resolution
        .unresolved
        .iter()
        .filter(|row| is_namesake_veto_row(row))
        .map(|row| row.callee_name.as_str())
        .filter(|name| !name.is_empty())
        .collect()
}

/// The one predicate for "this ledger row may be a call to a symbol of its
/// name", shared by the veto and by the site evidence an unconfirmed row
/// carries — so the count a reader sees is the count that demoted the row.
fn is_namesake_veto_row(row: &UnresolvedReference) -> bool {
    matches!(
        row.kind,
        UnresolvedKind::Call | UnresolvedKind::Reference | UnresolvedKind::Route
    ) && matches!(
        row.class,
        UnresolvedClass::UninferredReceiver
            | UnresolvedClass::UnresolvedMember
            | UnresolvedClass::Unresolved
            | UnresolvedClass::LocalBinding
    ) && match &row.resolution {
        Resolution::Unresolved { reason } => {
            reason != "implicit receiver method is overridden by an indexed subtype"
        }
        _ => true,
    }
}

/// Whether a resolved edge is one candidate of several rather than a binding.
///
/// Shared by the liveness join and the site evidence, so an edge counted as an
/// ambiguous caller is exactly an edge that made the row `only_ambiguous_callers`.
fn is_ambiguous_edge(edge: &ResolvedEdge) -> bool {
    !matches!(
        edge.edge_kind,
        EdgeKind::Contains | EdgeKind::Defines | EdgeKind::MemberOf
    ) && edge
        .resolution
        .as_deref()
        .is_some_and(Resolution::is_unproven_candidate_set)
}

/// The reason a symbol reached only through interface dispatch carries.
pub const REACHED_BY_DISPATCH: &str = "reached_by_dispatch";

/// Targets whose only unproven callers are interface dispatch edges.
///
/// A dispatch candidate and an ambiguous name match both leave a symbol at
/// 0.4, but they are different evidence — the first is a type-system claim that
/// the symbol *implements* what was called — and the reason says which. Keyed
/// the way [`called_and_ambiguous_symbols`] keys its sets (qualified, short and
/// member spellings), so the join cannot miss on spelling.
fn dispatch_only_symbols(resolution: &ResolutionResult) -> HashSet<(String, String)> {
    let spellings = |edge: &ResolvedEdge| -> Vec<(String, String)> {
        let short = edge
            .target_symbol
            .rsplit("::")
            .next()
            .unwrap_or(edge.target_symbol.as_str());
        let member = short.rsplit('.').next().unwrap_or(short);
        [edge.target_symbol.as_str(), short, member]
            .into_iter()
            .map(|form| (edge.target_file.clone(), form.to_string()))
            .collect()
    };
    let mut dispatched: HashSet<(String, String)> = HashSet::new();
    let mut other: HashSet<(String, String)> = HashSet::new();
    for edge in resolution
        .edges
        .iter()
        .filter(|edge| is_ambiguous_edge(edge))
    {
        let bucket = if matches!(
            edge.resolution.as_deref(),
            Some(Resolution::Dispatch { .. })
        ) {
            &mut dispatched
        } else {
            &mut other
        };
        bucket.extend(spellings(edge));
    }
    dispatched.retain(|key| !other.contains(key));
    dispatched
}

/// The manifests that scope module privacy, keyed by manifest family, each as
/// `(directory, proof)` — `proof` is `None` for a manifest that publishes its
/// package (a library), which is what stops a library nested in an application
/// from inheriting the application's verdict.
type ManifestScopes<'a> = HashMap<&'static str, Vec<(&'a str, Option<&'a str>)>>;

/// Every module manifest in the corpus, by family. See
/// [`devmap_extract::wiring::module_manifest_family`].
fn manifest_scopes(extractions: &[Extraction]) -> ManifestScopes<'_> {
    let mut scopes: ManifestScopes<'_> = HashMap::new();
    let indexed: HashSet<&str> = extractions
        .iter()
        .map(|ext| ext.file_path.as_str())
        .collect();
    for ext in extractions {
        let Some(family) = devmap_extract::wiring::module_manifest_family(&ext.file_path) else {
            continue;
        };
        // A Swift package proves privacy per executable target: the package
        // directory itself is a scope that proves nothing (so a library target
        // beside the executable is not widened), and each executable target's
        // directory, deeper and so nearer, carries the proof.
        if family == "swiftpm" {
            let entry = scopes.entry(family).or_default();
            entry.push((parent_dir(&ext.file_path), None));
            for annotation in &ext.wiring {
                if annotation.kind == WiringKind::ModulePrivateScope {
                    entry.push((
                        annotation.target_symbol.as_str(),
                        Some(annotation.details.as_str()),
                    ));
                }
            }
            continue;
        }
        let mut proof = ext
            .wiring
            .iter()
            .find(|annotation| annotation.kind == WiringKind::ModulePrivateScope)
            .map(|annotation| annotation.details.as_str());
        // A `Cargo.toml` with no `[lib]` still gets one when `src/lib.rs`
        // exists — Cargo discovers it — and the manifest cannot see that file.
        if family == "cargo" {
            let dir = parent_dir(&ext.file_path);
            let lib = if dir.is_empty() {
                "src/lib.rs".to_string()
            } else {
                format!("{dir}/src/lib.rs")
            };
            if indexed.contains(lib.as_str()) {
                proof = None;
            }
        }
        // `App.xcodeproj/project.pbxproj` scopes the directory holding the
        // project bundle, not the bundle itself.
        let dir = if family == "xcode" {
            parent_dir(parent_dir(&ext.file_path))
        } else {
            parent_dir(&ext.file_path)
        };
        scopes.entry(family).or_default().push((dir, proof));
    }
    scopes
}

/// The proof the nearest manifest of `family` above `path` gives, if it gives
/// one. The nearest wins, by directory depth.
fn nearest_manifest_proof<'a>(
    scopes: &ManifestScopes<'a>,
    family: &str,
    path: &str,
) -> Option<&'a str> {
    nearest_manifest(scopes, family, path).flatten()
}

/// The nearest manifest of `family` above `path`: `None` when there is none,
/// `Some(proof)` — itself possibly `None` — when there is.
fn nearest_manifest<'a>(
    scopes: &ManifestScopes<'a>,
    family: &str,
    path: &str,
) -> Option<Option<&'a str>> {
    scopes
        .get(family)?
        .iter()
        .filter(|(dir, _)| dir.is_empty() || path.starts_with(&format!("{dir}/")))
        .max_by_key(|(dir, _)| dir.len())
        .map(|(_, proof)| *proof)
}

/// Why an exported symbol is nonetheless private to its module, or `None` when
/// nothing in the manifest or the language proves it.
///
/// The one owner of the widening. A language enters here only once its
/// entry-point model is in place, because before that the population this
/// opens is mostly framework callbacks (measured on Swift: about 15 of 26
/// hidden internal-uncalled symbols):
///
/// * **Go** — `internal/` and `package main`, by the language's own import rule.
/// * **TypeScript / JavaScript** — a package whose `package.json` publishes no
///   entry point; framework lifecycle methods and JSX uses are already edges or
///   exemptions.
/// * **Kotlin / Java** — a Gradle module applying `com.android.application`,
///   once Android components (by supertype), DI and serialization annotations,
///   `override`s and `@Preview` are entry points.
///
/// * **Rust** — an item written `pub(crate)`, `pub(super)`, `pub(self)` or
///   `pub(in path)`, which the language confines to its crate (a per-symbol
///   annotation from the extractor); and every `pub` item of a package whose
///   `Cargo.toml` declares no library target and which has no `src/lib.rs`.
///   Trait-impl methods, `#[no_mangle]`/`extern "C"`, `#[tauri::command]` and
///   the binary entry points are exempt before this point.
///
/// * **Swift** — an internal symbol of an `executableTarget` in `Package.swift`,
///   or of an Xcode project whose every product is an application, extension
///   or test bundle; a file inside a Swift package is judged by the package
///   alone. Withheld — left not examined — when its type conforms to a
///   protocol the corpus does not declare ([`swift_member_may_witness`]): a
///   framework calls a requirement's witness by name, and an internal member
///   can be one. `@objc`, `@IBAction`, `override`, `@main` and the runtime
///   supertypes are exempt before this point.
fn module_private_proof<'a>(
    ext: &'a Extraction,
    sym: &ExtractedSymbol,
    scopes: &ManifestScopes<'a>,
    external_bases: &HashMap<(&str, &str), &str>,
    supertypes: &HashMap<(&str, &str), Vec<(&str, &str)>>,
) -> Option<&'a str> {
    if let Some(proof) = go_module_private_proof(ext) {
        return Some(proof);
    }
    let family = match ext.code_language() {
        "typescript" | "tsx" | "javascript" => "js",
        "kotlin" | "java" => "gradle",
        "rust" => {
            if let Some(annotation) = ext.wiring.iter().find(|annotation| {
                annotation.kind == WiringKind::ModulePrivateScope
                    && annotation.target_symbol == sym.qualified_name
            }) {
                return Some(annotation.details.as_str());
            }
            "cargo"
        }
        "swift" => {
            if swift_member_may_witness(ext, sym, external_bases, supertypes) {
                return None;
            }
            if let Some(proof) = nearest_manifest(scopes, "swiftpm", &ext.file_path) {
                return proof;
            }
            "xcode"
        }
        _ => return None,
    };
    nearest_manifest_proof(scopes, family, &ext.file_path)
}

/// How many ambiguous call sites reach a slot, and the smallest
/// `(path, enclosing symbol, callee)` among them.
type NamesakeSites<'a> = (u32, Option<(&'a str, &'a str, &'a str)>);

/// Counts one call site against a slot and keeps the smallest site, so the
/// reported example does not depend on edge order.
fn record_namesake_site<'a>(entry: &mut NamesakeSites<'a>, site: (&'a str, &'a str, &'a str)) {
    entry.0 = entry.0.saturating_add(1);
    if entry
        .1
        .is_none_or(|best| (site.0, site.1) < (best.0, best.1))
    {
        entry.1 = Some(site);
    }
}

/// Whether a Swift member may be the witness of a requirement of a protocol the
/// corpus does not declare.
///
/// `extension Controller: NSTableViewDelegate { func tableViewSelectionDidChange… }`
/// and `struct Badge: ViewModifier { func body… }` are called by AppKit and
/// SwiftUI by requirement name. Which members are requirements is in the SDK,
/// not the corpus, so every member of a conforming type is withheld; the
/// marker protocols [`supertype_dispatches_nothing`] lists do not count.
fn swift_member_may_witness(
    ext: &Extraction,
    sym: &ExtractedSymbol,
    external_bases: &HashMap<(&str, &str), &str>,
    supertypes: &HashMap<(&str, &str), Vec<(&str, &str)>>,
) -> bool {
    let Some(owner) = sym
        .parent_symbol
        .as_deref()
        .filter(|parent| *parent != ext.file_path)
        .and_then(|parent| parent.rsplit("::").next())
    else {
        return false;
    };
    external_supertype_of(&ext.file_path, owner, supertypes, external_bases).is_some()
}

/// Fill in the site evidence of every unconfirmed row in one pass.
///
/// `sites` counts the ambiguous edges into the symbol and the unattributed
/// ledger rows naming it — the two kinds of evidence that put a row below the
/// confident tier. The first site is the lexically smallest `(path, enclosing
/// symbol)`, located to a byte by the call or reference that produced it, so a
/// reader can jump straight to it and the choice is stable across builds.
fn attach_site_evidence(
    extractions: &[Extraction],
    resolution: &ResolutionResult,
    reports: &mut [DeadSymbolReport],
    pending: &[(usize, &str, &ExtractedSymbol)],
) {
    if pending.is_empty() {
        return;
    }
    let mut by_target: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for (slot, (_, file, sym)) in pending.iter().enumerate() {
        by_target
            .entry((file, sym.qualified_name.as_str()))
            .or_default()
            .push(slot);
        by_target
            .entry((file, sym.name.as_str()))
            .or_default()
            .push(slot);
        by_name.entry(sym.name.as_str()).or_default().push(slot);
    }

    // Per slot: how many sites, and the smallest `(path, enclosing, callee)`.
    let mut found: Vec<NamesakeSites> = vec![(0, None); pending.len()];

    for edge in resolution
        .edges
        .iter()
        .filter(|edge| is_ambiguous_edge(edge))
    {
        let short = edge
            .target_symbol
            .rsplit("::")
            .next()
            .unwrap_or(edge.target_symbol.as_str());
        let member = short.rsplit('.').next().unwrap_or(short);
        let mut slots: Vec<usize> = Vec::new();
        for form in [edge.target_symbol.as_str(), short, member] {
            if let Some(hit) = by_target.get(&(edge.target_file.as_str(), form)) {
                slots.extend(hit.iter().copied());
            }
        }
        slots.sort_unstable();
        slots.dedup();
        for slot in slots {
            let site = (
                edge.source_file.as_str(),
                edge.source_symbol.as_str(),
                member,
            );
            record_namesake_site(&mut found[slot], site);
        }
    }
    for row in resolution
        .unresolved
        .iter()
        .filter(|row| is_namesake_veto_row(row))
    {
        let Some(slots) = by_name.get(row.callee_name.as_str()) else {
            continue;
        };
        for &slot in slots {
            let site = (
                row.source_file.as_str(),
                row.source_symbol.as_str(),
                row.callee_name.as_str(),
            );
            record_namesake_site(&mut found[slot], site);
        }
    }

    let by_path: HashMap<&str, &Extraction> = extractions
        .iter()
        .map(|ext| (ext.file_path.as_str(), ext))
        .collect();
    for (slot, (index, _, _)) in pending.iter().enumerate() {
        let (sites, first) = found[slot];
        let Some(report) = reports.get_mut(*index) else {
            continue;
        };
        let first_byte = first.and_then(|(path, enclosing, callee)| {
            site_byte(by_path.get(path).copied()?, enclosing, callee)
        });
        report.evidence = Some(DeadEvidence {
            sites,
            first_path: first.map(|(path, _, _)| path.to_string()),
            first_byte,
            first_line: None,
        });
    }
}

/// The byte offset of the first site in `enclosing` that names `callee`: a
/// call, else a reference, else the enclosing symbol's own start.
fn site_byte(ext: &Extraction, enclosing: &str, callee: &str) -> Option<u32> {
    let call = ext
        .calls
        .iter()
        .filter(|call| {
            call.callee_name == callee && call.caller_symbol.as_deref() == Some(enclosing)
        })
        .map(|call| call.span.start_byte)
        .min();
    let reference = || {
        ext.references
            .iter()
            .filter(|reference| {
                reference.name == callee && reference.enclosing_symbol.as_deref() == Some(enclosing)
            })
            .map(|reference| reference.span.start_byte)
            .min()
    };
    let declaration = || {
        ext.symbols
            .iter()
            .filter(|sym| sym.qualified_name == enclosing)
            .map(|sym| sym.span.start_byte)
            .min()
    };
    call.or_else(reference)
        .or_else(declaration)
        .and_then(|byte| u32::try_from(byte).ok())
}

/// Reason carried by a finding the call-blind cap demoted.
///
/// Distinct from [`COVERAGE_LOSS_REASON`] on purpose. That one says extraction
/// "did not cover every file", which invites the reader to re-index. For a
/// language with no extractor there is nothing to re-run: the fact is about
/// this build's capabilities, it is permanent until someone writes the
/// extractor, and saying so is the difference between a transient gap and a
/// structural one.
pub const CALL_BLIND_REASON: &str =
    "no inbound call edges, but this file's language has no call extractor in this build — \
     the absence is the extractor's, not the code's";

/// Whether a grammar actually read this file.
///
/// The gate that keeps call-blindness from swallowing the tree. Prose and data
/// formats report `NotApplicable` and declare no capabilities, so without this
/// every `.md`, `.json` and `.yaml` would be charged as call-blind — 294 of
/// this repository's 1,310 files — and `is_complete()` would be false on every
/// corpus in existence. A degraded flag that is always on carries no
/// information, which is the same trap `ExtractionCoverage::parse_failed_files`
/// documents for its own count.
///
/// `RegexFallback` and `Unavailable` are excluded for a different reason: they
/// are already charged, as `PatternRecovered` and `ParseFailed` respectively.
/// Charging them again would double-count one file's single hole.
fn a_grammar_read_this_file(ext: &Extraction) -> bool {
    // Delegated rather than repeated. `devmap-query` asks the same question of
    // the same files and used to answer it differently — see
    // `Extraction::grammar_read_this_file` for the two numbers that disagreed.
    ext.grammar_read_this_file()
}

/// One file that call extraction did not cover, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractionGapEntry {
    pub path: String,
    pub gap: ExtractionGap,
    pub reason: String,
}

/// Name the files whose calls were never extracted.
///
/// The owner of the *set*; [`extraction_coverage`] is the fold over it, so a
/// count and a list of paths cannot disagree about which files they describe.
/// That mattered as soon as `devmap status` began naming them: a count derived
/// from one `matches!` chain and a list derived from another is precisely how
/// "2 file(s) failed to parse" came to sit beside a list of three.
pub fn extraction_gaps(extractions: &[Extraction]) -> Vec<ExtractionGapEntry> {
    let mut gaps = Vec::new();
    for ext in extractions {
        // `Extraction::is_parse_failure` is the canonical owner of the
        // `Failed`-vs-`NotApplicable` line — a `.md` is not a parse failure —
        // and asking it here is what keeps this list and the counts below in
        // step with it.
        let (gap, reason) = if ext.is_parse_failure() {
            (
                ExtractionGap::ParseFailed,
                match &ext.parse_outcome {
                    ParseOutcome::Failed { reason } => reason.clone(),
                    // Unreachable while `is_parse_failure` matches `Failed`,
                    // and stated rather than `unwrap`ped: a later variant that
                    // qualifies must still name itself in the inventory.
                    other => format!("{other:?}"),
                },
            )
        } else if let ParseOutcome::Fallback { reason } = &ext.parse_outcome {
            (ExtractionGap::PatternRecovered, reason.clone())
        } else if let ParseOutcome::Skipped { reason } = &ext.parse_outcome {
            // Before the arm existed this file matched nothing here and fell to
            // the `else { continue }` below — indistinguishable from a `.md`,
            // which is a file with no declarations to find rather than a file
            // whose declarations nobody looked for.
            (ExtractionGap::NotParsed, reason.clone())
        } else if a_grammar_read_this_file(ext) {
            if let ParseOutcome::Partial { .. } = &ext.parse_outcome {
                gaps.push(ExtractionGapEntry {
                    path: ext.file_path.clone(),
                    gap: ExtractionGap::PartialParse,
                    reason: partial_parse_reason(ext),
                });
            }
            // A clean parse in a language this build has no extractor for.
            // Both bits are asked independently: HCL is call-blind *and*
            // import-blind, Java only the second, and collapsing them would
            // make a file with one hole indistinguishable from a file with two.
            let capabilities = ext.capabilities();
            if !capabilities.contains(Capability::Calls) {
                gaps.push(ExtractionGapEntry {
                    path: ext.file_path.clone(),
                    gap: ExtractionGap::CallBlind,
                    reason: format!("`{}` has no call extractor in this build", ext.language),
                });
            }
            if !capabilities.contains(Capability::Imports) {
                gaps.push(ExtractionGapEntry {
                    path: ext.file_path.clone(),
                    gap: ExtractionGap::ImportBlind,
                    reason: format!("`{}` has no import extractor in this build", ext.language),
                });
            }
            continue;
        } else {
            continue;
        };
        gaps.push(ExtractionGapEntry {
            path: ext.file_path.clone(),
            gap,
            reason,
        });
    }
    gaps
}

/// Whether a partial parse can have erased a declaration, and with it the
/// calls inside it: some error region of nonzero width that holds a name lies
/// outside every declaration the extractor still found.
///
/// **A policy line, stated as one.** Every partial file is *listed* in the
/// `partial_parse` inventory; this decides which are *charged* against the
/// dead-code ceiling. The shapes it separates, measured:
///
/// * A `.tsx` holding `type R<T> = import('m').Ref<T>;` — the region runs from
///   the alias past where `AuthProvider` and `useAuth` should start. Outside
///   every surviving declaration: charged.
/// * Kotlin `suspend { … }` — the region swallows the rest of the file and no
///   declaration survives: charged.
/// * `&raw` in valid Rust — tree-sitter-rust 0.23.3 reads a variable named
///   `raw` after `&` as raw-borrow syntax and marks four bytes inside a
///   function body that was otherwise parsed. Inside a surviving declaration,
///   which the dead pass already exempts when a range overlaps it: listed, not
///   charged. Most of this repository's partial files were this shape, and
///   charging them took every Rust finding out of the confident tier.
/// * A zero-width `MISSING` token: nothing was consumed: not charged.
/// * A region holding no identifier character, which the extractor lists as
///   `nameless_ranges` — the `>()` tree-sitter-typescript strands after
///   `importOriginal<typeof import('m')>()`, or the lone `}` the CUDA
///   preprocessor scanner leaves behind a macro whose body line ends in a
///   comment. A declaration and a callee both need a name, so neither can be
///   inside: listed, not charged. Charging that `>()` took every TypeScript
///   finding of a 27-file fixture out of the confident tier.
///
/// The `File` symbol is not a declaration here — its span is the whole file,
/// which would contain every region.
fn partial_parse_erased_source(ext: &Extraction) -> bool {
    partial_region_verdicts(ext)
        .iter()
        .any(|(_, verdict)| *verdict == PartialRegionVerdict::Charged)
}

/// Why one error region of a partial parse is, or is not, charged.
///
/// The one owner of the per-region line [`partial_parse_erased_source`]
/// documents. The charge and the `partial_parse` reason both read it, so the
/// listing cannot tell a reader a region was lost from the graph while the
/// ceiling treated it as harmless, which is what one sentence for every
/// partial file used to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartialRegionVerdict<'a> {
    /// Holds a name and lies outside every surviving declaration.
    Charged,
    /// Lies inside this surviving declaration (its name relative to the file).
    InsideDeclaration(&'a str),
    /// Of nonzero width, but the extractor found no identifier character in it.
    Nameless,
    /// A zero-width `MISSING` token: nothing was consumed.
    ZeroWidth,
}

/// Every error region of `ext`'s partial parse with its verdict, in the order
/// the extractor reported them, duplicates removed. Empty for any other
/// outcome.
///
/// Inside a declaration, the innermost one is named: a method is more use to
/// a reader than the `impl` around it.
fn partial_region_verdicts(ext: &Extraction) -> Vec<(TextRange, PartialRegionVerdict<'_>)> {
    let ParseOutcome::Partial {
        error_ranges,
        nameless_ranges,
    } = &ext.parse_outcome
    else {
        return Vec::new();
    };
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    error_ranges
        .iter()
        .filter(|range| seen.insert((range.start_byte, range.end_byte)))
        .map(|range| {
            let verdict = if range.end_byte <= range.start_byte {
                PartialRegionVerdict::ZeroWidth
            } else if nameless_ranges.contains(range) {
                PartialRegionVerdict::Nameless
            } else {
                ext.symbols
                    .iter()
                    .filter(|symbol| {
                        symbol.kind != SymbolKind::File
                            && symbol.span.start_byte <= range.start_byte
                            && range.end_byte <= symbol.span.end_byte
                    })
                    .min_by_key(|symbol| symbol.span.end_byte - symbol.span.start_byte)
                    .map_or(PartialRegionVerdict::Charged, |symbol| {
                        PartialRegionVerdict::InsideDeclaration(
                            symbol
                                .qualified_name
                                .strip_prefix(ext.file_path.as_str())
                                .and_then(|rest| rest.strip_prefix("::"))
                                .unwrap_or(symbol.name.as_str()),
                        )
                    })
            };
            (range.clone(), verdict)
        })
        .collect()
}

/// Whether a partially parsed file is charged against the dead-code ceiling.
///
/// File-level gates first — a grammar read it, its language extracts calls,
/// it is not vendored — then the per-region verdict. One predicate, read by
/// the count in [`extraction_coverage`] and the sentence in
/// [`partial_parse_reason`].
fn partial_parse_charged(ext: &Extraction) -> bool {
    partial_parse_file_exemption(ext).is_none() && partial_parse_erased_source(ext)
}

/// Why a partially parsed file is not charged whatever its regions hold, or
/// `None` when its regions decide.
///
/// A vendored file is excluded by the rule over-limit files are charged by: a
/// vendored grammar's `parser.c` cannot call this repository's code. A file in
/// a language with no call extractor is already charged as `call_blind`, and
/// charging it again would double its weight in the share.
fn partial_parse_file_exemption(ext: &Extraction) -> Option<String> {
    if !a_grammar_read_this_file(ext) {
        return Some("no grammar read this file".to_string());
    }
    if !ext.capabilities().contains(Capability::Calls) {
        return Some(format!(
            "`{}` has no call extractor in this build, so the file is charged as call_blind instead",
            ext.language
        ));
    }
    if devmap_extract::wiring::is_vendored_path(&ext.file_path) {
        return Some(
            "the file is vendored, and a vendored file cannot call this repository's code"
                .to_string(),
        );
    }
    None
}

/// Most merged error regions a `partial_parse` reason spells out.
const PARTIAL_PARSE_RANGE_SAMPLE: usize = 8;

/// `ranges` as `a..b` items, at most [`PARTIAL_PARSE_RANGE_SAMPLE`] of them and
/// how many more when cut — both numbers, never a sample read as the whole.
fn partial_parse_range_list(ranges: &[String]) -> String {
    let shown = ranges.len().min(PARTIAL_PARSE_RANGE_SAMPLE);
    let more = ranges.len() - shown;
    format!(
        "{}{}",
        ranges[..shown].join(", "),
        if more > 0 {
            format!(", and {more} more")
        } else {
            String::new()
        }
    )
}

/// What a partial parse lost, and which of it is charged, stated.
///
/// Every region is named with its verdict from [`partial_region_verdicts`]:
/// the charged ones, then each exemption with its reason. An exempt region is
/// listed, never dropped, so the listing still says where the tree was
/// damaged even where the damage is judged harmless.
fn partial_parse_reason(ext: &Extraction) -> String {
    let error_ranges: &[TextRange] = match &ext.parse_outcome {
        ParseOutcome::Partial { error_ranges, .. } => error_ranges,
        _ => &[],
    };
    let lost = partial_parse_lost(error_ranges);
    let verdicts = partial_region_verdicts(ext);
    let named = |pick: &dyn Fn(&PartialRegionVerdict<'_>) -> Option<String>| -> Vec<String> {
        verdicts
            .iter()
            .filter_map(|(range, verdict)| {
                pick(verdict)
                    .map(|suffix| format!("{}..{}{suffix}", range.start_byte, range.end_byte))
            })
            .collect()
    };
    let charged = named(&|verdict| (*verdict == PartialRegionVerdict::Charged).then(String::new));
    let inside = named(&|verdict| match verdict {
        PartialRegionVerdict::InsideDeclaration(name) => Some(format!(" in `{name}`")),
        _ => None,
    });
    let nameless = named(&|verdict| (*verdict == PartialRegionVerdict::Nameless).then(String::new));
    let zero_width =
        named(&|verdict| (*verdict == PartialRegionVerdict::ZeroWidth).then(String::new));

    let mut sentence = lost;
    if !charged.is_empty() {
        let file_exemption = partial_parse_file_exemption(ext);
        sentence.push_str(&format!(
            "; {}: {} region(s) outside every surviving declaration ({}){}",
            if file_exemption.is_some() {
                "would be charged"
            } else {
                "charged"
            },
            charged.len(),
            partial_parse_range_list(&charged),
            match file_exemption {
                Some(why) => format!(", but the file is not charged: {why}"),
                None => " — declarations and calls inside them may be absent from the graph"
                    .to_string(),
            }
        ));
    }
    let mut exempt: Vec<String> = Vec::new();
    if !inside.is_empty() {
        exempt.push(format!(
            "{} region(s) inside a surviving declaration ({})",
            inside.len(),
            partial_parse_range_list(&inside)
        ));
    }
    if !nameless.is_empty() {
        exempt.push(format!(
            "{} region(s) that hold no name ({})",
            nameless.len(),
            partial_parse_range_list(&nameless)
        ));
    }
    if !zero_width.is_empty() {
        exempt.push(format!(
            "{} zero-width MISSING token(s) ({})",
            zero_width.len(),
            partial_parse_range_list(&zero_width)
        ));
    }
    if !exempt.is_empty() {
        sentence.push_str(&format!("; not charged: {}", exempt.join(", ")));
    }
    sentence
}

/// The byte ranges a partial parse lost, merged and stated.
///
/// Merged first because tree-sitter reports nested and overlapping regions —
/// an `ERROR` spanning the file and a `MISSING` token inside it — and summing
/// those would claim more bytes lost than the file holds. Both numbers are
/// given when the list is cut: a sample presented as the whole is how a hole
/// comes to look smaller than it is.
fn partial_parse_lost(error_ranges: &[TextRange]) -> String {
    let mut ranges: Vec<(usize, usize)> = error_ranges
        .iter()
        .map(|range| (range.start_byte, range.end_byte.max(range.start_byte)))
        .collect();
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        match merged.last_mut() {
            Some((_, last_end)) if start <= *last_end => *last_end = (*last_end).max(end),
            _ => merged.push((start, end)),
        }
    }
    let lost: usize = merged.iter().map(|(start, end)| end - start).sum();
    let shown: Vec<String> = merged
        .iter()
        .take(PARTIAL_PARSE_RANGE_SAMPLE)
        .map(|(start, end)| format!("{start}..{end}"))
        .collect();
    let more = merged.len().saturating_sub(shown.len());
    format!(
        "the tree holds ERROR/MISSING nodes over {} region(s), {lost} byte(s) lost: {}{}",
        merged.len(),
        shown.join(", "),
        if more > 0 {
            format!(", and {more} more region(s)")
        } else {
            String::new()
        }
    )
}

/// Count the files whose calls were never extracted.
///
/// One owner for the question, shared with `code_graph.rs`, which needs the
/// same two numbers for `meta.devmap_rust`. Two independent `matches!` chains
/// over `parse_outcome` is exactly how the `Failed`-vs-`NotApplicable`
/// distinction gets lost in one of them — so this counts
/// [`extraction_gaps`]'s entries rather than re-deciding them.
pub fn extraction_coverage(extractions: &[Extraction]) -> ExtractionCoverage {
    let mut coverage = ExtractionCoverage::default();
    let path_language: HashMap<&str, &str> = extractions
        .iter()
        .map(|ext| (ext.file_path.as_str(), ext.code_language()))
        .collect();
    // Partially parsed files whose language extracts calls: charged as blind,
    // and so not counted as covered below — one side of the partition each.
    // Which ones is `partial_parse_charged`'s call, the same predicate the
    // `partial_parse` reason states its verdict from.
    let partial_with_calls: HashSet<&str> = extractions
        .iter()
        .filter(|ext| partial_parse_charged(ext))
        .map(|ext| ext.file_path.as_str())
        .collect();
    for entry in extraction_gaps(extractions) {
        match entry.gap {
            ExtractionGap::ParseFailed => coverage.parse_failed_files += 1,
            ExtractionGap::PatternRecovered => coverage.pattern_recovered_files += 1,
            ExtractionGap::CallBlind => coverage.call_blind_files += 1,
            ExtractionGap::ImportBlind => coverage.import_blind_files += 1,
            ExtractionGap::NotParsed => coverage.not_parsed_files += 1,
            ExtractionGap::PartialParse => {
                if partial_with_calls.contains(entry.path.as_str()) {
                    coverage.partial_parse_files += 1;
                }
            }
        }
        // ImportBlind and NotParsed stay out of the capping numerator — same
        // policy as `blind_files` before language scoping. The rest feed the
        // per-language map that `cap_for` reads.
        // A partial parse is charged where its language extracts calls: a
        // call-blind language is already charged as `CallBlind`, and charging
        // the same file twice would double its weight in the share.
        let charged_partial = entry.gap == ExtractionGap::PartialParse
            && partial_with_calls.contains(entry.path.as_str());
        if charged_partial
            || matches!(
                entry.gap,
                ExtractionGap::ParseFailed
                    | ExtractionGap::PatternRecovered
                    | ExtractionGap::CallBlind
            )
        {
            let lang = path_language
                .get(entry.path.as_str())
                .copied()
                .unwrap_or("");
            *coverage
                .blind_by_language
                .entry(lang.to_string())
                .or_default() += 1;
        }
    }
    // The other side of the same partition, counted from the same two
    // predicates `extraction_gaps` charges `CallBlind` from. Derived here
    // rather than as `extractions.len() - gaps` because that subtraction would
    // fold every `.md` and `.json` into the covered side and flatter the share
    // — the denominator is files whose calls were *looked for*, not files seen.
    for ext in extractions {
        if a_grammar_read_this_file(ext)
            && ext.capabilities().contains(Capability::Calls)
            && !partial_with_calls.contains(ext.file_path.as_str())
        {
            coverage.files_with_call_extraction += 1;
            *coverage
                .covered_by_language
                .entry(ext.code_language().to_string())
                .or_default() += 1;
        }
    }
    coverage
}

/// Dead-symbol findings together with how much of the corpus produced them.
pub struct LivenessOutcome {
    pub reports: Vec<DeadSymbolReport>,
    pub coverage: ExtractionCoverage,
}

/// The two symbol sets the whole liveness question turns on: what an edge
/// names, and what an edge *might* name.
///
/// Hoisted out of `analyze_liveness_with_coverage`'s body so the exemption
/// index below — which both passes read — can be built from the same walk
/// instead of a second one that would eventually disagree about which
/// resolutions count as reaching.
type CallIndex = (HashSet<(String, String)>, HashSet<(String, String)>);

/// Record `symbol` of `file` as something an ambiguous site might name, under
/// the three spellings a declaration is looked up by: its identity, its short
/// name, and the member name with its owner stripped.
///
/// `ExtractedSymbol::name` is the bare `toJson`, while a site names
/// `File::RemoteTaskEntity.toJson`, so without the stripped form the two never
/// meet and the ambiguity is recorded against nothing. This became load-bearing
/// when Kotlin extension functions gained their receiver: one Android file
/// declares seven `private fun <T>.toJson()` on seven different types and calls
/// every one of them as `it.toJson()` inside a `map { }`. The receiver `it`
/// cannot be typed, so the site is ambiguous — and the other six candidates,
/// each genuinely called, were reported dead at 0.9 confidence.
///
/// Bounded to the ambiguous set on purpose: this can only move a finding from
/// 0.9 to 0.4 `only_ambiguous_callers`, never exempt it, so it cannot hide a
/// symbol nothing calls. Doing the same for `called_symbols` would silently
/// exempt every same-named method in the file, which is a different and much
/// worse trade.
fn record_ambiguous(set: &mut HashSet<(String, String)>, file: &str, symbol: &str) {
    set.insert((file.to_string(), symbol.to_string()));
    if let Some(short_name) = symbol.rsplit("::").next() {
        set.insert((file.to_string(), short_name.to_string()));
        if let Some(member) = short_name.rsplit('.').next() {
            set.insert((file.to_string(), member.to_string()));
        }
    }
}

/// The ambiguous *sites*: every call that named several candidates and bound
/// none of them (S2). A bare-name site with in-repo namesakes is carried by its
/// ledger row — `Resolution::AmbiguousGlobal` with the complete candidate list
/// — and never by edges, so liveness reads the ambiguity from the ledger.
fn ambiguous_ledger_rows(
    resolution: &ResolutionResult,
) -> impl Iterator<Item = (&UnresolvedReference, &[(String, String)])> {
    resolution.unresolved.iter().filter_map(|row| {
        if row.kind != UnresolvedKind::Call {
            return None;
        }
        match &row.resolution {
            Resolution::AmbiguousGlobal { candidates, .. } => Some((row, &candidates[..])),
            _ => None,
        }
    })
}

fn called_and_ambiguous_symbols(resolution: &ResolutionResult) -> CallIndex {
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
        if is_ambiguous_edge(edge) {
            record_ambiguous(
                &mut ambiguous_symbols,
                &edge.target_file,
                &edge.target_symbol,
            );
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

    for (_, candidates) in ambiguous_ledger_rows(resolution) {
        for (file, symbol) in candidates {
            record_ambiguous(&mut ambiguous_symbols, file, symbol);
        }
    }

    for u in &resolution.unresolved {
        if u.kind != UnresolvedKind::Call {
            continue;
        }
        if let Resolution::Unresolved { ref reason } = u.resolution {
            if reason == "implicit receiver method is overridden by an indexed subtype" {
                if let Some(short_caller) = u.source_symbol.rsplit("::").next() {
                    if let Some((type_name, _)) = short_caller.rsplit_once('.') {
                        called_symbols.insert((
                            u.source_file.clone(),
                            format!("{type_name}.{}", u.callee_name),
                        ));
                        called_symbols.insert((u.source_file.clone(), u.callee_name.clone()));
                    }
                }
            }
        }
    }

    (called_symbols, ambiguous_symbols)
}

/// Why the single-symbol pass exempts a symbol, keyed by
/// `(file path, qualified name)`.
///
/// **Hoisted, because two passes were answering this question and only one of
/// them knew the answers.** `dead_clusters::externally_reachable_symbols` seeded
/// its live set from `symbol.is_exported` and wiring annotations alone, so every
/// exemption computed here was invisible to it. The consequence is not a tier
/// disagreement, it is a proposal to delete public API: `__all__ +=
/// ["MyClass"]` with `MyClass.a()` and `MyClass.b()` mutually recursive exempts
/// both symbols in this pass and reports the pair as a cluster "reached by
/// nothing outside the component" in that one. Two mutually recursive C
/// functions declared in a shared header are the same shape.
///
/// One owner, read twice, rather than one computation copied. The cost is a
/// second walk of the corpus in [`exempt_symbol_names`]; the alternative —
/// threading this through `dead_clusters`' public signature — would have moved
/// the drift from the data to the call sites.
///
/// The `.or_else` precedence of the original chain is preserved exactly, because
/// the reason string is a machine token in three tests: symbol wiring, heritage
/// override, Go interface, C header, exported owner, Go build variant.
/// Why a Kotlin or Java class is live: an Android manifest declares it as a
/// component. Matched on the class's package-qualified name, inside the module
/// directory the manifest governs; a name written `.Name`, or bare as `Name`,
/// is relative to the module's namespace, which only the build script knows,
/// so it matches on its suffix.
fn android_component_reason(
    ext: &Extraction,
    sym: &ExtractedSymbol,
    components: &[(&str, &str, &str)],
) -> Option<String> {
    if components.is_empty()
        || !matches!(ext.language.as_str(), "kotlin" | "java")
        || sym.kind != SymbolKind::Class
        || sym
            .parent_symbol
            .as_deref()
            .is_some_and(|parent| parent != ext.file_path)
    {
        return None;
    }
    let package = ext.jvm_package.as_deref()?;
    let qualified = format!("{package}.{}", sym.name);
    components.iter().find_map(|(module, name, details)| {
        let in_module = module.is_empty()
            || ext
                .file_path
                .strip_prefix(module)
                .is_some_and(|rest| rest.starts_with('/'));
        // `.Sync` and a bare `Sync` are both relative to the namespace; only a
        // dotted name is written out in full.
        let names_it = if name.starts_with('.') {
            qualified.ends_with(name)
        } else if !name.contains('.') {
            qualified
                .strip_suffix(name)
                .is_some_and(|head| head.ends_with('.'))
        } else {
            qualified == *name
        };
        (in_module && names_it).then(|| (*details).to_string())
    })
}

fn symbol_exemption_index(
    extractions: &[Extraction],
    resolution: &ResolutionResult,
    called: &HashSet<(String, String)>,
    ambiguous: &HashSet<(String, String)>,
) -> HashMap<(String, String), String> {
    let supertypes = supertypes_by_type(resolution);
    // Dispatch candidates are not proven reached. The direct edge to the
    // interface method is a call, so the heritage exemption would spare every
    // conformer and the `reached_by_dispatch` verdict would never run.
    let dispatch_only = dispatch_only_symbols(resolution);
    let go_interface_specs = go_interface_specs_by_package(extractions);
    let c_header_exports = c_header_exported_names(extractions);
    let go_build_variants = go_build_variant_identities(extractions);
    let go_value_mentions = go_value_mentioned_names(extractions);
    let go_exported_specs = go_exported_interface_specs(extractions);

    // The one annotation kind whose target lives in a *different* file from the
    // one that carries it: `[project.scripts] cli = "pkg.mod:func"` is written
    // in `pyproject.toml` and names a symbol in `pkg/mod.py`. Every other
    // symbol-scoped kind is emitted by the extractor for the file it is
    // examining, which is why the per-extraction map below cannot see this one.
    //
    // Keyed on the whole target string, which the extractor builds as
    // `<file path>::<attr>` — the same shape as a symbol's `qualified_name`.
    // The file path is inside the key, so a corpus-wide map cannot exempt a
    // namesake in some other file; a bare name could, which is exactly why the
    // per-extraction map stays per-extraction.
    let config_entry_points: HashMap<&str, &str> = extractions
        .iter()
        .flat_map(|ext| ext.wiring.iter())
        .filter(|w| w.kind == WiringKind::ConfigEntryPoint)
        .map(|w| (w.target_symbol.as_str(), w.details.as_str()))
        .collect();

    // The Android half of the same declaration: a manifest names a class by
    // package-qualified name, so the join is made here, where each class's own
    // `package` header is known. `(module dir, name as written, why)`.
    let android_components: Vec<(&str, &str, &str)> = config_entry_points
        .iter()
        .filter_map(|(target, details)| {
            let (module, name) = target
                .strip_prefix(devmap_extract::wiring::ANDROID_COMPONENT_TARGET_PREFIX)?
                .split_once('|')?;
            Some((module, name, *details))
        })
        .collect();

    let mut index: HashMap<(String, String), String> = HashMap::new();
    for ext in extractions {
        // A wiring annotation is file-scoped only when it targets the file
        // itself. Symbol-scoped annotations must never be read as file-scoped:
        // one `#[test] fn` would otherwise exempt every symbol in the file.
        let wired: HashMap<&str, &str> = ext
            .wiring
            .iter()
            .filter(|w| w.target_symbol != ext.file_path)
            .filter(|w| {
                matches!(
                    w.kind,
                    WiringKind::RuntimeEntryPoint | WiringKind::StructuralExempt
                )
            })
            .map(|w| (w.target_symbol.as_str(), w.details.as_str()))
            .collect();

        // Read by the corpus-wide exported-interface arm below; empty for
        // every other language, which never reaches it.
        let go_param_counts = if ext.language == "go" {
            ext.go_method_param_counts()
        } else {
            BTreeMap::new()
        };

        // Cross-file half of the Go interface exemption. Extraction closes the
        // same-file case as a wiring annotation; the package-wide join can only
        // happen here, where every file is in scope.
        let go_interface_exemptions: HashMap<&str, String> = go_package_key(ext)
            .and_then(|key| go_interface_specs.get(&key))
            .map(|specs| {
                go_interface_method_matches(&ext.symbols, &go_param_counts, specs.iter().copied())
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

        // Types this file publishes, keyed by the exact `qualified_name` a
        // member's `parent_symbol` points at.
        //
        // Measured on `testdata/fixtures/tier_a/python_app`: `__all__ +=
        // ["MyClass"]` exempted `MyClass` and not `MyClass.execute`, so the
        // kernel called a public method of a declared-public class dead at the
        // `extracted` tier — the tier whose contract is "safe to act on".
        // Acting on that deletes public API.
        //
        // Empty for every language that encodes member visibility itself; see
        // `member_visibility_is_conventional`.
        let exported_owners: HashSet<&str> = if member_visibility_is_conventional(&ext.language) {
            ext.symbols
                .iter()
                .filter(|sym| sym.is_exported && sym.kind != SymbolKind::File)
                .map(|sym| sym.qualified_name.as_str())
                .collect()
        } else {
            HashSet::new()
        };

        for sym in &ext.symbols {
            if is_never_dead_candidate(sym.kind) || sym.name.starts_with('_') {
                continue;
            }
            let identity = dead_symbol_identity(sym, &ext.file_path);
            let is_ambiguously_called = ambiguous
                .contains(&(ext.file_path.clone(), sym.name.clone()))
                || ambiguous.contains(&(ext.file_path.clone(), sym.qualified_name.clone()));

            let reached_only_by_dispatch = dispatch_only
                .contains(&(ext.file_path.clone(), sym.qualified_name.clone()))
                || dispatch_only.contains(&(ext.file_path.clone(), sym.name.clone()));
            let heritage = (!reached_only_by_dispatch)
                .then(|| {
                    reached_through_a_supertype(&ext.file_path, &identity, &supertypes, called)
                })
                .flatten();

            let reason: Option<String> = wired
                .get(sym.qualified_name.as_str())
                .map(|details| (*details).to_string())
                // An explicit declaration in a manifest, ahead of every
                // inference below it: `pip` writes the launcher that calls this
                // function, and the launcher is generated at install time and
                // is not in any corpus.
                .or_else(|| {
                    config_entry_points
                        .get(sym.qualified_name.as_str())
                        .map(|details| (*details).to_string())
                })
                .or_else(|| android_component_reason(ext, sym, &android_components))
                .or(heritage)
                .or_else(|| {
                    go_interface_exemptions
                        .get(sym.qualified_name.as_str())
                        .cloned()
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
                    .then(|| "Declared in a C-family header — public interface".to_string())
                })
                // A member of an exported type, in a language where the member
                // could not have said so itself.
                //
                // Keyed on `parent_symbol`, which is the owner's exact
                // `qualified_name`. An earlier attempt split the member's own
                // qualified name on `.` and matched `A.java::A.used` as owner
                // `A` — the dot it found belonged to the file extension, and it
                // exempted every method in every Java class.
                .or_else(|| {
                    sym.parent_symbol
                        .as_deref()
                        .filter(|parent| exported_owners.contains(parent))
                        .map(|_| "Member of an exported type — public interface".to_string())
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
                                go_build_variants.contains(&(dir, package, identity.clone()))
                            })
                            .unwrap_or(false))
                    .then(|| GO_BUILD_VARIANT_REASON.to_string())
                })
                // A Go function used as a value rather than called. Appended
                // last, so no reason string above it changes: the arms are
                // machine tokens in the tests that pin them.
                //
                // `SymbolKind::Function` only. A method is reported under
                // `Type.Method`, so matching a bare mention against it would mean
                // deliberately stripping the type — and that over-exempts every
                // same-named method of every type in the package. The
                // `identity == name` check says the same thing structurally for
                // any other nested shape.
                .or_else(|| {
                    (sym.kind == SymbolKind::Function
                        && identity == sym.name
                        && go_package_key(ext)
                            .and_then(|(dir, package)| {
                                go_value_mentions.get(&(dir, package, sym.name.as_str()))
                            })
                            .is_some_and(|files| {
                                // A mention in the declaring file is not
                                // declined — the same-file rung answers it and
                                // the symbol is live by an edge. Only a mention
                                // from elsewhere in the package is evidence this
                                // index cannot attribute.
                                files.iter().any(|file| *file != ext.file_path.as_str())
                            }))
                    .then(|| GO_VALUE_MENTION_REASON.to_string())
                })
                // An exported Go method whose shape some interface declares.
                //
                // Exported methods never needed this while every exported Go
                // name was exempt for being exported. Widening a module-private
                // package into the unconfirmed tier makes them candidates, and
                // an exported method is satisfiable from *any* package — so the
                // join is corpus-wide, unlike the package-scoped one above,
                // which exists for unexported names only. The standard-library
                // interfaces are listed by name and arity because `fmt` calls
                // `String()` and the corpus never does.
                // Only where the package is module-private: in any other
                // package an exported method is spared for being exported, and
                // filing it under an interface would shrink the `excluded`
                // count of symbols never examined for their visibility.
                .or_else(|| {
                    if ext.language != "go"
                        || sym.kind != SymbolKind::Method
                        || !sym.is_exported
                        || go_module_private_proof(ext).is_none()
                    {
                        return None;
                    }
                    let params = *go_param_counts.get(sym.qualified_name.as_str())?;
                    go_exported_specs
                        .get(&(sym.name.as_str(), params))
                        .map(|interface_name| go_interface_exemption_reason(interface_name))
                        .or_else(|| {
                            GO_STD_INTERFACE_METHODS
                                .contains(&(sym.name.as_str(), params))
                                .then(|| GO_STD_INTERFACE_REASON.to_string())
                        })
                });

            if let Some(reason) = reason {
                index.insert((ext.file_path.clone(), sym.qualified_name.clone()), reason);
            }
        }
    }
    index
}

/// The qualified names the single-symbol pass would never call dead.
///
/// The seed `dead_clusters` was missing. Public because the cluster pass lives
/// in another module and must reach the same verdict from the same evidence —
/// that is the entire content of this fix.
///
/// Go build variants are in here too, and harmlessly: the cluster pass already
/// treats an ambiguously-named symbol as qualifying evidence, so an identity
/// exempted for being one platform's spelling of another was never going to be
/// reported confidently anyway.
pub fn exempt_symbol_names(
    extractions: &[Extraction],
    resolution: &ResolutionResult,
) -> std::collections::BTreeSet<String> {
    let (called, ambiguous) = called_and_ambiguous_symbols(resolution);
    symbol_exemption_index(extractions, resolution, &called, &ambiguous)
        .into_keys()
        .map(|(_, qualified_name)| qualified_name)
        .collect()
}

/// Dead-symbol findings only.
///
/// Thin delegate over [`analyze_liveness_with_coverage`], kept because callers
/// that only want the findings should not have to name the coverage record.
/// The confidence cap is applied by the canonical implementation, so both entry
/// points report the same tiers.
pub fn analyze_liveness(
    extractions: &[Extraction],
    resolution: &ResolutionResult,
) -> Vec<DeadSymbolReport> {
    analyze_liveness_with_coverage(extractions, resolution, DiscoveryCoverage::none()).reports
}

pub fn analyze_liveness_with_coverage(
    extractions: &[Extraction],
    resolution: &ResolutionResult,
    discovery: DiscoveryCoverage,
) -> LivenessOutcome {
    let mut coverage = extraction_coverage(extractions);
    // Folded in before the cap is applied, not after the reports are built: a
    // file discovery never read may hold the only call to a symbol here, so a
    // refusal has to reach `coverage.cap()` the same way a parse failure does.
    coverage.discovery_refused_files = discovery.charged();
    // An over-limit file is charged by language, like a parse failure: it has
    // a path, so it has a language, and a 2 MB generated Go client must not
    // demote a Python finding.
    for (_, language) in discovery.charged_over_limit() {
        coverage.over_limit_files += 1;
        *coverage
            .blind_by_language
            .entry(language.to_string())
            .or_default() += 1;
    }
    // Computed once for the whole corpus: the join is name-only, so it has no
    // per-file component to recompute.
    let unresolved_names = unresolved_namesake_names(resolution);

    let (called_symbols, ambiguous_symbols) = called_and_ambiguous_symbols(resolution);
    // The exemptions, computed ahead of the cascade rather than inside it, so
    // `dead_clusters` can read the same set. See `symbol_exemption_index`: the
    // cluster pass seeded its live set from `is_exported` and wiring alone and
    // therefore proposed deleting exported Python members, C functions declared
    // in a shared header, Go interface implementations and heritage overrides —
    // every one of which this pass had already exempted, one function later.
    let exemptions =
        symbol_exemption_index(extractions, resolution, &called_symbols, &ambiguous_symbols);

    let dispatch_only = dispatch_only_symbols(resolution);
    let mut reports = Vec::new();
    // Unconfirmed rows awaiting their site evidence, by report index. Filled
    // in one pass over the edges and the ledger after the loop, so the cost is
    // proportional to the rows that need it rather than to the corpus.
    let mut pending: Vec<(usize, &str, &ExtractedSymbol)> = Vec::new();
    let scopes = manifest_scopes(extractions);
    let supertypes = supertypes_by_type(resolution);
    let external_bases = external_supertypes(extractions, &supertypes);

    for ext in extractions {
        // X6: Parse-failed files must NEVER be reported as confirmed dead code
        // candidates — and neither must pattern-recovered ones.
        //
        // A `Fallback` file had its declarations recovered by line pattern
        // because no grammar exists for its language, and that tier extracts no
        // calls at all. So *every* symbol in such a file is uncalled by
        // construction, and reporting them would hand `devmap dead` one false
        // candidate per declaration in every `.proto`, `.ps1` and `.vb` in the
        // tree. "Nothing calls it" is only evidence when calls were looked for.
        //
        // `Skipped` joins them under the same sentence. Today its only symbol
        // is the `File` node, which the loop below exempts anyway, so this
        // changes no verdict — it is here because the rule is "nothing calls it
        // is only evidence when calls were looked for", and a file nobody
        // parsed is the clearest case of calls not being looked for. Leaving it
        // out would make the guard depend on the `File`-node exemption holding
        // somewhere else.
        let is_parse_failed = matches!(
            ext.parse_outcome,
            ParseOutcome::Failed { .. }
                | ParseOutcome::Fallback { .. }
                | ParseOutcome::Skipped { .. }
        );

        // The same sentence as `is_parse_failed`, one step further out: a file
        // whose grammar succeeded but whose language has no call extractor also
        // extracted no calls, so every symbol in it is uncalled by
        // construction. `is_parse_failed` could not see this because the parse
        // did not fail — that is exactly how CFML and Terraform symbols reached
        // the `extracted` tier.
        // `Extraction::is_call_blind` is the one predicate `impact` and
        // `trace` refuse an empty answer with, so the two surfaces cannot
        // disagree about which files were never looked at.
        let file_is_call_blind = ext.is_call_blind();

        // Every finding about a symbol in this file, priced against the
        // blindness that actually bears on it.
        //
        // `file_is_call_blind` used to select only the *reason string*; the
        // confidence still came from `coverage.cap`, which is a corpus-wide
        // ratio. So in a 99-Python / 1-Terraform corpus a `.tf` symbol published
        // at the top of `inferred` carrying "this language has no call extractor
        // in this build" — a permanent property of the build, priced as a one
        // percent transient. The reason and the number now agree.
        let cap = |confidence: f32| {
            if file_is_call_blind {
                coverage.cap_call_blind_file_for(ext.code_language(), confidence)
            } else {
                coverage.cap_for(ext.code_language(), confidence)
            }
        };

        // A wiring annotation is file-scoped only when it targets the file
        // itself. Symbol-scoped annotations must never be read as file-scoped:
        // one `#[test] fn` would otherwise exempt every symbol in the file,
        // which is the same over-exemption the file-level decorator rule
        // already suffers from.
        // The symbol-scoped half now lives in `symbol_exemption_index`, which
        // applies the same rule: an annotation is file-scoped only when it
        // targets the file itself, or one `#[test] fn` would exempt every
        // symbol in the file.
        let file_wiring: Vec<_> = ext
            .wiring
            .iter()
            .filter(|w| w.target_symbol == ext.file_path)
            .collect();

        // Deliberately **narrower** than `Extraction::file_liveness()`'s
        // exempt set, and the difference is the same one `TargetRoot` was
        // introduced for. A file-level exemption says nothing reaches the
        // *file*; this list exempts every symbol *in* it. `TargetRoot`,
        // `ToolConfig`, `PackageMarker`, `AmbientDeclaration` and
        // `DirectoryUnit` are all claims about the file, and an unused helper
        // somebody tucked inside a `src/bin/tool.rs` or a `noxfile.py` is
        // exactly as dead as one anywhere else —
        // `test_runtime_entry_points_are_exempt_without_exempting_their_file`
        // exists to refuse that widening.
        //
        // What the canonical predicate *is* consulted for is `NotCode`. A
        // Markdown or YAML file declares nothing, so today the branch is
        // vacuous; it is here because the rule is "a file outside the liveness
        // population is outside every liveness verdict", and leaving one
        // surface to rediscover that the day a YAML grammar is linked is how
        // the three surfaces disagreed in the first place.
        let exempting_wiring = file_wiring.iter().copied().find(|w| {
            // A Go `main.go` or a Rust `main.rs` is a binary's entry *file*,
            // and the language's entry *function* is already exempt by
            // name at symbol level (`go_runtime_entry_reason`, the Rust
            // `main` attribute rule). Exempting the whole file on top of
            // that spared every other function in it: an uncalled helper
            // in `main.go` was never reported while the same helper in the
            // package's next file was. The rule this list states — a
            // file-level claim is not a claim about the symbols in it —
            // applies, so for these two the annotation stays a claim about
            // the file.
            (w.kind == WiringKind::ScriptEntry && !matches!(ext.language.as_str(), "go" | "rust"))
                || matches!(
                    w.kind,
                    WiringKind::Vendored
                        | WiringKind::TestFile
                        // Fixture data, golden output and examples are test
                        // material under another name, and `is_test_path` does
                        // not know those directories — it is pinned equal to
                        // the Python rule. Same verdict as `TestFile`, for both
                        // the file and its symbols.
                        | WiringKind::Fixture
                        | WiringKind::GeneratedFile
                        | WiringKind::StructuralExempt
                        | WiringKind::FrameworkDecorator
                        | WiringKind::Launcher
                        | WiringKind::ReExportPackage
                        // An explicit author declaration. The Python side has
                        // honoured this since it was introduced and the kernel
                        // did not, so a file whose author had already answered
                        // the question was reported as dead on every build.
                        | WiringKind::AllowUnwired
                )
        });
        let is_file_exempt = is_parse_failed
            || matches!(ext.file_liveness(), FileLiveness::NotCode { .. })
            || exempting_wiring.is_some();

        // Per-symbol exemptions: a runtime, framework, or harness reaches the
        // symbol without an explicit call site, or the language forbids the
        // symbol from ever being marked public. Keyed by `qualified_name`,
        // which is what the extractor writes into `target_symbol`.
        let file_reason = if matches!(ext.parse_outcome, ParseOutcome::Fallback { .. }) {
            Some(
                "Declarations recovered by pattern, no call extraction — \
                 excluded from dead code candidates"
                    .to_string(),
            )
        } else if let ParseOutcome::Skipped { reason } = &ext.parse_outcome {
            // Ahead of the `is_parse_failed` arm, which now covers this outcome
            // and would label it "Parse failed" — the exact sentence this
            // outcome exists to stop the map from printing about a file that
            // was never handed to a grammar.
            Some(format!("{reason} — excluded from dead code candidates"))
        } else if is_parse_failed {
            Some("Parse failed — excluded from dead code candidates".to_string())
        } else {
            // From the annotation that exempted the file, never merely the
            // first one it carries: a Go `main.go`'s `ScriptEntry` no longer
            // exempts its symbols, and naming it as their reason would say it
            // did.
            exempting_wiring.map(|w| w.details.clone())
        };

        // One report per graph identity. Two `#[cfg]` variants of one method —
        // `ShutdownSignals::recv` for unix and for windows — are two extracted
        // symbols sharing one qualified name, which is one node with one set
        // of callers. Reporting each listed the same finding twice, and a
        // reader counting rows counted it twice. Every input to the verdict is
        // keyed by that name except the parse-error overlap, which is a fact
        // about one variant's span; it is taken over all of them, so the row
        // is exempt if any variant could hide a caller.
        let mut overlaps_by_identity: HashMap<&str, bool> = HashMap::new();
        for sym in &ext.symbols {
            let overlaps = match &ext.parse_outcome {
                ParseOutcome::Partial { error_ranges, .. } => error_ranges.iter().any(|range| {
                    sym.span.start_byte < range.end_byte && range.start_byte < sym.span.end_byte
                }),
                ParseOutcome::Clean
                | ParseOutcome::Failed { .. }
                | ParseOutcome::Fallback { .. }
                | ParseOutcome::Skipped { .. } => false,
            };
            *overlaps_by_identity
                .entry(sym.qualified_name.as_str())
                .or_default() |= overlaps;
        }
        let mut reported: HashSet<&str> = HashSet::new();

        for sym in &ext.symbols {
            // `starts_with("__")` was also tested here and is subsumed by the
            // single-underscore check — dead code that no mutant could kill.
            if is_never_dead_candidate(sym.kind) || sym.name.starts_with('_') {
                continue; // See `is_never_dead_candidate`; plus underscore-private
            }
            if !reported.insert(sym.qualified_name.as_str()) {
                continue;
            }

            let is_called = called_symbols.contains(&(ext.file_path.clone(), sym.name.clone()))
                || called_symbols.contains(&(ext.file_path.clone(), sym.qualified_name.clone()));
            let is_ambiguously_called = ambiguous_symbols
                .contains(&(ext.file_path.clone(), sym.name.clone()))
                || ambiguous_symbols.contains(&(ext.file_path.clone(), sym.qualified_name.clone()));

            // No grammar ran for `Failed`/`Fallback`/`Skipped`, so there are no
            // error ranges to overlap; the file is exempt wholesale via
            // `is_parse_failed` above.
            let overlaps_parse_error = overlaps_by_identity
                .get(sym.qualified_name.as_str())
                .copied()
                .unwrap_or(false);

            let is_exported = sym.is_exported;
            // One lookup where six `.or_else` arms used to sit. The arms moved
            // to `symbol_exemption_index` unchanged and in the same order —
            // symbol wiring, heritage override, Go interface, C header,
            // exported owner, Go build variant — because the reason string is a
            // machine token three tests match exactly.
            let symbol_exemption: Option<&str> = exemptions
                .get(&(ext.file_path.clone(), sym.qualified_name.clone()))
                .map(String::as_str);

            // Nothing else spares it: not its file, not a recorded exemption,
            // not a parse error over its span. Every report branch below
            // requires this; the exempt branch is what is left.
            let unspared = !is_file_exempt && symbol_exemption.is_none() && !overlaps_parse_error;
            let identity = dead_symbol_identity(sym, &ext.file_path);
            let language = ext.code_language();
            let dispatch_reason = (!is_called && !is_exported && unspared)
                .then(|| {
                    unconfirmed_dispatch_reason(ext, sym, &identity, &supertypes, &external_bases)
                })
                .flatten();

            if !is_called && is_ambiguously_called && !is_exported && unspared {
                pending.push((reports.len(), ext.file_path.as_str(), sym));
                reports.push(DeadSymbolReport {
                    symbol_name: identity,
                    file_path: ext.file_path.clone(),
                    confidence: cap(0.4),
                    is_exempt: false,
                    exemption_reason: Some(
                        if dispatch_only
                            .contains(&(ext.file_path.clone(), sym.qualified_name.clone()))
                            || dispatch_only.contains(&(ext.file_path.clone(), sym.name.clone()))
                        {
                            REACHED_BY_DISPATCH
                        } else {
                            "only_ambiguous_callers"
                        }
                        .to_string(),
                    ),
                    evidence: Some(DeadEvidence::none()),
                });
            } else if !is_called
                && !is_exported
                && unspared
                && unresolved_names.contains(sym.name.as_str())
            {
                // The defect ledger, read at last. Same tier as
                // `only_ambiguous_callers` — both mean "there is evidence
                // something reaches this and we could not prove which" — but a
                // distinct reason, because the two are different evidence and
                // `only_ambiguous_callers` is a machine token three tests match
                // exactly.
                //
                // Placed after the ambiguity branch so a symbol with both keeps
                // the older, more specific token rather than silently changing
                // what those tests observe.
                pending.push((reports.len(), ext.file_path.as_str(), sym));
                reports.push(DeadSymbolReport {
                    symbol_name: identity,
                    file_path: ext.file_path.clone(),
                    confidence: cap(0.4),
                    is_exempt: false,
                    exemption_reason: Some(UNRESOLVED_NAMESAKE_REASON.to_string()),
                    evidence: Some(DeadEvidence::none()),
                });
            } else if let Some(reason) = dispatch_reason {
                // Something outside the graph dispatches to it by name — a
                // library base class, or whatever an object literal is handed
                // to. Same tier as the namesake veto, its own reason.
                pending.push((reports.len(), ext.file_path.as_str(), sym));
                reports.push(DeadSymbolReport {
                    symbol_name: identity,
                    file_path: ext.file_path.clone(),
                    confidence: cap(0.4),
                    is_exempt: false,
                    exemption_reason: Some(reason),
                    evidence: Some(DeadEvidence::none()),
                });
            } else if !is_called && !is_exported && unspared {
                // The corpus-level half of X6. A confident finding here means
                // "no edge in the whole generation names this symbol" — which
                // is only evidence when every file got to contribute its edges.
                // While it did not, the finding stays visible (hiding it would
                // be its own lie) but must not reach the tier `CLAUDE.md` tells
                // agents to act on.
                let confidence = cap(0.9);
                if confidence > HIGHEST_DEGRADED_CONFIDENCE && !confident_tier_eligible(language) {
                    // The evidence earned the confident tier and the language
                    // has not. Only a row that would otherwise publish there is
                    // touched: a coverage-capped row already says why it is
                    // unconfirmed, and its graded confidence is the ranking
                    // the coverage curve exists to keep.
                    reports.push(DeadSymbolReport {
                        symbol_name: identity,
                        file_path: ext.file_path.clone(),
                        confidence: UNPROVEN_TIER_CONFIDENCE,
                        is_exempt: false,
                        exemption_reason: Some(confident_tier_unproven_reason(language)),
                        evidence: Some(DeadEvidence::none()),
                    });
                } else {
                    reports.push(DeadSymbolReport {
                        symbol_name: identity,
                        file_path: ext.file_path.clone(),
                        confidence,
                        is_exempt: false,
                        // Most specific reason wins, matching the exempt branch
                        // below. A symbol in a call-blind file is not merely
                        // downstream of somebody else's coverage hole — its own
                        // file is the hole, and the reader's next move differs:
                        // corpus loss invites a re-index, a missing extractor
                        // does not.
                        exemption_reason: if file_is_call_blind {
                            Some(CALL_BLIND_REASON.to_string())
                        } else if coverage.is_complete_for(language) {
                            None
                        } else {
                            Some(COVERAGE_LOSS_REASON.to_string())
                        },
                        evidence: None,
                    });
                }
            } else if let Some(proof) = (!is_called && is_exported && unspared)
                .then(|| module_private_proof(ext, sym, &scopes, &external_bases, &supertypes))
                .flatten()
            {
                // An export the manifest proves module-private. The keyword
                // that kept it off the list is not evidence of a caller, but
                // the language's entry-point model is not proven either, so it
                // enters at the unconfirmed tier and never above it.
                pending.push((reports.len(), ext.file_path.as_str(), sym));
                reports.push(DeadSymbolReport {
                    symbol_name: identity,
                    file_path: ext.file_path.clone(),
                    confidence: cap(UNPROVEN_TIER_CONFIDENCE),
                    is_exempt: false,
                    exemption_reason: Some(exported_unreferenced_reason(proof)),
                    evidence: Some(DeadEvidence::none()),
                });
            } else if !is_called {
                reports.push(DeadSymbolReport {
                    symbol_name: identity,
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
                            .or_else(|| {
                                Some(if is_exported {
                                    EXPORTED_NOT_EXAMINED_REASON.to_string()
                                } else {
                                    FILE_EXEMPT_REASON.to_string()
                                })
                            })
                    },
                    evidence: None,
                });
            }
        }
    }

    attach_site_evidence(extractions, resolution, &mut reports, &pending);

    LivenessOutcome { reports, coverage }
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
            body_signature: None,
            declaration_hash: None,
            return_type: None,
        }
    }

    /// A partial parse is listed wherever it is, and charged only where it can
    /// hide a caller: a call-extracting language, outside a vendored tree, a
    /// region that holds a name. The reason names the merged byte ranges lost,
    /// overlap counted once.
    #[test]
    fn partial_parses_are_listed_everywhere_and_charged_where_they_can_hide_a_caller() {
        let ranges = |ranges: &[(usize, usize)]| -> Vec<TextRange> {
            ranges
                .iter()
                .map(|(start_byte, end_byte)| TextRange {
                    start_byte: *start_byte,
                    end_byte: *end_byte,
                })
                .collect()
        };
        let partial = |path: &str, errors: &[(usize, usize)], nameless: &[(usize, usize)]| {
            let mut ext = devmap_extract::extract_file(path, "def x(): pass\n");
            ext.file_path = path.to_string();
            ext.parse_outcome = ParseOutcome::Partial {
                error_ranges: ranges(errors),
                nameless_ranges: ranges(nameless),
            };
            ext
        };
        // `def x(): pass` spans bytes 0..13: a region inside it is the `&raw`
        // shape — the declaration survived — and a zero-width one consumed
        // nothing. Both are listed and neither is charged. Nor is a region
        // outside it that the extractor found holds no name (a stray `}`); a
        // second region beside it that does hold one is charged.
        let corpus = vec![
            partial("src/app.py", &[(10, 50), (20, 30), (60, 64)], &[]),
            partial("src/inner.py", &[(4, 8), (11, 11)], &[]),
            partial("vendor/lib/x.py", &[(20, 40)], &[]),
            partial("src/stray.py", &[(20, 21)], &[(20, 21)]),
            partial("src/both.py", &[(20, 21), (30, 40)], &[(20, 21)]),
        ];
        let listed: Vec<(String, String)> = extraction_gaps(&corpus)
            .into_iter()
            .filter(|entry| entry.gap == ExtractionGap::PartialParse)
            .map(|entry| (entry.path, entry.reason))
            .collect();
        assert_eq!(
            listed
                .iter()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>(),
            vec![
                "src/app.py",
                "src/inner.py",
                "vendor/lib/x.py",
                "src/stray.py",
                "src/both.py"
            ],
            "every partial file is listed"
        );
        assert!(
            listed[0]
                .1
                .contains("2 region(s), 44 byte(s) lost: 10..50, 60..64"),
            "nested ranges are merged before they are summed: {}",
            listed[0].1
        );
        let coverage = extraction_coverage(&corpus);
        assert_eq!(
            coverage.partial_parse_files, 2,
            "only the files with a region that holds a name and lies outside every \
             surviving declaration, outside a vendored tree, are charged"
        );
        assert_eq!(coverage.blind_by_language.get("python"), Some(&2));
        assert!(!coverage.is_complete());
    }

    /// The listing says which regions were charged and which were not, and
    /// why — the same verdict the charge was decided by. Before, every
    /// partial file read "declarations and calls inside them are absent from
    /// the graph", including the `&raw` shape whose declaration survived and
    /// which was never charged: an exemption the reader could not see.
    #[test]
    fn a_partial_parse_reason_names_each_exempt_region_and_why() {
        let region = |start_byte: usize, end_byte: usize| TextRange {
            start_byte,
            end_byte,
        };
        let partial = |path: &str, errors: Vec<TextRange>, nameless: Vec<TextRange>| {
            let mut ext = devmap_extract::extract_file(path, "def x(): pass\n");
            ext.file_path = path.to_string();
            ext.parse_outcome = ParseOutcome::Partial {
                error_ranges: errors,
                nameless_ranges: nameless,
            };
            ext
        };
        let reason_for = |ext: &Extraction| -> String {
            extraction_gaps(std::slice::from_ref(ext))
                .into_iter()
                .find(|entry| entry.gap == ExtractionGap::PartialParse)
                .map(|entry| entry.reason)
                .expect("a partial file is listed")
        };

        // `def x(): pass` spans 0..13. Inside it, a stray nameless `}`
        // outside it, a zero-width MISSING token, and one charged region.
        let mixed = partial(
            "src/mixed.py",
            vec![region(4, 8), region(20, 21), region(11, 11), region(30, 40)],
            vec![region(20, 21)],
        );
        let reason = reason_for(&mixed);
        assert!(reason.contains("byte(s) lost"), "{reason}");
        assert!(
            reason.contains("charged: 1 region(s) outside every surviving declaration (30..40)"),
            "{reason}"
        );
        assert!(
            reason
                .contains("not charged: 1 region(s) inside a surviving declaration (4..8 in `x`)"),
            "{reason}"
        );
        assert!(
            reason.contains("1 region(s) that hold no name (20..21)"),
            "{reason}"
        );
        assert!(
            reason.contains("1 zero-width MISSING token(s) (11..11)"),
            "{reason}"
        );

        // Nothing charged: the reason must not claim anything was lost from
        // the graph.
        let exempt = partial("src/raw.py", vec![region(4, 8)], Vec::new());
        let reason = reason_for(&exempt);
        assert!(reason.contains("not charged"), "{reason}");
        assert!(
            !reason.contains("outside every surviving declaration"),
            "{reason}"
        );
        assert!(!reason.contains("absent from the graph"), "{reason}");

        // A vendored file's charged-shape region is named, and so is the
        // reason the file as a whole is not charged.
        let vendored = partial("vendor/lib/x.py", vec![region(30, 40)], Vec::new());
        let reason = reason_for(&vendored);
        assert!(reason.contains("vendored"), "{reason}");
        assert_eq!(extraction_coverage(&[vendored]).partial_parse_files, 0);
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
                nameless_ranges: Vec::new(),
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
            evidence: None,
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
                    candidates: vec![("f.py".to_string(), "helper".to_string())].into(),
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
