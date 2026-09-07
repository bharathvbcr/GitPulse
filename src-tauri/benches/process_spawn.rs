//! What making every child killable costs per spawn.
//!
//! Run with `cargo bench --bench process_spawn`. Deliberately not Criterion,
//! for the reasons `mcp_protocol.rs` gives; every row reports p50 and p95
//! because the interesting failure here is a tail, not a mean.
//!
//! `src/procguard` puts each child in a process group of its own so a signal
//! or a `process::exit` can take it down. There are two ways to ask for that,
//! and they are not equally priced:
//!
//! * `CommandExt::pre_exec` running `setpgid(0, 0)` by hand. Any `pre_exec`
//!   closure disqualifies the command from the standard library's
//!   `posix_spawn` fast path, so the child is produced by `fork` + `exec` of a
//!   process holding the parent's whole address space.
//! * `CommandExt::process_group(0)`, which the standard library forwards to
//!   `posix_spawnattr_setpgroup` and keeps the fast path.
//!
//! Same syscall, same result. This measures the difference, because it is paid
//! on every `git` GitPulse runs — and `run_bounded_capped` is the single seam
//! all of them go through. Measured on an M-series Mac across four runs,
//! `process_group(0)` sat within noise of a plain spawn (-26% to +4%, which is
//! this machine's spread) while `pre_exec` was slower every single time, by
//! +35% to +95%.
//!
//! `/usr/bin/true` is the subject: the point is to measure the spawn, not the
//! program.

// Everything below the doc comment measures process groups, which only exist
// on Unix; the `not(unix)` main prints why there is nothing to measure and
// touches none of it. Ungated, all of it is dead code on Windows and fails
// clippy's `-D warnings` there.
#[cfg(unix)]
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
const SAMPLES: usize = 200;
#[cfg(unix)]
const WARMUP: usize = 20;

#[cfg(unix)]
struct Row {
    name: &'static str,
    p50: Duration,
    p95: Duration,
    worst: Duration,
}

#[cfg(unix)]
fn trivial() -> Command {
    let mut cmd = Command::new("/usr/bin/true");
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
}

/// One spawn plus its `wait`, so nothing accumulates as a zombie between
/// samples and every row measures the same amount of work.
#[cfg(unix)]
fn spawn_and_reap(cmd: &mut Command) {
    let mut child = cmd.spawn().expect("spawn");
    let _ = child.wait();
}

#[cfg(unix)]
fn measure(name: &'static str, mut build: impl FnMut() -> Command) -> Row {
    for _ in 0..WARMUP {
        spawn_and_reap(&mut build());
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let mut cmd = build();
        let started = Instant::now();
        spawn_and_reap(&mut cmd);
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    Row {
        name,
        p50: samples[samples.len() / 2],
        p95: samples[samples.len() * 95 / 100],
        worst: *samples.last().expect("samples"),
    }
}

#[cfg(unix)]
fn main() {
    use std::os::unix::process::CommandExt;

    let rows = vec![
        measure("plain spawn (no process group)", trivial),
        measure("process_group(0)  [what procguard does]", || {
            let mut cmd = trivial();
            cmd.process_group(0);
            cmd
        }),
        measure("pre_exec setpgid  [the slow spelling]", || {
            let mut cmd = trivial();
            // SAFETY: the closure calls only `setpgid`, which is
            // async-signal-safe, and touches no shared memory.
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setpgid(0, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            cmd
        }),
    ];

    println!("\n{SAMPLES} samples per row, {WARMUP} warmup\n");
    println!("{:<42}  {:>10}  {:>10}  {:>10}", "", "p50", "p95", "worst");
    let baseline = rows[0].p50;
    for row in &rows {
        println!(
            "{:<42}  {:>10.3?}  {:>10.3?}  {:>10.3?}   {:+.1}% vs plain",
            row.name,
            row.p50,
            row.p95,
            row.worst,
            (row.p50.as_secs_f64() / baseline.as_secs_f64() - 1.0) * 100.0
        );
    }
    println!();
}

#[cfg(not(unix))]
fn main() {
    // Windows has no process groups to ask for, so there is nothing here to
    // compare — and printing a row anyway would read like a measurement.
    println!("process groups are a Unix concept; nothing to measure on this platform");
}
