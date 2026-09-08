//! Isolated end-to-end timing contract for the production bounded-process seam.
//!
//! This lives in its own integration-test executable because the library suite
//! runs hundreds of tests concurrently. Those tests share production's spawn
//! admission gate while the bare comparison bypasses it, so colocating this
//! measurement made unrelated gate contention affect only one side.

use gitpulse_lib::engine::git_cli::capture_command;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn no_op_program() -> (&'static str, &'static [&'static str]) {
    if cfg!(windows) {
        ("cmd", &["/C", "exit", "0"])
    } else {
        ("true", &[])
    }
}

/// Spawn, reap, and nothing else: the floor the bounded runner may approach
/// but must not add a fixed sleep on top of.
fn bare() -> Duration {
    let started = Instant::now();
    let (program, args) = no_op_program();
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawning the no-op should succeed");
    let status = child.wait().expect("waiting on the no-op should succeed");
    assert!(status.success());
    started.elapsed()
}

fn through_bounded_runner() -> Duration {
    let started = Instant::now();
    let (program, args) = no_op_program();
    let run = capture_command(program, args, None, Duration::from_secs(5), &[])
        .expect("spawning the no-op should succeed");
    assert!(run.success);
    started.elapsed()
}

/// End-to-end rather than schedule-only: the backoff constants are only worth
/// anything if the production bounded runner actually uses them.
///
/// The earlier absolute `< 200 ms` assertion could not detect a 15 ms fixed
/// poll. This compares against the same machine's bare spawn and reap. Keeping
/// it in a separate executable removes concurrent library tests that enter the
/// production spawn gate but do not delay the bare side.
#[test]
fn a_fast_command_is_not_delayed_by_a_fixed_poll_quantum() {
    // Best-of-N on both sides, sampled alternately so warm-up and scheduler
    // drift land on each equally. The minimum selects the sample least
    // contaminated by unrelated host work.
    const SAMPLES: usize = 12;
    let mut bare_best = Duration::MAX;
    let mut bounded_best = Duration::MAX;
    for i in 0..SAMPLES {
        if i % 2 == 0 {
            bare_best = bare_best.min(bare());
            bounded_best = bounded_best.min(through_bounded_runner());
        } else {
            bounded_best = bounded_best.min(through_bounded_runner());
            bare_best = bare_best.min(bare());
        }
    }

    // Comfortably above the ordinary gate, thread, and channel work, and
    // comfortably below the flat 15 ms poll this exists to catch.
    const SLACK: Duration = Duration::from_millis(8);
    assert!(
        bounded_best <= bare_best + SLACK,
        "bounded runner added {:?} on top of a bare spawn+reap ({bare_best:?} -> \
         {bounded_best:?}); a flat 15ms poll would add about that much, so the \
         backoff ramp is not being used",
        bounded_best.saturating_sub(bare_best)
    );
}
