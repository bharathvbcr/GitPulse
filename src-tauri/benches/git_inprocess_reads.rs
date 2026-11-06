//! Which git processes GitPulse starts, how often, and what each costs; and
//! (Phase 2) the in-process answers that replace the hot ones.
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench git_inprocess_reads`.
//!
//! # The spawn profile (Phase 1)
//!
//! A fixture repository (600 files, 61 commits, 17 linked worktrees, one dirty
//! file each, plus 120 branches and 30 tags at different commits) is driven
//! through the entry points a refresh of the app's views reaches, and git's own
//! trace2 logs every process they start (`GIT_TRACE2_EVENT` through the
//! hermetic global config: nothing GitPulse spawns can escape it, which a gate
//! counter on one thread could). Each process is reduced to an argv *shape*
//! (subcommand and flags; refs, paths and oids dropped) and counted, with the
//! wall time trace2 records between the process's own start and exit.
//!
//! * **idle**: one warm-up refresh (not counted; it also pays for the ref
//!   cache's stamp, which counting invalidates), then `IDLE_CYCLES` refreshes
//!   of an unchanged repository.
//! * **active**: the same refresh after every cycle changed the repository the
//!   way a working agent does: one file rewritten in the main checkout, one new
//!   commit, one branch moved.
//!
//! One refresh is: `get_status`, `current_branch`, `head_id`, `list_branches`,
//! `list_tags`, `branch_stats`, `read_commit_history_paged`, `list_worktrees`
//! (Full and Listing) and `insights::snapshot`. The app's refresh cadence is
//! event-driven (watcher, focus, UI), not a timer, so "per minute" figures
//! assume `CYCLES_PER_MINUTE` refreshes; the per-refresh figure is the
//! measurement.
//!
//! Wall time is the process's own, so it leaves out fork/exec and pipe drain
//! on GitPulse's side: a floor on what the caller waited.

mod common;

use common::fixture::{argv_shape, build_repo, git, git_process_profile, GitProc, Repo, Spec};
use common::Report;
use gitpulse_lib::engine::git_reader::GitReader;
use gitpulse_lib::engine::worktree::ScanDepth;
use gitpulse_lib::graph::RefScope;
use gitpulse_lib::insights;
use std::collections::BTreeMap;

const SPEC: Spec = Spec {
    files: 600,
    dirs: 30,
    extra_commits: 60,
    touch: 5,
    linked_worktrees: 17,
};

const BRANCHES: usize = 120;
const TAGS: usize = 30;
/// Counted refreshes per scenario.
const CYCLES: usize = 12;
/// Refreshes per simulated minute (one every 5 s): an assumption, see above.
const CYCLES_PER_MINUTE: f64 = 12.0;

/// One refresh of the views. Every error aborts: a refresh that failed would
/// be profiled as if it were the work.
fn refresh(path: &str) {
    GitReader::get_status(path).expect("get_status");
    GitReader::current_branch(path).expect("current_branch");
    GitReader::head_id(path).expect("head_id");
    GitReader::list_branches(path).expect("list_branches");
    GitReader::list_tags(path).expect("list_tags");
    GitReader::branch_stats(path).expect("branch_stats");
    GitReader::read_commit_history_paged(path, 0, 200, None, None, RefScope::All)
        .expect("read_commit_history_paged");
    gitpulse_lib::engine::worktree::list_worktrees(path).expect("list_worktrees full");
    gitpulse_lib::engine::worktree::list_worktrees_scanned(path, ScanDepth::Listing)
        .expect("list_worktrees listing");
    let snapshot = insights::snapshot(path);
    assert!(
        !snapshot.deadline_expired,
        "the insights snapshot hit its deadline"
    );
}

/// What a working agent does between two refreshes.
fn churn(repo: &Repo, n: usize) {
    std::fs::write(
        repo.main.join("d000/f000000.rs"),
        format!("// active edit {n}\n"),
    )
    .expect("edit a file");
    git(&repo.main, &["add", "-A"]);
    git(
        &repo.main,
        &["commit", "-q", "-m", &format!("feat: active {n}")],
    );
    git(&repo.main, &["update-ref", "refs/heads/bench/b000", "HEAD"]);
}

#[derive(Default)]
struct Tally {
    count: u64,
    wall_ns: u64,
}

fn tally(procs: &[GitProc]) -> BTreeMap<String, Tally> {
    let mut by_shape: BTreeMap<String, Tally> = BTreeMap::new();
    for proc in procs.iter().filter(|p| p.top_level) {
        let entry = by_shape.entry(argv_shape(&proc.argv)).or_default();
        entry.count += 1;
        entry.wall_ns += proc.wall_ns;
    }
    by_shape
}

fn render(title: &str, procs: &[GitProc], report: &mut Report) {
    let by_shape = tally(procs);
    let total: u64 = by_shape.values().map(|t| t.count).sum();
    let total_ns: u64 = by_shape.values().map(|t| t.wall_ns).sum();
    let nested = procs.iter().filter(|p| !p.top_level).count();
    let per_refresh = total as f64 / CYCLES as f64;
    let mut lines = vec![format!(
        "{title}: {total} processes over {CYCLES} refreshes ({per_refresh:.1}/refresh, {:.0}/min at {CYCLES_PER_MINUTE}/min), {:.1} ms git wall total, {nested} nested (not counted)",
        per_refresh * CYCLES_PER_MINUTE,
        total_ns as f64 / 1e6,
    )];
    let mut rows: Vec<(&String, &Tally)> = by_shape.iter().collect();
    rows.sort_by(|a, b| b.1.count.cmp(&a.1.count).then_with(|| a.0.cmp(b.0)));
    lines.push("  by count:".to_string());
    for (shape, t) in rows.iter().take(5) {
        lines.push(format!(
            "    {:>4} x {:>7.1} ms total ({:>6.2} ms each)  {shape}",
            t.count,
            t.wall_ns as f64 / 1e6,
            t.wall_ns as f64 / 1e6 / t.count as f64
        ));
    }
    rows.sort_by(|a, b| b.1.wall_ns.cmp(&a.1.wall_ns).then_with(|| a.0.cmp(b.0)));
    lines.push("  by total wall time:".to_string());
    for (shape, t) in rows.iter().take(5) {
        lines.push(format!(
            "    {:>4} x {:>7.1} ms total ({:>6.2} ms each)  {shape}",
            t.count,
            t.wall_ns as f64 / 1e6,
            t.wall_ns as f64 / 1e6 / t.count as f64
        ));
    }
    // What Phase 2 could replace (ref, HEAD and graph reads) against the whole.
    const REF_READS: [&str; 7] = [
        "symbolic-ref",
        "rev-parse",
        "for-each-ref",
        "merge-base",
        "rev-list",
        "show-ref",
        "remote",
    ];
    let (ref_count, ref_ns) = by_shape
        .iter()
        .filter(|(shape, _)| {
            REF_READS
                .iter()
                .any(|sub| shape.split(' ').next() == Some(*sub))
        })
        .fold((0u64, 0u64), |(c, n), (_, t)| (c + t.count, n + t.wall_ns));
    lines.push(format!(
        "  ref/HEAD/graph-read shapes ({}): {ref_count} processes = {:.1}% of count, {:.1} ms = {:.1}% of wall",
        REF_READS.join(", "),
        100.0 * ref_count as f64 / total.max(1) as f64,
        ref_ns as f64 / 1e6,
        100.0 * ref_ns as f64 / total_ns.max(1) as f64,
    ));
    for line in lines {
        println!("{line}");
        report.note(line);
    }
}

fn main() {
    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
    let mut report = Report::new("git_inprocess_reads");
    let temp = tempfile::tempdir().expect("temp dir");

    let preflight = gitpulse_lib::engine::git_cli::git_preflight();
    assert_eq!(
        preflight.status,
        gitpulse_lib::engine::git_cli::GitPreflightStatus::Ok,
        "git did not pass its preflight ({:?})",
        preflight.message
    );

    let repo = build_repo(&temp.path().join("fixture"), &SPEC);
    let path = repo.path().to_string();
    let tips: Vec<String> = git(&repo.main, &["rev-list", "HEAD"])
        .lines()
        .map(str::to_string)
        .collect();
    let mut refs = String::new();
    for n in 0..BRANCHES {
        refs.push_str(&format!(
            "create refs/heads/bench/b{n:03} {}\n",
            tips[n % tips.len()]
        ));
    }
    for n in 0..TAGS {
        refs.push_str(&format!(
            "create refs/tags/t{n:03} {}\n",
            tips[(n * 3) % tips.len()]
        ));
    }
    update_refs(&repo.main, &refs);
    report.note(format!(
        "fixture: {} files, {} commits, {} worktrees, {BRANCHES} branches, {TAGS} tags; {CYCLES} refreshes per scenario",
        SPEC.files,
        SPEC.extra_commits + 1,
        SPEC.linked_worktrees + 1
    ));
    // The ref cache keeps no answer read inside its racy window of the last
    // ref write: let the fixture's writes age out.
    std::thread::sleep(std::time::Duration::from_secs(4));

    let (_, idle) = git_process_profile(
        || refresh(&path),
        || {
            for _ in 0..CYCLES {
                refresh(&path);
            }
        },
    );
    assert!(
        idle.iter()
            .filter(|p| p.top_level)
            .all(|p| p.argv.iter().any(|a| a == "-c")),
        "an idle process has no `-c`: the active scenario's fixture filter would misjudge it"
    );
    render("idle", &idle, &mut report);

    let (_, active) = git_process_profile(
        || refresh(&path),
        || {
            for n in 0..CYCLES {
                churn(&repo, n);
                refresh(&path);
            }
        },
    );
    // `churn`'s own `git add/commit/update-ref` are fixture work, not the
    // app's. The app puts `-c key=value` pairs before every subcommand and the
    // fixture helper never does, so that is what tells them apart; the idle
    // scenario has no fixture processes and must lose none.
    let (active, fixture): (Vec<GitProc>, Vec<GitProc>) = active
        .into_iter()
        .partition(|p| p.argv.iter().any(|a| a == "-c"));
    report.note(format!(
        "active: {} fixture processes dropped from the tally",
        fixture.len()
    ));
    render("active", &active, &mut report);

    report.finish();
}

fn update_refs(repo: &std::path::Path, commands: &str) {
    use gitpulse_lib::procguard::LockedSpawn;
    use std::io::Write;
    let mut child = std::process::Command::new("git")
        .args(["update-ref", "--stdin"])
        .current_dir(repo)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn_locked()
        .expect("spawn git update-ref");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(commands.as_bytes())
        .expect("write the ref commands");
    let out = child.wait_with_output().expect("wait for update-ref");
    assert!(
        out.status.success(),
        "update-ref failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
