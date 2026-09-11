use crate::model::*;
use devmap_extract::languages::{capabilities_for_language, Capability};
use devmap_extract::model::*;
use devmap_resolve::model::*;
use std::collections::{BTreeMap, HashMap, HashSet};

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
}

/// What discovery refused, for the analysis that cannot see it.
///
/// A separate type rather than a bare `usize` so a caller cannot pass the wrong
/// count positionally, and so the one place that means "no discovery step ran"
/// is spelled [`DiscoveryCoverage::none`] rather than `0`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiscoveryCoverage {
    /// `None` means no discovery result is being reported — which is **not**
    /// the same as a discovery step that ran and refused nothing. Keeping the
    /// two apart is the whole reason this is an `Option` and not a `usize`: a
    /// summary that records `0` for the first case tells a later reader the
    /// tree was fully walked when nobody walked it.
    refused_files: Option<usize>,
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
        }
    }

    /// A discovery step ran and refused this many files. `refused(0)` is a
    /// measurement, and says more than [`DiscoveryCoverage::none`] does.
    pub fn refused(refused_files: usize) -> Self {
        Self {
            refused_files: Some(refused_files),
        }
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
            .saturating_add(self.discovery_refused_files);
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
            Some(Resolution::AmbiguousGlobal { .. }) | Some(Resolution::Unresolved { .. })
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
/// **Only two classes qualify for the veto, not every non-builtin.** Classes
/// with affirmative evidence that the site meant something else — `Builtin`,
/// `HostGlobal`, `LocalBinding`, `External`, `NoNamesake`, `ModulePath` — must
/// not veto. What is left is the two tiers where the resolver admits it does
/// not know: `UninferredReceiver` and `Unresolved`.
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
        .filter(|row| {
            matches!(
                row.kind,
                UnresolvedKind::Call | UnresolvedKind::Reference | UnresolvedKind::Route
            ) && matches!(
                row.class,
                UnresolvedClass::UninferredReceiver | UnresolvedClass::Unresolved
            )
        })
        .map(|row| row.callee_name.as_str())
        .filter(|name| !name.is_empty())
        .collect()
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
        .map(|ext| (ext.file_path.as_str(), ext.language.as_str()))
        .collect();
    for entry in extraction_gaps(extractions) {
        match entry.gap {
            ExtractionGap::ParseFailed => coverage.parse_failed_files += 1,
            ExtractionGap::PatternRecovered => coverage.pattern_recovered_files += 1,
            ExtractionGap::CallBlind => coverage.call_blind_files += 1,
            ExtractionGap::ImportBlind => coverage.import_blind_files += 1,
            ExtractionGap::NotParsed => coverage.not_parsed_files += 1,
        }
        // ImportBlind and NotParsed stay out of the capping numerator — same
        // policy as `blind_files` before language scoping. The rest feed the
        // per-language map that `cap_for` reads.
        if matches!(
            entry.gap,
            ExtractionGap::ParseFailed | ExtractionGap::PatternRecovered | ExtractionGap::CallBlind
        ) {
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
        if a_grammar_read_this_file(ext) && ext.capabilities().contains(Capability::Calls) {
            coverage.files_with_call_extraction += 1;
            *coverage
                .covered_by_language
                .entry(ext.language.clone())
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
fn symbol_exemption_index(
    extractions: &[Extraction],
    resolution: &ResolutionResult,
    called: &HashSet<(String, String)>,
    ambiguous: &HashSet<(String, String)>,
) -> HashMap<(String, String), String> {
    let supertypes = supertypes_by_type(resolution);
    let go_interface_specs = go_interface_specs_by_package(extractions);
    let c_header_exports = c_header_exported_names(extractions);
    let go_build_variants = go_build_variant_identities(extractions);

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
            if sym.kind == SymbolKind::File || sym.name.starts_with('_') {
                continue;
            }
            let identity = dead_symbol_identity(sym, &ext.file_path);
            let is_ambiguously_called = ambiguous
                .contains(&(ext.file_path.clone(), sym.name.clone()))
                || ambiguous.contains(&(ext.file_path.clone(), sym.qualified_name.clone()));

            let heritage =
                reached_through_a_supertype(&ext.file_path, &identity, &supertypes, called);

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

    let mut reports = Vec::new();

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
        let file_is_call_blind =
            a_grammar_read_this_file(ext) && !ext.capabilities().contains(Capability::Calls);

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
                coverage.cap_call_blind_file_for(&ext.language, confidence)
            } else {
                coverage.cap_for(&ext.language, confidence)
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
        let is_file_exempt = is_parse_failed
            || matches!(ext.file_liveness(), FileLiveness::NotCode { .. })
            || file_wiring.iter().any(|w| {
                matches!(
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
                        | WiringKind::ScriptEntry
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
                // No grammar ran, so there are no error ranges to overlap.
                // The file is exempt wholesale via `is_parse_failed` above.
                ParseOutcome::Clean
                | ParseOutcome::Failed { .. }
                | ParseOutcome::Fallback { .. }
                | ParseOutcome::Skipped { .. } => false,
            };

            let is_exported = sym.is_exported;
            // One lookup where six `.or_else` arms used to sit. The arms moved
            // to `symbol_exemption_index` unchanged and in the same order —
            // symbol wiring, heritage override, Go interface, C header,
            // exported owner, Go build variant — because the reason string is a
            // machine token three tests match exactly.
            let symbol_exemption: Option<&str> = exemptions
                .get(&(ext.file_path.clone(), sym.qualified_name.clone()))
                .map(String::as_str);

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
                    confidence: cap(0.4),
                    is_exempt: false,
                    exemption_reason: Some("only_ambiguous_callers".to_string()),
                });
            } else if !is_called
                && !is_exported
                && !is_file_exempt
                && symbol_exemption.is_none()
                && !overlaps_parse_error
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
                reports.push(DeadSymbolReport {
                    symbol_name: dead_symbol_identity(sym, &ext.file_path),
                    file_path: ext.file_path.clone(),
                    confidence: cap(0.4),
                    is_exempt: false,
                    exemption_reason: Some(UNRESOLVED_NAMESAKE_REASON.to_string()),
                });
            } else if !is_called
                && !is_exported
                && !is_file_exempt
                && symbol_exemption.is_none()
                && !overlaps_parse_error
            {
                // The corpus-level half of X6. A confident finding here means
                // "no edge in the whole generation names this symbol" — which
                // is only evidence when every file got to contribute its edges.
                // While it did not, the finding stays visible (hiding it would
                // be its own lie) but must not reach the tier `CLAUDE.md` tells
                // agents to act on.
                reports.push(DeadSymbolReport {
                    symbol_name: dead_symbol_identity(sym, &ext.file_path),
                    file_path: ext.file_path.clone(),
                    confidence: cap(0.9),
                    is_exempt: false,
                    // Most specific reason wins, matching the exempt branch
                    // below. A symbol in a call-blind file is not merely
                    // downstream of somebody else's coverage hole — its own
                    // file is the hole, and the reader's next move differs:
                    // corpus loss invites a re-index, a missing extractor does
                    // not.
                    exemption_reason: if file_is_call_blind {
                        Some(CALL_BLIND_REASON.to_string())
                    } else if coverage.is_complete_for(&ext.language) {
                        None
                    } else {
                        Some(COVERAGE_LOSS_REASON.to_string())
                    },
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
