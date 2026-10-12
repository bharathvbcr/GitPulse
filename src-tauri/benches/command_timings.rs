//! Command timings on a generated 18-worktree repository: the insights
//! snapshot, `list_worktrees`, language statistics, docs refresh, `catch_up`
//! and commit details.
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench command_timings`.
//!
//! These are the commands the 2026-10-12 lag audit names (insights, listing,
//! language stats, docs refresh, catch-up, commit details). Before this bench
//! the only evidence was the slow-command log from one developer's machine;
//! this gives each one a repeatable number on a fixed fixture, with the work it
//! did beside the time:
//!
//! * the fixture: 600 tracked files in 30 directories, 61 commits, and 17
//!   linked worktrees (18 in all), each linked worktree with one dirty file;
//!   markdown files for the docs vault; 60 transcript files for `catch_up`;
//! * counters: worktrees listed, files scanned, notes indexed, ledger events
//!   recorded, and (insights, listing) the git processes started, counted from
//!   git's own trace2 in a separate untimed pass;
//! * `catch_up` has two rows. `steady` is the re-open cost: the watermark makes
//!   every transcript a skip, which is what a user pays on each launch. `cold`
//!   is the first pass over a fresh repository and transcript set, rebuilt for
//!   every sample outside the timed span.
//!
//! Every response is checked: an `ok: false` facet in the insights snapshot,
//! a truncated language scan or a deadline-expired snapshot aborts the run
//! instead of being timed as if it were the work.

mod common;

use common::fixture::{build_repo, count_git_processes, Repo, Spec};
use common::{interleave, require_no_failed_facets, Case, Report, Sample};
use gitpulse_lib::engine::git_reader::GitReader;
use gitpulse_lib::{docs, ingest, insights};
use serde_json::json;
use std::path::Path;

const SPEC: Spec = Spec {
    files: 600,
    dirs: 30,
    extra_commits: 60,
    touch: 5,
    linked_worktrees: 17,
};

/// `count` transcripts, each with `edits` assistant tool_use lines naming a
/// file inside `repo`, in the shape `tests/catch_up_integration.rs` uses.
fn write_transcripts(root: &Path, repo: &str, count: usize, edits: usize) {
    let slug = root.join("project-slug");
    std::fs::create_dir_all(&slug).expect("transcript dir");
    for session in 0..count {
        let mut text = String::new();
        for edit in 0..edits {
            let line = json!({
                "type": "assistant",
                "sessionId": format!("S{session}"),
                "timestamp": format!("2026-09-01T12:{:02}:{:02}.000Z", session % 60, edit % 60),
                "version": "2.1.241",
                "cwd": repo,
                "gitBranch": "main",
                "message": { "content": [
                    { "type": "tool_use", "name": "Edit",
                      "input": { "file_path": format!("{repo}/d{:03}/f{:06}.rs", edit % 30, edit * 8) } }
                ]}
            });
            text.push_str(&line.to_string());
            text.push('\n');
        }
        std::fs::write(slug.join(format!("S{session}.jsonl")), text).expect("write transcript");
    }
}

fn catch_up_sample(root: &Path, repo: &str) -> (Sample, std::time::Duration) {
    std::env::set_var("GITPULSE_TRANSCRIPT_ROOT", root);
    let started = std::time::Instant::now();
    let out = ingest::catch_up(repo);
    let elapsed = started.elapsed();
    assert!(
        !out.truncated && out.error.is_empty(),
        "catch_up degraded ({}), so this run would time a partial pass",
        out.error
    );
    (
        Sample::default()
            .counter("recorded", out.recorded as u64)
            .counter("transcripts", out.transcripts as u64)
            .counter("reflog_entries", out.reflog_entries as u64)
            .counter("skipped_lines", out.skipped_lines as u64),
        elapsed,
    )
}

fn head(repo: &Repo) -> String {
    common::fixture::git(&repo.main, &["rev-parse", "HEAD"])
        .trim()
        .to_string()
}

fn main() {
    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
    let mut report = Report::new("command_timings");
    let temp = tempfile::tempdir().expect("temp dir");
    let repo = build_repo(&temp.path().join("fixture"), &SPEC);
    let path = repo.path().to_string();
    let commit = head(&repo);
    report.note(format!(
        "fixture: {} files, {} commits, {} worktrees ({} dirty), hermetic HOME, production spawn rate lifted",
        SPEC.files,
        SPEC.extra_commits + 1,
        SPEC.linked_worktrees + 1,
        SPEC.linked_worktrees
    ));

    // Transcripts for the steady catch_up row; one cold pass settles the
    // watermark so the timed passes are the re-open cost.
    let steady_root = temp.path().join("transcripts-steady");
    write_transcripts(&steady_root, &path, 60, 8);

    let snapshot = |path: &str| {
        let snapshot = insights::snapshot(path);
        assert!(
            !snapshot.deadline_expired,
            "the insights snapshot hit its deadline: this run would time a partial snapshot"
        );
        let value = serde_json::to_value(&snapshot).expect("snapshot serializes");
        let facets_ok = require_no_failed_facets("insights::snapshot", &value);
        (snapshot, facets_ok)
    };
    let list = |path: &str| {
        gitpulse_lib::engine::worktree::list_worktrees(path)
            .unwrap_or_else(|error| panic!("list_worktrees failed: {error}"))
    };

    // Work counters (git processes) in their own untimed pass.
    let (_, snapshot_procs) = count_git_processes(|| snapshot(&path));
    let (_, list_procs) = count_git_processes(|| list(&path));
    let (_, lang_procs) = count_git_processes(|| GitReader::get_repo_language_stats(&path));
    let (_, details_procs) = count_git_processes(|| GitReader::get_commit_details(&path, &commit));

    // Settle catch_up's watermark (first pass over the transcripts).
    let (first, _) = catch_up_sample(&steady_root, &path);
    report.note(format!(
        "catch_up first pass recorded {} events from {} transcripts (the timed `steady` rows start after it)",
        first.counters["recorded"], first.counters["transcripts"]
    ));

    let cases = vec![
        Case::new("insights snapshot", || {
            let (snap, facets_ok) = snapshot(&path);
            std::hint::black_box(&snap);
            Sample::default()
                .counter("worktrees", snap.worktrees.count as u64)
                .counter("facets_ok", facets_ok)
                .counter("git_processes", snapshot_procs)
        }),
        Case::new("list_worktrees", || {
            let worktrees = list(&path);
            std::hint::black_box(&worktrees);
            Sample::default()
                .counter("worktrees", worktrees.len() as u64)
                .counter("git_processes", list_procs)
        }),
        Case::new("language stats", || {
            let report = GitReader::get_repo_language_stats(&path)
                .unwrap_or_else(|error| panic!("language stats failed: {error}"));
            assert!(
                !report.truncated,
                "the language scan was truncated, so this run would time a partial scan"
            );
            std::hint::black_box(&report);
            Sample::default()
                .counter("scanned_files", report.scanned_files as u64)
                .counter("candidate_files", report.candidate_files as u64)
                .counter("languages", report.stats.len() as u64)
                .counter("git_processes", lang_procs)
        }),
        Case::new("docs refresh", || {
            let status = docs::refresh(&path)
                .unwrap_or_else(|error| panic!("docs refresh failed: {error}"));
            assert!(
                !status.truncated && status.skipped_unreadable == 0,
                "the docs vault was partial: {status:?}"
            );
            Sample::default().counter("notes", u64::from(status.note_count))
        }),
        Case::new("catch_up steady", || {
            let (sample, elapsed) = catch_up_sample(&steady_root, &path);
            sample.elapsed(elapsed)
        }),
        Case::new("commit details", || {
            let details = GitReader::get_commit_details(&path, &commit)
                .unwrap_or_else(|error| panic!("commit details failed: {error}"));
            std::hint::black_box(&details);
            Sample::default()
                .counter("parents", details.parent_ids.len() as u64)
                .counter("git_processes", details_procs)
        }),
    ];
    let mut rows = interleave(cases, report.opts.samples(15), report.opts.warmup(2));

    // The cold pass: a fresh repository and transcript set per sample, built
    // outside the timed span.
    let cold = Case::new("catch_up cold", || {
        let dir = tempfile::tempdir().expect("temp dir");
        let small = Spec { files: 50, dirs: 5, extra_commits: 4, touch: 2, linked_worktrees: 0 };
        let fresh = build_repo(&dir.path().join("cold"), &small);
        let root = dir.path().join("transcripts");
        write_transcripts(&root, fresh.path(), 60, 8);
        let (sample, elapsed) = catch_up_sample(&root, fresh.path());
        sample.elapsed(elapsed)
    });
    rows.extend(interleave(vec![cold], report.opts.samples(5), 0));

    report.extend(rows);
    report.finish();
}
