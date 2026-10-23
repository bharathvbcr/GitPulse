//! How much of what the resolver tried to attribute, it attributed.
//!
//! Every number this needs was already being counted and nothing divided. The
//! CLI tallied six unresolved classes and printed them beside an edge total;
//! there was no `resolution_rate` anywhere in the kernel or the Python surface.
//! Raw counts leave the arithmetic to a reader who will not do it, and the
//! quantity that matters — "of the sites we tried to bind, how many did we" —
//! was therefore never observed by anything.
//!
//! Two consequences follow from publishing it:
//!
//! * A language with no call extractor sits at zero, visibly. W0.2's entire bug
//!   class — CFML and Terraform reporting full coverage while contributing no
//!   call edges — would have surfaced here without anyone going looking.
//! * A grammar bump that quietly stops matching a node kind tanks one
//!   language's rate while passing every identity fixture, which is the
//!   regression golden files structurally cannot catch.

use devmap_extract::model::{EdgeKind, Extraction};
use devmap_resolve::model::{Resolution, ResolutionResult, ResolvedEdge, UnresolvedReference};
use serde::{Deserialize, Serialize};
use devmap_extract::wiring::is_vendored_path;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Permille rather than a float, and `Option` rather than a sentinel.
///
/// Integer permille because this number is compared for equality by a CI fence
/// and a ratchet baseline, and float comparison across a serialization round
/// trip is how a fence starts firing on noise — the same reason
/// `EXTRACTED_FLOOR_MILLIS` exists.
///
/// `None` means **no site was attempted**, which is not the same as every site
/// failing. A corpus of Markdown attempts nothing; reporting it at 0 would say
/// the resolver failed on everything it tried, and reporting it at 1000 would
/// say it succeeded at everything. Both are claims about work that never
/// happened.
pub type Permille = Option<u32>;

fn permille(numerator: usize, denominator: usize) -> Permille {
    if denominator == 0 {
        return None;
    }
    // Rounds toward zero. A rate one site short of perfect must not render as
    // 1000, or the fence cannot tell "resolved everything" from "resolved all
    // but one of forty thousand".
    Some(((numerator as u128 * 1000) / denominator as u128) as u32)
}

/// One language's share of the attribution work.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageResolution {
    /// Whether this build has a call extractor for the language at all.
    ///
    /// Carried on the row because without it a `None` rate is unreadable: HCL
    /// and a language that simply happens to make no calls both render as "not
    /// measured", and only one of them is a hole in the kernel. This is the
    /// field that turns the breakdown into the readout W0.2's bug class would
    /// have surfaced in.
    #[serde(default)]
    pub extracts_calls: bool,
    /// The capabilities this build does **not** have for the language, by name.
    ///
    /// `Capability::Heritage` and `Capability::References` had no production
    /// reader at all: two of the registry's four bits were declared, maintained
    /// and bidirectionally tested, and consulted by nobody — which is half the
    /// registry sitting in the state the constant it replaced was condemned for,
    /// minus the wrongness. This is the reader, and it is the same sentence
    /// `extracts_calls` already makes: an empty answer from a blind extractor
    /// and an empty answer from a language that simply has none of the thing
    /// must not render alike. A Pascal corpus with no `Extends` edges is not a
    /// Pascal corpus with no inheritance.
    ///
    /// Both projections come from one `Capabilities` value, so `extracts_calls`
    /// cannot disagree with the absence of `"calls"` here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blind_to: Vec<String>,
    /// Attribution sites that produced at least one edge.
    pub resolved_sites: usize,
    /// Sites the ladder gave up on, all six classes.
    pub unresolved_sites: usize,
    /// Of those, the ones that are explained rather than failed: `Builtin`,
    /// `HostGlobal`, `External`. See [`ResolutionRate::net_permille`].
    pub explained_sites: usize,
    /// `None` when `extracts_calls` is false: a language with no call extractor
    /// attributed no call, so a figure here would score missing work as perfect.
    pub gross_permille: Permille,
    /// `None` when `extracts_calls` is false, for the same reason.
    pub net_permille: Permille,
}

/// The corpus rate, and the same figure per language.
///
/// The per-language breakdown is the half that is actionable: a corpus figure
/// moving from 0.81 to 0.79 tells a reader nothing about where to look, and
/// "Kotlin fell from 0.74 to 0.02" names the grammar that broke.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionRate {
    pub resolved_sites: usize,
    pub unresolved_sites: usize,
    pub explained_sites: usize,
    /// `resolved / (resolved + every unresolved site)`.
    ///
    /// The pessimistic figure. It charges `print` and `setTimeout` as misses,
    /// which they are not, so it is dominated by how much of the language's
    /// standard library a corpus happens to use.
    pub gross_permille: Permille,
    /// `resolved / (resolved + unresolved - explained)`.
    ///
    /// The figure worth ratcheting. `Builtin`, `HostGlobal`, `External`,
    /// `NoNamesake` and `ModulePath` are misses with affirmative evidence
    /// behind them — the language declares the name, the runtime does, an
    /// import proves it comes from outside the corpus, the corpus has no
    /// symbol of that name, or the receiver is a module path rather than a
    /// value — and no amount of extractor work will ever bind them.
    ///
    /// `LocalBinding` is deliberately **kept in the denominator** even though
    /// it is also explained. A call to a value the enclosing function declares
    /// is something the kernel could in principle bind, to that declaration;
    /// excluding it would flatter a number whose whole purpose is to be
    /// ratcheted upward.
    ///
    /// Both corpus permille figures cover **measured** work only: languages
    /// with a call extractor, outside vendored paths. The site counts above
    /// stay raw totals so `unresolved_sites` keeps equalling
    /// `unresolved_calls`, which the attribution-coverage disclosure checks.
    pub net_permille: Permille,
    pub by_language: BTreeMap<String, LanguageResolution>,
    /// Attribution sites (resolved plus unresolved) in vendored files, per
    /// `devmap_extract::wiring::is_vendored_path`. Counted in the totals,
    /// absent from `by_language` and from both permille figures: a vendored
    /// file is not this repository's code.
    #[serde(default, deserialize_with = "null_as_default")]
    pub vendored_sites: usize,
    /// Grammar-read files under vendored paths.
    #[serde(default, deserialize_with = "null_as_default")]
    pub vendored_files: usize,
    /// Attribution sites (resolved plus unresolved) in languages with no call
    /// extractor. Counted in the totals, excluded from both permille figures.
    #[serde(default, deserialize_with = "null_as_default")]
    pub call_blind_sites: usize,
    /// The headline's own counts: the sums over measured languages that
    /// `net_permille` and `gross_permille` are computed from, so a reader can
    /// reproduce them. `net = measured_resolved / (measured_resolved +
    /// measured_unresolved - measured_explained)`. Absent or null reads as 0
    /// on a row stored before they existed.
    #[serde(default, deserialize_with = "null_as_default")]
    pub measured_resolved_sites: usize,
    #[serde(default, deserialize_with = "null_as_default")]
    pub measured_unresolved_sites: usize,
    #[serde(default, deserialize_with = "null_as_default")]
    pub measured_explained_sites: usize,
}

/// An explicit `null` reads as the type's default, the same as an absent field.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// Structural edges are not attributions.
///
/// `Contains`, `Defines` and `MemberOf` record where a symbol *lives*. No
/// resolution ladder ran for them, so counting them as resolved would mean a
/// repository's rate rose with its number of declarations. Liveness excludes
/// exactly this set for exactly this reason.
fn is_attribution(kind: EdgeKind) -> bool {
    !matches!(
        kind,
        EdgeKind::Contains | EdgeKind::Defines | EdgeKind::MemberOf
    )
}

/// Count *sites*, not edges.
///
/// One ambiguous call with N candidates fans out into N edges that share a
/// single `Arc<Resolution>` — the crate says so where it declares the field.
/// Counting those N as N resolutions would make a repository look better
/// resolved the more ambiguity it has, which inverts the number's meaning: an
/// ambiguity is a partial failure, not N successes. Deduplicating on the shared
/// allocation's address is exact for this, because the fan-out is the only
/// thing that shares one.
///
/// A non-ambiguous edge is its own site and is counted directly.
fn resolved_sites_by_language<'a>(
    edges: &'a [ResolvedEdge],
    language_of: &HashMap<&str, &'a str>,
) -> (usize, BTreeMap<String, usize>, usize) {
    let mut total = 0usize;
    let mut vendored = 0usize;
    let mut by_language: BTreeMap<String, usize> = BTreeMap::new();
    let mut seen_fanouts: HashSet<*const Resolution> = HashSet::new();

    for edge in edges {
        if !is_attribution(edge.edge_kind) {
            continue;
        }
        if let Some(resolution) = &edge.resolution {
            if matches!(**resolution, Resolution::Unresolved { .. }) {
                // Recorded as an edge but not an attribution: the ladder ran
                // and bound nothing. The matching row is in `unresolved`, so
                // counting it here as well would double it.
                continue;
            }
            if matches!(
                **resolution,
                Resolution::AmbiguousGlobal { .. }
                    | Resolution::LanguageServerDispatch { .. }
                    | Resolution::Dispatch { .. }
            ) && !seen_fanouts.insert(std::sync::Arc::as_ptr(resolution))
            {
                continue;
            }
        }
        total += 1;
        if is_vendored_path(&edge.source_file) {
            vendored += 1;
            continue;
        }
        let language = language_of
            .get(edge.source_file.as_str())
            .copied()
            .unwrap_or("unknown");
        *by_language.entry(language.to_string()).or_default() += 1;
    }
    (total, by_language, vendored)
}

/// Whether this miss has affirmative evidence that no edge was ever findable.
fn is_explained(row: &UnresolvedReference) -> bool {
    row.class.is_explained()
}

/// Whether a grammar (or notebook kernel) read the file, as opposed to prose
/// and data formats that attempt no attribution.
fn is_grammar_read(ext: &Extraction) -> bool {
    matches!(
        ext.engine,
        devmap_extract::model::ExtractionEngine::TreeSitter { .. }
            | devmap_extract::model::ExtractionEngine::Notebook { .. }
    )
}

/// The key `ext` is reported under. See [`devmap_extract::languages::report_language`].
fn reported(ext: &Extraction) -> &str {
    devmap_extract::languages::report_language(&ext.file_path, &ext.language)
}

/// Compute the rate over one generation's resolution.
pub fn resolution_rate(
    extractions: &[Extraction],
    resolution: &ResolutionResult,
) -> ResolutionRate {
    // Keyed by the language a file is written in, not the grammar that read
    // it: Metal is parsed by the C++ grammar and reported as Metal.
    let language_of: HashMap<&str, &str> = extractions
        .iter()
        .map(|ext| (ext.file_path.as_str(), reported(ext)))
        .collect();

    let (resolved_sites, resolved_by_language, vendored_resolved) =
        resolved_sites_by_language(&resolution.edges, &language_of);

    let mut unresolved_sites = 0usize;
    let mut explained_sites = 0usize;
    let mut vendored_unresolved = 0usize;
    let mut unresolved_by_language: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for row in &resolution.unresolved {
        unresolved_sites += 1;
        let explained = is_explained(row);
        if explained {
            explained_sites += 1;
        }
        if is_vendored_path(&row.source_file) {
            vendored_unresolved += 1;
            continue;
        }
        let language = language_of
            .get(row.source_file.as_str())
            .copied()
            .unwrap_or("unknown");
        let entry = unresolved_by_language
            .entry(language.to_string())
            .or_default();
        entry.0 += 1;
        if explained {
            entry.1 += 1;
        }
    }

    let mut by_language: BTreeMap<String, LanguageResolution> = BTreeMap::new();
    // Seeded from the corpus, not from the edges.
    //
    // A union of the two tallies alone omits precisely the row a reader is
    // looking for: a language that attributed nothing produces no edge and no
    // unresolved row, so it would simply not appear — and "silently absent" is
    // how a missing extractor looked before this work order existed.
    //
    // Restricted to languages a grammar actually read. Prose and data formats
    // attempt no attribution and never will; a row for every `.md` extension
    // would bury the source languages under noise, which is its own way of
    // hiding the answer.
    // One capability set per language key, unioned over the files reported
    // under it. See `Capabilities::union` for why the union is the honest
    // aggregate.
    let mut capabilities_by_language: std::collections::BTreeMap<
        String,
        devmap_extract::languages::Capabilities,
    > = std::collections::BTreeMap::new();
    for ext in extractions {
        let entry = capabilities_by_language
            .entry(reported(ext).to_string())
            .or_insert(devmap_extract::languages::Capabilities::NONE);
        *entry = entry.union(ext.capabilities());
    }

    let mut present: std::collections::BTreeSet<String> = extractions
        .iter()
        .filter(|ext| !is_vendored_path(&ext.file_path) && is_grammar_read(ext))
        .map(|ext| reported(ext).to_string())
        .collect();
    present.extend(resolved_by_language.keys().cloned());
    present.extend(unresolved_by_language.keys().cloned());

    for language in present {
        let resolved = resolved_by_language.get(&language).copied().unwrap_or(0);
        let (unresolved, explained) = unresolved_by_language
            .get(&language)
            .copied()
            .unwrap_or((0, 0));
        // Asked of the extractions, not of the language string, so a notebook
        // row reports its kernel's capabilities rather than `NONE`. Falls back
        // to the registry for a language that appears only in the resolution
        // maps and has no extraction in this corpus.
        let capabilities = capabilities_by_language
            .get(&language)
            .copied()
            .unwrap_or_else(|| devmap_extract::languages::capabilities_for_language(&language));
        let extracts_calls = capabilities.contains(devmap_extract::languages::Capability::Calls);
        let blind_to: Vec<String> = capabilities
            .missing()
            .map(|capability| capability.label().to_string())
            .collect();
        by_language.insert(
            language,
            LanguageResolution {
                extracts_calls,
                blind_to,
                resolved_sites: resolved,
                unresolved_sites: unresolved,
                explained_sites: explained,
                gross_permille: extracts_calls
                    .then(|| permille(resolved, resolved + unresolved))
                    .flatten(),
                net_permille: extracts_calls
                    .then(|| permille(resolved, resolved + unresolved.saturating_sub(explained)))
                    .flatten(),
            },
        );
    }

    let vendored_files = extractions
        .iter()
        .filter(|ext| is_vendored_path(&ext.file_path) && is_grammar_read(ext))
        .count();

    // The headline is the sum of the measured rows, so it cannot disagree with
    // the breakdown beneath it.
    let (mut measured_resolved, mut measured_unresolved, mut measured_explained) = (0, 0, 0);
    let mut call_blind_sites = 0usize;
    for row in by_language.values() {
        if row.extracts_calls {
            measured_resolved += row.resolved_sites;
            measured_unresolved += row.unresolved_sites;
            measured_explained += row.explained_sites;
        } else {
            call_blind_sites += row.resolved_sites + row.unresolved_sites;
        }
    }

    ResolutionRate {
        resolved_sites,
        unresolved_sites,
        explained_sites,
        gross_permille: permille(measured_resolved, measured_resolved + measured_unresolved),
        net_permille: permille(
            measured_resolved,
            measured_resolved + measured_unresolved.saturating_sub(measured_explained),
        ),
        by_language,
        vendored_sites: vendored_resolved + vendored_unresolved,
        vendored_files,
        call_blind_sites,
        measured_resolved_sites: measured_resolved,
        measured_unresolved_sites: measured_unresolved,
        measured_explained_sites: measured_explained,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three cases a sentinel would confuse.
    #[test]
    fn an_unattempted_rate_is_none_not_zero_and_not_perfect() {
        assert_eq!(permille(0, 0), None, "nothing attempted");
        assert_eq!(permille(0, 5), Some(0), "attempted five, bound none");
        assert_eq!(permille(5, 5), Some(1000), "bound everything");
    }

    /// One site short of perfect must not round to perfect.
    #[test]
    fn the_rate_rounds_toward_zero() {
        assert_eq!(permille(39_999, 40_000), Some(999));
        assert_eq!(permille(1, 3), Some(333));
    }

    /// Structural edges are not attributions and must not inflate the rate.
    #[test]
    fn structural_edge_kinds_are_excluded() {
        assert!(!is_attribution(EdgeKind::Contains));
        assert!(!is_attribution(EdgeKind::Defines));
        assert!(!is_attribution(EdgeKind::MemberOf));
        assert!(is_attribution(EdgeKind::Calls));
        assert!(is_attribution(EdgeKind::Imports));
        assert!(is_attribution(EdgeKind::References));
    }
}
