//! Randomized property-based stress tests for the graph lane solver.
//!
//! Deterministic by construction: every case is derived from a seed fed into
//! an inline LCG (`state = state * 6364136223846793005 + 1442695040888963407`,
//! wrapping u64 arithmetic), so any failure prints the seed that reproduces
//! it. No external property-testing crates are used.
//!
//! # Merge parent placement (verified against lane_solver.rs)
//!
//! The solver decomposes history into branch segments (maximal first-parent
//! chains) and gives each segment one column for its entire lifetime by
//! interval allocation. For a second-or-later parent edge (`is_merge ==
//! true`) of row `i` with parent id `pk`:
//!
//! 1. **Owned parent** — some earlier row already reserved `pk`, so the edge
//!    lands on that segment's column. That segment overlaps the merge row
//!    (born above it, ending at or below `pk`'s row) while the merge's own
//!    segment also covers row `i`; two overlapping segments can never share
//!    a column, so `to_lane != from_lane` — UNLESS `pk` duplicates
//!    `parent_ids[0]`, whose continuation legitimately holds the merge's own
//!    column.
//! 2. **Fresh parent** — no reservation exists. Normally a new segment is
//!    born at the merge row and interval allocation may place it left of,
//!    or right of, the merge (never on it — the two segments overlap).
//!    The exception: when the merge's FIRST parent is dead (empty id, out
//!    of window, or malformed), the first fresh live merged-in parent
//!    continues the merge's own segment straight down, so `to_lane ==
//!    from_lane` is then the correct, compact outcome.
//!
//! Hence the encoded invariant keeps only what holds in every case:
//!
//! - `pk != parent_ids[0]`, some earlier row mentions `pk`, → `to_lane !=
//!   from_lane` (owned by an overlapping segment);
//! - anything else → unconstrained relative to `from_lane`.
//!
//! # Width (verified independently from the output)
//!
//! Columns are reserved for a segment's whole visual lifetime — including
//! rows where only its closing connector is still descending, and the stub
//! row under a window-cut tail. Greedy interval allocation in birth order
//! is optimal for interval graphs, so the graph's width must EQUAL the peak
//! number of concurrently occupied columns, where "occupied" is
//! reconstructed from the output alone: a node, a pass-through lane, an
//! in-flight connector, or a dangling stub. Wider means columns leak;
//! narrower is impossible (pigeonhole).

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
    generate_history_sized(seed, 120)
}

fn generate_history_sized(seed: u64, max_nodes: u64) -> GeneratedHistory {
    let mut rng = Lcg(seed ^ 0x9E37_79B9_7F4A_7C15);
    let node_count = (1 + rng.below(max_nodes)) as usize;
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
        let mut parent_ids: Vec<String> = if ghosted {
            (0..parent_count)
                .map(|_| {
                    ghost_counter += 1;
                    format!("ghost{}", ghost_counter - 1)
                })
                .collect()
        } else {
            picks.into_iter().map(|p| format!("n{p}")).collect()
        };
        // Corrupt/truncated parent ids must dangle without allocating a column.
        if !parent_ids.is_empty() && rng.below(100) < 8 {
            let slot = rng.below(parent_ids.len() as u64) as usize;
            parent_ids[slot] = String::new();
        }

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
                if pk.is_empty() && conn.to_lane != conn.from_lane {
                    fail(
                        "empty parent id allocated a column instead of stubbing on the child lane"
                            .into(),
                    );
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
            } else if mentioned_earlier && conn.to_lane == conn.from_lane {
                fail("pre-existing merged-in parent landed on the merge commit's own lane".into());
            }
            // Fresh second parents are first-fit: they may reuse a hole
            // left of the merge. Compactness is checked separately.
        }
    }
}

/// Width == peak concurrent occupancy, reconstructed from the output alone.
///
/// A column is occupied at a row when a node sits on it, a pass-through
/// lane crosses it, a connector is in flight through it (a merge peel
/// descending on `to_lane`, or a closing/continuing edge descending on
/// `from_lane`), or a dangling stub protrudes into it from the row above.
/// The union of these per column is exactly the interval the solver
/// reserves, so greedy interval allocation makes the graph EXACTLY
/// `max_occupancy` columns wide: wider means a reservation leaked; narrower
/// is impossible by pigeonhole.
fn assert_width_is_peak_occupancy(rows: &[VisualCommitRow], seed: u64, label: &str) {
    let n = rows.len();
    if n == 0 {
        return;
    }
    let high_water = rows
        .iter()
        .flat_map(|r| {
            std::iter::once(r.lane)
                .chain(r.active_lanes.iter().copied())
                .chain(r.connections.iter().map(|c| c.to_lane))
        })
        .max()
        .unwrap_or(0);

    let mut occupied: Vec<std::collections::HashSet<u32>> = rows
        .iter()
        .map(|r| {
            let mut set: std::collections::HashSet<u32> = r.active_lanes.iter().copied().collect();
            set.insert(r.lane);
            set
        })
        .collect();
    for (i, row) in rows.iter().enumerate() {
        for conn in &row.connections {
            if conn.is_dangling {
                // The fading stub protrudes into the row below the commit.
                let below = (i + 1).min(n - 1);
                occupied[below].insert(conn.from_lane);
                continue;
            }
            let target = i + conn.to_row_offset as usize;
            if target >= n {
                continue;
            }
            let lane = if conn.is_merge {
                conn.to_lane
            } else {
                conn.from_lane
            };
            for slot in occupied.iter_mut().take(target + 1).skip(i) {
                slot.insert(lane);
            }
        }
    }
    let peak = occupied.iter().map(|s| s.len()).max().unwrap_or(0) as u32;
    assert_eq!(
        high_water + 1,
        peak,
        "{label} seed={seed}: width {} != peak concurrent occupancy {}",
        high_water + 1,
        peak
    );
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
        assert_width_is_peak_occupancy(&rows, seed, "randomized_dag");

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
    const SEEDS: u64 = 256;
    for seed in 10_000..10_000 + SEEDS {
        let history = generate_history_with_ghost_parents(seed);
        let rows = assert_all_invariants(&history, 12, true, seed, "dangling_fuzz");
        assert_width_is_peak_occupancy(&rows, seed, "dangling_fuzz");
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
/// from_lane` only for distinct *pre-existing* parents.
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

/// Truncating the window is how every real page load works (`max_commits`,
/// filters). It must only turn edges into dangling stubs — never move an
/// edge to a different row, and never dangle an edge whose parent is still
/// inside the window. Prefix rows keep their indices, so the two solves are
/// directly comparable connection by connection.
#[test]
fn windowed_prefix_solves_agree_on_edge_structure() {
    const SEEDS: u64 = 96;
    for seed in 30_000..30_000 + SEEDS {
        let history = generate_history(seed);
        let n = history.commits.len();
        let full = LaneSolver::new(12).solve(&history.commits);
        for w in [1, n / 3, n / 2, n.saturating_sub(1), n] {
            if w == 0 || w > n {
                continue;
            }
            let prefix: Vec<RawCommitNode> = history.commits[..w].to_vec();
            let rows = LaneSolver::new(12).solve(&prefix);
            assert_eq!(rows.len(), w, "seed={seed} w={w}: prefix row count");
            for (i, row) in rows.iter().enumerate() {
                for (k, conn) in row.connections.iter().enumerate() {
                    let f = &full[i].connections[k];
                    let live_in_window = !f.is_dangling && (i + f.to_row_offset as usize) < w;
                    if live_in_window {
                        assert!(
                            !conn.is_dangling,
                            "seed={seed} w={w} row={i} k={k}: in-window edge became a stub"
                        );
                        assert_eq!(
                            conn.to_row_offset, f.to_row_offset,
                            "seed={seed} w={w} row={i} k={k}: truncation moved an edge"
                        );
                    } else {
                        assert!(
                            conn.is_dangling,
                            "seed={seed} w={w} row={i} k={k}: edge outlived its window"
                        );
                    }
                }
            }
        }
    }
}

/// Opt-in deep fuzz: an order of magnitude more seeds plus larger DAGs
/// than the always-on suites. Run explicitly with:
/// `cargo test --test lane_solver_fuzz --release -- --ignored`
/// Set `DEEP_FUZZ_SCALE=<k>` to multiply every seed count by `k`.
#[test]
#[ignore = "deep fuzz; heavy CPU — run explicitly"]
fn deep_fuzz_many_seeds_and_large_dags() {
    let scale: u64 = std::env::var("DEEP_FUZZ_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&v| v >= 1)
        .unwrap_or(1);
    for seed in 0..4_000 * scale {
        let history = generate_history(seed);
        let palette = 1 + (seed % 24) as u32;
        let rows = assert_all_invariants(&history, palette, false, seed, "deep_small");
        assert_width_is_peak_occupancy(&rows, seed, "deep_small");
    }
    for seed in 40_000_000..40_000_000 + 800 * scale {
        let history = generate_history_sized(seed, 600);
        let rows = assert_all_invariants(&history, 12, false, seed, "deep_large");
        assert_width_is_peak_occupancy(&rows, seed, "deep_large");
    }
    for seed in 60_000_000..60_000_000 + 2_000 * scale {
        let history = generate_history_with_ghost_parents(seed);
        let rows = assert_all_invariants(&history, 12, true, seed, "deep_ghost");
        assert_width_is_peak_occupancy(&rows, seed, "deep_ghost");
    }
}

#[test]
fn reversed_history_does_not_panic_and_marks_back_edges_dangling() {
    const SEEDS: u64 = 40;
    for seed in 0..SEEDS {
        let history = generate_history(seed);
        let mut reversed = history.commits.clone();
        reversed.reverse();
        let rows = LaneSolver::new(12).solve(&reversed);
        assert_eq!(
            rows.len(),
            reversed.len(),
            "reversed seed={seed} changed row count"
        );
        let index: std::collections::HashMap<&str, usize> = reversed
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id.as_str(), i))
            .collect();
        for (i, commit) in reversed.iter().enumerate() {
            assert_eq!(rows[i].connections.len(), commit.parent_ids.len());
            for (k, parent) in commit.parent_ids.iter().enumerate() {
                if parent.is_empty() {
                    assert!(rows[i].connections[k].is_dangling);
                    continue;
                }
                match index.get(parent.as_str()) {
                    Some(&pidx) if pidx > i => {
                        assert!(
                            !rows[i].connections[k].is_dangling,
                            "reversed seed={seed} row {i} dangled a later parent"
                        );
                    }
                    _ => {
                        assert!(
                            rows[i].connections[k].is_dangling,
                            "reversed seed={seed} row {i} drew a back-edge as live"
                        );
                    }
                }
            }
        }
    }
}
