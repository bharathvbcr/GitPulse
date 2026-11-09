//! What one graph window costs once git has answered: assembling and
//! lane-solving it, serializing it, decoding it, and the `unchanged` answer
//! that replaces all of that when a revision token still names the state.
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench graph_window`
//! (`-- --smoke` for 3 samples, `-- --json <path>` for the report).
//!
//! The performance audit of 2026-11-09 could not say what a 100,000-row
//! "load more" window costs after the walk: how long the lane solve takes and
//! how many bytes cross the IPC boundary. Its estimate (45-60 MB) was inferred
//! from a row's key count. These rows measure it.
//!
//! Rows, at 5k / 30k / 100k commits:
//!
//! * `assemble`: `assemble_commit_graph` on generated history, which is the
//!   lane solve (`LaneSolver::solve_with_mainline`) plus the mainline lookup
//!   and the payload around it. Counters: rows, connections.
//! * `serialize`: `serde_json::to_vec` of that payload, as the IPC layer does
//!   before it hands the bytes to the WebView. Counter: `payload_bytes`.
//! * `decode`: `serde_json::from_slice` into a `Value`, a stand-in for the
//!   WebView's `JSON.parse` (same bytes, same allocation shape; the real parse
//!   is not measured here).
//! * `unchanged`: serializing the answer a matching token gets. Counter:
//!   `payload_bytes`, the O(1) the token buys.
//!
//! # Fixture
//!
//! Generated in process, no repository, newest first. A mainline with a
//! two-parent merge every 12th commit joining a three-commit side branch, so
//! the solver allocates and frees columns as a real history does. Subjects
//! are ~55 characters and the author a normal name and address: the
//! audit measured GitPulse's real rows at about 1.7x the size of the
//! `git log` fixture's. This is a synthetic shape; a real repository's width
//! and subject lengths move the byte counts, which is why they are counters
//! of THIS fixture and not a prediction for another.

mod common;

use common::{interleave, Case, Report, Sample};
use gitpulse_lib::analyzer::CommitFilter;
use gitpulse_lib::commands::{assemble_commit_graph, CommitGraphPayload, GraphNotes};
use gitpulse_lib::graph::RawCommitNode;

const SIZES: [(&str, usize); 3] = [("5k", 5_000), ("30k", 30_000), ("100k", 100_000)];

fn node(n: usize, parents: Vec<String>) -> RawCommitNode {
    RawCommitNode {
        id: format!("{n:040x}"),
        parent_ids: parents,
        timestamp: 1_700_000_000 + n as i64,
        author_name: "Ada Lovelace".to_string(),
        author_email: "ada.lovelace@example.com".to_string(),
        summary: format!("fix(graph): keep lane {n} straight across the merge window"),
    }
}

/// Newest first, `count` commits: ids count down from `count`, parents are the
/// next lower id (first parent the mainline), and every 12th mainline commit
/// is a merge of a three-commit side branch.
fn history(count: usize) -> Vec<RawCommitNode> {
    let mut commits = Vec::with_capacity(count);
    let mut next = count;
    while next > 0 && commits.len() < count {
        // A merge at `next`: first parent next-4, second parent the tip of a
        // side branch next-3 -> next-2 -> next-1 whose root is next-4.
        if next > 5 && next.is_multiple_of(12) {
            commits.push(node(
                next,
                vec![format!("{:040x}", next - 4), format!("{:040x}", next - 1)],
            ));
            commits.push(node(next - 1, vec![format!("{:040x}", next - 2)]));
            commits.push(node(next - 2, vec![format!("{:040x}", next - 3)]));
            commits.push(node(next - 3, vec![format!("{:040x}", next - 4)]));
            next -= 4;
        } else {
            let parents = if next > 1 {
                vec![format!("{:040x}", next - 1)]
            } else {
                Vec::new()
            };
            commits.push(node(next, parents));
            next -= 1;
        }
    }
    commits.truncate(count);
    commits
}

fn assemble(commits: &[RawCommitNode]) -> CommitGraphPayload {
    assemble_commit_graph(
        commits.to_vec(),
        commits.len(),
        &CommitFilter::default(),
        Vec::new(),
        Some("main"),
        commits.first().map(|c| c.id.clone()),
        GraphNotes::default(),
    )
}

fn main() {
    let mut report = Report::new("graph_window");
    let opts = &report.opts;

    let fixtures: Vec<(&str, Vec<RawCommitNode>)> = SIZES
        .iter()
        .map(|(label, count)| (*label, history(*count)))
        .collect();
    let payloads: Vec<CommitGraphPayload> = fixtures.iter().map(|(_, c)| assemble(c)).collect();
    let encoded: Vec<Vec<u8>> = payloads
        .iter()
        .map(|p| serde_json::to_vec(p).expect("serialize payload"))
        .collect();
    let unchanged = CommitGraphPayload {
        rows: Vec::new(),
        head_id: fixtures[0].1.first().map(|c| c.id.clone()),
        refs: Vec::new(),
        has_more: false,
        warnings: Vec::new(),
        notices: Vec::new(),
        mainline_id: None,
        mainline_name: None,
        graph_token: Some("0123456789abcdef".to_string()),
        unchanged: true,
    };

    let mut cases: Vec<Case> = Vec::new();
    for (index, (label, commits)) in fixtures.iter().enumerate() {
        let payload = &payloads[index];
        let bytes = &encoded[index];
        cases.push(Case::new(format!("assemble {label}"), move || {
            let payload = assemble(commits);
            let connections: usize = payload.rows.iter().map(|r| r.connections.len()).sum();
            std::hint::black_box(&payload);
            Sample::default()
                .counter("rows", payload.rows.len() as u64)
                .counter("connections", connections as u64)
        }));
        cases.push(Case::new(format!("serialize {label}"), move || {
            let out = serde_json::to_vec(payload).expect("serialize");
            std::hint::black_box(&out);
            Sample::default()
                .counter("rows", payload.rows.len() as u64)
                .counter("payload_bytes", out.len() as u64)
        }));
        cases.push(Case::new(format!("decode {label}"), move || {
            let value: serde_json::Value = serde_json::from_slice(bytes).expect("decode");
            std::hint::black_box(&value);
            Sample::default().counter("payload_bytes", bytes.len() as u64)
        }));
    }
    cases.push(Case::new("unchanged answer", || {
        let out = serde_json::to_vec(&unchanged).expect("serialize");
        std::hint::black_box(&out);
        Sample::default().counter("payload_bytes", out.len() as u64)
    }));

    let rows = interleave(cases, opts.samples(7), opts.warmup(1));
    report.note(
        "fixture generated in process: mainline with a 3-commit side branch merged every 12th \
         commit, ~55-character subjects; synthetic, so byte counts belong to this fixture"
            .to_string(),
    );
    report.extend(rows);
    report.finish();
}
