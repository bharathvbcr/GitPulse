//! The resolution ladder, addressable from outside.
//!
//! The kernel's ladder has five rungs with abstention, receiver poisoning, and
//! honesty invariants asserting that each rung claims only what its evidence
//! entitles it to. A caller could not ask for one.
//!
//! Not *nothing* was queryable: `min_confidence: f32` already reached `deps`,
//! `neighbors`, `explore`, `affected` and `preview`. But a float is the wrong
//! handle for a ladder with named rungs — a caller wanting deterministic edges
//! had to know that meant 1.0, and `impact` and `trace` took no threshold at
//! all, so the two queries a refactor actually runs could not be narrowed.
//!
//! Two things this adds:
//!
//! * **A name per rung**, resolved to an exact integer floor. Milliconfidence
//!   rather than float comparison, for the reason `EXTRACTED_FLOOR_MILLIS`
//!   already exists: SQLite REAL cannot round-trip `f32` 0.9, so `>= 0.9` on a
//!   persisted value is a comparison whose answer depends on rounding.
//! * **A histogram beside the results**, so a caller can see what the filter
//!   cost. A narrowed answer that does not say how much it dropped is
//!   indistinguishable from a sparse graph — the same "absence read as
//!   evidence" shape this kernel spends its effort avoiding.

use devmap_extract::model::confidence_millis;
use devmap_resolve::model::{ResolutionKind, ResolvedEdge};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The complete 11-tier resolution ladder with strict confidence scoring ($1.0$ down to $0.1$).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ResolutionLadderTier {
    /// Tier 1: Declared in same file and matching lexical scope ($1.0$).
    SameFile = 1,
    /// Tier 2: Declared in same package / inline module ($1.0$).
    SamePackage = 2,
    /// Tier 3: Matches explicit named import ($1.0$).
    ImportScoped = 3,
    /// Tier 4: Statically typed receiver with declared member ($1.0$).
    ReceiverType = 4,
    /// Tier 5: Language server compiler daemon binding ($0.9$).
    LanguageServer = 5,
    /// Tier 6: Exactly one in-family namesake passing visibility ($0.9$).
    UniqueGlobal = 6,
    /// Tier 7: Multiple namesakes without distinguishing evidence ($0.1$).
    AmbiguousGlobal = 7,
    /// Tier 8: Wide polymorphic LSP candidate set ($0.1$).
    LanguageServerDispatch = 8,
    /// Tier 9: Polymorphic interface/trait method bound up to cap ($0.5$).
    Dispatch = 9,
    /// Tier 10: Structural tree containment ($1.0$).
    Structural = 10,
    /// Tier 11: Floor for unresolved references ($0.1$ or $0.0$).
    Unresolved = 11,
}

impl ResolutionLadderTier {
    pub const ALL: [ResolutionLadderTier; 11] = [
        ResolutionLadderTier::SameFile,
        ResolutionLadderTier::SamePackage,
        ResolutionLadderTier::ImportScoped,
        ResolutionLadderTier::ReceiverType,
        ResolutionLadderTier::LanguageServer,
        ResolutionLadderTier::UniqueGlobal,
        ResolutionLadderTier::AmbiguousGlobal,
        ResolutionLadderTier::LanguageServerDispatch,
        ResolutionLadderTier::Dispatch,
        ResolutionLadderTier::Structural,
        ResolutionLadderTier::Unresolved,
    ];

    pub fn confidence_score(self) -> f32 {
        match self {
            ResolutionLadderTier::SameFile
            | ResolutionLadderTier::SamePackage
            | ResolutionLadderTier::ImportScoped
            | ResolutionLadderTier::ReceiverType
            | ResolutionLadderTier::Structural => 1.0,
            ResolutionLadderTier::LanguageServer | ResolutionLadderTier::UniqueGlobal => 0.9,
            ResolutionLadderTier::Dispatch => 0.5,
            ResolutionLadderTier::AmbiguousGlobal
            | ResolutionLadderTier::LanguageServerDispatch
            | ResolutionLadderTier::Unresolved => 0.1,
        }
    }

    pub fn confidence_millis(self) -> i64 {
        (self.confidence_score() * 1000.0) as i64
    }

    pub fn label(self) -> &'static str {
        match self {
            ResolutionLadderTier::SameFile => "SameFile",
            ResolutionLadderTier::SamePackage => "SamePackage",
            ResolutionLadderTier::ImportScoped => "ImportScoped",
            ResolutionLadderTier::ReceiverType => "ReceiverType",
            ResolutionLadderTier::LanguageServer => "LanguageServer",
            ResolutionLadderTier::UniqueGlobal => "UniqueGlobal",
            ResolutionLadderTier::AmbiguousGlobal => "AmbiguousGlobal",
            ResolutionLadderTier::LanguageServerDispatch => "LanguageServerDispatch",
            ResolutionLadderTier::Dispatch => "Dispatch",
            ResolutionLadderTier::Structural => "Structural",
            ResolutionLadderTier::Unresolved => "Unresolved",
        }
    }

    pub fn from_resolution_kind(kind: ResolutionKind) -> Self {
        match kind {
            ResolutionKind::SameFile => ResolutionLadderTier::SameFile,
            ResolutionKind::SamePackage => ResolutionLadderTier::SamePackage,
            ResolutionKind::ImportScoped => ResolutionLadderTier::ImportScoped,
            ResolutionKind::ReceiverType => ResolutionLadderTier::ReceiverType,
            ResolutionKind::LanguageServer => ResolutionLadderTier::LanguageServer,
            ResolutionKind::UniqueGlobal => ResolutionLadderTier::UniqueGlobal,
            ResolutionKind::AmbiguousGlobal => ResolutionLadderTier::AmbiguousGlobal,
            ResolutionKind::LanguageServerDispatch => ResolutionLadderTier::LanguageServerDispatch,
            ResolutionKind::Dispatch => ResolutionLadderTier::Dispatch,
            ResolutionKind::Structural => ResolutionLadderTier::Structural,
            ResolutionKind::Unresolved => ResolutionLadderTier::Unresolved,
        }
    }

    pub fn to_resolution_kind(self) -> ResolutionKind {
        match self {
            ResolutionLadderTier::SameFile => ResolutionKind::SameFile,
            ResolutionLadderTier::SamePackage => ResolutionKind::SamePackage,
            ResolutionLadderTier::ImportScoped => ResolutionKind::ImportScoped,
            ResolutionLadderTier::ReceiverType => ResolutionKind::ReceiverType,
            ResolutionLadderTier::LanguageServer => ResolutionKind::LanguageServer,
            ResolutionLadderTier::UniqueGlobal => ResolutionKind::UniqueGlobal,
            ResolutionLadderTier::AmbiguousGlobal => ResolutionKind::AmbiguousGlobal,
            ResolutionLadderTier::LanguageServerDispatch => ResolutionKind::LanguageServerDispatch,
            ResolutionLadderTier::Dispatch => ResolutionKind::Dispatch,
            ResolutionLadderTier::Structural => ResolutionKind::Structural,
            ResolutionLadderTier::Unresolved => ResolutionKind::Unresolved,
        }
    }
}

/// Histogram counting edges across all 11 resolution ladder tiers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LadderHistogram {
    pub counts: BTreeMap<String, usize>,
    pub total: usize,
}

impl LadderHistogram {
    pub fn from_edges(edges: &[ResolvedEdge]) -> Self {
        let mut counts = BTreeMap::new();
        for tier in ResolutionLadderTier::ALL {
            counts.insert(tier.label().to_string(), 0);
        }
        let mut total = 0;
        for edge in edges {
            if let Some(res) = &edge.resolution {
                let tier = ResolutionLadderTier::from_resolution_kind(res.kind());
                *counts.entry(tier.label().to_string()).or_default() += 1;
                total += 1;
            }
        }
        Self { counts, total }
    }

    pub fn count_for_tier(&self, tier: ResolutionLadderTier) -> usize {
        self.counts.get(tier.label()).copied().unwrap_or(0)
    }
}

/// A named floor on the resolution ladder.
///
/// Three names rather than five: these are the boundaries a caller actually
/// makes decisions at. The five internal rungs collapse into them, and the
/// histogram below reports the collapsed buckets so the vocabulary a caller
/// filters with and the vocabulary they read back are the same one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rung {
    /// Evidence that admits one answer: a same-file definition, an import
    /// binding, a typed receiver. `Confidence::DETERMINISTIC` (1000).
    Deterministic,
    /// A unique global match — one definition of the name in the corpus.
    /// `Confidence::HIGH` (900) and `MEDIUM` (700).
    High,
    /// An ambiguous match the resolver could not choose between, or an edge it
    /// recorded without binding. `Confidence::LOW` (400) and `SPECULATIVE`
    /// (200). The default: filtering nothing.
    Speculative,
}

impl Rung {
    /// The inclusive milliconfidence floor this rung admits.
    ///
    /// Integers, and integers all the way to the comparison. A float floor
    /// compared against a value that made a round trip through SQLite REAL is
    /// a filter whose boundary behaviour depends on the storage format.
    pub const fn floor_millis(self) -> i64 {
        match self {
            Rung::Deterministic => 1000,
            Rung::High => 700,
            Rung::Speculative => 0,
        }
    }

    /// The wire name, and the one the histogram buckets under.
    pub const fn label(self) -> &'static str {
        match self {
            Rung::Deterministic => "deterministic",
            Rung::High => "high",
            Rung::Speculative => "speculative",
        }
    }

    /// Every rung, strongest first — the order a histogram reads best in.
    pub const ALL: &'static [Rung] = &[Rung::Deterministic, Rung::High, Rung::Speculative];

    /// Parse a caller-supplied name.
    ///
    /// `None` for anything else, and callers refuse rather than defaulting: a
    /// typo silently answered at full breadth is a filtered answer a caller
    /// believes is narrow, which is worse than an error.
    pub fn parse(name: &str) -> Option<Rung> {
        Rung::ALL.iter().copied().find(|rung| rung.label() == name)
    }

    /// Which rung an edge's confidence sits on.
    pub fn of_millis(millis: i64) -> Rung {
        if millis >= Rung::Deterministic.floor_millis() {
            Rung::Deterministic
        } else if millis >= Rung::High.floor_millis() {
            Rung::High
        } else {
            Rung::Speculative
        }
    }

    /// Whether an edge at `millis` passes a floor of `self`.
    pub fn admits(self, millis: i64) -> bool {
        millis >= self.floor_millis()
    }
}

/// How many edges sat on each rung, before any filtering.
///
/// Reported alongside results so a caller can see what a narrowed query cost
/// them. Without it, `min_rung: deterministic` returning three edges is
/// indistinguishable from a graph that only had three — and the second reading
/// is the one that leads somebody to conclude the code is unreferenced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RungHistogram {
    pub deterministic: usize,
    pub high: usize,
    pub speculative: usize,
    /// Edges the floor removed. `total - kept`, carried rather than left to be
    /// derived, because that subtraction is exactly what a caller skips.
    pub filtered_out: usize,
}

impl RungHistogram {
    pub fn total(&self) -> usize {
        self.deterministic + self.high + self.speculative
    }

    pub fn as_map(&self) -> BTreeMap<&'static str, usize> {
        BTreeMap::from([
            (Rung::Deterministic.label(), self.deterministic),
            (Rung::High.label(), self.high),
            (Rung::Speculative.label(), self.speculative),
        ])
    }
}

/// Bucket `edges` by rung, and count what a `floor` would remove.
///
/// The histogram describes the **unfiltered** population deliberately: its job
/// is to answer "what did I not see", which a histogram of the survivors cannot
/// do.
pub fn histogram(edges: &[ResolvedEdge], floor: Option<Rung>) -> RungHistogram {
    let mut counts = RungHistogram::default();
    for edge in edges {
        let millis = confidence_millis(edge.confidence.0);
        match Rung::of_millis(millis) {
            Rung::Deterministic => counts.deterministic += 1,
            Rung::High => counts.high += 1,
            Rung::Speculative => counts.speculative += 1,
        }
        if floor.is_some_and(|rung| !rung.admits(millis)) {
            counts.filtered_out += 1;
        }
    }
    counts
}

/// Drop every edge below `floor`, keeping input order.
pub fn filter_by_rung(edges: Vec<ResolvedEdge>, floor: Rung) -> Vec<ResolvedEdge> {
    edges
        .into_iter()
        .filter(|edge| floor.admits(confidence_millis(edge.confidence.0)))
        .collect()
}

/// Filter and report in one step.
///
/// The two halves are not separable: a caller that filters without publishing
/// what the filter removed has produced exactly the artefact this kernel spends
/// its effort avoiding — a short list indistinguishable from a sparse graph.
/// Returning them together means no call site can do one and forget the other.
///
/// `None` filters nothing and still reports, because the distribution of an
/// unfiltered answer is worth reading too: three edges that are all
/// `speculative` and three that are all `deterministic` are not the same
/// answer, and before this they printed identically.
pub fn narrow(edges: Vec<ResolvedEdge>, floor: Option<Rung>) -> (Vec<ResolvedEdge>, RungHistogram) {
    let counts = histogram(&edges, floor);
    let kept = match floor {
        Some(rung) => filter_by_rung(edges, rung),
        None => edges,
    };
    debug_assert_eq!(
        counts.total() - counts.filtered_out,
        kept.len(),
        "the histogram must account for every edge the filter dropped"
    );
    (kept, counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use devmap_extract::model::Confidence;

    #[test]
    fn each_confidence_constant_lands_on_the_rung_it_names() {
        assert_eq!(
            Rung::of_millis(Confidence::DETERMINISTIC.to_millis()),
            Rung::Deterministic
        );
        assert_eq!(Rung::of_millis(Confidence::HIGH.to_millis()), Rung::High);
        assert_eq!(Rung::of_millis(Confidence::MEDIUM.to_millis()), Rung::High);
        assert_eq!(
            Rung::of_millis(Confidence::LOW.to_millis()),
            Rung::Speculative
        );
        assert_eq!(
            Rung::of_millis(Confidence::SPECULATIVE.to_millis()),
            Rung::Speculative
        );
    }

    /// The boundary the float comparison could not be trusted at.
    ///
    /// `Confidence::HIGH` is `0.9`, which SQLite REAL cannot round-trip; the
    /// integer floor is what makes `>= high` mean the same thing before and
    /// after a store round trip.
    #[test]
    fn the_high_rung_admits_its_own_boundary_exactly() {
        assert!(Rung::High.admits(Confidence::HIGH.to_millis()));
        assert!(Rung::High.admits(Confidence::MEDIUM.to_millis()));
        assert!(!Rung::High.admits(Confidence::LOW.to_millis()));
        assert!(Rung::Deterministic.admits(Confidence::DETERMINISTIC.to_millis()));
        assert!(!Rung::Deterministic.admits(Confidence::HIGH.to_millis()));
    }

    /// Speculative is the default and filters nothing.
    #[test]
    fn the_speculative_rung_admits_everything() {
        for confidence in [
            Confidence::DETERMINISTIC,
            Confidence::HIGH,
            Confidence::MEDIUM,
            Confidence::LOW,
            Confidence::SPECULATIVE,
        ] {
            assert!(Rung::Speculative.admits(confidence.to_millis()));
        }
        assert!(Rung::Speculative.admits(0));
    }

    #[test]
    fn a_rung_name_round_trips_and_a_typo_does_not_parse() {
        for rung in Rung::ALL {
            assert_eq!(Rung::parse(rung.label()), Some(*rung));
        }
        assert_eq!(Rung::parse("Deterministic"), None, "case is significant");
        assert_eq!(Rung::parse("lsp"), None, "another tool's vocabulary");
        assert_eq!(Rung::parse(""), None);
    }

    #[test]
    fn test_11_tier_resolution_ladder_and_histogram() {
        assert_eq!(ResolutionLadderTier::ALL.len(), 11);
        assert_eq!(ResolutionLadderTier::SameFile.confidence_score(), 1.0);
        assert_eq!(ResolutionLadderTier::SamePackage.confidence_score(), 1.0);
        assert_eq!(ResolutionLadderTier::ImportScoped.confidence_score(), 1.0);
        assert_eq!(ResolutionLadderTier::ReceiverType.confidence_score(), 1.0);
        assert_eq!(ResolutionLadderTier::LanguageServer.confidence_score(), 0.9);
        assert_eq!(ResolutionLadderTier::UniqueGlobal.confidence_score(), 0.9);
        assert_eq!(
            ResolutionLadderTier::AmbiguousGlobal.confidence_score(),
            0.1
        );
        assert_eq!(
            ResolutionLadderTier::LanguageServerDispatch.confidence_score(),
            0.1
        );
        assert_eq!(ResolutionLadderTier::Dispatch.confidence_score(), 0.5);
        assert_eq!(ResolutionLadderTier::Structural.confidence_score(), 1.0);
        assert_eq!(ResolutionLadderTier::Unresolved.confidence_score(), 0.1);

        let mut hist = LadderHistogram::default();
        for tier in ResolutionLadderTier::ALL {
            hist.counts.insert(tier.label().to_string(), 0);
        }
        *hist
            .counts
            .get_mut(ResolutionLadderTier::SameFile.label())
            .unwrap() += 5;
        *hist
            .counts
            .get_mut(ResolutionLadderTier::Unresolved.label())
            .unwrap() += 2;
        hist.total = 7;

        assert_eq!(hist.count_for_tier(ResolutionLadderTier::SameFile), 5);
        assert_eq!(hist.count_for_tier(ResolutionLadderTier::Unresolved), 2);
        assert_eq!(hist.count_for_tier(ResolutionLadderTier::SamePackage), 0);
        assert_eq!(hist.total, 7);
    }
}
