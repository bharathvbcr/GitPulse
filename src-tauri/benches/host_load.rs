//! What one host-load sampler tick costs, on this machine's real process
//! table.
//!
//! Run with `cargo bench --bench host_load`. Deliberately not Criterion, for
//! the reasons `mcp_protocol.rs` gives. Every row reports min, p50, p95 and
//! worst over `SAMPLES` runs after `WARMUP`; the min is the figure to compare
//! between builds (min-of-N), the tail is what the 2 s cadence has to absorb.
//!
//! Results are **real**: no fixture processes are started — the card caps a
//! fixture wave at 16 processes, so a ~1,000-process table is measured where
//! it already exists, and the row says how many processes it actually read.
//! A machine with fewer will report fewer; the number is printed rather than
//! assumed.
//!
//! Rows:
//! * `scan (cold)`: a fresh scanner, so every process's identity
//!   (`KERN_PROCARGS2`) is read. The sampler still pays this.
//!   `observe_sessions` does not; its cost is the fresh-process table below.
//! * `scan (warm)`: the sampler's steady state — identities cached by pid and
//!   start time, so only footprints, disk writes and CPU are read per process.
//! * `host probe`: the host-wide sysctls, `host_statistics64`, load, disk,
//!   thermal and power.
//! * `publish + read`: one snapshot write under the sequence lock and one
//!   reader decode, in a temp directory.
//!
//! `GITPULSE_HOST_LOAD_ONCE=scan|sessions|collision` runs one sample in this
//! process and exits. `gitpulse-hook` is a new process per call, so that is
//! the figure for the collision guard; the rows above reuse one process.
//!
//! Fresh release processes, 15 each, on 2026-10-14. Before is the full table
//! read `observe_sessions` used to pay (1,247–1,250 processes, 33 sessions).
//! After is `observe_sessions` once it reads arguments only for processes
//! that could be agents (1,197–1,205 processes, 35 sessions). Min is the
//! number to compare.
//!
//! | | min | p50 | p95 | worst |
//! |---|---|---|---|---|
//! | before, full scan | 20.34 ms | 22.95 ms | 29.61 ms | 29.82 ms |
//! | after, `observe_sessions` | 3.76 ms | 5.84 ms | 14.02 ms | 15.12 ms |
//! | after, full scan (sampler, unchanged path) | 17.70 ms | 21.42 ms | 33.82 ms | 34.85 ms |
//!
//! The collision-risk call around that scan is dominated by git worktree
//! listing. Before: min 377.5 ms, p50 646.0 ms, p95 969.0 ms, worst 1005.2 ms
//! (12 worktrees). After: min 287.1 ms, p50 340.6 ms, p95 563.7 ms, worst
//! 1049.4 ms (11 worktrees). `sessions_ok` was true throughout.

#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::time::{Duration, Instant};

#[cfg(any(target_os = "macos", target_os = "linux"))]
const SAMPLES: usize = 60;
#[cfg(any(target_os = "macos", target_os = "linux"))]
const WARMUP: usize = 5;

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct Row {
    name: &'static str,
    min: Duration,
    p50: Duration,
    p95: Duration,
    worst: Duration,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn measure(name: &'static str, samples: usize, mut run: impl FnMut()) -> Row {
    for _ in 0..WARMUP {
        run();
    }
    let mut times: Vec<Duration> = (0..samples)
        .map(|_| {
            let started = Instant::now();
            run();
            started.elapsed()
        })
        .collect();
    times.sort();
    let at = |q: f64| times[((times.len() - 1) as f64 * q).round() as usize];
    Row {
        name,
        min: times[0],
        p50: at(0.5),
        p95: at(0.95),
        worst: *times.last().unwrap(),
    }
}

/// One sample in this process, then exit.
///
/// `gitpulse-hook` is a new process per call, so the figure that matters is
/// the first scan, not a warm repeat inside one process. Launch this binary
/// once per sample with `GITPULSE_HOST_LOAD_ONCE=scan` (the sampler's full
/// table read), `sessions` (`observe_sessions`, what the hook's collision
/// guard pays) or `collision`. `GITPULSE_HOST_LOAD_REPO` is the repository
/// the collision sample reads.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn one_shot() -> bool {
    let Ok(mode) = std::env::var("GITPULSE_HOST_LOAD_ONCE") else {
        return false;
    };
    match mode.as_str() {
        "scan" => {
            use gitpulse_lib::sysload::{ProcessScanner, ScanContext};
            let started = std::time::Instant::now();
            let mut fresh = ProcessScanner::os();
            let view = fresh.scan(&ScanContext::here());
            let us = started.elapsed().as_micros();
            println!(
                "once scan_us={us} ok={} processes={} sessions={}",
                view.ok,
                view.processes,
                view.sessions.len()
            );
        }
        "sessions" => {
            let started = std::time::Instant::now();
            match gitpulse_lib::sysload::observe_sessions() {
                Ok(sessions) => {
                    let us = started.elapsed().as_micros();
                    println!("once sessions_us={us} ok=true sessions={}", sessions.len());
                }
                Err(error) => {
                    let us = started.elapsed().as_micros();
                    println!("once sessions_us={us} ok=false error={error}");
                    std::process::exit(1);
                }
            }
        }
        "collision" => {
            let repo = std::env::var("GITPULSE_HOST_LOAD_REPO").unwrap_or_else(|_| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").to_string())
            });
            let started = std::time::Instant::now();
            let risk = gitpulse_lib::insights::collision_for_path(
                &repo,
                "src-tauri/src/sysload/procscan.rs",
                None,
            );
            let us = started.elapsed().as_micros();
            println!(
                "once collision_us={us} ok={} sessions_ok={} scanned={}",
                risk.ok, risk.sessions_ok, risk.scanned_worktrees
            );
        }
        other => {
            eprintln!("unknown GITPULSE_HOST_LOAD_ONCE={other}");
            std::process::exit(2);
        }
    }
    true
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn main() {
    if one_shot() {
        return;
    }
    use gitpulse_lib::sysload::{
        Clock, OsSensor, ProcessScanner, ScanContext, Sensor, Snapshot, SnapshotWriter,
        WriterIdentity,
    };

    let ctx = ScanContext::here();
    let mut probe_scan = ProcessScanner::os();
    let view = probe_scan.scan(&ctx);
    assert!(
        view.ok,
        "the process table could not be read: {:?}",
        view.error
    );
    println!(
        "real: {} processes in the table ({} other users', {} unreadable), {} agent sessions",
        view.processes,
        view.other_users,
        view.unreadable,
        view.sessions.len()
    );

    let mut rows = Vec::new();
    // A cold scan reads every identity; fewer samples, it is the slow one.
    rows.push(measure("scan (cold)", SAMPLES / 4, || {
        let mut fresh = ProcessScanner::os();
        std::hint::black_box(fresh.scan(&ScanContext::here()));
    }));
    let mut warm = ProcessScanner::os();
    warm.scan(&ctx);
    rows.push(measure("scan (warm)", SAMPLES, || {
        std::hint::black_box(warm.scan(&ScanContext::here()));
    }));
    let dir = tempfile::tempdir().expect("temp dir");
    let mut sensor = OsSensor::new(dir.path());
    rows.push(measure("host probe", SAMPLES, || {
        std::hint::black_box(sensor.sample());
    }));
    let snapshot_dir = tempfile::tempdir().expect("temp dir");
    let path = snapshot_dir
        .path()
        .join(gitpulse_lib::sysload::SNAPSHOT_FILE_NAME);
    let mut writer = SnapshotWriter::elect(snapshot_dir.path())
        .expect("elect")
        .expect("a fresh directory has no other writer");
    let snap = Snapshot::empty(Clock::now(), WriterIdentity::default(), true);
    rows.push(measure("publish + read", SAMPLES, || {
        writer.publish(&snap).expect("publish");
        std::hint::black_box(gitpulse_lib::sysload::read(&path).expect("decodes"));
    }));

    println!(
        "{:<16} {:>10} {:>10} {:>10} {:>10}",
        "row", "min", "p50", "p95", "worst"
    );
    for row in &rows {
        println!(
            "{:<16} {:>10.2?} {:>10.2?} {:>10.2?} {:>10.2?}",
            row.name, row.min, row.p50, row.p95, row.worst
        );
    }
    let tick = rows[1].p95 + rows[2].p95 + rows[3].p95;
    println!(
        "steady-state tick (warm scan + probe + publish, p95 sum): {tick:.2?} of the 2 s cadence"
    );
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn main() {
    println!("no host-load sampler on this platform; nothing to measure");
}
