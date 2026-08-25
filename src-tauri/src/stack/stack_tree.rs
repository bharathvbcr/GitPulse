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
    pub fn build_stack_hierarchy(
        branch_tips: &HashMap<String, String>, // branch_name -> commit_id
        commit_parents: &HashMap<String, Vec<String>>, // commit_id -> parents
        default_branch: &str,
    ) -> Vec<StackedBranchNode> {
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
            let mut current = tip_id.clone();
            let mut count = 0;
            let mut parent_branch = None;

            let mut visited = std::collections::HashSet::new();

            while let Some(parents) = commit_parents.get(&current) {
                if visited.contains(&current) {
                    break;
                }
                visited.insert(current.clone());

                if let Some(first_parent) = parents.first() {
                    count += 1;
                    // Check if first_parent is the tip of another branch
                    for (other_bname, other_tip) in branch_tips {
                        if other_bname != branch_name && other_tip == first_parent {
                            parent_branch = Some(other_bname.clone());
                            break;
                        }
                    }
                    if parent_branch.is_some() {
                        break;
                    }
                    current = first_parent.clone();
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
