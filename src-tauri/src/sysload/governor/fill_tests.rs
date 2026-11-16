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
        let rates = self.tracker.rates(self.at_ms);
        self.input.readings.disk_fill = rates.quick;
        self.input.readings.disk_fill_sustained = rates.sustained;
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
fn deleting_the_output_holds_severe_for_the_180_s_release_then_clears() {
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
    // A Severe the fill rule caused is held for its own, longer release, not
    // the five ticks every other cause takes.
    assert_eq!(
        seen.last().unwrap().verdict.tier,
        Tier::Severe,
        "{:?}",
        seen.last().unwrap().verdict
    );
    for _ in 0..FILL_SEVERE_RELEASE_SAMPLES {
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

/// Free GiB at `s` seconds of a one-off loss of `size` GiB over `dur` seconds
/// starting at `at`, from `from` GiB.
fn burst(from: f64, size: f64, dur: u64, at: u64) -> impl Fn(u64) -> f64 {
    move |s| match s.checked_sub(at) {
        None => from,
        Some(t) if t >= dur => from - size,
        Some(t) => from - size * t as f64 / dur as f64,
    }
}

/// A fall of `per_min` GiB a minute from second `at`.
fn linear(from: f64, per_min: f64, at: u64) -> impl Fn(u64) -> f64 {
    move |s| from - per_min * s.saturating_sub(at) as f64 / 60.0
}

/// `size` GiB lost over `dur` seconds at the start of every `every` seconds,
/// from second `at`: a build that writes in lumps.
fn lumps(from: f64, size: f64, dur: u64, every: u64, at: u64) -> impl Fn(u64) -> f64 {
    move |s| match s.checked_sub(at) {
        None => from,
        Some(t) => {
            from - (t / every) as f64 * size - size * (t % every).min(dur) as f64 / dur as f64
        }
    }
}

/// A free-space pattern: GiB free at a second.
type Pattern = Box<dyn Fn(u64) -> f64>;

/// What a run through the real tracker and governor showed.
#[derive(Debug, Default)]
struct Outcome {
    /// (second, free GiB) of the first tick held at each tier.
    elevated: Option<(u64, f64)>,
    severe: Option<(u64, f64)>,
    refused: Option<(u64, f64)>,
    /// Seconds held at Elevated or above.
    shedding_s: u64,
    /// Second of the first fall back to Clear after Severe.
    cleared_after_severe: Option<u64>,
    /// Seconds not admitting launches.
    refused_s: u64,
    /// Seconds spent at a tier below Severe after the first Severe, before the
    /// run ended: the time launches were admitted in the middle of a fill.
    outside_severe_s: u64,
}

/// Drives `free` for `seconds` at the 2 s tick; the tracker is reset at `reset`.
fn drive(free: impl Fn(u64) -> f64, seconds: u64, reset: Option<u64>) -> Outcome {
    drive_until(free, seconds, reset, 0.0)
}

/// [`drive`], stopping when free space reaches `stop_at` GiB: the severe floor
/// is 16 GiB on the 64 GiB Mac these tests model, where the level rule has the
/// last word and the fill rule's work is done.
fn drive_until(
    free: impl Fn(u64) -> f64,
    seconds: u64,
    reset: Option<u64>,
    stop_at: f64,
) -> Outcome {
    let mut replay = Replay::new();
    let mut out = Outcome::default();
    for i in 0..seconds / 2 {
        let s = i * 2;
        let at = free(s);
        if at <= stop_at {
            break;
        }
        if reset == Some(s) {
            replay.tracker.reset();
        }
        let seen = replay.tick_gib(at);
        let tier = seen.verdict.tier;
        if tier >= Tier::Elevated {
            out.shedding_s += 2;
            out.elevated.get_or_insert((s, at));
        }
        if tier == Tier::Severe {
            out.severe.get_or_insert((s, at));
        }
        if !seen.verdict.admit {
            out.refused.get_or_insert((s, at));
            out.refused_s += 2;
        }
        if out.severe.is_some() && tier != Tier::Severe {
            out.outside_severe_s += 2;
        }
        if out.severe.is_some() && tier == Tier::Clear {
            out.cleared_after_severe.get_or_insert(s);
        }
    }
    out
}

/// How a pattern starts: 30 s into the run (cold), after ten flat minutes, or
/// with the tracker reset at 300 s (a pattern already going on loses its
/// history; a one-off burst follows the reset by 30 s).
#[derive(Clone, Copy, Debug)]
enum Start {
    Cold,
    Warm,
    Reset,
}

impl Start {
    const ALL: [Start; 3] = [Start::Cold, Start::Warm, Start::Reset];

    /// Second the pattern begins, for a continuing pattern and for a burst.
    fn begins(self, one_off: bool) -> u64 {
        match (self, one_off) {
            (Start::Cold, _) => 30,
            (Start::Warm, _) => 630,
            (Start::Reset, true) => 330,
            (Start::Reset, false) => 30,
        }
    }

    fn reset(self) -> Option<u64> {
        matches!(self, Start::Reset).then_some(300)
    }
}

/// Minutes until the 16 GiB floor at `average` GiB a minute from `free`.
fn minutes_left(free: f64, average: f64) -> f64 {
    (free - 16.0) / average
}

#[test]
fn a_one_off_burst_never_refuses_and_sheds_for_a_bounded_time_at_every_start() {
    // (from, GiB lost, seconds it took). Each is Elevated for at most three
    // minutes, which is what shedding on a burst costs, and never Severe:
    // refusing is for a fall that goes on, or a burst that ate what was left.
    for (from, size, dur) in [
        (70.0, 12.0, 30),
        (100.0, 20.0, 10),
        (50.0, 8.0, 30),
        (100.0, 20.0, 60),
        (120.0, 30.0, 90),
    ] {
        for start in Start::ALL {
            let at = start.begins(true) + 70;
            let out = drive(burst(from, size, dur, at), 1_200, start.reset());
            let case = format!("{size} GiB over {dur} s from {from} GiB, {start:?}: {out:?}");
            eprintln!("{case}");
            assert_eq!(out.severe, None, "{case}");
            assert_eq!(out.refused, None, "{case}");
            assert!(out.shedding_s <= 180, "{case}");
        }
    }
}

#[test]
fn smooth_falls_shed_as_early_as_before_and_refuse_with_minutes_to_spare() {
    // (from, GiB/min, the shed no later than this free space, the refusal with
    // at least this many true minutes left, and with more than this free).
    for (from, rate, shed_by, minutes, refuse_above) in [
        (99.0, 2.7, 91.0, 8.0, 40.0),
        (150.0, 10.0, 128.0, 6.0, 60.0),
    ] {
        for start in Start::ALL {
            let out = drive(
                linear(from, rate, start.begins(false)),
                2_400,
                start.reset(),
            );
            let case = format!("{rate} GiB/min from {from} GiB, {start:?}: {out:?}");
            eprintln!("{case}");
            let (_, shed_free) = out.elevated.expect(&case);
            assert!(shed_free >= shed_by, "{case}");
            let (_, free) = out.refused.expect(&case);
            assert!(minutes_left(free, rate) >= minutes, "{case}");
            assert!(free > refuse_above, "{case}");
            // A tracker reset forgets a fall that was already refused; nothing
            // else lets go of it.
            if let Some(second) = out.cleared_after_severe {
                assert!(
                    matches!(start, Start::Reset) && (300..=360).contains(&second),
                    "{case}"
                );
            }
        }
    }
}

#[test]
fn a_build_that_writes_in_lumps_is_a_fall_whatever_the_pauses_between() {
    // (from GiB, GiB per lump, seconds a lump takes, seconds between lump
    // starts, average GiB a minute). Shed with at least 20 true minutes left,
    // refuse with at least 6, and never let go while the pattern goes on.
    for (from, size, dur, every, average) in [
        (100.0, 5.0, 1, 60, 5.0),
        (150.0, 5.0, 1, 80, 3.75),
        (150.0, 5.0, 1, 90, 5.0 / 1.5),
        (99.0, 8.0 * 40.0 / 60.0, 40, 120, 8.0 * 40.0 / 120.0),
        (150.0, 12.0, 30, 90, 8.0),
    ] {
        for start in Start::ALL {
            let out = drive_until(
                lumps(from, size, dur, every, start.begins(false)),
                14_400,
                start.reset(),
                16.0,
            );
            let case =
                format!("{size} GiB over {dur} s every {every} s from {from}, {start:?}: {out:?}");
            eprintln!("{case}");
            // Shed with 20 true minutes left when the pattern starts with that
            // much; when it starts with less, within 160 s of its start, which
            // is what two minutes of history allow.
            let (shed_at, shed_free) = out.elevated.expect(&case);
            if minutes_left(from, average) >= 20.0 {
                assert!(minutes_left(shed_free, average) >= 20.0, "{case}");
            } else {
                assert!(shed_at <= start.begins(false) + 160, "{case}");
            }
            let (_, free) = out.severe.expect(&case);
            assert!(minutes_left(free, average) >= 6.0, "{case}");
            if let Some(second) = out.cleared_after_severe {
                assert!(
                    matches!(start, Start::Reset) && (300..=360).contains(&second),
                    "{case}"
                );
            }
        }
    }
}

/// The intermittent rows of the release review, each as (from GiB, GiB per
/// lump, seconds a lump takes, seconds between lump starts).
const INTERMITTENT: [(f64, f64, u64, u64); 8] = [
    (100.0, 5.0, 1, 60),
    (150.0, 5.0, 1, 80),
    (150.0, 5.0, 1, 90),
    (150.0, 10.0, 1, 150),
    (150.0, 10.0, 1, 180),
    (150.0, 15.0, 1, 240),
    (150.0, 12.0, 30, 90),
    (99.0, 8.0 * 40.0 / 60.0, 40, 120),
];

#[test]
fn a_severe_the_fill_rule_caused_is_held_through_every_pause_until_the_floor() {
    // Pattern from 30 s in, run until free space reaches the 16 GiB floor. The
    // time spent below Severe after the first Severe is launches admitted in
    // the middle of the fill, and must be zero.
    for (from, size, dur, every) in INTERMITTENT {
        let out = drive_until(lumps(from, size, dur, every, 30), 14_400, None, 16.0);
        let case = format!("{size} GiB over {dur} s every {every} s from {from}: {out:?}");
        eprintln!("{case}");
        let (second, free) = out.severe.expect(&case);
        assert!(free > 16.0, "{case}: Severe at second {second}");
        assert_eq!(out.outside_severe_s, 0, "{case}");
        assert_eq!(out.cleared_after_severe, None, "{case}");
    }
}

#[test]
fn writes_too_slow_to_matter_before_the_floor_are_reported_not_tuned_for() {
    // 20 GiB per 300 s and 30 GiB per 600 s from 150 GiB: 4 and 3 GiB a minute
    // on average. Nothing is asserted about when they bind; what they do is
    // printed (`--nocapture`) and the release rule still holds.
    for (size, every) in [(20.0, 300), (30.0, 600)] {
        let out = drive_until(lumps(150.0, size, 1, every, 30), 14_400, None, 16.0);
        eprintln!("{size} GiB per {every} s from 150: {out:?}");
        if out.severe.is_some() {
            assert_eq!(out.outside_severe_s, 0, "{out:?}");
        }
    }
}

/// A one-off write of `size` GiB over `dur` seconds from `from` GiB free,
/// starting at second `at`, and whatever followed it for 2 400 s.
fn refused_after_a_write(from: f64, size: f64, dur: u64, at: u64) -> Outcome {
    drive(burst(from, size, dur, at), at + dur + 2_400, None)
}

#[test]
fn a_finished_write_that_left_little_room_refuses_for_at_most_the_window_plus_the_hold() {
    // The bound: the ten-minute net fall keeps the write for ten minutes after
    // it ends, and a Severe the fill rule caused is held three minutes more
    // (`FILL_SEVERE_RELEASE`): 780 s, and a few ticks of hysteresis.
    let bound = 800;
    // (from GiB free, GiB written, seconds it took)
    for (from, size, dur) in [(40.0, 12.0, 30), (60.0, 20.0, 80), (107.0, 40.0, 120)] {
        for (name, at) in [("warm", 630), ("cold", 100)] {
            let out = refused_after_a_write(from, size, dur, at);
            eprintln!("{size} GiB over {dur} s from {from}, {name}: {out:?}");
            assert!(
                out.refused_s <= bound,
                "{size} GiB over {dur} s ending at {} GiB free, {name}: refused {} s: {out:?}",
                from - size,
                out.refused_s
            );
        }
    }
}

#[test]
fn a_lone_burst_binds_severe_at_half_the_headroom_cold_and_all_of_it_warm() {
    // Severe wants the sustained rate to put the floor under ten minutes away:
    // rate = fall / divisor, headroom / rate < 10 min. The divisor is
    // max(history, 5 min) and the fall is capped by the ten-minute window, so
    // on a cold history (under 5 min) the fall must exceed half the headroom
    // left after it, and on a warm one (10 min) all of it. 16 GiB floor.
    let floor = 16.0;
    // (starts at s, from GiB, size that must not bind, size that must)
    for (at, from, quiet, loud) in [(100, 100.0, 26.0, 30.0), (630, 200.0, 80.0, 100.0)] {
        let headroom_after = |size: f64| from - size - floor;
        let name = if at < 300 { "cold" } else { "warm" };
        let share = |size: f64| size / headroom_after(size);
        let quiet_out = drive(burst(from, quiet, 30, at), at + 30 + 900, None);
        let loud_out = drive(burst(from, loud, 30, at), at + 30 + 900, None);
        eprintln!(
            "{name}: {quiet} GiB is {:.2} of the headroom it leaves: severe {:?}; {loud} GiB is {:.2}: severe {:?}",
            share(quiet),
            quiet_out.severe,
            share(loud),
            loud_out.severe
        );
        assert_eq!(quiet_out.severe, None, "{name}: {quiet_out:?}");
        assert!(loud_out.severe.is_some(), "{name}: {loud_out:?}");
    }
}

/// Another resource that reads Severe over a window of seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Other {
    MemoryCritical,
    SwapSevere,
}

/// One tick of a replay with another cause overlapping the fill.
#[derive(Debug, Clone)]
struct Tick {
    second: u64,
    free: f64,
    tier: Tier,
    binding: Option<Binding>,
    /// What the fill rule itself read, whichever cause the governor named.
    fill: Tier,
}

fn fill_tier(input: &Input) -> Tier {
    input
        .thresholds
        .assess(&input.readings)
        .contributions
        .iter()
        .filter(|c| c.binding == Binding::DiskFilling)
        .map(|c| c.tier)
        .max()
        .unwrap_or(Tier::Clear)
}

/// [`drive_until`] with `other` read Severe from second `from` up to `to`.
fn drive_with(
    free: impl Fn(u64) -> f64,
    stop_at: f64,
    other: Option<(Other, u64, u64)>,
) -> Vec<Tick> {
    let mut replay = Replay::new();
    let memory = replay.input.readings.memsize.value().expect("memory size");
    let mut ticks = Vec::new();
    for i in 0..7_200u64 {
        let second = i * 2;
        let at = free(second);
        if at <= stop_at {
            break;
        }
        let active = |kind: Other| {
            other.is_some_and(|(k, from, to)| k == kind && (from..to).contains(&second))
        };
        replay.input.level = Some(if active(Other::MemoryCritical) {
            Level::Critical
        } else {
            Level::Normal
        });
        replay.input.readings.swap_used = Reading::Value(if active(Other::SwapSevere) {
            memory / 10 * 9
        } else {
            0
        });
        let seen = replay.tick_gib(at);
        ticks.push(Tick {
            second,
            free: at,
            tier: seen.verdict.tier,
            binding: seen.verdict.binding,
            fill: fill_tier(&replay.input),
        });
    }
    ticks
}

/// Seconds below Severe after the first Severe.
fn seconds_outside_severe(ticks: &[Tick]) -> Vec<u64> {
    let first = ticks.iter().position(|t| t.tier == Tier::Severe);
    first.map_or(Vec::new(), |first| {
        ticks[first..]
            .iter()
            .filter(|t| t.tier != Tier::Severe)
            .map(|t| t.second)
            .collect()
    })
}

#[test]
fn the_fill_hold_follows_the_fill_rule_whichever_cause_is_named() {
    // 5 GiB every 80 s from 150 GiB: the fill rule reads Severe in stretches
    // with pauses between them. Memory pressure critical, or swap Severe,
    // overlaps the first stretch and eases during a pause in which the fill
    // rule reads lower: the held binding is then not the fill rule, and the
    // hold must still keep Severe until the floor.
    let pattern = || lumps(150.0, 5.0, 1, 80, 30);
    let baseline = drive_with(pattern(), 16.0, None);
    let first = baseline
        .iter()
        .find(|t| t.tier == Tier::Severe)
        .expect("the fill rule alone reaches Severe")
        .second;
    let pause = baseline
        .windows(7)
        .find(|w| {
            w[0].second > first + 20
                && w.iter()
                    .all(|t| t.fill < Tier::Severe && t.tier == Tier::Severe)
        })
        .expect("the fill rule reads lower for 12 s inside the held Severe")[0]
        .second;
    assert_eq!(seconds_outside_severe(&baseline), Vec::<u64>::new());
    for (other, binding) in [
        (Other::MemoryCritical, Binding::MemoryPressure),
        (Other::SwapSevere, Binding::Swap),
    ] {
        let ticks = drive_with(pattern(), 16.0, Some((other, first - 30, pause)));
        assert!(
            ticks
                .iter()
                .any(|t| t.tier == Tier::Severe && t.binding == Some(binding)),
            "{other:?} must be the held cause for part of the run"
        );
        assert_eq!(
            seconds_outside_severe(&ticks),
            Vec::<u64>::new(),
            "{other:?}: launches were admitted in the middle of the fill"
        );
        // Free space stayed above the 16 GiB floor throughout, so the level
        // rule was never the reason.
        assert!(ticks.iter().all(|t| t.free > 16.0), "{other:?}");
        eprintln!("{other:?} eased at {pause} s: held Severe through the pauses");
    }
}

/// A governor fed directly: free space is 40 GiB, and the fill rule reads
/// Severe (10 GiB a minute, under four minutes to the floor) or nothing.
struct Direct {
    governor: Governor,
    input: Input,
    now: Instant,
}

impl Direct {
    fn new() -> Self {
        let mut input = Input::test_pressure(Level::Normal);
        input.readings.free_disk = Reading::Value(40 << 30);
        Self {
            governor: Governor::default(),
            input,
            now: Instant::now(),
        }
    }

    fn set(&mut self, memory_critical: bool, fill_severe: bool) {
        self.input.level = Some(if memory_critical {
            Level::Critical
        } else {
            Level::Normal
        });
        self.input.readings.disk_fill_sustained =
            Reading::Value(if fill_severe { 10 << 30 } else { 0 });
    }

    /// One counted tick.
    fn tick(&mut self, memory_critical: bool, fill_severe: bool) -> GovernorVerdict {
        self.set(memory_critical, fill_severe);
        self.now += Duration::from_millis(TICK_MS);
        self.governor.observe(self.now, self.input.clone()).verdict
    }

    /// A tick that read the same snapshot again: nothing is counted.
    fn repeat(&mut self, memory_critical: bool, fill_severe: bool) -> GovernorVerdict {
        self.set(memory_critical, fill_severe);
        self.now += Duration::from_millis(TICK_MS);
        self.governor
            .observe_repeat(self.now, self.input.clone())
            .verdict
    }

    /// Memory critical for ten ticks, then `fill_ticks` ticks with the fill
    /// rule Severe too. The last of those is the fill rule's last Severe.
    fn memory_severe_then_fill(&mut self, fill_ticks: u32) {
        for _ in 0..10 {
            self.tick(true, false);
        }
        assert_eq!(self.governor.held, Tier::Severe);
        assert_eq!(self.governor.binding, Some(Binding::MemoryPressure));
        for _ in 0..fill_ticks {
            self.tick(true, true);
        }
    }
}

#[test]
fn a_fill_blip_of_one_or_two_ticks_under_a_memory_held_severe_arms_no_hold() {
    for blip in [1, 2] {
        let mut d = Direct::new();
        d.memory_severe_then_fill(blip);
        for _ in 0..3 {
            d.tick(true, false);
        }
        // Memory eases and the fill rule reads nothing.
        for eased in 1..=EXIT_SAMPLES {
            let v = d.tick(false, false);
            let severe = v.tier == Tier::Severe;
            assert_eq!(
                severe,
                eased < EXIT_SAMPLES,
                "a {blip}-tick blip, {eased} ticks after memory eased: {v:?}"
            );
        }
    }
}

#[test]
fn a_fill_hold_armed_under_a_memory_held_severe_is_left_exactly_at_the_release_samples() {
    for gap in [1u32, 5, 20] {
        let mut d = Direct::new();
        d.memory_severe_then_fill(5);
        // `last` is the fill rule's last Severe reading; memory stays critical
        // for `gap` more ticks, then eases.
        let mut left = None;
        for after in 1..=FILL_SEVERE_RELEASE_SAMPLES + 5 {
            let v = d.tick(after <= gap, false);
            let severe = v.tier == Tier::Severe;
            if !severe && left.is_none() {
                left = Some(after);
            }
            if after > gap && severe {
                assert_eq!(
                    v.binding,
                    Some(Binding::DiskFilling),
                    "gap {gap}, {after} ticks after the fill rule's last Severe: {v:?}"
                );
                assert!(
                    v.message
                        .contains("held for 3 minutes after the fill rate last read severe"),
                    "{}",
                    v.message
                );
                assert!(!v.admit);
            }
            if after <= gap {
                assert_eq!(v.binding, Some(Binding::MemoryPressure), "{v:?}");
            }
        }
        assert_eq!(
            left,
            Some(FILL_SEVERE_RELEASE_SAMPLES),
            "gap {gap}: Severe is left that many counted ticks after the fill rule's last Severe"
        );
    }
}

#[test]
fn a_repeated_snapshot_neither_arms_nor_advances_the_fill_hold() {
    // Repeats of a fill-Severe reading under memory-held Severe arm nothing.
    let mut d = Direct::new();
    d.memory_severe_then_fill(0);
    for _ in 0..20 {
        d.repeat(true, true);
    }
    for eased in 1..=EXIT_SAMPLES {
        let v = d.tick(false, false);
        assert_eq!(v.tier == Tier::Severe, eased < EXIT_SAMPLES, "{v:?}");
    }
    // Repeats after the fill rule's last Severe do not run the hold's clock.
    let mut d = Direct::new();
    d.memory_severe_then_fill(5);
    for _ in 1..FILL_SEVERE_RELEASE_SAMPLES {
        assert_eq!(d.tick(false, false).tier, Tier::Severe);
    }
    for _ in 0..50 {
        assert_eq!(d.repeat(false, false).tier, Tier::Severe);
    }
    assert_ne!(d.tick(false, false).tier, Tier::Severe);
}

#[test]
fn a_sensor_that_goes_unavailable_mid_hold_carries_no_hold_back() {
    let mut d = Direct::new();
    d.memory_severe_then_fill(5);
    d.tick(false, false);
    d.input.state = SensorState::Dead;
    let gone = d.tick(false, false);
    assert_eq!(gone.tier, Tier::Clear, "{gone:?}");
    d.input.state = SensorState::Fresh;
    // Back and calm: no Severe is held over.
    assert_eq!(d.tick(false, false).tier, Tier::Clear);
    // A later memory Severe is left in the ordinary EXIT_SAMPLES.
    for _ in 0..6 {
        d.tick(true, false);
    }
    assert_eq!(d.governor.held, Tier::Severe);
    for eased in 1..=EXIT_SAMPLES {
        let v = d.tick(false, false);
        assert_eq!(v.tier == Tier::Severe, eased < EXIT_SAMPLES, "{v:?}");
    }
}

#[test]
fn a_burst_of_a_minute_or_less_never_refuses_when_it_leaves_42_gib_or_more() {
    for (from, size, dur) in [(54.0, 12.0, 30), (62.0, 20.0, 60), (50.0, 8.0, 30)] {
        let out = refused_after_a_write(from, size, dur, 630);
        assert_eq!(
            out.refused, None,
            "{size} GiB over {dur} s from {from}: {out:?}"
        );
    }
}

#[test]
fn a_disk_that_is_not_falling_never_refuses_and_a_write_then_delete_barely_sheds() {
    for start in Start::ALL {
        let begin = start.begins(false);
        let cases: Vec<(&str, Pattern)> = vec![
            ("flat", Box::new(|_| 60.0)),
            (
                "0.2 GiB/min",
                Box::new(move |s| linear(60.0, 0.2, begin)(s)),
            ),
            (
                "5 GiB written then deleted every 60 s",
                Box::new(move |s| {
                    if s >= begin && (s - begin) % 60 < 30 {
                        55.0
                    } else {
                        60.0
                    }
                }),
            ),
        ];
        for (name, free) in cases {
            let out = drive(free, 1_800, start.reset());
            let case = format!("{name}, {start:?}: {out:?}");
            eprintln!("{case}");
            assert_eq!(out.severe, None, "{case}");
            assert_eq!(out.refused, None, "{case}");
            assert!(out.shedding_s <= 60, "{case}");
        }
    }
}

#[test]
fn a_repeating_burst_binds_at_a_stated_point_not_just_at_some_point() {
    // 12 GiB over 30 s every 90 s from 150 GiB: 8 GiB a minute on average.
    let out = drive(lumps(150.0, 12.0, 30, 90, 100), 1_800, None);
    let (second, free) = out.elevated.expect("it sheds");
    assert!(
        (120..=900).contains(&second) && (125.0..=140.0).contains(&free),
        "{out:?}"
    );
    let (second, free) = out.severe.expect("it refuses");
    assert!(
        (80.0..=110.0).contains(&free) && minutes_left(free, 8.0) >= 6.0,
        "refused at second {second}, {free:.1} GiB: {out:?}"
    );
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
