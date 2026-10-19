//! Benchmark, not a correctness test: the steady-state cost of one catch-up on
//! a repository whose HEAD reflog holds 5 000 entries and gains one between
//! passes (the common shape: a long-lived checkout, one new move since the last
//! pass). Run by name, `-- --ignored --exact`, from two builds interleaved; it
//! prints the minimum and the median of the passes, the host load, and the
//! count of rows the passes recorded so a build that records nothing cannot win.

use super::*;
use crate::procguard::LockedSpawn;
use crate::test_support::{commit_at, git_in};
use std::io::Write;

const ENTRIES: usize = 5_000;
const PASSES: usize = 9;

fn load() -> String {
    std::process::Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output_locked()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default()
}

#[test]
#[ignore = "benchmark: run explicitly"]
fn bench_catch_up_on_a_five_thousand_entry_reflog() {
    let dir = tempfile::tempdir().expect("repo");
    git_in(dir.path(), &["init", "-b", "main"]);
    commit_at(dir.path(), "seed", 1_790_000_000, "gitpulse@test.local");
    commit_at(dir.path(), "second", 1_790_000_001, "gitpulse@test.local");
    let oid = |rev: &str| {
        let out = std::process::Command::new("git")
            .args(["rev-parse", rev])
            .current_dir(dir.path())
            .output_locked()
            .expect("rev-parse");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let (a, b) = (oid("HEAD~1"), oid("HEAD"));
    let log = dir.path().join(".git").join("logs").join("HEAD");
    let line = |n: usize| {
        let (old, new) = if n.is_multiple_of(2) {
            (&b, &a)
        } else {
            (&a, &b)
        };
        format!(
            "{old} {new} GitPulse <gitpulse@test.local> {} +0000\tcheckout: moving to bench-{n}\n",
            1_790_000_100 + n
        )
    };
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .expect("HEAD reflog");
    for n in 0..ENTRIES {
        file.write_all(line(n).as_bytes()).expect("write entry");
    }
    let repo = dir
        .path()
        .canonicalize()
        .expect("canonical")
        .to_string_lossy()
        .into_owned();
    let corpus = tempfile::tempdir().expect("corpus");
    let serial = crate::test_support::env::env_serial();
    let _env =
        crate::test_support::env::bind_env(&serial).set("GITPULSE_TRANSCRIPT_ROOT", corpus.path());

    // Bring the ledger to steady state: the first passes take in whatever the
    // build takes in from a 5 000-entry log.
    let mut warm = 0;
    for _ in 0..30 {
        let out = catch_up_into_bounded(&repo, &repo, None);
        warm += out.recorded;
        if out.recorded == 0 && !out.truncated {
            break;
        }
    }
    // The default-branch lookup is cached against the ref store's stamp, which
    // is not trusted while younger than the racy window (3 s): a repository
    // built a moment ago would price the lookup as five Git children on every
    // pass, which no checkout that has sat for a few seconds pays.
    std::thread::sleep(std::time::Duration::from_secs(5));
    let mut times = Vec::new();
    let mut recorded = 0;
    let mut spawned = Vec::new();
    for n in 0..PASSES {
        file.write_all(line(ENTRIES + n).as_bytes())
            .expect("write entry");
        let children = crate::engine::git_cli::spawn_log::spawns_in(std::path::Path::new(&repo));
        let started = std::time::Instant::now();
        let out = catch_up_into_bounded(&repo, &repo, None);
        times.push(started.elapsed().as_secs_f64() * 1000.0);
        recorded += out.recorded;
        spawned = crate::engine::git_cli::spawn_log::spawns_in(std::path::Path::new(&repo))
            .into_iter()
            .skip(children.len())
            .map(|argv| argv.join(" "))
            .collect();
    }
    println!("BENCH git children in the last pass: {spawned:#?}");
    times.sort_by(|x, y| x.partial_cmp(y).expect("finite"));
    println!(
        "BENCH catch_up_5k min_ms={:.2} median_ms={:.2} warm_recorded={warm} recorded={recorded} load={}",
        times[0],
        times[times.len() / 2],
        load()
    );
}
