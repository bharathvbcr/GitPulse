use super::{bare_callee_name, family_of_path, QueryEngine, StoreQueryEngine};
use crate::cancel::Cancel;
use crate::model::Response;
use devmap_extract::model::ParseOutcome;
use devmap_resolve::model::{LangFamily, UnresolvedReference};
use devmap_store::StoredSymbol;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Why one file's edge list is a lower bound, or `None` when it is not.
///
/// `Failed` is not here: a file that contributed nothing is refused outright
/// with `resolution: Unavailable`, which is a stronger statement than this one
/// and is made by the callers. The two states below did contribute — they are
/// the ones that answer in the shape of a complete extraction while holding
/// less than one.
///
/// `Clean` returns `None`, and that is the load-bearing case: a caveat that
/// rides on every answer tells a reader nothing, which is the failure mode
/// [`analysis_coverage_gap`] documents for its own marker.
///
/// One owner for both engines. `StoreQueryEngine::dependencies` reads the
/// outcome off a stored row and `QueryEngine::dependencies` off an in-memory
/// `Extraction`, and two near-copies of this sentence would let the same file
/// be described differently depending on which one was asked.
pub(super) fn file_edge_coverage_gap(outcome: &ParseOutcome) -> Option<String> {
    match outcome {
        ParseOutcome::Clean | ParseOutcome::Failed { .. } => None,
        ParseOutcome::Fallback { reason } => Some(format!(
            "this file's declarations were recovered by pattern rather than parsed ({reason}); \
             the pattern scanner extracts no calls and no imports at all, so an empty or short \
             list here is not evidence the file has no dependencies"
        )),
        ParseOutcome::Partial { error_ranges, .. } => Some(format!(
            "this file parsed with {} error range(s); a call or import inside an error region \
             is invisible to extraction, so this list is a lower bound",
            error_ranges.len()
        )),
        // Unlike `Failed`, the callers do *not* refuse this file — it is
        // indexed and its `File` node is a real node — so if the caveat were
        // `None` here the answer would be an empty dependency list presented as
        // a fact about the file. It is a fact about the extractor.
        ParseOutcome::Skipped { reason } => Some(format!(
            "this file was not parsed ({reason}); no calls or imports were extracted from it \
             at all, so an empty list here is not evidence the file has no dependencies"
        )),
    }
}

/// Most distinct start files one traversal reads a call-blind verdict for.
///
/// A traversal start is almost always one symbol in one file; a bare name like
/// `new` can match hundreds. Each check is one store read, so the fan-out is
/// bounded, and what the bound skipped is reported in
/// [`CallBlindStarts::reason`] rather than treated as clean.
pub(super) const MAX_CALL_BLIND_FILES_CHECKED: usize = 16;

/// The call-blind files among a traversal's starts. See
/// [`StoreQueryEngine::call_blind_starts`].
#[derive(Debug, Default)]
pub(super) struct CallBlindStarts {
    /// `(path, language)` of every checked start file that is call-blind.
    pub(super) blind: Vec<(String, String)>,
    /// Distinct start files actually read.
    pub(super) checked: usize,
    /// Distinct start files the bound left unread.
    pub(super) unchecked: usize,
    /// Checked start files whose stored parse was `Clean`. Read from the same
    /// row the call-blind verdict is, so the leaf answer in `traverse_walked`
    /// costs no second store read per target.
    pub(super) clean: BTreeSet<String>,
}

impl CallBlindStarts {
    /// Fold one read start file in. The single place a file's parse outcome and
    /// language become a call-blind verdict, shared by the store-backed engine
    /// (which reads the file row) and the in-memory one (which holds the
    /// extraction).
    pub(super) fn record(
        &mut self,
        path: &str,
        language: &str,
        engine: &devmap_extract::model::ExtractionEngine,
        parse_outcome: &devmap_extract::model::ParseOutcome,
    ) {
        self.checked += 1;
        if matches!(parse_outcome, devmap_extract::model::ParseOutcome::Clean) {
            self.clean.insert(path.to_string());
        }
        if devmap_extract::model::is_call_blind(language, engine, parse_outcome) {
            self.blind.push((path.to_string(), language.to_string()));
        }
    }

    /// The refusal an empty `dependencies` answer must give when nothing it rests
    /// on was measured, or `None` when the emptiness is a measured one.
    pub(super) fn refusal_when_empty(&self, target: &str) -> Option<String> {
        if !self.every_start_is_blind() {
            return None;
        }
        Some(format!(
            "{target} has no measured dependencies: {}",
            self.reason().unwrap_or_default()
        ))
    }

    /// Whether nothing the answer rests on was measured: at least one start
    /// was checked, every checked start is call-blind, and none went unread.
    pub(super) fn every_start_is_blind(&self) -> bool {
        self.checked > 0 && self.unchecked == 0 && self.blind.len() == self.checked
    }

    /// The caveat, or `None` when no checked start is call-blind.
    ///
    /// `None` on every answer about a language that has a call extractor is
    /// the load-bearing case, for the reason [`analysis_coverage_gap`] gives:
    /// a caveat that rides on every answer tells a reader nothing. Wording is
    /// direction-neutral because `impact` and `trace` share it, and it keeps
    /// the phrase "no call extractor" that `dead`'s `CALL_BLIND_REASON` and
    /// the coverage report use, so all three read as one fact.
    pub(super) fn reason(&self) -> Option<String> {
        if self.blind.is_empty() {
            return None;
        }
        let files: Vec<String> = self
            .blind
            .iter()
            .map(|(path, language)| format!("{path} (`{language}`)"))
            .collect();
        let mut reason = format!(
            "{} {} in a language with no call extractor in this build; no call into or out of \
             {} was ever extracted, so an empty or short answer here is the extractor's \
             absence, not the code's",
            files.join(", "),
            if files.len() == 1 { "is" } else { "are" },
            if files.len() == 1 { "it" } else { "them" },
        );
        if self.unchecked > 0 {
            reason.push_str(&format!(
                "; {} more start file(s) were not checked for this",
                self.unchecked
            ));
        }
        Some(reason)
    }
}

/// The file a traversal query names, when it names one — for the answer that
/// found no start and so has no start file to ask about.
pub(super) fn query_file(query: &str) -> Option<&str> {
    match crate::query_match::classify(query) {
        crate::query_match::StartQuery::Qualified { file, .. } => Some(file),
        crate::query_match::StartQuery::Path(path) => Some(path),
        crate::query_match::StartQuery::Symbol(_) | crate::query_match::StartQuery::Nothing => None,
    }
}

pub(super) fn qualify_response<T>(response: &mut Response<T>, reason: &str) {
    response.walk_incomplete =
        devmap_analyze::combine_reasons(response.walk_incomplete.take(), Some(reason.to_string()));
}

/// Why an answer derived from one generation's graph is a lower bound.
///
/// Two independent reasons, joined rather than ranked — a reader deciding
/// whether to act on "nothing calls this" needs every qualification the run
/// holds, not the first one that fired. `None` on a converged analysis with
/// every call attributed is the load-bearing case: a marker that appears on
/// every answer leaves a caller exactly where it started.
///
/// Shared by `dead_symbols` and by every traversal, because they are the same
/// claim about the same graph. `dead_symbols` had it and `impact` did not,
/// which is backwards: the dead list is explicitly a *candidate* list and
/// already exempts symbols in unread files, while `impact` is what a reader
/// consults immediately before deleting a symbol, and it answered `items: [],
/// resolution: Available, walk_incomplete: None` over a corpus whose only
/// calling file had never been parsed.
///
/// The wording is direction-neutral for that reason: it describes the holes in
/// the graph, and leaves what those holes mean to the surface that names
/// itself.
pub(super) fn analysis_coverage_gap(
    analysis: Option<&devmap_analyze::model::AnalysisDisclosure>,
) -> Option<String> {
    let unresolved = analysis.and_then(|analysis| {
        attribution_coverage_gap(analysis.unresolved_calls, analysis.resolution_rate.as_ref())
    });
    devmap_analyze::combine_reasons(analysis_status_gap(analysis), unresolved)
}

/// Unresolved sites include known external targets and are not an edge count.
/// Use the persisted rate's existing classification, with conservative fallback
/// for older or inconsistent summaries. These are repository-wide measurements;
/// no attribution data establishes how many missing links affect this target.
pub(super) fn attribution_coverage_gap(
    total: Option<usize>,
    coverage: Option<&devmap_analyze::AttributionCoverage>,
) -> Option<String> {
    let Some(total) = total else {
        return Some("the unresolved attribution count was not recorded for this generation; call-graph coverage is unknown".to_string());
    };
    match coverage {
        Some(coverage) if coverage.unresolved_sites == total && coverage.explained_sites <= total => {
            let remaining = total - coverage.explained_sites;
            (remaining > 0).then(|| format!(
                "{remaining} of {total} unresolved attribution site(s) have no indexed target after excluding {} site(s) classified as builtin, runtime-global, external-import, no-namesake, or module-path; classification does not prove complete source coverage; these repository-wide counts are not specific to this target, so this answer may omit callers or dependencies that name it",
                coverage.explained_sites,
            ))
        }
        None if total == 0 => None,
        _ => Some(format!(
            "this generation records {total} unresolved attribution site(s), but their classification breakdown is unavailable or inconsistent; these repository-wide counts are not specific to this target, so call-graph coverage for it is unknown"
        )),
    }
}

/// Most distinct keys — reached names, walked symbols, or files — one answer
/// checks against the unresolved ledger. Past it the check is reported as
/// partial, never as clean.
pub(super) const MAX_RADIUS_LEDGER_KEYS: usize = 512;

/// Ledger rows read per key. The note counts what it read and says "at least"
/// when a key had more.
pub(super) const RADIUS_SITES_PER_KEY: usize = 4;

/// How many site names one note spells out.
pub(super) const RADIUS_NOTE_SAMPLE: usize = 5;

/// Which unattributed sites can hide part of a walk's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RadiusSide {
    /// A walk toward callers: a site that *names* a reached symbol may be a
    /// caller the resolver could not bind, so the walk could not follow it.
    Callers,
    /// A walk toward callees: a site *inside* a walked symbol may call
    /// something the resolver could not bind.
    Callees,
    /// A file's own outbound edges: a site written in that file.
    FileCallees,
}

/// Most never-indexed paths an "outside index" answer names.
const OUTSIDE_INDEX_SAMPLE: usize = 5;

/// Most indexed files whose charged ranges one missed lookup reads from disk.
///
/// The cost of a miss is this many hardened reads: `safe_fs::read_repo_source`
/// walks every path component without following links, about 100 µs a file.
/// Release min-of-5 on `search_partial_attack::p5_timing_miss_on_2000_partial_files`
/// (2,000 partial files, unscoped): base miss about 0.39 ms; cap 64 about
/// 6.2 ms (16x); cap 16 about 1.93 ms (about 5x). The 3x target was missed and
/// the cap lowered rather than the target relaxed, per Fable's ruling of
/// 2026-10-18 (option b). Files past the cap are counted and reported as "could
/// not be checked", so correctness does not depend on the number. A
/// cross-query cache of read windows was rejected: global state to speed a cold
/// path.
const MAX_CHARGED_FILES_READ: usize = 16;

/// Most charged ranges (or unchecked files) one answer spells out.
const CHARGED_RANGE_SAMPLE: usize = 5;

/// Longest path one sentence of an outside-index reason carries, in bytes of
/// the escaped text; [`crate::bounded_text`] names what it cut.
const REASON_PATH_BYTES: usize = 160;

/// Longest looked-up name one sentence carries.
const REASON_NAME_BYTES: usize = 128;

/// The phrase that opens each region list of the stored partial-parse sentence
/// (see `devmap_analyze`, partial-parse reason). Also the SQL filter
/// that picks the rows worth reading, so the reader and the filter cannot
/// disagree about which rows list a region that could hold a declaration.
const REGION_LIST: &str = "surviving declaration (";

/// What a missed lookup found out about the holes it was asked about.
#[derive(Debug, Default)]
pub(super) struct OutsideIndex {
    /// The lookup cannot answer "no such symbol": the resolution is
    /// `Unavailable` with this reason.
    pub(super) reason: Option<String>,
    /// Charged regions that could not be checked, on a lookup that is not
    /// scoped to them. Not an `Unavailable` (most misses in a repository with
    /// unread files would be), but never silent: it joins `walk_incomplete`.
    pub(super) note: Option<String>,
}

fn bounded_path(path: &str) -> String {
    crate::bounded_text(path, REASON_PATH_BYTES)
}

/// Grammar keys of the languages whose identifiers are case-insensitive. The
/// extractor's `LanguageSpec` registry carries no case rule, so this is the one
/// place one is written; every other language — Rust, Go, Python, TypeScript,
/// Swift, Kotlin — matches a name exactly.
///
/// PHP is here because its function, method and class names ignore case. Its
/// variables do not, but a range does not say which kind of name it holds, and
/// a false "may declare it" is worded as a maybe where a missed one is silence.
/// The other registry grammars were checked: C#, Java, Ruby, Lua, Erlang, R,
/// Scala, Dart, Solidity, Terraform and Nix are case-sensitive, and the
/// markup grammars name no declarations.
fn language_folds_case(language: &str) -> bool {
    matches!(language, "sql" | "cobol" | "pascal" | "vb" | "cfml" | "php")
}

/// One charged region whose text holds the looked-up name.
#[derive(Debug)]
struct ChargedMatch {
    path: String,
    first_line: usize,
    last_line: usize,
    start: usize,
    end: usize,
    /// False when every occurrence sits on a line that looks like a comment or
    /// holds an unbalanced quote.
    in_code: bool,
}

/// A partial file whose charged regions the lookup could not rule out.
#[derive(Debug)]
struct UncheckedFile {
    path: String,
    /// Charged regions this covers (the stored listing's elided tail included).
    regions: usize,
    why: String,
}

#[derive(Debug, Default)]
struct ChargedHoles {
    matches: Vec<ChargedMatch>,
    unchecked: Vec<UncheckedFile>,
    /// Partial files past [`MAX_CHARGED_FILES_READ`], never opened.
    files_past_cap: usize,
    /// The caller narrowed to paths or languages, so an unchecked region in
    /// scope is the answer rather than background.
    scoped: bool,
    /// Attribution did not run (the query names no identifier) over partial
    /// files in scope: said, never silent.
    skipped: Option<String>,
}

fn listed(items: Vec<String>) -> String {
    let more = items.len().saturating_sub(CHARGED_RANGE_SAMPLE);
    let mut text = items
        .into_iter()
        .take(CHARGED_RANGE_SAMPLE)
        .collect::<Vec<_>>()
        .join("; ");
    if more > 0 {
        text.push_str(&format!("; and {more} more"));
    }
    text
}

impl ChargedHoles {
    fn match_sentence(&self, query: &str) -> Option<String> {
        if self.matches.is_empty() {
            return None;
        }
        let rows = self
            .matches
            .iter()
            .map(|m| {
                format!(
                    "{} lines {}-{} (bytes {}..{}{})",
                    bounded_path(&m.path),
                    m.first_line,
                    m.last_line,
                    m.start,
                    m.end,
                    if m.in_code {
                        ""
                    } else {
                        "; the name appears only on lines that look like a comment or a \
                         string, so this may not be a declaration"
                    }
                )
            })
            .collect();
        Some(format!(
            "no indexed symbol matches `{}`, but {} charged partial-parse range(s) of indexed \
             files contain that name and may declare it — the parser lost these regions, so \
             declarations inside them are absent from the index: {}",
            crate::bounded_text(query, REASON_NAME_BYTES),
            self.matches.len(),
            listed(rows)
        ))
    }

    fn unchecked_sentence(&self) -> Option<String> {
        let mut parts = Vec::new();
        if !self.unchecked.is_empty() {
            let regions: usize = self.unchecked.iter().map(|file| file.regions).sum();
            let items = self
                .unchecked
                .iter()
                .map(|file| format!("{} ({})", bounded_path(&file.path), file.why))
                .collect();
            parts.push(format!(
                "{regions} partial-parse region(s) in {} file(s) could not be checked for this \
                 name: {}",
                self.unchecked.len(),
                listed(items)
            ));
        }
        if self.files_past_cap > 0 {
            parts.push(format!(
                "{} more partial-parse file(s) could not be checked: past the \
                 {MAX_CHARGED_FILES_READ}-file read cap",
                self.files_past_cap
            ));
        }
        (!parts.is_empty()).then(|| parts.join("; "))
    }

    /// `(reason, background)`: the sentence that makes the answer unavailable,
    /// and the one that only qualifies it. A matching region or, under a
    /// scope, any unchecked one is the reason; unchecked regions elsewhere are
    /// background.
    fn split(&self, query: &str) -> (Option<String>, Option<String>) {
        let matched = self.match_sentence(query);
        let unchecked = self.unchecked_sentence();
        if let Some(skipped) = &self.skipped {
            return (None, Some(skipped.clone()));
        }
        if matched.is_some() || self.scoped {
            let reason = match (matched, unchecked) {
                (Some(matched), Some(unchecked)) => Some(format!("{matched}; {unchecked}")),
                (one, other) => one.or(other),
            };
            (reason, None)
        } else {
            (None, unchecked)
        }
    }
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_identifier(segment: &str) -> bool {
    let mut chars = segment.chars();
    chars
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_')
        && chars.all(is_ident)
}

/// What a lookup names: the qualifiers before the last segment, and the last
/// segment, or why the query names no identifier.
///
/// Several words describe a symbol rather than name it, and a path names a
/// file. A dotted or `::` query whose segments are all identifiers is a
/// qualified identifier (`mod::Thing`, `Class.method`, and also `foo.go`, a
/// method `go` on `foo`: an extension-shaped tail is not evidence of a file
/// name, and skipping it answered a clean zero over a hole). `release.0` is a
/// version, so a numeric segment is refused.
///
/// Qualifiers are returned because the tail alone is too weak a test:
/// `nosuchfile.rs` would otherwise blame any region that mentions an `rs`
/// token. [`qualifier_present`] asks that each qualifier also occur in the file.
fn query_target(query: &str) -> Result<(Vec<&str>, &str), &'static str> {
    let query = query.trim();
    if query.is_empty() || query.contains(char::is_whitespace) {
        return Err("the query is a phrase, not a name");
    }
    if query.contains('/') {
        return Err("the query contains a path separator");
    }
    let segments: Vec<&str> = query.split("::").flat_map(|part| part.split('.')).collect();
    if !segments.iter().all(|segment| is_identifier(segment)) {
        return Err("the query is not a (qualified) identifier");
    }
    let (name, qualifiers) = segments.split_last().ok_or("the query is empty")?;
    Ok((qualifiers.to_vec(), name))
}

/// Whether a qualifier of the query can belong to this file: it occurs in the
/// source as a whole identifier, or it is one of the file's own path
/// components (`mod.func` for a function in `pkg/mod.py`).
fn qualifier_present(qualifier: &str, source: &str, path: &str, fold: bool) -> bool {
    name_occurrence(source, qualifier, fold).is_some()
        || path.split('/').any(|component| {
            let stem = component
                .rsplit_once('.')
                .map_or(component, |(stem, _)| stem);
            stem == qualifier || (fold && stem.eq_ignore_ascii_case(qualifier))
        })
}

/// Whether `window` holds `name` as a whole identifier: `None` for no,
/// `Some(true)` when some occurrence is on a line that does not look like a
/// comment or a string, `Some(false)` when every occurrence does. Exact unless
/// `fold`, which is for languages whose identifiers ignore case.
fn name_occurrence(window: &str, name: &str, fold: bool) -> Option<bool> {
    let wanted: Vec<char> = if fold {
        name.chars().flat_map(char::to_lowercase).collect()
    } else {
        name.chars().collect()
    };
    let mut seen = false;
    let mut prev: Option<char> = None;
    for (index, c) in window.char_indices() {
        let boundary_before = prev.is_none_or(|p| !is_ident(p));
        prev = Some(c);
        if !boundary_before {
            continue;
        }
        let mut tail = window[index..].chars();
        let mut at = 0usize;
        let mut matched = true;
        while at < wanted.len() {
            let Some(next) = tail.next() else {
                matched = false;
                break;
            };
            if fold {
                for lowered in next.to_lowercase() {
                    matched &= wanted.get(at) == Some(&lowered);
                    at += 1;
                }
            } else {
                matched = wanted[at] == next;
                at += 1;
            }
            if !matched {
                break;
            }
        }
        if !matched || tail.next().is_some_and(is_ident) {
            continue;
        }
        seen = true;
        let line_start = window[..index].rfind('\n').map_or(0, |at| at + 1);
        if !looks_like_comment_or_string(&window[line_start..index]) {
            return Some(true);
        }
    }
    seen.then_some(false)
}

/// Whether the text before an occurrence, on its own line, puts it inside a
/// comment or a string literal. A heuristic, which is why it only changes the
/// wording of a "may declare" and never removes the claim.
fn looks_like_comment_or_string(before: &str) -> bool {
    let trimmed = before.trim_start();
    ["//", "#", "--", "*", "/*", ";"]
        .iter()
        .any(|marker| trimmed.starts_with(marker))
        || before.contains("//")
        || before.matches('"').count() % 2 == 1
}

/// The byte ranges the stored partial-parse sentence lists as able to hold a
/// declaration, and how many it elided with "and N more".
///
/// Two lists qualify, both introduced by `surviving declaration (`: the regions
/// *outside* every surviving declaration (charged, or "would be charged" for a
/// file the charge exempts), and the regions *inside* one (`4..8 in `x``). The
/// charge exempts the second kind from the dead-code cap, but a lost method of
/// a surviving class is still a declaration the index lacks, so a name lookup
/// must not skip it. Nameless regions and zero-width MISSING tokens cannot hold
/// a name and are not listed here.
fn parse_charged_ranges(reason: &str) -> (Vec<(usize, usize)>, usize) {
    let mut ranges = Vec::new();
    let mut elided = 0usize;
    let mut rest = reason;
    while let Some(at) = rest.find(REGION_LIST) {
        let list = &rest[at + REGION_LIST.len()..];
        // The list ends at the first `)` outside backticks (an item may name a
        // declaration such as `f()`).
        let mut in_name = false;
        let end = list
            .char_indices()
            .find(|&(_, c)| match c {
                '`' => {
                    in_name = !in_name;
                    false
                }
                ')' => !in_name,
                _ => false,
            })
            .map_or(list.len(), |(index, _)| index);
        for item in list[..end].split(", ") {
            if let Some((start, tail)) = item.split_once("..") {
                let digits = tail
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(tail.len());
                if let (Ok(start), Ok(stop)) = (start.trim().parse(), tail[..digits].parse()) {
                    ranges.push((start, stop));
                }
            } else if let Some(count) = item.trim().strip_prefix("and ") {
                elided += count.trim_end_matches(" more").trim().parse().unwrap_or(0);
            }
        }
        rest = &list[end..];
    }
    (ranges, elided)
}

/// Sorted, with overlapping and touching ranges joined: one region of text is
/// one region, however many tree nodes covered it.
fn merge_ranges(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            Some((_, last_end)) if start <= *last_end => *last_end = (*last_end).max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

impl StoreQueryEngine<'_> {
    /// Why a name lookup that matched nothing cannot answer "no such symbol":
    /// the paths this generation never indexed, or `None` when it indexed
    /// every path discovery met.
    ///
    /// `search detect_stubs` answered `resolution: Available, total: 0` over a
    /// repository whose `crates/dc-verify` — where `detect_stubs` is declared
    /// — was a symlink out of the tree that discovery refused. "Available, 0"
    /// is a completed check that found nothing; this was a check over a corpus
    /// with a hole exactly where the answer lives. A refused path and an
    /// over-limit file both have no rows, so a symbol declared in either is
    /// invisible to every name lookup, and an empty answer has to say so in
    /// the field a caller branches on, not only in a caveat.
    ///
    /// The caller's path scope narrows the holes to those under its prefixes
    /// (made repository-relative first): a lookup scoped to `src/` is not made
    /// unavailable by a refusal under `vendor/`.
    ///
    /// A hole is also a *charged partial-parse range of an indexed file*: the
    /// file has rows, but the parser lost a region and a declaration inside it
    /// has none. Those are checked first ([`Self::charged_hole_check`]) and
    /// named by file and line, because a range whose text holds the name is
    /// evidence and a never-read path is only absence of it; the latter is then
    /// listed as "also unread". A region the check could not rule out makes a
    /// scoped lookup unavailable, and qualifies an unscoped one
    /// ([`OutsideIndex::note`]). Every path and name that reaches the sentence
    /// is escaped and cut by [`crate::bounded_text`], and the whole sentence is
    /// held to [`crate::MAX_REASON_COMPONENT_BYTES`].
    pub(super) fn outside_index_reason(
        &self,
        filter: Option<&crate::scope::NameQueryFilter>,
        query: &str,
    ) -> anyhow::Result<OutsideIndex> {
        let root = self.store.latest_generation_repo_root()?;
        let prefixes: Vec<String> = filter
            .map_or(&[][..], |filter| filter.paths())
            .iter()
            .map(|prefix| {
                crate::scope::relative_to_root(prefix, root.as_deref())
                    .unwrap_or_else(|_| prefix.clone())
            })
            .collect();
        let inventory = self.store.latest_discovery_inventory()?;
        let holes: Vec<&str> = inventory
            .refusals
            .iter()
            .chain(inventory.over_limit.iter())
            .map(|row| row.path.as_str())
            .filter(|path| {
                prefixes.is_empty()
                    || prefixes
                        .iter()
                        .any(|prefix| crate::scope::under_prefix(path, prefix))
            })
            .collect();
        let unread = |label: &str| {
            let shown: Vec<String> = holes
                .iter()
                .take(OUTSIDE_INDEX_SAMPLE)
                .map(|path| bounded_path(path))
                .collect();
            let more = holes.len() - shown.len();
            format!(
                "{} path(s) this generation never indexed — refused by discovery or over the \
                 source size limit — {label}: {}{}",
                holes.len(),
                shown.join(", "),
                if more > 0 {
                    format!(", and {more} more")
                } else {
                    String::new()
                }
            )
        };
        let (charged, note) = self
            .charged_hole_check(filter, root.as_deref(), query)?
            .split(query);
        let reason = match (charged, holes.is_empty()) {
            (None, true) => None,
            (None, false) => Some(format!(
                "outside index: no indexed symbol matches, and {}",
                unread("may declare it")
            )),
            (Some(note), true) => Some(format!("outside index: {note}")),
            (Some(note), false) => Some(format!(
                "outside index: {note}; also unread, with nothing showing they declare it: {}",
                unread("not read at all")
            )),
        };
        let bound = |text: String| crate::bounded_text(&text, crate::MAX_REASON_COMPONENT_BYTES);
        Ok(OutsideIndex {
            reason: reason.map(bound),
            note: note.map(bound),
        })
    }

    /// The charged partial-parse regions of indexed files in the caller's scope
    /// whose text holds `query` as a whole identifier, and every region it could
    /// not rule out.
    ///
    /// The store keeps each partial file's charged byte ranges inside the
    /// sentence `status` prints (`coverage_gaps.partial_parse`), and
    /// [`parse_charged_ranges`] reads them back out of it; the round trip is
    /// pinned against the producer in this module's tests. The test per range
    /// is an identifier-boundary match over the file's bytes — exact, except in
    /// a language whose identifiers ignore case ([`language_folds_case`]). A
    /// declaration contains its own name, so a range without the name cannot be
    /// hiding it, and a range with it only might: the answer says "may",
    /// louder when every occurrence sits on a line that looks like a comment or
    /// a string.
    ///
    /// The file is read once, only when it is in scope and the query names an
    /// identifier, and only if its bytes still hash to what the generation
    /// recorded: ranges over an edited file describe other text. What cannot
    /// be decided is carried, never dropped: a changed or unreadable file, a
    /// range past the end or off a character boundary, the stored listing's
    /// elided tail, and files past [`MAX_CHARGED_FILES_READ`].
    fn charged_hole_check(
        &self,
        filter: Option<&crate::scope::NameQueryFilter>,
        repo_root: Option<&str>,
        query: &str,
    ) -> anyhow::Result<ChargedHoles> {
        let mut found = ChargedHoles::default();
        // An exact indexed file path is a file lookup, not a name.
        let target = match query_target(query) {
            Ok(_) if query.contains('.') && self.store.latest_file(query.trim())?.is_some() => {
                Err("the query is an indexed file path")
            }
            other => other,
        };
        // The scope layer's own resolution of paths and languages, so an
        // absolute path, `./sub` and a language-only scope admit exactly the
        // files the search itself was narrowed to.
        let resolved = match filter
            .map(|filter| filter.symbol_scope())
            .transpose()?
            .flatten()
        {
            Some(scope) => match self.store.latest_scope_inputs()? {
                Some((_, root, files)) => Some(scope.resolve(&files, root.as_deref())?),
                None => None,
            },
            None => None,
        };
        found.scoped = resolved.is_some();
        let admit_in_scope =
            |path: &str| resolved.as_ref().is_some_and(|scope| scope.contains(path));
        let admit: Option<&dyn Fn(&str) -> bool> = resolved.as_ref().map(|_| &admit_in_scope as _);
        let (qualifiers, name) = match target {
            Ok(target) => target,
            Err(why) => {
                // Attribution is skipped, and with partial files in scope that
                // must not read as a clean miss.
                let (_, held) = self
                    .store
                    .latest_partial_parse_gaps(REGION_LIST, admit, 0)?;
                if held > 0 {
                    found.skipped = Some(format!(
                        "{held} partial-parse file(s) in scope hold charged regions this lookup \
                         did not check for `{}`: {why}, so the name was not matched against them",
                        crate::bounded_text(query, REASON_NAME_BYTES)
                    ));
                }
                return Ok(found);
            }
        };
        let (rows, total) =
            self.store
                .latest_partial_parse_gaps(REGION_LIST, admit, MAX_CHARGED_FILES_READ)?;
        found.files_past_cap = total.saturating_sub(rows.len());
        for row in rows {
            self.cancel.check()?;
            let (ranges, elided) = parse_charged_ranges(&row.reason);
            let ranges = merge_ranges(ranges);
            let mut regions_unchecked = elided;
            let mut why: Vec<String> = Vec::new();
            if elided > 0 {
                why.push(format!(
                    "{elided} charged range(s) not listed in the stored reason"
                ));
            }
            if !ranges.is_empty() {
                match devmap_extract::safe_fs::read_repo_source(
                    repo_root.map(Path::new),
                    &row.path,
                    devmap_extract::MAX_SOURCE_BYTES,
                ) {
                    Err(error) => {
                        regions_unchecked += ranges.len();
                        why.push(format!(
                            "unreadable now: {}",
                            crate::bounded_text(&error.to_string(), REASON_PATH_BYTES)
                        ));
                    }
                    Ok(source) if devmap_extract::content_hash(&source) != row.content_hash => {
                        regions_unchecked += ranges.len();
                        why.push(
                            "changed since the build; its charged ranges were not checked"
                                .to_string(),
                        );
                    }
                    Ok(source) => {
                        let fold = language_folds_case(&row.language);
                        let line_of = |byte: usize| {
                            1 + source.as_bytes()[..byte.min(source.len())]
                                .iter()
                                .filter(|b| **b == b'\n')
                                .count()
                        };
                        let mut undecidable = 0usize;
                        let qualified_here = qualifiers
                            .iter()
                            .all(|q| qualifier_present(q, &source, &row.path, fold));
                        for (start, end) in ranges {
                            if !qualified_here {
                                break;
                            }
                            let Some(window) = source.get(start..end) else {
                                undecidable += 1;
                                continue;
                            };
                            let Some(in_code) = name_occurrence(window, name, fold) else {
                                continue;
                            };
                            found.matches.push(ChargedMatch {
                                path: row.path.clone(),
                                first_line: line_of(start),
                                last_line: line_of(end.saturating_sub(1).max(start)),
                                start,
                                end,
                                in_code,
                            });
                        }
                        if undecidable > 0 {
                            regions_unchecked += undecidable;
                            why.push(format!(
                                "{undecidable} range(s) past the end of the file or off a \
                                 character boundary"
                            ));
                        }
                    }
                }
            }
            if !why.is_empty() {
                found.unchecked.push(UncheckedFile {
                    path: row.path,
                    regions: regions_unchecked,
                    why: why.join(", "),
                });
            }
        }
        // Code occurrences first: they are the stronger claim.
        found.matches.sort_by(|a, b| {
            b.in_code
                .cmp(&a.in_code)
                .then_with(|| a.path.cmp(&b.path))
                .then_with(|| a.start.cmp(&b.start))
        });
        Ok(found)
    }

    /// Whether calls the resolver could not bind touch *this* answer.
    ///
    /// [`attribution_coverage_gap`] is a fact about the repository — "212,976
    /// of 571,999 sites have no indexed target" — and it is non-zero on every
    /// real one, so stapled to every walk it said the same sentence on every
    /// answer and told a reader nothing about the one in front of them
    /// (measured on ScholarLM, 2026-10-07: every `impact` and `affected` call
    /// carried it). The question a reader of one walk has is narrower and
    /// answerable from the ledger: does an unattributed site name a symbol this
    /// walk reached (a caller it could not follow), or sit inside one (a callee
    /// it could not follow)? Only the sites that may hide a repository edge
    /// count — [`devmap_resolve::UNATTRIBUTED_LABELS`]; a builtin or an external
    /// import cannot be a missing edge to a symbol here, and neither can a
    /// receiver call whose method name no indexed symbol carries (`rows.length`,
    /// `mu.Unlock()`: 126,023 of ScholarLM's 200,790 untyped-receiver rows).
    ///
    /// Falls back to the repository-wide sentence wherever the specific check
    /// cannot run — no generation on the index, a classification breakdown
    /// that is missing or inconsistent — because a check that could not run
    /// must not answer like one that found nothing. `None` only when the check
    /// ran and found no such site, or the corpus has none at all.
    pub(super) fn radius_attribution_gap(
        &self,
        generation: Option<u32>,
        analysis: Option<&devmap_analyze::model::AnalysisDisclosure>,
        reached: &BTreeSet<(String, String)>,
        side: RadiusSide,
    ) -> anyhow::Result<Option<String>> {
        // An unreadable summary is already disclosed by `analysis_status_gap`.
        let Some(analysis) = analysis else {
            return Ok(None);
        };
        let Some(repository_wide) =
            attribution_coverage_gap(analysis.unresolved_calls, analysis.resolution_rate.as_ref())
        else {
            return Ok(None);
        };
        let breakdown_usable = matches!(
            (analysis.unresolved_calls, analysis.resolution_rate.as_ref()),
            (Some(total), Some(rate))
                if rate.unresolved_sites == total && rate.explained_sites <= total
        );
        let Some(generation) = generation.filter(|_| breakdown_usable) else {
            return Ok(Some(repository_wide));
        };
        let (keys, keys_not_checked) = radius_keys(reached, side);
        if keys.is_empty() {
            return Ok(None);
        }
        let lookup: Vec<String> = keys.iter().map(|(key, _)| key.clone()).collect();
        self.cancel.check()?;
        let read = match side {
            RadiusSide::Callers => {
                self.store
                    .unattributed_sites_naming(generation, &lookup, RADIUS_SITES_PER_KEY)?
            }
            RadiusSide::Callees => {
                self.store
                    .unattributed_sites_within(generation, &lookup, RADIUS_SITES_PER_KEY)?
            }
            RadiusSide::FileCallees => {
                self.store
                    .unattributed_sites_in_files(generation, &lookup, RADIUS_SITES_PER_KEY)?
            }
        };
        let Some(found) = read else {
            return Ok(Some(format!(
                "the unresolved ledger could not be read at generation {generation}, the one this \
                 answer came from (it was pruned mid-query); whether calls the resolver could not \
                 bind touch this answer is unknown — ask again"
            )));
        };
        let found = found
            .into_iter()
            .map(|(key, (rows, truncated))| {
                let rows = rows
                    .into_iter()
                    .map(|row| RadiusSite {
                        source_file: row.source_file,
                        callee_name: row.callee_name,
                        receiver: row.receiver,
                    })
                    .collect();
                (key, (rows, truncated))
            })
            .collect();
        Ok(radius_note(side, &keys, keys_not_checked, found))
    }
}

/// One unattributed site, as either engine reads it.
pub(super) struct RadiusSite {
    source_file: String,
    callee_name: String,
    receiver: Option<String>,
}

/// The ledger keys a walk's reached set asks about — bare names for
/// [`RadiusSide::Callers`], walked symbols for `Callees`, files for
/// `FileCallees` — each with the families of the nodes that produced it, at
/// most [`MAX_RADIUS_LEDGER_KEYS`] of them, and how many were left unchecked.
pub(super) fn radius_keys(
    reached: &BTreeSet<(String, String)>,
    side: RadiusSide,
) -> (Vec<(String, BTreeSet<LangFamily>)>, usize) {
    let mut keys: BTreeMap<String, BTreeSet<LangFamily>> = BTreeMap::new();
    for (node, file) in reached {
        let key = match side {
            RadiusSide::Callers => {
                // A file node is not called by name.
                if !node.contains("::") {
                    continue;
                }
                match bare_callee_name(node) {
                    Some(name) => name.to_string(),
                    None => continue,
                }
            }
            RadiusSide::Callees => node.clone(),
            RadiusSide::FileCallees => file.clone(),
        };
        keys.entry(key).or_default().insert(family_of_path(file));
    }
    let not_checked = keys.len().saturating_sub(MAX_RADIUS_LEDGER_KEYS);
    (
        keys.into_iter().take(MAX_RADIUS_LEDGER_KEYS).collect(),
        not_checked,
    )
}

/// The note for one radius check, from the sites read per key (at most
/// [`RADIUS_SITES_PER_KEY`], with whether the key had more). `None` when no
/// site touches the answer and every key was checked.
///
/// Shared by both engines so the in-memory one cannot phrase or count the same
/// ledger differently. The sample is the sorted first few, so it does not
/// depend on the order a reader met the sites in.
pub(super) fn radius_note(
    side: RadiusSide,
    keys: &[(String, BTreeSet<LangFamily>)],
    keys_not_checked: usize,
    mut found: BTreeMap<String, (Vec<RadiusSite>, bool)>,
) -> Option<String> {
    let mut sites = 0usize;
    let mut more = false;
    let mut labels: BTreeSet<String> = BTreeSet::new();
    // Sites written as a qualified path (`gitpulse_lib::hooks::dispatch(..)`).
    // A path names a crate or module, never "an untyped receiver", so the
    // reason differs: the resolver has no indexed root for what the path starts
    // with.
    let mut path_sites = 0usize;
    let mut path_labels: BTreeSet<String> = BTreeSet::new();
    for (key, families) in keys {
        let (rows, truncated) = found.remove(key).unwrap_or_default();
        more |= truncated;
        for row in rows {
            // A Python call cannot be a missed caller of a Go function.
            if side == RadiusSide::Callers {
                let family = family_of_path(&row.source_file);
                if !families.iter().any(|reached| family.admits(*reached)) {
                    continue;
                }
            }
            if let Some(path) = row.receiver.as_deref().filter(|r| r.contains("::")) {
                path_sites += 1;
                path_labels.insert(format!("{path}::{}", row.callee_name));
                continue;
            }
            sites += 1;
            labels.insert(match &row.receiver {
                Some(receiver) => format!("{receiver}.{}", row.callee_name),
                None => row.callee_name,
            });
        }
    }
    let mut notes = Vec::new();
    if sites > 0 {
        let count = if more {
            format!("at least {sites}")
        } else {
            sites.to_string()
        };
        let sample = labels
            .into_iter()
            .take(RADIUS_NOTE_SAMPLE)
            .collect::<Vec<_>>()
            .join(", ");
        notes.push(match side {
            RadiusSide::Callers => format!(
                "{count} call site(s) the resolver could not bind name a symbol this answer \
                 reached ({sample}) — an untyped receiver, a local binding — so callers through \
                 them are not in it"
            ),
            RadiusSide::Callees | RadiusSide::FileCallees => format!(
                "{count} call site(s) inside what this answer walked could not be bound \
                 ({sample}), so what they call is not in it"
            ),
        });
    }
    if path_sites > 0 {
        let count = if more {
            format!("at least {path_sites}")
        } else {
            path_sites.to_string()
        };
        let sample = path_labels
            .into_iter()
            .take(RADIUS_NOTE_SAMPLE)
            .collect::<Vec<_>>()
            .join(", ");
        notes.push(match side {
            RadiusSide::Callers => format!(
                "{count} call site(s) written as a qualified path name a symbol this answer \
                 reached ({sample}) — the crate or module the path starts with is not mapped \
                 to an indexed root, which is not an untyped receiver — so callers through \
                 them are not in it"
            ),
            RadiusSide::Callees | RadiusSide::FileCallees => format!(
                "{count} call site(s) written as a qualified path inside what this answer \
                 walked could not be bound ({sample}) — the crate or module the path starts \
                 with is not mapped to an indexed root — so what they call is not in it"
            ),
        });
    }
    // A capped check must not read as a clean one.
    if keys_not_checked > 0 {
        notes.push(format!(
            "only {} of {} reached key(s) were checked against the unresolved ledger",
            keys.len(),
            keys.len() + keys_not_checked
        ));
    }
    (!notes.is_empty()).then(|| notes.join("; "))
}

impl<'a> QueryEngine<'a> {
    /// [`StoreQueryEngine::radius_attribution_gap`] over the in-memory
    /// resolution: the same keys, the same per-key cap in the same
    /// `(file, symbol)` order the ledger read uses, and the same note.
    pub(super) fn radius_attribution_gap(
        &self,
        reached: &BTreeSet<(String, String)>,
        side: RadiusSide,
    ) -> Option<String> {
        self.attribution_gap.as_ref()?;
        let (keys, keys_not_checked) = radius_keys(reached, side);
        if keys.is_empty() {
            return None;
        }
        let mut matched: BTreeMap<&str, Vec<&UnresolvedReference>> = BTreeMap::new();
        let wanted: BTreeSet<&str> = keys.iter().map(|(key, _)| key.as_str()).collect();
        // The store's filter (`Store::unattributed_sites_naming`): a receiver
        // call no symbol is named for cannot hide an edge into this index.
        let symbol_names: std::collections::HashSet<&str> = self
            .extractions
            .iter()
            .flat_map(|extraction| extraction.symbols.iter())
            .map(|symbol| symbol.name.as_str())
            .collect();
        for row in &self.resolution.unresolved {
            if row.class.is_explained()
                || (row.receiver.is_some() && !symbol_names.contains(row.callee_name.as_str()))
            {
                continue;
            }
            let key = match side {
                RadiusSide::Callers => row.callee_name.as_str(),
                RadiusSide::Callees => row.source_symbol.as_str(),
                RadiusSide::FileCallees => row.source_file.as_str(),
            };
            if let Some(key) = wanted.get(key) {
                matched.entry(key).or_default().push(row);
            }
        }
        let found = matched
            .into_iter()
            .map(|(key, mut rows)| {
                // The ledger's order, so the per-key cap keeps the same rows.
                rows.sort_by(|a, b| {
                    a.source_file
                        .cmp(&b.source_file)
                        .then_with(|| a.source_symbol.cmp(&b.source_symbol))
                });
                let truncated = rows.len() > RADIUS_SITES_PER_KEY;
                let rows = rows
                    .into_iter()
                    .take(RADIUS_SITES_PER_KEY)
                    .map(|row| RadiusSite {
                        source_file: row.source_file.clone(),
                        callee_name: row.callee_name.clone(),
                        receiver: row.receiver.clone(),
                    })
                    .collect();
                (key.to_string(), (rows, truncated))
            })
            .collect();
        radius_note(side, &keys, keys_not_checked, found)
    }
}

/// The corpus half of [`analysis_coverage_gap`], on its own.
///
/// Split out because the two halves answer different questions and not every
/// surface is entitled to both. `unresolved_calls` is about *edges* — how much
/// of the call graph was attributed — and it is non-zero on essentially every
/// real repository. A surface that does not answer from the call graph must not
/// carry it, or the marker rides on every answer and tells a reader nothing,
/// which is the failure mode [`analysis_coverage_gap`] documents.
///
/// What this half says is about the *corpus*: whether the generation is a
/// complete read of the repository at all. `AnalysisStatus::Partial` is where
/// `ExtractionCoverage::degraded_reason` lands, so the counts it carries —
/// files that failed to parse, were recovered by pattern, or were refused by
/// discovery — come through verbatim from their one owner.
pub(super) fn analysis_status_gap(
    analysis: Option<&devmap_analyze::model::AnalysisDisclosure>,
) -> Option<String> {
    use devmap_analyze::model::AnalysisStatus;
    // A generation exists but its analysis blob does not read back. That is a
    // check that could not run, and it must not answer like one that ran.
    let Some(analysis) = analysis else {
        return Some(
            "the analysis summary for this generation could not be read, so the coverage \
             behind this answer is unknown"
                .to_string(),
        );
    };
    match &analysis.status {
        AnalysisStatus::Ok => None,
        AnalysisStatus::Partial { reason } => Some(format!("the analysis is partial: {reason}")),
        AnalysisStatus::Timeout { reason } => Some(format!("the analysis timed out: {reason}")),
    }
}

/// What a corpus-level gap means for a *search*, or `None` when there is none.
///
/// `search` answers "does a symbol by this name exist here", and a miss is the
/// answer callers act on hardest: `total: 0, truncated: false, resolution:
/// Available, walk_incomplete: None` reads as *"there are zero matches in this
/// corpus"* stated as a completed check. Every symbol of a file that was
/// refused — a parse over its budget, a grammar that would not load, a NUL byte
/// caught at the boundary — is absent from the index, so a name that lives only
/// there answered identically to a name that exists nowhere. A reader deciding
/// "this symbol does not exist, so I may take the name" and one deciding "this
/// symbol may exist in a file nothing read" were given the same sentence.
///
/// One owner for both engines and one wording, fed from the two places the same
/// fact lives: the persisted disclosure for [`StoreQueryEngine`], and
/// [`devmap_analyze::extraction_coverage`] over the slice for [`QueryEngine`].
/// `None` in, `None` out is the load-bearing case — a fully read corpus must
/// keep answering without a caveat.
pub(super) fn search_coverage_gap(corpus_gap: Option<String>) -> Option<String> {
    corpus_gap.map(|gap| {
        format!(
            "the corpus behind this answer is not a complete read of the repository, so a \
             name that matches nothing here may still be declared in a file that was never \
             indexed: {gap}"
        )
    })
}

/// What an empty search result does *not* mean.
///
/// `total: 0, hidden: 0, truncated: false` on a fresh, undegraded index is the
/// most confident shape this API can produce, and for a keyword search it is
/// also the least informative: it is returned both when the repository has no
/// such symbol and when the caller asked a question this index does not answer.
///
/// Measured, and the reason this exists: an agent asked "where is the optimizer
/// constructed in this repository?", DevMap returned zero items, `devmap_status`
/// reported `is_fresh: true` with no coverage gaps and nothing quarantined, and
/// ripgrep then found **eight** construction sites. None of them is a symbol —
/// `self.optimizer = torch.optim.AdamW(...)` is an attribute assignment to an
/// externally-owned class — so the store was not stale and not degraded. It
/// simply does not index that shape, and it had no way to say so.
///
/// The comment on `search` already named this as the qualification "that
/// decides whether `total: 0` may be read as 'no such symbol'". It only ever
/// fired on a *partial* analysis, so on a complete one — the overwhelmingly
/// common case — zero carried no qualification at all.
///
/// **Only for a multi-term query**, and that restriction is the whole design.
///
/// The first version of this fired on every miss, and
/// `search_over_a_complete_corpus_claims_nothing` rejected it — correctly. A
/// one-word miss over a fully read corpus is a *completed check*: "no symbol is
/// named `no_such_symbol_anywhere`" is a whole, correct answer, and hanging a
/// caveat on it claims an incompleteness that does not exist. Worse, it would
/// fire on the overwhelmingly common case, which is how a qualification becomes
/// noise a caller learns to skip — and `walk_incomplete` is the field that has
/// to be believed when a corpus really does have holes in it.
///
/// A multi-term query is different, and not because the conjunction is
/// incomplete — it ran fully. It is different because the caller has almost
/// certainly *described* a symbol rather than named one, so the check that ran
/// is not the check they think they asked for. That is the same kind of note
/// `ranking_coverage_gap` carries: not "the corpus had holes" but "this answer
/// means less than its shape suggests". The audit's query,
/// `optimizer AdamW step`, is exactly that shape.
pub(super) fn empty_result_gap(total: u32, query: &str) -> Option<String> {
    let terms = query.split_whitespace().count();
    (total == 0 && terms > 1).then(|| {
        format!(
            "no symbol matched all {terms} terms — every term must appear in one symbol's \
             name, qualified name or path, so a phrase describing a symbol will not match it; \
             {SEARCH_SCOPE_NOTE}"
        )
    })
}

/// The boundary both search surfaces share, written once.
///
/// Keyword search and semantic search reach zero by different mechanisms — a
/// conjunction that filtered everything out, or a score that never rose above
/// nothing — but the thing a caller most needs to know about either zero is the
/// same, and it is not about the mechanism: **no file body was read.** Two
/// copies of this sentence would let one surface be fixed and the other left to
/// answer in the old shape, which is the failure this whole pass is about.
pub(super) const SEARCH_SCOPE_NOTE: &str =
    "this search matches symbol names, qualified names and file paths, and does not read file \
     contents — so \"where is X constructed\", \"where is X assigned\" and any other question \
     about what a body contains is outside what it can answer, and zero here is not evidence \
     of absence";

/// [`empty_result_gap`] for the semantic surface.
///
/// Separate because the conjunction sentence would be a lie here: semantic
/// search scores term overlap and has no all-terms-must-match rule to explain.
/// The scope sentence is shared, and is the half that matters.
///
/// Gated on the same multi-term condition, for the same reason: a one-word miss
/// is a completed check on either surface, and the two must not disagree about
/// when an empty answer is worth qualifying.
pub(super) fn empty_semantic_gap(total: u32, query: &str) -> Option<String> {
    (total == 0 && query.split_whitespace().count() > 1).then(|| {
        format!("no symbol name or qualified name scored against this query; {SEARCH_SCOPE_NOTE}")
    })
}

/// Empty-result disclosure for [`StoreQueryEngine::ask`].
///
/// Ask seeds on names and docstrings, then re-ranks over call edges. A zero
/// here means no shared terms — not that the behaviour is absent from bodies,
/// and not that the graph was walked and found nothing.
pub(super) fn empty_ask_gap(query: &str) -> Option<String> {
    (query.split_whitespace().count() > 1).then(|| {
        "no name or docstring shared a term with this query; ask seeds on names and \
         docstrings then re-ranks over call edges, and does not read file bodies — \
         zero here is not evidence of absence"
            .to_string()
    })
}

pub(super) fn ranking_coverage_gap(total: u32, pool: usize) -> Option<String> {
    (total as usize > pool).then(|| format!(
        "ranked the first {pool} of {total} matches, in the store's relevance order; a closer match may sit outside that page"
    ))
}

pub(super) fn rank_symbol_rows(
    rows: Vec<StoredSymbol>,
    query_lower: &str,
    cancel: &Cancel,
) -> anyhow::Result<Vec<(f32, StoredSymbol)>> {
    let mut ranked = Vec::with_capacity(rows.len());
    for (index, row) in rows.into_iter().enumerate() {
        cancel.check_every(index)?;
        ranked.push((name_match_score(&row, query_lower), row));
    }
    ranked.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .total_cmp(left_score)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.span_start.cmp(&right.span_start))
            .then_with(|| left.span_end.cmp(&right.span_end))
    });
    Ok(ranked)
}

/// Rank of one stored symbol against an already-lowercased query.
///
/// The single owner of the ordering, for `search` and for `explore` alike: two
/// copies would let the same query return a different "best match" depending on
/// which command asked. It runs on the stored row rather than on a built
/// [`SymbolHit`] precisely so that ranking can happen before the file reads do
/// — which is what lets the candidate pool be wider than the answer without
/// costing the caller anything.
pub(super) fn name_match_score(row: &devmap_store::StoredSymbol, query_lower: &str) -> f32 {
    let name = row.name.to_lowercase();
    if name == query_lower || row.qualified_name.to_lowercase() == query_lower {
        1.0
    } else if name.starts_with(query_lower) {
        0.95
    } else {
        0.8
    }
}

#[cfg(test)]
mod radius_note_tests {
    use super::*;

    fn site(receiver: Option<&str>) -> RadiusSite {
        RadiusSite {
            source_file: "src/bin/tool.rs".into(),
            callee_name: "dispatch".into(),
            receiver: receiver.map(str::to_string),
        }
    }

    fn note_for(receiver: Option<&str>) -> String {
        let keys = vec![("dispatch".to_string(), BTreeSet::from([LangFamily::Rust]))];
        let found = BTreeMap::from([("dispatch".to_string(), (vec![site(receiver)], false))]);
        radius_note(RadiusSide::Callers, &keys, 0, found).expect("a site touches the answer")
    }

    #[test]
    fn a_qualified_path_is_not_called_an_untyped_receiver() {
        let note = note_for(Some("gitpulse_lib::hooks"));
        assert!(note.contains("not mapped to an indexed root"), "{note}");
        assert!(note.contains("gitpulse_lib::hooks::dispatch"), "{note}");
        assert!(
            !note.contains("an untyped receiver, a local binding"),
            "{note}"
        );
    }

    #[test]
    fn a_value_receiver_keeps_the_untyped_receiver_reason() {
        let note = note_for(Some("state"));
        assert!(
            note.contains("an untyped receiver, a local binding"),
            "{note}"
        );
        assert!(!note.contains("indexed root"), "{note}");
    }
}

#[cfg(test)]
mod charged_range_tests {
    use super::*;
    use devmap_extract::model::{ParseOutcome, TextRange};

    fn producer_reason(regions: &[(usize, usize)]) -> String {
        let mut ext = devmap_extract::extract_file("src/a.py", "def x(): pass\n");
        ext.parse_outcome = ParseOutcome::Partial {
            error_ranges: regions
                .iter()
                .map(|&(start_byte, end_byte)| TextRange {
                    start_byte,
                    end_byte,
                })
                .collect(),
            nameless_ranges: Vec::new(),
        };
        devmap_analyze::extraction_gaps(&[ext])
            .into_iter()
            .find(|entry| entry.gap == devmap_analyze::ExtractionGap::PartialParse)
            .expect("a partial file is listed")
            .reason
    }

    /// The parser reads the producer's own sentence, so a rewording of it
    /// fails here instead of silently emptying every charged-hole answer.
    #[test]
    fn the_charged_clause_round_trips_through_the_producer() {
        // `def x(): pass` is 0..13: 4..8 is inside it (listed separately), the rest is charged.
        let reason = producer_reason(&[(4, 8), (20, 30), (40, 55)]);
        let (ranges, elided) = parse_charged_ranges(&reason);
        assert_eq!(
            (merge_ranges(ranges), elided),
            (vec![(4, 8), (20, 30), (40, 55)], 0)
        );
    }

    #[test]
    fn an_elided_tail_is_counted_not_dropped() {
        let regions: Vec<(usize, usize)> = (0..11).map(|n| (20 + n * 10, 25 + n * 10)).collect();
        let (listed, elided) = parse_charged_ranges(&producer_reason(&regions));
        assert_eq!((listed.len(), elided), (8, 3));
    }

    #[test]
    fn a_region_inside_a_surviving_declaration_can_still_hold_a_lost_method() {
        // `4..8 in `x``: exempt from the charge, but a lost method of a
        // surviving class is a declaration the index lacks.
        assert_eq!(
            parse_charged_ranges(&producer_reason(&[(4, 8)])),
            (vec![(4, 8)], 0)
        );
    }

    #[test]
    fn nameless_and_zero_width_regions_name_nothing_to_blame() {
        let mut ext = devmap_extract::extract_file("src/a.py", "def x(): pass\n");
        ext.parse_outcome = ParseOutcome::Partial {
            error_ranges: vec![
                TextRange {
                    start_byte: 20,
                    end_byte: 21,
                },
                TextRange {
                    start_byte: 11,
                    end_byte: 11,
                },
            ],
            nameless_ranges: vec![TextRange {
                start_byte: 20,
                end_byte: 21,
            }],
        };
        let reason = devmap_analyze::extraction_gaps(&[ext])
            .into_iter()
            .find(|entry| entry.gap == devmap_analyze::ExtractionGap::PartialParse)
            .expect("listed")
            .reason;
        assert_eq!(parse_charged_ranges(&reason), (Vec::new(), 0), "{reason}");
    }

    #[test]
    fn a_name_matches_whole_identifiers_only() {
        assert_eq!(
            name_occurrence("let FOO_BAR = 1;", "FOO_BAR", false),
            Some(true)
        );
        assert_eq!(name_occurrence("let FOO_BARS = 1;", "FOO_BAR", false), None);
        assert_eq!(name_occurrence("let XFOO_BAR = 1;", "FOO_BAR", false), None);
        assert_eq!(name_occurrence("// FOO_BAR", "FOO_BAR", false), Some(false));
        assert_eq!(
            name_occurrence("let s = \"FOO_BAR\";", "FOO_BAR", false),
            Some(false)
        );
        assert_eq!(
            name_occurrence("// FOO_BAR\nconst FOO_BAR: u8 = 1;", "FOO_BAR", false),
            Some(true),
            "one code occurrence outweighs a comment one"
        );
        assert_eq!(
            name_occurrence("const ÜBER: u8 = 1;", "ÜBER", false),
            Some(true)
        );
    }

    #[test]
    fn case_is_exact_unless_the_language_folds_it() {
        assert_eq!(name_occurrence("const FOO: u8 = 1;", "foo", false), None);
        assert_eq!(
            name_occurrence("const FOO: u8 = 1;", "foo", true),
            Some(true)
        );
        assert_eq!(
            name_occurrence("const ÜBER: u8 = 1;", "über", true),
            Some(true)
        );
        for folded in ["sql", "cobol"] {
            assert!(language_folds_case(folded));
        }
        for exact in ["rust", "go", "python", "typescript", "swift", "kotlin"] {
            assert!(!language_folds_case(exact), "{exact}");
        }
    }

    #[test]
    fn only_a_qualified_identifier_is_a_name() {
        assert!(query_target("a b").is_err());
        assert!(query_target("src/a.rs").is_err());
        assert_eq!(query_target("mod::Thing"), Ok((vec!["mod"], "Thing")));
        assert_eq!(query_target("Class.method"), Ok((vec!["Class"], "method")));
        assert_eq!(query_target("  plain "), Ok((vec![], "plain")));
        assert!(query_target("release.0").is_err());
        assert!(query_target("a..b").is_err());
        assert!(query_target("a::").is_err());
        assert_eq!(query_target("rs"), Ok((vec![], "rs")));
        // An extension-shaped tail is a qualified name (method `go` on `foo`).
        assert_eq!(query_target("foo.go"), Ok((vec!["foo"], "go")));
        assert_eq!(query_target("Config.ts"), Ok((vec!["Config"], "ts")));
    }

    #[test]
    fn a_qualifier_must_belong_to_the_file() {
        assert!(qualifier_present("foo", "type foo struct{}", "m.go", false));
        assert!(!qualifier_present(
            "nosuchfile",
            "use std::rs::x;",
            "consts.rs",
            false
        ));
        assert!(qualifier_present(
            "mod",
            "def f(): pass",
            "pkg/mod.py",
            false
        ));
        assert!(qualifier_present("MOD", "x", "pkg/mod.pas", true));
    }

    #[test]
    fn php_folds_case_and_the_case_sensitive_grammars_do_not() {
        assert!(language_folds_case("php"));
        for exact in [
            "csharp", "java", "ruby", "lua", "erlang", "r", "scala", "dart",
        ] {
            assert!(!language_folds_case(exact), "{exact}");
        }
    }

    #[test]
    fn touching_and_overlapping_ranges_are_one_region() {
        assert_eq!(
            merge_ranges(vec![(10, 20), (0, 5), (5, 8), (15, 30), (40, 41)]),
            vec![(0, 8), (10, 30), (40, 41)]
        );
    }
}
