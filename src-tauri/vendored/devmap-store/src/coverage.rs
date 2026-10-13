//! What a generation could not read, kept as an inventory rather than a count.
//!
//! Three kinds of hole, one table. A file discovery refused never reached an
//! extractor at all; a file a grammar was wanted for and did not read left a
//! `File` node and nothing else; a file recovered by line pattern left names
//! and spans but no calls. All three cap the dead-code pass below the confident
//! tier, and until now all three were reported as bare numbers — "2 file(s)
//! failed to parse, 1 recovered by pattern, 1 refused by discovery" — with
//! nothing anywhere naming a path. On this repository that one refusal is a
//! 30.6 MB vendored `parser.c` against a 1 MiB ceiling, which is the correct
//! verdict; the gap was that nothing said so, and an operator could not tell a
//! correct refusal from a broken one without opening the database by hand.
//!
//! The refusal half also has to be *maintained*, not merely reported: the
//! daemon's incremental drain never re-walks discovery, so it carries the
//! previous generation's verdicts forward. A count cannot be maintained — see
//! `schema::COVERAGE_GAPS_TABLE` for what carrying one instead cost.

use std::collections::BTreeMap;

use devmap_analyze::ExtractionGap;
use devmap_extract::model::DiscoveryReport;
use devmap_extract::ProjectManifests;

/// The stored `gap` value for a path discovery refused before extraction.
///
/// Also the label of a project manifest the resolver could not read, which is
/// the same hole met by the build's other walk of the tree: see
/// [`refusal_inventory`].
///
/// Not an [`ExtractionGap`] variant: those two are decided by looking at an
/// `Extraction`, and a refused path has none. Kept in the same table because
/// the question a consumer asks is one question — what did this generation not
/// read — and because `status` reports the three side by side.
pub const GAP_DISCOVERY_REFUSED: &str = "discovery_refused";

/// The stored `gap` value for a source file discovery skipped for exceeding
/// `MAX_SOURCE_BYTES`.
///
/// Its own label, not `discovery_refused`, because the two are read
/// differently: a refusal is coverage loss everywhere and is counted into
/// `discovery_refused_files`, while an over-limit file is a policy skip that is
/// always *listed* and caps the dead-code pass only when it could hold calls
/// (`DiscoveryCoverage::charged_over_limit`). Folding it into either neighbour
/// was tried: as a refusal it left every repository with a vendored blob
/// permanently `partial`; as an ordinary skip it vanished, and `status` called
/// a tree whose 1.7 MB caller was never read fresh.
pub const GAP_OVER_LIMIT: &str = "over_limit";

/// Most paths of one kind that a status answer carries.
///
/// A sample, and every surface that renders it carries the true total beside
/// it. `Store::DEGRADED_SAMPLE` is 5 for quarantined paths, which is a queue an
/// operator drains; this is a corpus inventory that is read once and acted on,
/// so it is wider.
pub const COVERAGE_GAP_SAMPLE: usize = 50;

/// One path a generation could not read, and the verdict that says why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryRefusal {
    pub path: String,
    /// `DiscoverySkipReason`'s own `Display`, which is the one wording the cold
    /// walk and the drain share.
    pub reason: String,
}

/// One row of a coverage-gap listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageGapRow {
    pub path: String,
    pub reason: String,
}

/// A capped listing and the total it was drawn from.
///
/// Never just the rows: a list that stops at fifty and does not say so reads
/// exactly like a corpus with fifty holes in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoverageGapSample {
    /// Every row of this kind in the generation.
    pub total: usize,
    /// At most [`COVERAGE_GAP_SAMPLE`] of them, by path.
    pub shown: Vec<CoverageGapRow>,
}

impl CoverageGapSample {
    pub fn truncated(&self) -> bool {
        self.shown.len() < self.total
    }
}

/// What a generation could not read, in the six kinds it can fail to.
///
/// The last three are not failures at all, which is exactly why they were
/// invisible until W0.2: a `.cfm` or a `.tf` parses `Clean` and this build has
/// no extractor for its calls or its imports, so the file sailed past every
/// check that looks for something going wrong. `not_parsed` is the same shape
/// arrived at from the other side — the extractor declined before any grammar
/// ran, so there is no failure to find and nothing to report unless the
/// decision itself is reported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoverageGaps {
    pub discovery_refused: CoverageGapSample,
    pub parse_failed: CoverageGapSample,
    pub pattern_recovered: CoverageGapSample,
    /// A grammar read the file and no call extractor exists for its language.
    pub call_blind: CoverageGapSample,
    /// A grammar read the file and no import extractor exists for its language.
    pub import_blind: CoverageGapSample,
    /// The extractor declined to parse the file — a minified bundle.
    pub not_parsed: CoverageGapSample,
    /// Discovery skipped the file for exceeding the source size limit.
    pub over_limit: CoverageGapSample,
    /// A grammar read the file and its tree holds `ERROR`/`MISSING` nodes;
    /// each row names the byte ranges lost.
    pub partial_parse: CoverageGapSample,
}

impl CoverageGaps {
    /// The sample for one stored `gap` label, or `None` for a label this
    /// binary does not know.
    ///
    /// Named rather than matched inline so a label added to the table stops
    /// compiling in one place instead of silently landing nowhere.
    pub(crate) fn slot(&mut self, gap: &str) -> Option<&mut CoverageGapSample> {
        if gap == GAP_DISCOVERY_REFUSED {
            Some(&mut self.discovery_refused)
        } else if gap == ExtractionGap::ParseFailed.label() {
            Some(&mut self.parse_failed)
        } else if gap == ExtractionGap::PatternRecovered.label() {
            Some(&mut self.pattern_recovered)
        } else if gap == ExtractionGap::CallBlind.label() {
            Some(&mut self.call_blind)
        } else if gap == ExtractionGap::ImportBlind.label() {
            Some(&mut self.import_blind)
        } else if gap == ExtractionGap::NotParsed.label() {
            Some(&mut self.not_parsed)
        } else if gap == GAP_OVER_LIMIT {
            Some(&mut self.over_limit)
        } else if gap == ExtractionGap::PartialParse.label() {
            Some(&mut self.partial_parse)
        } else {
            None
        }
    }

    /// The labels a reader asks for, in the order `status` reports them.
    ///
    /// The write side stores whatever `ExtractionGap::label()` returns, and
    /// this list is the read side. A label present in one and absent from the
    /// other is a row written to the database on every build and never read
    /// back — which is what happened to `call_blind` and `import_blind` between
    /// their introduction and this line.
    pub(crate) fn labels() -> [&'static str; 8] {
        [
            GAP_DISCOVERY_REFUSED,
            ExtractionGap::ParseFailed.label(),
            ExtractionGap::PatternRecovered.label(),
            ExtractionGap::CallBlind.label(),
            ExtractionGap::ImportBlind.label(),
            ExtractionGap::NotParsed.label(),
            GAP_OVER_LIMIT,
            ExtractionGap::PartialParse.label(),
        ]
    }
}

/// The refusal inventory a cold walk's report establishes.
///
/// `DiscoveryReport::refusals` decides *which* skips are coverage loss — the
/// one owner of that line — and this only puts them in the shape the store
/// persists, so the walk and the inventory cannot disagree about what a
/// refusal is. The reason is rendered through `Display` rather than `Debug`
/// because that is the wording the drain already logs and `devmap build`
/// already prints.
pub fn discovery_refusals(report: &DiscoveryReport) -> Vec<DiscoveryRefusal> {
    report
        .refusals()
        .map(|(path, reason)| DiscoveryRefusal {
            path: path.clone(),
            reason: reason.to_string(),
        })
        .collect()
}

/// The whole refusal inventory for one tree: what discovery refused, and the
/// project manifests the resolver reads beside source that could not be read.
///
/// One owner, because the inventory is *compared* as well as written: `status`
/// and the warm paths of `build` and the drain each decide "still current" by
/// testing the tree's inventory against the stored one. Every one of them must
/// compute it the same way, or a manifest refusal reads as permanent drift to
/// the sites that leave it out and as no change to the ones that include it.
///
/// A manifest is a file this indexer was meant to read and could not, which is
/// the definition [`DiscoverySkipReason::is_refusal`] draws, and it caps the
/// dead-code pass for the same reason: a `go.mod` or `tsconfig.json` that was
/// never read takes its module mapping with it, so the imports that resolved
/// through it — and the callers they carried — are absent from the graph.
///
/// Sorted by path, which is the order the store reads the rows back in, and
/// deduplicated by path with discovery's verdict standing: a root `go.mod` both
/// passes refuse is one row.
///
/// [`DiscoverySkipReason::is_refusal`]: devmap_extract::model::DiscoverySkipReason::is_refusal
pub fn refusal_inventory(
    report: &DiscoveryReport,
    manifests: &ProjectManifests,
) -> Vec<DiscoveryRefusal> {
    let mut inventory: BTreeMap<String, String> = discovery_refusals(report)
        .into_iter()
        .map(|refusal| (refusal.path, refusal.reason))
        .collect();
    add_manifest_refusals(&mut inventory, manifests);
    inventory
        .into_iter()
        .map(|(path, reason)| DiscoveryRefusal { path, reason })
        .collect()
}

/// The over-limit inventory a walk's report establishes: one row per source
/// file skipped for size, by path, worded by `DiscoverySkipReason`'s own
/// `Display` so [`DiscoverySkipReason::parse_over_limit`] can read the numbers
/// back.
///
/// [`DiscoverySkipReason::parse_over_limit`]: devmap_extract::model::DiscoverySkipReason::parse_over_limit
pub fn over_limit_inventory(report: &DiscoveryReport) -> Vec<DiscoveryRefusal> {
    let rows: BTreeMap<&str, String> = report
        .skipped_paths
        .iter()
        .filter(|(_, reason)| reason.over_limit().is_some())
        .map(|(path, reason)| (path.as_str(), reason.to_string()))
        .collect();
    rows.into_iter()
        .map(|(path, reason)| DiscoveryRefusal {
            path: path.to_string(),
            reason,
        })
        .collect()
}

/// Everything a walk of the tree established that the graph cannot show: what
/// discovery and the manifest pass refused, and what discovery skipped for
/// size.
///
/// The unit every "is the stored generation still this tree" check compares.
/// Comparing only the refusals is what let a file that crossed the size limit
/// leave `status` fresh — the hashes lost a path, nothing else noticed — and
/// every one of those checks (`status`, the build's warm path, the drain's two
/// restamp paths) must compare the same thing, so they compare this.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveryInventory {
    /// [`refusal_inventory`]'s rows, stored under [`GAP_DISCOVERY_REFUSED`].
    pub refusals: Vec<DiscoveryRefusal>,
    /// [`over_limit_inventory`]'s rows, stored under [`GAP_OVER_LIMIT`].
    pub over_limit: Vec<DiscoveryRefusal>,
}

impl DiscoveryInventory {
    /// The inventory one walk of the tree establishes.
    pub fn of(report: &DiscoveryReport, manifests: &ProjectManifests) -> Self {
        Self {
            refusals: refusal_inventory(report, manifests),
            over_limit: over_limit_inventory(report),
        }
    }

    /// The over-limit rows as the analysis records them, numbers included.
    ///
    /// A row whose wording does not parse is still listed — its path is the
    /// fact that matters — with zero bytes and limit, which no real verdict
    /// produces; `discovery_inventory_rows_parse` pins that the rows this
    /// crate writes always parse.
    pub fn over_limit_files(&self) -> Vec<devmap_analyze::OverLimitFile> {
        self.over_limit
            .iter()
            .map(|row| {
                let (bytes, limit) =
                    devmap_extract::model::DiscoverySkipReason::parse_over_limit(&row.reason)
                        .unwrap_or((0, 0));
                devmap_analyze::OverLimitFile {
                    path: row.path.clone(),
                    bytes,
                    limit,
                }
            })
            .collect()
    }

    /// What the analysis is charged with: the refusal count, and the
    /// over-limit files for liveness to scope by language.
    pub fn coverage(&self) -> devmap_analyze::DiscoveryCoverage {
        devmap_analyze::DiscoveryCoverage::refused(self.refusals.len())
            .with_over_limit(self.over_limit_files())
    }
}

/// Add the manifest pass's refusals to an inventory by path, leaving any
/// verdict already in it standing.
///
/// The half of [`refusal_inventory`] the incremental drain needs on its own: it
/// has no discovery report to start from, only the previous generation's rows
/// carried forward and the batch it re-read.
pub fn add_manifest_refusals(
    inventory: &mut BTreeMap<String, String>,
    manifests: &ProjectManifests,
) {
    for (path, reason) in &manifests.refusals {
        if reason.is_refusal() {
            inventory
                .entry(path.clone())
                .or_insert_with(|| reason.to_string());
        }
    }
}

#[cfg(test)]
mod write_read_symmetry {
    use super::*;

    /// Every gap the writer can store has a slot the reader can put it in.
    ///
    /// The store writes `ExtractionGap::label()` for whatever the analyzer
    /// produced, and `slot()` matches a fixed list. When W0.2 added `CallBlind`
    /// and `ImportBlind`, the write side picked them up for free — `label()` is
    /// one owner — and the read side did not, so both kinds were persisted on
    /// every build and silently discarded on every read. Nothing failed; the
    /// counts were simply always zero.
    ///
    /// Derived from `ExtractionGap::ALL` rather than restating the list, so a
    /// sixth kind cannot be added without this failing.
    #[test]
    fn every_stored_gap_label_has_a_read_slot() {
        let mut gaps = CoverageGaps::default();
        for gap in ExtractionGap::ALL {
            assert!(
                gaps.slot(gap.label()).is_some(),
                "`{}` is written by the store and has no read slot, so every \
                 row of that kind is discarded on read",
                gap.label()
            );
        }
    }

    /// And the reverse: every label a reader asks for is one a writer produces.
    ///
    /// A slot nothing can fill is a permanently-empty field, which is the
    /// shape W2.4 spent a work order removing two of.
    #[test]
    fn every_read_label_is_one_a_writer_can_produce() {
        let writable: Vec<&str> = ExtractionGap::ALL
            .iter()
            .map(|gap| gap.label())
            .chain([GAP_DISCOVERY_REFUSED, GAP_OVER_LIMIT])
            .collect();
        for label in CoverageGaps::labels() {
            assert!(
                writable.contains(&label),
                "`{label}` is read back and nothing writes it"
            );
        }
    }

    /// An unknown label is refused rather than silently folded into a
    /// neighbouring slot.
    #[test]
    fn an_unknown_gap_label_has_no_slot() {
        let mut gaps = CoverageGaps::default();
        assert!(gaps.slot("not_a_gap_kind").is_none());
    }
}

#[cfg(test)]
mod refusal_inventory_rules {
    use super::*;
    use devmap_extract::model::DiscoverySkipReason;

    fn unreadable(reason: &str) -> DiscoverySkipReason {
        DiscoverySkipReason::Unreadable {
            reason: reason.to_string(),
        }
    }

    fn manifests(refusals: Vec<(&str, DiscoverySkipReason)>) -> ProjectManifests {
        ProjectManifests {
            examined: refusals.iter().map(|(path, _)| path.to_string()).collect(),
            refusals: refusals
                .into_iter()
                .map(|(path, reason)| (path.to_string(), reason))
                .collect(),
            ..Default::default()
        }
    }

    /// The inventory is compared as well as written, so its shape is part of
    /// its meaning: one row per path, by path, discovery's verdict standing,
    /// and nothing that is not coverage loss.
    #[test]
    fn one_row_per_path_in_path_order_with_discovery_standing() {
        let report = DiscoveryReport {
            yielded_paths: vec!["a.py".into()],
            skipped_paths: vec![
                ("z.py".into(), unreadable("discovery said z")),
                ("go.mod".into(), unreadable("discovery said go.mod")),
                ("README.md".into(), DiscoverySkipReason::NonSource),
            ],
        };
        let manifests = manifests(vec![
            ("go.mod", unreadable("manifest said go.mod")),
            (
                "node_modules/b/tsconfig.json",
                unreadable("manifest said b"),
            ),
            // Never produced today, and refused here if it ever is: a skip that
            // is not coverage loss must not become a row because the manifest
            // pass, rather than discovery, met it.
            (
                "huge/tsconfig.json",
                DiscoverySkipReason::Oversized { bytes: 2, limit: 1 },
            ),
        ]);
        let inventory = refusal_inventory(&report, &manifests);
        assert_eq!(
            inventory
                .iter()
                .map(|row| (row.path.as_str(), row.reason.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("go.mod", "unreadable: discovery said go.mod"),
                (
                    "node_modules/b/tsconfig.json",
                    "unreadable: manifest said b"
                ),
                ("z.py", "unreadable: discovery said z"),
            ]
        );
    }

    /// Order-independent: the same facts in any order are the same inventory,
    /// or the warm paths that compare it would see drift that is not there.
    #[test]
    fn the_inventory_does_not_depend_on_the_order_facts_arrive_in() {
        let skipped = vec![
            ("b.py".to_string(), unreadable("b")),
            ("a.py".to_string(), unreadable("a")),
        ];
        let mut reversed = skipped.clone();
        reversed.reverse();
        let manifest_rows = vec![("m/go.mod", unreadable("m")), ("c/go.mod", unreadable("c"))];
        let mut manifest_reversed = manifest_rows.clone();
        manifest_reversed.reverse();
        let one = refusal_inventory(
            &DiscoveryReport {
                skipped_paths: skipped,
                ..Default::default()
            },
            &manifests(manifest_rows),
        );
        let two = refusal_inventory(
            &DiscoveryReport {
                skipped_paths: reversed,
                ..Default::default()
            },
            &manifests(manifest_reversed),
        );
        assert_eq!(one, two);
        assert!(one.windows(2).all(|pair| pair[0].path < pair[1].path));
    }

    /// Over-limit files are their own inventory: never refusals, always
    /// listed, and their numbers survive the trip through the stored wording.
    #[test]
    fn discovery_inventory_rows_parse() {
        let report = DiscoveryReport {
            yielded_paths: vec!["lib.py".into()],
            skipped_paths: vec![
                (
                    "gen.py".into(),
                    DiscoverySkipReason::Oversized {
                        bytes: 1_708_958,
                        limit: 1_048_576,
                    },
                ),
                ("z.py".into(), unreadable("z")),
                ("README.md".into(), DiscoverySkipReason::NonSource),
            ],
        };
        let inventory = DiscoveryInventory::of(&report, &ProjectManifests::default());
        assert_eq!(
            inventory
                .refusals
                .iter()
                .map(|row| row.path.as_str())
                .collect::<Vec<_>>(),
            vec!["z.py"],
            "an over-limit file is not a refusal"
        );
        assert_eq!(
            inventory.over_limit_files(),
            vec![devmap_analyze::OverLimitFile {
                path: "gen.py".into(),
                bytes: 1_708_958,
                limit: 1_048_576,
            }]
        );
        let coverage = inventory.coverage();
        assert_eq!(coverage.refused_files(), Some(1));
        assert_eq!(
            coverage.charged_over_limit().collect::<Vec<_>>(),
            vec![("gen.py", "python")],
            "a Python file can hold calls, so it is charged"
        );
    }

    /// The drain's half adds without overriding what the batch decided.
    #[test]
    fn adding_manifest_refusals_never_overrides_a_verdict_already_held() {
        let mut inventory: BTreeMap<String, String> =
            [("go.mod".to_string(), "unreadable: batch".to_string())].into();
        add_manifest_refusals(
            &mut inventory,
            &manifests(vec![
                ("go.mod", unreadable("manifest")),
                ("x/go.mod", unreadable("x")),
            ]),
        );
        assert_eq!(inventory["go.mod"], "unreadable: batch");
        assert_eq!(inventory["x/go.mod"], "unreadable: x");
    }
}
