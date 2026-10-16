//! `git` processes and time for the two graph-shaped loads that walk history:
//! the commit graph's default page and the Stack view's refresh.
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench graph_spawns`.
//!
//! * `graph default page 5k (S4-04)`: `cmd_get_commit_graph` with the frontend's
//!   default cap (`DEFAULT_MAX_COMMITS`, 5,000) on a 6,000-commit repository.
//!   The command asks the reader for cap + 1 rows (the has_more probe), one
//!   row more than a kept 5,000-row history chunk holds.
//! * `stack refresh 20 branches beyond the window (S4-03)`:
//!   `cmd_get_stack_hierarchy` on a 2,500-commit `main` with 20 branches that
//!   each point at an old `main` commit, 100 to 290 commits from the root and
//!   far outside the 2,000-commit window the command reads. Every one of them
//!   is left parentless by the window and is resolved by its own first-parent
//!   walk down to the previous branch's tip.
//!
//! Counters (deterministic, asserted equal across samples): `git_processes`,
//! counted from git's own trace2 in a separate untimed pass, and the rows or
//! nodes returned, so a change that returns less cannot pass as a speedup.
//! The wall clock is a property of host load; the process count is the gate.
//! The fixtures are written with `git fast-import`, so 6,000 commits cost a
//! moment, not 6,000 spawns. The spawn rate is lifted as the stress tests
//! lift it.

mod common;

use common::fixture::{count_git_processes, git, trust_repo};
use common::{interleave, Case, Report, Sample};
use gitpulse_lib::procguard::LockedSpawn;
use std::io::Write;
use std::path::Path;

const GRAPH_COMMITS: usize = 6_000;
const STACK_COMMITS: usize = 2_500;
const STACK_BRANCHES: usize = 20;
/// The frontend's `DEFAULT_MAX_COMMITS`.
const DEFAULT_PAGE: usize = 5_000;

/// A repository whose `main` is a linear chain of `commits` empty commits,
/// plus `branches` named `b00`... each pointing at the `main` commit
/// `100 + 10 * i` from the root.
fn build_chain(root: &Path, commits: usize, branches: usize) -> String {
    std::fs::create_dir_all(root).expect("create fixture root");
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.name", "Bench"]);
    git(root, &["config", "user.email", "bench@example.com"]);
    let mut stream = String::new();
    for n in 0..commits {
        stream.push_str("commit refs/heads/main\n");
        stream.push_str(&format!("mark :{}\n", n + 1));
        let message = format!("feat: change {n}\n");
        stream.push_str(&format!(
            "committer Bench <bench@example.com> {} +0000\n",
            1_700_000_000 + n
        ));
        stream.push_str(&format!("data {}\n{message}", message.len()));
        if n > 0 {
            stream.push_str(&format!("from :{n}\n"));
        }
        stream.push('\n');
    }
    for i in 0..branches {
        stream.push_str(&format!(
            "reset refs/heads/b{i:02}\nfrom :{}\n\n",
            100 + 10 * i
        ));
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

fn graph_page(path: &str) -> (u64, bool) {
    let payload = tauri::async_runtime::block_on(gitpulse_lib::commands::cmd_get_commit_graph(
        path.to_string(),
        DEFAULT_PAGE,
        None,
        None,
        None,
        None,
    ))
    .unwrap_or_else(|error| panic!("cmd_get_commit_graph failed: {error}"));
    assert_eq!(
        payload.rows.len(),
        DEFAULT_PAGE,
        "the default page came back short, so this run would time a partial answer"
    );
    assert!(payload.has_more, "6,000 commits must report has_more");
    (payload.rows.len() as u64, payload.has_more)
}

fn stack_refresh(path: &str) -> (u64, u64) {
    let payload = tauri::async_runtime::block_on(gitpulse_lib::commands::cmd_get_stack_hierarchy(
        path.to_string(),
    ))
    .unwrap_or_else(|error| panic!("cmd_get_stack_hierarchy failed: {error}"));
    let nodes = payload.nodes.len() as u64;
    let parented = payload
        .nodes
        .iter()
        .filter(|node| node.parent_branch_name.is_some())
        .count() as u64;
    assert_eq!(
        nodes as usize,
        STACK_BRANCHES + 1,
        "the stack lost a branch, so this run would time a partial answer"
    );
    (nodes, parented)
}

fn main() {
    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
    let mut report = Report::new("graph_spawns");
    let temp = tempfile::tempdir().expect("temp dir");
    let graph = build_chain(&temp.path().join("graph"), GRAPH_COMMITS, 0);
    let stack = build_chain(&temp.path().join("stack"), STACK_COMMITS, STACK_BRANCHES);
    let warm = build_chain(&temp.path().join("warm"), STACK_COMMITS, STACK_BRANCHES);
    report.note(format!(
        "fixtures: graph = {GRAPH_COMMITS} linear commits; stack = {STACK_COMMITS} commits on main \
         plus {STACK_BRANCHES} branches at main commits 100..290 (outside the 2,000-commit window); \
         hermetic HOME, production spawn rate lifted. The two cold rows move a tag \
         (`refs/tags/zz-touch`, alternating between two commits) before each sample, outside the \
         timed span, so the ref cache's stamp differs and every timed call is a real read; the \
         warm row reads a repository whose refs have been idle for longer than the cache's racy \
         window, after one priming call"
    ));

    // Moves a tag so the next read cannot be answered from the ref cache.
    let touch = |repo: &str, turn: bool| {
        let rev = if turn { "main~1" } else { "main~2" };
        git(Path::new(repo), &["update-ref", "refs/tags/zz-touch", rev]);
    };

    // Work counters first, in their own untimed pass.
    touch(&graph, true);
    let (_, graph_procs) = count_git_processes(|| graph_page(&graph));
    touch(&stack, true);
    let (_, stack_procs) = count_git_processes(|| stack_refresh(&stack));
    // The counting pass rewrites the global git config, which the ref cache
    // stamps, so the first call inside it is always cold, and nothing is kept
    // until the new config is older than the cache's racy window. The warm
    // figure is therefore the difference between two counted blocks that wait
    // out that window: one call (cold) and two calls (cold, then warm).
    let settled = std::time::Duration::from_secs(4);
    let (_, cold_once) = count_git_processes(|| {
        std::thread::sleep(settled);
        stack_refresh(&warm)
    });
    let (_, cold_then_warm) = count_git_processes(|| {
        std::thread::sleep(settled);
        stack_refresh(&warm);
        stack_refresh(&warm)
    });
    let warm_procs = cold_then_warm
        .checked_sub(cold_once)
        .expect("a second call cannot start fewer processes than none");

    let mut graph_turn = false;
    let mut stack_turn = false;
    let cases = vec![
        Case::new("graph default page 5k (S4-04)", || {
            graph_turn = !graph_turn;
            touch(&graph, graph_turn);
            let started = std::time::Instant::now();
            let (rows, _) = graph_page(&graph);
            Sample::default()
                .counter("rows", rows)
                .counter("git_processes", graph_procs)
                .elapsed(started.elapsed())
        }),
        Case::new(
            "stack refresh 20 branches beyond the window (S4-03)",
            || {
                stack_turn = !stack_turn;
                touch(&stack, stack_turn);
                let started = std::time::Instant::now();
                let (nodes, parented) = stack_refresh(&stack);
                Sample::default()
                    .counter("nodes", nodes)
                    .counter("parented", parented)
                    .counter("git_processes", stack_procs)
                    .elapsed(started.elapsed())
            },
        ),
        Case::new("stack refresh, refs unmoved (warm)", || {
            let (nodes, parented) = stack_refresh(&warm);
            Sample::default()
                .counter("nodes", nodes)
                .counter("parented", parented)
                .counter("git_processes", warm_procs)
        }),
    ];
    let rows = interleave(cases, report.opts.samples(9), report.opts.warmup(1));
    report.extend(rows);
    report.finish();
}
