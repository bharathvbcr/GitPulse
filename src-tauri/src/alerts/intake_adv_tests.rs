//! Adversarial checks on the intake: the order book's tombstones and bounds,
//! the queue reservation, and whether every counter still reconciles when
//! bells, hook reports and answers all arrive at once.
//!
//! Written by a reviewer who did not write the intake. Each test states the
//! claim it attacks; one that fails is a claim that is false.

use super::*;
use crate::tool_config::SessionAlertSettings;

#[derive(Default)]
struct Probe;

impl Host for Probe {
    fn submit(&self, _: &str, _: &str, _: &str, _: bool) -> Result<bool, String> {
        Ok(true)
    }
    fn attended(&self, _: &str) -> bool {
        false
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

fn bell(session: &str, detail: Option<&str>) -> Notice {
    Notice {
        key: session.to_owned(),
        origin: Origin::Terminal,
        label: "Claude Code".into(),
        place: None,
        event: None,
        detail: detail.map(str::to_owned),
        is_agent: true,
        channel: "bell",
        subject: None,
        subagent: false,
    }
}

fn answer(session: &str) -> Notice {
    Notice {
        origin: Origin::Reader,
        channel: "reader",
        event: Some("prompt_submitted"),
        ..bell(session, None)
    }
}

fn hook(event: &str, session: &str) -> Notice {
    let json = format!(r#"{{"v":1,"event":"{event}","agent":"claude","session":"{session}"}}"#);
    bridge::parse_report_stamped(json.as_bytes())
        .expect("a well-formed report")
        .0
}

fn counter(counters: &Counters) -> SessionAlertStatus {
    let c = counters;
    SessionAlertStatus {
        delivered: c.delivered.load(Ordering::Relaxed),
        suppressed_disabled: c.suppressed_disabled.load(Ordering::Relaxed),
        suppressed_not_agent: c.suppressed_not_agent.load(Ordering::Relaxed),
        suppressed_attended: c.suppressed_attended.load(Ordering::Relaxed),
        suppressed_quiet: c.suppressed_quiet.load(Ordering::Relaxed),
        coalesced: c.coalesced.load(Ordering::Relaxed),
        resolved: c.resolved.load(Ordering::Relaxed),
        displaced: c.displaced.load(Ordering::Relaxed),
        rate_limited: c.rate_limited.load(Ordering::Relaxed),
        failed: c.failed.load(Ordering::Relaxed),
        dropped_queue: c.dropped_queue.load(Ordering::Relaxed),
        stale: c.stale.load(Ordering::Relaxed),
        ..SessionAlertStatus::default()
    }
}

fn accounted(s: &SessionAlertStatus) -> u64 {
    s.delivered
        + s.suppressed_disabled
        + s.suppressed_not_agent
        + s.suppressed_attended
        + s.suppressed_quiet
        + s.coalesced
        + s.resolved
        + s.displaced
        + s.rate_limited
        + s.failed
        + s.dropped_queue
        + s.stale
}

/// A live intake with the real worker, closed and joined by `finish`.
struct Rig {
    intake: Arc<Intake>,
    counters: Arc<Counters>,
    worker: std::thread::JoinHandle<()>,
}

impl Rig {
    fn new() -> Self {
        let counters = Arc::new(Counters::default());
        let (intake, receiver) = Intake::channel(counters.clone());
        let bells = intake.bells();
        let worker = {
            let counters = counters.clone();
            std::thread::spawn(move || {
                run(
                    receiver,
                    bells,
                    Arc::new(Probe),
                    counters,
                    Arc::default(),
                    Arc::default(),
                )
            })
        };
        Self {
            intake: Arc::new(intake),
            counters,
            worker,
        }
    }

    fn finish(self) -> SessionAlertStatus {
        let Self {
            intake,
            counters,
            worker,
        } = self;
        drop(intake);
        worker.join().unwrap();
        counter(&counters)
    }
}

/* ── the order book ───────────────────────────────────────────────────────── */

/// A key reused after `session_ended`: the new run is not blocked by the old
/// run's tombstone, however many times it ends and restarts, and a straggler
/// from an earlier run never revives a later one.
#[test]
fn a_reused_session_key_is_not_blocked_by_its_tombstone() {
    let mut book = OrderBook::default();
    assert!(book.admit("term-1-a", 100, false));
    assert!(book.admit("term-1-a", 200, true), "the end itself");
    assert!(!book.admit("term-1-a", 150, false), "a straggler of run 1");
    for run in 1..50u64 {
        let base = 200 * run;
        assert!(
            book.admit("term-1-a", base + 10, false),
            "run {run}: the new run's first report is refused by the tombstone"
        );
        assert!(!book.admit("term-1-a", base - 1, false), "run {run}: late");
        assert!(book.admit("term-1-a", base + 100, true), "run {run}: end");
    }
    assert!(
        book.newest.is_empty(),
        "an ended session keeps a live entry"
    );
    assert_eq!(book.ended.len(), 1);
}

/// Keys differing only in case are different sessions (a session id is ASCII
/// and compared exactly, here and in the notifier), so ending one does not
/// tombstone the other.
#[test]
fn keys_differing_in_case_do_not_share_an_order_book_slot() {
    let mut book = OrderBook::default();
    assert!(book.admit("Term-1", 100, true));
    assert!(book.admit("term-1", 50, false), "the other key was blocked");
    assert!(book.admit("TERM-1", 10, false));
    // A non-ASCII (or normalisation-variant) id never reaches the book: the
    // parser refuses it as a session, so it cannot alias an ASCII one.
    for key in ["term-\u{e9}", "term-e\u{301}", "term-\u{ff11}"] {
        assert!(!bridge::valid_key(key), "{key:?} was accepted as a key");
    }
}

/// 10k distinct sessions, then ten thousand endings: both maps stay at the
/// cap, a tombstone flood does not push out a live session, and the newest
/// sessions' high-water marks still work after the churn.
#[test]
fn ten_thousand_sessions_stay_bounded_and_the_recent_ones_correct() {
    let mut book = OrderBook::default();
    // 100 real sessions, running.
    for n in 0..100u64 {
        assert!(book.admit(&format!("live-{n}"), 1_000_000 + n, false));
    }
    let started = Instant::now();
    // A flood of sessions that start and end, stamps newer than the live ones.
    for n in 0..10_000u64 {
        let key = format!("gone-{n}");
        assert!(book.admit(&key, 2_000_000 + 2 * n, false));
        assert!(book.admit(&key, 2_000_001 + 2 * n, true));
        assert!(book.newest.len() <= ORDER_BOOK_CAP);
        assert!(book.ended.len() <= ORDER_BOOK_CAP);
    }
    let took = started.elapsed();
    eprintln!("10k start+end cycles through the order book: {took:?}");
    assert_eq!(book.ended.len(), ORDER_BOOK_CAP);
    for n in 0..100u64 {
        assert!(
            book.newest.contains_key(&format!("live-{n}")),
            "live-{n} was displaced by tombstones"
        );
        assert!(
            !book.admit(&format!("live-{n}"), 1_000_000 + n - 1, false),
            "live-{n} lost its high-water mark"
        );
    }
    // The most recent ten endings are all still remembered.
    for n in 9_990..10_000u64 {
        assert!(
            !book.admit(&format!("gone-{n}"), 2_000_000 + 2 * n, false),
            "gone-{n}'s tombstone was evicted while newer than most"
        );
    }
    assert!(took < Duration::from_secs(5), "{took:?}");
}

/// The other direction: more live sessions than the cap evicts the oldest,
/// and the cap holds.
#[test]
fn more_live_sessions_than_the_cap_evict_the_oldest_and_stay_bounded() {
    let mut book = OrderBook::default();
    let total = (ORDER_BOOK_CAP * 4) as u64;
    for n in 0..total {
        assert!(book.admit(&format!("s-{n}"), 10 + n, false));
        assert!(book.newest.len() <= ORDER_BOOK_CAP);
    }
    assert_eq!(book.newest.len(), ORDER_BOOK_CAP);
    assert!(!book.newest.contains_key("s-0"));
    assert!(book.newest.contains_key(&format!("s-{}", total - 1)));
}

/// A zero stamp (a failed clock) must not wedge a session: every report ties,
/// and a tie is admitted.
#[test]
fn a_clock_that_reads_zero_orders_nothing_and_blocks_nothing() {
    let mut book = OrderBook::default();
    for _ in 0..5 {
        assert!(book.admit("term-1-a", 0, false));
    }
    assert!(book.admit("term-1-a", 0, true));
    assert!(book.admit("term-1-a", 0, false), "a tie with the end");
    assert_eq!(bridge::order_for(None, 0), 0);
}

/// The claim: an ended session does not come back. The hook's `session_ended`
/// does leave a tombstone, but a PTY exiting reports its own end
/// (`terminal::Session::ended`, origin Terminal, event `session_ended`, no
/// stamp), and that one goes round the book. A `permission_request` that was
/// sent before the exit and arrives after it then stands for a dead session.
#[test]
fn a_late_hook_report_does_not_revive_a_session_whose_pty_exited() {
    let rig = Rig::new();
    let exit = Notice {
        origin: Origin::Terminal,
        channel: "exit",
        event: Some("session_ended"),
        ..bell("term-7-a", None)
    };
    assert!(rig.intake.offer(exit).taken());
    // The request was sent before the exit, so its stamp is older than the
    // moment the PTY ended; it reached the socket late.
    let late = rig
        .intake
        .offer_stamped(hook("permission_request", "term-7-a"), 1);
    assert_eq!(
        late,
        Offer::Stale,
        "a report from before the session ended was applied after it: {late:?}"
    );
    rig.finish();
}

/* ── the reservation ──────────────────────────────────────────────────────── */

/// A bell that finds the queue full must not stay recorded as queued: once the
/// queue drains, the same bell is accepted, not folded into a ghost.
#[test]
fn a_bell_refused_at_the_door_leaves_no_ghost_in_the_gate() {
    let counters = Arc::new(Counters::default());
    let (intake, receiver) = Intake::channel(counters.clone());
    for n in 0..QUEUE {
        let (notice, _) = (hook("agent_completed", &format!("term-r-{n}")), ());
        assert_eq!(intake.offer_stamped(notice, 10 + n as u64), Offer::Accepted);
    }
    assert_eq!(intake.offer(bell("term-b", None)), Offer::QueueFull);
    assert_eq!(counters.dropped_queue.load(Ordering::Relaxed), 1);
    while receiver.try_recv().is_ok() {}
    assert_eq!(
        intake.offer(bell("term-b", None)),
        Offer::Accepted,
        "the refused bell was remembered as queued"
    );
}

/// Hook reports can fill every slot, so bells can be starved. That is the
/// design (reports say more than a bell) but the totals must still reconcile:
/// every refused bell is in `dropped_queue`.
#[test]
fn bells_starved_by_reports_are_all_counted() {
    let counters = Arc::new(Counters::default());
    let (intake, _receiver) = Intake::channel(counters.clone());
    for n in 0..QUEUE {
        assert!(intake
            .offer_stamped(hook("agent_completed", &format!("term-r-{n}")), 10)
            .taken());
    }
    let refused = (0..1_000)
        .filter(|n| intake.offer(bell(&format!("term-b-{n}"), None)) == Offer::QueueFull)
        .count();
    assert_eq!(refused, 1_000);
    assert_eq!(counters.dropped_queue.load(Ordering::Relaxed), 1_000);
}

/// The reservation is for hook reports, but nothing limits the renderer's
/// answers: 64 of them (the renderer sends one per keystroke) take every slot,
/// including the sixteen kept back, and the next hook report is refused.
#[test]
fn a_flood_of_answers_cannot_starve_hook_reports() {
    let counters = Arc::new(Counters::default());
    let (intake, _receiver) = Intake::channel(counters);
    let mut taken = 0;
    for _ in 0..QUEUE * 2 {
        taken += usize::from(intake.offer(answer("term-typing")).taken());
    }
    let report = intake.offer_stamped(hook("agent_completed", "term-other"), 5);
    assert!(
        report.taken(),
        "{taken} answers from one session left no room for another session's report: {report:?}"
    );
}

/// The same through the live worker: one thread answering flat out for 50k
/// answers while a hundred reports from other sessions arrive 2ms apart.
#[test]
fn answers_flat_out_do_not_cost_other_sessions_their_reports() {
    let rig = Rig::new();
    let typist = {
        let intake = rig.intake.clone();
        std::thread::spawn(move || {
            for _ in 0..50_000 {
                intake.offer(answer("term-typing"));
            }
        })
    };
    let mut refused = 0;
    for n in 0..100u64 {
        let offer = rig
            .intake
            .offer_stamped(hook("agent_completed", &format!("term-o-{n}")), 10 + n);
        refused += usize::from(!offer.taken());
        std::thread::sleep(Duration::from_millis(2));
    }
    typist.join().unwrap();
    rig.finish();
    assert_eq!(refused, 0, "{refused} of 100 hook reports were refused");
}

/// A queue whose worker is gone (it failed to start, or panicked) is not a
/// full queue: the reply's reason would otherwise say "queue full" for a
/// notifier that will never drain, and `not_running` could never be sent.
#[test]
fn a_queue_with_no_worker_says_not_running_rather_than_full() {
    let counters = Arc::new(Counters::default());
    let (intake, receiver) = Intake::channel(counters);
    drop(receiver);
    assert_eq!(
        intake.offer_stamped(hook("agent_completed", "term-1-a"), 5),
        Offer::NotRunning,
        "a dead worker was reported as a full queue"
    );
    assert_eq!(intake.offer(bell("term-1-a", None)), Offer::NotRunning);
}

/* ── reconciliation ───────────────────────────────────────────────────────── */

/// Bells from many sessions on several threads, hook reports (some stale) and
/// answers, all at once. Every bell and every banner report ends in exactly
/// one counter; a state or resolution adds one only when it is refused.
#[test]
fn counters_reconcile_under_a_mixed_flood() {
    use std::sync::atomic::AtomicU64;
    for round in 0..3u64 {
        let rig = Rig::new();
        let bells_offered = Arc::new(AtomicU64::new(0));
        let refusals_of_non_bells = Arc::new(AtomicU64::new(0));
        let banners_offered = Arc::new(AtomicU64::new(0));
        let mut threads = Vec::new();
        for t in 0..4u64 {
            let intake = rig.intake.clone();
            let offered = bells_offered.clone();
            threads.push(std::thread::spawn(move || {
                for n in 0..12_500u64 {
                    let session = format!("term-{}", (n * 7 + t) % 200);
                    let detail = match n % 3 {
                        0 => None,
                        1 => Some("a"),
                        _ => Some("b"),
                    };
                    intake.offer(bell(&session, detail));
                    offered.fetch_add(1, Ordering::Relaxed);
                }
            }));
        }
        for t in 0..2u64 {
            let intake = rig.intake.clone();
            let banners = banners_offered.clone();
            let refused = refusals_of_non_bells.clone();
            threads.push(std::thread::spawn(move || {
                for n in 0..3_000u64 {
                    let event =
                        &bridge::EVENTS[((n + t + round) % bridge::EVENTS.len() as u64) as usize];
                    let session = format!("term-{}", (n * 13 + t) % 40);
                    // Stamps wander backwards often enough to make many stale.
                    let stamp = 1_000 + (n * 5 + (n * 31 % 17) * 3) % 20_000;
                    let offer = intake.offer_stamped(hook(event.name, &session), stamp);
                    if event.role == bridge::Role::Banner {
                        banners.fetch_add(1, Ordering::Relaxed);
                    } else if matches!(offer, Offer::Stale | Offer::QueueFull) {
                        refused.fetch_add(1, Ordering::Relaxed);
                    }
                    if n % 50 == 0 {
                        let offer = intake.offer(answer(&session));
                        if offer == Offer::QueueFull {
                            refused.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        let status = {
            let counters = rig.counters.clone();
            let _ = rig.finish();
            counter(&counters)
        };
        let expected = bells_offered.load(Ordering::Relaxed)
            + banners_offered.load(Ordering::Relaxed)
            + refusals_of_non_bells.load(Ordering::Relaxed);
        assert_eq!(
            accounted(&status),
            expected,
            "round {round}: {status:?} (bells {}, banners {}, refused non-bells {})",
            bells_offered.load(Ordering::Relaxed),
            banners_offered.load(Ordering::Relaxed),
            refusals_of_non_bells.load(Ordering::Relaxed)
        );
        assert!(status.coalesced > 0 && status.stale > 0, "{status:?}");
    }
}

/// A coalesced bell is counted once, at the producer, and the bell it folded
/// into is counted once, by the worker: 1 + 999 = 1000 notices, 1000 outcomes.
#[test]
fn a_coalesced_bell_is_counted_exactly_once() {
    let counters = Arc::new(Counters::default());
    let (intake, receiver) = Intake::channel(counters.clone());
    let bells = intake.bells();
    let mut coalesced = 0u64;
    assert_eq!(intake.offer(bell("term-1", None)), Offer::Accepted);
    for _ in 0..999 {
        coalesced += u64::from(intake.offer(bell("term-1", None)) == Offer::Coalesced);
    }
    assert_eq!(coalesced, 999);
    assert_eq!(counters.coalesced.load(Ordering::Relaxed), 999);
    let worker = {
        let counters = counters.clone();
        std::thread::spawn(move || {
            run(
                receiver,
                bells,
                Arc::new(Probe),
                counters,
                Arc::default(),
                Arc::default(),
            )
        })
    };
    drop(intake);
    worker.join().unwrap();
    let status = counter(&counters);
    assert_eq!(accounted(&status), 1_000, "{status:?}");
}
