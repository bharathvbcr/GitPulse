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
//!   from_lane` (owned by an overlapping segment) — unless BOTH the merge
//!   row and `pk` lie on the pinned mainline, whose whole chain shares
//!   column 0 by construction (a mainline merge of its own ancestor);
//! - anything else → unconstrained relative to `from_lane`;
//! - always: `to_lane` is the column the parent row actually occupies.
//!
//! # Mainline (verified against an independent first-parent walk)
//!
//! One first-parent chain — the hinted branch, HEAD as fallback, row 0 by
//! default — is pinned to column 0 in colour 0 for the whole window. Every
//! row on that chain reports `is_mainline`, sits on column 0, and paints in
//! colour 0; no other row ever sits on column 0, column 0 never carries any
//! other colour, and nothing is drawn on it above the mainline's tip.
//!
//! # Width (verified independently from the output)
//!
//! Columns are reserved for a segment's whole visual lifetime — including
//! rows where only its closing connector is still descending, and the stub
//! row under a window-cut tail — and the mainline's column for the whole
//! window. Greedy interval allocation in birth order is optimal for
//! interval graphs, so the graph's width must EQUAL the peak number of
//! concurrently occupied columns, where "occupied" is reconstructed from
//! the output alone: a node, a pass-through lane, an in-flight connector,
//! a dangling stub, or the mainline reservation on column 0. Wider means
//! columns leak; narrower is impossible (pigeonhole).

use gitpulse_lib::graph::{
    mainline_chain_ids, simplify_history, LaneSolver, MainlineHint, RawCommitNode, VisualCommitRow,
    MAINLINE_COLOR, MAINLINE_COLUMN, MAX_REWRITTEN_PARENTS,
};
use std::collections::{HashMap, HashSet};

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
            if conn.to_lane != rows[i + offset].lane {
                fail(format!(
                    "endpoint column violated: edge lands on column {} but the parent sits on {}",
                    conn.to_lane,
                    rows[i + offset].lane
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
            } else if mentioned_earlier
                && conn.to_lane == conn.from_lane
                && !(row.is_mainline && rows[i + offset].is_mainline)
            {
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
    // The mainline's column is reserved for the whole window — above its
    // tip, below its root, and through every hole between its commits.
    for slot in occupied.iter_mut() {
        slot.insert(MAINLINE_COLUMN);
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

/// First-occurrence row index per id, as the solver resolves endpoints.
fn row_index(commits: &[RawCommitNode]) -> HashMap<&str, usize> {
    let mut index = HashMap::new();
    for (i, c) in commits.iter().enumerate() {
        if !c.id.is_empty() {
            index.entry(c.id.as_str()).or_insert(i);
        }
    }
    index
}

/// Independent first-parent walk from `tip`: strictly descending rows,
/// stopping at a root, an empty or unknown parent, or a parent at or above
/// the current row.
fn first_parent_rows(
    commits: &[RawCommitNode],
    index: &HashMap<&str, usize>,
    tip: usize,
) -> Vec<usize> {
    let mut chain = vec![tip];
    let mut current = tip;
    while let Some(parent) = commits[current].parent_ids.first() {
        match index.get(parent.as_str()) {
            Some(&row) if !parent.is_empty() && row > current => {
                chain.push(row);
                current = row;
            }
            _ => break,
        }
    }
    chain
}

/// The tip [`MainlineHint`] documents: the first loaded branch tip (else
/// the fallback, else row 0), extended upward through any branch tip whose
/// first-parent chain passes through it.
fn expected_mainline_tip(commits: &[RawCommitNode], hint: &MainlineHint) -> usize {
    let index = row_index(commits);
    let row_for = |id: &String| -> Option<usize> {
        if id.is_empty() {
            None
        } else {
            index.get(id.as_str()).copied()
        }
    };
    let anchor = hint
        .branch_tips
        .iter()
        .find_map(row_for)
        .or_else(|| hint.fallback_tip.as_ref().and_then(row_for));
    let Some(mut tip) = anchor else {
        return 0;
    };
    loop {
        let mut moved = false;
        for id in &hint.branch_tips {
            if let Some(row) = row_for(id) {
                if row < tip && first_parent_rows(commits, &index, row).contains(&tip) {
                    tip = row;
                    moved = true;
                }
            }
        }
        if !moved {
            return tip;
        }
    }
}

/// The mainline contract, checked against an independent walk from
/// `expected_tip` (see the module docs).
fn assert_mainline_invariants(
    commits: &[RawCommitNode],
    rows: &[VisualCommitRow],
    expected_tip: usize,
    palette_size: u32,
    seed: u64,
    label: &str,
) {
    if rows.is_empty() {
        return;
    }
    let index = row_index(commits);
    let chain: HashSet<usize> = first_parent_rows(commits, &index, expected_tip)
        .into_iter()
        .collect();
    let mainline_color = MAINLINE_COLOR % palette_size;
    // Every reserved column interval holds a distinct column, so at most
    // `width` reservations overlap anywhere — and the mainline's colour is
    // reserved for the whole window. With more palette slots than columns a
    // free slot therefore always exists when a segment is born, and no
    // branch may ever take main's colour; with fewer, collisions are the
    // documented unavoidable case and the check does not apply.
    let width = rows
        .iter()
        .flat_map(|r| {
            std::iter::once(r.lane)
                .chain(r.active_lanes.iter().copied())
                .chain(r.connections.iter().map(|c| c.to_lane))
        })
        .max()
        .map_or(0, |max| max as usize + 1);
    let colour_slots_suffice = palette_size as usize > width;
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(
            row.is_mainline,
            chain.contains(&i),
            "{label} seed={seed}: row {i} is_mainline={} but the first-parent walk from {expected_tip} says {}",
            row.is_mainline,
            chain.contains(&i)
        );
        if row.is_mainline {
            assert_eq!(
                row.lane, MAINLINE_COLUMN,
                "{label} seed={seed}: mainline row {i} left column 0"
            );
            assert_eq!(
                row.color_index, mainline_color,
                "{label} seed={seed}: mainline row {i} is off-colour"
            );
        } else {
            assert_ne!(
                row.lane, MAINLINE_COLUMN,
                "{label} seed={seed}: row {i} sits on the mainline column"
            );
            if colour_slots_suffice {
                assert_ne!(
                    row.color_index, mainline_color,
                    "{label} seed={seed}: row {i} paints in the mainline colour with slots free"
                );
            }
        }
        for (l, &lane) in row.active_lanes.iter().enumerate() {
            if lane != MAINLINE_COLUMN {
                continue;
            }
            assert!(
                i >= expected_tip,
                "{label} seed={seed}: column 0 is drawn on row {i}, above the mainline tip {expected_tip}"
            );
            assert_eq!(
                row.active_lane_colors[l], mainline_color,
                "{label} seed={seed}: column 0 carries a foreign colour on row {i}"
            );
        }
        for conn in &row.connections {
            if conn.is_dangling || conn.to_lane != MAINLINE_COLUMN {
                continue;
            }
            let target = i + conn.to_row_offset as usize;
            assert!(
                rows[target].is_mainline,
                "{label} seed={seed}: row {i} draws an edge onto column 0 at row {target}, which is not mainline"
            );
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
    assert_mainline_invariants(&history.commits, &rows, 0, palette_size, seed, label);
    rows
}

/// Hinted solves: random branch tips (some absent), a random fallback, and
/// every invariant above plus the mainline contract against the documented
/// resolution rule.
#[test]
fn hinted_mainline_invariants_hold_across_seeds() {
    const SEEDS: u64 = 240;
    for seed in 50_000..50_000 + SEEDS {
        let history = generate_history(seed);
        let n = history.commits.len() as u64;
        let mut rng = Lcg(seed ^ 0xC0FF_EE00_1234_5678);
        let tip_count = 1 + rng.below(3) as usize;
        let mut branch_tips = Vec::with_capacity(tip_count);
        for _ in 0..tip_count {
            if rng.below(100) < 20 {
                branch_tips.push(format!("ghost-{}", rng.below(1000)));
            } else {
                branch_tips.push(format!("n{}", rng.below(n)));
            }
        }
        let fallback_tip = if rng.below(100) < 50 {
            Some(format!("n{}", rng.below(n)))
        } else {
            None
        };
        let hint = MainlineHint {
            branch_tips,
            fallback_tip,
        };
        let palette = 1 + (seed % 24) as u32;
        let rows = LaneSolver::new(palette).solve_with_mainline(&history.commits, &hint);
        check_all_invariants(&history, palette, false, seed, "hinted", &rows);
        assert_width_is_peak_occupancy(&rows, seed, "hinted");
        let expected_tip = expected_mainline_tip(&history.commits, &hint);
        assert_mainline_invariants(
            &history.commits,
            &rows,
            expected_tip,
            palette,
            seed,
            "hinted",
        );
        let again = LaneSolver::new(palette).solve_with_mainline(&history.commits, &hint);
        assert_eq!(rows, again, "hinted solve is nondeterministic; seed={seed}");
    }
}

/// Server-side commit filters thin the window AFTER the mainline hint is
/// chosen, rewrite parents (`simplify_history`) and re-anchor the hint on
/// the chain's first survivor — the `assemble_commit_graph` recipe. Under
/// random keep masks the filtered window must (1) hold exactly the kept
/// commits in order, (2) name only kept ancestors or the survivor's own
/// unresolvable ids, deduplicated and capped, (3) keep first-parent lineage
/// (the nearest kept commit on the original first-parent chain comes
/// first), and (4) solve with every in-window parent drawn live and the
/// mainline straight from the re-anchored tip.
#[test]
fn simplified_histories_stay_connected_and_keep_the_mainline_straight() {
    const SEEDS: u64 = 240;
    for seed in 70_000..70_000 + SEEDS {
        let ghosts = seed % 3 == 0;
        let history = if ghosts {
            generate_history_with_ghost_parents(seed)
        } else {
            generate_history(seed)
        };
        let commits = &history.commits;
        let n = commits.len();
        if n == 0 {
            continue;
        }
        let index = row_index(commits);
        let mut rng = Lcg(seed ^ 0x5EED_F11E_0000_0001);
        let keep_pct = 5 + rng.below(91);
        let keep: Vec<bool> = (0..n).map(|_| rng.below(100) < keep_pct).collect();
        let hint = MainlineHint {
            branch_tips: vec![format!("n{}", rng.below(n as u64))],
            fallback_tip: None,
        };
        let chain = mainline_chain_ids(commits, &hint);
        let simplified = simplify_history(commits, &keep);

        // (1) exactly the kept commits, in window order.
        let kept_rows: Vec<usize> = (0..n).filter(|i| keep[*i]).collect();
        let expected_ids: Vec<&str> = kept_rows.iter().map(|&i| commits[i].id.as_str()).collect();
        let got_ids: Vec<&str> = simplified.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            got_ids, expected_ids,
            "seed={seed}: kept set or order changed"
        );

        for (pos, commit) in simplified.iter().enumerate() {
            let orig = kept_rows[pos];
            let id = &commit.id;
            // (2) parents: kept ancestors, ancestry past the window, or
            // the survivor's own empty/malformed stubs — never a dropped
            // row, never an inherited empty or malformed id.
            assert!(
                commit.parent_ids.len() <= MAX_REWRITTEN_PARENTS,
                "seed={seed}: {id} has {} rewritten parents",
                commit.parent_ids.len()
            );
            let distinct: HashSet<&str> = commit.parent_ids.iter().map(String::as_str).collect();
            assert_eq!(
                distinct.len(),
                commit.parent_ids.len(),
                "seed={seed}: {id} lists a parent twice"
            );
            for parent in &commit.parent_ids {
                match index.get(parent.as_str()) {
                    Some(&row) if !parent.is_empty() && row > orig => assert!(
                        keep[row],
                        "seed={seed}: {id} names dropped ancestor {parent}"
                    ),
                    None if !parent.is_empty() => {} // past the window: travels
                    _ => assert!(
                        commits[orig].parent_ids.iter().any(|own| own == parent),
                        "seed={seed}: {id} inherited empty/malformed id {parent:?}"
                    ),
                }
            }
            // (3) first-parent lineage survives.
            let walk = first_parent_rows(commits, &index, orig);
            if let Some(&nearest) = walk[1..].iter().find(|row| keep[**row]) {
                assert_eq!(
                    commit.parent_ids.first().map(String::as_str),
                    Some(commits[nearest].id.as_str()),
                    "seed={seed}: {id}'s first parent is not the nearest kept first-parent ancestor"
                );
            } else if let Some(own_first) = commits[orig].parent_ids.first() {
                let resolvable = !own_first.is_empty()
                    && matches!(index.get(own_first.as_str()), Some(&row) if row > orig);
                if !resolvable {
                    assert_eq!(
                        commit.parent_ids.first(),
                        Some(own_first),
                        "seed={seed}: {id}'s own stub {own_first:?} must stay first"
                    );
                }
            }
        }

        // (4) the filtered window solves clean.
        if simplified.is_empty() {
            continue;
        }
        let survivor = chain
            .iter()
            .find(|id| got_ids.contains(&id.as_str()))
            .cloned();
        let re_hint = MainlineHint {
            branch_tips: survivor.into_iter().collect(),
            fallback_tip: None,
        };
        let palette = 1 + (seed % 24) as u32;
        let rows = LaneSolver::new(palette).solve_with_mainline(&simplified, &re_hint);
        let filtered = GeneratedHistory {
            commits: simplified.clone(),
            max_columns: history.max_columns,
        };
        check_all_invariants(&filtered, palette, ghosts, seed, "simplified", &rows);
        assert_width_is_peak_occupancy(&rows, seed, "simplified");
        let expected_tip = expected_mainline_tip(&simplified, &re_hint);
        assert_mainline_invariants(
            &simplified,
            &rows,
            expected_tip,
            palette,
            seed,
            "simplified",
        );
        let sindex = row_index(&simplified);
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(
                row.connections.len(),
                row.parent_ids.len(),
                "seed={seed}: connection per parent"
            );
            for (k, conn) in row.connections.iter().enumerate() {
                let parent = row.parent_ids[k].as_str();
                match sindex.get(parent) {
                    Some(&target) if !parent.is_empty() && target > i => {
                        assert!(
                            !conn.is_dangling,
                            "seed={seed}: {} -> {parent} is a stub although the parent is loaded",
                            row.id
                        );
                        assert_eq!(conn.to_row_offset as usize, target - i, "seed={seed}");
                    }
                    _ => assert!(
                        conn.is_dangling,
                        "seed={seed}: unresolvable parent drawn live"
                    ),
                }
            }
        }
    }
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
                // The anchor (row 0) is in every prefix, and truncation can
                // only shorten its chain: mainline membership and the
                // pinned column survive every window size.
                assert_eq!(
                    row.is_mainline, full[i].is_mainline,
                    "seed={seed} w={w} row={i}: truncation changed mainline membership"
                );
                if row.is_mainline {
                    assert_eq!(
                        row.lane, full[i].lane,
                        "seed={seed} w={w} row={i}: mainline moved"
                    );
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
