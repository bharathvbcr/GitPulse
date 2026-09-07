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
use devmap_resolve::model::ResolvedEdge;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
}
