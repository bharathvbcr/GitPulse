use super::*;

const GIB: u64 = 1 << 30;
const TICK_MS: u64 = 2_000;

/// Feeds `ticks` samples at the 2 s cadence starting at `start_ms`, free space
/// moving by `per_min` bytes a minute (negative falls) from `from`.
fn feed(t: &mut FillTracker, boot: u64, start_ms: u64, from: u64, per_min: i64, ticks: u64) -> u64 {
    let mut at = start_ms;
    for i in 0..ticks {
        let delta = per_min as f64 * (i * TICK_MS) as f64 / 60_000.0;
        let free = (from as f64 + delta).max(0.0) as u64;
        t.observe(boot, at, Reading::Value(free));
        at += TICK_MS;
    }
    at - TICK_MS
}

fn rate_in_gib_per_min(r: Reading<u64>) -> f64 {
    r.value().expect("a value") as f64 / GIB as f64
}

#[test]
fn a_steady_fall_reads_its_rate_once_the_history_covers_the_long_window() {
    let mut t = FillTracker::default();
    let now = feed(&mut t, 1, 0, 99 * GIB, -(2.7 * GIB as f64) as i64, 70);
    let rate = rate_in_gib_per_min(t.rate(now));
    assert!((rate - 2.7).abs() < 0.05, "{rate}");
}

#[test]
fn too_few_samples_or_too_short_a_span_is_not_yet_never_a_rate() {
    let mut t = FillTracker::default();
    assert_eq!(t.rate(0), Reading::Unavailable(Why::NotYet));
    let now = feed(&mut t, 1, 0, 99 * GIB, -(5 * GIB as i64), 30);
    // 58 s of history: the long window refuses to speak.
    assert_eq!(t.rate(now), Reading::Unavailable(Why::NotYet));
}

#[test]
fn a_flat_or_rising_disk_reads_zero() {
    let mut t = FillTracker::default();
    let now = feed(&mut t, 1, 0, 99 * GIB, 0, 90);
    assert_eq!(t.rate(now), Reading::Value(0));
    let mut t = FillTracker::default();
    let now = feed(&mut t, 1, 0, 50 * GIB, GIB as i64, 90);
    assert_eq!(t.rate(now), Reading::Value(0));
}

#[test]
fn a_short_burst_does_not_read_as_a_sustained_fall() {
    let mut t = FillTracker::default();
    // Flat for 150 s, then 10 GiB lost over 20 s, then flat again.
    let mut at = feed(&mut t, 1, 0, 160 * GIB, 0, 75);
    let mut free = 160 * GIB;
    for _ in 0..10 {
        at += TICK_MS;
        free -= GIB;
        t.observe(1, at, Reading::Value(free));
    }
    let burst = rate_in_gib_per_min(t.rate(at));
    // 10 GiB over 3 minutes is at most a few GiB a minute, not the 30 GiB a
    // minute the short window alone would say.
    assert!(burst < 4.0, "{burst}");
}

#[test]
fn a_gap_a_new_boot_or_an_impossible_step_start_over() {
    let mut t = FillTracker::default();
    let now = feed(&mut t, 1, 0, 99 * GIB, -(2 * GIB as i64), 70);
    assert!(t.rate(now).value().is_some());

    // The sampler paused for a minute.
    t.observe(1, now + 60_000, Reading::Value(90 * GIB));
    assert_eq!(t.rate(now + 60_000), Reading::Unavailable(Why::NotYet));

    // A new boot.
    let mut t = FillTracker::default();
    let now = feed(&mut t, 1, 0, 99 * GIB, -(2 * GIB as i64), 70);
    t.observe(2, now + TICK_MS, Reading::Value(90 * GIB));
    assert_eq!(t.rate(now + TICK_MS), Reading::Unavailable(Why::NotYet));

    // A volume remounted: 100 GiB between two ticks.
    let mut t = FillTracker::default();
    let now = feed(&mut t, 1, 0, 99 * GIB, -(2 * GIB as i64), 70);
    t.observe(1, now + TICK_MS, Reading::Value(300 * GIB));
    assert_eq!(t.rate(now + TICK_MS), Reading::Unavailable(Why::NotYet));

    // A clock that stepped back.
    let mut t = FillTracker::default();
    let now = feed(&mut t, 1, 0, 99 * GIB, -(2 * GIB as i64), 70);
    t.observe(1, now - 10_000, Reading::Value(90 * GIB));
    assert_eq!(t.rate(now - 10_000), Reading::Unavailable(Why::NotYet));
}

#[test]
fn an_unreadable_disk_adds_nothing_and_then_reads_failed() {
    let mut t = FillTracker::default();
    let now = feed(&mut t, 1, 0, 99 * GIB, -(2 * GIB as i64), 70);
    t.observe(1, now + TICK_MS, Reading::Unavailable(Why::Failed));
    assert!(
        t.rate(now + TICK_MS).value().is_some(),
        "one miss is not a gap"
    );
    assert_eq!(t.rate(now + 30_000), Reading::Unavailable(Why::Failed));
}

#[test]
fn a_recovery_unbinds_at_once() {
    let mut t = FillTracker::default();
    let now = feed(&mut t, 1, 0, 99 * GIB, -(2 * GIB as i64), 70);
    assert!(t.rate(now).value().unwrap() > 0);
    // 45 GiB to 167 GiB when the output is deleted: a step no tick can make.
    t.observe(1, now + TICK_MS, Reading::Value(167 * GIB));
    assert_ne!(
        t.rate(now + TICK_MS).value().unwrap_or(0),
        2 * GIB,
        "the old fall must not survive the recovery"
    );
    assert_eq!(t.rate(now + TICK_MS), Reading::Unavailable(Why::NotYet));
}

#[test]
fn history_is_bounded() {
    let mut t = FillTracker::default();
    feed(&mut t, 1, 0, 500 * GIB, -(GIB as i64) / 10, 5_000);
    assert!(t.samples.len() <= CAPACITY);
}
