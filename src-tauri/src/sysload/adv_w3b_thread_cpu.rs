//! Adversary tests for lane W3b item 2: can the CPU-bounded tests pass
//! vacuously on a host that has the clock, and is `cpu_since` right?
use super::{clock_or_skip, cpu_since, try_thread_cpu};
use std::time::{Duration, Instant};

/// A skip taken on a host that has the clock would turn four timing tests into
/// silent passes. macOS and Linux have the clock, so on them the reading must
/// be there, on the test thread and on a fresh thread.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn held_the_clock_is_there_on_macos_and_linux_for_every_thread() {
    assert!(try_thread_cpu().is_some());
    assert!(clock_or_skip("adv", try_thread_cpu()).is_some());
    let fresh = std::thread::spawn(|| clock_or_skip("adv", try_thread_cpu()))
        .join()
        .unwrap();
    assert!(fresh.is_some(), "a fresh thread has no clock reading");
}

/// The reading is CPU, not wall: a sleeping thread spends none, a spinning one
/// spends about what it ran. A clock that silently read wall time would make
/// every bound loose on a loaded host and tight on an idle one.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn held_the_reading_is_cpu_time_not_wall_time() {
    let Some(start) = clock_or_skip(
        "held_the_reading_is_cpu_time_not_wall_time",
        try_thread_cpu(),
    ) else {
        return;
    };
    std::thread::sleep(Duration::from_millis(300));
    let slept = cpu_since(start);
    assert!(
        slept < Duration::from_millis(100),
        "a sleeping thread used {slept:?} of CPU"
    );
    let wall = Instant::now();
    let mut x = 0u64;
    while wall.elapsed() < Duration::from_millis(120) {
        x = std::hint::black_box(x.wrapping_add(1));
    }
    let spun = cpu_since(start) - slept;
    assert!(
        spun >= Duration::from_millis(60),
        "a spinning thread used only {spun:?} of CPU ({x})"
    );
}
