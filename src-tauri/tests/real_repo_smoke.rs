//! Opt-in smoke test: solve a REAL repository's history and check every
//! stable-column invariant at scale.
//!
//! Synthetic fuzz covers the input space; this covers the input
//! distribution — actual merge-heavy histories with octopus merges,
//! criss-crossing worktree branches, and window cuts. Run explicitly with:
//!
//! ```sh
//! GITPULSE_SMOKE_REPO=/path/to/repo cargo test --test real_repo_smoke -- --ignored
//! ```
//!
//! It is `#[ignore]` because it depends on a repository outside the build
//! sandbox; it FAILS (never silently passes) when the env var is set but
//! unusable, so a check that could not run can never report as one that ran.

use gitpulse_lib::analyzer::CommitFilter;
use gitpulse_lib::commands::{assemble_commit_graph, resolve_mainline_hint};
use gitpulse_lib::engine::GitReader;
use gitpulse_lib::graph::{
    hidden_ref_warning, list_ref_decorations, probe_hidden_history, LaneSolver, MainlineHint,
    RawCommitNode, RefKind, RefScope, VisualCommitRow, MAINLINE_COLOR, MAINLINE_COLUMN,
};
use std::collections::{HashMap, HashSet};

/// The scope the smoke run walks under, from `GITPULSE_SMOKE_SCOPE`
/// (`named`, the default, or `all`).
fn smoke_scope() -> RefScope {
    match std::env::var("GITPULSE_SMOKE_SCOPE")
        .unwrap_or_default()
        .as_str()
    {
        "all" => RefScope::All,
        _ => RefScope::Named,
    }
}

/// History through the PRODUCTION reader, not a private `git log --all`.
///
/// It used to shell out to its own `git log --all`, which made this smoke run
/// a third independent copy of "which refs the graph is about" — the exact
/// drift that let unnameable lanes reach the screen. A fixture dumped from a
/// walk the app does not perform proves nothing about the app.
fn load_history(repo: &str, max: usize) -> Vec<RawCommitNode> {
    GitReader::read_commit_history_paged(repo, 0, max, None, None, smoke_scope())
        .expect("history through the production reader")
}

fn check_stable_column_invariants(commits: &[RawCommitNode], rows: &[VisualCommitRow]) {
    let n = rows.len();
    assert_eq!(n, commits.len());
    let row_of: HashMap<&str, usize> = commits
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id.as_str(), i))
        .collect();

    // Per-row claims: column -> set of colors. Any column claimed by two
    // colors on one row is the branch-overlap artifact.
    let mut claims: Vec<HashMap<u32, HashSet<u32>>> = vec![HashMap::new(); n];
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row.connections.len(), commits[i].parent_ids.len());
        assert!(row.active_lanes.contains(&row.lane));
        claims[i]
            .entry(row.lane)
            .or_default()
            .insert(row.color_index);
        for (l, &lane) in row.active_lanes.iter().enumerate() {
            claims[i]
                .entry(lane)
                .or_default()
                .insert(row.active_lane_colors[l]);
        }
    }
    for (i, row) in rows.iter().enumerate() {
        for (k, conn) in row.connections.iter().enumerate() {
            assert_eq!(conn.from_lane, row.lane);
            if conn.is_dangling {
                assert_eq!(conn.to_row_offset, 1);
                // The fading stub protrudes into the row below; the solver
                // keeps the column reserved there so a new tip is never
                // drawn under it. Model that claim or width looks padded.
                let below = (i + 1).min(n - 1);
                claims[below]
                    .entry(conn.from_lane)
                    .or_default()
                    .insert(conn.color_index);
                continue;
            }
            let t = i + conn.to_row_offset as usize;
            assert!(t < n, "edge escapes the window without is_dangling");
            assert_eq!(
                rows[t].id, commits[i].parent_ids[k],
                "edge endpoint truth violated at row {i}"
            );
            assert_eq!(
                conn.to_lane, rows[t].lane,
                "edge lands beside its parent's column at row {i}"
            );
            let lane = if conn.is_merge {
                conn.to_lane
            } else {
                conn.from_lane
            };
            for claim_row in claims.iter_mut().take(t + 1).skip(i) {
                claim_row.entry(lane).or_default().insert(conn.color_index);
            }
        }
    }
    for (i, row_claims) in claims.iter().enumerate() {
        for (column, colors) in row_claims {
            assert!(
                colors.len() <= 1,
                "row {i} ({}): column {column} claimed by {} branches",
                rows[i].id,
                colors.len()
            );
        }
    }

    // First-parent chains keep lane+color while uncontested.
    let mut claimed_parent: HashSet<&str> = HashSet::new();
    for (i, commit) in commits.iter().enumerate() {
        let Some(p0) = commit.parent_ids.first() else {
            continue;
        };
        if p0.is_empty() {
            continue;
        }
        let Some(&p_row) = row_of.get(p0.as_str()) else {
            continue;
        };
        if p_row <= i {
            continue;
        }
        let contested = !claimed_parent.insert(p0.as_str());
        // A feature whose first parent is a mainline commit closes INTO the
        // pinned column rather than inheriting it: that is the mainline
        // contract, not a lane move.
        let closes_into_mainline = rows[p_row].is_mainline && !rows[i].is_mainline;
        if !contested && !closes_into_mainline {
            assert_eq!(
                rows[p_row].lane, rows[i].lane,
                "uncontested first parent {p0} moved lanes"
            );
            assert_eq!(
                rows[p_row].color_index, rows[i].color_index,
                "uncontested first parent {p0} changed color"
            );
        }
        for pk in commit.parent_ids.iter().skip(1) {
            claimed_parent.insert(pk.as_str());
        }
    }

    // Width == peak occupancy (interval allocation is optimal). The
    // mainline's column is reserved for the whole window, so it counts on
    // every row.
    for row_claims in claims.iter_mut() {
        row_claims
            .entry(MAINLINE_COLUMN)
            .or_default()
            .insert(MAINLINE_COLOR);
    }
    let width = rows
        .iter()
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
        + 1;
    let peak = claims.iter().map(|c| c.len()).max().unwrap_or(0) as u32;
    assert_eq!(width, peak, "width {width} != peak occupancy {peak}");
}

/// The mainline contract on real data, against an independent first-parent
/// walk from the tip [`MainlineHint`] documents (first loaded branch tip,
/// else the fallback, else row 0, extended upward through a branch tip
/// whose chain passes through the anchor).
fn check_mainline_invariants(
    commits: &[RawCommitNode],
    rows: &[VisualCommitRow],
    hint: &MainlineHint,
) {
    if rows.is_empty() {
        return;
    }
    let mut index: HashMap<&str, usize> = HashMap::new();
    for (i, c) in commits.iter().enumerate() {
        index.entry(c.id.as_str()).or_insert(i);
    }
    let chain_from = |tip: usize| -> Vec<usize> {
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
    };
    let row_for = |id: &String| index.get(id.as_str()).copied();
    let mut tip = hint
        .branch_tips
        .iter()
        .find_map(row_for)
        .or_else(|| hint.fallback_tip.as_ref().and_then(row_for))
        .unwrap_or_default();
    loop {
        let mut moved = false;
        for id in &hint.branch_tips {
            if let Some(row) = row_for(id) {
                if row < tip && chain_from(row).contains(&tip) {
                    tip = row;
                    moved = true;
                }
            }
        }
        if !moved {
            break;
        }
    }
    let chain: HashSet<usize> = chain_from(tip).into_iter().collect();
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(
            row.is_mainline,
            chain.contains(&i),
            "row {i} ({}) is_mainline disagrees with the first-parent walk from row {tip}",
            row.id
        );
        if row.is_mainline {
            assert_eq!(row.lane, MAINLINE_COLUMN, "mainline row {i} left column 0");
            assert_eq!(
                row.color_index, MAINLINE_COLOR,
                "mainline row {i} is off-colour"
            );
            for conn in &row.connections {
                if !conn.is_merge && !conn.is_dangling {
                    assert_eq!(conn.to_lane, MAINLINE_COLUMN, "mainline row {i} bends");
                }
            }
        } else {
            assert_ne!(
                row.lane, MAINLINE_COLUMN,
                "row {i} ({}) sits on main's column",
                row.id
            );
        }
        for (l, &lane) in row.active_lanes.iter().enumerate() {
            if lane == MAINLINE_COLUMN {
                assert!(i >= tip, "column 0 drawn above the mainline tip at row {i}");
                assert_eq!(row.active_lane_colors[l], MAINLINE_COLOR);
            }
        }
    }
}

/// Resolves the hint exactly as `cmd_get_commit_graph` does, from the
/// repository's own refs, default branch and HEAD.
fn mainline_for(
    repo: &str,
) -> (
    gitpulse_lib::commands::ResolvedMainline,
    Vec<gitpulse_lib::graph::RefDecoration>,
    Option<String>,
    Option<String>,
) {
    // The SAME scope the history was walked under. Labelling under Named
    // while walking under All reproduced the original defect inside the
    // harness meant to detect it: the dump showed 35 lanes and 7 labels, and
    // the fixture described a graph the app would never produce.
    let refs = list_ref_decorations(repo, smoke_scope())
        .expect("ref decorations")
        .decorations;
    let head = GitReader::head_id(repo).ok();
    let default_branch = GitReader::default_branch_name(repo).expect("default branch probe");
    let resolved = resolve_mainline_hint(&refs, default_branch.as_deref(), head.as_deref());
    (resolved, refs, head, default_branch)
}

/// The default branch of a real repository is one straight rail on column
/// 0, through every merge the walk interleaved with it, at every window
/// size — and the solved payload can be dumped as a fixture for the
/// renderer's oracle tests:
///
/// ```sh
/// A stub may only ever point outside the payload: every parent that IS a
/// loaded row must be drawn as a live edge.
fn check_no_stub_points_at_a_loaded_row(rows: &[VisualCommitRow]) {
    let loaded: std::collections::HashSet<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    for row in rows {
        for (k, conn) in row.connections.iter().enumerate() {
            let parent = row.parent_ids.get(k).map(String::as_str).unwrap_or("");
            assert!(
                !conn.is_dangling || !loaded.contains(parent),
                "{} -> {parent}: stub points at a loaded row",
                row.id
            );
        }
    }
}

/// Server-side commit filters on a real repository: the filtered graph is
/// assembled exactly as `cmd_get_commit_graph` assembles it, and must stay
/// connected with the same branch pinned straight — anchored on the first
/// survivor of the unfiltered mainline chain.
///
/// GITPULSE_SMOKE_REPO=/path/to/repo cargo test --test real_repo_smoke -- --ignored
#[test]
#[ignore = "needs GITPULSE_SMOKE_REPO pointing at a real repository"]
fn real_repository_filtered_graphs_stay_connected() {
    use gitpulse_lib::graph::{mainline_chain_ids, MAINLINE_COLOR, MAINLINE_COLUMN};
    let repo =
        std::env::var("GITPULSE_SMOKE_REPO").expect("set GITPULSE_SMOKE_REPO to a repository path");
    let commits = load_history(&repo, 2_000);
    assert!(
        !commits.is_empty(),
        "smoke repo {repo} produced no history — the check did not run"
    );
    let (resolved, refs, head, default_branch) = mainline_for(&repo);
    let chain = mainline_chain_ids(&commits, &resolved.hint);
    assert!(
        !chain.is_empty(),
        "no mainline chain — the check did not run"
    );

    // The busiest author, the busiest conventional type, and a free-text
    // word from the newest summary: three filters real users type.
    let mut by_author: std::collections::HashMap<&str, usize> = Default::default();
    for c in &commits {
        *by_author.entry(c.author_name.as_str()).or_default() += 1;
    }
    let author = by_author
        .iter()
        .max_by_key(|(_, n)| **n)
        .map(|(a, _)| a.split_whitespace().next().unwrap_or(a).to_string())
        .expect("an author");
    let word = commits[0]
        .summary
        .split(|ch: char| !ch.is_alphanumeric())
        .find(|w| w.len() >= 3)
        .unwrap_or("a")
        .to_string();
    let queries = [
        format!("author:{author}"),
        "fix:".to_string(),
        word,
        "sha:0".to_string(),
    ];
    let mut ran = 0;
    for query in &queries {
        let filter = CommitFilter::parse(query);
        let keep: Vec<bool> = commits.iter().map(|c| filter.matches_commit(c)).collect();
        let kept = keep.iter().filter(|k| **k).count();
        if kept == 0 || kept == commits.len() {
            println!(
                "{repo}: query {query:?} keeps {kept}/{} rows — skipped",
                commits.len()
            );
            continue;
        }
        ran += 1;
        let payload = assemble_commit_graph(
            commits.clone(),
            commits.len(),
            &filter,
            refs.clone(),
            default_branch.as_deref(),
            head.clone(),
            Vec::new(),
        );
        assert_eq!(payload.rows.len(), kept, "{query:?}: filtered row count");
        check_no_stub_points_at_a_loaded_row(&payload.rows);
        check_stable_column_invariants(
            &payload
                .rows
                .iter()
                .map(|r| RawCommitNode {
                    id: r.id.clone(),
                    parent_ids: r.parent_ids.clone(),
                    timestamp: r.timestamp,
                    author_name: r.author_name.clone(),
                    author_email: r.author_email.clone(),
                    summary: r.summary.clone(),
                })
                .collect::<Vec<_>>(),
            &payload.rows,
        );
        // The rail: anchored on the chain's first survivor, straight, in
        // the mainline colour, and named after the branch it came from.
        let loaded: std::collections::HashSet<&str> =
            payload.rows.iter().map(|r| r.id.as_str()).collect();
        let survivor = chain.iter().find(|id| loaded.contains(id.as_str()));
        assert_eq!(
            payload.mainline_id.as_deref(),
            survivor
                .map(String::as_str)
                .or(payload.rows.first().map(|r| r.id.as_str())),
            "{query:?}: mainline anchor"
        );
        if survivor.is_some() {
            assert_eq!(
                payload.mainline_name,
                resolved.name_for(&chain[0], &refs),
                "{query:?}: the rail keeps the branch's name"
            );
        }
        for row in payload.rows.iter().filter(|r| r.is_mainline) {
            assert_eq!(
                row.lane, MAINLINE_COLUMN,
                "{query:?}: {} off the rail",
                row.id
            );
            assert_eq!(
                row.color_index, MAINLINE_COLOR,
                "{query:?}: {} off-colour",
                row.id
            );
        }
        println!(
            "{repo}: query {query:?} keeps {kept} rows, {} on the rail ({:?}), width {}",
            payload.rows.iter().filter(|r| r.is_mainline).count(),
            payload.mainline_name,
            payload.rows.iter().map(|r| r.lane).max().unwrap_or(0) + 1
        );
    }
    assert!(
        ran > 0,
        "every query degenerated on {repo} — the check did not run"
    );
}

/// GITPULSE_SMOKE_REPO=/path/to/repo GITPULSE_SMOKE_DUMP=/tmp/graph.json \
///   GITPULSE_SMOKE_DUMP_ROWS=300 cargo test --test real_repo_smoke -- --ignored
/// ```
/// What the named scope leaves out on a REAL repository, reported the way the
/// graph pane reports it.
///
/// The synthetic fixtures in `graph_ref_scope.rs` prove the mechanism; this
/// prints the actual sentence a user would see, so a repository carrying
/// agent-harness or prefetch namespaces can be checked by eye.
#[test]
#[ignore = "needs GITPULSE_SMOKE_REPO pointing at a real repository"]
fn real_repository_reports_the_history_the_named_scope_hides() {
    let repo =
        std::env::var("GITPULSE_SMOKE_REPO").expect("set GITPULSE_SMOKE_REPO to a repository path");
    let hidden = probe_hidden_history(&repo).expect("hidden-history probe");
    match hidden_ref_warning(&hidden) {
        Some(note) => {
            assert!(
                hidden.commits > 0,
                "a warning was produced for zero hidden commits"
            );
            println!("{repo}: {note}");
        }
        None => {
            assert_eq!(
                hidden.commits, 0,
                "{repo} hides {} commit(s) and said nothing",
                hidden.commits
            );
            println!("{repo}: nothing outside branches, remotes and tags");
        }
    }
}

#[test]
#[ignore = "needs GITPULSE_SMOKE_REPO pointing at a real repository"]
fn real_repository_mainline_is_one_straight_rail() {
    let repo =
        std::env::var("GITPULSE_SMOKE_REPO").expect("set GITPULSE_SMOKE_REPO to a repository path");
    let commits = load_history(&repo, 5_000);
    assert!(
        !commits.is_empty(),
        "smoke repo {repo} produced no history — the check did not run"
    );
    let (resolved, refs, head, default_branch) = mainline_for(&repo);
    assert!(
        !resolved.hint.branch_tips.is_empty(),
        "smoke repo {repo} has no default-branch tip to pin — the check did not run"
    );
    let rows = LaneSolver::new(12).solve_with_mainline(&commits, &resolved.hint);
    check_stable_column_invariants(&commits, &rows);
    check_mainline_invariants(&commits, &rows, &resolved.hint);

    // The repository's own default branch tip, when loaded, is ON the rail.
    if let Some(name) = default_branch.as_deref() {
        if let Some(tip) = refs
            .iter()
            .find(|r| r.kind == RefKind::Local && r.name == name)
        {
            if let Some(row) = rows.iter().find(|r| r.id == tip.commit_id) {
                assert!(row.is_mainline, "{name}'s tip is not on the mainline");
            }
        }
    }
    let mainline_rows = rows.iter().filter(|r| r.is_mainline).count();
    println!(
        "{repo}: {} rows, {mainline_rows} on the mainline ({:?}), width {}",
        rows.len(),
        rows.iter()
            .find(|r| r.is_mainline)
            .and_then(|r| resolved.name_for(&r.id, &refs)),
        rows.iter().map(|r| r.lane).max().unwrap_or(0) + 1
    );

    for window in [1, 2, 10, 127, 300, 1000] {
        let cut: Vec<RawCommitNode> = commits.iter().take(window).cloned().collect();
        let rows = LaneSolver::new(12).solve_with_mainline(&cut, &resolved.hint);
        check_stable_column_invariants(&cut, &rows);
        check_mainline_invariants(&cut, &rows, &resolved.hint);
    }

    if let Ok(out) = std::env::var("GITPULSE_SMOKE_DUMP") {
        let n: usize = std::env::var("GITPULSE_SMOKE_DUMP_ROWS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300);
        // GITPULSE_SMOKE_QUERY applies a commit filter exactly as the
        // command does (server-side, with history simplification).
        let query = std::env::var("GITPULSE_SMOKE_QUERY").unwrap_or_default();
        let window: Vec<RawCommitNode> = commits.iter().take(n + 1).cloned().collect();
        let payload = assemble_commit_graph(
            window,
            n,
            &CommitFilter::parse(&query),
            refs.clone(),
            default_branch.as_deref(),
            head.clone(),
            Vec::new(),
        );
        check_no_stub_points_at_a_loaded_row(&payload.rows);
        let json = serde_json::to_string(&payload).expect("serialize payload");
        std::fs::write(&out, json).expect("write fixture");
        println!(
            "wrote {}-row fixture (query {query:?}) to {out}",
            payload.rows.len()
        );
    }
}

#[test]
#[ignore = "needs GITPULSE_SMOKE_REPO pointing at a real repository"]
fn real_repository_history_solves_clean() {
    let repo =
        std::env::var("GITPULSE_SMOKE_REPO").expect("set GITPULSE_SMOKE_REPO to a repository path");
    let commits = load_history(&repo, 5_000);
    assert!(
        !commits.is_empty(),
        "smoke repo {repo} produced no history — the check did not run"
    );
    let rows = LaneSolver::new(12).solve(&commits);
    check_stable_column_invariants(&commits, &rows);

    // Windowed loads must also hold: truncation turns tail parents into
    // dangling stubs, never into edges pointing at wrong rows.
    for window in [1, 2, 10, 127, 1000] {
        let cut: Vec<RawCommitNode> = commits.iter().take(window).cloned().collect();
        let rows = LaneSolver::new(12).solve(&cut);
        check_stable_column_invariants(&cut, &rows);
    }
}

#[test]
#[ignore = "needs GITPULSE_SMOKE_REPO pointing at a real repository"]
fn real_repository_status_smoke() {
    let repo =
        std::env::var("GITPULSE_SMOKE_REPO").expect("set GITPULSE_SMOKE_REPO to a repository path");
    let statuses = gitpulse_lib::engine::GitReader::get_status(&repo).expect("get_status");
    println!("Real repository status count: {}", statuses.len());
    let mut total_add = 0;
    let mut total_del = 0;
    for s in &statuses {
        total_add += s.additions;
        total_del += s.deletions;
        if s.additions > 0 || s.deletions > 0 || s.status_code == "??" {
            println!(
                "  [{}] {} (+{} -{})",
                s.status_code, s.path, s.additions, s.deletions
            );
        }
    }
    println!("Total uncommitted churn: +{} -{}", total_add, total_del);
    assert!(!statuses.is_empty(), "expected dirty repo with changes");
}
