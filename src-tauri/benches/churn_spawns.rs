//! `git` processes and time for `branch_stats` after the default branch moves
//! for a reason that has nothing to do with the branches (S4-06).
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench churn_spawns`.
//!
//! * `branch_stats after a default-branch advance unrelated to N branches`:
//!   `cmd_branch_stats` on a repository whose `main` has one commit and which
//!   has `N` branches, each one commit ahead of it touching its own file. A
//!   priming call measures every branch. Then `main` gets one more commit that
//!   none of the branches has anything to do with, and the call is made again:
//!   the same thing the app does on every repository change.
//!
//! Counters (deterministic, asserted equal across samples): processes by kind
//! for the call after the advance, counted from git's own trace2 in a separate
//! untimed pass, and the branches the report carries, so a change that returns
//! less cannot pass as a speedup. `shortstat` is the `diff --shortstat`
//! measurements, `merge_base` the processes that name `merge-base`,
//! `ahead_behind` the `for-each-ref` listings that carry `%(ahead-behind:)`,
//! `git_processes` every process. The wall clock is a property of host load;
//! the process counters are the gate.
//!
//! The counted call's processes are the difference between two counted blocks
//! on separate copies of the repository, one that stops after the advance and
//! one that makes the call, so the priming call and the advance itself (which
//! are `git` processes too) do not count against the call.

mod common;

use common::fixture::{git, git_process_argvs, trust_repo};
use common::{interleave, Case, Report, Sample};
use gitpulse_lib::procguard::LockedSpawn;
use std::io::Write;
use std::path::Path;

const BRANCHES: usize = 32;

/// `main` with one commit and `BRANCHES` branches (`b00`...), each one commit
/// on top of it that adds a file of its own. Written with `git fast-import`.
fn build(root: &Path) -> String {
    std::fs::create_dir_all(root).expect("create fixture root");
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.name", "Bench"]);
    git(root, &["config", "user.email", "bench@example.com"]);
    let mut stream = String::new();
    let commit =
        |stream: &mut String, refname: &str, mark: usize, from: Option<usize>, path: &str| {
            let body = format!("content of {path}\n{}", "line\n".repeat(20));
            let message = format!("change {path}\n");
            stream.push_str(&format!("commit {refname}\nmark :{mark}\n"));
            stream.push_str(&format!(
                "committer Bench <bench@example.com> {} +0000\n",
                1_700_000_000 + mark
            ));
            stream.push_str(&format!("data {}\n{message}", message.len()));
            if let Some(parent) = from {
                stream.push_str(&format!("from :{parent}\n"));
            }
            stream.push_str(&format!(
                "M 100644 inline {path}\ndata {}\n{body}\n",
                body.len()
            ));
        };
    commit(&mut stream, "refs/heads/main", 1, None, "base.txt");
    for i in 0..BRANCHES {
        commit(
            &mut stream,
            &format!("refs/heads/b{i:02}"),
            i + 2,
            Some(1),
            &format!("branch-{i:02}.txt"),
        );
    }
    let mut child = std::process::Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn_locked()
        .expect("spawn git fast-import");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stream.as_bytes())
        .expect("feed fast-import");
    let out = child.wait_with_output().expect("wait for fast-import");
    assert!(
        out.status.success(),
        "fast-import failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    git(root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    git(root, &["reset", "-q", "--hard", "main"]);
    trust_repo(root);
    root.to_str().expect("utf-8").to_string()
}

/// One more commit on `main`, which no branch has anything to do with.
fn advance(repo: &str, n: usize) {
    git(
        Path::new(repo),
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!("unrelated {n}"),
        ],
    );
}

fn branch_stats(repo: &str) -> usize {
    let report =
        tauri::async_runtime::block_on(gitpulse_lib::commands::cmd_branch_stats(repo.to_string()))
            .unwrap_or_else(|error| panic!("cmd_branch_stats failed: {error}"));
    assert_eq!(
        report.updates.len(),
        BRANCHES,
        "a branch is missing, so this run would time a partial answer"
    );
    assert!(!report.capped, "{BRANCHES} branches fit in one call");
    report.updates.len()
}

/// Processes by kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Spawns {
    shortstat: u64,
    merge_base: u64,
    ahead_behind: u64,
    total: u64,
}

impl Spawns {
    fn of(argvs: &[Vec<String>]) -> Self {
        let has = |argv: &Vec<String>, needle: &str| argv.iter().any(|a| a.contains(needle));
        Self {
            shortstat: argvs.iter().filter(|a| has(a, "--shortstat")).count() as u64,
            merge_base: argvs.iter().filter(|a| has(a, "merge-base")).count() as u64,
            ahead_behind: argvs.iter().filter(|a| has(a, "ahead-behind")).count() as u64,
            total: argvs.len() as u64,
        }
    }

    fn minus(self, other: Self) -> Self {
        let sub = |a: u64, b: u64| {
            a.checked_sub(b)
                .expect("the call cannot start fewer processes than none")
        };
        Self {
            shortstat: sub(self.shortstat, other.shortstat),
            merge_base: sub(self.merge_base, other.merge_base),
            ahead_behind: sub(self.ahead_behind, other.ahead_behind),
            total: sub(self.total, other.total),
        }
    }
}

fn main() {
    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
    let mut report = Report::new("churn_spawns");
    let temp = tempfile::tempdir().expect("temp dir");
    let stopped = build(&temp.path().join("stopped"));
    let called = build(&temp.path().join("called"));
    let timed = build(&temp.path().join("timed"));
    report.note(format!(
        "fixture: main with one commit and {BRANCHES} branches, each one commit ahead touching its \
         own file; the unrelated advance is an empty commit on main; hermetic HOME, production \
         spawn rate lifted. Counters come from a separate pass on two copies (advance only, \
         advance then call) after one priming call each, the process counts of the call being \
         the difference; timed samples advance main outside the timed span, then time the call"
    ));

    // The counting pass rewrites the global git config, which the ref cache
    // stamps: wait out its racy window after priming, as the graph bench does.
    let settled = std::time::Duration::from_secs(4);
    branch_stats(&stopped);
    let (_, advance_only) = git_process_argvs(|| {
        std::thread::sleep(settled);
        advance(&stopped, 1);
    });
    branch_stats(&called);
    let (_, advance_and_call) = git_process_argvs(|| {
        std::thread::sleep(settled);
        advance(&called, 1);
        branch_stats(&called)
    });
    let spawns = Spawns::of(&advance_and_call).minus(Spawns::of(&advance_only));

    branch_stats(&timed);
    let mut turn = 0usize;
    let cases = vec![Case::new(
        format!("branch_stats after a default-branch advance unrelated to {BRANCHES} branches"),
        || {
            turn += 1;
            advance(&timed, turn);
            let started = std::time::Instant::now();
            let branches = branch_stats(&timed);
            Sample::default()
                .counter("branches", branches as u64)
                .counter("shortstat", spawns.shortstat)
                .counter("merge_base", spawns.merge_base)
                .counter("ahead_behind", spawns.ahead_behind)
                .counter("git_processes", spawns.total)
                .elapsed(started.elapsed())
        },
    )];
    let rows = interleave(cases, report.opts.samples(9), report.opts.warmup(1));
    report.extend(rows);
    report.finish();
}
