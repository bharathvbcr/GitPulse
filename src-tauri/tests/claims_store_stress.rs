//! The claim store under concurrent hook processes and at its compaction
//! threshold. Each child process is this test binary re-run with an
//! environment variable, because the hook is one process per edit and the lock
//! that matters is the file lock between processes, not a mutex.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use gitpulse_lib::insights::claims::{
    append_claim, read_store, ClaimSource, FileClaim, DEFAULT_CAP, DEFAULT_TTL_MS,
};
use gitpulse_lib::procguard::LockedSpawn;

const T0: u64 = 1_794_110_184_306;
/// The store compacts at this many bytes per row of cap. Kept here, not
/// exported, so the test fills "to the threshold" the way a real store gets
/// there: by appends.
const COMPACT_BYTES_PER_ROW: u64 = 512;

fn realistic(tag: &str, i: usize, ts: u64) -> FileClaim {
    FileClaim {
        source: ClaimSource::Observed,
        session_id: Some(format!("0b6c7f3e-1d2a-4c5b-9e8f-{:012}", i % 7)),
        run_id: None,
        path: format!("src-tauri/src/insights/{tag}_{i:05}.rs"),
        worktree: "/Users/bharath/Code/devtools/GitPulse/.gitpulse/worktrees/b1106w9-claims".into(),
        ts_ms: ts,
        cwd: Some(
            "/Users/bharath/Code/devtools/GitPulse/.gitpulse/worktrees/b1106w9-claims".into(),
        ),
    }
}

fn fill_to_threshold(store: &Path, cap: usize) -> usize {
    let threshold = cap as u64 * COMPACT_BYTES_PER_ROW;
    let mut rows = 0usize;
    while fs::metadata(store).map(|m| m.len()).unwrap_or(0) + 400 < threshold {
        append_claim(
            store,
            &realistic("fill", rows, T0 + rows as u64),
            cap,
            DEFAULT_TTL_MS,
            T0,
        )
        .unwrap();
        rows += 1;
    }
    rows
}

fn percentile(sorted: &[Duration], q: f64) -> Duration {
    sorted[((sorted.len() as f64 - 1.0) * q) as usize]
}

/// Six processes, fifty appends each, against a store one append short of
/// compacting. The previous store rewrote and fsynced the whole file on every
/// append past a threshold below what its own cap needed, and held an
/// exclusive lock for it: 251 of 300 appends timed out on the 20 ms lock wait.
#[cfg(unix)]
#[test]
fn concurrent_hook_processes_at_the_compaction_threshold_lose_no_claim() {
    if let Ok(store) = std::env::var("W9_CHILD_STORE") {
        let id: usize = std::env::var("W9_CHILD_ID").unwrap().parse().unwrap();
        let mut failed = 0;
        let mut took = Vec::new();
        for k in 0..50usize {
            let ts = T0 + 100_000 + (id * 1000 + k) as u64;
            let started = Instant::now();
            let result = append_claim(
                Path::new(&store),
                &realistic(&format!("child{id}"), k, ts),
                DEFAULT_CAP,
                DEFAULT_TTL_MS,
                ts,
            );
            took.push(started.elapsed());
            if result.is_err() {
                failed += 1;
            }
        }
        took.sort();
        println!(
            "W9_STATS failed={failed} p50_us={} p99_us={} max_us={}",
            percentile(&took, 0.5).as_micros(),
            percentile(&took, 0.99).as_micros(),
            took.last().unwrap().as_micros()
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("gitpulse").join("file-claims.jsonl");
    let filled = fill_to_threshold(&store, DEFAULT_CAP);
    let exe = std::env::current_exe().unwrap();
    let children: Vec<_> = (0..6usize)
        .map(|id| {
            std::process::Command::new(&exe)
                .args([
                    "concurrent_hook_processes_at_the_compaction_threshold_lose_no_claim",
                    "--exact",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("W9_CHILD_STORE", &store)
                .env("W9_CHILD_ID", id.to_string())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn_locked()
                .unwrap()
        })
        .collect();
    let mut failed_total = 0usize;
    for child in children {
        let out = child.wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let stats = text
            .lines()
            .find(|line| line.contains("W9_STATS"))
            .unwrap_or_else(|| panic!("child printed no stats: {text}"));
        eprintln!("child {stats}");
        let failed: usize = stats
            .split("failed=")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|digits| digits.parse().ok())
            .expect("failed count");
        failed_total += failed;
    }
    let read = read_store(&store, T0 + 200_000, DEFAULT_TTL_MS, DEFAULT_CAP).unwrap();
    eprintln!(
        "6 processes x 50 appends over a store of {filled} rows at the compaction threshold: \
         {failed_total} failed of 300; {} rows live, {} lines skipped",
        read.rows.len(),
        read.skipped
    );
    assert_eq!(failed_total, 0, "an append failed against the claim lock");
    assert_eq!(
        read.skipped, 0,
        "a torn or interleaved line reached the file"
    );
    for id in 0..6usize {
        let last = format!("src-tauri/src/insights/child{id}_{:05}.rs", 49);
        assert!(
            read.rows.iter().any(|row| row.path == last),
            "child {id}'s newest claim is missing: a compaction dropped an append"
        );
    }
}

/// One process appending to a store at the threshold: the numbers recorded in
/// docs/PERFORMANCE.md. Only a loose bound is asserted; the figures are for
/// the record.
#[test]
fn an_append_to_a_full_store_stays_far_under_the_lock_wait() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("gitpulse").join("file-claims.jsonl");
    fill_to_threshold(&store, DEFAULT_CAP);
    let mut took = Vec::new();
    for k in 0..400usize {
        let ts = T0 + 50_000 + k as u64;
        let started = Instant::now();
        append_claim(
            &store,
            &realistic("late", k, ts),
            DEFAULT_CAP,
            DEFAULT_TTL_MS,
            ts,
        )
        .unwrap();
        took.push(started.elapsed());
    }
    took.sort();
    eprintln!(
        "append at a full store (cap {DEFAULT_CAP}, 400 appends incl. one compaction): \
         p50 {:?} p99 {:?} max {:?}",
        percentile(&took, 0.5),
        percentile(&took, 0.99),
        took.last().unwrap()
    );
    assert!(
        percentile(&took, 0.5) < Duration::from_millis(5),
        "median append took {:?}",
        percentile(&took, 0.5)
    );
}
