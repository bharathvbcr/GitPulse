//! The graph command as the app calls it (`cmd_get_commit_graph`, plus the
//! JSON the IPC layer would write), with and without a revision token.
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench graph_hit_path`
//! (`-- --smoke` for a quick pass). Start it only on a quiet host: the report
//! prints the load at the start and the end, and a run that ends loaded is
//! contaminated.
//!
//! # What is compared
//!
//! * `old` is the command with no token, which is the path every reload took
//!   before the token existed. It is the same code plus one stamp read and one
//!   token hash, so it slightly overstates the old path; the merge-base build
//!   cannot be called from here (its command has no token argument).
//! * `new-hit` is the command with the token the previous read issued, and the
//!   repository unmoved: the `unchanged` answer.
//! * `new-stale` is the command with a well-formed token that no longer names
//!   the state, which is a reload after a ref moved: the new path runs the
//!   probes and then the walk, where `old` overlaps them.
//!
//! Pairs: **cold** (`old` against `new-stale`, each on its own fresh APFS
//! clone of the fixture, so neither has a ref-cache entry, and neither is
//! favoured by the page cache of the other's files), **warm** (`old` against
//! `new-hit` on one repository after one untimed read of it), and a **null**
//! pair for each of the two (the same arm twice) whose paired difference is the
//! noise floor. There is no read-ahead on this branch: scrolling to an older
//! window raises `max_commits`, no token is sent for a different window, and
//! the command takes the `old` path, so that case is the `old` arm and is not
//! measured separately.
//!
//! # Method
//!
//! Each round runs every pair once; within a pair the arm that goes first
//! alternates with the round's parity (the order is recorded in the output).
//! The first two rounds are discarded. Cold means a repository path the
//! process has never read (no ref-cache entry, no trust-store hit beyond the
//! untimed grant), and every cold arm gets its own. The timed region includes
//! `serde_json::to_vec` of the payload, as the IPC layer does. Git child CPU is
//! `getrusage(RUSAGE_CHILDREN)` user plus system time across the sample; git
//! processes are counted by trace2 in a separate untimed pass per arm.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::fixture::{build_chain, git, git_process_profile, trust_repo};
use common::{percentile, Report, Row};
use gitpulse_lib::procguard::LockedSpawn;

const BRANCHES: usize = 20;
/// Rounds run in all; the first `DISCARD` are not reported.
const ROUNDS: usize = 23;
const DISCARD: usize = 2;
/// (label, commits, window) the card names: the frontend's default page on a
/// small repository, and a "load more" window on a 100,000-commit one.
const SIZES: [(&str, usize, usize); 2] = [("6k/5k", 6_000, 5_000), ("100k/30k", 100_000, 30_000)];

#[derive(Clone, Copy, PartialEq)]
enum Arm {
    Old,
    NewHit,
    /// A well-formed token that names nothing real (another view and stamp).
    NewStale,
    /// The genuine token the same repository issued before one ref moved.
    NewMoved,
}

struct Obs {
    ns: u64,
    cpu_us: u64,
}

fn children_cpu_us() -> u64 {
    // SAFETY: `rusage` is plain data; getrusage writes it and reads nothing.
    let usage = unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        assert_eq!(libc::getrusage(libc::RUSAGE_CHILDREN, &mut usage), 0);
        usage
    };
    let micros = |t: libc::timeval| t.tv_sec as u64 * 1_000_000 + t.tv_usec as u64;
    micros(usage.ru_utime) + micros(usage.ru_stime)
}

/// One call of the command plus its JSON, checked to be the answer the arm
/// claims, so a timing can never be of a different answer.
///
/// `strict` is off only for the counting pass: trace2 needs a fresh global git
/// config, which the ref cache stamps, so the stamp is unsettled there and no
/// token can be issued. Timed samples are always strict.
fn call(
    path: &str,
    window: usize,
    arm: Arm,
    token: Option<&str>,
    strict: bool,
) -> (Option<String>, usize) {
    let stale = format!(
        "{:0>48}-{}",
        0,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs()
    );
    let known = match arm {
        Arm::Old => None,
        Arm::NewHit => Some(
            token
                .expect("a hit needs the token of a prior read")
                .to_string(),
        ),
        Arm::NewStale => Some(stale),
        Arm::NewMoved => Some(
            token
                .expect("a moved-ref arm needs the token from before the move")
                .to_string(),
        ),
    };
    let payload = tauri::async_runtime::block_on(gitpulse_lib::commands::cmd_get_commit_graph(
        path.to_string(),
        window,
        None,
        None,
        None,
        None,
        None,
        known,
    ))
    .unwrap_or_else(|error| panic!("cmd_get_commit_graph failed: {error}"));
    let wire = serde_json::to_vec(&payload).expect("serialize").len();
    if !strict {
        return (payload.graph_token, wire);
    }
    match arm {
        Arm::NewHit => {
            assert!(payload.unchanged, "the token should have named the state");
            assert!(payload.rows.is_empty());
        }
        Arm::Old | Arm::NewStale | Arm::NewMoved => {
            assert!(!payload.unchanged);
            assert_eq!(
                payload.rows.len(),
                window,
                "a short page would time a partial answer"
            );
            assert!(
                payload.graph_token.is_some(),
                "no token issued: the stamp was unsettled, so this sample is not the path measured"
            );
        }
    }
    (payload.graph_token, wire)
}

fn timed(path: &str, window: usize, arm: Arm, token: Option<&str>) -> Obs {
    let cpu_before = children_cpu_us();
    let started = Instant::now();
    std::hint::black_box(call(path, window, arm, token, true));
    let ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    Obs {
        ns,
        cpu_us: children_cpu_us() - cpu_before,
    }
}

/// An APFS clone of `source` with timestamps kept (a copy stamped "now" would
/// leave its ref directories inside the racy window and no token would issue).
fn clone_repo(source: &Path, dest: &Path) -> String {
    let status = std::process::Command::new("cp")
        .arg("-cRp")
        .arg(source)
        .arg(dest)
        .status_locked()
        .expect("spawn cp");
    assert!(status.success(), "cp -cRp failed");
    trust_repo(dest);
    dest.to_str().expect("utf-8").to_string()
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("no NaN"));
    percentile_f(&sorted, 0.5)
}

fn percentile_f(sorted: &[f64], q: f64) -> f64 {
    let rank = ((sorted.len() as f64 - 1.0) * q).round() as usize;
    sorted[rank]
}

/// Everything one pair observed, round by round: (first arm's obs, second's).
struct Pair {
    name: &'static str,
    first: Arm,
    second: Arm,
    a: Vec<Obs>,
    b: Vec<Obs>,
    /// Which arm ran first, per round: true when `a` did.
    a_first: Vec<bool>,
}

impl Pair {
    fn new(name: &'static str, first: Arm, second: Arm) -> Self {
        Self {
            name,
            first,
            second,
            a: Vec::new(),
            b: Vec::new(),
            a_first: Vec::new(),
        }
    }

    /// Runs `a` and `b` in the order this round's parity picks.
    fn round(&mut self, round: usize, mut a: impl FnMut() -> Obs, mut b: impl FnMut() -> Obs) {
        let a_first = round.is_multiple_of(2);
        let (oa, ob) = if a_first {
            let oa = a();
            (oa, b())
        } else {
            let ob = b();
            (a(), ob)
        };
        if round >= DISCARD {
            self.a.push(oa);
            self.b.push(ob);
            self.a_first.push(a_first);
        }
    }

    fn label(arm: Arm) -> &'static str {
        match arm {
            Arm::Old => "old",
            Arm::NewHit => "new-hit",
            Arm::NewStale => "new-stale",
            Arm::NewMoved => "new-moved",
        }
    }
}

fn ms(ns: u64) -> f64 {
    ns as f64 / 1e6
}

fn arm_stats(label: &str, obs: &[Obs]) -> String {
    let times: Vec<f64> = obs.iter().map(|o| ms(o.ns)).collect();
    let cpu: Vec<f64> = obs.iter().map(|o| o.cpu_us as f64 / 1000.0).collect();
    let min = times.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = times.iter().cloned().fold(0.0, f64::max);
    format!(
        "{label:<22} n={:<3} median {:>9.2} ms  min {:>9.2}  max {:>9.2}  git child cpu median {:>8.2} ms",
        times.len(),
        median(&times),
        min,
        max,
        median(&cpu)
    )
}

/// The four probes the command runs beside the walk (HEAD, ref decorations,
/// default branch, hidden-ref census), joined, on a path nothing has read.
fn run_probes(path: &str) {
    std::thread::scope(|scope| {
        let head = scope.spawn(|| gitpulse_lib::engine::git_reader::GitReader::head_id(path));
        let refs = scope.spawn(|| {
            gitpulse_lib::graph::list_ref_decorations(path, gitpulse_lib::graph::RefScope::Named)
        });
        let default =
            scope.spawn(|| gitpulse_lib::engine::git_reader::GitReader::default_branch_name(path));
        let hidden = scope.spawn(|| gitpulse_lib::graph::probe_hidden_history(path));
        head.join().expect("head").expect("head ok");
        refs.join().expect("refs").expect("refs ok");
        default.join().expect("default").expect("default ok");
        hidden.join().expect("hidden").expect("hidden ok");
    });
}

/// The history walk alone for a window, on a path nothing has read.
fn run_walk(path: &str, window: usize) {
    let rows = gitpulse_lib::engine::git_reader::GitReader::read_commit_history_paged(
        path,
        0,
        window + 1,
        None,
        None,
        gitpulse_lib::graph::RefScope::Named,
    )
    .expect("walk");
    assert_eq!(rows.len(), window + 1);
}

/// Where a cold reload's time goes: the probes alone and the walk alone, each
/// on its own fresh clone, in alternating order. The old path overlaps them
/// (about the larger of the two plus the assembly); the probes-first path puts
/// them in series (about their sum), so the difference between the two arms is
/// about the smaller one.
fn stage_breakdown(source: &Path, scratch: &Path, window: usize, rounds: usize, label: &str) {
    let mut probes = Vec::new();
    let mut walks = Vec::new();
    for round in 0..rounds {
        let pa = scratch.join(format!("bp{round}"));
        let wa = scratch.join(format!("bw{round}"));
        let (probe_path, walk_path) = (clone_repo(source, &pa), clone_repo(source, &wa));
        let time = |f: &dyn Fn()| {
            let started = Instant::now();
            f();
            ms(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX))
        };
        let (p, w);
        if round.is_multiple_of(2) {
            p = time(&|| run_probes(&probe_path));
            w = time(&|| run_walk(&walk_path, window));
        } else {
            w = time(&|| run_walk(&walk_path, window));
            p = time(&|| run_probes(&probe_path));
        }
        if round >= DISCARD {
            probes.push(p);
            walks.push(w);
        }
        let _ = std::fs::remove_dir_all(&pa);
        let _ = std::fs::remove_dir_all(&wa);
    }
    println!(
        "\n-- stage breakdown {label}, cold, n={}: probes alone median {:.2} ms (min {:.2}, max {:.2}); walk alone median {:.2} ms (min {:.2}, max {:.2})",
        probes.len(),
        median(&probes),
        probes.iter().cloned().fold(f64::INFINITY, f64::min),
        probes.iter().cloned().fold(0.0, f64::max),
        median(&walks),
        walks.iter().cloned().fold(f64::INFINITY, f64::min),
        walks.iter().cloned().fold(0.0, f64::max),
    );
}

fn main() {
    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
    let mut report = Report::new("graph_hit_path");
    let smoke = report.opts.smoke;
    // `--breakdown-only` skips the paired arms and prints only where a cold
    // reload's time goes.
    let breakdown_only = std::env::args().any(|arg| arg == "--breakdown-only");
    let rounds = if smoke { DISCARD + 3 } else { ROUNDS };
    let temp = tempfile::tempdir().expect("temp dir");
    report.note(format!(
        "{} rounds, first {DISCARD} discarded; arm order alternates by round parity (a first on even \
         rounds); cold = a fresh APFS clone per arm per round",
        rounds
    ));
    let mut rows: Vec<Row> = Vec::new();

    for (label, commits, window) in SIZES {
        let source = temp.path().join(format!("src-{commits}"));
        let source_path = build_chain(&source, commits, BRANCHES);
        // The fixture's refs and directories must be older than the racy
        // window for a token to be issued at all.
        std::thread::sleep(Duration::from_secs(4));
        let warm_path = source_path.clone();
        if breakdown_only {
            stage_breakdown(&source, temp.path(), window, rounds, label);
            continue;
        }

        // Work counters, in their own untimed pass (trace2 adds time).
        let spawns = |arm: Arm, cold: bool| -> u64 {
            let (path, _guard) = if cold {
                let dest = temp
                    .path()
                    .join(format!("count-{commits}-{}", Pair::label(arm)));
                (clone_repo(&source, &dest), Some(dest))
            } else {
                (warm_path.clone(), None)
            };
            // The warm closure runs after the counting config is written; the
            // sleep lets that config settle, so the read it makes issues the
            // token a warm arm would hold, and the counted call is a real hit.
            let token = std::cell::RefCell::new(None);
            let (_, procs) = git_process_profile(
                || {
                    if !cold {
                        std::thread::sleep(Duration::from_secs(4));
                        *token.borrow_mut() = call(&path, window, Arm::Old, None, false).0;
                    }
                },
                || call(&path, window, arm, token.borrow().as_deref(), false),
            );
            procs.len() as u64
        };
        let counters = [
            ("cold old", spawns(Arm::Old, true)),
            ("cold new-stale", spawns(Arm::NewStale, true)),
            ("warm old", spawns(Arm::Old, false)),
            ("warm new-hit", spawns(Arm::NewHit, false)),
        ];

        let mut cold = Pair::new("cold", Arm::Old, Arm::NewStale);
        let mut cold_null = Pair::new("cold-null", Arm::Old, Arm::Old);
        let mut warm = Pair::new("warm", Arm::Old, Arm::NewHit);
        let mut warm_null = Pair::new("warm-null", Arm::Old, Arm::Old);
        let mut moved = Pair::new("moved", Arm::Old, Arm::NewMoved);
        let mut moved_null = Pair::new("moved-null", Arm::Old, Arm::Old);

        for round in 0..rounds {
            // Cold pairs: both arms on repository paths no one has read.
            for pair in [&mut cold, &mut cold_null] {
                let (first, second) = (pair.first, pair.second);
                let ca = temp.path().join(format!("c{round}-{}-a", pair.name));
                let cb = temp.path().join(format!("c{round}-{}-b", pair.name));
                let (pa, pb) = (clone_repo(&source, &ca), clone_repo(&source, &cb));
                pair.round(
                    round,
                    || timed(&pa, window, first, None),
                    || timed(&pb, window, second, None),
                );
                let _ = std::fs::remove_dir_all(&ca);
                let _ = std::fs::remove_dir_all(&cb);
            }
            // Warm pairs: one untimed read first, so the answers are cached
            // and the token is the one a client would hold. Re-read before
            // each arm so neither inherits the other's state.
            for pair in [&mut warm, &mut warm_null] {
                let (first, second) = (pair.first, pair.second);
                let path = warm_path.clone();
                let token = call(&path, window, Arm::Old, None, true).0;
                pair.round(
                    round,
                    || timed(&path, window, first, token.as_deref()),
                    || timed(&path, window, second, token.as_deref()),
                );
            }
            // One ref moved by one commit after the client's last read, and
            // the stamp settled: the reload after an ordinary commit. Each arm
            // has its own clone, read once (warm, and the token the client
            // would hold) and then moved; one 4 s wait settles all of them.
            //
            // The clone prepared first has the longer wait behind it, so which
            // arm gets that clone alternates with the round, the same way the
            // timed order does. A first run tied "arm b" to "prepared second"
            // and its null pair read 1.9 ms apart in 16 of 21 rounds: invalid.
            let tags = ["m1a", "m1b", "m2a", "m2b"];
            let order: [usize; 4] = if round.is_multiple_of(2) {
                [0, 1, 2, 3]
            } else {
                [1, 0, 3, 2]
            };
            let mut slots: Vec<Option<(PathBuf, String, Option<String>)>> = vec![None; 4];
            for at in order {
                let dir = temp.path().join(format!("{}-{round}", tags[at]));
                let path = clone_repo(&source, &dir);
                let token = call(&path, window, Arm::Old, None, true).0;
                git(&dir, &["commit", "--allow-empty", "-q", "-m", "one more"]);
                slots[at] = Some((dir, path, token));
            }
            let prepared: Vec<(PathBuf, String, Option<String>)> = slots
                .into_iter()
                .map(|slot| slot.expect("every slot prepared"))
                .collect();
            std::thread::sleep(Duration::from_secs(4));
            for (pair, at) in [(&mut moved, 0), (&mut moved_null, 2)] {
                let (first, second) = (pair.first, pair.second);
                let (_, pa, ta) = &prepared[at];
                let (_, pb, tb) = &prepared[at + 1];
                pair.round(
                    round,
                    || timed(pa, window, first, ta.as_deref()),
                    || timed(pb, window, second, tb.as_deref()),
                );
            }
            for (dir, _, _) in prepared {
                let _ = std::fs::remove_dir_all(dir);
            }
        }

        println!("\n=== {label}: {commits} commits, {BRANCHES} branches, window {window}");
        for (name, n) in counters {
            println!("git processes, one untimed sample, {name}: {n}");
        }
        stage_breakdown(&source, temp.path(), window, rounds, label);
        for pair in [&cold_null, &cold, &moved_null, &moved, &warm_null, &warm] {
            println!(
                "\n-- pair {} ({} vs {})",
                pair.name,
                Pair::label(pair.first),
                Pair::label(pair.second)
            );
            println!("{}", arm_stats(Pair::label(pair.first), &pair.a));
            println!("{}", arm_stats(Pair::label(pair.second), &pair.b));
            // Paired difference, second minus first: negative means the
            // second arm was faster.
            let diffs: Vec<f64> = pair
                .a
                .iter()
                .zip(&pair.b)
                .map(|(a, b)| ms(b.ns) - ms(a.ns))
                .collect();
            let faster = diffs.iter().filter(|d| **d < 0.0).count();
            let mut sorted = diffs.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).expect("no NaN"));
            let (q1, q3) = (percentile_f(&sorted, 0.25), percentile_f(&sorted, 0.75));
            let first_ran_first = pair.a_first.iter().filter(|f| **f).count();
            println!(
                "paired diff (second - first): median {:>9.2} ms  IQR [{:.2}, {:.2}]  second faster in {}/{} rounds  (first arm ran first in {first_ran_first})",
                median(&diffs),
                q1,
                q3,
                faster,
                diffs.len()
            );
            if pair.first == pair.second {
                let valid = median(&diffs).abs() <= (q3 - q1) / 2.0;
                println!(
                    "NULL PAIR {}: median {:.2} ms against IQR/2 {:.2} ms -> {}",
                    pair.name,
                    median(&diffs),
                    (q3 - q1) / 2.0,
                    if valid {
                        "valid"
                    } else {
                        "INVALID: the harness is not neutral; do not report from this run"
                    }
                );
            }
            for (obs, arm) in [(&pair.a, pair.first), (&pair.b, pair.second)] {
                let mut ns: Vec<u64> = obs.iter().map(|o| o.ns).collect();
                ns.sort_unstable();
                debug_assert!(percentile(&ns, 0.5) > 0);
                rows.push(Row {
                    name: format!("{label} {} {}", pair.name, Pair::label(arm)),
                    time_gated: false,
                    ns,
                    counters: Default::default(),
                    metrics: Default::default(),
                });
            }
        }
    }
    report.extend_unrecorded(rows);
    report.finish();
}
