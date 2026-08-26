//! Stable-column layout contract (GitKraken-style).
//!
//! These tests encode the visual guarantees a readable commit graph must
//! hold. They were written against the OLD greedy first-fit solver to
//! demonstrate its defects before the interval-based rework landed:
//!
//! 1. **Connector-column exclusivity** — when a branch's last commit closes
//!    into a parent on another lane, its own column is still visually
//!    occupied by the descending connector until the merge point. The old
//!    solver freed the column at the child's row, so an unrelated branch
//!    born one row later was drawn straight through the connector — the
//!    "branch disconnect / overlap" artifact.
//! 2. **Lane and colour stability** — a branch keeps one column and one
//!    colour for its entire first-parent life while uncontested.
//! 3. **Endpoint truth** — every live edge lands on the column its parent
//!    commit actually occupies.
//! 4. **Width optimality** — the graph is exactly as wide as its peak
//!    concurrent occupancy (nodes, pass-throughs, and in-flight
//!    connectors), never wider.

use gitpulse_lib::graph::{LaneSolver, RawCommitNode, VisualCommitRow};
use std::collections::{BTreeMap, BTreeSet};

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

fn solve(commits: &[RawCommitNode]) -> Vec<VisualCommitRow> {
    LaneSolver::new(12).solve(commits)
}

fn row<'a>(rows: &'a [VisualCommitRow], id: &str) -> &'a VisualCommitRow {
    rows.iter()
        .find(|r| r.id == id)
        .unwrap_or_else(|| panic!("row {id} missing"))
}

fn row_idx(rows: &[VisualCommitRow], id: &str) -> usize {
    rows.iter()
        .position(|r| r.id == id)
        .unwrap_or_else(|| panic!("row {id} missing"))
}

/// Every column visually occupied at each row: node lanes, pass-through
/// lanes, and the columns of connectors in flight (closing descents and
/// merge peels between their child and parent rows).
fn occupied_columns_by_row(rows: &[VisualCommitRow]) -> Vec<BTreeSet<u32>> {
    let mut occupied: Vec<BTreeSet<u32>> = rows
        .iter()
        .map(|r| {
            let mut set: BTreeSet<u32> = r.active_lanes.iter().copied().collect();
            set.insert(r.lane);
            set
        })
        .collect();
    for (i, r) in rows.iter().enumerate() {
        for conn in &r.connections {
            if conn.is_dangling {
                continue;
            }
            let target = i + conn.to_row_offset as usize;
            if target >= rows.len() {
                continue;
            }
            for occupied_at_row in occupied.iter_mut().take(target + 1).skip(i) {
                occupied_at_row.insert(conn.from_lane);
                occupied_at_row.insert(conn.to_lane);
            }
        }
    }
    occupied
}

fn max_lane_used(rows: &[VisualCommitRow]) -> u32 {
    rows.iter()
        .flat_map(|r| {
            std::iter::once(r.lane)
                .chain(r.active_lanes.iter().copied())
                .chain(
                    r.connections
                        .iter()
                        .filter(|c| !c.is_dangling)
                        .map(|c| c.to_lane),
                )
        })
        .max()
        .unwrap_or(0)
}

/// The defect this rework exists to fix, in its smallest reproduction.
///
/// `x` closes into `b` (main's pending parent) across three rows. The old
/// solver freed x's column the moment x's row was processed, so the fresh
/// tip `y` — born one row below — was allocated the SAME column and drawn
/// on top of x's still-descending connector, together with its parent `t`.
///
/// History (newest first):
/// ```text
/// a [b]   main tip, lane 0
/// x [b]   branch, closes into b — its connector descends 3 rows
/// y [t]   unrelated tip born while x's connector is in flight
/// t []
/// b []    main
/// ```
#[test]
fn closing_connector_column_is_not_reused_while_in_flight() {
    let commits = vec![
        make_commit("a", vec!["b"]),
        make_commit("x", vec!["b"]),
        make_commit("y", vec!["t"]),
        make_commit("t", vec![]),
        make_commit("b", vec![]),
    ];
    let rows = solve(&commits);

    let x = row(&rows, "x");
    let close = &x.connections[0];
    assert_ne!(
        close.to_lane, close.from_lane,
        "x must close into main's column, not its own"
    );
    let x_idx = row_idx(&rows, "x");
    let b_idx = row_idx(&rows, "b");
    assert_eq!(x_idx + close.to_row_offset as usize, b_idx);

    // No commit between x and b may sit on x's connector column.
    for (j, intermediate) in rows.iter().enumerate().take(b_idx).skip(x_idx + 1) {
        assert_ne!(
            intermediate.lane, close.from_lane,
            "row {} ({}) was drawn on top of x's in-flight closing connector (column {})",
            j, intermediate.id, close.from_lane
        );
        assert!(
            !intermediate.active_lanes.contains(&close.from_lane)
                || intermediate.active_lane_colors[intermediate
                    .active_lanes
                    .iter()
                    .position(|&l| l == close.from_lane)
                    .unwrap()]
                    == close.color_index,
            "row {} ({}) routed a different branch through x's connector column",
            j,
            intermediate.id
        );
    }

    // Three things are genuinely concurrent on rows y/t: main's line, x's
    // descent, and the y branch. Optimal width is exactly 3 — no more.
    assert_eq!(
        max_lane_used(&rows) + 1,
        3,
        "width must equal peak occupancy"
    );
}

/// Same shape, one level deeper: TWO branches close into main while two
/// fresh tips appear. Neither tip may land on either in-flight connector.
#[test]
fn multiple_in_flight_connectors_all_keep_their_columns() {
    let commits = vec![
        make_commit("a", vec!["base"]),
        make_commit("x1", vec!["base"]),
        make_commit("x2", vec!["base"]),
        make_commit("y1", vec!["t1"]),
        make_commit("y2", vec!["t2"]),
        make_commit("t1", vec![]),
        make_commit("t2", vec![]),
        make_commit("base", vec![]),
    ];
    let rows = solve(&commits);
    let base_idx = row_idx(&rows, "base");

    for closer in ["x1", "x2"] {
        let c = row(&rows, closer);
        let idx = row_idx(&rows, closer);
        let conn = &c.connections[0];
        assert!(!conn.is_dangling);
        for (j, intermediate) in rows.iter().enumerate().take(base_idx).skip(idx + 1) {
            assert_ne!(
                intermediate.lane, conn.from_lane,
                "row {} ({}) overlaps {}'s closing connector",
                j, intermediate.id, closer
            );
        }
    }
}

/// A branch's first-parent chain keeps one lane and one colour while no
/// other child contests the parent.
#[test]
fn uncontested_first_parent_chain_is_lane_and_colour_stable() {
    // Two parallel chains, interleaved so reservations cross.
    let commits = vec![
        make_commit("a3", vec!["a2"]),
        make_commit("b3", vec!["b2"]),
        make_commit("a2", vec!["a1"]),
        make_commit("b2", vec!["b1"]),
        make_commit("a1", vec![]),
        make_commit("b1", vec![]),
    ];
    let rows = solve(&commits);
    for chain in [["a3", "a2", "a1"], ["b3", "b2", "b1"]] {
        let lanes: BTreeSet<u32> = chain.iter().map(|id| row(&rows, id).lane).collect();
        let colors: BTreeSet<u32> = chain.iter().map(|id| row(&rows, id).color_index).collect();
        assert_eq!(lanes.len(), 1, "chain {:?} moved lanes: {:?}", chain, lanes);
        assert_eq!(
            colors.len(),
            1,
            "chain {:?} changed colour: {:?}",
            chain,
            colors
        );
    }
    let (a, b) = (row(&rows, "a3"), row(&rows, "b3"));
    assert_ne!(a.lane, b.lane, "parallel chains must not share a lane");
    assert_ne!(
        a.color_index, b.color_index,
        "parallel chains must not share a colour"
    );
}

/// Every live edge must land on the column its parent commit occupies.
#[test]
fn live_edges_land_on_the_parent_commit_column() {
    let commits = vec![
        make_commit("m", vec!["p", "q"]),
        make_commit("side", vec!["q"]),
        make_commit("p", vec!["r"]),
        make_commit("q", vec![]),
        make_commit("r", vec![]),
    ];
    let rows = solve(&commits);
    for (i, r) in rows.iter().enumerate() {
        for (k, conn) in r.connections.iter().enumerate() {
            if conn.is_dangling {
                continue;
            }
            let target = &rows[i + conn.to_row_offset as usize];
            assert_eq!(target.id, r.parent_ids[k]);
            assert_eq!(
                conn.to_lane, target.lane,
                "edge {}→{} points at column {} but the parent sits on {}",
                r.id, target.id, conn.to_lane, target.lane
            );
        }
    }
}

/// Width equals peak concurrent occupancy — computed independently from the
/// output — on a merge-heavy handcrafted history.
#[test]
fn width_is_exactly_peak_occupancy() {
    let commits = vec![
        make_commit("m2", vec!["m1", "f2"]),
        make_commit("f2", vec!["m1"]),
        make_commit("m1", vec!["m0", "f1"]),
        make_commit("f1", vec!["m0"]),
        make_commit("m0", vec![]),
    ];
    let rows = solve(&commits);
    let occupied = occupied_columns_by_row(&rows);
    let peak = occupied.iter().map(|s| s.len()).max().unwrap_or(0);
    assert_eq!(
        (max_lane_used(&rows) + 1) as usize,
        peak,
        "graph width {} != peak concurrent occupancy {}",
        max_lane_used(&rows) + 1,
        peak
    );
}

/// A column freed by a finished branch may be reused by a LATER branch —
/// stability must not cost unbounded width. The reuse is only legal once
/// the previous occupant (including any connector still descending) is
/// fully gone.
#[test]
fn columns_are_recycled_after_the_occupant_fully_ends() {
    // b1 lives rows 1..2 and dies; b2 is born well after and must reuse
    // b1's column instead of growing the graph.
    let commits = vec![
        make_commit("a5", vec!["a4"]),
        make_commit("b1", vec!["b0"]),
        make_commit("a4", vec!["a3"]),
        make_commit("b0", vec![]),
        make_commit("a3", vec!["a2"]),
        make_commit("b2", vec!["bb"]),
        make_commit("a2", vec!["a1"]),
        make_commit("bb", vec![]),
        make_commit("a1", vec![]),
    ];
    let rows = solve(&commits);
    assert_eq!(
        max_lane_used(&rows),
        1,
        "a branch born after another fully ended must reuse its column"
    );
}

/// A column's next occupant must not inherit its previous occupant's
/// colour while the two are visually adjacent and a free colour exists:
/// same column + same colour + a one-or-two-row gap reads as ONE
/// continuous branch, which is a lie.
///
/// Construction (palette 3): main M holds colour 0 throughout; branch A
/// takes colour 1 and dies; short branch C takes colour 2 and dies; B is
/// then born two rows under A in A's recycled column. Plain rotation
/// hands B colour 1 — A's — while colour 2 sits free.
#[test]
fn adjacent_column_reuse_does_not_inherit_the_previous_colour() {
    let commits = vec![
        make_commit("m4", vec!["m3"]), // row 0: M col0
        make_commit("a2", vec!["a1"]), // row 1: A tip
        make_commit("c1", vec![]),     // row 2: C tip+root
        make_commit("a1", vec![]),     // row 3: A ends
        make_commit("m3", vec!["m2"]), // row 4: M
        make_commit("b2", vec!["b1"]), // row 5: B tip, reuses A's column
        make_commit("b1", vec![]),     // row 6: B ends
        make_commit("m2", vec!["m1"]),
        make_commit("m1", vec!["m0"]),
        make_commit("m0", vec![]),
    ];
    let rows = LaneSolver::new(3).solve(&commits);
    let a = row(&rows, "a2");
    let b = row(&rows, "b2");
    assert_eq!(
        a.lane, b.lane,
        "precondition: B must reuse A's recycled column for this test to bite"
    );
    assert_ne!(
        a.color_index, b.color_index,
        "B inherited A's colour {} in the same column two rows later — \
         two unrelated branches now read as one line (a free colour existed)",
        a.color_index
    );
}

/// No two distinct branches may ever be drawn in the same column on the
/// same row. Reconstructed from output alone: a column is "claimed" at a
/// row by a node, a pass-through, or an in-flight connector; a claim by
/// two different colours is exactly the overlap artifact.
#[test]
fn no_two_branches_share_a_column_on_any_row() {
    let commits = vec![
        make_commit("a", vec!["b"]),
        make_commit("x", vec!["b"]),
        make_commit("y", vec!["t"]),
        make_commit("z", vec!["t"]),
        make_commit("t", vec![]),
        make_commit("b", vec![]),
    ];
    let rows = solve(&commits);

    // column -> colour claims per row
    for (i, r) in rows.iter().enumerate() {
        let mut claims: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
        claims.entry(r.lane).or_default().insert(r.color_index);
        for (l, &lane) in r.active_lanes.iter().enumerate() {
            claims
                .entry(lane)
                .or_default()
                .insert(r.active_lane_colors[l]);
        }
        // connectors crossing row i
        for (j, other) in rows.iter().enumerate().take(i) {
            for conn in &other.connections {
                if conn.is_dangling {
                    continue;
                }
                let target = j + conn.to_row_offset as usize;
                if target <= i || target >= rows.len() {
                    continue;
                }
                // in flight across row i: it occupies from_lane (closing)
                // or to_lane (merge peel) depending on shape; both recorded.
                let lane = if conn.is_merge {
                    conn.to_lane
                } else {
                    conn.from_lane
                };
                claims.entry(lane).or_default().insert(conn.color_index);
            }
        }
        for (col, colors) in claims {
            assert!(
                colors.len() <= 1,
                "row {} ({}): column {} claimed by {} different branches (colours {:?})",
                i,
                r.id,
                col,
                colors.len(),
                colors
            );
        }
    }
}
