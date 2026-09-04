//! The graph must not draw a lane it cannot name.
//!
//! Reproduces the topology that exposed the defect: an agent harness (cmux)
//! writes one `refs/cmux/last-turn/<sha>` ref per turn, each pointing at a
//! stash-shaped pair — an `On main:` merge plus its `index on main:` second
//! parent — and every pair forks from the SAME base commit. The history walk
//! asked git for `--all`, so all of them became graph tips; the decoration
//! listing read only heads/remotes/tags, so none of them could be labelled.
//!
//! The result on a real repository (MarkDev, Sep 2026): 18 such refs turned
//! 65 commits of straight history into 101 rows and 35 lanes, 34 of them
//! anonymous rails descending 30-odd rows to the shared base. At the default
//! graph width (440px, ~14 lanes) 21 of those rows drew their node off-canvas
//! and appeared to have no node at all.

use std::process::Command;

use gitpulse_lib::analyzer::CommitFilter;
use gitpulse_lib::commands::assemble_commit_graph;
use gitpulse_lib::engine::GitReader;
use gitpulse_lib::graph::{
    hidden_ref_warning, list_ref_decorations, probe_hidden_history, RefKind, RefScope,
};
use tempfile::TempDir;

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// `checkpoints` stash-shaped pairs, all forked from the same base commit,
/// each reachable ONLY from a `refs/cmux/last-turn/*` ref.
fn repo_with_harness_refs(checkpoints: usize) -> (TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();
    git(path, &["init", "--initial-branch=main", "."]);
    for i in 0..4 {
        std::fs::write(path.join("a.txt"), format!("line {i}\n")).unwrap();
        git(path, &["add", "a.txt"]);
        git(path, &["commit", "-m", &format!("main commit {i}")]);
    }
    // Fork every checkpoint from the same mid-history commit, exactly as a
    // stash does: the base stays on the mainline and the pair hangs off it.
    let base = git(path, &["rev-parse", "HEAD~2"]);
    for i in 0..checkpoints {
        let index = git(
            path,
            &[
                "commit-tree",
                &format!("{base}^{{tree}}"),
                "-p",
                &base,
                "-m",
                &format!("index on main: checkpoint {i}"),
            ],
        );
        let turn = git(
            path,
            &[
                "commit-tree",
                &format!("{base}^{{tree}}"),
                "-p",
                &base,
                "-p",
                &index,
                "-m",
                &format!("On main: cmux last turn baseline {i}"),
            ],
        );
        git(
            path,
            &["update-ref", &format!("refs/cmux/last-turn/{turn}"), &turn],
        );
    }
    let repo = path
        .canonicalize()
        .expect("canonical repo path")
        .to_string_lossy()
        .into_owned();
    (dir, repo)
}

fn solve(repo: &str, scope: RefScope) -> gitpulse_lib::commands::CommitGraphPayload {
    let commits =
        GitReader::read_commit_history_paged(repo, 0, 500, None, None, scope).expect("history");
    let refs = list_ref_decorations(repo, scope).expect("refs").decorations;
    let head = GitReader::head_id(repo).ok();
    assemble_commit_graph(
        commits,
        500,
        &CommitFilter::default(),
        refs,
        Some("main"),
        head,
        Vec::new(),
    )
}

fn width(payload: &gitpulse_lib::commands::CommitGraphPayload) -> u32 {
    payload.rows.iter().map(|r| r.lane).max().unwrap_or(0) + 1
}

/// The regression. Before the fix the walk was `--all`, so eight harness
/// checkpoints turned four commits of straight history into twenty rows and
/// seventeen lanes.
#[test]
fn harness_namespaces_do_not_open_lanes_under_the_named_scope() {
    let (_dir, repo) = repo_with_harness_refs(8);

    let named = solve(&repo, RefScope::Named);
    assert_eq!(
        named.rows.len(),
        4,
        "named scope drew {} rows; only the four commits on main are named",
        named.rows.len()
    );
    assert_eq!(
        width(&named),
        1,
        "a repository whose only branch is linear must solve to one lane"
    );
    assert!(
        named
            .rows
            .iter()
            .all(|r| !r.summary.starts_with("On main:") && !r.summary.contains("index on main:")),
        "a checkpoint commit reached the named-scope graph"
    );
}

/// The invariant the fix establishes: every lane the graph opens traces back
/// to a ref the SAME scope labels. Checked in both scopes, because a scope
/// that walks more must also label more.
#[test]
fn every_scope_labels_every_ref_it_walks() {
    let (_dir, repo) = repo_with_harness_refs(3);

    for scope in [RefScope::Named, RefScope::All] {
        let payload = solve(&repo, scope);
        let rows: std::collections::HashSet<&str> =
            payload.rows.iter().map(|r| r.id.as_str()).collect();
        // Every tip git was asked to walk resolves to a row, and every row a
        // ref points at carries that ref's label.
        let tips = Command::new("git")
            .args(["rev-list", "--no-walk"])
            .args(gitpulse_lib::graph::history_rev_args(scope))
            .current_dir(&repo)
            .output()
            .expect("rev-list tips");
        let labelled: std::collections::HashSet<&str> =
            payload.refs.iter().map(|r| r.commit_id.as_str()).collect();
        let head = payload.head_id.clone().unwrap_or_default();
        for tip in String::from_utf8_lossy(&tips.stdout).lines() {
            let tip = tip.trim();
            if tip.is_empty() || !rows.contains(tip) {
                continue;
            }
            assert!(
                labelled.contains(tip) || tip == head,
                "{scope:?}: tip {tip} opens a lane with no ref decoration to name it"
            );
        }
    }
}

/// The escape hatch still works, and the refs it walks are named rather than
/// left anonymous — otherwise it would just restore the original defect.
#[test]
fn the_all_scope_walks_and_labels_custom_namespaces() {
    let (_dir, repo) = repo_with_harness_refs(3);

    let all = solve(&repo, RefScope::All);
    assert_eq!(
        all.rows.len(),
        10,
        "all scope must reach the four main commits and all six checkpoint commits"
    );
    assert!(
        width(&all) > 1,
        "the checkpoints fork from one base, so they must occupy their own lanes"
    );
    let named_checkpoints = all
        .refs
        .iter()
        .filter(|r| r.kind == RefKind::Other && r.name.starts_with("cmux/last-turn/"))
        .count();
    assert_eq!(
        named_checkpoints,
        3,
        "every walked checkpoint ref must carry a label; got {:?}",
        all.refs.iter().map(|r| &r.name).collect::<Vec<_>>()
    );
}

/// History the named scope leaves out is reported, never silently absent:
/// "GitPulse is not drawing this" must be distinguishable from "this does not
/// exist".
#[test]
fn the_named_scope_reports_the_history_it_leaves_out() {
    let (_dir, repo) = repo_with_harness_refs(4);

    let hidden = probe_hidden_history(&repo).expect("hidden probe");
    assert_eq!(hidden.commits, 8, "four checkpoints hide two commits each");
    assert!(!hidden.capped, "eight commits are far under the probe cap");
    assert_eq!(hidden.namespaces.get("refs/cmux"), Some(&4));

    let warning = hidden_ref_warning(&hidden).expect("a warning naming the namespace");
    assert!(warning.contains("8 commit(s)"), "{warning}");
    assert!(warning.contains("refs/cmux (4 refs)"), "{warning}");
}

/// The probe must stay quiet on an ordinary repository: a breadcrumb that
/// fires for every load is noise, and noise is how a real one gets ignored.
#[test]
fn an_ordinary_repository_hides_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();
    git(path, &["init", "--initial-branch=main", "."]);
    std::fs::write(path.join("a.txt"), "hello\n").unwrap();
    git(path, &["add", "a.txt"]);
    git(path, &["commit", "-m", "first"]);
    git(path, &["branch", "feature"]);
    git(path, &["tag", "v1.0"]);
    let repo = path
        .canonicalize()
        .expect("canonical repo path")
        .to_string_lossy()
        .into_owned();

    let hidden = probe_hidden_history(&repo).expect("hidden probe");
    assert_eq!(
        hidden,
        Default::default(),
        "nothing is outside the named set"
    );
    assert_eq!(hidden_ref_warning(&hidden), None);
}

/// A detached HEAD is labelled by the decoration listing, so it must also be
/// walked — `--branches --remotes --tags` alone would drop the commit the
/// user is sitting on.
#[test]
fn a_detached_head_is_still_walked_under_the_named_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();
    git(path, &["init", "--initial-branch=main", "."]);
    for i in 0..2 {
        std::fs::write(path.join("a.txt"), format!("line {i}\n")).unwrap();
        git(path, &["add", "a.txt"]);
        git(path, &["commit", "-m", &format!("commit {i}")]);
    }
    // A commit on no branch at all, with HEAD detached onto it.
    let base = git(path, &["rev-parse", "HEAD"]);
    let loose = git(
        path,
        &[
            "commit-tree",
            &format!("{base}^{{tree}}"),
            "-p",
            &base,
            "-m",
            "detached work",
        ],
    );
    git(path, &["checkout", "--detach", &loose]);
    let repo = path
        .canonicalize()
        .expect("canonical repo path")
        .to_string_lossy()
        .into_owned();

    let named = solve(&repo, RefScope::Named);
    assert!(
        named.rows.iter().any(|r| r.id == loose),
        "the detached HEAD commit must be drawn"
    );
    assert!(
        named
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Head && r.commit_id == loose),
        "and labelled"
    );
}

/// Contributor metrics are claims about what people did. A `--all` walk
/// credited them with machine-written checkpoint commits.
#[test]
fn pulse_metrics_ignore_machine_written_namespaces() {
    let (_dir, repo) = repo_with_harness_refs(5);

    let report = GitReader::pulse_report(&repo, Some(500)).expect("pulse report");
    assert_eq!(
        report.commits.len(),
        4,
        "pulse counted {} commits; only the four on main were authored by a person",
        report.commits.len()
    );
    assert!(
        report
            .commits
            .iter()
            .all(|c| !c.summary.starts_with("On main:")),
        "a checkpoint commit was counted as work"
    );
}
