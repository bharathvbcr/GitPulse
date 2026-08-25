use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Representation of a raw commit input to the lane solver.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RawCommitNode {
    pub id: String,
    pub parent_ids: Vec<String>,
    pub timestamp: i64,
    pub author_name: String,
    pub author_email: String,
    pub summary: String,
}

/// A visual connection link drawn between rows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LaneConnection {
    pub from_lane: u32,
    pub to_lane: u32,
    pub to_row_offset: u32,
    pub is_merge: bool,
    pub color_index: u32,
    /// True when the parent is not in the loaded window — history was cut off
    /// by `max_commits`, or by a filter.
    ///
    /// It exists because the alternative is a lie: with no way to say "this
    /// edge leaves the window", the only offset available is 1, and the
    /// renderer draws a line from this commit to whatever unrelated commit
    /// happens to sit on the next row. A dangling edge is drawn as a stub that
    /// stops instead.
    pub is_dangling: bool,
}

/// Calculated visual representation of a single commit row in the graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualCommitRow {
    pub id: String,
    pub parent_ids: Vec<String>,
    pub summary: String,
    pub author_name: String,
    pub author_email: String,
    pub timestamp: i64,
    pub lane: u32,
    pub color_index: u32,
    pub active_lanes: Vec<u32>,       // All lanes passing through this row
    pub active_lane_colors: Vec<u32>, // Matching color indices for active lanes
    pub connections: Vec<LaneConnection>, // Outgoing connections to parents
    pub is_merge: bool,
    pub is_root: bool,
}

/// Greedy lane allocator state machine.
/// Implements first-parent lane continuity and column recycling.
pub struct LaneSolver {
    active_columns: Vec<Option<String>>, // Commit ID reserved at each column index
    column_colors: Vec<u32>,             // Assigned color index for each active column
    next_color_index: u32,
    palette_size: u32,
}

impl LaneSolver {
    pub fn new(palette_size: u32) -> Self {
        Self {
            active_columns: Vec::new(),
            column_colors: Vec::new(),
            next_color_index: 0,
            palette_size: if palette_size > 0 { palette_size } else { 12 },
        }
    }

    /// Picks a colour no live lane is already using.
    ///
    /// Plain round-robin hands the same colour to two lanes that are on screen
    /// together as soon as the palette wraps, and two same-coloured lanes
    /// crossing is exactly the case a reader cannot untangle. Scanning from the
    /// rotation cursor for a colour that is currently free keeps neighbours
    /// distinct while still rotating, so the graph does not settle into two
    /// colours. When every colour is live — more concurrent branches than the
    /// palette has entries — the cursor's colour is taken, because some
    /// collision is then unavoidable and a deterministic one is preferable.
    fn allocate_color(&mut self) -> u32 {
        let mut in_use = vec![false; self.palette_size as usize];
        for (idx, occupant) in self.active_columns.iter().enumerate() {
            if occupant.is_some() {
                if let Some(slot) = in_use.get_mut(self.column_colors[idx] as usize) {
                    *slot = true;
                }
            }
        }
        let start = self.next_color_index % self.palette_size;
        let mut chosen = start;
        for step in 0..self.palette_size {
            let candidate = (start + step) % self.palette_size;
            if !in_use[candidate as usize] {
                chosen = candidate;
                break;
            }
        }
        self.next_color_index = (chosen + 1) % self.palette_size;
        chosen
    }

    /// Solves the visual DAG layout for a topologically sorted list of commits.
    pub fn solve(&mut self, commits: &[RawCommitNode]) -> Vec<VisualCommitRow> {
        let mut visual_rows = Vec::with_capacity(commits.len());
        let commit_to_row_idx: HashMap<String, usize> = commits
            .iter()
            .enumerate()
            .map(|(idx, c)| (c.id.clone(), idx))
            .collect();

        // Track how many children still need each parent so we know when to free lanes
        let mut pending_parent_references: HashMap<String, usize> = HashMap::new();
        for commit in commits {
            for parent in &commit.parent_ids {
                *pending_parent_references.entry(parent.clone()).or_insert(0) += 1;
            }
        }

        for (row_idx, commit) in commits.iter().enumerate() {
            let is_merge = commit.parent_ids.len() > 1;
            let is_root = commit.parent_ids.is_empty();

            // 1. Locate or assign the lane for this commit
            let current_lane = match self.find_column(&commit.id) {
                Some(col) => col,
                None => {
                    let free_col = self.find_or_create_free_column();
                    let color = self.allocate_color();
                    self.active_columns[free_col] = Some(commit.id.clone());
                    self.column_colors[free_col] = color;
                    free_col
                }
            };

            let current_color = self.column_colors[current_lane];

            // 2. Snapshot the active lanes passing through this row
            let mut active_lanes = Vec::new();
            let mut active_lane_colors = Vec::new();
            for (col_idx, occupant) in self.active_columns.iter().enumerate() {
                if occupant.is_some() {
                    active_lanes.push(col_idx as u32);
                    active_lane_colors.push(self.column_colors[col_idx]);
                }
            }

            // 3. Clear this commit from the current column so parents can claim or reuse it
            self.active_columns[current_lane] = None;

            // 4. Process parents & establish outgoing connections
            let mut connections = Vec::new();

            for (parent_idx, parent_id) in commit.parent_ids.iter().enumerate() {
                let remaining_refs = pending_parent_references.get_mut(parent_id);
                if let Some(count) = remaining_refs {
                    *count = count.saturating_sub(1);
                }

                let target_lane = if parent_idx == 0 {
                    if let Some(existing_col) = self.find_column(parent_id) {
                        existing_col
                    } else {
                        self.active_columns[current_lane] = Some(parent_id.clone());
                        self.column_colors[current_lane] = current_color;
                        current_lane
                    }
                } else {
                    if let Some(existing_col) = self.find_column(parent_id) {
                        existing_col
                    } else {
                        // A merged-in branch is drawn to the right of the
                        // commit that merges it, so its edge fans out rather
                        // than crossing back over the lanes to its left.
                        let free_col = self.find_or_create_free_column_from(current_lane + 1);
                        let color = self.allocate_color();
                        self.active_columns[free_col] = Some(parent_id.clone());
                        self.column_colors[free_col] = color;
                        free_col
                    }
                };

                let target_color = self.column_colors[target_lane];
                let (row_offset, is_dangling) = match commit_to_row_idx.get(parent_id) {
                    Some(&target_row) if target_row > row_idx => {
                        ((target_row - row_idx) as u32, false)
                    }
                    // Either the parent is outside the window, or it sits at or
                    // above this row, which only happens on a malformed DAG.
                    // Both are edges that must not be drawn into a real node.
                    _ => (1, true),
                };

                connections.push(LaneConnection {
                    from_lane: current_lane as u32,
                    to_lane: target_lane as u32,
                    to_row_offset: row_offset,
                    is_merge: parent_idx > 0,
                    color_index: if parent_idx == 0 {
                        current_color
                    } else {
                        target_color
                    },
                    is_dangling,
                });
            }

            // 5. Clean up columns whose parents are no longer referenced
            for col_idx in 0..self.active_columns.len() {
                if let Some(ref occupied_id) = self.active_columns[col_idx] {
                    if let Some(&remaining) = pending_parent_references.get(occupied_id) {
                        if remaining == 0
                            && commit_to_row_idx
                                .get(occupied_id)
                                .is_none_or(|&r| r <= row_idx)
                        {
                            self.active_columns[col_idx] = None;
                        }
                    }
                }
            }

            visual_rows.push(VisualCommitRow {
                id: commit.id.clone(),
                parent_ids: commit.parent_ids.clone(),
                summary: commit.summary.clone(),
                author_name: commit.author_name.clone(),
                author_email: commit.author_email.clone(),
                timestamp: commit.timestamp,
                lane: current_lane as u32,
                color_index: current_color,
                active_lanes,
                active_lane_colors,
                connections,
                is_merge,
                is_root,
            });
        }

        visual_rows
    }

    fn find_column(&self, commit_id: &str) -> Option<usize> {
        self.active_columns
            .iter()
            .position(|col| col.as_deref() == Some(commit_id))
    }

    fn find_or_create_free_column(&mut self) -> usize {
        self.find_or_create_free_column_from(0)
    }

    /// The first free column at or after `preferred_start`, falling back to any
    /// free column before it, and to a new column when none is free.
    fn find_or_create_free_column_from(&mut self, preferred_start: usize) -> usize {
        if preferred_start < self.active_columns.len() {
            if let Some(offset) = self.active_columns[preferred_start..]
                .iter()
                .position(|col| col.is_none())
            {
                return preferred_start + offset;
            }
        }
        if let Some(free_idx) = self.active_columns.iter().position(|col| col.is_none()) {
            return free_idx;
        }
        self.active_columns.push(None);
        self.column_colors.push(0);
        self.active_columns.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn make_commit(id: &str, parents: Vec<&str>) -> RawCommitNode {
        RawCommitNode {
            id: id.to_string(),
            parent_ids: parents.into_iter().map(String::from).collect(),
            timestamp: 1000,
            author_name: "Developer".to_string(),
            author_email: "dev@example.com".to_string(),
            summary: format!("Commit {}", id),
        }
    }

    #[test]
    fn test_linear_history() {
        let mut solver = LaneSolver::new(12);
        let commits = vec![
            make_commit("c3", vec!["c2"]),
            make_commit("c2", vec!["c1"]),
            make_commit("c1", vec![]),
        ];

        let result = solver.solve(&commits);
        assert_eq!(result.len(), 3);
        for row in &result {
            assert_eq!(row.lane, 0);
            assert!(!row.summary.is_empty());
        }
    }

    #[test]
    fn test_diamond_merge() {
        let mut solver = LaneSolver::new(12);
        let commits = vec![
            make_commit("m", vec!["b1", "b2"]),
            make_commit("b1", vec!["root"]),
            make_commit("b2", vec!["root"]),
            make_commit("root", vec![]),
        ];

        let result = solver.solve(&commits);
        assert_eq!(result.len(), 4);
        assert_eq!(result[0].lane, 0);
        assert_eq!(result[1].lane, 0);
        assert_eq!(result[2].lane, 1);
        assert_eq!(result[3].lane, 0);
    }

    #[test]
    fn test_concurrent_lanes_do_not_share_a_colour() {
        // Six branches live at once against a four-colour palette: the first
        // four must all differ, and none may collide with a live neighbour
        // while a free colour remains.
        let mut solver = LaneSolver::new(4);
        let commits = vec![
            make_commit("m", vec!["b1", "b2", "b3", "b4"]),
            make_commit("b1", vec!["root"]),
            make_commit("b2", vec!["root"]),
            make_commit("b3", vec!["root"]),
            make_commit("b4", vec!["root"]),
            make_commit("root", vec![]),
        ];
        let rows = solver.solve(&commits);
        let branch_colors: HashSet<u32> = rows[1..=4].iter().map(|r| r.color_index).collect();
        assert_eq!(
            branch_colors.len(),
            4,
            "four branches on screen together were given {} colours",
            branch_colors.len()
        );
    }

    #[test]
    fn test_parent_outside_the_window_is_marked_dangling() {
        // `tip` has a parent the window never loaded. The edge must not be
        // drawn into the unrelated commit that happens to be on the next row.
        let mut solver = LaneSolver::new(12);
        let commits = vec![
            make_commit("tip", vec!["cut-off"]),
            make_commit("other", vec![]),
        ];
        let rows = solver.solve(&commits);
        assert_eq!(rows[0].connections.len(), 1);
        assert!(rows[0].connections[0].is_dangling);
        assert!(!rows[1].connections.iter().any(|c| c.is_dangling));
    }

    #[test]
    fn test_in_window_parents_are_not_dangling() {
        let mut solver = LaneSolver::new(12);
        let commits = vec![make_commit("c2", vec!["c1"]), make_commit("c1", vec![])];
        let rows = solver.solve(&commits);
        assert_eq!(rows[0].connections[0].to_row_offset, 1);
        assert!(!rows[0].connections[0].is_dangling);
    }

    #[test]
    fn test_merged_branch_lands_right_of_the_merge_commit() {
        let mut solver = LaneSolver::new(12);
        let commits = vec![
            make_commit("m", vec!["b1", "b2"]),
            make_commit("b1", vec!["root"]),
            make_commit("b2", vec!["root"]),
            make_commit("root", vec![]),
        ];
        let rows = solver.solve(&commits);
        let merge_edge = rows[0]
            .connections
            .iter()
            .find(|c| c.is_merge)
            .expect("merge commit has a second-parent edge");
        assert!(
            merge_edge.to_lane > merge_edge.from_lane,
            "merged branch went to lane {} from lane {}",
            merge_edge.to_lane,
            merge_edge.from_lane
        );
    }

    #[test]
    fn test_octopus_merge_10_parents() {
        let mut solver = LaneSolver::new(12);
        let mut parents = Vec::new();
        let mut all_commits = Vec::new();

        for i in 1..=10 {
            let pid = format!("p{}", i);
            parents.push(pid.clone());
        }

        let parent_refs: Vec<&str> = parents.iter().map(|s| s.as_str()).collect();
        all_commits.push(make_commit("merge", parent_refs));

        for p in &parents {
            all_commits.push(make_commit(p, vec!["root"]));
        }
        all_commits.push(make_commit("root", vec![]));

        let result = solver.solve(&all_commits);
        assert_eq!(result.len(), 12);
        let mut lanes_used = HashSet::new();
        for row in &result[1..=10] {
            lanes_used.insert(row.lane);
        }
        assert_eq!(lanes_used.len(), 10);
    }
}
