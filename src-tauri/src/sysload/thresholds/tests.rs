use super::*;
use serde_json::json;

fn tier_of(t: &Thresholds, r: &Readings, binding: Binding) -> Option<Tier> {
    t.assess(r)
        .contributions
        .iter()
        .find(|c| c.binding == binding)
        .map(|c| c.tier)
}

fn swap(r: &mut Readings, ratio: f64) {
    r.swap_used = Reading::Value((ratio * f64::from(64u32) * GIB) as u64);
}

#[test]
fn the_defaults_are_the_documented_ones_and_valid() {
    let d = Thresholds::default();
    assert_eq!(d.problem(), None);
    assert!(d.is_default());
    assert_eq!(
        (d.swap.mode, d.swap.elevated_at, d.swap.severe_at),
        (InputMode::Refuse, 0.50, 0.75)
    );
    assert_eq!(
        (
            d.compressor_segments.mode,
            d.compressor_segments.elevated_at,
            d.compressor_segments.severe_at
        ),
        (InputMode::Refuse, 0.60, 0.80)
    );
    assert_eq!(
        (d.load.mode, d.load.elevated_at, d.load.severe_at),
        (InputMode::Shed, 3.0, 6.0)
    );
    assert_eq!(d.thermal.mode, InputMode::Shed);
    assert_eq!(d.battery.mode, InputMode::Off);
    assert_eq!(
        (d.disk.mode, d.disk.elevated_at, d.disk.severe_at),
        (InputMode::Refuse, 20.0, 10.0)
    );
}

#[test]
fn swap_is_judged_against_physical_memory_and_never_swap_total() {
    let t = Thresholds::default();
    let mut r = Readings::calm();
    // The incident: 57.3 GB used of 64 GiB. Swap total reads 0 or anything.
    r.swap_used = Reading::Value(57_300_000_000);
    assert_eq!(tier_of(&t, &r, Binding::Swap), Some(Tier::Severe));
    swap(&mut r, 0.49);
    assert_eq!(tier_of(&t, &r, Binding::Swap), Some(Tier::Clear));
    swap(&mut r, 0.50);
    assert_eq!(tier_of(&t, &r, Binding::Swap), Some(Tier::Elevated));
    swap(&mut r, 0.74);
    assert_eq!(tier_of(&t, &r, Binding::Swap), Some(Tier::Elevated));
    swap(&mut r, 0.75);
    assert_eq!(tier_of(&t, &r, Binding::Swap), Some(Tier::Severe));
}

#[test]
fn disk_is_worse_when_lower_and_the_boundary_is_strict() {
    let t = Thresholds::default();
    let mut r = Readings::calm();
    for (gib_free, want) in [
        (20.0, Tier::Clear),
        (19.9, Tier::Elevated),
        (10.0, Tier::Elevated),
        (9.9, Tier::Severe),
    ] {
        r.free_disk = Reading::Value((gib_free * GIB) as u64);
        assert_eq!(tier_of(&t, &r, Binding::LowDisk), Some(want), "{gib_free}");
    }
}

#[test]
fn a_zero_or_unreadable_denominator_is_a_gap_never_infinity_or_zero() {
    let t = Thresholds::default();
    let mut r = Readings::calm();
    r.swap_used = Reading::Value(1 << 40);
    r.memsize = Reading::Value(0);
    let a = t.assess(&r);
    assert_eq!(tier_of(&t, &r, Binding::Swap), None);
    assert!(a
        .warnings
        .iter()
        .any(|w| w.starts_with("swap is unreadable")));

    let mut r = Readings::calm();
    r.cores = Reading::Value(Cores::default());
    assert_eq!(tier_of(&t, &r, Binding::CpuLoad), None);
    let mut r = Readings::calm();
    r.segments = Reading::Value(CompressorSegments { total: 5, limit: 0 });
    assert_eq!(tier_of(&t, &r, Binding::CompressorSegments), None);
}

#[test]
fn only_a_reading_that_failed_warns_and_off_never_does() {
    let mut t = Thresholds::default();
    let mut r = Readings::calm();
    r.swap_used = Reading::Unavailable(Why::Unreadable);
    r.thermal = Reading::Unavailable(Why::Unsupported);
    r.segments = Reading::Unavailable(Why::Unsupported);
    r.free_disk = Reading::Unavailable(Why::Failed);
    let a = t.assess(&r);
    assert_eq!(
        a.warnings,
        [
            "swap is unreadable: not governed on",
            "free disk space is unreadable: not governed on"
        ]
    );
    assert!(a
        .contributions
        .iter()
        .all(|c| !matches!(c.binding, Binding::Swap | Binding::LowDisk)));
    t.swap.mode = InputMode::Off;
    t.disk.mode = InputMode::Off;
    assert!(t.assess(&r).warnings.is_empty());
}

#[test]
fn validation_names_the_input_and_the_rule_it_broke() {
    let mut t = Thresholds::default();
    t.swap.elevated_at = f64::NAN;
    assert!(t.problem().unwrap().starts_with("swap:"));
    let mut t = Thresholds::default();
    t.swap.severe_at = 0.4; // below elevated
    assert!(t.problem().unwrap().starts_with("swap:"));
    let mut t = Thresholds::default();
    t.compressor_segments.severe_at = 1.5;
    assert!(t.problem().unwrap().starts_with("compressor_segments:"));
    let mut t = Thresholds::default();
    t.load.elevated_at = 0.0;
    assert!(t.problem().unwrap().starts_with("load:"));
    let mut t = Thresholds::default();
    t.disk.severe_at = 30.0; // lower is worse: severe must not be above
    assert!(t.problem().unwrap().starts_with("disk:"));
    let mut t = Thresholds::default();
    t.disk.elevated_at = f64::INFINITY;
    assert!(t.problem().is_some());
}

#[test]
fn a_stored_block_is_read_one_input_at_a_time() {
    let stored = json!({
        "swap": {"mode": "shed", "elevated_at": 0.4, "severe_at": 0.9},
        "load": {"mode": "refuse", "elevated_at": 8.0, "severe_at": 2.0},
        "disk": "nope",
        "battery": {"mode": "shed"},
        "someday": {"mode": "off"},
    });
    let t = Thresholds::from_stored(stored);
    let d = Thresholds::default();
    assert_eq!(t.swap.mode, InputMode::Shed);
    assert_eq!(t.swap.elevated_at, 0.4);
    assert_eq!(t.load, d.load, "one invalid value costs only that input");
    assert_eq!(t.disk, d.disk, "a malformed input keeps its default");
    assert_eq!(t.battery.mode, InputMode::Shed);
    assert_eq!(t.compressor_segments, d.compressor_segments);
    assert_eq!(t.problem(), None);
    assert_eq!(
        Thresholds::from_stored(json!("garbage")),
        d,
        "a block that is not an object is all defaults"
    );
    assert_eq!(Thresholds::from_stored(json!({"swap": 7})), d);
}

#[test]
fn only_inputs_that_differ_are_stored() {
    assert!(Thresholds::default().to_stored().is_empty());
    let mut t = Thresholds::default();
    t.battery.mode = InputMode::Shed;
    t.disk.severe_at = 5.0;
    let stored = t.to_stored();
    assert_eq!(stored.len(), 2);
    assert_eq!(
        Thresholds::from_stored(serde_json::Value::Object(stored)),
        t
    );
}
