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

/// First-parent walk ceiling: far beyond any real stacked-branch depth, and
/// turns a pathological history into bounded work instead of an unbounded one.
const MAX_FIRST_PARENT_WALK: usize = 100_000;

impl StackTreeEngine {
    /// Computes stacked branch hierarchies based on merge base and branch heads.
    ///
    /// Branch-tip lookups along each first-parent walk go through a
    /// prebuilt `tip -> owning branch` index instead of scanning every branch
    /// tip at every step, turning the per-branch cost from O(depth × branches)
    /// into O(depth + tips). Ties resolve deterministically: the default
    /// branch wins, otherwise the lexicographically smallest name — where the
    /// old scan depended on HashMap iteration order.
    pub fn build_stack_hierarchy(
        branch_tips: &HashMap<String, String>, // branch_name -> commit_id
        commit_parents: &HashMap<String, Vec<String>>, // commit_id -> parents
        default_branch: &str,
    ) -> Vec<StackedBranchNode> {
        // One pass builds tip -> owning branch, so the per-branch walk is a
        // hash lookup instead of a scan over every tip at every step. Ties
        // (two branches on one commit) resolve deterministically: the default
        // branch wins, otherwise the lexicographically smallest name.
        let mut tip_owner: HashMap<&str, &str> = HashMap::with_capacity(branch_tips.len());
        for (name, tip) in branch_tips {
            match tip_owner.get(tip.as_str()) {
                None => {
                    tip_owner.insert(tip.as_str(), name.as_str());
                }
                Some(&existing) => {
                    let better = name == default_branch
                        || (existing != default_branch && name.as_str() < existing);
                    if better {
                        tip_owner.insert(tip.as_str(), name.as_str());
                    }
                }
            }
        }

        let mut names: Vec<&String> = branch_tips.keys().collect();
        names.sort();

        let mut nodes = Vec::with_capacity(names.len());
        for branch_name in names {
            let tip_id = &branch_tips[branch_name.as_str()];
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

            // Walk first-parent history until landing on another branch's
            // literal tip. Exhausting the chain means no discoverable base:
            // reporting none is honest; grafting onto the default branch with
            // the whole depth as "ahead" invented hierarchy that does not
            // exist in git.
            let mut current: &str = tip_id;
            let mut count = 0usize;
            let mut parent_branch: Option<String> = None;
            let mut visited = std::collections::HashSet::new();
            visited.insert(current.to_string());

            for _ in 0..MAX_FIRST_PARENT_WALK {
                let Some(parents) = commit_parents.get(current) else {
                    break;
                };
                let Some(first_parent) = parents.first() else {
                    break;
                };
                if !visited.insert(first_parent.clone()) {
                    break;
                }
                count += 1;
                if let Some(owner) = tip_owner.get(first_parent.as_str()) {
                    if *owner != branch_name.as_str() {
                        parent_branch = Some((*owner).to_string());
                        break;
                    }
                }
                current = first_parent.as_str();
            }

            nodes.push(StackedBranchNode {
                branch_name: branch_name.clone(),
                tip_commit_id: tip_id.clone(),
                parent_branch_name: parent_branch.clone(),
                child_branch_names: Vec::new(),
                commit_count_ahead_of_parent: if parent_branch.is_some() { count } else { 0 },
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
        // trunk's own first-parent walk lands on main's literal tip (s0),
        // so its base is discovered, not grafted: exactly DEPTH commits ahead.
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

    /// Regression (audit H1-stack): a branch whose first-parent chain never
    /// lands on another branch's literal tip must NOT be grafted onto the
    /// default branch with its full depth as an invented ahead-count.
    #[test]
    fn fork_off_mid_history_is_not_fabricated_onto_default_branch() {
        let mut tips = HashMap::new();
        tips.insert("main".to_string(), "m9".to_string());
        tips.insert("feat-x".to_string(), "f5".to_string());

        let mut parents = HashMap::new();
        // feat-x descends f5 -> f4 -> ... -> r0; no tip sits on that chain.
        parents.insert("f5".to_string(), vec!["f4".to_string()]);
        parents.insert("f4".to_string(), vec!["f3".to_string()]);
        parents.insert("f3".to_string(), vec!["r0".to_string()]);
        // main lives on a separate line entirely.
        parents.insert("m9".to_string(), vec!["m8".to_string()]);

        let nodes = StackTreeEngine::build_stack_hierarchy(&tips, &parents, "main");
        let x = nodes.iter().find(|n| n.branch_name == "feat-x").unwrap();
        assert_eq!(
            x.parent_branch_name, None,
            "no real base means no claimed base"
        );
        assert_eq!(x.commit_count_ahead_of_parent, 0);
        // And the default branch must not silently grow this orphan as a child.
        let m = nodes.iter().find(|n| n.branch_name == "main").unwrap();
        assert!(
            !m.child_branch_names.contains(&"feat-x".to_string()),
            "fabricated hierarchy leaked into child list"
        );
    }

    /// The walk must stay linear per branch even when another tip never
    /// appears: a long unbranched history cannot degrade into repeated
    /// whole-map scans.
    #[test]
    fn deep_unbranched_history_walks_bounded_and_reports_no_base() {
        let mut tips = HashMap::new();
        tips.insert("main".to_string(), "n50000".to_string());
        tips.insert("orphan".to_string(), "o1".to_string());

        let mut parents = HashMap::new();
        parents.insert("o1".to_string(), vec!["o0".to_string()]);
        for i in 1..=50_000usize {
            parents.insert(format!("n{i}"), vec![format!("n{}", i - 1)]);
        }

        let nodes = StackTreeEngine::build_stack_hierarchy(&tips, &parents, "main");
        let o = nodes.iter().find(|n| n.branch_name == "orphan").unwrap();
        assert_eq!(o.parent_branch_name, None);
        assert_eq!(o.commit_count_ahead_of_parent, 0);
    }

    /// Branch iteration order comes from a HashMap, so identical inputs used
    /// to produce differently-ordered output (and child lists) run to run.
    #[test]
    fn output_is_deterministic_across_runs() {
        let build = || {
            let mut tips = HashMap::new();
            for name in ["zeta", "alpha", "main", "mid"] {
                tips.insert(name.to_string(), format!("tip-{name}"));
            }
            let mut parents = HashMap::new();
            parents.insert("tip-mid".to_string(), vec!["tip-main".to_string()]);
            parents.insert("tip-alpha".to_string(), vec!["tip-main".to_string()]);
            StackTreeEngine::build_stack_hierarchy(&tips, &parents, "main")
        };
        let a = build();
        let b = build();
        assert_eq!(a, b, "same input must give byte-identical output");
    }
}
