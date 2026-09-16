//! Adversarial cover for the spawn seam the installation-health report
//! depends on: the `PATH` every spawned tool is handed.
//!
//! It exists because of a reported fault. GitPulse resolved `devmap` through
//! the GUI-launch fallback dirs, spawned it with launchd's minimal `PATH`, and
//! rendered devmap's resulting "host MCP config names a devmap path that is
//! not a file" under *Installation health* — a fault GitPulse had manufactured
//! from the environment it chose, presented as the user's broken install.
//!
//! The fix rewrites the environment of every child that reaches the bounded
//! runner. This is where that rewrite is attacked: hundreds of spawns at once,
//! and a caller whose own explicit `PATH` must survive the race. The shape of
//! the value itself is asserted in `engine::git_cli`'s own tests, where the
//! builder is visible; nothing is restated here.
//!
//! Its own executable because the library suite runs hundreds of tests
//! concurrently against the same admission gate, and a contention measurement
//! sharing that gate measures the suite instead.
#![cfg(unix)]

use gitpulse_lib::engine::git_cli::capture_command;
use std::time::Duration;

const PRINT_PATH: [&str; 2] = ["-c", "printf '%s' \"$PATH\""];

fn child_path(extra_env: &[(&str, &str)]) -> String {
    let run = capture_command(
        "/bin/sh",
        &PRINT_PATH,
        None,
        Duration::from_secs(20),
        extra_env,
    )
    .expect("spawn");
    assert!(run.success, "child failed");
    String::from_utf8_lossy(&run.stdout).into_owned()
}

/// Several hundred children through the seam at once, each having its
/// environment rewritten on the way in.
///
/// The rewrite reads the process environment and allocates per spawn, inside
/// the admission gate and across every worker thread. A race, a lock, or a
/// value that depends on how many children are in flight would show up here as
/// a failed spawn or a child holding a different `PATH` — never as a compile
/// error, and never in a single-threaded test.
///
/// Compared against an uncontended child rather than a spelled-out expected
/// value: the question this test asks is whether contention changes the
/// answer, and a hand-written expectation would also have to be kept in step
/// with the fallback list it is not the owner of.
#[test]
#[cfg(unix)]
fn concurrent_spawns_each_receive_the_same_complete_path() {
    const THREADS: usize = 16;
    const PER_THREAD: usize = 12;

    let quiet = child_path(&[]);
    assert!(!quiet.is_empty(), "uncontended child reported no PATH");

    let observed: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                scope.spawn(|| {
                    (0..PER_THREAD)
                        .map(|_| child_path(&[]))
                        .collect::<Vec<String>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("worker panicked"))
            .collect()
    });

    assert_eq!(observed.len(), THREADS * PER_THREAD);
    for (index, seen) in observed.iter().enumerate() {
        assert_eq!(
            seen,
            &quiet,
            "child {index} of {} saw a different PATH under contention",
            observed.len()
        );
    }
}

/// A caller that chose its own `PATH` keeps it under contention too: the
/// default must never win a race against an explicit decision.
///
/// Each thread picks a distinguishable value, so a default that overwrote one
/// and a thread that read another thread's value are two different failures
/// and neither can be mistaken for a pass.
#[test]
#[cfg(unix)]
fn an_explicit_path_survives_contention() {
    const THREADS: usize = 12;
    std::thread::scope(|scope| {
        for index in 0..THREADS {
            scope.spawn(move || {
                let chosen = format!("/caller/{index}");
                assert_eq!(
                    child_path(&[("PATH", chosen.as_str())]),
                    chosen,
                    "an explicitly chosen PATH was replaced or crossed threads"
                );
            });
        }
    });
}
