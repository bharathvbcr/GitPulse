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

use gitpulse_lib::graph::{LaneSolver, RawCommitNode, VisualCommitRow};
use std::collections::{HashMap, HashSet};
use std::process::Command;

fn load_history(repo: &str, max: usize) -> Vec<RawCommitNode> {
    let output = Command::new("git")
        .args([
            "-C",
            repo,
            "log",
            "--all",
            "--topo-order",
            &format!("--max-count={max}"),
            "--format=%H\u{1}%P\u{1}%ct\u{1}%an\u{1}%ae\u{1}%s",
        ])
        .output()
        .expect("git must be runnable for the smoke test");
    assert!(
        output.status.success(),
        "git log failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split('\u{1}');
            let id = parts.next()?.to_string();
            let parents = parts.next().unwrap_or_default();
            let timestamp = parts.next().and_then(|t| t.parse().ok()).unwrap_or(0);
            let author_name = parts.next().unwrap_or_default().to_string();
            let author_email = parts.next().unwrap_or_default().to_string();
            let summary = parts.next().unwrap_or_default().to_string();
            Some(RawCommitNode {
                id,
                parent_ids: parents.split_whitespace().map(str::to_string).collect(),
                timestamp,
                author_name,
                author_email,
                summary,
            })
        })
        .collect()
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
        if !contested {
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

    // Width == peak occupancy (interval allocation is optimal).
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
