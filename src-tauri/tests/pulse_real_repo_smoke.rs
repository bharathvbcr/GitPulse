//! Exercises every Pulse-backing reader against a REAL repository.
//!
//! The unit tests build tiny fixture repos, which cannot surface the failures
//! that only appear at real scale: a blame walk over hundreds of files, a
//! `describe --contains` against real tags, a numstat stream past the byte
//! budget. Ignored by default because it needs a repository to point at.
//!
//! GITPULSE_PULSE_REPO=/path/to/repo \
//!   cargo test --manifest-path src-tauri/Cargo.toml \
//!   --test pulse_real_repo_smoke -- --ignored --nocapture

use gitpulse_lib::engine::GitReader;

fn repo_from_env() -> String {
    std::env::var("GITPULSE_PULSE_REPO").expect("set GITPULSE_PULSE_REPO to a repository path")
}

#[test]
#[ignore = "needs GITPULSE_PULSE_REPO pointing at a real repository"]
fn every_pulse_reader_answers_on_a_real_repository() {
    let path = repo_from_env();

    match GitReader::pulse_report(&path, Some(5_000)) {
        Ok(r) => println!(
            "pulse_report OK commits={} top_files={} exts={} truncated={} payload_truncated={} ms={}",
            r.commits.len(),
            r.top_files_by_churn.len(),
            r.extensions.len(),
            r.truncated,
            r.payload_truncated,
            r.duration_ms
        ),
        Err(e) => panic!("pulse_report FAILED: {e}"),
    }

    match GitReader::knowledge_report(&path, None) {
        Ok(r) => println!(
            "knowledge_report OK scanned={}/{} lines={} bus_factor={} orphans={} truncated={} ms={}",
            r.scanned_files,
            r.candidate_files,
            r.scanned_lines,
            r.bus_factor,
            r.orphaned_files.len(),
            r.truncated,
            r.duration_ms
        ),
        Err(e) => panic!("knowledge_report FAILED: {e}"),
    }

    match GitReader::dora_report(&path, Some(90)) {
        Ok(r) => println!(
            "dora_report OK releases={} freq/wk={:.2} lead_h={:.1} cfr={:.1}% mttr_h={:.1}",
            r.total_releases,
            r.deploy_frequency_per_week,
            r.median_lead_time_hours,
            r.change_failure_rate_pct,
            r.mttr_hours
        ),
        Err(e) => panic!("dora_report FAILED: {e}"),
    }
}
