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

/// Free GiB at tick `i` (2 s each) of a one-off step: `lost` GiB over
/// `ticks` ticks from tick `begin`, flat before and after.
fn stepped(i: u64, from: u64, begin: u64, ticks: u64, lost: u64) -> u64 {
    let done = match i {
        i if i < begin => 0.0,
        i if i >= begin + ticks => lost as f64,
        i => (i - begin + 1) as f64 * lost as f64 / ticks as f64,
    };
    ((from as f64 - done) * GIB as f64) as u64
}

#[test]
fn a_one_off_step_never_reads_as_a_fall_wherever_it_sits_in_the_window() {
    // A step of h GiB fitted over the 180 s window reads about 1.5 h / 3 min.
    // The long window must fall in every third, so a step under a minute long
    // leaves a flat third and the rate is zero at every tick after it.
    for (lost, ticks) in [(12, 15), (20, 5), (8, 15), (30, 3)] {
        let mut t = FillTracker::default();
        let mut worst = 0.0f64;
        for i in 0..400u64 {
            t.observe(
                1,
                i * TICK_MS,
                Reading::Value(stepped(i, 200, 100, ticks, lost)),
            );
            if let Reading::Value(rate) = t.rate(i * TICK_MS) {
                worst = worst.max(rate as f64 / GIB as f64);
            }
        }
        assert_eq!(
            worst,
            0.0,
            "{lost} GiB over {} s read as a fall of {worst:.2} GiB/min",
            ticks * 2
        );
    }
}

#[test]
fn a_staircase_and_a_sustained_fall_both_read_as_falling() {
    let mut t = FillTracker::default();
    // 5 GiB lost at the top of every minute for six minutes. A step that lands
    // on the edge of a third leaves that third flat for a few ticks, so the
    // rate dips to zero then (the governor's hysteresis rides through it); the
    // rest of the time it reads as the fall it is, at most a step's worst
    // case (1.5 x 5 GiB/min).
    let mut seen = Vec::new();
    for i in 0..180u64 {
        let free = (200 - 5 * (i * 2 / 60)) * GIB;
        t.observe(1, i * TICK_MS, Reading::Value(free));
        if i >= 120 {
            seen.push(rate_in_gib_per_min(t.rate(i * TICK_MS)));
        }
    }
    let falling = seen.iter().filter(|r| **r > 0.25).count();
    assert!(
        falling * 10 >= seen.len() * 8,
        "{falling} of {}: {seen:?}",
        seen.len()
    );
    assert!(seen.iter().all(|r| *r < 8.0), "{seen:?}");
}

#[test]
fn a_fall_that_starts_after_a_long_quiet_spell_is_read_within_a_few_minutes() {
    let mut t = FillTracker::default();
    let mut first_seen = None;
    for i in 0..600u64 {
        let minutes_in = i.saturating_sub(300) as f64 * 2.0 / 60.0;
        let free = ((150.0 - 10.0 * minutes_in) * GIB as f64) as u64;
        t.observe(1, i * TICK_MS, Reading::Value(free));
        if first_seen.is_none() && t.rate(i * TICK_MS).value().unwrap_or(0) > GIB {
            first_seen = Some(i - 300);
        }
    }
    let ticks = first_seen.expect("a 10 GiB a minute fall must be read");
    assert!(ticks * 2 <= 180, "read after {} s", ticks * 2);
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
