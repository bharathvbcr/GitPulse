//! The producers' side of the worker: ordering by `seq`, the room reserved for
//! hook reports, and the order book's tombstones.
//!
//! These drive the real [`Intake`] and the real worker loop (`run`), not a
//! model of them, so what they prove about a refusal is what a hook report
//! meets in the app.

use super::*;
use crate::tool_config::SessionAlertSettings;
use std::thread::JoinHandle;

/// A host that records what was delivered and nothing else.
#[derive(Default)]
struct Probe {
    delivered: Mutex<Vec<String>>,
}

impl Host for Probe {
    fn submit(&self, native: &str, _: &str, _: &str, _: bool) -> Result<bool, String> {
        self.delivered.lock().unwrap().push(native.to_owned());
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

/// An intake with a live worker behind it.
struct Rig {
    intake: Arc<Intake>,
    counters: Arc<Counters>,
    standing: Arc<Mutex<Vec<StandingAttention>>>,
    host: Arc<Probe>,
    worker: JoinHandle<()>,
}

impl Rig {
    fn new() -> Self {
        let counters = Arc::new(Counters::default());
        let (intake, receiver) = Intake::channel(counters.clone());
        let bells = intake.bells();
        let standing = Arc::new(Mutex::new(Vec::new()));
        let host = Arc::new(Probe::default());
        let worker = {
            let (counters, standing, host) = (counters.clone(), standing.clone(), host.clone());
            std::thread::spawn(move || {
                run(receiver, bells, host, counters, Arc::default(), standing)
            })
        };
        Self {
            intake: Arc::new(intake),
            counters,
            standing,
            host,
            worker,
        }
    }

    /// Closes the queue, lets the worker act on everything it holds, and
    /// returns what it ended up with.
    fn finish(self) -> Ended {
        let Self {
            intake,
            counters,
            standing,
            host,
            worker,
        } = self;
        drop(intake);
        worker.join().unwrap();
        let standing = standing.lock().unwrap().clone();
        let delivered = host.delivered.lock().unwrap().len();
        Ended {
            standing,
            delivered,
            stale: counters.stale.load(Ordering::Relaxed),
            dropped_queue: counters.dropped_queue.load(Ordering::Relaxed),
            coalesced: counters.coalesced.load(Ordering::Relaxed),
        }
    }
}

struct Ended {
    standing: Vec<StandingAttention>,
    delivered: usize,
    stale: u64,
    dropped_queue: u64,
    coalesced: u64,
}

impl Ended {
    fn standing_event(&self, session: &str) -> Option<String> {
        self.standing
            .iter()
            .find(|s| s.session == session)
            .and_then(|s| s.event.clone())
    }
}

/// A hook report as the socket would parse it: the notice and its `seq`.
fn report(event: &str, session: &str, seq: Option<u64>) -> (Notice, Option<u64>) {
    let seq = seq.map_or(String::new(), |seq| format!(r#","seq":{seq}"#));
    let json =
        format!(r#"{{"v":1,"event":"{event}","agent":"claude","session":"{session}"{seq}}}"#);
    bridge::parse_report_stamped(json.as_bytes()).expect("a well-formed report")
}

/// Offers `(report, accepted)` pairs the way the bridge does: ordered by the
/// report's `seq` when it has one, else by the accept stamp.
fn arrive(rig: &Rig, arrivals: &[((Notice, Option<u64>), u64)]) -> Vec<Offer> {
    arrivals
        .iter()
        .map(|((notice, seq), accepted)| {
            rig.intake
                .offer_stamped(notice.clone(), bridge::order_for(*seq, *accepted))
        })
        .collect()
}

const S: &str = "term-9-1a";

/// The tool finished (seq 2) and its permission request (seq 1) arrives after
/// it, because the two hook processes raced. The request was answered by the
/// tool that ran, so nothing stands.
#[test]
fn a_permission_request_older_than_the_tool_that_answered_it_does_not_stand() {
    let rig = Rig::new();
    let offers = arrive(
        &rig,
        &[
            // Accepted in this order; `seq` says they happened in the other.
            (report("tool_finished", S, Some(2)), 100),
            (report("permission_request", S, Some(1)), 101),
        ],
    );
    assert_eq!(offers, [Offer::Accepted, Offer::Stale]);
    let ended = rig.finish();
    assert_eq!(ended.standing_event(S), None, "{:?}", ended.standing);
    assert_eq!(ended.stale, 1);
}

/// Same race with the banner form of the request: no banner, no standing.
#[test]
fn a_permission_prompt_older_than_the_tool_that_answered_it_raises_no_banner() {
    let rig = Rig::new();
    let offers = arrive(
        &rig,
        &[
            (report("tool_finished", S, Some(2)), 100),
            (report("permission_prompt", S, Some(1)), 101),
        ],
    );
    assert_eq!(offers, [Offer::Accepted, Offer::Stale]);
    let ended = rig.finish();
    assert_eq!(
        ended.delivered, 0,
        "a stale prompt reached the notification centre"
    );
    assert_eq!(ended.standing_event(S), None, "{:?}", ended.standing);
    assert_eq!(ended.stale, 1);
}

/// The turn finished (seq 2) and the `PostToolUse` that preceded it (seq 1)
/// arrives after: the late tool result must not erase "Finished".
#[test]
fn a_tool_result_older_than_the_finished_turn_does_not_erase_it() {
    let rig = Rig::new();
    let offers = arrive(
        &rig,
        &[
            (report("turn_finished", S, Some(2)), 100),
            (report("tool_finished", S, Some(1)), 101),
        ],
    );
    assert_eq!(offers, [Offer::Accepted, Offer::Stale]);
    let ended = rig.finish();
    assert_eq!(
        ended.standing_event(S).as_deref(),
        Some("turn_finished"),
        "{:?}",
        ended.standing
    );
    assert_eq!(ended.stale, 1);
}

/// Reports with no `seq` are ordered by their accept stamp exactly as before:
/// the later accept wins, and nothing is stale.
#[test]
fn reports_without_seq_keep_their_accept_order() {
    let rig = Rig::new();
    let offers = arrive(
        &rig,
        &[
            (report("tool_finished", S, None), 100),
            (report("turn_finished", S, None), 101),
        ],
    );
    assert_eq!(offers, [Offer::Accepted, Offer::Accepted]);
    let ended = rig.finish();
    assert_eq!(ended.standing_event(S).as_deref(), Some("turn_finished"));
    assert_eq!(ended.stale, 0);
}

/// A report carrying `seq` and one without are ordered against each other on
/// the one clock: the stamp a report is ordered by is its `seq` or its accept
/// stamp, and both are `order_stamp_ns`.
#[test]
fn a_seq_and_an_accept_stamp_order_against_each_other() {
    let rig = Rig::new();
    let offers = arrive(
        &rig,
        &[
            // The hook began at 50 and its connection was accepted at 200:
            // ordered at 50.
            (report("turn_finished", S, Some(50)), 200),
            // No seq, accepted at 150: ordered at 150, which is after 50.
            (report("tool_finished", S, None), 150),
        ],
    );
    assert_eq!(offers, [Offer::Accepted, Offer::Accepted]);
    drop(rig.finish());
}

/// A `seq` from the future is capped at the accept stamp, so a peer cannot
/// raise a session's high-water mark past everything that follows.
#[test]
fn a_seq_from_the_future_cannot_silence_a_session() {
    assert_eq!(bridge::order_for(Some(u64::MAX), 500), 500);
    assert_eq!(bridge::order_for(Some(40), 500), 40);
    assert_eq!(bridge::order_for(None, 500), 500);
    let rig = Rig::new();
    let offers = arrive(
        &rig,
        &[
            (report("turn_finished", S, Some(u64::MAX)), 500),
            (report("tool_finished", S, None), 501),
        ],
    );
    assert_eq!(offers, [Offer::Accepted, Offer::Accepted]);
    drop(rig.finish());
}

#[test]
fn seq_must_be_a_positive_integer() {
    for bad in [r#""7""#, "0", "-1", "1.5", "true", "[]"] {
        let json = format!(r#"{{"v":1,"event":"error","seq":{bad}}}"#);
        assert_eq!(
            bridge::parse_report_stamped(json.as_bytes()).map(|_| ()),
            Err(bridge::Reason::MalformedSeq),
            "{bad}"
        );
    }
    let json = r#"{"v":1,"event":"error","seq":null}"#;
    assert_eq!(
        bridge::parse_report_stamped(json.as_bytes()).unwrap().1,
        None
    );
}

/* ── the order book ───────────────────────────────────────────────────────── */

/// A stale report that arrives after its session's `session_ended` is dropped
/// and creates no entry: it was sent before the session ended.
#[cfg(unix)]
#[test]
fn a_stale_report_after_session_ended_does_not_recreate_the_entry() {
    let mut book = OrderBook::default();
    assert!(book.admit("a", 5, false));
    assert!(book.admit("a", 9, true));
    assert!(book.newest.is_empty());
    for stamp in [1, 5, 8] {
        assert!(
            !book.admit("a", stamp, false),
            "stamp {stamp} outlived the end"
        );
        assert!(book.newest.is_empty(), "stamp {stamp} re-created the entry");
    }
    // A report newer than the end is a new run under a reused key.
    assert!(book.admit("a", 12, false));
    assert!(book.ended.is_empty());
    assert_eq!(book.newest.get("a"), Some(&12));
}

/// Tombstones have their own bound, so a fleet of ended sessions neither grows
/// the book without limit nor pushes a running session out of it.
#[cfg(unix)]
#[test]
fn tombstones_are_bounded_and_do_not_displace_live_sessions() {
    let mut book = OrderBook::default();
    assert!(book.admit("live", 1, false));
    for n in 0..ORDER_BOOK_CAP * 3 {
        let key = format!("gone-{n}");
        assert!(book.admit(&key, 10 + n as u64, true));
        assert!(
            book.ended.len() <= ORDER_BOOK_CAP,
            "{} tombstones",
            book.ended.len()
        );
    }
    assert_eq!(book.ended.len(), ORDER_BOOK_CAP);
    assert!(
        book.newest.contains_key("live"),
        "a tombstone displaced a live session"
    );
    // The oldest tombstone was forgotten (costing one late report), the newest kept.
    assert!(!book.ended.contains_key("gone-0"));
    assert!(book
        .ended
        .contains_key(&format!("gone-{}", ORDER_BOOK_CAP * 3 - 1)));
}

/// Through the real intake: the late report after the end is counted stale and
/// never reaches the worker.
#[cfg(unix)]
#[test]
fn a_late_report_after_session_ended_is_stale_through_the_intake() {
    let rig = Rig::new();
    let offers = arrive(
        &rig,
        &[
            (report("session_ended", S, Some(10)), 100),
            (report("tool_finished", S, Some(5)), 101),
        ],
    );
    assert_eq!(offers, [Offer::Accepted, Offer::Stale]);
    let ended = rig.finish();
    assert_eq!(ended.stale, 1);
}

/* ── room for hook reports ────────────────────────────────────────────────── */

fn bell(session: &str) -> Notice {
    Notice {
        key: session.to_owned(),
        origin: Origin::Terminal,
        label: "Claude Code".into(),
        place: None,
        event: None,
        detail: None,
        is_agent: true,
        channel: "bell",
        subject: None,
        subagent: false,
    }
}

/// One program ringing flat out, and a hundred hook reports from other
/// sessions: none of the reports is dropped.
#[test]
fn fifty_thousand_bells_on_one_session_drop_no_hook_report() {
    let rig = Rig::new();
    let ringer = {
        let intake = rig.intake.clone();
        std::thread::spawn(move || {
            for _ in 0..50_000 {
                intake.offer(bell("term-ringer"));
            }
        })
    };
    let mut refused = Vec::new();
    for batch in 0..25 {
        for n in 0..4 {
            let session = format!("term-hook-{}", batch * 4 + n);
            let (notice, _) = report("agent_completed", &session, None);
            let offer = rig.intake.offer_stamped(notice, order_stamp(batch * 4 + n));
            if !offer.taken() {
                refused.push((session, offer));
            }
        }
        // The worker is a thread on a loaded machine: a short pause between
        // groups smaller than the room kept for reports, so what this measures
        // is the bells' effect and not the scheduler's.
        std::thread::sleep(Duration::from_millis(2));
    }
    ringer.join().unwrap();
    let ended = rig.finish();
    assert!(refused.is_empty(), "hook reports were refused: {refused:?}");
    // The flood was folded at the producer, not dropped.
    assert!(ended.coalesced > 49_000, "{} coalesced", ended.coalesced);
    assert_eq!(
        ended.dropped_queue, 0,
        "something was dropped from the queue"
    );
}

fn order_stamp(n: u64) -> u64 {
    1_000 + n
}

/// With nothing draining, the queue is exactly full: bells from many sessions
/// take [`BELL_SLOTS`] and no more, and a hook report still has every slot
/// they leave, which includes the reserve.
#[test]
fn bells_leave_the_reserved_slots_to_hook_reports() {
    let counters = Arc::new(Counters::default());
    let (intake, receiver) = Intake::channel(counters.clone());
    let accepted_bells = (0..500)
        .filter(|n| intake.offer(bell(&format!("term-{n}"))) == Offer::Accepted)
        .count();
    // The contract is at least this many slots kept back, stated here as a
    // number so that shrinking the reservation fails this test rather than
    // moving the goalposts with it.
    const AT_LEAST_RESERVED: usize = 16;
    assert_eq!(accepted_bells, intake::BELL_SLOTS);
    assert!(
        intake::RESERVED_SLOTS >= AT_LEAST_RESERVED && accepted_bells <= QUEUE - AT_LEAST_RESERVED,
        "bells took {accepted_bells} of {QUEUE} slots"
    );
    assert_eq!(
        counters.dropped_queue.load(Ordering::Relaxed),
        (500 - intake::BELL_SLOTS) as u64
    );
    for n in 0..QUEUE - intake::BELL_SLOTS {
        let (notice, _) = report("agent_completed", &format!("term-r-{n}"), None);
        assert_eq!(
            intake.offer_stamped(notice, order_stamp(n as u64)),
            Offer::Accepted,
            "hook report {n} found the queue full"
        );
    }
    // Now it is full, and the next report is told so, and counted.
    let (notice, _) = report("agent_completed", "term-last", None);
    let before = counters.dropped_queue.load(Ordering::Relaxed);
    assert_eq!(
        intake.offer_stamped(notice, order_stamp(999)),
        Offer::QueueFull
    );
    assert_eq!(counters.dropped_queue.load(Ordering::Relaxed), before + 1);
    drop(receiver);
}

/// The renderer's answers have their own bound ([`ANSWER_SLOTS`], one per
/// session): the next session's answer is refused, and that is `taken() ==
/// false`, which is what makes `answered()` say so.
#[test]
fn an_answer_the_queue_cannot_take_is_not_reported_as_answered() {
    let counters = Arc::new(Counters::default());
    let (intake, receiver) = Intake::channel(counters);
    let answer = || Notice {
        origin: Origin::Reader,
        channel: "reader",
        event: Some("prompt_submitted"),
        ..bell("term-1")
    };
    let of = |n: usize| Notice {
        key: format!("term-{n}"),
        ..answer()
    };
    for n in 0..intake::ANSWER_SLOTS {
        assert_eq!(intake.offer(of(n)), Offer::Accepted);
    }
    // The same session again is folded, not refused: only its latest matters.
    assert_eq!(intake.offer(of(0)), Offer::Coalesced);
    assert_eq!(intake.offer(of(intake::ANSWER_SLOTS)), Offer::QueueFull);
    assert!(!intake.offer(of(intake::ANSWER_SLOTS)).taken());
    drop(receiver);
}

/// Different bells are different requests: only an identical one is folded.
#[test]
fn only_an_identical_bell_is_coalesced() {
    let counters = Arc::new(Counters::default());
    let (intake, receiver) = Intake::channel(counters.clone());
    assert_eq!(intake.offer(bell("a")), Offer::Accepted);
    assert_eq!(intake.offer(bell("a")), Offer::Coalesced);
    let said = Notice {
        detail: Some("build finished".into()),
        ..bell("a")
    };
    assert_eq!(
        intake.offer(said),
        Offer::Accepted,
        "newer words were folded away"
    );
    assert_eq!(intake.offer(bell("b")), Offer::Accepted);
    assert_eq!(counters.coalesced.load(Ordering::Relaxed), 1);
    drop(receiver);
}

/// Once the worker has taken a bell off the queue, the next identical one is
/// queued again rather than folded into a bell that is already gone.
#[test]
fn a_bell_the_worker_has_taken_can_be_rung_again() {
    let counters = Arc::new(Counters::default());
    let (intake, receiver) = Intake::channel(counters);
    let gate = intake.bells();
    assert_eq!(intake.offer(bell("a")), Offer::Accepted);
    assert_eq!(intake.offer(bell("a")), Offer::Coalesced);
    gate.release(&receiver.recv().unwrap());
    assert_eq!(intake.offer(bell("a")), Offer::Accepted);
}

#[test]
fn a_closed_queue_says_not_running_not_full() {
    let counters = Arc::new(Counters::default());
    let (intake, receiver) = Intake::channel(counters);
    drop(receiver);
    // Nobody will ever drain it, which is not the same as being full.
    assert_eq!(intake.offer(bell("a")), Offer::NotRunning);
}

/// A hook report carrying the digest of the tool call it is about.
#[cfg(unix)]
fn report_about(event: &str, session: &str, subject: Option<&str>) -> Notice {
    let subject = subject.map_or(String::new(), |s| format!(r#","subject":"{s}""#));
    let json =
        format!(r#"{{"v":1,"event":"{event}","agent":"claude","session":"{session}"{subject}}}"#);
    bridge::parse_report_stamped(json.as_bytes())
        .expect("a well-formed report")
        .0
}

const MS: u64 = 1_000_000;

/// The plugin's hook and the one GitPulse injects both fire for one event, and
/// each stamps its own entry a few milliseconds apart. The second is the same
/// report said again: it is folded, not offered to the worker as a second one.
#[cfg(unix)]
#[test]
fn one_event_said_by_the_plugin_and_the_injected_hook_is_one_report() {
    let call = Some("00000000deadbeef");
    for (event, subject) in [
        ("permission_request", call),
        ("turn_finished", None),
        ("tool_finished", call),
        ("session_ended", None),
    ] {
        let rig = Rig::new();
        let first = rig
            .intake
            .offer_stamped(report_about(event, S, subject), 1_000 * MS);
        let second = rig
            .intake
            .offer_stamped(report_about(event, S, subject), 1_000 * MS + 7 * MS);
        assert_eq!(first, Offer::Accepted, "{event}");
        assert_eq!(second, Offer::Coalesced, "{event}");
        assert!(second.taken());
        let ended = rig.finish();
        // A state or a resolution has no outcome of its own to count.
        assert_eq!(ended.coalesced, 0, "{event}");
        assert_eq!(ended.stale, 0, "{event}");
    }
}

/// A banner twice is one banner, and its folded copy is counted as the
/// coalesced notice it is.
#[cfg(unix)]
#[test]
fn a_banner_said_twice_is_counted_as_coalesced() {
    let rig = Rig::new();
    let first = rig
        .intake
        .offer_stamped(report_about("permission_prompt", S, None), 3_000 * MS);
    let second = rig.intake.offer_stamped(
        report_about("permission_prompt", S, None),
        3_000 * MS + 5 * MS,
    );
    assert_eq!((first, second), (Offer::Accepted, Offer::Coalesced));
    assert_eq!(rig.finish().coalesced, 1);
}

/// One standing, one announcement: the second `permission_request` neither
/// restands the question nor announces it again.
#[cfg(unix)]
#[test]
fn a_duplicated_permission_request_leaves_one_standing() {
    let rig = Rig::new();
    let call = Some("00000000deadbeef");
    rig.intake
        .offer_stamped(report_about("permission_request", S, call), 5_000 * MS);
    rig.intake.offer_stamped(
        report_about("permission_request", S, call),
        5_000 * MS + 4 * MS,
    );
    let ended = rig.finish();
    assert_eq!(ended.standing.len(), 1, "{:?}", ended.standing);
    assert_eq!(
        ended.standing_event(S).as_deref(),
        Some("permission_request")
    );
}

/// The copy may stamp earlier than the report that got here first: the two
/// processes start in either order.
#[cfg(unix)]
#[test]
fn the_twin_that_stamped_first_is_still_a_twin() {
    let rig = Rig::new();
    let first = rig
        .intake
        .offer_stamped(report_about("turn_finished", S, None), 2_000 * MS);
    let late_twin = rig
        .intake
        .offer_stamped(report_about("turn_finished", S, None), 2_000 * MS - 6 * MS);
    assert_eq!((first, late_twin), (Offer::Accepted, Offer::Coalesced));
    rig.finish();
}

/// What is not the same report is not folded: another call, another event,
/// another session, or the same event much later.
#[cfg(unix)]
#[test]
fn only_the_same_event_for_the_same_call_close_in_time_is_folded() {
    let rig = Rig::new();
    let at = 9_000 * MS;
    let a = Some("00000000000000aa");
    let b = Some("00000000000000bb");
    let offers = [
        rig.intake
            .offer_stamped(report_about("permission_request", S, a), at),
        // Another call.
        rig.intake
            .offer_stamped(report_about("permission_request", S, b), at + MS),
        // Another event.
        rig.intake
            .offer_stamped(report_about("turn_finished", S, None), at + 2 * MS),
        // Another session.
        rig.intake.offer_stamped(
            report_about("turn_finished", "term-9-2b", None),
            at + 3 * MS,
        ),
        // The same event again, a real second one.
        rig.intake
            .offer_stamped(report_about("turn_finished", S, None), at + 2_000 * MS),
    ];
    assert_eq!(offers, [Offer::Accepted; 5]);
    let ended = rig.finish();
    assert_eq!(ended.coalesced, 0);
}
