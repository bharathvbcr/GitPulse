use super::{bare_callee_name, family_of_path, QueryEngine, StoreQueryEngine};
use crate::cancel::Cancel;
use crate::model::Response;
use devmap_extract::model::ParseOutcome;
use devmap_resolve::model::{LangFamily, UnresolvedReference};
use devmap_store::StoredSymbol;
use std::collections::{BTreeMap, BTreeSet};

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
        ParseOutcome::Partial { error_ranges } => Some(format!(
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
}

impl CallBlindStarts {
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

impl StoreQueryEngine<'_> {
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
