//! The host-load sampler holds its 2 s cadence: a soak, run deliberately.
//!
//! ```text
//! cargo test --release --test host_load_soak -- --ignored --nocapture
//! ```
//!
//! `GITPULSE_HOST_LOAD_SOAK_SECS` sets the length (600 by default: the
//! card's 10-minute soak; 14400 is the 4-hour one) and
//! `GITPULSE_HOST_LOAD_SOAK_REPORT` names a file the JSON result is written
//! to, so a run can be recorded rather than remembered.
//!
//! What it measures is **real**: the production sampler on this machine's
//! own process table, at the production cadence, with its QoS class and App
//! Nap activity requested exactly as the app requests them. What it is not:
//! the app with its window hidden. A test process is not an app, so App Nap
//! would not apply to it either way; the hidden-window condition is the
//! installed app's, and is checked there.

mod common;

use gitpulse_lib::sysload::{start, SamplerOptions};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CADENCE: Duration = Duration::from_millis(gitpulse_lib::sysload::CADENCE_MS);

fn soak_secs() -> u64 {
    std::env::var("GITPULSE_HOST_LOAD_SOAK_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|s| *s >= 10)
        .unwrap_or(600)
}

#[test]
#[ignore = "a 10-minute soak at the production cadence; run deliberately"]
fn the_sampler_misses_no_tick_over_a_soak() {
    let secs = soak_secs();
    let dir = tempfile::tempdir().unwrap();
    let arrivals = Arc::new(Mutex::new(Vec::with_capacity((secs / 2 + 16) as usize)));
    let seen = arrivals.clone();
    let mut options = SamplerOptions::new(dir.path().to_path_buf());
    options.cadence = CADENCE;
    options.on_tick = Some(Box::new(move |_| seen.lock().unwrap().push(Instant::now())));
    let started = Instant::now();
    let sampler = start(options).expect("the sampler starts on this platform");
    std::thread::sleep(Duration::from_secs(secs));
    let status = sampler.status().expect("a status after the soak");
    sampler.stop();
    let elapsed = started.elapsed();

    let arrivals = arrivals.lock().unwrap();
    let mut gaps: Vec<Duration> = arrivals.windows(2).map(|w| w[1] - w[0]).collect();
    gaps.sort();
    let gap_at = |q: f64| {
        gaps.get(((gaps.len().saturating_sub(1)) as f64 * q).round() as usize)
            .copied()
            .unwrap_or_default()
    };
    let expected = elapsed.as_millis() as u64 / CADENCE.as_millis() as u64;
    let cpu_share = status.tick.cpu_ns as f64 / elapsed.as_nanos() as f64;
    let report = serde_json::json!({
        "tag": "real",
        "what": "host-load sampler soak, test process (not the app's hidden window)",
        "seconds": elapsed.as_secs_f64(),
        "cadence_ms": CADENCE.as_millis() as u64,
        "ticks": status.tick.ticks,
        "ticks_expected": expected,
        "missed": status.tick.missed,
        "overruns": status.tick.overruns,
        "tick_last_ms": status.tick.last_us as f64 / 1000.0,
        "tick_max_ms": status.tick.max_us as f64 / 1000.0,
        "gap_p50_ms": gap_at(0.5).as_secs_f64() * 1000.0,
        "gap_p99_ms": gap_at(0.99).as_secs_f64() * 1000.0,
        "gap_max_ms": gaps.last().copied().unwrap_or_default().as_secs_f64() * 1000.0,
        "sampler_cpu_s": status.tick.cpu_ns as f64 / 1e9,
        "sampler_cpu_share": cpu_share,
        "role": status.role,
        "problems": status.problems,
        "processes": status.procs.as_ref().map(|p| p.processes),
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    if let Ok(path) = std::env::var("GITPULSE_HOST_LOAD_SOAK_REPORT") {
        std::fs::write(&path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    }

    assert!(status.problems.is_empty(), "{:?}", status.problems);
    assert_eq!(status.tick.missed, 0, "missed ticks: {report}");
    assert_eq!(status.tick.overruns, 0, "ticks over the cadence: {report}");
    assert!(
        status.tick.ticks + 1 >= expected,
        "{} ticks in {:?}, expected {expected}",
        status.tick.ticks,
        elapsed
    );
    // A tick that is late but not missed still shows as a long gap.
    assert!(
        gaps.last().copied().unwrap_or_default() < CADENCE * 2,
        "a gap of {:?} between ticks: {report}",
        gaps.last()
    );
}
