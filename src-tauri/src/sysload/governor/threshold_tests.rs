//! The governor's added inputs (swap, compressor segments, CPU load, thermal,
//! power source, free disk), over the pure `decide` and the hysteresis.

use super::*;
use crate::sysload::sensor::{CompressorSegments, Cores, Power, Thermal};
use crate::sysload::thresholds::InputMode;
use crate::sysload::{Reading, Why};

const GIB: f64 = (1u64 << 30) as f64;

fn calm() -> Input {
    Input::test_pressure(Level::Normal)
}

/// Feeds one input for as many samples as entering a tier takes, returning
/// the verdict the hysteresis holds.
fn settle(input: Input) -> GovernorVerdict {
    let mut governor = Governor::default();
    let mut now = Instant::now();
    let mut last = None;
    for _ in 0..ENTER_SAMPLES {
        last = Some(governor.observe(now, input.clone()));
        now += Duration::from_secs(2);
    }
    last.unwrap().verdict
}

fn swap_at(input: &mut Input, ratio: f64) {
    input.readings.swap_used = Reading::Value((ratio * 64.0 * GIB) as u64);
}

fn load_at(input: &mut Input, per_cpu: f64) {
    input.readings.load = Reading::Value([per_cpu * 18.0, 1.0, 1.0]);
}

fn segments_at(input: &mut Input, fraction: f64) {
    input.readings.segments = Reading::Value(CompressorSegments {
        total: (fraction * 1_000_000.0) as u64,
        limit: 1_000_000,
    });
}

fn free_at(input: &mut Input, gib: f64) {
    input.readings.free_disk = Reading::Value((gib * GIB) as u64);
}

#[test]
fn the_incident_is_refused_on_swap_though_the_kernel_said_warn() {
    // 2026-11-14: level warn, swap 0.9x physical memory, load 6.7 per CPU.
    let mut input = Input::test_pressure(Level::Warn);
    swap_at(&mut input, 0.9);
    input.readings.swap_used = Reading::Value(57_300_000_000);
    load_at(&mut input, 6.7);
    let verdict = settle(input);
    assert_eq!(verdict.tier, Tier::Severe);
    assert_eq!(verdict.binding, Some(Binding::Swap));
    assert!(!verdict.admit, "{}", verdict.message);
    assert!(verdict.shed.reactive && verdict.shed.background);
    assert!(
        verdict
            .message
            .contains("swap used is 0.83x physical memory")
            && verdict.message.contains("at or above 0.75x"),
        "{}",
        verdict.message
    );
}

/// (input name, how to push it, tier at the lower threshold, the binding)
#[test]
fn every_input_reaches_elevated_and_severe_at_its_threshold() {
    type Push = fn(&mut Input, bool);
    let cases: [(&str, Push, Binding); 6] = [
        (
            "swap",
            |i, severe| swap_at(i, if severe { 0.8 } else { 0.6 }),
            Binding::Swap,
        ),
        (
            "segments",
            |i, severe| segments_at(i, if severe { 0.85 } else { 0.65 }),
            Binding::CompressorSegments,
        ),
        (
            "disk",
            |i, severe| free_at(i, if severe { 5.0 } else { 25.0 }),
            Binding::LowDisk,
        ),
        (
            "load",
            |i, severe| {
                i.thresholds.load.mode = InputMode::Refuse;
                load_at(i, if severe { 6.5 } else { 3.5 });
            },
            Binding::CpuLoad,
        ),
        (
            "thermal",
            |i, severe| {
                i.thresholds.thermal.mode = InputMode::Refuse;
                i.readings.thermal = Reading::Value(if severe {
                    Thermal::Critical
                } else {
                    Thermal::Serious
                });
            },
            Binding::Thermal,
        ),
        (
            "battery",
            |i, severe| {
                i.thresholds.battery.mode = InputMode::Refuse;
                i.readings.power = Reading::Value(Power::Battery);
                // Battery never reaches severe, whatever the mode.
                let _ = severe;
            },
            Binding::Battery,
        ),
    ];
    for (name, push, binding) in cases {
        let mut input = calm();
        push(&mut input, false);
        let elevated = settle(input);
        assert_eq!(
            elevated.tier,
            Tier::Elevated,
            "{name}: {}",
            elevated.message
        );
        assert_eq!(elevated.binding, Some(binding), "{name}");
        assert!(elevated.admit && elevated.shed.background && !elevated.shed.reactive);

        let mut input = calm();
        push(&mut input, true);
        let severe = settle(input);
        if name == "battery" {
            assert_eq!(severe.tier, Tier::Elevated, "battery never reaches severe");
            continue;
        }
        assert_eq!(severe.tier, Tier::Severe, "{name}: {}", severe.message);
        assert_eq!(severe.binding, Some(binding), "{name}");
        assert!(!severe.admit, "{name}");
    }
}

#[test]
fn off_ignores_shed_stops_at_elevated_and_refuse_reaches_severe() {
    let mut input = calm();
    swap_at(&mut input, 0.9);

    input.thresholds.swap.mode = InputMode::Off;
    let off = settle(input.clone());
    assert_eq!(
        (off.tier, off.admit, off.binding),
        (Tier::Clear, true, None)
    );
    assert!(off.warnings.is_empty());

    input.thresholds.swap.mode = InputMode::Shed;
    let shed = settle(input.clone());
    assert_eq!(shed.tier, Tier::Elevated);
    assert!(shed.admit && shed.shed.background && !shed.shed.reactive);

    input.thresholds.swap.mode = InputMode::Refuse;
    let refuse = settle(input);
    assert_eq!(refuse.tier, Tier::Severe);
    assert!(!refuse.admit && refuse.shed.reactive);
}

#[test]
fn launch_refusal_off_means_no_input_ever_refuses() {
    let mut input = calm();
    swap_at(&mut input, 0.95);
    free_at(&mut input, 1.0);
    input.govern_launches = false;
    let verdict = settle(input);
    assert_eq!(verdict.tier, Tier::Severe);
    assert!(verdict.admit);
    assert_eq!(verdict.reason, Reason::LaunchGateOff);
    assert!(verdict.shed.background && verdict.shed.reactive);
}

#[test]
fn equal_tiers_bind_in_the_documented_order() {
    // Every added input at Elevated, plus kernel warn; strip them in order.
    let mut input = Input::test_pressure(Level::Warn);
    input.thresholds.battery.mode = InputMode::Shed;
    swap_at(&mut input, 0.6);
    segments_at(&mut input, 0.65);
    free_at(&mut input, 25.0);
    input.readings.thermal = Reading::Value(Thermal::Serious);
    load_at(&mut input, 3.5);
    input.readings.power = Reading::Value(Power::Battery);

    let bound = |input: &Input| settle(input.clone()).binding;
    assert_eq!(bound(&input), Some(Binding::MemoryPressure));
    input.level = Some(Level::Normal);
    assert_eq!(bound(&input), Some(Binding::CompressorSegments));
    input.readings.segments = Readings::calm().segments;
    assert_eq!(bound(&input), Some(Binding::Swap));
    input.readings.swap_used = Reading::Value(0);
    assert_eq!(bound(&input), Some(Binding::LowDisk));
    free_at(&mut input, 200.0);
    assert_eq!(bound(&input), Some(Binding::Thermal));
    input.readings.thermal = Reading::Value(Thermal::Nominal);
    assert_eq!(bound(&input), Some(Binding::CpuLoad));
    load_at(&mut input, 0.1);
    assert_eq!(bound(&input), Some(Binding::Battery));
    input.readings.power = Reading::Value(Power::Ac);
    assert_eq!(bound(&input), None);

    // Thrash outranks every added input at the same tier (severe).
    let mut severe = calm();
    swap_at(&mut severe, 0.9);
    severe.compressor_thrash = true;
    assert_eq!(bound(&severe), Some(Binding::CompressorThrash));
    severe.compressor_thrash = false;
    segments_at(&mut severe, 0.9);
    assert_eq!(bound(&severe), Some(Binding::CompressorSegments));
}

#[test]
fn an_unreadable_reading_adds_a_warning_and_no_tier() {
    let mut input = calm();
    input.readings.swap_used = Reading::Unavailable(Why::Unreadable);
    let verdict = settle(input.clone());
    assert_eq!(verdict.tier, Tier::Clear);
    assert!(verdict.admit);
    assert_eq!(verdict.warnings, ["swap is unreadable: not governed on"]);
    // The warning reaches the admission a launch step sees.
    let report = GovernorReport {
        verdict: verdict.clone(),
        ..GovernorReport::ungoverned()
    };
    assert_eq!(
        admit_on(Site::PtySpawn, &report)
            .unwrap()
            .warning
            .as_deref(),
        Some("swap is unreadable: not governed on")
    );
    // Not governed on at all, or never provided: nothing to warn about.
    input.thresholds.swap.mode = InputMode::Off;
    assert!(settle(input.clone()).warnings.is_empty());
    input.thresholds.swap.mode = InputMode::Refuse;
    input.readings.swap_used = Reading::Unavailable(Why::Unsupported);
    assert!(settle(input).warnings.is_empty());
}

#[test]
fn the_combined_tier_keeps_the_three_in_five_out_hysteresis() {
    let mut hot = calm();
    swap_at(&mut hot, 0.9);
    let mut governor = Governor::default();
    let mut now = Instant::now();
    let mut step = |g: &mut Governor, input: &Input| {
        let v = g.observe(now, input.clone()).verdict;
        now += Duration::from_secs(2);
        v
    };
    assert_eq!(step(&mut governor, &hot).tier, Tier::Clear);
    assert_eq!(step(&mut governor, &hot).tier, Tier::Clear);
    assert_eq!(step(&mut governor, &hot).tier, Tier::Severe);
    for _ in 0..4 {
        assert_eq!(step(&mut governor, &calm()).tier, Tier::Severe);
    }
    // While held but recovered, the words do not claim a crossed threshold.
    let held = step(&mut governor, &calm());
    assert_eq!(held.tier, Tier::Clear);
    let mut governor = Governor::default();
    for _ in 0..3 {
        step(&mut governor, &hot);
    }
    let mut cooler = calm();
    swap_at(&mut cooler, 0.2);
    let recovering = step(&mut governor, &cooler);
    assert_eq!(recovering.tier, Tier::Severe);
    assert!(
        recovering.message.contains("now back under 0.75x"),
        "{}",
        recovering.message
    );
    assert!(!recovering.message.contains("at or above"));
}

#[test]
fn describe_names_each_resource_its_value_and_its_threshold() {
    let mut input = calm();
    segments_at(&mut input, 0.85);
    let v = settle(input);
    assert!(
        v.message.contains("0.85 of its segment limit") && v.message.contains("at or above 0.80"),
        "{}",
        v.message
    );
    let mut input = calm();
    free_at(&mut input, 4.0);
    let v = settle(input);
    assert!(
        v.message
            .contains("free disk space is 4.0 GiB, below 16.0 GiB"),
        "{}",
        v.message
    );
    let mut input = calm();
    load_at(&mut input, 3.5);
    input.readings.cores = Reading::Value(Cores {
        logical: 18,
        levels: 0,
        perf0: 18,
        perf1: 0,
    });
    let v = settle(input);
    assert!(
        v.message.contains("per CPU") && v.message.contains("at or above 3.00"),
        "{}",
        v.message
    );
}

/// The disk guard was advice only (owner decision 2026-10-21) and is now a
/// setting (owner instruction 2026-11-14): off and shed never refuse a launch
/// on disk, refuse does once free space is below `severe_at`.
#[test]
fn disk_refuses_a_launch_only_in_refuse_mode_below_severe_at() {
    let mut input = calm();
    free_at(&mut input, 2.0);
    for mode in [InputMode::Off, InputMode::Shed] {
        input.thresholds.disk.mode = mode;
        for site in Site::ALL {
            let report = GovernorReport {
                verdict: settle(input.clone()),
                ..GovernorReport::ungoverned()
            };
            assert!(
                admit_on(site, &report).is_ok(),
                "{mode:?} refused {}",
                site.name()
            );
        }
    }
    input.thresholds.disk.mode = InputMode::Refuse;
    let verdict = settle(input.clone());
    assert_eq!(verdict.binding, Some(Binding::LowDisk));
    for site in Site::ALL {
        let report = GovernorReport {
            verdict: verdict.clone(),
            ..GovernorReport::ungoverned()
        };
        let refused = admit_on(site, &report).unwrap_err();
        assert_eq!(refused.verdict.binding, Some(Binding::LowDisk));
    }
    // Between the severe and elevated floors (16 and 32 GiB on a 64 GiB Mac)
    // it only sheds.
    free_at(&mut input, 25.0);
    let between = settle(input);
    assert!(between.admit && between.shed.background);
}
