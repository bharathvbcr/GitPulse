//! Attacks on the twin fold in `Intake::offer_stamped` (W7-L3 adversarial
//! review). Each test names the claim it tries to falsify.

use super::*;
use std::sync::atomic::Ordering;

const MS: u64 = 1_000_000;
const S: &str = "term-7-aa";

fn report_about(event: &str, session: &str, subject: Option<&str>) -> Notice {
    let subject = subject.map_or(String::new(), |s| format!(r#","subject":"{s}""#));
    let json =
        format!(r#"{{"v":1,"event":"{event}","agent":"claude","session":"{session}"{subject}}}"#);
    bridge::parse_report_stamped(json.as_bytes())
        .expect("a well-formed report")
        .0
}

fn rig() -> (Intake, mpsc::Receiver<Notice>, Arc<Counters>) {
    let counters = Arc::new(Counters::default());
    let (intake, receiver) = Intake::channel(counters.clone());
    (intake, receiver, counters)
}

fn queued(receiver: &mpsc::Receiver<Notice>) -> usize {
    std::iter::from_fn(|| receiver.try_recv().ok()).count()
}

const A: Option<&str> = Some("00000000000000aa");
const B: Option<&str> = Some("00000000000000bb");

/// Claude runs tool calls in parallel, so two calls' reports (each said by the
/// plugin hook and the injected hook) interleave: A, B, A', B'. The fold
/// remembers only the LAST report, so A' is compared with B.
#[test]
fn twins_of_two_parallel_calls_interleaved_are_both_folded() {
    for event in ["permission_request", "tool_finished"] {
        let (intake, receiver, _) = rig();
        let at = 1_000 * MS;
        let offers = [
            intake.offer_stamped(report_about(event, S, A), at),
            intake.offer_stamped(report_about(event, S, B), at + 2 * MS),
            intake.offer_stamped(report_about(event, S, A), at + 5 * MS),
            intake.offer_stamped(report_about(event, S, B), at + 7 * MS),
        ];
        assert_eq!(
            offers,
            [
                Offer::Accepted,
                Offer::Accepted,
                Offer::Coalesced,
                Offer::Coalesced
            ],
            "{event}: {offers:?}; {} reports reached the worker, 2 expected",
            queued(&receiver)
        );
    }
}

/// The twin of a report that a later, different report has overtaken is judged
/// on stamp order and counted stale once.
#[test]
fn a_twin_behind_a_later_event_is_stale_and_counted_once() {
    let (intake, receiver, counters) = rig();
    let at = 2_000 * MS;
    assert_eq!(
        intake.offer_stamped(report_about("tool_finished", S, A), at),
        Offer::Accepted
    );
    assert_eq!(
        intake.offer_stamped(report_about("turn_finished", S, None), at + 100 * MS),
        Offer::Accepted
    );
    assert_eq!(
        intake.offer_stamped(report_about("tool_finished", S, A), at + 5 * MS),
        Offer::Stale
    );
    assert_eq!(counters.stale.load(Ordering::Relaxed), 1);
    assert_eq!(queued(&receiver), 2);
}

/// A folded twin stamped late must not make a report that really happened
/// between the two copies Stale. (The base admitted the twin and moved the
/// mark the same way: not a regression.)
#[test]
fn a_late_twin_does_not_turn_a_real_report_between_the_copies_stale() {
    let (intake, _receiver, counters) = rig();
    let at = 3_000 * MS;
    intake.offer_stamped(report_about("tool_finished", S, A), at);
    // The twin process started 400 ms late (loaded machine).
    intake.offer_stamped(report_about("tool_finished", S, A), at + 400 * MS);
    let stop = intake.offer_stamped(report_about("turn_finished", S, None), at + 200 * MS);
    assert_eq!(
        stop,
        Offer::Accepted,
        "Stop stamped between the copies was dropped as stale ({} stale)",
        counters.stale.load(Ordering::Relaxed)
    );
}

/// Two real, distinct, identical-digest events (two identical parallel calls
/// finishing 200 ms apart) cannot be told from a twin by this key. Expected to
/// fail by design: documents that a real second report is folded.
#[test]
fn two_real_identical_events_inside_the_window_are_both_offered() {
    let (intake, receiver, _) = rig();
    let at = 4_000 * MS;
    let first = intake.offer_stamped(report_about("tool_finished", S, A), at);
    let second = intake.offer_stamped(report_about("tool_finished", S, A), at + 200 * MS);
    assert_eq!((first, second), (Offer::Accepted, Offer::Accepted));
    assert_eq!(queued(&receiver), 2);
}

/// A subagent's report and its parent's, same event and digest: `subagent` is
/// not part of the key, so the subagent's flag is lost when it is second.
#[test]
fn a_subagent_report_is_not_folded_into_its_parents() {
    let (intake, receiver, _) = rig();
    let at = 5_000 * MS;
    let parent = report_about("tool_finished", S, A);
    let sub = {
        let json = format!(
            r#"{{"v":1,"event":"tool_finished","agent":"claude","session":"{S}","subject":"00000000000000aa","subagent":true}}"#
        );
        bridge::parse_report_stamped(json.as_bytes()).unwrap().0
    };
    assert_eq!(intake.offer_stamped(parent, at), Offer::Accepted);
    assert_eq!(intake.offer_stamped(sub, at + 3 * MS), Offer::Accepted);
    assert_eq!(queued(&receiver), 2);
}

/// The code comment says a full queue refuses the twin as it refused the
/// first. The twin is answered Coalesced (taken) before the queue is
/// consulted, so a report the queue dropped is accounted as taken for its twin.
#[test]
fn a_twin_of_a_report_the_queue_refused_is_not_answered_as_taken() {
    let (intake, _receiver, counters) = rig();
    let mut n = 0u64;
    let refused = loop {
        let notice = report_about("turn_finished", &format!("fill-{n}"), None);
        let offer = intake.offer_stamped(notice, 10_000 * MS + n);
        n += 1;
        if offer != Offer::Accepted {
            break (offer, n);
        }
        assert!(n < 1_000, "the queue never filled");
    };
    assert_ne!(refused.0, Offer::Accepted);
    let twin = intake.offer_stamped(
        report_about("turn_finished", &format!("fill-{}", refused.1 - 1), None),
        10_000 * MS + refused.1 + 1,
    );
    assert!(
        !twin.taken(),
        "the twin of a refused report was answered {twin:?} (dropped_queue={})",
        counters.dropped_queue.load(Ordering::Relaxed)
    );
}

/// Accounting: a deterministic mix of twins (either stamp order), stale and
/// fresh reports. The offers returned, the queue and the counters agree.
#[test]
fn the_totals_reconcile_across_mixed_twins_and_stale_reports() {
    let (intake, receiver, counters) = rig();
    let mut offers = Vec::new();
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let events = [
        ("permission_request", true),
        ("tool_finished", true),
        ("turn_finished", false),
        ("prompt_submitted", false),
    ];
    for i in 0..400u64 {
        let session = format!("mix-{}", next() % 3);
        let (event, tool) = events[(next() % 4) as usize];
        let subject = tool.then(|| ["00000000000000aa", "00000000000000bb"][(next() % 2) as usize]);
        let stamp = 50_000 * MS + i * 40 * MS + next() % (700 * MS);
        offers.push(intake.offer_stamped(report_about(event, &session, subject), stamp));
        if next() % 3 == 0 {
            let twin_stamp = stamp.saturating_sub(next() % (20 * MS)) + next() % (20 * MS);
            offers.push(intake.offer_stamped(report_about(event, &session, subject), twin_stamp));
        }
    }
    let count = |wanted: Offer| offers.iter().filter(|o| **o == wanted).count();
    let (accepted, stale, full, coalesced) = (
        count(Offer::Accepted),
        count(Offer::Stale),
        count(Offer::QueueFull),
        count(Offer::Coalesced),
    );
    assert_eq!(accepted + stale + full + coalesced, offers.len());
    assert_eq!(counters.stale.load(Ordering::Relaxed) as usize, stale);
    assert_eq!(queued(&receiver), accepted);
    assert_eq!(counters.dropped_queue.load(Ordering::Relaxed) as usize, full);
    assert!(coalesced > 0 && stale > 0);
}
