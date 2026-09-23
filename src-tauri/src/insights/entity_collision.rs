//! Symbol classification for one already-reported overlapping path.
//!
//! The porcelain collision scan stays path-only and cheap. This module runs
//! *after* that scan names a path, and only then: each worktree's old-side
//! diff line ranges are joined to DevMap symbol spans when that worktree's
//! index `head_sha` equals its `HEAD`. Disjoint symbols are a distinct notice,
//! not a merge promise. A mismatch, missing store, or untracked file stays
//! today's file-level notice plus why.

use crate::codeintel::{self, CodeintelFileSymbol};
use crate::engine::git_cli::git_text;
use crate::engine::git_reader::GitReader;
use crate::engine::validate_repo;
use crate::insights::{CollisionItem, CollisionParty};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

/// How the symbol join classified one overlapping path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityCollisionKind {
    /// At least one symbol appears on both sides' old-side ranges.
    SharedSymbol,
    /// Both sides resolved symbols and none overlap — file overlap only.
    DisjointSymbols,
    /// Index / diff / trackability could not support a symbol join.
    FileLevel,
}

/// Verdict for one overlapping path the porcelain scan already named.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntityCollisionVerdict {
    pub path: String,
    pub kind: EntityCollisionKind,
    /// Human-readable explanation. Always set — including for shared symbols.
    pub reason: String,
    /// Symbol names shared across parties when `kind` is [`SharedSymbol`].
    pub shared_symbols: Vec<String>,
}

/// Inclusive line range on the old (HEAD) side of a unified diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

impl LineRange {
    pub fn overlaps_symbol(&self, symbol: &CodeintelFileSymbol) -> bool {
        if symbol.span_start_line == 0 || symbol.span_end_line == 0 {
            return false;
        }
        self.start <= symbol.span_end_line && symbol.span_start_line <= self.end
    }
}

/// Symbols whose spans overlap any of the old-side ranges.
pub fn symbols_touching_ranges(
    symbols: &[CodeintelFileSymbol],
    ranges: &[LineRange],
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for symbol in symbols {
        if ranges.iter().any(|range| range.overlaps_symbol(symbol)) {
            out.insert(symbol.symbol_name.clone());
        }
    }
    out
}

/// Pure classifier over already-gathered per-party symbol sets.
///
/// `failures` are reasons a party could not be joined; any failure forces
/// [`EntityCollisionKind::FileLevel`] so a stale index never borrows the
/// disjoint-symbol wording.
pub fn classify_party_symbols(
    path: &str,
    party_symbols: &[BTreeSet<String>],
    failures: &[String],
) -> EntityCollisionVerdict {
    if !failures.is_empty() {
        return EntityCollisionVerdict {
            path: path.to_string(),
            kind: EntityCollisionKind::FileLevel,
            reason: failures.join("; "),
            shared_symbols: Vec::new(),
        };
    }
    if party_symbols.len() < 2 {
        return EntityCollisionVerdict {
            path: path.to_string(),
            kind: EntityCollisionKind::FileLevel,
            reason: "fewer than two worktrees produced symbol sets for this path".into(),
            shared_symbols: Vec::new(),
        };
    }

    let mut shared = party_symbols[0].clone();
    for set in &party_symbols[1..] {
        shared = shared.intersection(set).cloned().collect();
    }
    if shared.is_empty() {
        EntityCollisionVerdict {
            path: path.to_string(),
            kind: EntityCollisionKind::DisjointSymbols,
            reason: format!(
                "{path} is dirty in multiple worktrees, but no shared symbol was seen on the old-side ranges — file overlap, not a merge promise"
            ),
            shared_symbols: Vec::new(),
        }
    } else {
        let names: Vec<String> = shared.into_iter().collect();
        EntityCollisionVerdict {
            path: path.to_string(),
            kind: EntityCollisionKind::SharedSymbol,
            reason: format!(
                "{path} shares symbol(s) {} across worktrees; editing it here will conflict when these branches meet",
                names.join(", ")
            ),
            shared_symbols: names,
        }
    }
}

/// Classify one overlapping path already reported by the porcelain scan.
///
/// Opens each party's own DevMap store and diffs that worktree against its
/// own HEAD. Does not run inside [`super::collision_from_list`].
pub fn classify_overlapping_path(item: &CollisionItem) -> EntityCollisionVerdict {
    let mut party_symbols = Vec::new();
    let mut failures = Vec::new();

    for party in &item.worktrees {
        match party_old_side_symbols(&item.path, party) {
            Ok(symbols) => party_symbols.push(symbols),
            Err(reason) => failures.push(format!("{}: {reason}", short_party(party))),
        }
    }

    classify_party_symbols(&item.path, &party_symbols, &failures)
}

fn short_party(party: &CollisionParty) -> String {
    Path::new(&party.path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(party.path.as_str())
        .to_string()
}

fn party_old_side_symbols(
    file_path: &str,
    party: &CollisionParty,
) -> Result<BTreeSet<String>, String> {
    let repo = validate_repo(&party.path)?;
    let repo_str = repo.to_string_lossy();

    if !GitReader::is_tracked(&repo_str, file_path).unwrap_or(false) {
        return Err("file is untracked (no HEAD blob for an old-side join)".into());
    }

    let head = GitReader::head_id(&repo_str)?;
    let spans = codeintel::symbols_for_file(&repo_str, file_path, &head);
    if !spans.available {
        return Err(spans
            .reason
            .unwrap_or_else(|| "DevMap store unavailable".into()));
    }
    if !spans.head_matches {
        return Err(spans
            .reason
            .unwrap_or_else(|| format!("index head_sha does not equal HEAD ({head})")));
    }
    if spans.items.iter().any(|s| s.span_start_line == 0) {
        return Err(spans.reason.unwrap_or_else(|| {
            "symbol line spans could not be computed from the indexed blob".into()
        }));
    }

    let ranges = old_side_ranges(&repo, file_path)?;
    if ranges.is_empty() {
        // Pure additions (or an empty diff) have no old-side lines to join.
        // That is not evidence of a clean symbol split — stay at file level.
        return Err(
            "no old-side line ranges in the worktree diff (additions-only or empty)".into(),
        );
    }

    Ok(symbols_touching_ranges(&spans.items, &ranges))
}

/// Old-side line ranges from unstaged and staged diffs against HEAD.
fn old_side_ranges(repo: &Path, file_path: &str) -> Result<Vec<LineRange>, String> {
    // `:(literal)` stops `*?[` in a filename from widening the pathspec.
    let spec = format!(":(literal){file_path}");
    let mut ranges = Vec::new();
    for args in [
        vec![
            "-c",
            "core.quotepath=off",
            "diff",
            "--unified=0",
            "--",
            spec.as_str(),
        ],
        vec![
            "-c",
            "core.quotepath=off",
            "diff",
            "--cached",
            "--unified=0",
            "--",
            spec.as_str(),
        ],
    ] {
        let text = git_text(repo, &args)?;
        ranges.extend(parse_old_side_ranges(&text));
    }
    ranges.sort_by_key(|r| (r.start, r.end));
    ranges.dedup();
    Ok(ranges)
}

/// Parse `@@ -old_start,old_lines +… @@` headers into inclusive old-side ranges.
pub fn parse_old_side_ranges(diff: &str) -> Vec<LineRange> {
    let mut ranges = Vec::new();
    for line in diff.lines() {
        let Some(rest) = line.strip_prefix("@@ ") else {
            continue;
        };
        let Some(old) = rest.split_whitespace().next() else {
            continue;
        };
        let Some(old) = old.strip_prefix('-') else {
            continue;
        };
        let (start_s, count_s) = match old.split_once(',') {
            Some((s, c)) => (s, c),
            None => (old, "1"),
        };
        let Ok(start) = start_s.parse::<u32>() else {
            continue;
        };
        let Ok(count) = count_s.parse::<u32>() else {
            continue;
        };
        if count == 0 || start == 0 {
            // Pure addition: no old-side lines.
            continue;
        }
        ranges.push(LineRange {
            start,
            end: start.saturating_add(count).saturating_sub(1),
        });
    }
    ranges
}

/// Attach a symbol verdict to the first overlapping path only.
pub fn enrich_first_item(
    mut risk: crate::insights::CollisionRisk,
) -> crate::insights::CollisionRisk {
    if let Some(item) = risk.items.first().cloned() {
        let verdict = classify_overlapping_path(&item);
        if let Some(first) = risk.items.first_mut() {
            first.entity = Some(verdict);
        }
    }
    risk
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(name: &str, start: u32, end: u32) -> CodeintelFileSymbol {
        CodeintelFileSymbol {
            symbol_name: name.into(),
            qualified_name: name.into(),
            kind: "Function".into(),
            span_start_line: start,
            span_end_line: end,
        }
    }

    #[test]
    fn non_overlapping_spans_do_not_share_symbols() {
        let left =
            symbols_touching_ranges(&[sym("alpha", 1, 10)], &[LineRange { start: 1, end: 5 }]);
        let right =
            symbols_touching_ranges(&[sym("beta", 20, 30)], &[LineRange { start: 20, end: 25 }]);
        let verdict = classify_party_symbols("src/x.rs", &[left, right], &[]);
        assert_eq!(verdict.kind, EntityCollisionKind::DisjointSymbols);
        assert!(
            !verdict
                .reason
                .contains("will conflict when these branches meet"),
            "disjoint must not use the shared-symbol collision wording: {}",
            verdict.reason
        );
        assert!(
            verdict.reason.contains("not a merge promise"),
            "{}",
            verdict.reason
        );
    }

    #[test]
    fn overlapping_spans_share_a_symbol() {
        let left =
            symbols_touching_ranges(&[sym("shared", 1, 20)], &[LineRange { start: 5, end: 8 }]);
        let right =
            symbols_touching_ranges(&[sym("shared", 1, 20)], &[LineRange { start: 10, end: 12 }]);
        let verdict = classify_party_symbols("src/x.rs", &[left, right], &[]);
        assert_eq!(verdict.kind, EntityCollisionKind::SharedSymbol);
        assert_eq!(verdict.shared_symbols, vec!["shared".to_string()]);
        assert!(
            verdict
                .reason
                .contains("will conflict when these branches meet"),
            "{}",
            verdict.reason
        );
    }

    #[test]
    fn head_sha_mismatch_failure_stays_file_level_not_disjoint() {
        let left = BTreeSet::from(["alpha".into()]);
        let right = BTreeSet::from(["beta".into()]);
        let verdict = classify_party_symbols(
            "src/x.rs",
            &[left, right],
            &["wt-a: indexed generation head_sha abc is not the requested def".into()],
        );
        assert_eq!(verdict.kind, EntityCollisionKind::FileLevel);
        assert!(
            !verdict.reason.contains("not a merge promise"),
            "a failed join must not use the disjoint-symbol wording: {}",
            verdict.reason
        );
        assert!(
            verdict.reason.contains("head_sha"),
            "file-level reason should carry the mismatch: {}",
            verdict.reason
        );
    }

    #[test]
    fn parse_old_side_ranges_skips_pure_additions() {
        let diff = "\
@@ -10,2 +10,3 @@
-old
 context
+new
@@ -0,0 +1,2 @@
+brand
+new
";
        let ranges = parse_old_side_ranges(diff);
        assert_eq!(ranges, vec![LineRange { start: 10, end: 11 }]);
    }
}
