//! The disk fill rule, over the real tracker and the real hysteresis: a replay
//! of an earlier incident, when free disk fell from 99 to 45 GiB in twenty
//! minutes under one coverage build and the governor, which looked only at the
//! level, said nothing, and of 2026-11-15, when it fell from about 150 GiB to
//! nothing in about fifteen.
//!
//! Everything here runs on a 64 GiB machine, so the automatic floors are 32 GiB
//! (elevated) and 16 GiB (severe).

use super::*;
use crate::sysload::diskrate::FillTracker;
use crate::sysload::sensor::Why;
use crate::sysload::thresholds::InputMode;
use crate::sysload::Reading;

const GIB: f64 = (1u64 << 30) as f64;
const TICK_MS: u64 = 2_000;
const BOOT: u64 = 7;

/// One replay: free space as a function of seconds since the start, sampled at
/// the sampler's 2 s tick, through the tracker and the governor.
struct Replay {
    tracker: FillTracker,
    governor: Governor,
    input: Input,
    at_ms: u64,
    now: Instant,
}

/// What one tick showed.
struct Seen {
    free_gib: f64,
    verdict: GovernorVerdict,
}

impl Replay {
    fn new() -> Self {
        Self {
            tracker: FillTracker::default(),
            governor: Governor::default(),
            input: Input::test_pressure(Level::Normal),
            at_ms: 0,
            now: Instant::now(),
        }
    }

    fn tick(&mut self, free: Reading<u64>) -> Seen {
        self.tracker.observe(BOOT, self.at_ms, free);
        self.input.readings.free_disk = free;
        self.input.readings.disk_fill = self.tracker.rate(self.at_ms);
        let verdict = self.governor.observe(self.now, self.input.clone()).verdict;
        self.at_ms += TICK_MS;
        self.now += Duration::from_millis(TICK_MS);
        Seen {
            free_gib: free.value().map_or(f64::NAN, |b| b as f64 / GIB),
            verdict,
        }
    }

    fn tick_gib(&mut self, gib: f64) -> Seen {
        self.tick(Reading::Value((gib * GIB) as u64))
    }
}

/// Free space at `seconds` into a fall of `from` GiB at `per_min` GiB a minute.
fn falling(from: f64, per_min: f64, seconds: u64) -> f64 {
    from - per_min * seconds as f64 / 60.0
}

#[test]
fn the_replay_binds_on_the_fall_long_before_the_level_would_and_says_why() {
    let mut replay = Replay::new();
    let mut first_elevated = None;
    let mut ever_severe = None;
    // 99 -> 45 GiB in 20 minutes: 2.7 GiB a minute, 600 ticks.
    for i in 0..=600u64 {
        let seen = replay.tick_gib(falling(99.0, 2.7, i * 2));
        if seen.verdict.tier >= Tier::Elevated && first_elevated.is_none() {
            first_elevated = Some((i * 2, seen.free_gib, seen.verdict.clone()));
        }
        if seen.verdict.tier == Tier::Severe && ever_severe.is_none() {
            ever_severe = Some((i * 2, seen.free_gib));
        }
    }
    let (second, free, verdict) = first_elevated.expect("the fall must bind");
    assert_eq!(verdict.binding, Some(Binding::DiskFilling), "{verdict:?}");
    assert_eq!(verdict.tier, Tier::Elevated);
    assert!(
        verdict.admit,
        "elevated sheds background work, it does not refuse"
    );
    assert!(verdict.shed.background && !verdict.shed.reactive);
    assert_eq!(verdict.shed.cause, Some(Binding::DiskFilling));
    // The level rule's own elevated floor is 32 GiB, which this replay never
    // reaches; the rate rule bound while 60 GiB were still free.
    assert!(free > 85.0, "bound at {free} GiB after {second} s");
    assert!(second <= 180, "bound after {second} s");
    for must in [
        "falling by 2.7 GiB a minute",
        "minutes until the 16 GiB floor",
        "30 minute horizon",
    ] {
        assert!(
            verdict.message.contains(must),
            "{must}: {}",
            verdict.message
        );
    }
    // At 45 GiB the projection is 10.7 minutes: elevated, not yet severe.
    assert_eq!(ever_severe, None, "severe at {ever_severe:?}");
}

#[test]
fn the_fall_continued_refuses_launches_before_free_space_reaches_40_gib() {
    let mut replay = Replay::new();
    let mut refused_at = None;
    let mut i = 0u64;
    loop {
        let free = falling(99.0, 2.7, i * 2);
        if free < 20.0 {
            break;
        }
        let seen = replay.tick_gib(free);
        if !seen.verdict.admit && refused_at.is_none() {
            refused_at = Some((i * 2, seen.free_gib, seen.verdict));
        }
        i += 1;
    }
    let (second, free, verdict) = refused_at.expect("launches must be refused before 20 GiB");
    assert_eq!(verdict.binding, Some(Binding::DiskFilling));
    assert_eq!(verdict.tier, Tier::Severe);
    assert!(verdict.shed.reactive);
    // Severe is a projection under ten minutes to 16 GiB: 43 GiB at 2.7/min,
    // plus the three ticks the hysteresis counts.
    assert!(
        free > 40.0,
        "launches were refused at {free:.1} GiB after {second} s; the rule must act before 40 GiB"
    );
}

#[test]
fn the_level_rule_alone_says_nothing_on_the_same_replay() {
    let mut replay = Replay::new();
    replay.input.thresholds.disk_guard.fill.mode = InputMode::Off;
    for i in 0..=600u64 {
        let seen = replay.tick_gib(falling(99.0, 2.7, i * 2));
        assert_eq!(seen.verdict.tier, Tier::Clear, "{} GiB", seen.free_gib);
    }
}

#[test]
fn deleting_the_output_unbinds_within_the_exit_samples() {
    let mut replay = Replay::new();
    let mut last = None;
    for i in 0..=678u64 {
        last = Some(replay.tick_gib(falling(99.0, 2.7, i * 2)));
    }
    let last = last.unwrap();
    assert!(last.free_gib < 38.5, "{}", last.free_gib);
    assert_eq!(last.verdict.tier, Tier::Severe);
    // 45 GiB -> 167 GiB when the build output is deleted.
    let mut seen = Vec::new();
    for _ in 0..(EXIT_SAMPLES + 1) {
        seen.push(replay.tick_gib(167.0));
    }
    let last = seen.last().unwrap();
    assert_eq!(last.verdict.tier, Tier::Clear, "{:?}", last.verdict);
    assert!(last.verdict.admit);
    assert_eq!(last.verdict.binding, None);
}

#[test]
fn an_ordinary_compile_burst_with_plenty_of_space_never_binds() {
    // 160 GiB falling at 2.7 GiB a minute is 53 minutes from a 16 GiB floor.
    let mut replay = Replay::new();
    for i in 0..=600u64 {
        let seen = replay.tick_gib(falling(160.0, 2.7, i * 2));
        assert_eq!(seen.verdict.tier, Tier::Clear, "{} GiB", seen.free_gib);
        assert_eq!(seen.verdict.binding, None);
    }
    // And a lumpy one: 4 GiB lost in a 30 s burst, then quiet.
    let mut replay = Replay::new();
    for i in 0..300u64 {
        let lost = lump(i, 4.0);
        let seen = replay.tick_gib(160.0 - lost);
        assert_eq!(
            seen.verdict.tier,
            Tier::Clear,
            "tick {i}: {:?}",
            seen.verdict
        );
    }
}

/// GiB lost by tick `i` of a burst that takes `total` GiB over ticks 100..115.
fn lump(i: u64, total: f64) -> f64 {
    match i {
        0..=99 => 0.0,
        100..=114 => (i - 99) as f64 * total / 15.0,
        _ => total,
    }
}

/// GiB lost by tick `i` of a one-off step: `total` GiB spread evenly over the
/// `ticks` ticks from tick `begin`, and nothing after.
fn step(i: u64, begin: u64, ticks: u64, total: f64) -> f64 {
    if i < begin {
        0.0
    } else if i >= begin + ticks {
        total
    } else {
        (i - begin + 1) as f64 * total / ticks as f64
    }
}

#[test]
fn a_one_off_step_followed_by_a_flat_line_never_binds_at_any_starting_level() {
    // Each was a review replay on this 64 GiB Mac (16 GiB severe floor): a
    // least-squares fit over the long window read a step of h GiB as a fall of
    // about 1.5 h / window, which refused launches for about 25 seconds.
    // (start free GiB, GiB lost, ticks the loss took: 15 ticks is 30 s)
    for (start, lost, ticks) in [
        (70.0, 12.0, 15),
        (100.0, 20.0, 5),
        (50.0, 8.0, 15),
        (160.0, 12.0, 15),
        (45.0, 6.0, 5),
    ] {
        let mut replay = Replay::new();
        for i in 0..450u64 {
            let seen = replay.tick_gib(start - step(i, 100, ticks, lost));
            assert_eq!(
                seen.verdict.tier,
                Tier::Clear,
                "{lost} GiB over {} s from {start} GiB, tick {i} ({:.1} GiB free): {:?}",
                ticks * 2,
                seen.free_gib,
                seen.verdict
            );
            assert!(seen.verdict.admit, "tick {i}");
        }
    }
}

#[test]
fn two_steps_a_minute_apart_are_a_sustained_fall_and_bind() {
    // 12 GiB over 30 s, again 90 s later, again: the lumps of a build that keeps
    // writing. It is a fall, however it arrives.
    let mut replay = Replay::new();
    let mut bound = None;
    for i in 0..450u64 {
        let lost = (0..6).map(|k| step(i, 100 + k * 45, 15, 12.0)).sum::<f64>();
        let seen = replay.tick_gib(150.0 - lost);
        if seen.verdict.tier >= Tier::Elevated && bound.is_none() {
            bound = Some((i * 2, seen.free_gib, seen.verdict.binding));
        }
    }
    let (second, free, binding) = bound.expect("a repeating burst is a fall");
    assert_eq!(binding, Some(Binding::DiskFilling));
    assert!(free > 60.0, "bound at {free:.1} GiB after {second} s");
}

#[test]
fn a_staircase_of_five_gib_a_minute_is_a_sustained_fall() {
    let mut replay = Replay::new();
    let mut elevated = None;
    let mut refused = None;
    for i in 0..750u64 {
        let free = 100.0 - 5.0 * (i * 2 / 60) as f64;
        let seen = replay.tick_gib(free);
        if seen.verdict.tier >= Tier::Elevated && elevated.is_none() {
            elevated = Some((i * 2, seen.free_gib, seen.verdict.binding));
        }
        if !seen.verdict.admit && refused.is_none() {
            refused = Some((i * 2, seen.free_gib, seen.verdict.binding));
        }
    }
    let (second, free, binding) = elevated.expect("a staircase is a fall");
    assert_eq!(binding, Some(Binding::DiskFilling));
    assert!(free > 80.0, "elevated at {free:.1} GiB after {second} s");
    let (second, free, binding) = refused.expect("and it is refused before the floor");
    assert_eq!(binding, Some(Binding::DiskFilling));
    assert!(free > 40.0, "refused at {free:.1} GiB after {second} s");
}

#[test]
fn a_fall_of_ten_gib_a_minute_from_150_gib_refuses_while_most_of_the_disk_is_free() {
    // 2026-11-15: about 150 GiB to nothing in about fifteen minutes.
    let mut replay = Replay::new();
    let mut elevated = None;
    let mut refused = None;
    for i in 0..450u64 {
        let free = falling(150.0, 10.0, i * 2);
        if free < 20.0 {
            break;
        }
        let seen = replay.tick_gib(free);
        if seen.verdict.tier >= Tier::Elevated && elevated.is_none() {
            elevated = Some((i * 2, seen.free_gib));
        }
        if !seen.verdict.admit && refused.is_none() {
            refused = Some((i * 2, seen.free_gib, seen.verdict.binding));
        }
    }
    let (second, free) = elevated.expect("it must shed");
    assert!(free > 120.0, "shed at {free:.1} GiB after {second} s");
    let (second, free, binding) = refused.expect("it must refuse");
    assert_eq!(binding, Some(Binding::DiskFilling));
    assert!(free > 100.0, "refused at {free:.1} GiB after {second} s");
}

#[test]
fn unavailable_readings_a_pause_and_a_remount_never_bind_or_panic() {
    // An unreadable disk: no rate, and the level rule's warning, not a binding.
    let mut replay = Replay::new();
    for i in 0..200u64 {
        replay.tick_gib(falling(99.0, 0.0, i * 2));
    }
    let mut last = None;
    for _ in 0..40 {
        last = Some(replay.tick(Reading::Unavailable(Why::Failed)));
    }
    let verdict = last.unwrap().verdict;
    assert_eq!(verdict.tier, Tier::Clear);
    assert!(
        verdict
            .warnings
            .iter()
            .any(|w| w.contains("free disk space is unreadable")),
        "{:?}",
        verdict.warnings
    );

    // A counter that jumps (a volume remounted) restarts the history.
    let mut replay = Replay::new();
    for i in 0..100u64 {
        replay.tick_gib(falling(400.0, 1.0, i * 2));
    }
    for i in 0..30u64 {
        let seen = replay.tick_gib(50.0 - i as f64 * 0.01);
        assert_eq!(seen.verdict.binding, None, "{:?}", seen.verdict);
    }

    // A sampler pause (the machine slept): the same, with a gap in time.
    let mut replay = Replay::new();
    for i in 0..100u64 {
        replay.tick_gib(falling(99.0, 2.7, i * 2));
    }
    replay.at_ms += 3_600_000;
    let seen = replay.tick_gib(60.0);
    assert!(
        replay.input.readings.disk_fill == Reading::Unavailable(Why::NotYet),
        "{:?} {:?}",
        replay.input.readings.disk_fill,
        seen.verdict
    );
}

#[test]
fn too_few_samples_is_silent_and_a_first_tick_is_not_a_binding() {
    let mut replay = Replay::new();
    for i in 0..30u64 {
        let seen = replay.tick_gib(falling(99.0, 20.0, i * 2));
        assert_eq!(seen.verdict.binding, None, "tick {i}");
        assert!(
            seen.verdict.warnings.is_empty(),
            "{:?}",
            seen.verdict.warnings
        );
    }
}

#[test]
fn the_shed_names_the_cause_it_is_shed_for() {
    let swap = {
        let mut input = Input::test_pressure(Level::Normal);
        input.readings.swap_used = Reading::Value(60 << 30);
        let mut governor = Governor::default();
        let mut now = Instant::now();
        let mut last = None;
        for _ in 0..ENTER_SAMPLES {
            last = Some(governor.observe(now, input.clone()).verdict);
            now += Duration::from_secs(2);
        }
        last.unwrap()
    };
    assert_eq!(swap.shed.cause, Some(Binding::Swap));
    assert!(swap.shed.cause_phrase().contains("swap"));
    assert!(!swap.shed.cause_phrase().contains("memory pressure"));
    assert_eq!(Shed::default().cause, None);
}

#[test]
fn the_report_carries_the_projection_when_the_rule_is_elevated() {
    let mut replay = Replay::new();
    for i in 0..=300u64 {
        replay.tick_gib(falling(99.0, 2.7, i * 2));
    }
    let report = replay.governor.report(replay.now);
    let fill = report.disk_fill.expect("a falling disk is reported");
    assert!((fill.rate_bytes_per_min as f64 / GIB - 2.7).abs() < 0.1);
    assert_eq!(fill.floor_bytes, 16 << 30);
    assert!(
        fill.eta_min > 10.0 && fill.eta_min < 30.0,
        "{}",
        fill.eta_min
    );
    // A calm disk reports nothing.
    let calm = Replay::new();
    assert_eq!(calm.governor.report(calm.now).disk_fill, None);
}
