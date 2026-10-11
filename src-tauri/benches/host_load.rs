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
//!   (`KERN_PROCARGS2`) is read — what `observe_sessions` pays in a new
//!   process such as `gitpulse-mcp`.
//! * `scan (warm)`: the sampler's steady state — identities cached by pid and
//!   start time, so only footprints, disk writes and CPU are read per process.
//! * `host probe`: the host-wide sysctls, `host_statistics64`, load, disk,
//!   thermal and power.
//! * `publish + read`: one snapshot write under the sequence lock and one
//!   reader decode, in a temp directory.

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

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn main() {
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
