//! Adversarial bench for lane W4b: what a background `fork` costs the NEXT
//! foreground spawn, and how the background spawn scales with the parent's
//! resident size.
//!
//! `cargo bench --bench adv_w4b_fork_aftermath` (macOS only; the rest prints why
//! there is nothing to measure). `ADV_RESIDENT_MIB` sets the block the parent
//! holds (default 128).
//!
//! Rounds are interleaved, one of each per round, so drift hits every series
//! alike. Every series times only the `procguard::spawn` call of a foreground
//! `/usr/bin/true` (what holds the process-wide inheritance lock):
//!
//! * `fg after fg`: a foreground spawn right after a foreground spawn.
//! * `fg after bg`: a foreground spawn right after a background child (a `fork`
//!   of this process) was spawned and reaped.
//! * `fg after bg, 5 ms later`: the same with a pause between them, to see
//!   whether the cost is a transient of the fork.
//! * `bg`: the background spawn call itself.

#[cfg(target_os = "macos")]
fn main() {
    use gitpulse_lib::procguard;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const ROUNDS: usize = 400;
    const WARMUP: usize = 30;
    let mib: usize = std::env::var("ADV_RESIDENT_MIB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(128);
    let resident = vec![7u8; mib * 1024 * 1024];
    std::hint::black_box(&resident);

    let trivial = || {
        let mut cmd = Command::new("/usr/bin/true");
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    };
    let spawn_timed = |background: bool| -> Duration {
        let mut cmd = trivial();
        let started = Instant::now();
        let (mut child, registration) = if background {
            procguard::spawn_background(&mut cmd, "bench")
        } else {
            procguard::spawn(&mut cmd, "bench")
        }
        .expect("spawn");
        let spawned = started.elapsed();
        let _ = registration.reap(|| child.wait());
        spawned
    };

    let mut fg_after_fg = Vec::with_capacity(ROUNDS);
    let mut fg_after_bg = Vec::with_capacity(ROUNDS);
    let mut fg_after_bg_later = Vec::with_capacity(ROUNDS);
    let mut bg = Vec::with_capacity(ROUNDS);
    for round in 0..ROUNDS + WARMUP {
        spawn_timed(false);
        let a = spawn_timed(false);
        let b_spawn = spawn_timed(true);
        let b = spawn_timed(false);
        spawn_timed(true);
        std::thread::sleep(Duration::from_millis(5));
        let c = spawn_timed(false);
        if round >= WARMUP {
            fg_after_fg.push(a);
            bg.push(b_spawn);
            fg_after_bg.push(b);
            fg_after_bg_later.push(c);
        }
    }
    let figures = |mut v: Vec<Duration>| {
        v.sort_unstable();
        (v[0], v[v.len() / 2], v[v.len() * 95 / 100])
    };
    println!(
        "\nforeground spawn call after a background fork, {ROUNDS} interleaved rounds, {mib} MiB held\n"
    );
    println!("{:<34} {:>10} {:>10} {:>10}", "", "min", "p50", "p95");
    for (name, series) in [
        ("fg after fg", fg_after_fg),
        ("fg after bg", fg_after_bg),
        ("fg after bg, 5 ms later", fg_after_bg_later),
        ("bg spawn call", bg),
    ] {
        let (min, p50, p95) = figures(series);
        println!("{name:<34} {min:>10.3?} {p50:>10.3?} {p95:>10.3?}");
    }
    std::hint::black_box(&resident);
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("the background band exists on macOS only; nothing to measure here");
}
