use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StackedBranchNode {
    pub branch_name: String,
    pub tip_commit_id: String,
    pub parent_branch_name: Option<String>,
    pub child_branch_names: Vec<String>,
    pub commit_count_ahead_of_parent: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BranchAncestryChain {
    pub current_branch: String,
    pub root_branch: String,           // Typically "main" or "master"
    pub breadcrumb_chain: Vec<String>, // e.g. ["main", "feat-auth", "feat-oauth-google"]
}

pub struct StackTreeEngine;

impl StackTreeEngine {
    /// Computes stacked branch hierarchies based on merge base and branch heads.
    ///
    /// Branch-tip lookups along each first-parent walk go through a
    /// prebuilt `tip -> branches` index instead of scanning every branch tip
    /// at every step, turning the per-branch cost from O(depth × branches)
    /// into O(depth + tips). When several branches share one tip, the
    /// lexicographically smallest name wins — deterministic, where the old
    /// scan depended on HashMap iteration order.
    pub fn build_stack_hierarchy(
        branch_tips: &HashMap<String, String>, // branch_name -> commit_id
        commit_parents: &HashMap<String, Vec<String>>, // commit_id -> parents
        default_branch: &str,
    ) -> Vec<StackedBranchNode> {
        let mut tip_index: HashMap<&str, Vec<&str>> = HashMap::new();
        for (name, tip) in branch_tips {
            tip_index
                .entry(tip.as_str())
                .or_default()
                .push(name.as_str());
        }
        for candidates in tip_index.values_mut() {
            candidates.sort_unstable();
        }

        let mut nodes = Vec::new();

        for (branch_name, tip_id) in branch_tips {
            if branch_name == default_branch {
                nodes.push(StackedBranchNode {
                    branch_name: branch_name.clone(),
                    tip_commit_id: tip_id.clone(),
                    parent_branch_name: None,
                    child_branch_names: Vec::new(),
                    commit_count_ahead_of_parent: 0,
                });
                continue;
            }

            // Walk back from tip_id until hitting another branch tip or root
            let mut current = tip_id.as_str();
            let mut count = 0;
            let mut parent_branch = None;

            let mut visited = std::collections::HashSet::new();

            while let Some(parents) = commit_parents.get(current) {
                if !visited.insert(current) {
                    break;
                }

                if let Some(first_parent) = parents.first() {
                    count += 1;
                    // Check if first_parent is the tip of another branch
                    if let Some(candidates) = tip_index.get(first_parent.as_str()) {
                        if let Some(&other) =
                            candidates.iter().find(|&&b| b != branch_name.as_str())
                        {
                            parent_branch = Some(other.to_string());
                            break;
                        }
                    }
                    current = first_parent.as_str();
                } else {
                    break;
                }
            }

            nodes.push(StackedBranchNode {
                branch_name: branch_name.clone(),
                tip_commit_id: tip_id.clone(),
                parent_branch_name: parent_branch.or_else(|| Some(default_branch.to_string())),
                child_branch_names: Vec::new(),
                commit_count_ahead_of_parent: count,
            });
        }

        let mut children_map: HashMap<String, Vec<String>> = HashMap::new();
        for node in &nodes {
            if let Some(ref parent) = node.parent_branch_name {
                children_map
                    .entry(parent.clone())
                    .or_default()
                    .push(node.branch_name.clone());
            }
        }
        for node in &mut nodes {
            if let Some(children) = children_map.remove(&node.branch_name) {
                node.child_branch_names = children;
            }
        }

        nodes
    }

    /// Computes breadcrumbs from default_branch down to current_branch.
    pub fn get_ancestry_breadcrumbs(
        nodes: &[StackedBranchNode],
        current_branch: &str,
    ) -> BranchAncestryChain {
        let mut chain = vec![current_branch.to_string()];
        let node_map: HashMap<&str, &StackedBranchNode> =
            nodes.iter().map(|n| (n.branch_name.as_str(), n)).collect();

        let mut visited = std::collections::HashSet::new();
        visited.insert(current_branch.to_string());

        let mut curr = current_branch;
        while let Some(node) = node_map.get(curr) {
            if let Some(ref parent) = node.parent_branch_name {
                if visited.contains(parent.as_str()) || chain.len() > 256 {
                    break;
                }
                visited.insert(parent.clone());
                chain.push(parent.clone());
                curr = parent;
            } else {
                break;
            }
        }

        chain.reverse();
        let root = chain.first().cloned().unwrap_or_else(|| "main".to_string());

        BranchAncestryChain {
            current_branch: current_branch.to_string(),
            root_branch: root,
            breadcrumb_chain: chain,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Agent-farm shape: many long-lived stacked branches over one spine.
    /// The tip index must keep this linear-ish; the old all-tips scan per
    /// step made it O(branches x depth x branches).
    #[test]
    fn deep_wide_stack_hierarchies_resolve_correctly() {
        const BRANCHES: usize = 200;
        const DEPTH: usize = 50;

        let mut branch_tips = HashMap::new();
        branch_tips.insert("main".to_string(), "s0".to_string());
        let mut commit_parents: HashMap<String, Vec<String>> = HashMap::new();
        commit_parents.insert("s0".to_string(), vec![]);

        // Spine s0 <- s1 <- ... <- s_DEPTH
        for i in 1..=DEPTH {
            commit_parents.insert(format!("s{i}"), vec![format!("s{}", i - 1)]);
        }
        branch_tips.insert("trunk".to_string(), format!("s{DEPTH}"));

        // Each stacked branch grows ON TOP of trunk's tip (the realistic
        // stacked-branch shape), so its downward walk meets the trunk tip.
        for b in 0..BRANCHES {
            let mut prev = format!("s{DEPTH}");
            for d in 1..=3 {
                let id = format!("b{b}_d{d}");
                commit_parents.insert(id.clone(), vec![prev]);
                prev = id;
            }
            branch_tips.insert(format!("feat/{b}"), prev);
        }

        // feat/N's walk: b_d3 -> b_d2 -> b_d1 -> s{DEPTH} == trunk's tip,
        // so parent = trunk with exactly its own three commits ahead.
        let nodes = StackTreeEngine::build_stack_hierarchy(&branch_tips, &commit_parents, "main");
        assert_eq!(nodes.len(), BRANCHES + 2);
        for b in 0..BRANCHES {
            let node = nodes
                .iter()
                .find(|n| n.branch_name == format!("feat/{b}"))
                .unwrap();
            assert_eq!(node.parent_branch_name.as_deref(), Some("trunk"));
            assert_eq!(node.commit_count_ahead_of_parent, 3, "feat/{b} ahead-count");
        }
        let trunk = nodes.iter().find(|n| n.branch_name == "trunk").unwrap();
        assert_eq!(trunk.parent_branch_name.as_deref(), Some("main"));
        assert_eq!(trunk.commit_count_ahead_of_parent, DEPTH);
    }

    #[test]
    fn test_stack_hierarchy_building() {
        let mut branch_tips = HashMap::new();
        branch_tips.insert("main".to_string(), "c0".to_string());
        branch_tips.insert("feat-auth".to_string(), "c1".to_string());
        branch_tips.insert("feat-oauth".to_string(), "c2".to_string());

        let mut commit_parents = HashMap::new();
        commit_parents.insert("c2".to_string(), vec!["c1".to_string()]);
        commit_parents.insert("c1".to_string(), vec!["c0".to_string()]);
        commit_parents.insert("c0".to_string(), vec![]);

        let nodes = StackTreeEngine::build_stack_hierarchy(&branch_tips, &commit_parents, "main");
        assert_eq!(nodes.len(), 3);

        let oauth = nodes
            .iter()
            .find(|n| n.branch_name == "feat-oauth")
            .unwrap();
        assert_eq!(oauth.parent_branch_name.as_deref(), Some("feat-auth"));
        let auth = nodes.iter().find(|n| n.branch_name == "feat-auth").unwrap();
        assert!(auth.child_branch_names.contains(&"feat-oauth".to_string()));

        let breadcrumbs = StackTreeEngine::get_ancestry_breadcrumbs(&nodes, "feat-oauth");
        assert_eq!(
            breadcrumbs.breadcrumb_chain,
            vec!["main", "feat-auth", "feat-oauth"]
        );
    }

    #[test]
    fn test_cyclic_parent_pointers_do_not_loop() {
        let cyclic = vec![
            StackedBranchNode {
                branch_name: "feat-a".into(),
                tip_commit_id: "c1".into(),
                parent_branch_name: Some("feat-b".into()),
                child_branch_names: vec!["feat-b".into()],
                commit_count_ahead_of_parent: 1,
            },
            StackedBranchNode {
                branch_name: "feat-b".into(),
                tip_commit_id: "c2".into(),
                parent_branch_name: Some("feat-a".into()),
                child_branch_names: vec!["feat-a".into()],
                commit_count_ahead_of_parent: 1,
            },
        ];
        let chain = StackTreeEngine::get_ancestry_breadcrumbs(&cyclic, "feat-a");
        assert!(chain.breadcrumb_chain.len() <= 3);
        assert!(chain.breadcrumb_chain.contains(&"feat-a".to_string()));
    }
}
