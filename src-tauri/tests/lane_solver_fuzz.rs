//! Randomized property-based stress tests for the graph lane solver.
//!
//! Deterministic by construction: every case is derived from a seed fed into
//! an inline LCG (`state = state * 6364136223846793005 + 1442695040888963407`,
//! wrapping u64 arithmetic), so any failure prints the seed that reproduces
//! it. No external property-testing crates are used.
//!
//! # Merge fan-out semantics (verified against lane_solver.rs)
//!
//! For a second-or-later parent edge (`is_merge == true`) of row `i` with
//! parent id `pk`, the solver does exactly one of:
//!
//! 1. **Reuse an existing column** — `find_column(pk)` succeeds because some
//!    earlier row already reserved `pk`. The reused column can sit anywhere
//!    relative to the merge commit: left of it, on it, or right of it. Only
//!    one relation is impossible: `to_lane == from_lane`. While iteration
//!    `k > 0` runs, the merge commit's own lane is either empty (its first
//!    parent lived elsewhere) or holds `parent_ids[0]`; a *distinct* `pk`
//!    therefore can never be found there.
//! 2. **Allocate a fresh column** — `find_column(pk)` fails, so
//!    `find_or_create_free_column_strictly_after(current_lane + 1)` places
//!    `pk` at a column `>= current_lane + 1`: strictly right of the merge.
//!
//! From the output alone the two cases are indistinguishable, but the input
//! tells them apart: `pk` has a reserved column at row `i` iff some earlier
//! row mentioned `pk` as a parent (a reservation for a still-pending commit
//! cannot be freed while later references remain). Hence the encoded
//! invariant:
//!
//! - `pk != parent_ids[0]` and no earlier row mentions `pk` → `to_lane > from_lane`
//!   (fresh allocation must be strictly-after);
//! - `pk != parent_ids[0]` and some earlier row mentions `pk` → `to_lane != from_lane`
//!   (pre-existing column, left or right but never the merge's own lane);
//! - `pk == parent_ids[0]` (duplicated first parent) → unconstrained; landing
//!   on the merge's own lane is the legitimate outcome, because iteration 0
//!   just placed `pk` there.
//!
//! Asserting plain `to_lane >= from_lane` (or `>`) on all merge edges would be
//! wrong: pre-existing columns left of the merge are legal by design.

use gitpulse_lib::graph::{LaneSolver, RawCommitNode, VisualCommitRow};

const LCG_MUL: u64 = 6364136223846793005;
const LCG_INC: u64 = 1442695040888963407;

struct Lcg(u64);

impl Lcg {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(LCG_MUL).wrapping_add(LCG_INC);
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound
    }
}

/// A generated history plus metadata the checker needs.
struct GeneratedHistory {
    commits: Vec<RawCommitNode>,
    /// Upper bound on column count: every commit id reserves at most one
    /// column per solve (re-reservation after a free is impossible because a
    /// reservation is only freed once all its references are consumed), so
    /// pushes are bounded by the number of distinct ids ever mentioned.
    max_columns: usize,
}

fn make_node(id: String, parent_ids: Vec<String>, tick: i64) -> RawCommitNode {
    RawCommitNode {
        summary: format!("commit {id}"),
        id,
        parent_ids,
        timestamp: tick,
        author_name: "Fuzz Author".to_string(),
        author_email: "fuzz@example.com".to_string(),
    }
}

/// Valid DAG in topological order: every parent of node `i` sits at an index
/// `> i`. Parent-count distribution per node: 0 @10%, 1 @70%, 2 @15%,
/// 3..=6 @5%. Multi-parent nodes duplicate their first parent ~2% of the time
/// (the same id twice in one list), which exercises the duplicate-parent path.
fn generate_history(seed: u64) -> GeneratedHistory {
    let mut rng = Lcg(seed ^ 0x9E37_79B9_7F4A_7C15);
    let node_count = (1 + rng.below(120)) as usize;
    let mut commits = Vec::with_capacity(node_count);

    for i in 0..node_count {
        let later = node_count - i - 1;
        let parent_count = if later == 0 {
            0
        } else {
            match rng.below(100) {
                0..=9 => 0,
                10..=79 => 1,
                80..=94 => 2,
                _ => (3 + rng.below(4)) as usize,
            }
            .min(later)
        };

        let mut picks: Vec<usize> = Vec::with_capacity(parent_count);
        while picks.len() < parent_count {
            let candidate = i + 1 + rng.below(later as u64) as usize;
            if !picks.contains(&candidate) {
                picks.push(candidate);
            }
        }
        let mut parent_ids: Vec<String> = picks.into_iter().map(|p| format!("n{p}")).collect();
        if parent_ids.len() >= 2 && rng.below(100) < 2 {
            let last = parent_ids.len() - 1;
            parent_ids[last] = parent_ids[0].clone();
        }

        commits.push(make_node(format!("n{i}"), parent_ids, 1_000 - i as i64));
    }

    GeneratedHistory {
        commits,
        max_columns: node_count,
    }
}

/// Like [`generate_history`] but ~25% of parented nodes get all their parents
/// replaced by ids that never appear as rows ("ghosts"), exercising the
/// dangling-edge stub path alongside ordinary edges.
fn generate_history_with_ghost_parents(seed: u64) -> GeneratedHistory {
    let mut rng = Lcg(seed ^ 0x0DDB_1A5E_5EED_0001);
    let node_count = (1 + rng.below(120)) as usize;
    let mut commits = Vec::with_capacity(node_count);
    let mut ghost_counter = 0usize;

    for i in 0..node_count {
        let later = node_count - i - 1;
        let parent_count = if later == 0 {
            0
        } else {
            match rng.below(100) {
                0..=9 => 0,
                10..=79 => 1,
                80..=94 => 2,
                _ => (3 + rng.below(4)) as usize,
            }
            .min(later)
        };

        let mut picks: Vec<usize> = Vec::with_capacity(parent_count);
        while picks.len() < parent_count {
            let candidate = i + 1 + rng.below(later as u64) as usize;
            if !picks.contains(&candidate) {
                picks.push(candidate);
            }
        }

        let ghosted = parent_count > 0 && rng.below(100) < 25;
        let parent_ids: Vec<String> = if ghosted {
            (0..parent_count)
                .map(|_| {
                    ghost_counter += 1;
                    format!("ghost{}", ghost_counter - 1)
                })
                .collect()
        } else {
            picks.into_iter().map(|p| format!("n{p}")).collect()
        };

        commits.push(make_node(format!("n{i}"), parent_ids, 1_000 - i as i64));
    }

    GeneratedHistory {
        commits,
        // Node ids plus ghost ids: ghosts reserve columns too.
        max_columns: node_count + ghost_counter,
    }
}

/// Asserts every structural invariant on an already-produced solve result.
/// Panics with the seed and a dump of the offending row/connection; the input
/// is fully reproducible from the seed.
fn check_all_invariants(
    history: &GeneratedHistory,
    palette_size: u32,
    allow_dangling: bool,
    seed: u64,
    label: &str,
    rows: &[VisualCommitRow],
) {
    let commits = &history.commits;
    let n = commits.len();
    let ctx = || format!("{label} seed={seed} nodes={n}");

    assert_eq!(rows.len(), n, "row count changed; {}", ctx());
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row.id, commits[i].id, "row {i} lost its id; {}", ctx());
        assert_eq!(
            row.parent_ids,
            commits[i].parent_ids,
            "row {i} lost its parents; {}",
            ctx()
        );
        assert_eq!(row.is_merge, commits[i].parent_ids.len() > 1);
        assert_eq!(row.is_root, commits[i].parent_ids.is_empty());
        assert_eq!(
            row.connections.len(),
            commits[i].parent_ids.len(),
            "row {i} emitted {} connections for {} parents; {}",
            row.connections.len(),
            commits[i].parent_ids.len(),
            ctx()
        );

        assert!(
            (row.lane as usize) < history.max_columns,
            "row {i} lane {} outside bound {}; {}",
            row.lane,
            history.max_columns,
            ctx()
        );
        assert!(
            row.active_lanes.contains(&row.lane),
            "row {i} lane {} missing from its own active_lanes {:?}; {}",
            row.lane,
            row.active_lanes,
            ctx()
        );
        assert!(
            row.active_lanes.windows(2).all(|w| w[0] < w[1]),
            "row {i} active_lanes not strictly ascending: {:?}; {}",
            row.active_lanes,
            ctx()
        );
        assert_eq!(
            row.active_lanes.len(),
            row.active_lane_colors.len(),
            "row {i} active lane/color vectors diverge; {}",
            ctx()
        );
        assert!(
            (row.color_index as usize) < palette_size as usize,
            "row {i} color {} outside palette {palette_size}; {}",
            row.color_index,
            ctx()
        );
    }

    for (i, row) in rows.iter().enumerate() {
        for (k, conn) in row.connections.iter().enumerate() {
            let pk = &commits[i].parent_ids[k];
            let fail = |msg: String| {
                panic!(
                    "{msg}\n  at row {i} connection {k} (parent {pk:?})\n  connection: {conn:#?}\n  row: lane {}, active_lanes {:?}\n  {}",
                    row.lane, row.active_lanes,
                    ctx()
                )
            };
            let later_ids: Vec<&str> = rows[i + 1..].iter().map(|r| r.id.as_str()).collect();

            assert_eq!(
                conn.from_lane,
                row.lane,
                "connection {k} of row {i} disagrees with its row lane; {}",
                ctx()
            );
            assert_eq!(conn.is_merge, k > 0, "is_merge mislabelled; {}", ctx());
            assert!(
                (conn.from_lane as usize) < history.max_columns
                    && (conn.to_lane as usize) < history.max_columns,
                "connection lanes {},{} outside bound {}; {}",
                conn.from_lane,
                conn.to_lane,
                history.max_columns,
                ctx()
            );
            assert!(
                (conn.color_index as usize) < palette_size as usize,
                "connection color {} outside palette {palette_size}; {}",
                conn.color_index,
                ctx()
            );

            if conn.is_dangling {
                if !allow_dangling {
                    fail("valid topological input produced a dangling edge".into());
                }
                if later_ids.contains(&pk.as_str()) {
                    fail("dangling edge names a parent that exists below this row".into());
                }
                if conn.to_row_offset != 1 {
                    fail(format!(
                        "dangling stub offset is {} (documented stub offset is 1)",
                        conn.to_row_offset
                    ));
                }
                continue;
            }

            let offset = conn.to_row_offset as usize;
            if offset == 0 || i + offset >= n {
                fail(format!(
                    "offset sanity violated: offset {offset}, row {i}, {n} nodes"
                ));
            }
            if rows[i + offset].id != *pk {
                fail(format!(
                    "endpoint truth violated: rows[{}].id = {:?}, expected parent {:?}",
                    i + offset,
                    rows[i + offset].id,
                    pk
                ));
            }

            if k == 0 {
                assert_eq!(
                    conn.color_index,
                    row.color_index,
                    "first-parent edge must carry its row's color; {}",
                    ctx()
                );
                continue;
            }

            // Second+-parent lane relations; see module docs for the proof.
            let dup_of_first = *pk == commits[i].parent_ids[0];
            let mentioned_earlier = commits[..i]
                .iter()
                .any(|c| c.parent_ids.iter().any(|p| p == pk));
            if dup_of_first {
                // Unconstrained by design: iteration 0 just placed pk on (or
                // found pk already occupying) some column, and iteration k
                // finds that same column again.
            } else if mentioned_earlier {
                if conn.to_lane == conn.from_lane {
                    fail(
                        "pre-existing merged-in parent landed on the merge commit's own lane"
                            .into(),
                    );
                }
            } else if conn.to_lane <= conn.from_lane {
                fail(format!(
                    "freshly allocated merged-in branch landed at/left of the merge lane \
                     (strictly-after placement violated): to={}",
                    conn.to_lane
                ));
            }
        }
    }
}

/// Solves with a fresh solver, checks every invariant, and returns the rows
/// for further comparison.
fn assert_all_invariants(
    history: &GeneratedHistory,
    palette_size: u32,
    allow_dangling: bool,
    seed: u64,
    label: &str,
) -> Vec<VisualCommitRow> {
    let rows = LaneSolver::new(palette_size).solve(&history.commits);
    check_all_invariants(history, palette_size, allow_dangling, seed, label, &rows);
    rows
}

#[test]
fn randomized_dag_invariants_hold_across_seeds() {
    const SEEDS: u64 = 520;
    for seed in 0..SEEDS {
        let history = generate_history(seed);
        let palette = 1 + (seed % 24) as u32;

        let rows = assert_all_invariants(&history, palette, false, seed, "randomized_dag");

        // DETERMINISM: same input, fresh solver, identical output.
        let again = LaneSolver::new(palette).solve(&history.commits);
        assert_eq!(
            rows, again,
            "solver is nondeterministic across runs; seed={seed}"
        );
    }
}

#[test]
fn dangling_parent_edges_stay_stubs_across_seeds() {
    const SEEDS: u64 = 128;
    for seed in 10_000..10_000 + SEEDS {
        let history = generate_history_with_ghost_parents(seed);
        assert_all_invariants(&history, 12, true, seed, "dangling_fuzz");
    }
}

/// Solving B after A on a reused solver must match solving B alone: solve()
/// resets `active_columns`, `column_colors`, and the color cursor before each
/// run (lane_solver.rs solve()), and production creates one solver per call
/// anyway (commands/mod.rs). This guards against state leaks if either
/// changes.
#[test]
fn solver_state_does_not_leak_between_solves() {
    const SEEDS: u64 = 48;
    for seed in 20_000..20_000 + SEEDS {
        let a = generate_history(seed);
        let b = generate_history(seed + 500);

        let mut reused = LaneSolver::new(12);
        let _ = reused.solve(&a.commits);
        let b_on_reused = reused.solve(&b.commits);
        check_all_invariants(
            &b,
            12,
            false,
            seed,
            "state_reset_reused_after_a",
            &b_on_reused,
        );

        let b_fresh = assert_all_invariants(&b, 12, false, seed, "state_reset_fresh");
        assert_eq!(
            b_fresh, b_on_reused,
            "solving B after A on a reused solver differed from solving B alone; seed={seed}"
        );
    }
}

/// Pins the duplicate-first-parent semantics documented in the module header:
/// the duplicated edge is labelled a merge, resolves endpoint-correctly, and
/// both occurrences converge on the same column — the merge commit's own
/// lane here, which is why the general invariant forbids `to_lane ==
/// from_lane` only for distinct non-pre-existing parents.
#[test]
fn duplicate_first_parent_merge_edge_converges_on_one_column() {
    let m = make_node("m".into(), vec!["p".into(), "p".into()], 100);
    let p = make_node("p".into(), vec![], 99);
    let commits = vec![m, p];

    let history = GeneratedHistory {
        max_columns: commits.len(),
        commits: commits.clone(),
    };
    let rows = assert_all_invariants(&history, 12, false, 42, "dup_first_parent");

    let merge_row = &rows[0];
    let (first, dup) = (&merge_row.connections[0], &merge_row.connections[1]);
    assert!(dup.is_merge, "second occurrence must be flagged is_merge");
    assert_eq!(
        first.to_row_offset, dup.to_row_offset,
        "both hit p at row 1"
    );
    assert_eq!(first.to_lane, dup.to_lane, "both resolve to p's column");
    assert_eq!(
        dup.to_lane, merge_row.lane,
        "duplicate first parent reuses the merge commit's own lane"
    );
}
