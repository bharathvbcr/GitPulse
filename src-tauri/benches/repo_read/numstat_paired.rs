//! The numstat limit (F-3), measured as counterbalanced pairs.
//!
//! At 50k tracked files `get_status` used to run `git diff --numstat -z` over
//! every index entry to find churn for the few paths `status` had just named.
//! The shipped read limits it to those paths (and falls back to the whole tree
//! above `MAX_NUMSTAT_PATHSPECS`); `get_status_whole_tree_numstat` is the read it
//! replaced. One repository per dirty count is shared by both arms.
//!
//! Why this is not an `interleave` row pair. Two arms queued one after the
//! other always run in the same order, and the first of a pair pays the cold
//! file-system cache the second reads warm; that alone made the limited arm
//! look slower at every count, including where both arms ran the same code.
//! So each round here starts with one untimed status read of the repository
//! (both arms start warm), then runs the two arms in an order that alternates
//! round by round (limited-whole, whole-limited, ...), so host drift lands on
//! both arms equally. Each round yields a paired difference (limited minus
//! whole), reported with how many rounds the limited arm was faster.
//!
//! Self-validation: at a dirty count above the pathspec bound the shipped read
//! IS the whole-tree read, so those two arms are one code path. They must agree
//! within their own spread on wall time and child CPU; if they do not, the
//! harness is biased, the run is invalid, and it aborts rather than report.

use std::path::Path;
use std::time::Instant;

use gitpulse_lib::engine::git_reader::GitReader;

use super::{git_bytes, ChildCpu};
use crate::common::fixture::{build_repo, count_git_processes, dirty_files, Spec};
use crate::common::Report;

/// Rounds run and discarded first, so the first timed pair is not the first
/// read of anything.
const WARMUP_ROUNDS: usize = 2;
/// Dirty counts measured; the last is above the pathspec bound (512).
const DIRTY_COUNTS: [usize; 7] = [1, 50, 100, 200, 300, 500, 2_000];
/// A dirty count above the pathspec bound: the null check runs here.
const NULL_CHECK_DIRTY: usize = 2_000;

#[derive(Default)]
struct Arm {
    wall_ms: Vec<f64>,
    cpu_ms: Vec<f64>,
}

fn sorted(values: &[f64]) -> Vec<f64> {
    let mut copy = values.to_vec();
    copy.sort_by(|a, b| a.total_cmp(b));
    copy
}

fn median(values: &[f64]) -> f64 {
    sorted(values)[values.len() / 2]
}

/// The inter-quartile range: how far one arm's own samples spread.
fn iqr(values: &[f64]) -> f64 {
    let s = sorted(values);
    s[s.len() * 3 / 4] - s[s.len() / 4]
}

fn range(values: &[f64]) -> (f64, f64) {
    let s = sorted(values);
    (s[0], s[s.len() - 1])
}

/// Runs `warm`, then both arms in alternating order, `rounds` times after the
/// warm-up rounds. Returns `(limited, whole)`, sample `i` of each from round `i`.
fn paired(rounds: usize, warm: &dyn Fn(), limited: &dyn Fn(), whole: &dyn Fn()) -> (Arm, Arm) {
    let (mut l, mut w) = (Arm::default(), Arm::default());
    for round in 0..WARMUP_ROUNDS + rounds {
        warm();
        let limited_first = round % 2 == 0;
        for limited_turn in [limited_first, !limited_first] {
            let cpu = ChildCpu::start();
            let started = Instant::now();
            if limited_turn {
                limited();
            } else {
                whole();
            }
            let wall = started.elapsed().as_secs_f64() * 1_000.0;
            let cpu = cpu.spent_ms();
            if round >= WARMUP_ROUNDS {
                let arm = if limited_turn { &mut l } else { &mut w };
                arm.wall_ms.push(wall);
                arm.cpu_ms.push(cpu);
            }
        }
    }
    (l, w)
}

/// One report line for a pair of arms: per arm median [min..max], then the
/// paired difference's median and in how many rounds the limited arm was
/// smaller.
fn describe(label: &str, children: (u64, u64), l: &Arm, w: &Arm) -> String {
    let column = |name: &str, l: &[f64], w: &[f64]| {
        let diff: Vec<f64> = l.iter().zip(w).map(|(a, b)| a - b).collect();
        let faster = diff.iter().filter(|d| **d < 0.0).count();
        let (lmin, lmax) = range(l);
        let (wmin, wmax) = range(w);
        format!(
            "{name}: limited {:.0} [{lmin:.0}..{lmax:.0}] | whole {:.0} [{wmin:.0}..{wmax:.0}] | \
             paired limited-whole median {:+.0}, limited smaller in {faster}/{}",
            median(l),
            median(w),
            median(&diff),
            diff.len()
        )
    };
    format!(
        "{label} (n={}, git children {}/{}) ms -- {}; {}",
        l.wall_ms.len(),
        children.0,
        children.1,
        column("wall", &l.wall_ms, &w.wall_ms),
        column("child cpu", &l.cpu_ms, &w.cpu_ms),
    )
}

/// The null check: arms that run the same code agree within their own spread.
fn null_check_agrees(l: &Arm, w: &Arm) -> Result<(), String> {
    for (name, l, w) in [
        ("wall", &l.wall_ms, &w.wall_ms),
        ("child cpu", &l.cpu_ms, &w.cpu_ms),
    ] {
        let gap = (median(l) - median(w)).abs();
        let spread = iqr(l).min(iqr(w));
        if gap > spread {
            return Err(format!(
                "{name}: medians differ by {gap:.1} ms, more than the smaller arm's IQR {spread:.1} ms"
            ));
        }
    }
    Ok(())
}

pub fn run(report: &mut Report, root: &Path, rounds: usize) {
    let spec = Spec {
        files: 50_000,
        dirs: 500,
        extra_commits: 1,
        touch: 5,
        linked_worktrees: 0,
    };
    report.note(format!(
        "numstat limit, counterbalanced pairs: 50k-file repository (500 dirs, 24-line files, 2 \
         commits), one repository per dirty count shared by both arms; each round = one untimed \
         status read, then both arms in alternating order; {WARMUP_ROUNDS} warm-up rounds \
         discarded, n={rounds}; ms are wall and child CPU (user+system, getrusage \
         RUSAGE_CHILDREN deltas)"
    ));
    let churn = |rows: &[gitpulse_lib::engine::git_reader::FileStatus]| {
        rows.iter()
            .map(|row| (row.additions + row.deletions) as u64)
            .sum::<u64>()
    };
    for dirty in DIRTY_COUNTS {
        let repo = build_repo(&root.join(format!("wide-{dirty}")), &spec);
        dirty_files(&repo, &spec, dirty);
        let path = repo.path().to_string();
        let shipped = || {
            std::hint::black_box(GitReader::get_status(&path).expect("limited status"));
        };
        let oracle = || {
            std::hint::black_box(
                GitReader::get_status_whole_tree_numstat(&path).expect("whole-tree status"),
            );
        };
        let warm = || {
            std::hint::black_box(git_bytes(
                &repo.main,
                &["status", "--porcelain=v1", "-z", "-uall"],
            ));
        };
        // The arms must read the same answer, or this is not a comparison.
        let (limited_rows, whole_rows) = (
            GitReader::get_status(&path).expect("limited status"),
            GitReader::get_status_whole_tree_numstat(&path).expect("whole-tree status"),
        );
        assert_eq!(
            format!("{limited_rows:?}"),
            format!("{whole_rows:?}"),
            "the arms disagree on {dirty} dirty files"
        );
        assert_eq!(churn(&limited_rows), churn(&whole_rows));
        let (_, limited_children) = count_git_processes(shipped);
        let (_, whole_children) = count_git_processes(oracle);
        let (l, w) = paired(rounds, &warm, &shipped, &oracle);
        report.note(describe(
            &format!("get_status {dirty} dirty"),
            (limited_children, whole_children),
            &l,
            &w,
        ));
        if dirty == NULL_CHECK_DIRTY {
            if let Err(why) = null_check_agrees(&l, &w) {
                report.note(format!("NULL CHECK FAILED at {dirty} dirty: {why}"));
                panic!("the identical-code arms disagree ({why}): the harness is biased, the run is invalid");
            }
            report.note(format!(
                "null check passed at {dirty} dirty: identical-code arms agree within their spread"
            ));
            raw_arms(report, &repo.main, dirty, rounds, &warm);
        }
    }
}

/// The raw numstat alone, whole tree against one literal pathspec per dirty
/// file: what the limit would cost with no bound.
fn raw_arms(report: &mut Report, dir: &Path, dirty: usize, rounds: usize, warm: &dyn Fn()) {
    let names = git_bytes(dir, &["diff", "--name-only", "-z"]);
    let specs: Vec<String> = names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| format!(":(literal){}", String::from_utf8_lossy(name)))
        .collect();
    assert_eq!(specs.len(), dirty, "every dirty file is named");
    let mut limited_args = vec!["diff", "--numstat", "-z", "--"];
    limited_args.extend(specs.iter().map(String::as_str));
    let whole_args = ["diff", "--numstat", "-z"];
    assert_eq!(
        git_bytes(dir, &whole_args),
        git_bytes(dir, &limited_args),
        "the raw arms read the same numstat"
    );
    let limited = || {
        std::hint::black_box(git_bytes(dir, &limited_args));
    };
    let whole = || {
        std::hint::black_box(git_bytes(dir, &whole_args));
    };
    let (l, w) = paired(rounds, warm, &limited, &whole);
    report.note(describe(
        &format!("raw git diff --numstat -z, {dirty} literal pathspecs"),
        (1, 1),
        &l,
        &w,
    ));
}
