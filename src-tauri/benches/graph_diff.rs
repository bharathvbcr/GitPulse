//! Intra-line word diff and the stable-column lane solver, on generated input.
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench graph_diff`
//! (`-- --smoke` for 3 samples, `-- --json <path>` for the report).
//!
//! Neither hot path had a benchmark: `compute_word_diff` had a bare `< 50 ms`
//! assertion in `tests/stress_test.rs`, and `LaneSolver::solve` had 5-15 s
//! ceilings in `tests/lane_solver_stress.rs` that catch an O(n^2) regression
//! and nothing smaller. These rows give both a trend, with the work they did
//! (segments emitted, rows laid out, connections drawn) reported as counters so
//! a change that does less work cannot pass as a speedup.
//!
//! Word diff, by line length:
//!
//! * 100 chars: a handful of tokens, the common case in a diff view.
//! * 10k chars: past the 500-token cap, so the LCS table is at its maximum
//!   size (500 x 500). This is the row that costs.
//! * 60k chars: past the 50,000-byte guard, which returns whole-line
//!   Removed/Added without tokenizing. The row exists so that guard stays a
//!   guard.
//!
//! Lane solver, by history shape: `linear` is one first-parent chain;
//! `merge-heavy` is the repeating six-commit merge cycle from the solver's
//! unit suite (a mainline of merges with a feature and a dummy branch per
//! cycle), the shape that exercises column allocation.

mod common;

use common::{interleave, Case, Report, Sample};
use gitpulse_lib::diff::word_diff::{compute_word_diff, DiffChunkKind};
use gitpulse_lib::graph::{LaneSolver, RawCommitNode};

/// A line of about `chars` characters made of `word_N` tokens, and the same
/// line with every seventh word replaced.
fn line_pair(chars: usize) -> (String, String) {
    let mut old = String::with_capacity(chars + 16);
    let mut new = String::with_capacity(chars + 16);
    let mut word = 0usize;
    while old.len() < chars {
        old.push_str(&format!("word_{word} "));
        if word % 7 == 3 {
            new.push_str(&format!("changed_{word} "));
        } else {
            new.push_str(&format!("word_{word} "));
        }
        word += 1;
    }
    (old, new)
}

fn commit(id: String, parents: Vec<String>) -> RawCommitNode {
    RawCommitNode {
        summary: format!("commit {id}"),
        id,
        parent_ids: parents,
        timestamp: 0,
        author_name: "Bench".to_string(),
        author_email: "bench@example.com".to_string(),
    }
}

fn linear(n: usize) -> Vec<RawCommitNode> {
    (0..n)
        .map(|i| {
            let parents = if i + 1 < n {
                vec![format!("c{}", i + 1)]
            } else {
                Vec::new()
            };
            commit(format!("c{i}"), parents)
        })
        .collect()
}

fn merge_heavy(cycles: usize) -> Vec<RawCommitNode> {
    let mut commits = Vec::with_capacity(cycles * 6 + 1);
    for i in (0..cycles).rev() {
        let mainline = if i == 0 {
            "root".to_string()
        } else {
            format!("m{}", i - 1)
        };
        commits.push(commit(format!("t{i}"), vec![format!("d{i}")]));
        commits.push(commit(format!("e{i}"), vec![format!("m{i}")]));
        commits.push(commit(format!("d{i}"), vec![format!("dp{i}")]));
        commits.push(commit(format!("dp{i}"), vec![]));
        commits.push(commit(format!("m{i}"), vec![mainline, format!("f{i}")]));
        commits.push(commit(format!("f{i}"), vec![]));
    }
    commits.push(commit("root".to_string(), vec![]));
    commits
}

fn solve_case<'a>(name: &str, commits: &'a [RawCommitNode]) -> Case<'a> {
    Case::new(name, move || {
        let rows = LaneSolver::new(12).solve(commits);
        let connections: usize = rows.iter().map(|r| r.connections.len()).sum();
        let merges = rows.iter().filter(|r| r.is_merge).count();
        let width = rows
            .iter()
            .flat_map(|r| r.active_lanes.iter().copied())
            .max()
            .map_or(0, |lane| u64::from(lane) + 1);
        std::hint::black_box(&rows);
        Sample::default()
            .counter("rows", rows.len() as u64)
            .counter("connections", connections as u64)
            .counter("merges", merges as u64)
            .counter("width", width)
    })
}

fn main() {
    let mut report = Report::new("graph_diff");
    let opts = &report.opts;

    // ---- compute_word_diff
    let pairs: Vec<(usize, (String, String))> = [100usize, 10_000, 60_000]
        .into_iter()
        .map(|chars| (chars, line_pair(chars)))
        .collect();
    let word_cases: Vec<Case> = pairs
        .iter()
        .map(|(chars, (old, new))| {
            Case::new(format!("word_diff {chars} chars"), move || {
                let diff = compute_word_diff(old, new);
                let changed = diff
                    .original_segments
                    .iter()
                    .chain(&diff.modified_segments)
                    .filter(|s| s.kind != DiffChunkKind::Equal)
                    .count();
                std::hint::black_box(&diff);
                Sample::default()
                    .counter("input_bytes", (old.len() + new.len()) as u64)
                    .counter("original_segments", diff.original_segments.len() as u64)
                    .counter("modified_segments", diff.modified_segments.len() as u64)
                    .counter("changed_segments", changed as u64)
            })
        })
        .collect();
    let word_rows = interleave(word_cases, opts.samples(100), opts.warmup(5));

    // ---- LaneSolver::solve
    let fixtures = [
        ("linear 10k", linear(10_000)),
        ("merge-heavy 10k", merge_heavy(1_666)),
        ("linear 100k", linear(100_000)),
        ("merge-heavy 100k", merge_heavy(16_600)),
    ];
    let small: Vec<Case> = fixtures[..2]
        .iter()
        .map(|(name, commits)| solve_case(&format!("lane_solver {name}"), commits))
        .collect();
    let large: Vec<Case> = fixtures[2..]
        .iter()
        .map(|(name, commits)| solve_case(&format!("lane_solver {name}"), commits))
        .collect();
    let small_rows = interleave(small, opts.samples(30), opts.warmup(2));
    let large_rows = interleave(large, opts.samples(7), opts.warmup(1));

    report.note(
        "fixtures are generated in-process (no repository): word-diff lines of ~100/10k/60k chars \
         with every 7th word changed; linear and 6-commit-cycle merge-heavy histories"
            .to_string(),
    );
    report.extend(word_rows);
    report.extend(small_rows);
    report.extend(large_rows);
    report.finish();
}
