use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadSymbolReport {
    pub symbol_name: String,
    pub file_path: String,
    pub confidence: f32,
    pub is_exempt: bool,
    pub exemption_reason: Option<String>,
    /// What a reader checks before acting on an unconfirmed row.
    ///
    /// Set on every non-exempt row below the confident tier, `None` elsewhere.
    /// A 0.4 row says "something may reach this"; without the count and the
    /// first site, checking that claim meant grepping the whole tree for a
    /// name, which is the work the row was supposed to save.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<DeadEvidence>,
}

impl DeadEvidence {
    /// No site names the symbol. The analyzer starts every unconfirmed row
    /// here and the site pass raises it.
    pub fn none() -> Self {
        Self {
            sites: 0,
            first_path: None,
            first_byte: None,
            first_line: None,
        }
    }
}

/// The symbols `dead` never examined, so that a short list over a corpus that
/// was mostly exempted cannot read as a clean bill of health.
///
/// Counts exempt rows: symbols nothing calls that the analysis spared, either
/// for their visibility (`not_examined`) or for evidence that something reaches
/// them outside the call graph (`exempt` — a runtime entry point, an interface
/// it implements, a test file). A symbol something *calls* writes no dead row
/// at all and is in neither number.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExcludedPopulation {
    /// Uncalled symbols spared by visibility alone, across every language.
    pub not_examined: u64,
    /// Uncalled symbols spared by a recorded reason, across every language.
    pub exempt: u64,
    /// One entry per language with any exempt row, sorted by language.
    pub languages: Vec<ExcludedLanguage>,
}

/// One language's share of [`ExcludedPopulation`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExcludedLanguage {
    pub language: String,
    /// Spared by a visibility keyword or a language default — see
    /// [`crate::liveness::VISIBILITY_TOKENS`].
    pub not_examined: u64,
    /// Spared by evidence.
    pub exempt: u64,
    /// Both of the above by [`crate::liveness::exemption_token`]; sums to
    /// `not_examined + exempt`.
    pub by_reason: std::collections::BTreeMap<String, u64>,
}

impl ExcludedPopulation {
    /// Fold `(language, stored reason, count)` rows into the disclosure.
    ///
    /// A row with no reason is counted as `exported`: the only writer that
    /// leaves the reason empty on an exempt row predates the split of the old
    /// "Exported or exempt" fallback, whose dominant case was visibility.
    pub fn from_grouped_rows(
        rows: impl IntoIterator<Item = (String, Option<String>, u64)>,
    ) -> Self {
        let mut by_language: std::collections::BTreeMap<String, ExcludedLanguage> =
            std::collections::BTreeMap::new();
        for (language, reason, count) in rows {
            let token = reason
                .as_deref()
                .map(crate::liveness::exemption_token)
                .unwrap_or("exported");
            let entry = by_language
                .entry(language.clone())
                .or_insert_with(|| ExcludedLanguage {
                    language,
                    ..ExcludedLanguage::default()
                });
            *entry.by_reason.entry(token.to_string()).or_default() += count;
            if crate::liveness::VISIBILITY_TOKENS.contains(&token) {
                entry.not_examined += count;
            } else {
                entry.exempt += count;
            }
        }
        let languages: Vec<ExcludedLanguage> = by_language.into_values().collect();
        Self {
            not_examined: languages.iter().map(|entry| entry.not_examined).sum(),
            exempt: languages.iter().map(|entry| entry.exempt).sum(),
            languages,
        }
    }
}

/// The sites behind an unconfirmed dead row.
///
/// `sites` counts the call or reference sites that name the symbol and that
/// the resolver could not bind to exactly one target — ambiguous edges into it
/// and unattributed ledger rows naming it. Zero is a statement, not an absence:
/// a `confident_tier_unproven` or `exported_unreferenced` row with no such site
/// carries `sites: 0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeadEvidence {
    pub sites: u32,
    /// The first such site, ordered by `(path, byte)`, so the answer is stable
    /// across builds. `None` exactly when `sites` is zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_path: Option<String>,
    /// Byte offset of that site in `first_path`, as extracted. The index keeps
    /// no line table, so the query layer converts this to `first_line` against
    /// the file on disk, the way `skeleton` does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_byte: Option<u32>,
    /// One-based line of the first site, when the query layer could read the
    /// file. `None` from the analyzer, which never has the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_line: Option<u32>,
}

/// One source file discovery skipped for exceeding the size ceiling.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OverLimitFile {
    pub path: String,
    pub bytes: u64,
    pub limit: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityReport {
    pub community_id: u32,
    pub name: String,
    pub members: Vec<String>,
    pub cohesion_score: f32, // R6: every community reports cohesion
}

/// The scalar half of [`AnalysisSummary`]: what a coverage disclosure needs.
///
/// `AnalysisSummary` embeds `dead_symbols: Vec<DeadSymbolReport>`, which is a
/// second complete copy of the list `dead_symbols` pages. Measured on the
/// benchmark corpus, the stored blob is 10,122,764 bytes and 10,084,001 of
/// them — 99.6% — are that list. Deserializing the whole summary to read a
/// status field therefore re-materialises the entire corpus, which is what made
/// bounding the row read achieve nothing on its own.
///
/// Deserializing into this instead skips both vectors: serde ignores unknown
/// fields, so the tokens are stepped over rather than turned into `String`s.
/// It reads from exactly the same JSON — no second format, no second writer.
///
/// `analysis_disclosure_agrees_with_the_summary_it_reads` pins the two together;
/// a field that drifts out of this struct silently becomes "not recorded".
#[derive(Debug, Clone, Deserialize)]
pub struct AnalysisDisclosure {
    pub total_files: usize,
    pub total_symbols: usize,
    pub total_edges: usize,
    pub status: AnalysisStatus,
    /// Missing in older summaries is unmeasured, not zero.
    #[serde(default)]
    pub unresolved_calls: Option<usize>,
    /// Read the existing rate's counters without materializing its language map.
    #[serde(default)]
    pub resolution_rate: Option<AttributionCoverage>,
}

/// Scalar projection of `ResolutionRate`, computed by the existing rate owner.
/// No defaults on the counters: a partial breakdown cannot imply zero gaps.
#[derive(Debug, Clone, Deserialize)]
pub struct AttributionCoverage {
    pub unresolved_sites: usize,
    pub explained_sites: usize,
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisSummary {
    pub total_files: usize,
    pub total_symbols: usize,
    pub total_edges: usize,
    pub dead_symbols: Vec<DeadSymbolReport>,
    pub communities: Vec<CommunityReport>,
    pub status: AnalysisStatus,
    /// Calls the resolution ladder could not attribute (R5 / D17).
    ///
    /// Persisted with the generation so a reader can tell "nothing calls this"
    /// apart from "we could not work out what this calls". `serde(default)`
    /// keeps pre-existing serialized summaries readable, where the field's
    /// absence honestly means "not recorded", not "zero".
    #[serde(default)]
    pub unresolved_calls: usize,
    /// How much of this generation carries a body signature.
    ///
    /// Not the duplicate groups themselves — those are derived on demand from
    /// the signature columns on the symbol table, so persisting them here would
    /// be a truncated second copy of a derivable fact. What is *not* derivable
    /// is how many symbols were never signed, which is the denominator every
    /// clone report has to be read against.
    ///
    /// `serde(default)` yields zeroes for generations written before clone
    /// detection existed, and zero signed symbols is exactly the truth about
    /// them: nothing was examined.
    #[serde(default)]
    pub clone_coverage: crate::clones::CloneCoverage,
    /// How many files discovery refused to read when this generation was built.
    ///
    /// Persisted because it cannot be recovered from the stored graph: a file
    /// discovery turned away has no rows at all, so a later reader cannot count
    /// what is missing by looking at what is there.
    ///
    /// The daemon is why this matters. Its incremental resync carries the
    /// previous generation's extractions forward and never re-walks discovery,
    /// so it cannot measure refusals itself — and it overwrites this summary on
    /// every drain. Without the count to carry forward, the `Partial` that
    /// `devmap build` correctly recorded survived only until the next watcher
    /// event, and a corpus with unread files was relabelled complete.
    ///
    /// `None` means **not recorded** — no discovery step's result was reported
    /// — and is not the same as `Some(0)`, which is a measurement that found
    /// nothing refused. `serde(default)` yields `None` for generations written
    /// before this field existed, which is the honest reading of them.
    #[serde(default)]
    pub discovery_refused_files: Option<usize>,
    /// Source files skipped for exceeding the size ceiling, by path.
    ///
    /// Persisted for the reason `discovery_refused_files` is — a skipped file
    /// has no rows, so nothing derived from the graph can name it — and as a
    /// list rather than a count because `repo_map.json` is read by agents
    /// deciding what to trust, and "3 files were not read" without the three
    /// paths is not something they can act on.
    ///
    /// Not part of `discovery_refused_files`: an over-limit file is not a
    /// refusal (see `DiscoverySkipReason::over_limit`), and only the ones that
    /// could hold calls cap the dead-code pass.
    ///
    /// `None` means not recorded, as for `discovery_refused_files`.
    #[serde(default)]
    pub over_limit_files: Option<Vec<OverLimitFile>>,
    /// How much of what the resolver tried to attribute, it attributed —
    /// corpus-wide and per language. See [`crate::resolution_rate`].
    ///
    /// Persisted rather than derived on read, because the unresolved ledger it
    /// divides is not kept on the generation: a later reader has the edges but
    /// not the misses, so the denominator cannot be reconstructed.
    ///
    /// `serde(default)` yields an all-zero rate with `None` percentages for
    /// generations written before this existed, which reads as "never
    /// measured" rather than as "measured zero" — the distinction the
    /// `Permille` alias exists to preserve.
    #[serde(default)]
    pub resolution_rate: crate::resolution_rate::ResolutionRate,
    /// Components of the call graph that reference only each other and are
    /// reached by nothing outside. See [`crate::dead_clusters`].
    ///
    /// Reported apart from `dead_symbols` rather than merged into it, and the
    /// separation is load-bearing twice. A cluster is **one** finding, not one
    /// per member, or a 40-symbol abandoned subsystem pushes forty real
    /// single-symbol findings past `DEAD_CANDIDATE_CAP`. And a cluster verdict
    /// is a weaker claim — it depends on the whole graph being complete, where
    /// a single-symbol verdict depends only on one symbol's inbound edges — so
    /// it carries its own ceiling rather than borrowing the other's tiers.
    ///
    /// `serde(default)` yields an empty scan for generations written before
    /// this existed, which reads as "not computed" and is the truth about them.
    #[serde(default)]
    pub dead_clusters: crate::dead_clusters::DeadClusterScan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AnalysisStatus {
    Ok,
    Partial { reason: String },
    Timeout { reason: String },
}

impl Default for AnalysisStatus {
    /// A summary nobody filled in reports `Ok`.
    ///
    /// Only reachable through [`AnalysisSummary::default`], which exists for
    /// test fixtures: the one production construction site in `analyze()` names
    /// every field, so this can never stand in for a status that was not
    /// computed.
    fn default() -> Self {
        AnalysisStatus::Ok
    }
}

#[cfg(test)]
mod disclosure_tests {
    use super::*;

    fn summary(status: AnalysisStatus, dead: usize) -> AnalysisSummary {
        AnalysisSummary {
            total_files: 7,
            total_symbols: 41,
            total_edges: 93,
            dead_symbols: (0..dead)
                .map(|index| DeadSymbolReport {
                    symbol_name: format!("mod.py::orphan_{index}"),
                    file_path: "mod.py".to_string(),
                    confidence: 0.9,
                    is_exempt: false,
                    exemption_reason: None,
                    evidence: None,
                })
                .collect(),
            communities: Vec::new(),
            status,
            unresolved_calls: 13,
            clone_coverage: crate::clones::CloneCoverage::default(),
            discovery_refused_files: Some(2),
            resolution_rate: crate::resolution_rate::ResolutionRate::default(),
            // Fields this fixture does not exercise. Spread rather than
            // enumerated so a new analysis field does not break every test
            // literal in the workspace; the one production construction in
            // `analyze()` still names every field exhaustively.
            ..Default::default()
        }
    }

    /// The disclosure reads the summary's own JSON, so the two must not drift.
    ///
    /// [`AnalysisDisclosure`] exists so `dead_page` can read a coverage status
    /// without deserializing the summary's embedded copy of the dead-symbol
    /// list. It relies on serde ignoring unknown fields, which means a field
    /// renamed on `AnalysisSummary` would not fail to compile here — it would
    /// silently start reading as its `Default`, and a disclosure that quietly
    /// reports "nothing unattributed" is exactly the reassuring answer this
    /// type must never invent.
    #[test]
    fn analysis_disclosure_agrees_with_the_summary_it_reads() {
        for status in [
            AnalysisStatus::Ok,
            AnalysisStatus::Partial {
                reason: "app.py did not parse".to_string(),
            },
            AnalysisStatus::Timeout {
                reason: "budget exhausted".to_string(),
            },
        ] {
            let full = summary(status.clone(), 4);
            let json = serde_json::to_string(&full).expect("summary serializes");
            let disclosure: AnalysisDisclosure =
                serde_json::from_str(&json).expect("disclosure reads the summary's own JSON");

            assert_eq!(disclosure.total_files, full.total_files);
            assert_eq!(disclosure.total_symbols, full.total_symbols);
            assert_eq!(disclosure.total_edges, full.total_edges);
            assert_eq!(
                disclosure.unresolved_calls,
                Some(full.unresolved_calls),
                "unresolved_calls drifting to its default would turn \"13 calls \
                 are unattributed\" into \"this list is complete\""
            );
            let coverage = disclosure.resolution_rate.as_ref().expect("recorded rate");
            assert_eq!(
                coverage.unresolved_sites,
                full.resolution_rate.unresolved_sites
            );
            assert_eq!(
                coverage.explained_sites,
                full.resolution_rate.explained_sites
            );
            assert_eq!(
                format!("{:?}", disclosure.status),
                format!("{:?}", full.status),
                "the disclosure must carry the same status the summary recorded"
            );
        }
    }
}
