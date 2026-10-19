//! What one pair costs `gitpulse_merge_conflicts`: `merge_tree_candidate` on a
//! repository of 2,000 tracked files.
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench merge_conflicts`.
//!
//! The tool merges every pair of worktree HEADs, so its cost is the pair count
//! times one `merge_tree_candidate`. Each call resolves nothing it was not
//! given a name for, makes a quarantine directory, runs one `git merge-tree
//! --write-tree` and deletes the directory. Rows, interleaved:
//!
//! * `merge_tree_candidate clean pair`: two commits editing different files;
//! * `merge_tree_candidate conflict pair`: two commits editing the same lines;
//! * `raw git merge-tree clean pair (control)`: the same clean merge run
//!   straight at the repository, which is what the quarantine and the operand
//!   resolution are measured against. It writes its merged tree into the
//!   fixture's object store, the thing the quarantine exists to prevent, so it
//!   is a control and never a code path.

mod common;

use common::fixture::{build_repo, count_git_processes, git, trust_repo, Spec};
use common::{interleave, Case, Report, Sample};
use gitpulse_lib::engine::object_quarantine::{merge_tree_candidate, MergeCandidate};
use std::time::{Duration, Instant};

const SPEC: Spec = Spec {
    files: 2000,
    dirs: 50,
    extra_commits: 0,
    touch: 0,
    linked_worktrees: 0,
};

/// A branch off `main` with `file` rewritten, returning its commit.
fn lane(main: &std::path::Path, name: &str, file: &str, content: &str) -> String {
    git(main, &["checkout", "-q", "-b", name, "main"]);
    std::fs::write(main.join(file), content).expect("write lane file");
    git(main, &["add", "-A"]);
    git(main, &["commit", "-q", "-m", &format!("feat: {name}")]);
    let oid = git(main, &["rev-parse", "HEAD"]).trim().to_string();
    git(main, &["checkout", "-q", "main"]);
    oid
}

fn main() {
    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
    let mut report = Report::new("merge_conflicts");
    let temp = tempfile::tempdir().expect("temp dir");
    let repo = build_repo(&temp.path().join("fixture"), &SPEC);
    trust_repo(&repo.main);
    // `d000/f000000.rs` and `d001/f000001.ts` are two of the generated files.
    let left = lane(&repo.main, "bench/left", "d000/f000000.rs", "left edit\n");
    let disjoint = lane(
        &repo.main,
        "bench/disjoint",
        "d001/f000001.ts",
        "disjoint edit\n",
    );
    let same = lane(
        &repo.main,
        "bench/same",
        "d000/f000000.rs",
        "same-lines edit\n",
    );
    report.note(format!(
        "fixture: {} tracked files in {} directories, three one-file lanes off main, hermetic HOME, production spawn rate lifted",
        SPEC.files, SPEC.dirs
    ));

    let budget = Duration::from_secs(60);
    let candidate = |other: &str, want_conflict: bool| {
        let started = Instant::now();
        let answer = merge_tree_candidate(&repo.main, &left, other, Instant::now() + budget);
        let elapsed = started.elapsed();
        match (&answer, want_conflict) {
            (MergeCandidate::Conflict { .. }, true) | (MergeCandidate::Clean { .. }, false) => {}
            _ => panic!("the pair did not merge as the row claims: {answer:?}"),
        }
        Sample::default()
            .counter("conflicts", u64::from(want_conflict))
            .elapsed(elapsed)
    };
    let (_, candidate_procs) = count_git_processes(|| {
        candidate(&disjoint, false);
    });
    let raw = || {
        let started = Instant::now();
        git(
            &repo.main,
            &["merge-tree", "--write-tree", &left, &disjoint],
        );
        Sample::default().elapsed(started.elapsed())
    };

    let cases = vec![
        Case::new("merge_tree_candidate clean pair", || {
            candidate(&disjoint, false).counter("git_processes", candidate_procs)
        }),
        Case::new("merge_tree_candidate conflict pair", || {
            candidate(&same, true)
        }),
        Case::new("raw git merge-tree clean pair (control)", raw),
    ];
    let rows = interleave(cases, report.opts.samples(15), report.opts.warmup(2));
    report.extend(rows);
    report.finish();
}
