//! Per-scan cost: gix in-process status against spawning
//! `git status --porcelain=v1 -u -z`, on a fixture shaped like the phase-1
//! profile (one main checkout plus 17 linked worktrees, every one dirty).
//!
//! Run (release profile, which `cargo bench` uses):
//!
//! ```text
//! GSP_FILES=600 GSP_ROUNDS=15 cargo bench --manifest-path crates/git-status-parity/Cargo.toml
//! GSP_FILES=3000 GSP_ROUNDS=15 cargo bench --manifest-path crates/git-status-parity/Cargo.toml
//! # lean gix features against gix's own performance bundle:
//! GSP_FILES=3000 cargo bench --manifest-path crates/git-status-parity/Cargo.toml --features max-performance
//! ```
//!
//! One *sweep* is one status of every one of the 18 worktrees, in order, which
//! is what one GitPulse refresh does. A round runs every arm once, the start
//! rotating each round (interleaved A/B). The first round is a warm-up and is
//! dropped. The noise floor is the same arm (`git spawn`) timed twice per round
//! under different labels: the gap between those two is what no A/B gap smaller
//! than it can be believed over.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use git_status_parity::{git, git_status, gix_status, hermetic_environment, Row, Untracked};

const LINKED: usize = 17;
const DIRS: usize = 30;

fn rel(index: usize) -> String {
    let ext = ["rs", "ts", "go", "md", "json", "py", "css", "html"][index % 8];
    format!("d{:03}/f{index:06}.{ext}", index % DIRS)
}

fn content(index: usize, revision: usize) -> String {
    (0..24)
        .map(|line| {
            format!(
                "// file {index} rev {revision} line {line}: let value_{line} = {};\n",
                index * 31 + line * 7 + revision
            )
        })
        .collect()
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, text).expect("write");
}

/// Main checkout plus `LINKED` linked worktrees. Every worktree has `dirty`
/// modified files and one untracked file; the main one has extra.
fn build(root: &Path, files: usize, dirty: usize) -> Vec<PathBuf> {
    let main = root.join("main");
    fs::create_dir_all(&main).expect("mkdir");
    git(&main, &["init", "-q", "-b", "main"]);
    git(&main, &["config", "commit.gpgsign", "false"]);
    for i in 0..files {
        write(&main.join(rel(i)), &content(i, 0));
    }
    git(&main, &["add", "-A"]);
    git(&main, &["commit", "-q", "-m", "initial"]);
    let mut all = vec![main.clone()];
    for n in 0..LINKED {
        let path = root.join(format!("wt{n:02}"));
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &format!("wt/{n:02}"),
                path.to_str().expect("utf8"),
            ],
        );
        all.push(path);
    }
    for (n, wt) in all.iter().enumerate() {
        for k in 0..dirty {
            let i = (n * 7 + k * 101) % files;
            write(&wt.join(rel(i)), &content(i, 9_000 + n));
        }
        write(&wt.join(format!("scratch-{n}.txt")), "untracked\n");
    }
    all
}

/// One timed sweep over every worktree; returns the row count so the work is observable.
type Sweep = Box<dyn FnMut(&[PathBuf]) -> usize>;

struct Arm {
    name: &'static str,
    run: Sweep,
}

fn git_spawn(extra: &'static [&'static str]) -> Sweep {
    Box::new(move |wts| {
        let mut total = 0;
        for wt in wts {
            if extra.is_empty() {
                total += git_status(wt).expect("git status").len();
            } else {
                let out = std::process::Command::new("git")
                    .args(["status", "--porcelain=v1"])
                    .args(extra)
                    .arg("-z")
                    .current_dir(wt)
                    .output()
                    .expect("spawn");
                total += git_status_parse(&out.stdout);
            }
        }
        total
    })
}

fn git_status_parse(bytes: &[u8]) -> usize {
    git_status_parity::parse_porcelain_z(bytes).len()
}

fn gix_warm(handles: Vec<gix::Repository>, mode: Untracked) -> Sweep {
    Box::new(move |_| {
        handles
            .iter()
            .map(|repo| gix_status(repo, mode).expect("gix").len())
            .sum()
    })
}

fn gix_cold(mode: Untracked) -> Sweep {
    Box::new(move |wts| {
        wts.iter()
            .map(|wt| {
                let repo = gix::open(wt).expect("open");
                gix_status(&repo, mode).expect("gix").len()
            })
            .sum()
    })
}

fn pct(sorted: &[Duration], p: f64) -> Duration {
    sorted[(((sorted.len() - 1) as f64) * p).round() as usize]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn main() {
    let files: usize = std::env::var("GSP_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600);
    let rounds: usize = std::env::var("GSP_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(15);
    let dirty: usize = std::env::var("GSP_DIRTY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let root = std::env::temp_dir().join(format!("git-status-parity-bench-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("mkdir root");
    hermetic_environment(&root);
    let wts = build(&root, files, dirty);

    // Correctness gate before any timing: the arms must return git's rows.
    let mut mismatches = 0;
    for wt in &wts {
        let expected: Vec<Row> = git_status(wt).expect("git");
        let got = gix_status(&gix::open(wt).expect("open"), Untracked::Files).expect("gix");
        if got != expected {
            mismatches += 1;
            eprintln!(
                "PARITY MISMATCH in {}: git {expected:?} gix {got:?}",
                wt.display()
            );
        }
    }
    let rows_per_sweep: usize = wts.iter().map(|w| git_status(w).expect("git").len()).sum();

    let handles = |_: ()| {
        wts.iter()
            .map(|w| gix::open(w).expect("open"))
            .collect::<Vec<_>>()
    };
    let mut arms = [
        Arm {
            name: "git spawn -u (A)",
            run: git_spawn(&[]),
        },
        Arm {
            name: "git spawn -u (A', same arm: noise floor)",
            run: git_spawn(&[]),
        },
        Arm {
            name: "git spawn -uno",
            run: git_spawn(&["-uno"]),
        },
        Arm {
            name: "gix warm, untracked Files",
            run: gix_warm(handles(()), Untracked::Files),
        },
        Arm {
            name: "gix warm, untracked Off",
            run: gix_warm(handles(()), Untracked::Off),
        },
        Arm {
            name: "gix cold open, untracked Files",
            run: gix_cold(Untracked::Files),
        },
        Arm {
            name: "gix cold open, untracked Off",
            run: gix_cold(Untracked::Off),
        },
    ];
    let n_arms = arms.len();
    let mut samples: Vec<Vec<Duration>> = vec![Vec::new(); n_arms];
    for round in 0..=rounds {
        for step in 0..n_arms {
            let idx = (round + step) % n_arms;
            let start = Instant::now();
            let rows = (arms[idx].run)(&wts);
            let took = start.elapsed();
            std::hint::black_box(rows);
            if round > 0 {
                samples[idx].push(took);
            }
        }
    }

    let load = std::process::Command::new("uptime")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("fixture: {files} tracked files, {} worktrees (1 main + {LINKED} linked), {dirty} modified files + 1 untracked file each, {rows_per_sweep} status rows per sweep", wts.len());
    println!("rounds: n={rounds} (first round dropped), arms interleaved with rotating start; profile: {}", if cfg!(debug_assertions) { "DEBUG (not a valid measurement)" } else { "release" });
    println!(
        "features: {}",
        if cfg!(feature = "max-performance") {
            "gix status+sha1+max-performance"
        } else {
            "gix status+sha1 (lean)"
        }
    );
    println!("parity gate (gix Files vs git -u, every worktree): {} mismatching worktrees\nhost: {load}\n", mismatches);
    println!("| arm | min sweep ms | median | max | min per scan ms | min vs git spawn -u |");
    println!("| --- | --- | --- | --- | --- | --- |");
    let base = samples[0].iter().min().copied().expect("samples");
    for (arm, s) in arms.iter().zip(&samples) {
        let mut sorted = s.clone();
        sorted.sort();
        let min = sorted[0];
        println!(
            "| {} | {:.1} | {:.1} | {:.1} | {:.2} | {:.2}x |",
            arm.name,
            ms(min),
            ms(pct(&sorted, 0.5)),
            ms(*sorted.last().expect("samples")),
            ms(min) / wts.len() as f64,
            ms(min) / ms(base),
        );
    }
    let paired: Vec<f64> = samples[0]
        .iter()
        .zip(&samples[1])
        .map(|(a, b)| ms(*a) / ms(*b))
        .collect();
    let (lo, hi) = paired
        .iter()
        .fold((f64::MAX, f64::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
    println!("\nnoise floor: same arm timed twice per round, paired ratio A/A' ranges {lo:.2}x to {hi:.2}x over n={rounds}; min/min = {:.2}x", ms(samples[0].iter().min().copied().expect("s")) / ms(samples[1].iter().min().copied().expect("s")));
    let _ = fs::remove_dir_all(&root);
}
