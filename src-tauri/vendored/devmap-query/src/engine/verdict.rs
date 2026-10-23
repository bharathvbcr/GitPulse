//! Where each answer type keeps its `Response`s, so one exit can stamp them all.
//!
//! `StoreQueryEngine::exit` writes the store's freshness verdict onto every
//! `Response` an answer holds. This is the only place that knows the shape of
//! each answer. There is deliberately no blanket impl: a method that returns a
//! new type through `exit` fails to compile until the type lists its parts.

use crate::evidence::EvidencePack;
use crate::model::{
    AffectedTestsReport, BlastRadius, CloneReport, ExploreReport, LayeredImpact, Neighbors,
    Response, SkeletonReport, SourceFreshness,
};

pub(super) trait StoreVerdict {
    fn stamp(&mut self, verdict: &SourceFreshness);
}

impl<T> StoreVerdict for Response<T> {
    fn stamp(&mut self, verdict: &SourceFreshness) {
        self.source_freshness = verdict.clone();
    }
}

impl StoreVerdict for BlastRadius {
    fn stamp(&mut self, verdict: &SourceFreshness) {
        self.layers.stamp(verdict);
    }
}

impl StoreVerdict for Vec<Neighbors> {
    fn stamp(&mut self, verdict: &SourceFreshness) {
        for neighbors in self {
            neighbors.callers.stamp(verdict);
            neighbors.callees.stamp(verdict);
        }
    }
}

impl StoreVerdict for LayeredImpact {
    fn stamp(&mut self, verdict: &SourceFreshness) {
        self.edges.stamp(verdict);
        self.blast_radius.stamp(verdict);
    }
}

impl StoreVerdict for ExploreReport {
    fn stamp(&mut self, verdict: &SourceFreshness) {
        self.definitions.stamp(verdict);
        for definition in &mut self.definitions.items {
            definition.callers.stamp(verdict);
            definition.callees.stamp(verdict);
        }
        self.blast_radius.stamp(verdict);
    }
}

impl StoreVerdict for AffectedTestsReport {
    fn stamp(&mut self, verdict: &SourceFreshness) {
        self.tests.stamp(verdict);
        self.blast_radius.stamp(verdict);
    }
}

impl StoreVerdict for CloneReport {
    fn stamp(&mut self, verdict: &SourceFreshness) {
        self.groups.stamp(verdict);
    }
}

impl StoreVerdict for SkeletonReport {
    fn stamp(&mut self, verdict: &SourceFreshness) {
        self.source_freshness = verdict.clone();
    }
}

impl StoreVerdict for EvidencePack {
    fn stamp(&mut self, verdict: &SourceFreshness) {
        self.source_freshness = verdict.clone();
        self.related_tests.stamp(verdict);
    }
}

impl StoreVerdict for crate::model::PreviewReport {
    fn stamp(&mut self, verdict: &SourceFreshness) {
        self.broken_callers.stamp(verdict);
    }
}
