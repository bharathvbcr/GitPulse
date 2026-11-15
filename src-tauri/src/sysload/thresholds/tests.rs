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
    let t = Thresholds {
        disk_auto: false,
        ..Thresholds::default()
    };
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
    // One unreadable disk is one warning, not one per rule that reads it.
    t.swap.mode = InputMode::Off;
    t.disk.mode = InputMode::Off;
    assert_eq!(
        t.assess(&r).warnings,
        ["the disk fill rate is unreadable: not governed on"],
        "with the level rule off, the fill rule is the one that says so"
    );
    t.disk_guard.fill.mode = InputMode::Off;
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
    t.disk_auto = false;
    t.disk.severe_at = 30.0; // lower is worse: severe must not be above
    assert!(t.problem().unwrap().starts_with("disk:"));
    let mut t = Thresholds::default();
    t.disk_auto = false;
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
    t.disk_auto = false;
    t.disk.severe_at = 5.0;
    let stored = t.to_stored();
    assert_eq!(stored.len(), 2);
    assert_eq!(
        Thresholds::from_stored(serde_json::Value::Object(stored)),
        t
    );
}

#[test]
fn the_automatic_disk_floors_follow_physical_memory_and_never_fall_below_the_fixed_ones() {
    let at = |gib: u64| auto_disk_floors(Some(gib << 30));
    assert_eq!(at(64), (32.0, 16.0));
    assert_eq!(at(128), (64.0, 32.0));
    assert_eq!(at(16), (20.0, 10.0));
    assert_eq!(at(8), (20.0, 10.0));
    // Unreadable or nonsense memory falls back to the fixed numbers.
    assert_eq!(auto_disk_floors(None), (20.0, 10.0));
    assert_eq!(auto_disk_floors(Some(0)), (20.0, 10.0));
    // Never past what a setting may hold.
    let huge = auto_disk_floors(Some(u64::MAX));
    assert!(huge.0 <= MAX_DISK_GIB && huge.1 <= MAX_DISK_GIB && huge.1 <= huge.0);
}

#[test]
fn the_disk_rule_judges_by_the_floors_of_this_machine_unless_a_person_set_them() {
    let t = Thresholds::default();
    let mut r = Readings::calm();
    r.memsize = Reading::Value(64 << 30);
    for (free, want) in [
        (33.0, Tier::Clear),
        (31.9, Tier::Elevated),
        (16.0, Tier::Elevated),
        (15.9, Tier::Severe),
    ] {
        r.free_disk = Reading::Value((free * GIB) as u64);
        assert_eq!(
            tier_of(&t, &r, Binding::LowDisk),
            Some(want),
            "64 GiB: {free}"
        );
    }
    // The same free space on a 16 GiB Mac is judged by the fixed floors.
    r.memsize = Reading::Value(16 << 30);
    for (free, want) in [
        (21.0, Tier::Clear),
        (19.0, Tier::Elevated),
        (9.0, Tier::Severe),
    ] {
        r.free_disk = Reading::Value((free * GIB) as u64);
        assert_eq!(
            tier_of(&t, &r, Binding::LowDisk),
            Some(want),
            "16 GiB: {free}"
        );
    }
    // Memory that cannot be read: the fixed floors, no warning for it.
    r.memsize = Reading::Unavailable(Why::Failed);
    r.free_disk = Reading::Value((15.0 * GIB) as u64);
    assert_eq!(tier_of(&t, &r, Binding::LowDisk), Some(Tier::Elevated));
    // A person's own numbers always win, even the old defaults on a big Mac.
    let explicit = Thresholds {
        disk_auto: false,
        ..Thresholds::default()
    };
    r.memsize = Reading::Value(64 << 30);
    r.free_disk = Reading::Value((15.0 * GIB) as u64);
    assert_eq!(
        tier_of(&explicit, &r, Binding::LowDisk),
        Some(Tier::Elevated)
    );
    // The resolved copy is what Settings shows.
    let shown = t.resolved(Reading::Value(64 << 30));
    assert_eq!((shown.disk.elevated_at, shown.disk.severe_at), (32.0, 16.0));
    assert!(shown.disk_auto);
}

#[test]
fn automatic_floors_are_a_stored_fact_not_an_inference_from_the_numbers() {
    // Untouched: nothing stored, so a later build that changes the formula
    // reaches this person.
    assert!(Thresholds::default().to_stored().is_empty());
    assert!(Thresholds::default().is_default());

    // Settings round trip: load (resolved for display), change swap, save.
    let mut shown = Thresholds::default().resolved(Reading::Value(64 << 30));
    shown.swap.mode = InputMode::Shed;
    let stored = shown.to_stored();
    assert!(
        stored.contains_key("swap") && !stored.contains_key("disk"),
        "{stored:?}"
    );
    let back = Thresholds::from_stored(serde_json::Value::Object(stored));
    assert!(back.disk_auto, "the disk floors stayed automatic");

    // A changed mode on automatic floors is stored with `auto` and the fixed
    // numbers an older build reads, and comes back automatic.
    let mut shed = Thresholds::default();
    shed.disk.mode = InputMode::Shed;
    let stored = shed.to_stored();
    assert_eq!(
        stored["disk"],
        json!({"mode": "shed", "elevated_at": 20.0, "severe_at": 10.0, "auto": true})
    );
    let back = Thresholds::from_stored(serde_json::Value::Object(stored));
    assert!(back.disk_auto && back.disk.mode == InputMode::Shed);

    // Explicit 20/10 equals the old defaults and is still stored and still
    // theirs: on a 64 GiB Mac it differs from the automatic 32/16.
    let mut same = Thresholds::default();
    same.disk_auto = false;
    let stored = same.to_stored();
    assert_eq!(
        stored["disk"],
        json!({"mode": "refuse", "elevated_at": 20.0, "severe_at": 10.0})
    );
    let back = Thresholds::from_stored(serde_json::Value::Object(stored));
    assert!(!back.disk_auto);
    assert_eq!(back.resolved(Reading::Value(64 << 30)).disk.severe_at, 10.0);

    // A value stored by a build that predates scaling has no `auto`: explicit.
    let old = Thresholds::from_stored(
        json!({"disk": {"mode": "refuse", "elevated_at": 40.0, "severe_at": 15.0}}),
    );
    assert!(!old.disk_auto);
    assert_eq!((old.disk.elevated_at, old.disk.severe_at), (40.0, 15.0));
    // An automatic floor is never validated against numbers it does not read.
    let mut odd = Thresholds::default();
    odd.disk.severe_at = 999.0;
    assert_eq!(odd.problem(), None);
}

#[test]
fn the_disk_guards_are_validated_and_stored_apart() {
    let d = DiskGuard::default();
    assert_eq!(
        (
            d.fill.mode,
            d.fill.elevated_within_min,
            d.fill.severe_within_min
        ),
        (InputMode::Refuse, 30.0, 10.0)
    );
    assert_eq!(
        (d.coverage.mode, d.coverage.headroom_gib),
        (InputMode::Refuse, 60.0)
    );
    assert!(d.is_default() && d.to_stored().is_empty());

    let mut t = Thresholds::default();
    t.disk_guard.fill.severe_within_min = 45.0; // longer than the elevated one
    assert!(t.problem().unwrap().starts_with("disk_fill:"));
    let mut t = Thresholds::default();
    t.disk_guard.fill.elevated_within_min = f64::NAN;
    assert!(t.problem().is_some());
    let mut t = Thresholds::default();
    t.disk_guard.coverage.headroom_gib = 0.0;
    assert!(t.problem().unwrap().starts_with("coverage:"));

    let mut changed = DiskGuard::default();
    changed.fill.mode = InputMode::Shed;
    changed.coverage.headroom_gib = 90.0;
    let back = DiskGuard::from_stored(serde_json::Value::Object(changed.to_stored()));
    assert_eq!(back, changed);
    // One bad guard keeps its default and costs the other nothing.
    let lenient = DiskGuard::from_stored(json!({
        "fill": {"mode": "shed", "elevated_within_min": 5.0, "severe_within_min": 9.0},
        "coverage": {"mode": "off", "headroom_gib": 12.0},
    }));
    assert_eq!(lenient.fill, DiskGuard::default().fill);
    assert_eq!(lenient.coverage.mode, InputMode::Off);
    assert_eq!(
        DiskGuard::from_stored(json!("garbage")),
        DiskGuard::default()
    );
}

#[test]
fn the_fill_rule_modes_cap_what_the_projection_can_do() {
    // 40 GiB free, falling 4 GiB a minute: 6 minutes to the 16 GiB floor.
    let mut r = Readings::calm();
    r.free_disk = Reading::Value(40 << 30);
    r.disk_fill = Reading::Value(4 << 30);
    let mut t = Thresholds::default();
    assert_eq!(tier_of(&t, &r, Binding::DiskFilling), Some(Tier::Severe));
    t.disk_guard.fill.mode = InputMode::Shed;
    assert_eq!(tier_of(&t, &r, Binding::DiskFilling), Some(Tier::Elevated));
    t.disk_guard.fill.mode = InputMode::Off;
    assert_eq!(tier_of(&t, &r, Binding::DiskFilling), None);
    // The report still shows the fall the mode does not act on.
    assert!(t.disk_fill(&r).is_none(), "off shows nothing");
    t.disk_guard.fill.mode = InputMode::Shed;
    let fill = t.disk_fill(&r).unwrap();
    assert_eq!(fill.tier, Tier::Severe);
    assert!((fill.eta_min - 6.0).abs() < 0.01, "{}", fill.eta_min);
    // Not falling, noise-slow, or not yet known: nothing to project.
    for rate in [
        Reading::Value(0),
        Reading::Value(1 << 20),
        Reading::Unavailable(Why::NotYet),
        Reading::Unavailable(Why::Unsupported),
    ] {
        r.disk_fill = rate;
        let t = Thresholds::default();
        assert_eq!(
            tier_of(&t, &r, Binding::DiskFilling),
            Some(Tier::Clear).filter(|_| rate.value().is_some()),
            "{rate:?}"
        );
        assert!(t.assess(&r).warnings.is_empty(), "{rate:?}");
    }
    // A rate that could not be measured is said, never a pass.
    r.disk_fill = Reading::Unavailable(Why::Failed);
    assert!(Thresholds::default()
        .assess(&r)
        .warnings
        .iter()
        .any(|w| w.contains("the disk fill rate is unreadable")));
}
