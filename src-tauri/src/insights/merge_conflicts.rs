//! Would the lanes' committed work merge? The pairwise `merge-tree` read behind
//! `gitpulse_merge_conflicts`.
//!
//! Collision detection reads `git status`, so a lane's files leave every scan
//! the moment it commits, which is the normal end state of a lane. This asks
//! the question that survives a commit: for each pair of checkouts, would the
//! two HEAD commits merge, and if not, in which paths. The merge is computed by
//! [`crate::engine::object_quarantine`], which writes its objects to a
//! scratch directory that is deleted afterwards, so the read leaves the
//! repository's object database as it found it.
//!
//! Three states per pair, and the third is not a flavour of the first: a pair
//! whose merge could not be computed is `unknown` with its reason, never
//! `clean`.

use crate::engine::object_quarantine::{merge_tree_resolved, refuse_partial_clone, MergeCandidate};
use crate::engine::validate_repo;
use crate::engine::worktree::{self, WorktreeInfo};
use serde::Serialize;
use std::time::{Duration, Instant};

/// Most pairs one call merges. Pairs grow with the square of the worktrees (64
/// worktrees are 2,016), and each costs a process or more: past this the call
/// says so (`ok: false`, `truncated: true`) and reports the totals.
pub const MAX_MERGE_PAIRS: usize = 64;

/// Most worktrees one call considers, the snapshot's own cap.
const MAX_MERGE_WORKTREES: usize = 64;

/// Wall-clock for the whole call, and for one pair's merge inside it.
const TOTAL_BUDGET: Duration = Duration::from_secs(60);
const PAIR_BUDGET: Duration = Duration::from_secs(20);

/// One pair of checkouts and what merging their HEADs would do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PairVerdict {
    pub a_path: String,
    pub b_path: String,
    pub a_head: String,
    pub b_head: String,
    /// `state` is `clean` (the merged tree exists only in the quarantine that
    /// has been deleted), `conflict` (with `paths` and `paths_total`) or
    /// `unknown` (with `reason`).
    pub verdict: MergeCandidate,
}

/// What `gitpulse_merge_conflicts` returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MergeConflicts {
    /// False when the answer is incomplete or unavailable: past the pair cap,
    /// a partial clone, a listing that failed. `error` says why. A true `ok`
    /// means every pair counted in `pairs_total` was merged.
    pub ok: bool,
    pub error: String,
    /// Checkouts with a HEAD that were considered.
    pub worktrees: u32,
    /// Pairs of those whose HEADs differ: the pairs a merge can say anything
    /// about.
    pub pairs_total: u32,
    /// How many of those were merged (and are in `items`).
    pub pairs_checked: u32,
    /// Pairs of checkouts on the very same commit, which need no merge.
    pub same_head_pairs: u32,
    pub clean: u32,
    pub conflict: u32,
    pub unknown: u32,
    /// True when `pairs_checked < pairs_total`, or the listing was cut at the
    /// worktree cap: `items` and the counts are a floor, not the whole.
    pub truncated: bool,
    pub items: Vec<PairVerdict>,
}

impl MergeConflicts {
    fn failed(error: String) -> Self {
        Self {
            ok: false,
            error,
            worktrees: 0,
            pairs_total: 0,
            pairs_checked: 0,
            same_head_pairs: 0,
            clean: 0,
            conflict: 0,
            unknown: 0,
            truncated: false,
            items: Vec::new(),
        }
    }
}

/// Every pair of the repository's checkouts, merged in a quarantine.
pub fn merge_conflicts(repo_path: &str) -> MergeConflicts {
    merge_conflicts_within(repo_path, MAX_MERGE_PAIRS, TOTAL_BUDGET)
}

/// [`merge_conflicts`] with the pair cap and the wall-clock as arguments, so
/// the cap and the deadline are reachable in a test without sixty-five
/// worktrees or a minute of waiting.
pub(crate) fn merge_conflicts_within(
    repo_path: &str,
    max_pairs: usize,
    budget: Duration,
) -> MergeConflicts {
    let started = Instant::now();
    let overall = started.checked_add(budget).unwrap_or(started);
    let repo = match validate_repo(repo_path) {
        Ok(repo) => repo,
        Err(error) => return MergeConflicts::failed(error),
    };
    let listing = match worktree::list_worktrees_lite(repo_path) {
        Ok(listing) => listing,
        Err(error) => return MergeConflicts::failed(error),
    };
    // Told once for the call: a merge in a partial clone would make git fetch
    // blobs, and every pair would say so separately.
    if let Err(reason) = refuse_partial_clone(&repo) {
        return MergeConflicts::failed(reason);
    }
    let cut_listing = listing.len() > MAX_MERGE_WORKTREES;
    let lanes: Vec<&WorktreeInfo> = listing
        .iter()
        .take(MAX_MERGE_WORKTREES)
        .filter(|info| !info.is_bare && crate::engine::git_writer::is_full_oid(&info.head))
        .collect();

    let mut out = MergeConflicts::failed(String::new());
    out.ok = true;
    out.worktrees = lanes.len() as u32;
    let mut pairs: Vec<(&WorktreeInfo, &WorktreeInfo)> = Vec::new();
    for (index, a) in lanes.iter().enumerate() {
        for b in &lanes[index + 1..] {
            if a.head == b.head {
                out.same_head_pairs += 1;
            } else {
                pairs.push((a, b));
            }
        }
    }
    out.pairs_total = pairs.len() as u32;
    for (a, b) in pairs.iter().take(max_pairs) {
        let pair_deadline = Instant::now()
            .checked_add(PAIR_BUDGET)
            .unwrap_or(overall)
            .min(overall);
        // `a` is the base side and `b` the incoming side; the merge is
        // symmetric in what it says conflicts.
        let verdict = merge_tree_resolved(&repo, &a.head, &b.head, pair_deadline);
        match &verdict {
            MergeCandidate::Clean { .. } => out.clean += 1,
            MergeCandidate::Conflict { .. } => out.conflict += 1,
            MergeCandidate::Unknown { .. } => out.unknown += 1,
        }
        out.items.push(PairVerdict {
            a_path: a.path.clone(),
            b_path: b.path.clone(),
            a_head: a.head.clone(),
            b_head: b.head.clone(),
            verdict,
        });
    }
    out.pairs_checked = out.items.len() as u32;
    let capped = out.pairs_checked < out.pairs_total;
    out.truncated = capped || cut_listing;
    if capped {
        out.ok = false;
        out.error = format!(
            "{} pairs of checkouts exceed the cap of {max_pairs}: only the first {} were \
             merged, so the counts are a floor",
            out.pairs_total, out.pairs_checked
        );
    } else if cut_listing {
        out.ok = false;
        out.error = format!(
            "the repository has {} worktrees and only the first {MAX_MERGE_WORKTREES} were considered",
            listing.len()
        );
    } else if out.unknown > 0 {
        // `ok` is true only when every checked pair was merged to a clean or
        // conflicting answer. When every unknown pair is the spawn gate
        // deferring under load, the error is made of those sentences alone, so
        // `insights::mark_deferrals` adds `deferred` and `retry_after_ms`;
        // any other reason keeps its own words and is never made retryable.
        let mut reasons: Vec<&str> = Vec::new();
        for item in &out.items {
            if let MergeCandidate::Unknown { reason } = &item.verdict {
                if !reasons.contains(&reason.as_str()) {
                    reasons.push(reason);
                }
            }
        }
        out.ok = false;
        out.error = if reasons
            .iter()
            .all(|reason| crate::engine::git_cli::is_deferred_under_load(reason))
        {
            reasons
                .iter()
                .take(3)
                .copied()
                .collect::<Vec<_>>()
                .join("; ")
        } else {
            format!(
                "{} of {} pairs could not be merged (unknown): {}",
                out.unknown,
                out.pairs_checked,
                reasons.first().copied().unwrap_or_default()
            )
        };
    }
    out
}

#[cfg(test)]
#[path = "merge_conflicts_tests.rs"]
mod tests;
