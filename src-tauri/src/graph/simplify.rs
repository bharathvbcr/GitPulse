//! History simplification for server-side commit filters.
//!
//! A filter that drops rows from a topologically sorted window leaves every
//! survivor naming parents that are no longer there. Git solves this for
//! path limiting by rewriting each survivor's parents to its nearest
//! surviving ancestors (`git log --parents -- path`); this module does the
//! same for the filter terms git cannot apply itself — author, sha prefix,
//! conventional type, free text — so a filtered graph stays connected and
//! the pinned mainline stays straight instead of dissolving into stubs.

use super::RawCommitNode;
use std::collections::HashMap;

/// Upper bound on rewritten parents per kept commit.
///
/// A run of dropped merges would otherwise union an unbounded number of
/// ancestors into one row; the solver allocates a column per parent, so the
/// window's width and the payload would grow with the filter instead of
/// the history. First-parent lineage always sits first and always fits.
pub const MAX_REWRITTEN_PARENTS: usize = 32;

/// Keeps the commits flagged in `keep` (parallel to `commits`) and rewrites
/// each survivor's parents to its nearest surviving ancestors.
///
/// Contract, for a window sorted newest-first with parents below children:
/// - a kept parent stays as-is;
/// - a dropped parent is replaced by ITS effective parents, in order, so the
///   first rewritten parent of a survivor is the nearest survivor along its
///   first-parent chain whenever that chain reaches one;
/// - an id the window does not hold at all is ancestry past the cut: it is
///   handed on through dropped commits like any parent, so the survivor
///   ends in the same honest fading stub `git log --parents -n N -- path`
///   prints for a parent beyond the limit, instead of posing as a root;
/// - an empty id, or one sitting at or above the child (malformed order),
///   carries no lineage: the survivor that names it keeps it verbatim (the
///   lane solver still draws the stub) but a dropped commit never passes
///   it on;
/// - duplicates collapse to the first mention and the list is capped at
///   [`MAX_REWRITTEN_PARENTS`].
///
/// A survivor whose loaded ancestors were all dropped becomes a root of the
/// filtered graph — exactly how `git log -- path` shows the oldest commit
/// touching a path. Missing `keep` entries count as dropped.
pub fn simplify_history(commits: &[RawCommitNode], keep: &[bool]) -> Vec<RawCommitNode> {
    let n = commits.len();
    let kept = |i: usize| keep.get(i).copied().unwrap_or(false);
    let mut row_of: HashMap<&str, usize> = HashMap::with_capacity(n);
    for (i, commit) in commits.iter().enumerate() {
        // First mention wins, matching the lane solver's own row index.
        row_of.entry(commit.id.as_str()).or_insert(i);
    }

    // Effective parents per row, resolved bottom-up: every in-window parent
    // sits below its child, so a dropped parent's list is final by the time
    // its children ask for it.
    let mut effective: Vec<Vec<String>> = vec![Vec::new(); n];
    for i in (0..n).rev() {
        let mut parents: Vec<String> = Vec::new();
        for parent in &commits[i].parent_ids {
            match row_of.get(parent.as_str()) {
                Some(&row) if row > i => {
                    if kept(row) {
                        push_parent(&mut parents, parent);
                    } else {
                        for inherited in &effective[row] {
                            push_parent(&mut parents, inherited);
                        }
                    }
                }
                _ => {
                    // Ancestry past the window is real and travels; an
                    // empty or malformed id is the survivor's own stub only.
                    let past_window = !parent.is_empty() && !row_of.contains_key(parent.as_str());
                    if kept(i) || past_window {
                        push_parent(&mut parents, parent);
                    }
                }
            }
        }
        effective[i] = parents;
    }

    commits
        .iter()
        .enumerate()
        .filter(|(i, _)| kept(*i))
        .map(|(i, commit)| RawCommitNode {
            parent_ids: std::mem::take(&mut effective[i]),
            ..commit.clone()
        })
        .collect()
}

fn push_parent(parents: &mut Vec<String>, id: &str) {
    if parents.len() >= MAX_REWRITTEN_PARENTS || parents.iter().any(|p| p == id) {
        return;
    }
    parents.push(id.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, parents: &[&str]) -> RawCommitNode {
        RawCommitNode {
            id: id.to_string(),
            parent_ids: parents.iter().map(|p| p.to_string()).collect(),
            timestamp: 1,
            author_name: "dev".to_string(),
            author_email: "dev@example.com".to_string(),
            summary: format!("commit {id}"),
        }
    }

    fn parents_of<'a>(rows: &'a [RawCommitNode], id: &str) -> &'a [String] {
        &rows
            .iter()
            .find(|c| c.id == id)
            .expect("kept commit")
            .parent_ids
    }

    fn ids(rows: &[RawCommitNode]) -> Vec<&str> {
        rows.iter().map(|c| c.id.as_str()).collect()
    }

    #[test]
    fn keeping_everything_is_the_identity() {
        let commits = vec![
            node("a", &["b", "c"]),
            node("b", &["d"]),
            node("c", &["d"]),
            node("d", &[]),
        ];
        let keep = vec![true; commits.len()];
        assert_eq!(simplify_history(&commits, &keep), commits);
    }

    #[test]
    fn keeping_nothing_is_empty_and_missing_flags_count_as_dropped() {
        let commits = vec![node("a", &["b"]), node("b", &[])];
        assert!(simplify_history(&commits, &[false, false]).is_empty());
        // A short mask drops the unflagged tail rather than panicking.
        let rows = simplify_history(&commits, &[true]);
        assert_eq!(ids(&rows), vec!["a"]);
        assert!(
            parents_of(&rows, "a").is_empty(),
            "dropped parent leaves a root"
        );
    }

    #[test]
    fn a_dropped_middle_commit_relinks_child_to_grandparent() {
        let commits = vec![
            node("a", &["b"]),
            node("b", &["c"]),
            node("c", &["d"]),
            node("d", &[]),
        ];
        let rows = simplify_history(&commits, &[true, false, false, true]);
        assert_eq!(ids(&rows), vec!["a", "d"]);
        assert_eq!(parents_of(&rows, "a"), ["d"]);
    }

    #[test]
    fn a_dropped_merge_hands_its_parents_on_first_parent_first() {
        // m (kept) -> x (dropped merge of [p, q]) ; p -> r ; q -> r ; all kept but x.
        let commits = vec![
            node("m", &["x"]),
            node("x", &["p", "q"]),
            node("p", &["r"]),
            node("q", &["r"]),
            node("r", &[]),
        ];
        let rows = simplify_history(&commits, &[true, false, true, true, true]);
        assert_eq!(parents_of(&rows, "m"), ["p", "q"]);
        // Dropping p as well pulls r in through p's slot, and q's own edge to
        // r collapses onto it instead of duplicating.
        let rows = simplify_history(&commits, &[true, false, false, false, true]);
        assert_eq!(parents_of(&rows, "m"), ["r"]);
    }

    #[test]
    fn the_first_rewritten_parent_is_the_nearest_survivor_on_the_first_parent_chain() {
        // Main chain m0 -> m1 -> m2 -> m3 with a feature f merged at m0.
        let commits = vec![
            node("m0", &["m1", "f"]),
            node("f", &["m2"]),
            node("m1", &["m2"]),
            node("m2", &["m3"]),
            node("m3", &[]),
        ];
        // Drop m1 and m2: m0's FIRST parent must be m3 (main's lineage), with
        // the merged feature still second; f relinks onto m3 as well.
        let rows = simplify_history(&commits, &[true, true, false, false, true]);
        assert_eq!(parents_of(&rows, "m0"), ["m3", "f"]);
        assert_eq!(parents_of(&rows, "f"), ["m3"]);
    }

    #[test]
    fn ancestry_past_the_window_is_inherited_but_empty_and_malformed_ids_are_not() {
        let commits = vec![
            node("a", &["b", "", "ghost"]),
            node("b", &["", "ghost", "c"]),
            node("c", &["a"]), // malformed: parent above the child
        ];
        let rows = simplify_history(&commits, &[true, false, true]);
        // Through dropped b, a inherits the out-of-window ghost and c (b's
        // empty id is not lineage); its own "" stays, its own ghost is a
        // duplicate of the inherited one.
        assert_eq!(parents_of(&rows, "a"), ["ghost", "c", ""]);
        // c's malformed back-edge is its own, so it stays.
        assert_eq!(parents_of(&rows, "c"), ["a"]);
        // With c dropped too, c's malformed edge is not handed on: a keeps
        // the real past-window ancestor and its own empty stub only.
        let rows = simplify_history(&commits, &[true, false, false]);
        assert_eq!(parents_of(&rows, "a"), ["ghost", ""]);
    }

    /// The has_more cut: a survivor whose dropped parent continues past the
    /// window ends in a stub to that parent, never as a fake root.
    #[test]
    fn a_dropped_parent_past_the_window_leaves_a_stub_not_a_root() {
        let commits = vec![node("a", &["b"]), node("b", &["older"])];
        let rows = simplify_history(&commits, &[true, false]);
        assert_eq!(parents_of(&rows, "a"), ["older"]);
    }

    #[test]
    fn rewritten_parents_are_capped_with_first_parent_lineage_kept() {
        // top -> hub (dropped) -> 40 kept leaves; hub's first parent is leaf_00.
        let leaves: Vec<String> = (0..40).map(|i| format!("leaf_{i:02}")).collect();
        let mut commits = vec![
            node("top", &["hub"]),
            node(
                "hub",
                &leaves.iter().map(String::as_str).collect::<Vec<_>>(),
            ),
        ];
        commits.extend(leaves.iter().map(|l| node(l, &[])));
        let mut keep = vec![true; commits.len()];
        keep[1] = false;
        let rows = simplify_history(&commits, &keep);
        let top = parents_of(&rows, "top");
        assert_eq!(top.len(), MAX_REWRITTEN_PARENTS);
        assert_eq!(top[0], "leaf_00");
        assert_eq!(
            top[MAX_REWRITTEN_PARENTS - 1],
            format!("leaf_{:02}", MAX_REWRITTEN_PARENTS - 1)
        );
    }

    #[test]
    fn duplicate_ids_resolve_to_the_first_mention() {
        let commits = vec![
            node("a", &["b"]),
            node("b", &["c"]),
            node("b", &["x"]),
            node("c", &[]),
        ];
        let rows = simplify_history(&commits, &[true, false, false, true]);
        assert_eq!(
            parents_of(&rows, "a"),
            ["c"],
            "the first b (parent c) is the one that counts"
        );
    }
}
