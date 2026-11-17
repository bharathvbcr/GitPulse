use super::*;

const GIB: u64 = 1 << 30;
const TICK_MS: u64 = 2_000;

fn gib_per_min(r: Reading<u64>) -> f64 {
    r.value().expect("a value") as f64 / GIB as f64
}

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

/// Free GiB at `seconds` of a pattern that loses `size` GiB over `dur` seconds
/// at the start of every `every` seconds, from `from` GiB, beginning at `at`.
fn lumps(seconds: u64, from: f64, size: f64, dur: u64, every: u64, at: u64) -> u64 {
    let Some(t) = seconds.checked_sub(at) else {
        return (from * GIB as f64) as u64;
    };
    let (done, into) = (t / every, (t % every).min(dur));
    let lost = done as f64 * size + size * into as f64 / dur as f64;
    ((from - lost) * GIB as f64) as u64
}

/// The two rates at every tick of `seconds` of a pattern, as GiB/min.
fn run(seconds: u64, free_at: impl Fn(u64) -> u64, mut at_tick: impl FnMut(u64, f64, f64)) {
    let mut t = FillTracker::default();
    for i in 0..seconds / 2 {
        let s = i * 2;
        t.observe(1, s * 1000, Reading::Value(free_at(s)));
        let rates = t.rates(s * 1000);
        at_tick(
            s,
            rates.quick.value().map_or(0.0, |r| r as f64 / GIB as f64),
            rates
                .sustained
                .value()
                .map_or(0.0, |r| r as f64 / GIB as f64),
        );
    }
}

#[test]
fn a_steady_fall_reads_its_rate_once_the_history_covers_the_long_window() {
    let mut t = FillTracker::default();
    let now = feed(&mut t, 1, 0, 99 * GIB, -(2.7 * GIB as f64) as i64, 70);
    let rates = t.rates(now);
    assert!((gib_per_min(rates.quick) - 2.7).abs() < 0.05, "{rates:?}");
    // After 138 s the net fall is 6.2 GiB, over the five-minute floor of the
    // divisor: the sustained rate is the leave-one-burst-out one, which is exact.
    assert!(
        (gib_per_min(rates.sustained) - 2.7).abs() < 0.3,
        "{rates:?}"
    );
}

#[test]
fn too_few_samples_or_too_short_a_span_is_not_yet_never_a_rate() {
    let mut t = FillTracker::default();
    assert_eq!(
        t.rates(0),
        FillRates::both(Reading::Unavailable(Why::NotYet))
    );
    let now = feed(&mut t, 1, 0, 99 * GIB, -(5 * GIB as i64), 30);
    // 58 s of history: the long window refuses to speak.
    assert_eq!(
        t.rates(now),
        FillRates::both(Reading::Unavailable(Why::NotYet))
    );
}

#[test]
fn a_flat_or_rising_disk_reads_zero() {
    let mut t = FillTracker::default();
    let now = feed(&mut t, 1, 0, 99 * GIB, 0, 90);
    assert_eq!(t.rates(now), FillRates::both(Reading::Value(0)));
    let mut t = FillTracker::default();
    let now = feed(&mut t, 1, 0, 50 * GIB, GIB as i64, 90);
    assert_eq!(t.rates(now), FillRates::both(Reading::Value(0)));
}

#[test]
fn a_disk_that_cannot_be_read_says_so_from_the_first_tick() {
    // Never readable: "can't be read", not "warming up" forever.
    let mut t = FillTracker::default();
    t.observe(1, 0, Reading::Unavailable(Why::Failed));
    assert_eq!(
        t.rates(0),
        FillRates::both(Reading::Unavailable(Why::Failed))
    );
    t.observe(1, 2_000, Reading::Unavailable(Why::Unreadable));
    assert_eq!(
        t.rates(2_000),
        FillRates::both(Reading::Unavailable(Why::Failed))
    );
    // A platform with no reader says that, as its own state.
    let mut t = FillTracker::default();
    t.observe(1, 0, Reading::Unavailable(Why::Unsupported));
    assert_eq!(
        t.rates(0),
        FillRates::both(Reading::Unavailable(Why::Unsupported))
    );
    // Once it reads, it is history that is missing, not the disk.
    t.observe(1, 2_000, Reading::Value(50 * GIB));
    assert_eq!(
        t.rates(2_000),
        FillRates::both(Reading::Unavailable(Why::NotYet))
    );
}

#[test]
fn a_one_off_burst_never_reads_as_a_sustained_fall_beyond_what_it_took_over_five_minutes() {
    // The sustained rate decides Severe. A step of h GiB in a minute or less
    // may read as at most h over the five-minute floor of its divisor: one
    // burst can refuse only by eating half of what is left above the floor.
    for (from, lost, dur) in [(200.0, 12.0, 30), (200.0, 20.0, 10), (200.0, 20.0, 60)] {
        for begin in [30, 630] {
            let mut worst = 0.0f64;
            run(
                1_500,
                |s| lumps(s, from, lost, dur, 100_000, begin),
                |_, _, sustained| worst = worst.max(sustained),
            );
            let ceiling = lost / 5.0;
            assert!(
                worst <= ceiling * 1.02,
                "{lost} GiB over {dur} s from second {begin} read as {worst:.2} GiB/min, over {ceiling:.2}"
            );
        }
    }
}

#[test]
fn the_quick_rate_may_follow_a_burst_but_lets_go_of_it_within_the_long_window() {
    let mut last_quick_at = 0;
    run(
        900,
        |s| lumps(s, 200.0, 12.0, 30, 1_000_000, 100),
        |s, quick, _| {
            if quick > 0.0 {
                last_quick_at = s;
            }
        },
    );
    // The burst ends at 130 s; the three-minute window forgets it by 310 s.
    assert!(last_quick_at <= 320, "{last_quick_at}");
}

#[test]
fn a_pause_does_not_hide_a_fall_that_goes_on() {
    // Each of these is a build that writes in lumps. Every tick after the
    // history has built up must read it: no pause length in the pattern may
    // zero the sustained rate.
    for (size, dur, every, average) in [
        (5.0, 1, 90, 5.0 / 1.5),
        (5.0, 1, 80, 3.75),
        (8.0 * 40.0 / 60.0, 40, 120, 8.0 * 40.0 / 120.0),
        (12.0, 30, 90, 8.0),
    ] {
        let mut least = f64::MAX;
        let mut most = 0.0f64;
        run(
            1_800,
            |s| lumps(s, 300.0, size, dur, every, 30),
            |s, _, sustained| {
                if s >= 700 {
                    least = least.min(sustained);
                    most = most.max(sustained);
                }
            },
        );
        assert!(
            least > average * 0.5 && most < average * 1.6,
            "{size} GiB over {dur} s every {every} s, average {average:.2}: read {least:.2} .. {most:.2}"
        );
    }
}

#[test]
fn a_fall_that_starts_after_a_long_quiet_spell_is_read_within_a_few_minutes() {
    let mut t = FillTracker::default();
    let mut first_seen = None;
    for i in 0..600u64 {
        let minutes_in = i.saturating_sub(300) as f64 * 2.0 / 60.0;
        let free = ((150.0 - 10.0 * minutes_in) * GIB as f64) as u64;
        t.observe(1, i * TICK_MS, Reading::Value(free));
        if first_seen.is_none() && t.rates(i * TICK_MS).sustained.value().unwrap_or(0) > GIB {
            first_seen = Some(i - 300);
        }
    }
    let ticks = first_seen.expect("a 10 GiB a minute fall must be read");
    assert!(ticks * 2 <= 150, "read after {} s", ticks * 2);
}

#[test]
fn nine_second_ticks_still_measure_through_the_long_window() {
    // The short window cannot hold eight samples at this cadence, so the
    // fitted rate is not there, but a fall is still read, never "not falling".
    let mut t = FillTracker::default();
    let mut at = 0;
    for i in 0..60u64 {
        at = i * 9_000;
        let free = ((200.0 - 5.0 * at as f64 / 60_000.0) * GIB as f64) as u64;
        t.observe(1, at, Reading::Value(free));
    }
    let rates = t.rates(at);
    assert!((gib_per_min(rates.quick) - 5.0).abs() < 0.5, "{rates:?}");
    assert!(
        (gib_per_min(rates.sustained) - 5.0).abs() < 0.5,
        "{rates:?}"
    );
}

#[test]
fn a_sampler_that_stalls_over_ten_seconds_every_two_minutes_is_not_measured_never_not_falling() {
    // Every gap starts the history over, so 120 s never accumulates: the rates
    // are `NotYet`, which Health says as "warming up", and never `Value(0)`.
    let mut t = FillTracker::default();
    for s in (0..1_200u64).step_by(2) {
        if s % 120 < 12 && s > 0 {
            continue;
        }
        let free = ((300.0 - 10.0 * s as f64 / 60.0) * GIB as f64) as u64;
        t.observe(1, s * 1000, Reading::Value(free));
        assert_ne!(
            t.rates(s * 1000).quick,
            Reading::Value(0),
            "second {s} read as not falling"
        );
    }
}

#[test]
fn a_gap_a_new_boot_or_an_impossible_step_start_over() {
    let mut t = FillTracker::default();
    let now = feed(&mut t, 1, 0, 99 * GIB, -(2 * GIB as i64), 70);
    assert!(t.rates(now).quick.value().is_some());

    // The sampler paused for a minute.
    t.observe(1, now + 60_000, Reading::Value(90 * GIB));
    assert_eq!(
        t.rates(now + 60_000),
        FillRates::both(Reading::Unavailable(Why::NotYet))
    );

    // A new boot.
    let mut t = FillTracker::default();
    let now = feed(&mut t, 1, 0, 99 * GIB, -(2 * GIB as i64), 70);
    t.observe(2, now + TICK_MS, Reading::Value(90 * GIB));
    assert_eq!(
        t.rates(now + TICK_MS),
        FillRates::both(Reading::Unavailable(Why::NotYet))
    );

    // A volume remounted: 100 GiB between two ticks.
    let mut t = FillTracker::default();
    let now = feed(&mut t, 1, 0, 99 * GIB, -(2 * GIB as i64), 70);
    t.observe(1, now + TICK_MS, Reading::Value(300 * GIB));
    assert_eq!(
        t.rates(now + TICK_MS),
        FillRates::both(Reading::Unavailable(Why::NotYet))
    );

    // A clock that stepped back.
    let mut t = FillTracker::default();
    let now = feed(&mut t, 1, 0, 99 * GIB, -(2 * GIB as i64), 70);
    t.observe(1, now - 10_000, Reading::Value(90 * GIB));
    assert_eq!(
        t.rates(now - 10_000),
        FillRates::both(Reading::Unavailable(Why::NotYet))
    );
}

#[test]
fn an_unreadable_disk_adds_nothing_and_then_reads_failed() {
    let mut t = FillTracker::default();
    let now = feed(&mut t, 1, 0, 99 * GIB, -(2 * GIB as i64), 70);
    t.observe(1, now + TICK_MS, Reading::Unavailable(Why::Failed));
    assert!(
        t.rates(now + TICK_MS).quick.value().is_some(),
        "one miss is not a gap"
    );
    assert_eq!(
        t.rates(now + 30_000),
        FillRates::both(Reading::Unavailable(Why::Failed))
    );
}

#[test]
fn a_recovery_unbinds_at_once() {
    let mut t = FillTracker::default();
    let now = feed(&mut t, 1, 0, 99 * GIB, -(2 * GIB as i64), 70);
    assert!(t.rates(now).quick.value().unwrap() > 0);
    // 45 GiB to 167 GiB when the output is deleted: a step no tick can make.
    t.observe(1, now + TICK_MS, Reading::Value(167 * GIB));
    assert_eq!(
        t.rates(now + TICK_MS),
        FillRates::both(Reading::Unavailable(Why::NotYet)),
        "the old fall must not survive the recovery"
    );
}

#[test]
fn history_is_bounded() {
    let mut t = FillTracker::default();
    feed(&mut t, 1, 0, 500 * GIB, -(GIB as i64) / 10, 5_000);
    assert!(t.samples.len() <= CAPACITY);
    // Ten minutes at one every ten seconds, and the edge.
    assert!(t.coarse.len() <= 62, "{}", t.coarse.len());
}
