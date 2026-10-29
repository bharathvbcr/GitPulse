//! Adversarial bench for lane W4b: what a background `fork` costs the NEXT
//! foreground spawn, and how the background spawn scales with the parent's
//! resident size.
//!
//! `cargo bench --bench adv_w4b_fork_aftermath` (macOS only; the rest prints why
//! there is nothing to measure). `ADV_RESIDENT_MIB` sets the block the parent
//! holds (default 128); `ADV_VM_ENTRIES` adds that many unmergeable regions;
//! `ADV_NO_BG` makes every "background" spawn a plain
//! foreground one, the control for whether the fork is what slows the rest.
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
    // With `ADV_NO_BG` set the background spawns are replaced by foreground
    // ones: the same binary and the same schedule of spawns, with no fork.
    let no_bg = std::env::var_os("ADV_NO_BG").is_some();
    let resident = vec![7u8; mib * 1024 * 1024];
    std::hint::black_box(&resident);
    // `ADV_VM_ENTRIES` separate 16 KiB regions, each followed by a PROT_NONE
    // guard page so the kernel cannot merge them: `fork` copies the address
    // space entry by entry, and a GUI process has thousands (frameworks,
    // allocator zones, thread stacks) where one big block is a single entry.
    let entries: usize = std::env::var("ADV_VM_ENTRIES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut fragments = Vec::with_capacity(entries);
    for _ in 0..entries {
        // SAFETY: anonymous private mappings, never unmapped before exit; the
        // first page of each is written so it is resident.
        unsafe {
            let size = 5 * 4096;
            let p = libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANON | libc::MAP_PRIVATE,
                -1,
                0,
            );
            assert_ne!(p, libc::MAP_FAILED);
            std::ptr::write_volatile(p.cast::<u8>(), 1);
            libc::mprotect(p.cast::<u8>().add(4 * 4096).cast(), 4096, libc::PROT_NONE);
            fragments.push(p as usize);
        }
    }
    std::hint::black_box(&fragments);

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
        let (mut child, registration) = if background && !no_bg {
            procguard::spawn_background(&mut cmd, "bench")
        } else {
            procguard::spawn(&mut cmd, "bench")
        }
        .expect("spawn");
        let spawned = started.elapsed();
        let _ = registration.reap(|| child.wait());
        spawned
    };

    // `ADV_CONTEND=n`: n threads spawn background children back to back while
    // this thread times the whole `procguard::spawn` call of a foreground
    // child (lock wait included), the latency a user action sees when it
    // starts a git command beside background work.
    if let Some(threads) = std::env::var("ADV_CONTEND")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        let stop = std::sync::atomic::AtomicBool::new(false);
        let mut waits = Vec::with_capacity(ROUNDS);
        let background_spawns = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(|| {
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        spawn_timed(true);
                        background_spawns.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                });
            }
            std::thread::sleep(Duration::from_millis(200));
            for _ in 0..ROUNDS {
                waits.push(spawn_timed(false));
                std::thread::sleep(Duration::from_millis(2));
            }
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        waits.sort_unstable();
        println!(
            "\nforeground spawn call beside {threads} threads of background spawns, {} entries, {mib} MiB held\n\
             min {:.3?}  p50 {:.3?}  p95 {:.3?}  max {:.3?}  ({} background spawns completed)",
            entries,
            waits[0],
            waits[waits.len() / 2],
            waits[waits.len() * 95 / 100],
            waits[waits.len() - 1],
            background_spawns.load(std::sync::atomic::Ordering::Relaxed)
        );
        return;
    }
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
