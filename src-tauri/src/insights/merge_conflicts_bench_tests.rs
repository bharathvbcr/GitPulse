//! Benchmark, not a correctness test: what one pair costs
//! `gitpulse_merge_conflicts`, on a repository of 2,000 tracked files. Run by
//! name, `cargo test --manifest-path src-tauri/Cargo.toml --lib -- --ignored
//! --exact insights::merge_conflicts_bench_tests::bench_merge_tree_per_pair`.
//!
//! Three rows, interleaved, min and median of the rounds, with the host load:
//! `merge_tree_candidate` on a clean pair and on a conflicting pair, and a raw
//! `git merge-tree --write-tree` as the control. The control writes its merged
//! tree into the fixture's object store, which the quarantine exists to
//! prevent, so it is a control and never a code path.

use super::*;
use crate::engine::object_quarantine::{merge_tree_candidate, MergeCandidate};
use crate::test_support::{git_in, trust_repo};
use std::time::Instant;

const FILES: usize = 2_000;
const DIRS: usize = 50;
const ROUNDS: usize = 15;

fn load() -> String {
    use crate::procguard::LockedSpawn;
    std::process::Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output_locked()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default()
}

fn head(dir: &std::path::Path) -> String {
    crate::engine::git_cli::git_text(dir, &["rev-parse", "HEAD"])
        .expect("rev-parse")
        .trim()
        .to_string()
}

/// A branch off `main` with one file rewritten; returns its commit.
fn lane(dir: &std::path::Path, name: &str, file: &str, content: &str) -> String {
    git_in(dir, &["checkout", "-q", "-b", name, "main"]);
    crate::test_support::write(dir, file, content);
    git_in(dir, &["add", "-A"]);
    git_in(dir, &["commit", "-q", "-m", name]);
    let oid = head(dir);
    git_in(dir, &["checkout", "-q", "main"]);
    oid
}

fn row(name: &str, mut times: Vec<f64>) {
    times.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
    println!(
        "BENCH merge_conflicts {name}: min_ms={:.2} median_ms={:.2} n={}",
        times[0],
        times[times.len() / 2],
        times.len()
    );
}

#[test]
#[ignore = "benchmark: run explicitly"]
fn bench_merge_tree_per_pair() {
    let dir = tempfile::tempdir().expect("repo");
    let repo = dir.path().canonicalize().expect("canonical");
    git_in(&repo, &["init", "-q", "-b", "main"]);
    trust_repo(&repo);
    for index in 0..FILES {
        let file = format!("d{:03}/f{index:06}.rs", index % DIRS);
        let text: String = (0..24)
            .map(|line| {
                format!(
                    "// file {index} line {line}: let v_{line} = {};\n",
                    index + line
                )
            })
            .collect();
        crate::test_support::write(&repo, &file, &text);
    }
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-q", "-m", "initial tree"]);
    let left = lane(&repo, "bench/left", "d000/f000000.rs", "left edit\n");
    let disjoint = lane(
        &repo,
        "bench/disjoint",
        "d001/f000001.rs",
        "disjoint edit\n",
    );
    let same = lane(&repo, "bench/same", "d000/f000000.rs", "same-lines edit\n");

    let budget = Duration::from_secs(60);
    let (mut clean, mut conflict, mut raw) = (Vec::new(), Vec::new(), Vec::new());
    for round in 0..ROUNDS + 2 {
        let started = Instant::now();
        let answer = merge_tree_candidate(&repo, &left, &disjoint, Instant::now() + budget);
        let took = started.elapsed().as_secs_f64() * 1000.0;
        assert!(matches!(answer, MergeCandidate::Clean { .. }), "{answer:?}");
        let started = Instant::now();
        let answer = merge_tree_candidate(&repo, &left, &same, Instant::now() + budget);
        let took_conflict = started.elapsed().as_secs_f64() * 1000.0;
        assert!(
            matches!(answer, MergeCandidate::Conflict { .. }),
            "{answer:?}"
        );
        let started = Instant::now();
        git_in(&repo, &["merge-tree", "--write-tree", &left, &disjoint]);
        let took_raw = started.elapsed().as_secs_f64() * 1000.0;
        // Two warm-up rounds are not recorded.
        if round >= 2 {
            clean.push(took);
            conflict.push(took_conflict);
            raw.push(took_raw);
        }
    }
    println!("BENCH merge_conflicts load={}", load());
    row("merge_tree_candidate clean pair", clean);
    row("merge_tree_candidate conflict pair", conflict);
    row("raw git merge-tree clean pair (control)", raw);
}
