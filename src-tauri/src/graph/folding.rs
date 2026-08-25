use super::lane_solver::RawCommitNode;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Represents a folded branch run where linear feature branch commits are compressed into a summary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FoldedBranchRun {
    pub merge_commit_id: String,
    pub branch_root_id: String,
    pub folded_commit_ids: Vec<String>,
    pub commit_count: usize,
    pub is_collapsed: bool,
}

/// Identifies subgraphs where a branch diverged and subsequently merged back into the mainline.
pub struct BranchFoldingEngine {
    folded_runs: HashMap<String, FoldedBranchRun>,
}

impl Default for BranchFoldingEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl BranchFoldingEngine {
    pub fn new() -> Self {
        Self {
            folded_runs: HashMap::new(),
        }
    }

    /// Identifies all linear feature subgraphs that can be collapsed.
    pub fn identify_foldable_runs(&mut self, commits: &[RawCommitNode]) {
        self.folded_runs.clear();

        let commit_map: HashMap<String, &RawCommitNode> =
            commits.iter().map(|c| (c.id.clone(), c)).collect();

        for commit in commits {
            // Check if this commit is a 2-parent merge commit
            if commit.parent_ids.len() == 2 {
                let mainline_parent = &commit.parent_ids[0];
                let feature_parent = &commit.parent_ids[1];

                // Walk back from feature_parent along 1-parent chains until meeting mainline ancestry
                let mut current = feature_parent.clone();
                let mut chain = Vec::new();
                let mut visited = HashSet::new();

                while let Some(node) = commit_map.get(&current) {
                    if visited.contains(&current) {
                        break;
                    }
                    visited.insert(current.clone());
                    chain.push(current.clone());

                    if node.parent_ids.len() == 1 {
                        let next_parent = &node.parent_ids[0];
                        if next_parent == mainline_parent {
                            if !chain.is_empty() {
                                self.folded_runs.insert(
                                    commit.id.clone(),
                                    FoldedBranchRun {
                                        merge_commit_id: commit.id.clone(),
                                        branch_root_id: next_parent.clone(),
                                        folded_commit_ids: chain.clone(),
                                        commit_count: chain.len(),
                                        is_collapsed: false,
                                    },
                                );
                            }
                            break;
                        }
                        current = next_parent.clone();
                    } else {
                        // Nested merge or branch fork — do not fold automatically
                        break;
                    }
                }
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
}
