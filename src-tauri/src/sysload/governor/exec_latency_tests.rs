//! Slow process start-up as a governor input: it sheds background work and
//! nothing else, and it neither enters nor leaves on one reading.

use super::*;
use crate::sysload::exec_probe::{ExecLatency, Outcome, Window, CADENCE, ENTER_MS};
use crate::sysload::{Reading, Why};

fn with_latency(reading: Reading<ExecLatency>) -> Input {
    let mut input = Input::test_pressure(Level::Normal);
    input.readings.exec_latency = reading;
    input
}

fn slow(median_ms: u64) -> Reading<ExecLatency> {
    Reading::Value(ExecLatency {
        median_ms,
        n: 9,
        slow: true,
    })
}

fn quick() -> Reading<ExecLatency> {
    Reading::Value(ExecLatency {
        median_ms: 8,
        n: 9,
        slow: false,
    })
}

/// A governor that has counted `readings`, one 2 s tick each.
fn counted(readings: impl IntoIterator<Item = Reading<ExecLatency>>) -> (Governor, Instant) {
    let mut governor = Governor::default();
    let mut now = Instant::now();
    for reading in readings {
        governor.observe(now, with_latency(reading));
        now += Duration::from_millis(crate::sysload::CADENCE_MS);
    }
    (governor, now)
}

#[test]
fn slow_start_up_sheds_background_work_and_leaves_reactive_work_and_launches() {
    let (governor, now) = counted(vec![slow(1_360); ENTER_SAMPLES as usize]);
    let verdict = governor.verdict(now);
    assert_eq!(verdict.tier, Tier::Elevated, "{verdict:?}");
    assert_eq!(verdict.binding, Some(Binding::ExecLatency));
    assert!(verdict.shed.background, "background work is shed");
    assert!(!verdict.shed.reactive, "reactive work is not");
    assert!(verdict.admit, "launches are not refused for slow start-up");
    assert_eq!(verdict.shed.cause, Some(Binding::ExecLatency));
    assert!(
        verdict.message.contains("starting a process takes a median 1.4 s"),
        "{}",
        verdict.message
    );
}

#[test]
fn a_start_up_that_is_not_slow_sheds_nothing() {
    let (governor, now) = counted(vec![quick(); 6]);
    let verdict = governor.verdict(now);
    assert_eq!(verdict.tier, Tier::Clear, "{verdict:?}");
    assert!(!verdict.shed.background && !verdict.shed.reactive);
}

#[test]
fn a_probe_that_could_not_run_is_said_and_never_read_as_fast_or_slow() {
    let (governor, now) = counted(vec![Reading::Unavailable(Why::Failed); 6]);
    let verdict = governor.verdict(now);
    assert!(!verdict.shed.background, "unknown is not slow");
    assert!(
        verdict
            .warnings
            .iter()
            .any(|w| w.contains("process start-up time is unreadable")),
        "a check that did not run must say so: {:?}",
        verdict.warnings
    );
}

#[test]
fn no_probe_in_this_process_is_silent() {
    let (governor, now) = counted(vec![Reading::default(); 6]);
    let verdict = governor.verdict(now);
    assert!(verdict.warnings.is_empty(), "{:?}", verdict.warnings);
    assert!(!verdict.shed.background);
}

#[test]
fn the_tier_is_entered_after_three_slow_ticks_and_left_after_five_quick_ones() {
    let (two, now) = counted(vec![slow(900); ENTER_SAMPLES as usize - 1]);
    assert!(!two.verdict(now).shed.background, "two ticks do not enter");

    let mut readings = vec![slow(900); ENTER_SAMPLES as usize];
    readings.extend(vec![quick(); EXIT_SAMPLES as usize - 1]);
    let (held, now) = counted(readings.clone());
    assert!(held.verdict(now).shed.background, "four quick ticks hold it");
    readings.push(quick());
    let (left, now) = counted(readings);
    assert!(!left.verdict(now).shed.background, "five leave");
}

/// The probe changes its answer once per cadence (about eight sensor ticks),
/// so counting ticks alone would follow the probe across the line every time.
/// Each median here is held for more than `EXIT_SAMPLES` ticks, which is what
/// lets the tick count give way; only the band in the window keeps the verdict
/// still.
#[test]
fn a_median_hovering_at_the_line_does_not_flap_the_verdict() {
    let ticks_per_probe = (CADENCE.as_millis() / u128::from(crate::sysload::CADENCE_MS)) as usize;
    assert!(ticks_per_probe > EXIT_SAMPLES as usize);
    let mut window = Window::default();
    let mut at = Instant::now();
    let mut governor = Governor::default();
    let mut now = Instant::now();
    let (mut shedding, mut transitions) = (false, 0);
    for probe in 0..40u64 {
        let ms = if probe % 2 == 0 { ENTER_MS - 20 } else { ENTER_MS + 20 };
        window.push(at, Outcome::Sample(Duration::from_millis(ms)));
        at += CADENCE;
        for _ in 0..ticks_per_probe {
            governor.observe(now, with_latency(window.reading(at)));
            now += Duration::from_millis(crate::sysload::CADENCE_MS);
            let now_shedding = governor.verdict(now).shed.background;
            if now_shedding != shedding {
                transitions += 1;
                shedding = now_shedding;
            }
        }
    }
    assert!(
        transitions <= 1,
        "the shed verdict changed {transitions} times across 40 probes at the line"
    );
}

#[test]
fn the_binding_is_published_to_older_readers_as_cpu_load_and_read_back_exactly() {
    assert_eq!(Binding::ExecLatency.wire(), Binding::CpuLoad);
    let (base, detail) = Binding::wire_codes(Some(Binding::ExecLatency));
    assert_eq!(Binding::of_wire(base, detail), Some(Some(Binding::ExecLatency)));
    assert_eq!(
        Binding::of_wire(base, 0),
        Some(Some(Binding::CpuLoad)),
        "a reader without the detail byte sees the binding it knows"
    );
}
