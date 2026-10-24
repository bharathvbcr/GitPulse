//! What a connected same-uid peer can no longer do: report for a session the
//! terminal registry does not hold, or faster than a session can.
//!
//! These drive the real [`Intake`], the real worker loop and the real socket
//! handler, with a host whose registry the test controls.

use super::limits::{self, Limits};
use super::*;
use crate::tool_config::SessionAlertSettings;
use std::collections::HashSet;
use std::thread::JoinHandle;
use std::time::Instant;

/// A host whose terminal registry is a set the test edits.
#[derive(Default)]
struct Registry {
    live: Mutex<HashSet<String>>,
}

impl Registry {
    fn register(&self, key: &str) {
        self.live.lock().unwrap().insert(key.to_owned());
    }
    fn remove(&self, key: &str) {
        self.live.lock().unwrap().remove(key);
    }
}

impl Host for Registry {
    fn submit(&self, _: &str, _: &str, _: &str, _: bool) -> Result<bool, String> {
        Ok(true)
    }
    fn attended(&self, _: &str) -> bool {
        false
    }
    fn knows(&self, key: &str) -> bool {
        self.live.lock().unwrap().contains(key)
    }
    fn config(&self) -> SessionAlertSettings {
        SessionAlertSettings {
            enabled: true,
            ..SessionAlertSettings::default()
        }
    }
    fn minute(&self) -> Option<u16> {
        Some(12 * 60)
    }
}

struct Rig {
    intake: Intake,
    registry: Arc<Registry>,
    standing: Arc<Mutex<Vec<StandingAttention>>>,
    worker: JoinHandle<()>,
}

impl Rig {
    fn new() -> Self {
        let counters = Arc::new(Counters::default());
        let registry = Arc::new(Registry::default());
        let (intake, receiver) = Intake::channel(counters.clone());
        let intake = intake.guarded_by(registry.clone());
        let bells = intake.bells();
        let standing = Arc::new(Mutex::new(Vec::new()));
        let worker = {
            let (standing, host) = (standing.clone(), registry.clone());
            std::thread::spawn(move || {
                run(receiver, bells, host, counters, Arc::default(), standing)
            })
        };
        Self {
            intake,
            registry,
            standing,
            worker,
        }
    }

    /// Closes the queue, lets the worker act on everything it holds, and
    /// returns the sessions that stand asking.
    fn finish(self) -> Vec<String> {
        let Self {
            intake,
            standing,
            worker,
            ..
        } = self;
        drop(intake);
        worker.join().unwrap();
        let mut standing: Vec<String> = standing
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.session.clone())
            .collect();
        standing.sort();
        standing
    }
}

/// A hook report as the socket parses it.
fn report(event: &str, session: &str) -> (Notice, Option<u64>) {
    let json = format!(r#"{{"v":1,"event":"{event}","agent":"claude","session":"{session}"}}"#);
    bridge::parse_report_stamped(json.as_bytes()).expect("a well-formed report")
}

/// A report from outside any terminal, for the place named `place`.
fn outside(place: &str) -> Notice {
    let json =
        format!(r#"{{"v":1,"event":"permission_prompt","agent":"claude","cwd":"/work/{place}"}}"#);
    bridge::parse_report_stamped(json.as_bytes()).unwrap().0
}

/// What the PTY's own reader says when its child exits.
fn pty_exit(key: &str) -> Notice {
    Notice {
        origin: Origin::Terminal,
        channel: "pty",
        ..report("session_ended", key).0
    }
}

const REAL: [&str; 3] = ["term-1-a", "term-1-b", "term-1-c"];

#[test]
fn a_refused_unknown_report_leaves_nothing_so_a_session_registered_a_moment_later_is_heard() {
    let rig = Rig::new();
    let (notice, _) = report("permission_request", REAL[0]);
    // The window between the PTY being spawned and being listed: refused, and
    // not remembered, so the same report (same stamp) is taken once it is.
    assert_eq!(
        rig.intake.offer_stamped(notice.clone(), 100),
        Offer::UnknownSession
    );
    rig.registry.register(REAL[0]);
    assert_eq!(rig.intake.offer_stamped(notice, 100), Offer::Accepted);
    assert_eq!(rig.finish(), [REAL[0]]);
}

#[test]
fn a_report_after_the_pty_exited_is_stale_when_older_and_unknown_when_newer() {
    let rig = Rig::new();
    rig.registry.register(REAL[0]);
    assert_eq!(
        rig.intake
            .offer_stamped(report("permission_request", REAL[0]).0, 10),
        Offer::Accepted
    );
    // The PTY exits: its own end report is ordered into the book although the
    // registry no longer lists the session.
    rig.registry.remove(REAL[0]);
    assert_eq!(
        rig.intake.offer_stamped(pty_exit(REAL[0]), 50),
        Offer::Accepted
    );
    // Sent before the end, arrived after: stale, as before this change.
    assert_eq!(
        rig.intake
            .offer_stamped(report("idle_prompt", REAL[0]).0, 40),
        Offer::Stale
    );
    // Sent after the end for a session that is gone: nothing to be about.
    assert_eq!(
        rig.intake
            .offer_stamped(report("idle_prompt", REAL[0]).0, 60),
        Offer::UnknownSession
    );
    assert!(rig.finish().is_empty());
}

#[test]
fn a_bell_from_the_pty_itself_needs_no_registry_entry() {
    let rig = Rig::new();
    let bell = Notice {
        origin: Origin::Terminal,
        channel: "bell",
        event: None,
        ..report("idle_prompt", "term-9-9").0
    };
    assert_eq!(rig.intake.offer(bell), Offer::Accepted);
    drop(rig.finish());
}

#[test]
fn a_thousand_invented_sessions_evict_no_real_standing() {
    let rig = Rig::new();
    for (index, key) in REAL.iter().enumerate() {
        rig.registry.register(key);
        let (notice, _) = report("permission_request", key);
        assert_eq!(
            rig.intake.offer_stamped(notice, 1 + index as u64),
            Offer::Accepted
        );
    }
    let mut refused = 0;
    for index in 0..1000u64 {
        let (notice, _) = report("permission_prompt", &format!("term-{index}-ff"));
        if rig.intake.offer_stamped(notice, 1000 + index) == Offer::UnknownSession {
            refused += 1;
        }
    }
    assert_eq!(refused, 1000);
    let mut real = REAL.to_vec();
    real.sort_unstable();
    assert_eq!(rig.finish(), real);
}

#[test]
fn reports_from_outside_any_terminal_share_one_bucket_and_go_first_when_room_is_short() {
    let rig = Rig::new();
    for (index, key) in REAL.iter().enumerate() {
        rig.registry.register(key);
        let (notice, _) = report("permission_request", key);
        rig.intake.offer_stamped(notice, 1 + index as u64);
    }
    let mut taken = 0;
    let mut limited = 0;
    let now = Instant::now();
    for index in 0..1000u64 {
        match rig
            .intake
            .offer_stamped_at(outside(&format!("proj{index}")), 1000 + index, now)
        {
            Offer::RateLimited => limited += 1,
            Offer::Accepted | Offer::Coalesced | Offer::QueueFull => taken += 1,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(taken, limits::OUTSIDE_BURST as usize);
    assert_eq!(limited, 1000 - taken);
    let mut real = REAL.to_vec();
    real.sort_unstable();
    let standing: Vec<String> = rig
        .finish()
        .into_iter()
        .filter(|session| !session.starts_with("hook-"))
        .collect();
    assert_eq!(standing, real);
}

#[test]
fn a_session_may_burst_then_is_held_to_its_rate_and_others_are_unaffected() {
    let rig = Rig::new();
    rig.registry.register(REAL[0]);
    rig.registry.register(REAL[1]);
    let t0 = Instant::now();
    let mut stamp = 0u64;
    let mut send = |key: &str, at: Instant| {
        stamp += 2_000_000_000;
        rig.intake
            .offer_stamped_at(report("tool_finished", key).0, stamp, at)
    };
    let burst = limits::SESSION_BURST as usize;
    let passed = (0..burst + 50)
        .filter(|_| send(REAL[0], t0) != Offer::RateLimited)
        .count();
    assert_eq!(
        passed, burst,
        "a burst a real agent can reach was refused, or none was"
    );
    assert_eq!(send(REAL[0], t0), Offer::RateLimited);
    assert_ne!(
        send(REAL[1], t0),
        Offer::RateLimited,
        "one session's flood spent another's tokens"
    );
    // A second later the bucket has regained `SESSION_PER_SEC`, no more.
    let later = t0 + std::time::Duration::from_secs(1);
    let regained = (0..200)
        .filter(|_| send(REAL[0], later) != Offer::RateLimited)
        .count();
    assert_eq!(regained, limits::SESSION_PER_SEC as usize);
    drop(rig.finish());
}

#[test]
fn the_bucket_map_is_bounded_and_evicts_the_least_recent() {
    let mut limits = Limits::default();
    let t0 = Instant::now();
    for index in 0..limits::CAPACITY * 4 {
        let at = t0 + std::time::Duration::from_millis(index as u64);
        assert!(limits.admit(&format!("term-{index}"), false, at));
        assert!(limits.len() <= limits::CAPACITY);
    }
    assert_eq!(limits.len(), limits::CAPACITY);
    limits.forget(&format!("term-{}", limits::CAPACITY * 4 - 1));
    assert_eq!(limits.len(), limits::CAPACITY - 1);
}
