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

use crate::engine::git_cli::git_captured_with_stdin_timeout;
use crate::engine::git_reader::default_base;
use crate::engine::object_quarantine::{refuse_partial_clone, MergeCandidate, ObjectQuarantine};
use crate::engine::validate_repo;
use crate::engine::worktree::{self, WorktreeInfo};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

/// Most pairs one call merges. Pairs grow with the square of the worktrees (64
/// worktrees are 2,016), and each costs a process or more: past this the call
/// says so (`ok: false`, `truncated: true`) and reports the totals.
pub const MAX_MERGE_PAIRS: usize = 64;

/// Most worktrees one call considers, the snapshot's own cap.
const MAX_MERGE_WORKTREES: usize = 64;

/// Wall-clock for the whole call, and for one pair's merge inside it.
const TOTAL_BUDGET: Duration = Duration::from_secs(25);
const PAIR_BUDGET: Duration = Duration::from_secs(5);

/// Cap per lane for `diff-tree --name-only` path sets used to rank likely conflicts.
const MAX_DIFF_PATHS_PER_LANE: usize = 2048;

/// Cap on time spent computing the `diff-tree` prioritization heuristic.
const DIFF_TREE_HEURISTIC_BUDGET: Duration = Duration::from_secs(2);

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
    let repo = match validate_repo(repo_path) {
        Ok(repo) => repo,
        Err(error) => return MergeConflicts::failed(error.into_message()),
    };
    let listing = match worktree::list_worktrees_lite(repo_path) {
        Ok(listing) => listing,
        Err(error) => return MergeConflicts::failed(error.into_message()),
    };
    let refs: Vec<&WorktreeInfo> = listing.iter().collect();
    merge_conflicts_from_lanes(&repo, &refs, None, max_pairs, budget)
}

/// Computes pairwise committed merge conflicts across `lanes` inside `repo`
/// using one [`ObjectQuarantine::open_stream`].
///
/// Prioritizes `(default_base, lane_i)` pairs first, then active/dirty lanes,
/// using `git diff-tree -r --name-only` intersection to rank likely-conflicting
/// pairs into `max_pairs`. Skipped pairs are never marked `Clean`; `truncated`
/// is set when `pairs_checked < pairs_total` or `lanes.len() > MAX_MERGE_WORKTREES`.
pub fn merge_conflicts_from_lanes(
    repo: &Path,
    lanes: &[&WorktreeInfo],
    active_paths: Option<&HashSet<String>>,
    max_pairs: usize,
    budget: Duration,
) -> MergeConflicts {
    let started = Instant::now();
    let overall = started.checked_add(budget).unwrap_or(started);
    // Told once for the call: a merge in a partial clone would make git fetch
    // blobs, and every pair would say so separately.
    if let Err(reason) = refuse_partial_clone(repo) {
        return MergeConflicts::failed(reason.into_message());
    }
    let cut_listing = lanes.len() > MAX_MERGE_WORKTREES;
    let valid_lanes: Vec<&WorktreeInfo> = lanes
        .iter()
        .copied()
        .take(MAX_MERGE_WORKTREES)
        .filter(|info| !info.is_bare && crate::engine::git_writer::is_full_oid(&info.head))
        .collect();

    let mut out = MergeConflicts::failed(String::new());
    out.ok = true;
    out.worktrees = valid_lanes.len() as u32;
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for (index, a) in valid_lanes.iter().enumerate() {
        for (offset, b) in valid_lanes[index + 1..].iter().enumerate() {
            if a.head == b.head {
                out.same_head_pairs += 1;
            } else {
                pairs.push((index, index + 1 + offset));
            }
        }
    }
    out.pairs_total = pairs.len() as u32;

    if !pairs.is_empty() && max_pairs > 0 && Instant::now() < overall {
        prioritize_pairs(
            repo,
            &valid_lanes,
            active_paths,
            max_pairs,
            overall,
            &mut pairs,
        );
    }

    let selected: Vec<(usize, usize)> = pairs.into_iter().take(max_pairs).collect();
    if !selected.is_empty() {
        match ObjectQuarantine::open_stream(repo, overall) {
            Ok(mut stream) => {
                for (ai, bi) in selected {
                    let a = valid_lanes[ai];
                    let b = valid_lanes[bi];
                    let pair_deadline = Instant::now()
                        .checked_add(PAIR_BUDGET)
                        .unwrap_or(overall)
                        .min(overall);
                    let verdict = stream.merge_candidate_until(&a.head, &b.head, pair_deadline);
                    record_verdict(&mut out, a, b, verdict);
                }
            }
            Err(err) => {
                let reason = err.to_string();
                for (ai, bi) in selected {
                    let a = valid_lanes[ai];
                    let b = valid_lanes[bi];
                    record_verdict(
                        &mut out,
                        a,
                        b,
                        MergeCandidate::Unknown {
                            reason: reason.clone(),
                        },
                    );
                }
            }
        }
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
            lanes.len()
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
            .all(|reason| crate::engine::process::is_deferred_under_load(reason))
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

fn record_verdict(
    out: &mut MergeConflicts,
    a: &WorktreeInfo,
    b: &WorktreeInfo,
    verdict: MergeCandidate,
) {
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

fn prioritize_pairs(
    repo: &Path,
    lanes: &[&WorktreeInfo],
    active_paths: Option<&HashSet<String>>,
    max_pairs: usize,
    overall: Instant,
    pairs: &mut [(usize, usize)],
) {
    let base = default_base(repo).base;
    let is_default: Vec<bool> = lanes
        .iter()
        .map(|w| match &base {
            Some((name, oid)) => w.branch.as_deref() == Some(name.as_str()) || w.head == *oid,
            None => w.is_main,
        })
        .collect();
    let is_active: Vec<bool> = lanes
        .iter()
        .map(|w| {
            w.dirty_files.is_some_and(|n| n > 0)
                || active_paths.is_some_and(|set| set.contains(&w.path))
        })
        .collect();

    let changed_by_head = if pairs.len() > max_pairs {
        let base_oid = base
            .as_ref()
            .map(|(_, oid)| oid.as_str())
            .or_else(|| lanes.iter().find(|w| w.is_main).map(|w| w.head.as_str()));
        collect_diff_tree_paths(repo, lanes, base_oid, overall)
    } else {
        HashMap::new()
    };

    let lane_paths: Vec<Option<&HashSet<String>>> =
        lanes.iter().map(|w| changed_by_head.get(&w.head)).collect();

    pairs.sort_by_key(|&(i, j)| {
        let involves_default = is_default[i] || is_default[j];
        let active_count = u8::from(is_active[i]) + u8::from(is_active[j]);
        let overlap = match (lane_paths[i], lane_paths[j]) {
            (Some(a), Some(b)) => {
                let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
                small.iter().filter(|p| large.contains(*p)).count()
            }
            _ => 0,
        };
        (
            u8::from(!involves_default),
            2u8.saturating_sub(active_count),
            usize::MAX - overlap,
            i,
            j,
        )
    });
}

/// Runs one `git diff-tree --stdin -r --name-only -z` to map each lane's HEAD
/// OID to the paths it changed relative to `base_oid` (best-effort ordering
/// heuristic; any failure returns an empty map).
fn collect_diff_tree_paths(
    repo: &Path,
    lanes: &[&WorktreeInfo],
    base_oid: Option<&str>,
    overall: Instant,
) -> HashMap<String, HashSet<String>> {
    let remaining = overall
        .saturating_duration_since(Instant::now())
        .min(DIFF_TREE_HEURISTIC_BUDGET);
    if remaining.is_zero() {
        return HashMap::new();
    }
    let mut asked = HashSet::new();
    let mut stdin = String::new();
    for lane in lanes {
        if let Some(base) = base_oid {
            if lane.head == base {
                continue;
            }
        }
        if asked.insert(lane.head.clone()) {
            match base_oid {
                Some(base) => {
                    stdin.push_str(&lane.head);
                    stdin.push(' ');
                    stdin.push_str(base);
                    stdin.push('\n');
                }
                None => {
                    stdin.push_str(&lane.head);
                    stdin.push('\n');
                }
            }
        }
    }
    if asked.is_empty() {
        return HashMap::new();
    }
    let args: &[&str] = if base_oid.is_some() {
        &["diff-tree", "--stdin", "-r", "--name-only", "-z"]
    } else {
        &["diff-tree", "--stdin", "--root", "-r", "--name-only", "-z"]
    };
    let Ok(run) = git_captured_with_stdin_timeout(repo, args, stdin.as_bytes(), remaining) else {
        return HashMap::new();
    };
    if !run.success || run.incomplete.is_some() {
        return HashMap::new();
    }
    let mut out: HashMap<String, HashSet<String>> = HashMap::new();
    let mut current: Option<String> = None;
    for token in run.stdout.split(|&b| b == 0) {
        if token.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(token);
        if asked.contains(text.as_ref()) {
            current = Some(text.into_owned());
            continue;
        }
        if let Some(head) = &current {
            let set = out.entry(head.clone()).or_default();
            if set.len() < MAX_DIFF_PATHS_PER_LANE {
                set.insert(text.into_owned());
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "merge_conflicts_tests.rs"]
mod tests;

#[cfg(test)]
mod stream_tests {
    use super::*;
    use crate::test_support::{git_in, git_repo, write};
    use std::path::PathBuf;

    #[test]
    fn fifty_worktrees_complete_under_5s_prioritize_default_base_and_truncate() {
        let main = git_repo();
        let root = main.path().canonicalize().unwrap();
        write(&root, "seed.txt", "base\n");
        git_in(&root, &["add", "seed.txt"]);
        git_in(&root, &["commit", "-q", "-m", "seed"]);
        let base_oid = crate::engine::git_cli::git_text(&root, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();

        // Synthetic 50-worktree fixture (1 main on default_base + 49 distinct feature lanes)
        // exercising `merge_conflicts_from_lanes` without spending 49x `git worktree add`
        // setup seconds in the test harness.
        let main_info = WorktreeInfo {
            path: root.to_string_lossy().into_owned(),
            name: "main".into(),
            head: base_oid.clone(),
            branch: Some("main".into()),
            is_bare: false,
            is_detached: false,
            is_main: true,
            is_locked: false,
            is_prunable: false,
            dirty_files: Some(0),
            diff_stat: None,
            main_divergence: None,
            active_routes: Vec::new(),
            scan_note: None,
            status_read: worktree::StatusRead::NotRead,
        };
        let mut storage = vec![main_info];
        let base_tree = crate::engine::git_cli::git_text(&root, &["rev-parse", "HEAD^{tree}"])
            .unwrap()
            .trim()
            .to_string();
        for i in 0..49 {
            let branch = format!("lane-{i:02}");
            let msg = format!("commit {branch}");
            let tip = crate::engine::git_cli::git_text(
                &root,
                &["commit-tree", &base_tree, "-p", &base_oid, "-m", &msg],
            )
            .unwrap()
            .trim()
            .to_string();
            git_in(
                &root,
                &["update-ref", &format!("refs/heads/{branch}"), &tip],
            );
            storage.push(WorktreeInfo {
                path: PathBuf::from(&root)
                    .join(format!(".lanes/lane-{i:02}"))
                    .to_string_lossy()
                    .into_owned(),
                name: branch.clone(),
                head: tip,
                branch: Some(branch),
                is_bare: false,
                is_detached: false,
                is_main: false,
                is_locked: false,
                is_prunable: false,
                dirty_files: Some(usize::from(i < 5)),
                diff_stat: None,
                main_divergence: None,
                active_routes: Vec::new(),
                scan_note: None,
                status_read: worktree::StatusRead::NotRead,
            });
        }
        let refs: Vec<&WorktreeInfo> = storage.iter().collect();
        let started = Instant::now();
        let report = merge_conflicts_from_lanes(&root, &refs, None, MAX_MERGE_PAIRS, TOTAL_BUDGET);
        let took = started.elapsed();

        assert!(
            took < Duration::from_secs(5),
            "50-worktree conflict check took {took:?} (>= 5s)"
        );
        assert!(report.truncated, "expected truncated: true, got {report:?}");
        assert!(!report.ok, "truncated call must report ok: false");
        assert_eq!(report.worktrees, 50);
        assert_eq!(report.pairs_total, 50 * 49 / 2);
        assert_eq!(report.pairs_checked, MAX_MERGE_PAIRS as u32);
        // All 49 `(default_base, lane_i)` pairs MUST appear first in `items`.
        let main_path = storage[0].path.as_str();
        for (idx, item) in report.items[..49].iter().enumerate() {
            assert!(
                item.a_path == main_path || item.b_path == main_path,
                "item {idx} was not a default_base pair: {item:?}"
            );
        }
    }

    #[test]
    fn diff_tree_intersection_and_active_lanes_prioritize_conflicting_pairs() {
        let main = git_repo();
        let root = main.path().canonicalize().unwrap();
        write(&root, "shared.txt", "base\n");
        write(&root, "other.txt", "base\n");
        git_in(&root, &["add", "."]);
        git_in(&root, &["commit", "-q", "-m", "base"]);
        let base_oid = crate::engine::git_cli::git_text(&root, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();

        let make_lane = |name: &str, file: &str, content: &str| {
            git_in(&root, &["checkout", "-q", "-b", name, &base_oid]);
            write(&root, file, content);
            git_in(&root, &["add", "."]);
            git_in(&root, &["commit", "-q", "-m", name]);
            let tip = crate::engine::git_cli::git_text(&root, &["rev-parse", "HEAD"])
                .unwrap()
                .trim()
                .to_string();
            git_in(&root, &["checkout", "-q", "main"]);
            WorktreeInfo {
                path: PathBuf::from(&root)
                    .join(format!(".lanes/{name}"))
                    .to_string_lossy()
                    .into_owned(),
                name: name.into(),
                head: tip,
                branch: Some(name.into()),
                is_bare: false,
                is_detached: false,
                is_main: false,
                is_locked: false,
                is_prunable: false,
                dirty_files: Some(0),
                diff_stat: None,
                main_divergence: None,
                active_routes: Vec::new(),
                scan_note: None,
                status_read: worktree::StatusRead::NotRead,
            }
        };

        // 4 non-default lanes: lane-a and lane-c both touch `shared.txt` (conflict!),
        // while lane-b touches `other.txt`. With max_pairs=1 and no active/dirty flag,
        // `diff-tree` intersection must prioritize `(lane-a, lane-c)` into the 1-pair cap.
        let lane_a = make_lane("lane-a", "shared.txt", "from a\n");
        let lane_b = make_lane("lane-b", "other.txt", "from b\n");
        let lane_c = make_lane("lane-c", "shared.txt", "from c\n");
        let refs = [&lane_a, &lane_b, &lane_c];

        let report = merge_conflicts_from_lanes(&root, &refs, None, 1, TOTAL_BUDGET);
        assert!(report.truncated);
        assert_eq!(report.pairs_checked, 1);
        assert_eq!(
            report.conflict, 1,
            "expected conflict pair first: {report:?}"
        );
    }
}
