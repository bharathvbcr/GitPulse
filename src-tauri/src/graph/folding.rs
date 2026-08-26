use super::lane_solver::RawCommitNode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Represents a folded branch run where linear feature branch commits are compressed into a summary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FoldedBranchRun {
    pub merge_commit_id: String,
    pub branch_root_id: String,
    pub folded_commit_ids: Vec<String>,
    pub commit_count: usize,
    pub is_collapsed: bool,
}

/// Hard bound on walk steps per merge during fold identification. A healthy
/// fold run is far shorter; hitting this means pathological lineage depth,
/// which is left unfolded instead of paid on every graph load.
const MAX_FOLD_WALK: usize = 50_000;

/// Identifies subgraphs where a branch diverged and subsequently merged back into the mainline.
pub struct BranchFoldingEngine {
    folded_runs: HashMap<String, FoldedBranchRun>,
}

impl Default for BranchFoldingEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// How far up the mainline the fork-point probe may search when the chain's
/// terminating parent does not equal the merge's first parent outright.
const FOLD_ANCESTRY_DEPTH: usize = 64;

fn is_in_ancestry(
    target_id: &str,
    start_id: &str,
    commits: &[RawCommitNode],
    index: &HashMap<&str, usize>,
    max_depth: usize,
) -> bool {
    if target_id == start_id {
        return true;
    }
    let Some(start) = index.get(start_id).copied() else {
        return false;
    };
    let mut queue = vec![start];
    let mut visited: Vec<usize> = Vec::new();
    let mut depth = 0;
    while let Some(curr) = queue.pop() {
        if commits[curr].id == target_id {
            return true;
        }
        if depth >= max_depth || visited.contains(&curr) {
            continue;
        }
        visited.push(curr);
        depth += 1;
        for p in &commits[curr].parent_ids {
            if let Some(&pi) = index.get(p.as_str()) {
                queue.push(pi);
            }
        }
    }
    false
}

impl BranchFoldingEngine {
    pub fn new() -> Self {
        Self {
            folded_runs: HashMap::new(),
        }
    }

    /// For every 2-parent merge the feature parent's ancestry is walked back
    /// until a node whose single parent is the merge's mainline parent or an
    /// ancestor of it within [`FOLD_ANCESTRY_DEPTH`] levels (fold — branches
    /// commonly fork from an older mainline commit, not the merge's immediate
    /// first parent), or until the walk can say no (fork, merge, root, or
    /// history boundary). The walk is a literal mirror of that specification
    /// over integer indices — no per-step `String` clones or `HashSet` churn
    /// like the first implementation — and is hard-bounded by `MAX_FOLD_WALK`
    /// steps per merge so an agent-scale window (hundreds of merges over
    /// 100k-commit lineages) degrades by skipping the deepest runs instead of
    /// stalling the graph load.
    pub fn identify_foldable_runs(&mut self, commits: &[RawCommitNode]) {
        self.folded_runs.clear();
        if commits.is_empty() {
            return;
        }

        let index: HashMap<&str, usize> = commits
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id.as_str(), i))
            .collect();

        for commit in commits {
            if commit.parent_ids.len() != 2 {
                continue;
            }
            let mainline_parent = commit.parent_ids[0].as_str();
            let Some(mut current) = index.get(commit.parent_ids[1].as_str()).copied() else {
                continue;
            };

            let mut chain: Vec<usize> = Vec::new();
            let mut folded = false;
            // Where the run rejoins mainline: the mainline parent itself on
            // the exact-match path, the fork point on the ancestry path.
            let mut fold_root = mainline_parent;
            let mut steps = 0usize;
            loop {
                chain.push(current);
                let parents = &commits[current].parent_ids;
                if parents.len() == 1 {
                    let next_parent = parents[0].as_str();
                    if next_parent == mainline_parent {
                        folded = true;
                        break;
                    }
                    // The chain may rejoin below the immediate first parent:
                    // a branch forked from any ancestor of the mainline
                    // parent still forms a clean diverge-and-rejoin run.
                    if is_in_ancestry(
                        next_parent,
                        mainline_parent,
                        commits,
                        &index,
                        FOLD_ANCESTRY_DEPTH,
                    ) {
                        folded = true;
                        fold_root = next_parent;
                        break;
                    }
                    match index.get(next_parent) {
                        Some(&next) => current = next,
                        None => break, // parent outside the window
                    }
                } else {
                    // Nested merge, fork, or root: not foldable.
                    break;
                }
                steps += 1;
                if steps > MAX_FOLD_WALK {
                    // Bound the worst case: leave this deep run unfoldable
                    // rather than stall the whole solve.
                    folded = false;
                    break;
                }
            }

            if folded && !chain.is_empty() {
                self.folded_runs.insert(
                    commit.id.clone(),
                    FoldedBranchRun {
                        merge_commit_id: commit.id.clone(),
                        branch_root_id: fold_root.to_string(),
                        folded_commit_ids: chain.iter().map(|&i| commits[i].id.clone()).collect(),
                        commit_count: chain.len(),
                        is_collapsed: false,
                    },
                );
            }
        }
    }

    pub fn get_foldable_runs(&self) -> &HashMap<String, FoldedBranchRun> {
        &self.folded_runs
    }

    pub fn toggle_fold(&mut self, merge_commit_id: &str) -> bool {
        if let Some(run) = self.folded_runs.get_mut(merge_commit_id) {
            run.is_collapsed = !run.is_collapsed;
            run.is_collapsed
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node(id: &str, parents: Vec<&str>) -> RawCommitNode {
        RawCommitNode {
            id: id.to_string(),
            parent_ids: parents.into_iter().map(String::from).collect(),
            timestamp: 1000,
            author_name: "Author".to_string(),
            author_email: "author@example.com".to_string(),
            summary: format!("Commit {}", id),
        }
    }

    #[test]
    fn test_identify_foldable_feature_branch() {
        let mut engine = BranchFoldingEngine::new();
        let commits = vec![
            make_node("m", vec!["main1", "feat3"]),
            make_node("feat3", vec!["feat2"]),
            make_node("feat2", vec!["feat1"]),
            make_node("feat1", vec!["main1"]),
            make_node("main1", vec![]),
        ];

        engine.identify_foldable_runs(&commits);
        let runs = engine.get_foldable_runs();
        assert_eq!(runs.len(), 1);
        let run = runs.get("m").unwrap();
        assert_eq!(run.commit_count, 3);
        assert_eq!(run.folded_commit_ids, vec!["feat3", "feat2", "feat1"]);
    }

    /// The old per-merge walk stopped at the FIRST node whose parent equals
    /// the merge's mainline parent, even when the single-parent run continues
    /// deeper. The compressed implementation must cut the run at exactly the
    /// same place: mainline sits mid-chain here (main0 below the fold point
    /// keeps walking legal), so only f2..f4 may fold.
    #[test]
    fn fold_cuts_at_the_first_mainline_match_not_the_chain_tail() {
        let mut engine = BranchFoldingEngine::new();
        // mid(root) <- f1 <- f2 <- f3 <- f4, merged by m. The old walk had to
        // stop at f1 (its parent equals mp), not run to a chain tail.
        let commits = vec![
            make_node("m", vec!["mid", "f4"]),
            make_node("f4", vec!["f3"]),
            make_node("f3", vec!["f2"]),
            make_node("f2", vec!["f1"]),
            make_node("f1", vec!["mid"]),
            make_node("mid", vec![]),
        ];
        engine.identify_foldable_runs(&commits);
        let runs = engine.get_foldable_runs();
        assert_eq!(runs.len(), 1, "one fold for m");
        let run = runs.get("m").unwrap();
        assert_eq!(
            run.folded_commit_ids,
            vec!["f4", "f3", "f2", "f1"],
            "fold spans feature parent down to the node above mid"
        );
        assert_eq!(run.branch_root_id, "mid");
    }

    /// A merge used as someone's feature parent cannot fold FOR THAT MERGE
    /// (the walk pushes it and stops), while the inner merge still folds its
    /// own feature branch independently.
    #[test]
    fn merge_as_feature_parent_blocks_outer_but_inner_still_folds() {
        let mut engine = BranchFoldingEngine::new();
        let commits = vec![
            make_node("top", vec!["base", "inner_merge"]),
            make_node("inner_merge", vec!["base", "side"]),
            make_node("side", vec!["base"]),
            make_node("base", vec![]),
        ];
        engine.identify_foldable_runs(&commits);
        let runs = engine.get_foldable_runs();
        assert!(
            !runs.contains_key("top"),
            "outer merge must not fold through a merge feature parent"
        );
        let inner = runs
            .get("inner_merge")
            .expect("inner merge folds its branch");
        assert_eq!(inner.folded_commit_ids, vec!["side"]);
    }

    /// When the mainline parent is not a row in the window (pruned history),
    /// the fold still lands if the chain terminates on a node whose single
    /// parent is that same id — the old walk compared raw strings.
    #[test]
    fn fold_into_a_mainline_parent_outside_the_window() {
        let mut engine = BranchFoldingEngine::new();
        let commits = vec![
            make_node("m", vec!["0000000000000000000000000000000000000000", "fa"]),
            make_node("fa", vec!["fb"]),
            make_node("fb", vec!["0000000000000000000000000000000000000000"]),
        ];
        engine.identify_foldable_runs(&commits);
        let run = engine.get_foldable_runs().get("m").expect("fold exists");
        assert_eq!(run.folded_commit_ids, vec!["fa", "fb"]);
        assert_eq!(
            run.branch_root_id,
            "0000000000000000000000000000000000000000"
        );
    }

    /// The genuinely quadratic shape: many merges whose feature ancestry is
    /// one long shared spine segment. Each merge walks DEPTH/2 steps before
    /// matching its mainline parent, so total work is merges x depth — with
    /// integer-index steps instead of String/HashSet churn, and hard-bounded
    /// by MAX_FOLD_WALK per merge.
    #[test]
    fn many_merges_on_a_long_shared_chain_stay_linear() {
        let mut engine = BranchFoldingEngine::new();
        const DEPTH: usize = 8_000;
        const MERGES: usize = 300;
        const HALF: usize = DEPTH / 2;
        let mut commits = Vec::with_capacity(DEPTH + MERGES);
        commits.push(make_node("l0", vec![]));
        for i in 1..=DEPTH {
            commits.push(make_node(&format!("l{i}"), vec![&format!("l{}", i - 1)]));
        }
        for k in 0..MERGES {
            commits.push(make_node(
                &format!("m{k}"),
                vec![&format!("l{HALF}"), &format!("l{DEPTH}")],
            ));
        }
        engine.identify_foldable_runs(&commits);
        let runs = engine.get_foldable_runs();
        assert_eq!(runs.len(), MERGES, "every merge records its own run");
        for k in 0..MERGES {
            let run = runs.get(&format!("m{k}")).unwrap();
            assert_eq!(
                run.commit_count,
                DEPTH - HALF,
                "merge {k} folds the deep segment"
            );
            assert_eq!(run.branch_root_id, format!("l{HALF}"));
            assert_eq!(
                run.folded_commit_ids.first().map(String::as_str),
                Some(&*format!("l{DEPTH}"))
            );
            assert_eq!(
                run.folded_commit_ids.last().map(String::as_str),
                Some(&*format!("l{}", HALF + 1))
            );
        }
    }

    /// Corrupt cyclic input must terminate promptly. Legacy parity note:
    /// b's single parent equals m's mainline parent, so the literal walk —
    /// old and new alike — records the one-node run [b]; the guarantee under
    /// test is termination and stability, not extra cleverness.
    #[test]
    fn cyclic_parent_pointers_do_not_hang_or_panic() {
        let mut engine = BranchFoldingEngine::new();
        let commits = vec![
            make_node("a", vec!["b"]),
            make_node("b", vec!["a"]),
            make_node("m", vec!["a", "b"]),
        ];
        engine.identify_foldable_runs(&commits);
        let runs = engine.get_foldable_runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs.get("m").unwrap().folded_commit_ids, vec!["b"]);

        // A self-referential cycle must also terminate.
        let mut engine2 = BranchFoldingEngine::new();
        let commits2 = vec![make_node("s", vec!["s"]), make_node("m2", vec!["s", "s"])];
        engine2.identify_foldable_runs(&commits2);
    }

    /// The feature branch forked from an ancestor of the merge's first parent,
    /// not from that parent itself. Folding must walk mainline ancestry rather
    /// than requiring `next_parent == mainline_parent`.
    #[test]
    fn test_fold_when_feature_forked_from_mainline_ancestor() {
        let mut engine = BranchFoldingEngine::new();
        let commits = vec![
            make_node("m", vec!["main2", "feat2"]),
            make_node("feat2", vec!["feat1"]),
            make_node("feat1", vec!["main0"]),
            make_node("main2", vec!["main1"]),
            make_node("main1", vec!["main0"]),
            make_node("main0", vec![]),
        ];

        engine.identify_foldable_runs(&commits);
        let run = engine
            .get_foldable_runs()
            .get("m")
            .expect("feature run must fold onto the mainline ancestor");
        assert_eq!(run.folded_commit_ids, vec!["feat2", "feat1"]);
        assert_eq!(run.branch_root_id, "main0");
    }
}
