//! `GitReader::get_status` on generated repositories of 1k and 10k files,
//! clean and dirty.
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench repo_read`.
//!
//! `get_status` is what every watcher-triggered refresh pays. The work counter
//! that matters is the number of `git` processes it starts (counted from git's
//! own trace2 in a separate untimed pass, see `common::fixture`): a
//! regression to one spawn per file reads as `git_processes` growing with the
//! file count long before the wall clock moves. `rows` is the number of status
//! rows returned, so a change that returns fewer cannot pass as a speedup.
//!
//! All four repositories are generated once, in a hermetic temp `HOME`; dirty
//! ones have 50 (1k) or 500 (10k) tracked files modified in the working tree.
//! The spawn rate is lifted as the stress tests lift it, so the rows time the
//! work and not the production gate's 8-spawns-per-second refill.

mod common;

use common::fixture::{build_repo, count_git_processes, dirty_files, Repo, Spec};
use common::{interleave, Case, Report, Sample};
use gitpulse_lib::engine::git_reader::GitReader;

struct Shape {
    label: &'static str,
    spec: Spec,
    dirty: usize,
}

fn main() {
    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
    let mut report = Report::new("repo_read");
    let temp = tempfile::tempdir().expect("temp dir");

    let shapes = [
        Shape {
            label: "1k files",
            spec: Spec { files: 1_000, dirs: 50, extra_commits: 3, touch: 5, linked_worktrees: 0 },
            dirty: 50,
        },
        Shape {
            label: "10k files",
            spec: Spec { files: 10_000, dirs: 200, extra_commits: 3, touch: 5, linked_worktrees: 0 },
            dirty: 500,
        },
    ];

    // (name, repository) pairs, built once.
    let mut repos: Vec<(String, Repo)> = Vec::new();
    for (index, shape) in shapes.iter().enumerate() {
        for state in ["clean", "dirty"] {
            let root = temp.path().join(format!("r{index}-{state}"));
            let repo = build_repo(&root, &shape.spec);
            if state == "dirty" {
                dirty_files(&repo, &shape.spec, shape.dirty);
            }
            repos.push((format!("get_status {} {state}", shape.label), repo));
        }
    }
    report.note(
        "generated repositories: 1k files (50 dirty) and 10k files (500 dirty), 4 commits each, \
         hermetic HOME, production spawn rate lifted"
            .to_string(),
    );

    let status = |repo: &Repo| {
        GitReader::get_status(repo.path()).unwrap_or_else(|error| {
            panic!("get_status failed on {}: {error}", repo.path());
        })
    };

    // Work counters first, in their own untimed pass.
    let mut processes = Vec::new();
    for (_, repo) in &repos {
        let (_rows, spawned) = count_git_processes(|| status(repo));
        processes.push(spawned);
    }

    let cases: Vec<Case> = repos
        .iter()
        .zip(&processes)
        .map(|((name, repo), spawned)| {
            let spawned = *spawned;
            Case::new(name.clone(), move || {
                let rows = status(repo);
                let count = rows.len() as u64;
                std::hint::black_box(&rows);
                Sample::default()
                    .counter("rows", count)
                    .counter("git_processes", spawned)
            })
        })
        .collect();
    let rows = interleave(cases, report.opts.samples(15), report.opts.warmup(2));
    report.extend(rows);
    report.finish();
}
