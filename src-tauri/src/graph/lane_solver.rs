use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap};

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
    /// True when this commit lies on the pinned mainline — the first-parent
    /// chain the solver was asked to keep straight (see [`MainlineHint`]).
    /// Mainline rows always sit on [`MAINLINE_COLUMN`] in
    /// [`MAINLINE_COLOR`]. `default` keeps rows serialized before the field
    /// existed deserializable.
    #[serde(default)]
    pub is_mainline: bool,
}

/// The column the mainline is pinned to for the whole window.
pub const MAINLINE_COLUMN: u32 = 0;
/// The palette slot the mainline always paints in, so `main` looks the
/// same in every repository and on every reload.
pub const MAINLINE_COLOR: u32 = 0;

/// Which branch the solver keeps straight.
///
/// The solver pins ONE first-parent chain — the mainline — to column 0 for
/// the entire window, in colour 0, so `main` reads as a single vertical rail
/// however the history walk interleaved it with feature branches. The
/// anchor commit is resolved in this order:
///
/// 1. the first of `branch_tips` present in the window (local `main` before
///    `origin/main`, by caller convention). The chain is then extended
///    UPWARD through any other listed tip whose first-parent chain passes
///    through the anchor, so a remote-tracking branch that is ahead of the
///    local one continues the same rail instead of opening a second column;
/// 2. `fallback_tip` (HEAD, typically) when none of the branch tips is
///    loaded — a filtered or single-branch window;
/// 3. row 0, the newest commit in the window, when nothing was given or
///    nothing resolved. An unhinted [`LaneSolver::solve`] pins exactly this.
///
/// Extension is deliberately limited to `branch_tips`: HEAD on a feature
/// branch forked from main's tip would otherwise extend the rail through
/// the feature's commits and paint them as main.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MainlineHint {
    pub branch_tips: Vec<String>,
    pub fallback_tip: Option<String>,
}

/// Stable-column lane solver with a pinned mainline.
///
/// The graph is decomposed into **branch segments**: maximal first-parent
/// chains. A segment is born at its tip row (or at the merge row that pulls
/// it in as a second-or-later parent), and ends when its chain hits a root,
/// leaves the window, or closes into a parent another segment already owns.
///
/// Each segment holds ONE column for its ENTIRE lifetime — including the
/// rows its closing connector is still descending through and the stub row
/// under a window-cut tail. Columns are assigned by greedy interval
/// allocation in birth order (lowest free column whose previous occupant
/// has fully ended), which is optimal for interval graphs: the graph is
/// exactly as wide as its peak concurrent occupancy, never wider.
///
/// One segment is special. The **mainline** — the first-parent chain of the
/// branch named by [`MainlineHint`] — is reserved BEFORE any row is walked:
/// every commit on it is pre-claimed for segment 0, which is pinned to
/// column 0 for the whole window and painted in colour 0. Without that
/// reservation, ownership of a shared ancestor went to whichever child
/// reached it first in row order, and `git log --topo-order` routinely lists
/// a merged feature's commits above the main commits they forked from: the
/// feature claimed main's ancestor, main's own chain "closed" into the
/// feature's column, and the branch every reader orients by jogged sideways
/// at each merge. Feature chains now always close INTO the mainline column,
/// never the reverse, so main is one straight rail from its tip to the
/// bottom of the window. The mainline follows first parents strictly: a
/// window cut under a mainline merge ends the rail with a stub rather than
/// continuing it into the merged-in branch, because the rail's meaning is
/// "main's own history" and a merged branch painted in main's colour would
/// be a lie.
///
/// This is what the old greedy row-by-row allocator could not guarantee:
/// it freed a closing branch's column at the branch's own row, so a tip
/// born one row later was drawn straight through the still-descending
/// connector — the overlapping-branch artifact — and the display layer
/// papered over the holes with per-row repacking, which made every lane
/// right of a dying neighbour jog sideways. With whole-lifetime columns
/// neither failure mode can exist, and a lane index IS its final visual
/// column: no repacking layer is needed or wanted.
pub struct LaneSolver {
    palette_size: u32,
}

/// Index of the mainline segment; it is always created first.
const MAINLINE_SEGMENT: usize = 0;

/// One branch segment: a maximal first-parent chain and the column
/// reservation that carries it.
struct Segment {
    /// Row where the column reservation begins: the tip row, or the merge
    /// row that spawned this segment as a second-or-later parent.
    alloc_from: usize,
    /// First row on which the column appears in `active_lanes`. Equal to
    /// `alloc_from` for tips; the row below the merge for spawned segments
    /// (the merge row itself shows only the peeling connector).
    visual_from: usize,
    /// Row of the segment's last own commit. Updated as members join.
    last_row: usize,
    /// Row of the parent this segment closes into when it ends by merging
    /// into a column another segment owns; the closing connector keeps this
    /// segment's column busy down to that row.
    close_target: Option<usize>,
    /// The segment's last commit carries a dangling edge, whose fading stub
    /// protrudes into the row below; pad the reservation so a later tip
    /// cannot be drawn under the stub.
    dangling_tail: bool,
    /// The mainline: its column is reserved for the whole window, so no
    /// other branch is ever drawn on it — above the mainline's tip, below
    /// its root, or in any hole between its commits.
    pinned: bool,
    column: u32,
    color: u32,
}

impl Segment {
    fn new(alloc_from: usize, visual_from: usize, last_row: usize) -> Self {
        Self {
            alloc_from,
            visual_from,
            last_row,
            close_target: None,
            dangling_tail: false,
            pinned: false,
            column: 0,
            color: 0,
        }
    }

    /// Last row this segment's column reservation covers; `window_end` is
    /// the window's final row index.
    fn alloc_until(&self, window_end: usize) -> usize {
        if self.pinned {
            return window_end;
        }
        let mut until = self.last_row;
        if let Some(target) = self.close_target {
            until = until.max(target);
        }
        if self.dangling_tail {
            until = until.max(self.last_row.saturating_add(1));
        }
        until
    }
}

/// One outgoing edge, resolved during the segment pass and emitted later
/// once columns and colors exist.
enum ConnSpec {
    /// Empty id, out-of-window parent, or malformed (at-or-above) parent:
    /// a fading stub on the child's own lane, never a line into a real row.
    Dangling,
    /// Live edge into `segment`'s column, landing on `target_row`.
    Edge { segment: usize, target_row: usize },
}

/// Rows of the first-parent chain starting at `from`, tip first, strictly
/// descending in row order. Stops at a root, at a parent outside the window,
/// and at a malformed parent sitting at or above the current row — the same
/// three cases the segment pass turns into dangling stubs.
fn first_parent_chain(
    commits: &[RawCommitNode],
    row_of: &HashMap<&str, usize>,
    from: usize,
) -> Vec<usize> {
    let mut chain = vec![from];
    let mut current = from;
    while let Some(parent) = commits[current].parent_ids.first() {
        if parent.is_empty() {
            break;
        }
        match row_of.get(parent.as_str()) {
            Some(&row) if row > current => {
                chain.push(row);
                current = row;
            }
            _ => break,
        }
    }
    chain
}

/// Resolves the mainline for a non-empty window per [`MainlineHint`]: the
/// rows of the pinned chain, tip first.
fn mainline_chain(
    commits: &[RawCommitNode],
    row_of: &HashMap<&str, usize>,
    hint: &MainlineHint,
) -> Vec<usize> {
    debug_assert!(!commits.is_empty());
    let row_for = |id: &str| -> Option<usize> {
        if id.is_empty() {
            None
        } else {
            row_of.get(id).copied()
        }
    };
    let anchor = hint
        .branch_tips
        .iter()
        .find_map(|id| row_for(id))
        .or_else(|| hint.fallback_tip.as_deref().and_then(row_for));
    let Some(anchor) = anchor else {
        return first_parent_chain(commits, row_of, 0);
    };
    // Extend upward through same-branch tips that are ahead of the anchor:
    // a remote-tracking branch not yet pulled continues the same rail. Each
    // extension moves the tip strictly upward, so the loop terminates.
    let mut tip = anchor;
    let mut extended = true;
    while extended {
        extended = false;
        for id in &hint.branch_tips {
            let Some(row) = row_for(id) else {
                continue;
            };
            if row < tip && first_parent_chain(commits, row_of, row).contains(&tip) {
                tip = row;
                extended = true;
            }
        }
    }
    first_parent_chain(commits, row_of, tip)
}

/// The ids of the chain [`LaneSolver::solve_with_mainline`] pins for
/// `commits` under `hint`, tip first (empty for an empty window).
///
/// Callers that thin the window AFTER deciding the hint (server-side commit
/// filters) use it to re-anchor on the chain's first survivor, so the
/// filtered graph keeps the same branch straight as the unfiltered one.
pub fn mainline_chain_ids(commits: &[RawCommitNode], hint: &MainlineHint) -> Vec<String> {
    if commits.is_empty() {
        return Vec::new();
    }
    let mut row_of: HashMap<&str, usize> = HashMap::with_capacity(commits.len());
    for (i, commit) in commits.iter().enumerate() {
        row_of.entry(commit.id.as_str()).or_insert(i);
    }
    mainline_chain(commits, &row_of, hint)
        .into_iter()
        .map(|row| commits[row].id.clone())
        .collect()
}

impl LaneSolver {
    pub fn new(palette_size: u32) -> Self {
        Self {
            palette_size: if palette_size > 0 { palette_size } else { 12 },
        }
    }

    /// Solves the visual DAG layout for a topologically sorted list of
    /// commits, pinning the newest commit's first-parent chain as the
    /// mainline (an empty [`MainlineHint`]).
    pub fn solve(&mut self, commits: &[RawCommitNode]) -> Vec<VisualCommitRow> {
        self.solve_with_mainline(commits, &MainlineHint::default())
    }

    /// Solves the visual DAG layout for a topologically sorted list of
    /// commits, keeping the branch described by `hint` straight on column 0.
    pub fn solve_with_mainline(
        &mut self,
        commits: &[RawCommitNode],
        hint: &MainlineHint,
    ) -> Vec<VisualCommitRow> {
        let n = commits.len();
        if n == 0 {
            return Vec::new();
        }
        let window_end = n - 1;

        // First occurrence wins: a later duplicate id is corrupt input and
        // must not steal endpoints from the earlier row.
        let mut row_of: HashMap<&str, usize> = HashMap::with_capacity(n);
        for (idx, commit) in commits.iter().enumerate() {
            if commit.id.is_empty() {
                continue;
            }
            row_of.entry(commit.id.as_str()).or_insert(idx);
        }

        // ---- Pass 0: reserve the mainline. ------------------------------
        // Every commit on the pinned chain is claimed for segment 0 before
        // any row is walked, so no feature chain reaching a main ancestor
        // first in row order can take ownership of it.
        let mut segments: Vec<Segment> = Vec::new();
        // Parent id -> segment that reserved it. A reservation exists only
        // for ids whose first occurrence lies strictly below the reserving
        // row (or, for the mainline, on the pre-claimed chain itself), so it
        // is always consumed exactly at that first occurrence.
        let mut pending: HashMap<&str, usize> = HashMap::new();
        {
            let chain = mainline_chain(commits, &row_of, hint);
            let tip = chain[0];
            let mut mainline = Segment::new(0, tip, tip);
            mainline.pinned = true;
            segments.push(mainline);
            debug_assert_eq!(segments.len() - 1, MAINLINE_SEGMENT);
            for &row in &chain {
                pending.insert(commits[row].id.as_str(), MAINLINE_SEGMENT);
            }
        }

        // ---- Pass 1: build segments and per-row connection specs. -------
        let mut seg_of_row: Vec<usize> = vec![0; n];
        let mut conn_specs: Vec<Vec<ConnSpec>> = Vec::with_capacity(n);

        for (i, commit) in commits.iter().enumerate() {
            let seg_id = match pending.remove(commit.id.as_str()) {
                Some(seg) => seg,
                None => {
                    segments.push(Segment::new(i, i, i));
                    segments.len() - 1
                }
            };
            seg_of_row[i] = seg_id;
            segments[seg_id].last_row = i;

            // Does this segment's chain continue below this commit? Set by a
            // live first parent (continuation or close); when the first
            // parent is dead (empty / ghost / malformed), the first unowned
            // merged-in parent inherits the column instead, so a merge whose
            // mainline left the window still draws compactly straight down.
            // The mainline itself never inherits: its rail means "main's own
            // first-parent history", so it ends with a stub instead.
            let mut continues_below = false;

            let mut specs = Vec::with_capacity(commit.parent_ids.len());
            for (k, parent_id) in commit.parent_ids.iter().enumerate() {
                let target_row = if parent_id.is_empty() {
                    None
                } else {
                    row_of.get(parent_id.as_str()).copied().filter(|&r| r > i)
                };
                let Some(parent_row) = target_row else {
                    specs.push(ConnSpec::Dangling);
                    continue;
                };

                if k == 0 {
                    match pending.get(parent_id.as_str()) {
                        Some(&owner) if owner != seg_id => {
                            // Another segment already reserved this parent:
                            // this chain ends here and its connector descends
                            // to the owner's row, keeping the column busy
                            // until then.
                            specs.push(ConnSpec::Edge {
                                segment: owner,
                                target_row: parent_row,
                            });
                            let seg = &mut segments[seg_id];
                            seg.close_target =
                                Some(seg.close_target.map_or(parent_row, |existing: usize| {
                                    existing.max(parent_row)
                                }));
                        }
                        Some(_) => {
                            // Pre-claimed by this very segment (the mainline
                            // chain): straight continuation, nothing to
                            // reserve.
                            specs.push(ConnSpec::Edge {
                                segment: seg_id,
                                target_row: parent_row,
                            });
                        }
                        None => {
                            pending.insert(parent_id.as_str(), seg_id);
                            specs.push(ConnSpec::Edge {
                                segment: seg_id,
                                target_row: parent_row,
                            });
                        }
                    }
                    continues_below = true;
                } else if let Some(&owner) = pending.get(parent_id.as_str()) {
                    specs.push(ConnSpec::Edge {
                        segment: owner,
                        target_row: parent_row,
                    });
                } else if !continues_below && seg_id != MAINLINE_SEGMENT {
                    // Dead first parent: this live merged-in parent continues
                    // the segment in place, straight down the same column.
                    pending.insert(parent_id.as_str(), seg_id);
                    specs.push(ConnSpec::Edge {
                        segment: seg_id,
                        target_row: parent_row,
                    });
                    continues_below = true;
                } else {
                    // A genuinely new branch peels off this merge: born at
                    // the merge row (the connector occupies its column from
                    // here), visible in active_lanes from the row below.
                    segments.push(Segment::new(i, i + 1, parent_row));
                    let spawned = segments.len() - 1;
                    pending.insert(parent_id.as_str(), spawned);
                    specs.push(ConnSpec::Edge {
                        segment: spawned,
                        target_row: parent_row,
                    });
                }
            }
            conn_specs.push(specs);
        }
        debug_assert!(
            pending.is_empty(),
            "every reservation targets an in-window row and must be consumed"
        );

        // Stub padding: a dangling edge on a segment's LAST commit protrudes
        // into the row below; keep the column reserved for it.
        for (i, specs) in conn_specs.iter().enumerate() {
            if specs.iter().any(|s| matches!(s, ConnSpec::Dangling)) {
                let seg = &mut segments[seg_of_row[i]];
                if seg.last_row == i {
                    seg.dangling_tail = true;
                }
            }
        }

        // ---- Pass 2: interval column allocation + colors. ---------------
        // Segments were created in nondecreasing `alloc_from` order (the
        // mainline at row 0, tips at their own row, spawns at their merge
        // row), so creation order IS the sweep order. Lowest-free-column
        // greedy over intervals is optimal: width == maximum number of
        // concurrently reserved columns, the mainline's whole-window
        // reservation included. The mainline is swept first from row 0, so
        // it takes column 0 and — reserved to the window's end — keeps it.
        let mut width: u32 = 0;
        {
            let mut free: BTreeSet<u32> = BTreeSet::new();
            let mut busy: BinaryHeap<Reverse<(usize, u32)>> = BinaryHeap::new();
            for seg in segments.iter_mut() {
                let start = seg.alloc_from;
                while let Some(&Reverse((until, column))) = busy.peek() {
                    if until < start {
                        busy.pop();
                        free.insert(column);
                    } else {
                        break;
                    }
                }
                let column = match free.iter().next().copied() {
                    Some(c) => {
                        free.remove(&c);
                        c
                    }
                    None => {
                        width += 1;
                        width - 1
                    }
                };
                seg.column = column;
                busy.push(Reverse((seg.alloc_until(window_end), column)));
            }
        }
        debug_assert_eq!(segments[MAINLINE_SEGMENT].column, MAINLINE_COLUMN);

        // Colors rotate through the palette, never handing a live neighbour's
        // color to a new segment while a free one remains: two same-colored
        // lanes crossing is exactly the case a reader cannot untangle. A
        // recycled column's next occupant additionally avoids its previous
        // occupant's color while the two are visually adjacent — same
        // column, same color, a row or two apart reads as ONE continuous
        // branch, which is a lie. When more segments overlap than the
        // palette has entries, the cursor's color is taken — a
        // deterministic, unavoidable collision. The mainline is swept first
        // with the cursor at MAINLINE_COLOR, so it always paints in that
        // slot and holds it for the whole window.
        {
            /// Rows between a column's previous occupant ending and its next
            /// beginning under which the two still read as one line.
            const ADJACENT_REUSE_ROWS: usize = 2;
            let palette = self.palette_size as usize;
            let mut in_use = vec![0u32; palette];
            let mut expiry: BinaryHeap<Reverse<(usize, u32)>> = BinaryHeap::new();
            let mut cursor: usize = MAINLINE_COLOR as usize;
            // Per column: (alloc_until, color) of its latest occupant so far.
            // Allocation order is start order, so this IS the predecessor.
            let mut column_history: Vec<Option<(usize, u32)>> = vec![None; width as usize];
            for seg in segments.iter_mut() {
                let start = seg.alloc_from;
                while let Some(&Reverse((until, color))) = expiry.peek() {
                    if until < start {
                        expiry.pop();
                        in_use[color as usize] = in_use[color as usize].saturating_sub(1);
                    } else {
                        break;
                    }
                }
                let avoid = column_history[seg.column as usize].and_then(|(until, color)| {
                    (start <= until.saturating_add(ADJACENT_REUSE_ROWS)).then_some(color)
                });
                let mut chosen = (cursor % palette) as u32;
                let mut found = false;
                for step in 0..palette {
                    let candidate = (cursor + step) % palette;
                    if in_use[candidate] == 0 && avoid != Some(candidate as u32) {
                        chosen = candidate as u32;
                        found = true;
                        break;
                    }
                }
                if !found {
                    // Every non-adjacent color is live: the predecessor's
                    // color beats colliding with a concurrently visible lane.
                    for step in 0..palette {
                        let candidate = (cursor + step) % palette;
                        if in_use[candidate] == 0 {
                            chosen = candidate as u32;
                            break;
                        }
                    }
                }
                seg.color = chosen;
                cursor = (chosen as usize + 1) % palette;
                in_use[chosen as usize] += 1;
                let until = seg.alloc_until(window_end);
                expiry.push(Reverse((until, chosen)));
                column_history[seg.column as usize] = Some((until, chosen));
            }
        }
        debug_assert_eq!(
            segments[MAINLINE_SEGMENT].color,
            MAINLINE_COLOR % self.palette_size
        );

        // ---- Pass 3: emit rows. ------------------------------------------
        // Sweep the visual spans so each row snapshots exactly the columns
        // occupied by a node, a pass-through, or a pending reservation —
        // sorted ascending by construction (BTreeMap keys).
        let mut starts_at: Vec<Vec<usize>> = vec![Vec::new(); n + 1];
        let mut ends_before: Vec<Vec<usize>> = vec![Vec::new(); n + 1];
        for (s, seg) in segments.iter().enumerate() {
            starts_at[seg.visual_from.min(n)].push(s);
            ends_before[(seg.last_row + 1).min(n)].push(s);
        }

        let mut active: BTreeMap<u32, u32> = BTreeMap::new(); // column -> color
        let mut visual_rows = Vec::with_capacity(n);
        for (i, commit) in commits.iter().enumerate() {
            for &s in &ends_before[i] {
                active.remove(&segments[s].column);
            }
            for &s in &starts_at[i] {
                active.insert(segments[s].column, segments[s].color);
            }

            let seg = &segments[seg_of_row[i]];
            let lane = seg.column;
            let color = seg.color;

            let mut active_lanes = Vec::with_capacity(active.len());
            let mut active_lane_colors = Vec::with_capacity(active.len());
            for (&column, &lane_color) in active.iter() {
                active_lanes.push(column);
                active_lane_colors.push(lane_color);
            }

            let connections = conn_specs[i]
                .iter()
                .enumerate()
                .map(|(k, spec)| match spec {
                    ConnSpec::Dangling => LaneConnection {
                        from_lane: lane,
                        to_lane: lane,
                        to_row_offset: 1,
                        is_merge: k > 0,
                        color_index: color,
                        is_dangling: true,
                    },
                    ConnSpec::Edge {
                        segment,
                        target_row,
                    } => LaneConnection {
                        from_lane: lane,
                        to_lane: segments[*segment].column,
                        to_row_offset: (target_row - i) as u32,
                        is_merge: k > 0,
                        // First-parent edges carry their row's color (the
                        // branch keeps its color while it descends, even
                        // into a merge point); merged-in edges carry the
                        // merged branch's color, so the peel reads as that
                        // branch arriving.
                        color_index: if k == 0 {
                            color
                        } else {
                            segments[*segment].color
                        },
                        is_dangling: false,
                    },
                })
                .collect();

            visual_rows.push(VisualCommitRow {
                id: commit.id.clone(),
                parent_ids: commit.parent_ids.clone(),
                summary: commit.summary.clone(),
                author_name: commit.author_name.clone(),
                author_email: commit.author_email.clone(),
                timestamp: commit.timestamp,
                lane,
                color_index: color,
                active_lanes,
                active_lane_colors,
                connections,
                is_merge: commit.parent_ids.len() > 1,
                is_root: commit.parent_ids.is_empty(),
                is_mainline: seg_of_row[i] == MAINLINE_SEGMENT,
            });
        }

        visual_rows
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

    /// The mainline's colour stays live while the merge chain descends. A
    /// branch peeling off the merge must not take the same colour while the
    /// palette still has a free slot.
    #[test]
    fn test_second_parent_colour_does_not_reuse_the_first_parent_edge_colour() {
        let mut solver = LaneSolver::new(4);
        let commits = vec![
            make_commit("keep", vec!["k"]),
            make_commit("park", vec!["m"]),
            make_commit("fill", vec!["f"]),
            make_commit("f", vec![]),
            make_commit("m", vec!["k", "s1", "s2", "s3"]),
            make_commit("k", vec![]),
            make_commit("s1", vec![]),
            make_commit("s2", vec![]),
            make_commit("s3", vec![]),
        ];
        let rows = solver.solve(&commits);
        let merge = rows.iter().find(|r| r.id == "m").expect("merge");
        let first = merge
            .connections
            .iter()
            .find(|c| !c.is_merge)
            .expect("first-parent edge");
        let merge_colors: HashSet<u32> = merge
            .connections
            .iter()
            .filter(|c| c.is_merge)
            .map(|c| c.color_index)
            .collect();
        assert!(
            !merge_colors.contains(&first.color_index),
            "a merged-in parent reused the still-visible first-parent colour {}",
            first.color_index
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
    fn test_diamond_second_parent_gets_a_distinct_column() {
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
        assert_ne!(
            merge_edge.to_lane, merge_edge.from_lane,
            "diamond second parent landed on the merge commit's own lane"
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

    #[test]
    fn test_octopus_merge_50_parents_stays_compact() {
        let mut solver = LaneSolver::new(12);
        let parents: Vec<String> = (1..=50).map(|i| format!("p{i}")).collect();
        let parent_refs: Vec<&str> = parents.iter().map(|s| s.as_str()).collect();
        let mut all_commits = vec![make_commit("merge", parent_refs)];
        for p in &parents {
            all_commits.push(make_commit(p, vec!["root"]));
        }
        all_commits.push(make_commit("root", vec![]));
        let rows = solver.solve(&all_commits);
        assert_eq!(rows.len(), 52);
        let mut lanes_used = HashSet::new();
        for row in &rows[1..=50] {
            lanes_used.insert(row.lane);
        }
        assert_eq!(lanes_used.len(), 50);
        assert_eq!(max_lane_used(&rows) + 1, 50);
    }

    #[test]
    fn test_parent_before_child_is_dangling_not_a_panic() {
        let mut solver = LaneSolver::new(12);
        let commits = vec![
            make_commit("parent", vec![]),
            make_commit("child", vec!["parent"]),
        ];
        let rows = solver.solve(&commits);
        assert_eq!(rows.len(), 2);
        assert!(rows[1].connections[0].is_dangling);
        assert_eq!(rows[1].connections[0].to_lane, rows[1].lane);
    }

    /// Shape: `e` reserves `m` on a high lane; `y` then frees a lower lane;
    /// `m`'s first parent `z` already lives on lane 0. The merged-in parent
    /// `t` must reuse the column `y` fully vacated instead of growing the
    /// graph — stable columns must not cost unbounded width.
    fn hole_then_merge_commits() -> Vec<RawCommitNode> {
        vec![
            make_commit("b1", vec!["z"]),     // b1 takes lane 0; z inherits lane 0
            make_commit("h", vec!["y"]),      // h takes lane 1; y inherits lane 1
            make_commit("e", vec!["m"]),      // e takes lane 2; m is pre-assigned lane 2
            make_commit("y", vec![]),         // y's own row ends lane 1's life
            make_commit("m", vec!["z", "t"]), // merge at lane 2; z already lives on lane 0
            make_commit("z", vec![]),
            make_commit("t", vec![]),
        ]
    }

    fn max_lane_used(rows: &[VisualCommitRow]) -> u32 {
        rows.iter()
            .flat_map(|r| {
                std::iter::once(r.lane)
                    .chain(r.active_lanes.iter().copied())
                    .chain(r.connections.iter().map(|c| c.to_lane))
            })
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn test_second_parent_reuses_a_hole_instead_of_growing() {
        let mut solver = LaneSolver::new(12);
        let rows = solver.solve(&hole_then_merge_commits());
        let merge_row = rows.iter().find(|r| r.id == "m").expect("merge row");
        let merge_edge = merge_row
            .connections
            .iter()
            .find(|c| c.is_merge)
            .expect("merge commit has a second-parent edge");
        let t_row = rows.iter().find(|r| r.id == "t").expect("t");
        assert_eq!(
            t_row.lane, merge_edge.to_lane,
            "t must occupy the lane the merge edge was drawn to"
        );
        assert!(
            merge_edge.to_lane < 3,
            "t grew the graph to lane {} instead of reusing a hole at or below the merge",
            merge_edge.to_lane
        );
        assert_eq!(
            max_lane_used(&rows),
            2,
            "t grew the graph to lane {} instead of reusing the hole left by y",
            max_lane_used(&rows)
        );
    }

    /// Merge reserved on lane 0; lane 2's occupant has fully ended while
    /// lane 1 is still live. The merged-in parent must land on the fully
    /// vacated column — never on the merge commit's own lane, and never on
    /// a new column.
    #[test]
    fn test_second_parent_prefers_a_hole_over_the_merge_lane() {
        let mut solver = LaneSolver::new(12);
        let commits = vec![
            make_commit("a", vec!["m"]),       // lane 0; m reserved on 0
            make_commit("b", vec!["pb"]),      // lane 1; pb reserved on 1
            make_commit("c", vec!["pc"]),      // lane 2; pc reserved on 2
            make_commit("pc", vec![]),         // lane 2's life ends
            make_commit("m", vec!["pb", "t"]), // merge on 0; pb already on 1
            make_commit("pb", vec![]),
            make_commit("t", vec![]),
        ];
        let rows = solver.solve(&commits);
        let merge_row = rows.iter().find(|r| r.id == "m").expect("merge");
        let merge_edge = merge_row
            .connections
            .iter()
            .find(|c| c.is_merge)
            .expect("second-parent edge");
        assert_ne!(
            merge_edge.to_lane, merge_edge.from_lane,
            "feature parent reused the merge commit's own lane while a hole existed"
        );
        assert_eq!(
            max_lane_used(&rows),
            2,
            "preferring a non-merge hole must not grow the graph (got width {})",
            max_lane_used(&rows) + 1
        );
    }

    /// Repeated "park a merge on a high reserved lane after lower lanes
    /// free" must not ratchet width: interval allocation recycles columns
    /// whose occupants fully ended, so a handful of live branches can never
    /// paint dozens of empty columns.
    #[test]
    fn test_repeated_merges_do_not_ratchet_width() {
        let mut commits = Vec::new();
        // Newest first: 24 cycles of (dummy tip, merge reserved high).
        for i in (0..24).rev() {
            let dummy = format!("d{i}");
            let dummy_parent = format!("dp{i}");
            let merge = format!("m{i}");
            let feature = format!("f{i}");
            let mainline = if i == 0 {
                "root".to_string()
            } else {
                format!("m{}", i - 1)
            };
            commits.push(make_commit(&format!("t{i}"), vec![&dummy]));
            commits.push(make_commit(&format!("e{i}"), vec![&merge]));
            commits.push(make_commit(&dummy, vec![&dummy_parent]));
            commits.push(make_commit(&dummy_parent, vec![]));
            commits.push(make_commit(&merge, vec![&mainline, &feature]));
            commits.push(make_commit(&feature, vec![]));
        }
        commits.push(make_commit("root", vec![]));

        let mut solver = LaneSolver::new(12);
        let rows = solver.solve(&commits);
        let max_lane = max_lane_used(&rows);
        let max_live = rows
            .iter()
            .map(|r| r.active_lanes.len() as u32)
            .max()
            .unwrap_or(0);
        assert!(
            max_lane < max_live + 2,
            "width {} for max live {} — lanes drifted right of live occupancy",
            max_lane + 1,
            max_live
        );
    }

    /// An empty parent id is not a real commit. The corresponding
    /// connection is still emitted (index k still matches parent_ids[k])
    /// but as a dangling stub on the child's own lane, and an ordinary
    /// segment's column continues straight into the first live parent
    /// instead of widening the graph.
    #[test]
    fn test_empty_parent_id_does_not_allocate_a_column() {
        let mut solver = LaneSolver::new(12);
        let commits = vec![make_commit("m", vec!["p", ""]), make_commit("p", vec![])];
        let rows = solver.solve(&commits);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].connections.len(), 2);
        assert!(rows[0].connections[1].is_dangling);
        assert_eq!(rows[0].connections[1].to_lane, rows[0].lane);
        assert_eq!(max_lane_used(&rows), 0);

        // Empty FIRST parent on a feature segment must not steal the
        // first-parent column either: `m` sits under a mainline tip, so it
        // is an ordinary segment and inherits its live merged-in parent.
        let mut solver = LaneSolver::new(12);
        let commits = vec![
            make_commit("tip", vec!["r"]),
            make_commit("m", vec!["", "p"]),
            make_commit("p", vec![]),
            make_commit("r", vec![]),
        ];
        let rows = solver.solve(&commits);
        assert!(rows[1].connections[0].is_dangling);
        assert!(!rows[1].connections[1].is_dangling);
        assert_eq!(rows[1].connections[0].to_lane, rows[1].lane);
        assert_eq!(rows[2].lane, rows[1].lane, "p continues m's own column");
        assert_eq!(max_lane_used(&rows), 1);
    }

    /// The mainline follows first parents strictly. With an empty FIRST
    /// parent on the mainline tip, the rail ends in a stub and the live
    /// merged-in parent opens its own column: painting it in the mainline
    /// colour on column 0 would present a merged branch as main's history.
    #[test]
    fn test_mainline_does_not_inherit_a_merged_in_parent_past_an_empty_first_parent() {
        let mut solver = LaneSolver::new(12);
        let commits = vec![make_commit("m", vec!["", "p"]), make_commit("p", vec![])];
        let rows = solver.solve(&commits);
        assert!(rows[0].is_mainline);
        assert!(rows[0].connections[0].is_dangling);
        assert_eq!(rows[0].connections[0].to_lane, MAINLINE_COLUMN);
        assert!(!rows[0].connections[1].is_dangling);
        assert_eq!(rows[0].connections[1].to_lane, 1);
        assert_eq!(rows[1].lane, 1);
        assert!(!rows[1].is_mainline);
        assert_ne!(rows[1].color_index, MAINLINE_COLOR);
    }

    /// Duplicate ids are corrupt input. Endpoint lookup must pin the FIRST
    /// occurrence so a later twin cannot steal edges and paint a line into
    /// the wrong row.
    #[test]
    fn test_duplicate_commit_id_pins_the_first_occurrence() {
        let mut solver = LaneSolver::new(12);
        let commits = vec![
            make_commit("child", vec!["dup"]),
            make_commit("dup", vec!["p"]),
            make_commit("dup", vec!["p"]),
            make_commit("p", vec![]),
        ];
        let rows = solver.solve(&commits);
        assert_eq!(rows.len(), 4);
        let edge = &rows[0].connections[0];
        assert!(!edge.is_dangling);
        assert_eq!(edge.to_row_offset, 1);
        assert_eq!(rows[1].id, "dup");
    }

    /// A feature merge whose first parent left the window continues
    /// straight down into its first live merged-in parent instead of
    /// peeling that branch onto a fresh column beside a stub.
    #[test]
    fn test_window_cut_first_parent_promotes_the_next_live_parent() {
        let mut solver = LaneSolver::new(12);
        let commits = vec![
            make_commit("tip", vec!["r"]),
            make_commit("m", vec!["ghost", "p"]),
            make_commit("p", vec![]),
            make_commit("r", vec![]),
        ];
        let rows = solver.solve(&commits);
        assert!(!rows[1].is_mainline, "precondition: m is a feature segment");
        assert!(rows[1].connections[0].is_dangling);
        let live = &rows[1].connections[1];
        assert!(!live.is_dangling);
        assert_eq!(
            live.to_lane, rows[1].lane,
            "the surviving parent must continue the merge's own column"
        );
        assert_eq!(rows[2].lane, rows[1].lane);
        assert_eq!(max_lane_used(&rows), 1);
    }

    /// The mainline never inherits: when the window cuts main's first
    /// parent under a merge, the rail ends with a stub and the merged-in
    /// branch opens its own column. Continuing column 0 in main's colour
    /// through a merged branch would present that branch as main's own
    /// history, and the rows would jump columns on the next "load more".
    #[test]
    fn test_mainline_ends_with_a_stub_at_a_window_cut_instead_of_inheriting() {
        let mut solver = LaneSolver::new(12);
        let commits = vec![
            make_commit("m", vec!["ghost", "p"]),
            make_commit("p", vec![]),
        ];
        let rows = solver.solve(&commits);
        assert!(rows[0].is_mainline);
        assert!(rows[0].connections[0].is_dangling);
        let live = &rows[0].connections[1];
        assert!(!live.is_dangling);
        assert_eq!(live.to_lane, 1, "the merged-in branch opens its own column");
        assert_eq!(rows[1].lane, 1);
        assert!(!rows[1].is_mainline);
        assert_ne!(rows[1].color_index, MAINLINE_COLOR);
    }

    fn hint(branch_tips: &[&str], fallback: Option<&str>) -> MainlineHint {
        MainlineHint {
            branch_tips: branch_tips.iter().map(|s| s.to_string()).collect(),
            fallback_tip: fallback.map(String::from),
        }
    }

    fn lane_of(rows: &[VisualCommitRow], id: &str) -> u32 {
        rows.iter().find(|r| r.id == id).expect(id).lane
    }

    fn mainline_ids(rows: &[VisualCommitRow]) -> Vec<&str> {
        rows.iter()
            .filter(|r| r.is_mainline)
            .map(|r| r.id.as_str())
            .collect()
    }

    /// The defect the pinned mainline exists to fix: `--topo-order` lists
    /// the merged feature `f2, f1` above the main commits `m2, m1`, so `f1`
    /// reaches the shared ancestor `m0` first in row order. Without a
    /// reservation, `f1` owned `m0` and main's own chain closed into the
    /// feature's column — main jogged sideways at its own fork point.
    #[test]
    fn test_feature_reaching_a_main_ancestor_first_does_not_displace_main() {
        let commits = vec![
            make_commit("f2", vec!["f1"]),
            make_commit("m2", vec!["m1"]),
            make_commit("f1", vec!["m0"]),
            make_commit("m1", vec!["m0"]),
            make_commit("m0", vec![]),
        ];
        let rows = LaneSolver::new(12).solve_with_mainline(&commits, &hint(&["m2"], None));
        assert_eq!(mainline_ids(&rows), vec!["m2", "m1", "m0"]);
        for id in ["m2", "m1", "m0"] {
            assert_eq!(
                lane_of(&rows, id),
                MAINLINE_COLUMN,
                "{id} left the mainline column"
            );
        }
        assert_eq!(lane_of(&rows, "f2"), 1);
        assert_eq!(lane_of(&rows, "f1"), 1);
        let f1 = rows.iter().find(|r| r.id == "f1").unwrap();
        let close = &f1.connections[0];
        assert_eq!(
            close.to_lane, MAINLINE_COLUMN,
            "f1 must close INTO main, not own m0"
        );
        assert_eq!(close.to_row_offset, 2);
        // The main chain's own edges are straight verticals.
        for id in ["m2", "m1"] {
            let row = rows.iter().find(|r| r.id == id).unwrap();
            assert_eq!(row.connections[0].to_lane, MAINLINE_COLUMN);
        }
        assert_eq!(
            max_lane_used(&rows),
            1,
            "two concurrent branches, two columns"
        );
    }

    /// Unhinted, the newest commit's chain is the mainline — and it is
    /// pre-reserved just the same, so even that chain can no longer be
    /// displaced by a sibling that reaches its ancestor first.
    #[test]
    fn test_unhinted_solve_pins_the_newest_commits_chain() {
        let commits = vec![
            make_commit("a2", vec!["a1"]),
            make_commit("b2", vec!["b1"]),
            make_commit("b1", vec!["base"]),
            make_commit("a1", vec!["base"]),
            make_commit("base", vec![]),
        ];
        let rows = LaneSolver::new(12).solve(&commits);
        assert_eq!(mainline_ids(&rows), vec!["a2", "a1", "base"]);
        for id in ["a2", "a1", "base"] {
            assert_eq!(lane_of(&rows, id), MAINLINE_COLUMN);
        }
        assert_eq!(lane_of(&rows, "b2"), 1);
        assert_eq!(lane_of(&rows, "b1"), 1);
    }

    /// When feature tips are newer than main's tip, main still owns column
    /// 0: the rows above its tip simply leave that column empty rather than
    /// letting a feature borrow it and forcing main to start further right.
    #[test]
    fn test_mainline_below_newer_tips_keeps_column_zero_empty_above_it() {
        let commits = vec![
            make_commit("f2", vec!["f1"]),
            make_commit("f1", vec!["m2"]),
            make_commit("m2", vec!["m1"]),
            make_commit("m1", vec![]),
        ];
        let rows = LaneSolver::new(12).solve_with_mainline(&commits, &hint(&["m2"], None));
        assert_eq!(mainline_ids(&rows), vec!["m2", "m1"]);
        assert_eq!(lane_of(&rows, "m2"), 0);
        assert_eq!(lane_of(&rows, "f2"), 1);
        assert_eq!(lane_of(&rows, "f1"), 1);
        for row in &rows[..2] {
            assert!(
                !row.active_lanes.contains(&MAINLINE_COLUMN),
                "column 0 must stay empty above the mainline tip, row {} shows it",
                row.id
            );
        }
        assert!(rows[2].active_lanes.contains(&MAINLINE_COLUMN));
        assert_eq!(rows[1].connections[0].to_lane, MAINLINE_COLUMN);
        assert_eq!(rows[0].color_index, rows[1].color_index);
        assert_ne!(rows[0].color_index, MAINLINE_COLOR);
    }

    /// A remote-tracking branch that is ahead of the local one continues
    /// the same rail: local `main` anchors, and the chain extends upward
    /// through `origin/main` because its first-parent chain passes through
    /// the anchor.
    #[test]
    fn test_remote_tracking_tip_ahead_of_local_extends_the_mainline() {
        let commits = vec![
            make_commit("o2", vec!["o1"]),
            make_commit("o1", vec!["m2"]),
            make_commit("m2", vec!["m1"]),
            make_commit("m1", vec![]),
        ];
        let rows = LaneSolver::new(12).solve_with_mainline(&commits, &hint(&["m2", "o2"], None));
        assert_eq!(mainline_ids(&rows), vec!["o2", "o1", "m2", "m1"]);
        assert!(rows.iter().all(|r| r.lane == MAINLINE_COLUMN));
        assert_eq!(max_lane_used(&rows), 0);
    }

    /// A diverged remote does not extend the rail: its chain never reaches
    /// the local tip, so it is an ordinary branch closing into main.
    #[test]
    fn test_diverged_remote_does_not_extend_the_mainline() {
        let commits = vec![
            make_commit("o1", vec!["m1"]),
            make_commit("m2", vec!["m1"]),
            make_commit("m1", vec![]),
        ];
        let rows = LaneSolver::new(12).solve_with_mainline(&commits, &hint(&["m2", "o1"], None));
        assert_eq!(mainline_ids(&rows), vec!["m2", "m1"]);
        assert_eq!(lane_of(&rows, "o1"), 1);
        assert_eq!(rows[0].connections[0].to_lane, MAINLINE_COLUMN);
    }

    /// The fallback tip anchors only when no branch tip is loaded, and is
    /// never extended through: HEAD on a feature forked from main's tip
    /// must not paint the feature as main.
    #[test]
    fn test_fallback_tip_anchors_without_extension() {
        let commits = vec![
            make_commit("h2", vec!["h1"]),
            make_commit("h1", vec!["m2"]),
            make_commit("m2", vec!["m1"]),
            make_commit("m1", vec![]),
        ];
        // Branch tip loaded: HEAD's chain passes through it but must not
        // extend the rail.
        let rows = LaneSolver::new(12).solve_with_mainline(&commits, &hint(&["m2"], Some("h2")));
        assert_eq!(mainline_ids(&rows), vec!["m2", "m1"]);
        assert_eq!(lane_of(&rows, "h2"), 1);

        // Branch tip absent: HEAD anchors.
        let rows =
            LaneSolver::new(12).solve_with_mainline(&commits, &hint(&["absent"], Some("h2")));
        assert_eq!(mainline_ids(&rows), vec!["h2", "h1", "m2", "m1"]);
        assert_eq!(max_lane_used(&rows), 0);

        // Nothing resolves: the newest commit anchors.
        let rows = LaneSolver::new(12)
            .solve_with_mainline(&commits, &hint(&["absent", ""], Some("also-absent")));
        assert_eq!(mainline_ids(&rows), vec!["h2", "h1", "m2", "m1"]);
    }

    /// The mainline always paints in palette slot 0, and no concurrent
    /// branch takes that slot while another is free.
    #[test]
    fn test_mainline_paints_in_colour_zero_and_neighbours_avoid_it() {
        let commits = vec![
            make_commit("f", vec!["m1"]),
            make_commit("g", vec!["m1"]),
            make_commit("m2", vec!["m1"]),
            make_commit("m1", vec![]),
        ];
        let rows = LaneSolver::new(4).solve_with_mainline(&commits, &hint(&["m2"], None));
        for row in &rows {
            if row.is_mainline {
                assert_eq!(row.color_index, MAINLINE_COLOR, "{} is off-palette", row.id);
            } else {
                assert_ne!(
                    row.color_index, MAINLINE_COLOR,
                    "{} stole main's colour",
                    row.id
                );
            }
        }
    }

    /// Column 0 belongs to the mainline for the whole window: nothing is
    /// drawn there below main's root, however much room that leaves.
    #[test]
    fn test_mainline_column_is_never_recycled_below_its_root() {
        let commits = vec![
            make_commit("m2", vec!["m1"]),
            make_commit("m1", vec![]),
            make_commit("orphan", vec![]),
            make_commit("other", vec!["other-root"]),
            make_commit("other-root", vec![]),
        ];
        let rows = LaneSolver::new(12).solve(&commits);
        assert_eq!(mainline_ids(&rows), vec!["m2", "m1"]);
        for row in &rows[2..] {
            assert_ne!(
                row.lane, MAINLINE_COLUMN,
                "{} was drawn on main's column",
                row.id
            );
            assert!(!row.active_lanes.contains(&MAINLINE_COLUMN));
        }
    }

    /// The row flags and the pinned chain agree with an independent walk of
    /// first parents from the tip, for a merge-heavy shape.
    #[test]
    fn test_is_mainline_matches_the_first_parent_chain_from_the_tip() {
        let commits = vec![
            make_commit("m3", vec!["m2", "f2"]),
            make_commit("f2", vec!["f1"]),
            make_commit("m2", vec!["m1", "g1"]),
            make_commit("g1", vec!["m1"]),
            make_commit("f1", vec!["m1"]),
            make_commit("m1", vec!["m0"]),
            make_commit("m0", vec![]),
        ];
        let rows = LaneSolver::new(12).solve_with_mainline(&commits, &hint(&["m3"], None));
        assert_eq!(mainline_ids(&rows), vec!["m3", "m2", "m1", "m0"]);
        for row in &rows {
            assert_eq!(row.is_mainline, row.lane == MAINLINE_COLUMN, "{}", row.id);
            for conn in &row.connections {
                if conn.is_dangling {
                    continue;
                }
                let target = &rows[rows.iter().position(|r| r.id == row.id).unwrap()
                    + conn.to_row_offset as usize];
                assert_eq!(conn.to_lane, target.lane, "{} -> {}", row.id, target.id);
            }
        }
        // Both merged-in branches peel right and close back into column 0.
        assert_eq!(lane_of(&rows, "f2"), 1);
        assert_eq!(lane_of(&rows, "f1"), 1);
        assert_eq!(lane_of(&rows, "g1"), 2);
    }
}
