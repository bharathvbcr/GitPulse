//! Scale and pathology stress for the stable-column lane solver.
//!
//! The fuzz suite covers the input space at small sizes; this suite covers
//! the AXES that break allocators: history depth (100k rows), merge arity
//! (500-parent octopus), reservation span (a column held across 50k rows),
//! and corrupt-input storms (duplicate ids, empty parents). Time bounds
//! are deliberately generous — they exist to catch an accidental O(n²)
//! regression, not to benchmark.

use gitpulse_lib::graph::{LaneSolver, RawCommitNode, VisualCommitRow};
use std::collections::HashSet;
use std::time::{Duration, Instant};

fn make_commit(id: String, parents: Vec<String>) -> RawCommitNode {
    RawCommitNode {
        summary: format!("commit {id}"),
        id,
        parent_ids: parents,
        timestamp: 0,
        author_name: "Stress".to_string(),
        author_email: "stress@example.com".to_string(),
    }
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

fn assert_basic_row_sanity(rows: &[VisualCommitRow]) {
    for (i, row) in rows.iter().enumerate() {
        assert!(
            row.active_lanes.contains(&row.lane),
            "row {i} lane missing from active_lanes"
        );
        assert!(
            row.active_lanes.windows(2).all(|w| w[0] < w[1]),
            "row {i} active_lanes not strictly ascending"
        );
        assert_eq!(row.active_lanes.len(), row.active_lane_colors.len());
        assert_eq!(row.connections.len(), row.parent_ids.len());
    }
}

fn timed(
    label: &str,
    budget: Duration,
    f: impl FnOnce() -> Vec<VisualCommitRow>,
) -> Vec<VisualCommitRow> {
    let start = Instant::now();
    let rows = f();
    let elapsed = start.elapsed();
    assert!(
        elapsed < budget,
        "{label} took {elapsed:?} (budget {budget:?}) — a complexity regression, not a slow machine"
    );
    rows
}

#[test]
fn linear_100k_rows_solve_on_one_lane_in_linear_time() {
    const N: usize = 100_000;
    let commits: Vec<RawCommitNode> = (0..N)
        .map(|i| {
            let parents = if i + 1 < N {
                vec![format!("c{}", i + 1)]
            } else {
                Vec::new()
            };
            make_commit(format!("c{i}"), parents)
        })
        .collect();
    let rows = timed("linear 100k", Duration::from_secs(10), || {
        LaneSolver::new(12).solve(&commits)
    });
    assert_eq!(rows.len(), N);
    assert!(
        rows.iter().all(|r| r.lane == 0),
        "linear history must stay on lane 0"
    );
    assert!(
        rows[..N - 1].iter().all(|r| r.connections.len() == 1
            && !r.connections[0].is_dangling
            && r.connections[0].to_row_offset == 1),
        "linear edges must all be adjacent and live"
    );
    assert_eq!(max_lane_used(&rows), 0);
}

#[test]
fn merge_heavy_100k_rows_stay_narrow_and_fast() {
    // ~16.6k cycles of the 6-commit merge pattern from the unit suite:
    // a mainline of merges with a feature and a dummy branch per cycle.
    const CYCLES: usize = 16_600;
    let mut commits = Vec::with_capacity(CYCLES * 6 + 1);
    for i in (0..CYCLES).rev() {
        let mainline = if i == 0 {
            "root".to_string()
        } else {
            format!("m{}", i - 1)
        };
        commits.push(make_commit(format!("t{i}"), vec![format!("d{i}")]));
        commits.push(make_commit(format!("e{i}"), vec![format!("m{i}")]));
        commits.push(make_commit(format!("d{i}"), vec![format!("dp{i}")]));
        commits.push(make_commit(format!("dp{i}"), vec![]));
        commits.push(make_commit(
            format!("m{i}"),
            vec![mainline, format!("f{i}")],
        ));
        commits.push(make_commit(format!("f{i}"), vec![]));
    }
    commits.push(make_commit("root".to_string(), vec![]));

    let rows = timed("merge-heavy 100k", Duration::from_secs(15), || {
        LaneSolver::new(12).solve(&commits)
    });
    assert_eq!(rows.len(), commits.len());
    assert_basic_row_sanity(&rows);
    let width = max_lane_used(&rows) + 1;
    assert!(
        width <= 6,
        "a bounded repeating pattern must keep bounded width, got {width}"
    );

    // Determinism at scale.
    let again = LaneSolver::new(12).solve(&commits);
    assert_eq!(rows, again, "100k-row solve is nondeterministic");
}

#[test]
fn octopus_500_parents_allocates_500_distinct_columns() {
    const P: usize = 500;
    let parents: Vec<String> = (0..P).map(|i| format!("p{i}")).collect();
    let mut commits = vec![make_commit("m".to_string(), parents.clone())];
    for p in &parents {
        commits.push(make_commit(p.clone(), vec!["root".to_string()]));
    }
    commits.push(make_commit("root".to_string(), vec![]));

    let rows = timed("octopus 500", Duration::from_secs(5), || {
        LaneSolver::new(12).solve(&commits)
    });
    assert_basic_row_sanity(&rows);
    let lanes: HashSet<u32> = rows[1..=P].iter().map(|r| r.lane).collect();
    assert_eq!(lanes.len(), P, "each octopus parent needs its own column");
    assert_eq!(
        max_lane_used(&rows) + 1,
        P as u32,
        "width must equal the genuine peak"
    );
}

/// A column reserved across 50k rows: a merge pulls in a branch whose only
/// commit sits at the bottom of the window. Every intermediate row must
/// carry the reservation (that is what the renderer draws and the hit-test
/// reads), and the solve must stay linear despite the span.
#[test]
fn reservation_spanning_50k_rows_is_active_the_whole_way() {
    const N: usize = 50_002;
    let mut commits = Vec::with_capacity(N);
    commits.push(make_commit(
        "m".to_string(),
        vec!["f0".to_string(), "far".to_string()],
    ));
    for i in 0..N - 3 {
        commits.push(make_commit(format!("f{i}"), vec![format!("f{}", i + 1)]));
    }
    commits.push(make_commit(format!("f{}", N - 3), vec![]));
    commits.push(make_commit("far".to_string(), vec![]));

    let rows = timed("50k-span reservation", Duration::from_secs(10), || {
        LaneSolver::new(12).solve(&commits)
    });
    let far = rows.last().expect("far row");
    assert_eq!(far.id, "far");
    for probe in [1usize, 1_000, 25_000, N - 2] {
        assert!(
            rows[probe].active_lanes.contains(&far.lane),
            "row {probe} lost the 50k-row reservation for column {}",
            far.lane
        );
    }
    let merge_edge = rows[0]
        .connections
        .iter()
        .find(|c| c.is_merge)
        .expect("merge edge");
    assert_eq!(merge_edge.to_lane, far.lane);
    assert_eq!(merge_edge.to_row_offset as usize, N - 1);
}

#[test]
fn duplicate_id_storm_is_deterministic_and_bounded() {
    const N: usize = 10_000;
    let commits: Vec<RawCommitNode> = (0..N)
        .map(|_| make_commit("dup".to_string(), vec!["dup".to_string()]))
        .collect();
    let rows = timed("duplicate-id storm", Duration::from_secs(5), || {
        LaneSolver::new(12).solve(&commits)
    });
    assert_eq!(rows.len(), N);
    assert_basic_row_sanity(&rows);
    // Every parent reference resolves to the FIRST occurrence (row 0),
    // which is never below any row — every edge must dangle.
    assert!(
        rows.iter()
            .all(|r| r.connections.iter().all(|c| c.is_dangling)),
        "self/duplicate parents must all dangle"
    );
    // Stub padding gives each one-row segment a two-row footprint, and
    // column 0 belongs to the pinned mainline (row 0's chain) for the whole
    // window, so the storm alternates on the two columns beside it. It
    // still may not grow past that overlap.
    assert!(max_lane_used(&rows) <= 2, "duplicate storm leaked columns");
    assert!(
        rows[1..].iter().all(|r| r.lane != 0),
        "a storm row was drawn on the mainline column"
    );
    let again = LaneSolver::new(12).solve(&commits);
    assert_eq!(rows, again);
}

#[test]
fn empty_parent_storm_never_allocates_columns() {
    const N: usize = 10_000;
    let commits: Vec<RawCommitNode> = (0..N)
        .map(|i| {
            make_commit(
                format!("c{i}"),
                vec![String::new(), String::new(), String::new()],
            )
        })
        .collect();
    let rows = timed("empty-parent storm", Duration::from_secs(5), || {
        LaneSolver::new(12).solve(&commits)
    });
    assert_basic_row_sanity(&rows);
    for row in &rows {
        assert_eq!(row.connections.len(), 3);
        for conn in &row.connections {
            assert!(conn.is_dangling);
            assert_eq!(conn.to_lane, row.lane, "empty parent invented a column");
        }
    }
    // Two-row stub footprints alternate on the two columns beside the
    // pinned mainline column (row 0's, for the whole window).
    assert!(
        max_lane_used(&rows) <= 2,
        "empty-parent storm leaked columns"
    );
    assert!(
        rows[1..].iter().all(|r| r.lane != 0),
        "a storm row was drawn on the mainline column"
    );
}

/// Fifty branches all closing into one deep parent while unrelated tips
/// come and go: every closing column stays reserved for its whole descent
/// (no filler may land on one), and the fillers all share ONE recycled
/// column beyond them — exclusivity without width creep.
#[test]
fn fifty_in_flight_closes_hold_their_columns_against_churn() {
    const CLOSERS: usize = 50;
    const FILLERS: usize = 100;
    let mut commits = Vec::new();
    for i in 0..CLOSERS {
        commits.push(make_commit(format!("t{i}"), vec!["base".to_string()]));
    }
    for i in 0..FILLERS {
        commits.push(make_commit(format!("fill{i}"), vec![]));
    }
    commits.push(make_commit("base".to_string(), vec![]));

    let rows = LaneSolver::new(12).solve(&commits);
    assert_basic_row_sanity(&rows);
    let base_idx = rows.len() - 1;

    let closer_lanes: HashSet<u32> = rows[..CLOSERS].iter().map(|r| r.lane).collect();
    assert_eq!(
        closer_lanes.len(),
        CLOSERS,
        "closers must hold distinct columns"
    );
    for row in &rows[CLOSERS..CLOSERS + FILLERS] {
        assert!(
            !closer_lanes.contains(&row.lane),
            "filler {} landed on a column with an in-flight close",
            row.id
        );
    }
    // t0 continues into base (first claim); t1..t49 close into t0's column
    // at base's row. 49 descents + t0's own line + one filler column.
    assert_eq!(
        max_lane_used(&rows) + 1,
        CLOSERS as u32 + 1,
        "fillers must recycle one column beyond the closers, not grow"
    );
    let t1 = &rows[1];
    assert_eq!(
        1 + t1.connections[0].to_row_offset as usize,
        base_idx,
        "closes must land on base"
    );
}
