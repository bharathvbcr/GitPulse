//! Duplicate-body detection over the signatures stamped at extraction time.
//!
//! Grouping is a join on a hash, so the cost is one pass over the symbols. The
//! judgement is all in what gets grouped and what gets reported:
//!
//! - **Type-1 (`Exact`)** — identical code, modulo formatting and comments. Any
//!   kind with a comparable body qualifies.
//! - **Type-2 (`Structural`)** — the same shape under renaming. Restricted to
//!   callables, because a type declaration's "shape" is only its field count
//!   and arity. Measured on a 651-file Go repository, `Struct` symbols were 107
//!   of 506 structural group members: a fifth of the report was "these two
//!   structs both have four fields".
//!
//! A structural group whose members are all byte-identical says nothing the
//! exact report has not already said, so it is dropped rather than duplicated.

use devmap_extract::model::{Extraction, SymbolKind};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Members listed per group.
///
/// This bounds one *item*, which is the only cap this layer applies. Grouping
/// itself is not truncated: the query layer already has a token budget that
/// reports exactly what it withheld, and a second cap here would mean two
/// numbers describing the same cut — the arrangement where a reader adds them,
/// or trusts the smaller one, and is wrong either way. A tree with thousands of
/// duplications gets thousands of groups, and the budget decides how many of
/// them a given caller sees.
pub const MAX_GROUP_MEMBERS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CloneKind {
    /// The same code: node kinds and leaf text both match.
    Exact,
    /// The same shape: node kinds match, names and literals differ.
    Structural,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloneMember {
    pub file_path: String,
    pub symbol_name: String,
    pub qualified_name: String,
    pub span_start: usize,
    pub span_end: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloneGroup {
    pub kind: CloneKind,
    /// The hash the members share. Stable across builds for unchanged code, so
    /// a caller can track one duplication over time.
    pub signature: u64,
    /// Smallest body in the group, in non-comment parse nodes. The weight
    /// behind the finding: 500 nodes shared is evidence, 32 is a coincidence.
    pub min_nodes: u32,
    pub members: Vec<CloneMember>,
    /// Members beyond [`MAX_GROUP_MEMBERS`], or 0 when the list is complete.
    #[serde(default)]
    pub members_omitted: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CloneSummary {
    pub groups: Vec<CloneGroup>,
    /// Symbols that carried a body signature — the denominator this report is
    /// over. Without it "no clones" is unreadable: it could mean a clean tree
    /// or a tree nothing was signed in.
    pub signed_symbols: usize,
    /// Symbols with no signature: below the size floor, a kind with no
    /// comparable body, or a file no grammar parsed.
    pub unsigned_symbols: usize,
}

/// What a generation records about duplication: the denominator only.
///
/// The groups themselves are *not* persisted. They are derivable from the three
/// signature columns on the symbol table by the grouping below, so storing them
/// too would put a second copy of the same fact in the database — one that goes
/// stale, and one that had to be truncated to fit. Measured on this workspace a
/// full listing is 496 groups; a persisted copy capped at
/// [`MAX_CLONE_GROUPS`] would have silently dropped 296 of them.
///
/// The coverage counts are kept because they are *not* derivable after the
/// fact: a symbol with no signature leaves no row saying why, and without these
/// two numbers an empty clone report cannot be told from an unexamined tree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloneCoverage {
    pub signed_symbols: usize,
    pub unsigned_symbols: usize,
}

/// One symbol considered for duplication.
///
/// Owned rather than borrowed because the two producers are a live extraction
/// pass and a SQL row set, and only one of them has anything to borrow from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneCandidate {
    pub file_path: String,
    pub symbol_name: String,
    pub qualified_name: String,
    pub span_start: usize,
    pub span_end: usize,
    pub kind: SymbolKind,
    pub exact: u64,
    pub structural: u64,
    pub nodes: u32,
}

/// Whether a kind's shape means anything under renaming.
///
/// Callables only. A struct is its fields; two unrelated structs with the same
/// field count are not "the same shape" in any sense a reader can act on.
fn structurally_comparable(kind: SymbolKind) -> bool {
    matches!(kind, SymbolKind::Function | SymbolKind::Method)
}

/// Count signed and unsigned symbols without grouping anything.
///
/// This is what a build records. It walks the symbols once and allocates
/// nothing: the build path needs the denominator, not the findings, and paying
/// for a full grouping on every build to throw the result away would be a cost
/// with no reader.
pub fn clone_coverage(extractions: &[Extraction]) -> CloneCoverage {
    let mut coverage = CloneCoverage::default();
    for extraction in extractions {
        for symbol in &extraction.symbols {
            if symbol.body_signature.is_some() {
                coverage.signed_symbols += 1;
            } else {
                coverage.unsigned_symbols += 1;
            }
        }
    }
    coverage
}

/// Lift signed symbols out of extractions into grouping candidates.
///
/// The in-process counterpart to reading the signature columns back from the
/// store: both produce the same candidate list, so both reach the same report
/// through [`group_clones`] rather than through two grouping implementations
/// that agree until one of them is changed.
pub fn candidates_from_extractions(extractions: &[Extraction]) -> Vec<CloneCandidate> {
    let mut candidates = Vec::new();
    for extraction in extractions {
        for symbol in &extraction.symbols {
            let Some(signature) = symbol.body_signature else {
                continue;
            };
            candidates.push(CloneCandidate {
                file_path: extraction.file_path.clone(),
                symbol_name: symbol.name.clone(),
                qualified_name: symbol.qualified_name.clone(),
                span_start: symbol.span.start_byte,
                span_end: symbol.span.end_byte,
                kind: symbol.kind,
                exact: signature.exact,
                structural: signature.structural,
                nodes: signature.nodes,
            });
        }
    }
    candidates
}

/// The single owner of what counts as a clone group.
///
/// `unsigned_symbols` is passed in rather than inferred: a candidate list has
/// no way to know how many symbols were skipped before it was built, and
/// guessing zero would turn a partially-signed tree into a clean bill of health.
pub fn group_clones(candidates: &[CloneCandidate], unsigned_symbols: usize) -> CloneSummary {
    let mut by_exact: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut by_structural: HashMap<u64, Vec<usize>> = HashMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        by_exact.entry(candidate.exact).or_default().push(index);
        if structurally_comparable(candidate.kind) {
            by_structural
                .entry(candidate.structural)
                .or_default()
                .push(index);
        }
    }

    let mut groups: Vec<CloneGroup> = Vec::new();
    for (signature, members) in &by_exact {
        if members.len() < 2 {
            continue;
        }
        groups.push(build_group(
            CloneKind::Exact,
            *signature,
            members,
            candidates,
        ));
    }
    for (signature, members) in &by_structural {
        if members.len() < 2 {
            continue;
        }
        // Every member byte-identical means the exact report already carries
        // this finding. Reporting it twice inflates the count without adding a
        // fact.
        let distinct_exact = members
            .iter()
            .map(|index| candidates[*index].exact)
            .collect::<std::collections::HashSet<_>>()
            .len();
        if distinct_exact < 2 {
            continue;
        }
        groups.push(build_group(
            CloneKind::Structural,
            *signature,
            members,
            candidates,
        ));
    }

    // Largest bodies first: a 900-node duplication is worth more of a reader's
    // attention than a 33-node one, whatever the member count. Ties break on
    // full member count then signature. Counting just the displayed sample
    // makes every group of 32 or more tie and lets a smaller group win on its
    // hash instead. The report must rank the population, not its sample (R7).
    groups.sort_by(|a, b| {
        b.min_nodes
            .cmp(&a.min_nodes)
            .then_with(|| {
                (b.members.len() + b.members_omitted).cmp(&(a.members.len() + a.members_omitted))
            })
            .then(a.signature.cmp(&b.signature))
    });

    CloneSummary {
        groups,
        signed_symbols: candidates.len(),
        unsigned_symbols,
    }
}

fn build_group(
    kind: CloneKind,
    signature: u64,
    member_indices: &[usize],
    candidates: &[CloneCandidate],
) -> CloneGroup {
    let min_nodes = member_indices
        .iter()
        .map(|index| candidates[*index].nodes)
        .min()
        .unwrap_or(0);

    let mut members: Vec<CloneMember> = member_indices
        .iter()
        .map(|index| {
            let candidate = &candidates[*index];
            CloneMember {
                file_path: candidate.file_path.clone(),
                symbol_name: candidate.symbol_name.clone(),
                qualified_name: candidate.qualified_name.clone(),
                span_start: candidate.span_start,
                span_end: candidate.span_end,
            }
        })
        .collect();
    // Sorted by location so the same duplication lists its members in the same
    // order on every build, whatever order the files were walked in.
    members.sort_by(|a, b| {
        (&a.file_path, a.span_start, &a.symbol_name).cmp(&(
            &b.file_path,
            b.span_start,
            &b.symbol_name,
        ))
    });
    let members_omitted = members.len().saturating_sub(MAX_GROUP_MEMBERS);
    members.truncate(MAX_GROUP_MEMBERS);

    CloneGroup {
        kind,
        signature,
        min_nodes,
        members,
        members_omitted,
    }
}

#[cfg(test)]
mod ranking_regressions {
    use super::{group_clones, CloneCandidate, MAX_GROUP_MEMBERS};
    use devmap_extract::model::SymbolKind;

    // R4/R7: rank the population before applying the member sample cap.
    #[test]
    fn clone_ranking_uses_all_members_before_the_display_cap() {
        let sizes = [2, 31, 32, 33, 65, 1_000];
        let mut candidates = Vec::new();
        for (group, count) in sizes.into_iter().enumerate() {
            let signature = u64::try_from(group).expect("six fixture groups fit in u64");
            for member in 0..count {
                candidates.push(CloneCandidate {
                    file_path: format!("g{group}/m{member:04}.rs"),
                    symbol_name: "Record".into(),
                    qualified_name: "Record".into(),
                    span_start: 0,
                    span_end: 100,
                    kind: SymbolKind::Struct,
                    exact: signature,
                    structural: signature,
                    nodes: 40,
                });
            }
        }
        let report = group_clones(&candidates, 7);
        let counts: Vec<_> = report
            .groups
            .iter()
            .map(|group| group.members.len() + group.members_omitted)
            .collect();
        assert_eq!(counts, sizes.into_iter().rev().collect::<Vec<_>>());
        assert_eq!(report.signed_symbols, candidates.len());
        assert_eq!(report.unsigned_symbols, 7);
        for group in &report.groups {
            assert!(group.members.len() <= MAX_GROUP_MEMBERS);
        }
        candidates.reverse();
        assert_eq!(
            serde_json::to_string(&report).expect("serialize report"),
            serde_json::to_string(&group_clones(&candidates, 7))
                .expect("serialize reversed report")
        );
    }

    // A mutation exposed that the prior structural fixtures all had two
    // members: rejecting every larger group still passed those tests.
    #[test]
    fn structural_clone_groups_include_more_than_two_distinct_bodies() {
        let candidates: Vec<_> = (0..3)
            .map(|member| CloneCandidate {
                file_path: format!("member{member}.py"),
                symbol_name: "compute".into(),
                qualified_name: "compute".into(),
                span_start: 0,
                span_end: 100,
                kind: SymbolKind::Function,
                exact: member,
                structural: 99,
                nodes: 40,
            })
            .collect();
        let report = group_clones(&candidates, 0);
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].kind, super::CloneKind::Structural);
        assert_eq!(report.groups[0].members.len(), 3);
        assert_eq!(report.groups[0].members_omitted, 0);
    }
}

#[cfg(all(test, feature = "parse"))]
mod tests {
    use super::*;
    use devmap_extract::extract_file;

    const BODY: &str = "\n    total = 0\n    for row in rows:\n        if row.active:\n            total += row.amount * rate\n        else:\n            total -= row.penalty\n    return total\n";

    fn summarize(files: &[(&str, String)]) -> CloneSummary {
        let extractions: Vec<_> = files
            .iter()
            .map(|(path, source)| extract_file(path, source))
            .collect();
        let coverage = clone_coverage(&extractions);
        let candidates = candidates_from_extractions(&extractions);
        assert_eq!(
            candidates.len(),
            coverage.signed_symbols,
            "coverage and candidate extraction disagree on what is signed"
        );
        group_clones(&candidates, coverage.unsigned_symbols)
    }

    #[test]
    fn identical_bodies_in_two_files_are_one_exact_group() {
        let body = format!("def compute(rows, rate):{BODY}");
        let summary = summarize(&[("a.py", body.clone()), ("b.py", body)]);
        let exact: Vec<_> = summary
            .groups
            .iter()
            .filter(|g| g.kind == CloneKind::Exact)
            .collect();
        assert_eq!(
            exact.len(),
            1,
            "expected one exact group: {:?}",
            summary.groups
        );
        assert_eq!(exact[0].members.len(), 2);
        assert_eq!(exact[0].members[0].file_path, "a.py");
        assert_eq!(exact[0].members[1].file_path, "b.py");
    }

    #[test]
    fn a_structural_group_of_identical_bodies_is_not_reported_twice() {
        let body = format!("def compute(rows, rate):{BODY}");
        let summary = summarize(&[("a.py", body.clone()), ("b.py", body)]);
        assert!(
            summary.groups.iter().all(|g| g.kind == CloneKind::Exact),
            "byte-identical bodies were reported as both an exact and a \
             structural finding: {:?}",
            summary.groups
        );
    }

    #[test]
    fn renamed_bodies_are_a_structural_group_and_not_an_exact_one() {
        let original = format!("def compute(rows, rate):{BODY}");
        let renamed = format!("def compute(rows, rate):{}", BODY.replace("total", "sum_"));
        let summary = summarize(&[("a.py", original), ("b.py", renamed)]);
        let kinds: Vec<_> = summary.groups.iter().map(|g| g.kind).collect();
        assert_eq!(
            kinds,
            vec![CloneKind::Structural],
            "renamed copies should be a Type-2 finding only: {:?}",
            summary.groups
        );
        assert_eq!(summary.groups[0].members.len(), 2);
    }

    #[test]
    fn unrelated_bodies_produce_no_groups() {
        let a = "def compute(rows, rate):\n    total = 0\n    for row in rows:\n        total += row.amount * rate\n    return total\n";
        let b = "def render(template, ctx):\n    out = []\n    for key in sorted(ctx):\n        out.append(template.format(key, ctx[key]))\n    return \"\\n\".join(out)\n";
        let summary = summarize(&[("a.py", a.to_string()), ("b.py", b.to_string())]);
        assert!(
            summary.groups.is_empty(),
            "unrelated functions were grouped: {:?}",
            summary.groups
        );
        assert!(
            summary.signed_symbols >= 2,
            "the negative result is vacuous: only {} symbols were signed",
            summary.signed_symbols
        );
    }

    /// "No clones" and "nothing was examined" must not look the same.
    #[test]
    fn an_empty_report_still_carries_its_denominator() {
        let summary = summarize(&[]);
        assert!(summary.groups.is_empty());
        assert_eq!(summary.signed_symbols, 0);
        assert_eq!(summary.unsigned_symbols, 0);

        let trivial = "def name(self):\n    return self._name\n";
        let with_unsigned = summarize(&[("a.py", trivial.to_string())]);
        assert!(with_unsigned.groups.is_empty());
        assert_eq!(
            with_unsigned.signed_symbols, 0,
            "an accessor was signed despite the floor"
        );
        assert!(
            with_unsigned.unsigned_symbols > 0,
            "a file with symbols reported none unsigned; the denominator is wrong"
        );
    }

    /// Two structs with the same field count are not a finding.
    #[test]
    fn type_declarations_are_not_compared_structurally() {
        // Large enough to clear the size floor, so the exclusion under test is
        // the kind rule and not the floor.
        let fields = "\tPath string\n\tLine int\n\tText string\n\tScore float64\n\tKind string\n\tOwner string\n\tDepth int\n\tRatio float64\n\tLabel string\n\tCount int\n\tFlags int\n\tNote string\n";
        let a = format!("package p\n\ntype Match struct {{\n{fields}}}\n");
        let b = format!("package p\n\ntype Skipped struct {{\n{fields}}}\n");
        let summary = summarize(&[("a.go", a), ("b.go", b)]);
        // Non-vacuity: the exclusion must be the kind rule, not the size floor.
        // If these structs were never signed, the assertion below would hold
        // for a reason that has nothing to do with what it claims to test.
        assert_eq!(
            summary.signed_symbols, 2,
            "both structs must carry signatures for this test to mean anything"
        );
        assert!(
            summary
                .groups
                .iter()
                .all(|g| g.kind != CloneKind::Structural),
            "two unrelated structs were reported as the same shape: {:?}",
            summary.groups
        );
    }

    #[test]
    fn groups_are_ordered_by_weight_and_are_build_stable() {
        let small = format!("def compute(rows, rate):{BODY}");
        let large = format!(
            "def wide(rows, rate):{}",
            BODY.replace(
                "return total",
                "for extra in rows:\n        total += extra.bonus * rate\n    return total"
            )
        );
        let files = vec![
            ("a.py", small.clone()),
            ("b.py", small.clone()),
            ("c.py", large.clone()),
            ("d.py", large.clone()),
        ];
        let first = summarize(&files);
        assert_eq!(first.groups.len(), 2);
        assert!(
            first.groups[0].min_nodes > first.groups[1].min_nodes,
            "groups are not ordered by body weight: {:?}",
            first.groups.iter().map(|g| g.min_nodes).collect::<Vec<_>>()
        );

        // Same tree, files walked in the opposite order: identical report.
        let mut reversed = files.clone();
        reversed.reverse();
        let second = summarize(&reversed);
        assert_eq!(
            serde_json::to_string(&first.groups).unwrap(),
            serde_json::to_string(&second.groups).unwrap(),
            "walk order changed the clone report"
        );
    }
}
