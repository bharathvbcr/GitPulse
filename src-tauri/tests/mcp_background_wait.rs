//! The MCP configuration end to end, in a process of its own: both switches
//! `gitpulse-mcp`'s `main` makes, then real spawns from a worker thread (as a
//! request thread is). The per-thread flag the switch seeds must reach threads
//! the process starts afterwards; if it did not, a drained bucket would still
//! shed at 0.000s and the lane's fix would be a no-op in production.

mod common;

use gitpulse_lib::ipc_error::CommandError;
use std::time::{Duration, Instant};

use gitpulse_lib::engine::git_cli::{
    current_admission_name, git_global, run_process_as_background,
    run_process_awaiting_background_budget,
};

const SHED: &str = "background work is shed while the git spawn rate budget is low";

/// Spawns `git --version` until one call is slowed by the wait (the bucket is
/// at the reserve and the call waited for a refill) or refused. Returns how
/// many spawns it took, so a call slowed by host load alone (too few spawns to
/// have drained 144 tokens) cannot pass for the reserve.
fn drain_until_paced() -> Result<(usize, Duration), CommandError> {
    for spawns in 1..=800 {
        let began = Instant::now();
        git_global(&["--version"])?;
        if began.elapsed() >= Duration::from_millis(60) && spawns >= 120 {
            return Ok((spawns, began.elapsed()));
        }
    }
    Err("never reached the reserve in 800 spawns".into())
}

#[test]
fn a_worker_thread_in_the_mcp_configuration_waits_instead_of_shedding() {
    run_process_as_background();
    if std::env::var_os("GP_ADV_CONTROL_NO_WAIT").is_none() {
        run_process_awaiting_background_budget();
    }
    let outcome = std::thread::spawn(|| {
        assert_eq!(current_admission_name(), "background");
        drain_until_paced()
    })
    .join()
    .unwrap();
    match outcome {
        Ok((spawns, paced)) => {
            println!("paced after {spawns} spawns, one call took {paced:?}");
            assert!(
                paced < common::env::coverage_relaxed(Duration::from_millis(2_500)),
                "{paced:?}"
            );
        }
        Err(error) => panic!(
            "shed={} at the reserve: {error}",
            error.contains(SHED),
            error = error.message()
        ),
    }
}
