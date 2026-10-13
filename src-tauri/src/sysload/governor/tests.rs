//! The verdict, tested over the replay path: a real `host-load.v1` record
//! written for a chosen sample, read back at a chosen age through the same
//! `report` every consumer uses, so the four sensor states and the
//! compressor flag arrive as they do in the app. Pure cases use hand-built
//! inputs.

use super::*;
use crate::sysload::sensor::{Compressor, HostSample, ReplaySensor, Sensor};
use crate::sysload::{
    snapshot, Clock, CompressorTrend, Reading, SnapshotWriter, WriterIdentity, SNAPSHOT_FILE_NAME,
};

const SECOND: Duration = Duration::from_secs(1);

fn clock(mono_s: u64) -> Clock {
    Clock {
        wall_ms: 1_700_000_000_000 + mono_s * 1000,
        mono_ns: mono_s * 1_000_000_000,
        boot_id: 0x5eed_b007,
    }
}

fn input(level: Level) -> Input {
    Input::test_pressure(level)
}

fn thrashing(level: Level) -> Input {
    Input {
        compressor_thrash: true,
        thrash_wakeups: Some(4),
        swapouts: Some(120),
        ..input(level)
    }
}

/// Feeds `samples` one tick (2 s) apart from `start`; returns the last
/// observation and the instant after it.
fn feed(
    governor: &mut Governor,
    start: Instant,
    samples: impl IntoIterator<Item = Input>,
) -> (Observation, Instant) {
    let mut now = start;
    let mut last = None;
    for sample in samples {
        last = Some(governor.observe(now, sample));
        now += 2 * SECOND;
    }
    (last.expect("at least one sample"), now)
}

/// Replays `samples` through the real sensor seam (`ReplaySensor`,
/// `CompressorTrend`), publishes the last into a temp record written at
/// monotonic second `written`, and reads it back at second `read`.
fn replay(counters: &[(u64, u64)], level: Level, written: u64, read: u64) -> Input {
    let dir = tempfile::tempdir().unwrap();
    let mut sensor = ReplaySensor::new(
        counters
            .iter()
            .map(|&(thrash, swapouts)| HostSample {
                pressure: Reading::Value(level),
                compressor: Reading::Value(Compressor {
                    thrashing_detected: thrash,
                    swapouts_pressure: swapouts,
                    ..Compressor::default()
                }),
                ..HostSample::default()
            })
            .collect(),
    );
    let mut trend = CompressorTrend::default();
    let mut snap = snapshot::Snapshot::empty(
        clock(written),
        WriterIdentity {
            pid: 1,
            start_us: 1,
            version: "test".into(),
            path: "/test".into(),
        },
        true,
    );
    snap.platform = Platform::Governed;
    for _ in counters {
        snap.host = sensor.sample();
        let (delta, thrash) = trend.observe(snap.host.compressor);
        snap.compressor_delta = delta;
        snap.compressor_thrash = thrash;
    }
    let mut writer = SnapshotWriter::elect(dir.path()).unwrap().unwrap();
    writer.publish(&snap).unwrap();
    let (report, snap) = snapshot::report(&dir.path().join(SNAPSHOT_FILE_NAME), clock(read));
    Input::from_report(&report, snap.as_ref(), true)
}

#[test]
fn the_never_gated_verbs_have_no_site() {
    let names: Vec<&str> = Site::ALL.iter().map(|s| s.name()).collect();
    assert_eq!(names, ["checkout_probe", "ensure_room", "pty_spawn"]);
    for verb in [
        "stop",
        "kill",
        "cancel",
        "save",
        "commit",
        "release",
        "reconcile",
    ] {
        assert!(
            !names.iter().any(|name| name.contains(verb)),
            "{verb} must never be a gated site"
        );
    }
}

#[test]
fn memory_pressure_critical_refuses_launches_and_sheds_both_classes_naming_the_value() {
    let mut governor = Governor::default();
    let (seen, _) = feed(
        &mut governor,
        Instant::now(),
        vec![input(Level::Critical); 3],
    );
    let v = seen.verdict;
    assert!(!v.admit);
    assert_eq!(v.reason, Reason::Pressure);
    assert_eq!(v.binding, Some(Binding::MemoryPressure));
    assert_eq!(
        v.shed,
        Shed {
            background: true,
            reactive: true
        }
    );
    assert!(
        v.message.contains("critical (kernel level 4)"),
        "{}",
        v.message
    );
}

#[test]
fn memory_pressure_warn_sheds_background_only_and_still_admits() {
    let mut governor = Governor::default();
    let (seen, _) = feed(&mut governor, Instant::now(), vec![input(Level::Warn); 3]);
    let v = seen.verdict;
    assert!(v.admit);
    assert_eq!(v.reason, Reason::Elevated);
    assert_eq!(
        v.shed,
        Shed {
            background: true,
            reactive: false
        }
    );
}

#[test]
fn thrash_with_normal_pressure_refuses_and_names_the_compressor() {
    // The 2026-10-07 shape: the level lagged while the compressor ran out.
    let mut governor = Governor::default();
    let (seen, _) = feed(
        &mut governor,
        Instant::now(),
        vec![thrashing(Level::Normal); 3],
    );
    let v = seen.verdict;
    assert!(!v.admit, "{v:?}");
    assert_eq!(v.binding, Some(Binding::CompressorThrash));
    assert!(v.shed.background && v.shed.reactive);
    assert!(
        v.message.contains("compressor is thrashing"),
        "{}",
        v.message
    );
    assert!(
        v.message.contains("+4 thrash wakeups") && v.message.contains("+120 pressure swapouts"),
        "the values travel with the reason: {}",
        v.message
    );
    assert!(v.message.contains("normal (kernel level 1)"));
}

#[test]
fn replayed_rising_counters_raise_thrash_and_the_verdict_refuses_on_it() {
    // Four rising samples: the sensor's own three-sample rule, then ours.
    let rising = [(0, 0), (1, 10), (2, 20), (3, 30)];
    let at = replay(&rising, Level::Normal, 100, 105);
    assert!(at.compressor_thrash, "the replay must raise the flag");
    let mut governor = Governor::default();
    let (seen, _) = feed(&mut governor, Instant::now(), vec![at; 3]);
    assert!(!seen.verdict.admit);
    assert_eq!(seen.verdict.binding, Some(Binding::CompressorThrash));
    // A quiet replay does not.
    let quiet = replay(&[(0, 0), (0, 0), (0, 0), (0, 0)], Level::Normal, 100, 105);
    assert!(!quiet.compressor_thrash);
    let (seen, _) = feed(&mut Governor::default(), Instant::now(), vec![quiet; 6]);
    assert!(seen.verdict.admit && seen.verdict.shed == Shed::default());
}

#[test]
fn a_tier_is_entered_after_three_samples_and_left_after_five() {
    let mut governor = Governor::default();
    let t0 = Instant::now();
    let (two, t) = feed(&mut governor, t0, vec![input(Level::Critical); 2]);
    assert!(two.verdict.admit, "two samples are not sustained");
    let (three, t) = feed(&mut governor, t, vec![input(Level::Critical)]);
    assert!(!three.verdict.admit, "the third enters the tier");
    let (four_clear, t) = feed(&mut governor, t, vec![input(Level::Normal); 4]);
    assert!(
        !four_clear.verdict.admit,
        "four clear samples do not leave it"
    );
    let (fifth, _) = feed(&mut governor, t, vec![input(Level::Normal)]);
    assert!(fifth.verdict.admit && fifth.verdict.shed == Shed::default());
}

#[test]
fn a_level_flapping_at_the_threshold_neither_trips_nor_releases_the_verdict() {
    let t0 = Instant::now();
    // Flapping around critical from clear: never three in a row.
    let mut governor = Governor::default();
    let flap = (0..60).map(|i| {
        if i % 2 == 0 {
            input(Level::Critical)
        } else {
            input(Level::Normal)
        }
    });
    let (seen, _) = feed(&mut governor, t0, flap);
    assert!(
        seen.verdict.admit && seen.verdict.shed == Shed::default(),
        "{seen:?}"
    );

    // Flapping around warn from clear: no shedding either.
    let mut governor = Governor::default();
    let flap = (0..60).map(|i| {
        if i % 2 == 0 {
            input(Level::Warn)
        } else {
            input(Level::Normal)
        }
    });
    let (seen, _) = feed(&mut governor, t0, flap);
    assert!(seen.verdict.shed == Shed::default());

    // Once severe is held, a flap (critical / normal) does not release it.
    let mut governor = Governor::default();
    let (_, t) = feed(&mut governor, t0, vec![input(Level::Critical); 3]);
    let flap = (0..60).map(|i| {
        if i % 2 == 0 {
            input(Level::Normal)
        } else {
            input(Level::Critical)
        }
    });
    let (seen, _) = feed(&mut governor, t, flap);
    assert!(!seen.verdict.admit, "held across the flap");

    // Critical/warn alternating from clear enters elevated, never severe.
    let mut governor = Governor::default();
    let flap = (0..60).map(|i| {
        if i % 2 == 0 {
            input(Level::Warn)
        } else {
            input(Level::Critical)
        }
    });
    let (seen, _) = feed(&mut governor, t0, flap);
    assert_eq!(seen.verdict.tier, Tier::Elevated);
    assert!(seen.verdict.admit);
}

#[test]
fn stale_recent_governs_to_exactly_120_seconds_and_older_fails_open() {
    // Replayed: a critical, thrashing snapshot written at second 100.
    let rising = [(0, 0), (1, 10), (2, 20), (3, 30)];
    let t0 = Instant::now();
    let mut governor = Governor::default();
    let fresh = replay(&rising, Level::Critical, 100, 105);
    let (_, _) = feed(&mut governor, t0, vec![fresh; 3]);

    // Exactly 120 s old: stale-recent, still governing, with the warning.
    let at_120 = replay(&rising, Level::Critical, 100, 220);
    assert_eq!(at_120.state, SensorState::StaleRecent);
    let (seen, _) = feed(&mut governor, t0 + 10 * SECOND, vec![at_120.clone()]);
    assert!(!seen.verdict.admit, "{:?}", seen.verdict);
    assert!(
        seen.verdict
            .warnings
            .iter()
            .any(|w| w.starts_with("stale 120s")),
        "{:?}",
        seen.verdict.warnings
    );

    // 120.001 s is stale-old: admitted with a warning, nothing shed.
    let mut over = at_120.clone();
    over.age_ms = Some(120_001);
    over.state = SensorState::from_age_ms(120_001);
    let (seen, _) = feed(&mut governor, t0 + 12 * SECOND, vec![over]);
    assert!(seen.verdict.admit);
    assert_eq!(seen.verdict.reason, Reason::SensorUnavailable);
    assert_eq!(seen.verdict.shed, Shed::default());
    assert!(!seen.verdict.warnings.is_empty());

    // Dead (601 s) and a snapshot from before this boot: the same.
    let dead = replay(&rising, Level::Critical, 100, 701);
    assert_eq!(dead.state, SensorState::Dead);
    let (seen, _) = feed(&mut Governor::default(), t0, vec![dead; 4]);
    assert!(seen.verdict.admit && seen.verdict.reason == Reason::SensorUnavailable);
}

#[test]
fn a_sampler_that_stops_ticking_walks_the_verdict_out_of_governing() {
    let t0 = Instant::now();
    let mut governor = Governor::default();
    let (seen, last) = feed(&mut governor, t0, vec![input(Level::Critical); 3]);
    assert!(!seen.verdict.admit);
    let last = last - 2 * SECOND;
    // 60 s on, no tick: stale-recent, still refusing, says so.
    let v = governor.verdict(last + 60 * SECOND);
    assert!(!v.admit);
    assert!(
        v.warnings.iter().any(|w| w.starts_with("stale 60s")),
        "{:?}",
        v.warnings
    );
    // 200 s on: stale-old, admitted with a warning.
    let v = governor.verdict(last + 200 * SECOND);
    assert!(v.admit && v.reason == Reason::SensorUnavailable, "{v:?}");
    // 700 s on: dead, still admitted.
    assert!(governor.verdict(last + 700 * SECOND).admit);
}

#[test]
fn the_clock_going_backwards_changes_nothing() {
    let t0 = Instant::now() + 100 * SECOND;
    let mut governor = Governor::default();
    let (_, next) = feed(&mut governor, t0, vec![input(Level::Critical); 3]);
    // An observation dated before the previous one is clamped to it.
    let earlier = t0 - 50 * SECOND;
    let seen = governor.observe(earlier, input(Level::Critical));
    assert!(!seen.verdict.admit);
    assert!(governor.report(earlier).refusing_for_s.unwrap() < 5);
    // Asking the verdict at an earlier time ages nothing negative.
    let v = governor.verdict(earlier);
    assert_eq!(v.age_ms, Some(0));
    let _ = next;
    // A snapshot dated after now has no age, so the sensor is dead: fail open.
    let future = replay(&[(0, 0)], Level::Critical, 500, 100);
    assert_eq!(future.state, SensorState::Dead);
    assert!(
        Governor::instant(Instant::now(), future)
            .verdict(Instant::now())
            .admit
    );
}

#[test]
fn refusal_raises_its_notice_at_thirty_minutes_and_not_at_twenty_nine_fifty_nine() {
    let t0 = Instant::now();
    let mut governor = Governor::default();
    let (_, _) = feed(&mut governor, t0, vec![input(Level::Critical); 3]);
    // The third sample (t0 + 4 s) is the first refusing one.
    let since = t0 + 4 * SECOND;
    let just_under = governor.observe(
        since + REFUSAL_NOTICE_AFTER - SECOND,
        input(Level::Critical),
    );
    assert!(!just_under.notice_raised, "29:59");
    assert!(
        !governor
            .report(since + REFUSAL_NOTICE_AFTER - SECOND)
            .notice
    );
    let exactly = governor.observe(since + REFUSAL_NOTICE_AFTER, input(Level::Critical));
    assert!(exactly.notice_raised, "30:00");
    let again = governor.observe(
        since + REFUSAL_NOTICE_AFTER + 2 * SECOND,
        input(Level::Critical),
    );
    assert!(!again.notice_raised, "raised once per run");
    assert!(
        governor
            .report(since + REFUSAL_NOTICE_AFTER + 2 * SECOND)
            .notice
    );
}

#[test]
fn a_refusal_never_silently_becomes_an_allow() {
    let t0 = Instant::now();
    let mut governor = Governor::default();
    let (_, _) = feed(&mut governor, t0, vec![input(Level::Critical); 3]);
    // Four hours of continuous pressure, observed every minute: every
    // verdict refuses, and the notice stays up from minute 30.
    for minute in 1..=240u64 {
        let now = t0 + 4 * SECOND + Duration::from_secs(60 * minute);
        let seen = governor.observe(now, input(Level::Critical));
        assert!(!seen.verdict.admit, "minute {minute} became an allow");
        let report = governor.report(now);
        assert_eq!(report.notice, minute >= 30, "minute {minute}");
        assert!(
            admit_on(Site::PtySpawn, &report).is_err(),
            "minute {minute}"
        );
    }
    // Pressure clearing for five samples ends the run and the notice.
    let now = t0 + Duration::from_secs(60 * 241);
    let (released, _) = feed(&mut governor, now, vec![input(Level::Normal); 5]);
    assert!(released.verdict.admit);
    assert!(!governor.report(now + 10 * SECOND).notice);
    // A sensor that goes away mid-run admits, and says why — it is not the
    // quiet kind of allow.
    let mut governor = Governor::default();
    let (_, t) = feed(&mut governor, t0, vec![input(Level::Critical); 3]);
    let lost = Input {
        state: SensorState::Dead,
        age_ms: Some(700_000),
        ..input(Level::Critical)
    };
    let (seen, _) = feed(&mut governor, t, vec![lost]);
    assert!(seen.verdict.admit && seen.verdict.reason == Reason::SensorUnavailable);
    assert!(!seen.verdict.warnings.is_empty());
}

#[test]
fn only_a_person_admits_what_pressure_refuses_and_only_for_the_window() {
    let t0 = Instant::now();
    let mut governor = Governor::default();
    let (_, t) = feed(&mut governor, t0, vec![input(Level::Critical); 3]);
    let refused = admit_on(Site::CheckoutProbe, &governor.report(t)).unwrap_err();
    assert_eq!(refused.site, Site::CheckoutProbe);
    assert!(refused.message().contains("Admit anyway"));
    governor.grant_override(t);
    // The sensor keeps ticking while the override lives, as it does in the app.
    let tick = |governor: &mut Governor, at: Instant| {
        governor.observe(at, input(Level::Critical));
        governor.report(at)
    };
    let live = tick(&mut governor, t + 119 * SECOND);
    let admitted = admit_on(Site::PtySpawn, &live).unwrap();
    assert!(admitted.overridden);
    // The override does not stop shedding: it is for the launch, not the load.
    assert!(live.verdict.shed.reactive);
    let expired = tick(&mut governor, t + OVERRIDE_WINDOW);
    assert!(admit_on(Site::PtySpawn, &expired).is_err());
}

#[test]
fn switching_launch_refusal_off_admits_but_keeps_shedding_and_says_so() {
    let mut governor = Governor::default();
    let ungated = Input {
        govern_launches: false,
        ..input(Level::Critical)
    };
    let (seen, _) = feed(&mut governor, Instant::now(), vec![ungated; 3]);
    assert!(seen.verdict.admit);
    assert_eq!(seen.verdict.reason, Reason::LaunchGateOff);
    assert!(seen.verdict.shed.reactive && seen.verdict.shed.background);
    assert!(seen
        .verdict
        .warnings
        .iter()
        .any(|w| w.contains("switched off")));
}

#[test]
fn an_ungoverned_process_platform_or_switch_admits_without_shedding() {
    let off = Input {
        enabled: false,
        ..input(Level::Critical)
    };
    let (seen, _) = feed(&mut Governor::default(), Instant::now(), vec![off; 4]);
    assert!(seen.verdict.admit && seen.verdict.reason == Reason::SwitchedOff);
    assert_eq!(seen.verdict.shed, Shed::default());

    let linux = Input {
        platform: Platform::Advise,
        ..input(Level::Critical)
    };
    let (seen, _) = feed(&mut Governor::default(), Instant::now(), vec![linux; 4]);
    assert!(seen.verdict.admit && seen.verdict.reason == Reason::NotGoverned);

    let unreadable = Input {
        level: None,
        ..input(Level::Critical)
    };
    let (seen, _) = feed(
        &mut Governor::default(),
        Instant::now(),
        vec![unreadable; 4],
    );
    assert!(seen.verdict.admit && seen.verdict.reason == Reason::SensorUnavailable);

    // No sample yet: fail open, with a warning.
    let v = Governor::default().verdict(Instant::now());
    assert!(v.admit && !v.warnings.is_empty());
    // This test process runs no governor at all.
    assert_eq!(report().verdict.reason, Reason::NotGoverned);
    assert_eq!(shed(), Shed::default());
}

#[test]
fn concurrent_admits_each_agree_with_the_verdict_they_read() {
    let governor = Arc::new(Mutex::new(Governor::default()));
    let t0 = Instant::now();
    feed(
        &mut governor.lock().unwrap(),
        t0,
        vec![input(Level::Critical); 3],
    );
    let feeder = {
        let governor = governor.clone();
        std::thread::spawn(move || {
            for i in 0..400u64 {
                governor
                    .lock()
                    .unwrap()
                    .observe(t0 + Duration::from_millis(10 * i), input(Level::Critical));
            }
        })
    };
    let admitters: Vec<_> = (0..8)
        .map(|_| {
            let governor = governor.clone();
            std::thread::spawn(move || {
                let mut refused = 0;
                for _ in 0..400 {
                    let report = governor.lock().unwrap().report(Instant::now());
                    let decision = admit_on(Site::PtySpawn, &report);
                    assert_eq!(decision.is_ok(), report.verdict.admit);
                    refused += usize::from(decision.is_err());
                }
                refused
            })
        })
        .collect();
    feeder.join().unwrap();
    let refused: usize = admitters.into_iter().map(|t| t.join().unwrap()).sum();
    assert_eq!(refused, 8 * 400, "sustained pressure admitted a launch");
}

#[test]
fn a_decision_is_recorded_in_the_ledger_with_its_binding_and_value() {
    let repo = tempfile::tempdir().unwrap();
    let path = repo.path().to_str().unwrap();
    {
        let _pressed = inject(input(Level::Critical));
        let refused = admit(Site::CheckoutProbe, path).unwrap_err();
        assert!(refused
            .verdict
            .message
            .contains("critical (kernel level 4)"));
    }
    {
        let _quiet = inject(input(Level::Normal));
        admit(Site::PtySpawn, path).unwrap();
    }
    {
        let _blind = inject(Input {
            state: SensorState::Dead,
            age_ms: Some(900_000),
            ..input(Level::Critical)
        });
        let admitted = admit(Site::EnsureRoom, path).unwrap();
        assert!(admitted.warning.is_some());
    }
    let events = crate::ledger::tail(path, 0, 20).unwrap();
    let actions: Vec<&str> = events.iter().map(|e| e.action.as_str()).collect();
    assert_eq!(
        actions,
        [
            "host_load.refuse",
            "host_load.admit",
            "host_load.sensor_unavailable"
        ]
    );
    let refusal = &events[0];
    assert_eq!(refusal.outcome, "blocked");
    let detail = refusal.detail_json.as_deref().unwrap();
    assert!(
        detail.contains("memory_pressure") && detail.contains("checkout_probe"),
        "{detail}"
    );
}

/// A reader whose writer stalled rereads one record every tick. One critical
/// sample read many times is still one sample: it must not enter the tier
/// three samples enter, and the stale walk still fails it open.
#[test]
fn one_sample_reread_never_trips_the_hysteresis() {
    let t0 = Instant::now();
    let mut governor = Governor::default();
    governor.observe(t0, input(Level::Critical));
    for tick in 1..=10u64 {
        let reread = Input {
            state: SensorState::StaleRecent,
            age_ms: Some(tick * 2000),
            ..input(Level::Critical)
        };
        let seen = governor.observe_repeat(t0 + SECOND * (2 * tick) as u32, reread);
        assert!(seen.verdict.admit, "tick {tick}: {:?}", seen.verdict);
        assert_eq!(seen.verdict.tier, Tier::Clear, "tick {tick}");
    }
    // Two genuinely new critical samples make three: entered.
    let (seen, _) = feed(&mut governor, t0 + 30 * SECOND, vec![input(Level::Critical); 2]);
    assert!(!seen.verdict.admit, "{:?}", seen.verdict);
    // While refusing, a reread keeps the tier and the refusal clock running.
    let reread = governor.observe_repeat(t0 + 40 * SECOND, input(Level::Critical));
    assert!(!reread.verdict.admit);
    assert!(governor.report(t0 + 40 * SECOND).refusing_for_s.is_some());
    // And a reread the stale walk made ungovernable fails open.
    let old = Input {
        state: SensorState::StaleOld,
        age_ms: Some(130_000),
        ..input(Level::Critical)
    };
    let seen = governor.observe_repeat(t0 + 170 * SECOND, old);
    assert!(seen.verdict.admit && seen.verdict.reason == Reason::SensorUnavailable);
}

/// A sampler that stops ticking mid-refusal walks the verdict to "sensor
/// unavailable": the report must not keep saying launches have been refused
/// for ever-longer, nor keep the 30-minute notice up beside an admit.
#[test]
fn a_stopped_sampler_ends_the_refusal_run_in_the_report() {
    let t0 = Instant::now();
    let mut governor = Governor::default();
    let (_, t) = feed(&mut governor, t0, vec![input(Level::Critical); 3]);
    let later = t + REFUSAL_NOTICE_AFTER;
    assert!(governor.observe(later, input(Level::Critical)).notice_raised);
    assert!(governor.report(later).notice);
    let quiet = governor.report(later + 200 * SECOND);
    assert!(quiet.verdict.admit, "{:?}", quiet.verdict);
    assert_eq!(quiet.refusing_for_s, None);
    assert!(!quiet.notice);
}

/// Hysteresis is symmetric: a run enters the lowest tier it held throughout
/// and leaves to the highest. Leaving severe on four warn samples and a clear
/// one lands on elevated (background still shed), not on clear; and the tier
/// entered carries the binding of the sample that set it.
#[test]
fn a_run_leaves_to_the_highest_tier_it_held_and_enters_with_its_binding() {
    let t0 = Instant::now();
    let mut governor = Governor::default();
    let (_, t) = feed(&mut governor, t0, vec![input(Level::Critical); 3]);
    let mut run = vec![input(Level::Warn); 4];
    run.push(input(Level::Normal));
    let (seen, _) = feed(&mut governor, t, run);
    assert!(seen.verdict.admit);
    assert_eq!(seen.verdict.tier, Tier::Elevated, "{:?}", seen.verdict);
    assert!(seen.verdict.shed.background && !seen.verdict.shed.reactive);

    // Warn, critical, thrash: the run enters elevated, bound by the pressure
    // level that put it there — not by the compressor of the last sample.
    let mut governor = Governor::default();
    let (seen, _) = feed(
        &mut governor,
        t0,
        vec![
            input(Level::Warn),
            input(Level::Critical),
            thrashing(Level::Normal),
        ],
    );
    assert_eq!(seen.verdict.tier, Tier::Elevated);
    assert_eq!(seen.verdict.binding, Some(Binding::MemoryPressure));
}
