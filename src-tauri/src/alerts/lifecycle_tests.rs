//! What the intake tells its host about a session's lifecycle: a change of
//! state is told once, and a report that repeats the state is not told at all,
//! so a tool-call-per-report hook (`PostToolUse`) does not become a row per call.
//!
//! These drive the real [`Intake`], with a host that records what it is told.

#![cfg(unix)]

use super::*;
use crate::ledger::lifecycle::Change;

const MS: u64 = 1_000_000;
const S: &str = "term-5-1a";

#[derive(Default)]
struct Told {
    changes: Mutex<Vec<Change>>,
}

impl Host for Told {
    fn submit(&self, _: &str, _: &str, _: &str, _: bool) -> Result<bool, String> {
        Ok(true)
    }
    fn attended(&self, _: &str) -> bool {
        false
    }
    fn lifecycle(&self, change: Change) {
        self.changes.lock().unwrap().push(change);
    }
}

struct Rig {
    intake: Intake,
    receiver: mpsc::Receiver<Notice>,
    told: Arc<Told>,
    at: u64,
}

impl Rig {
    fn new() -> Self {
        let counters = Arc::new(Counters::default());
        let told = Arc::new(Told::default());
        let (intake, receiver) = Intake::channel(counters);
        Self {
            intake: intake.guarded_by(told.clone()),
            receiver,
            told,
            at: 1_000 * MS,
        }
    }

    /// Offers one report a second after the last, drained so the queue never fills.
    fn say(&mut self, event: &str, session: &str) -> Offer {
        self.say_json(&format!(
            r#"{{"v":1,"event":"{event}","agent":"claude","session":"{session}"}}"#
        ))
    }

    fn say_json(&mut self, json: &str) -> Offer {
        let notice = bridge::parse_report_stamped(json.as_bytes())
            .expect("a well-formed report")
            .0;
        self.at += 1_000 * MS;
        let offer = self.intake.offer_stamped(notice, self.at);
        while self.receiver.try_recv().is_ok() {}
        offer
    }

    fn told(&self) -> Vec<(String, &'static str, &'static str)> {
        self.told
            .changes
            .lock()
            .unwrap()
            .iter()
            .map(|c| (c.session.clone(), c.event, c.state))
            .collect()
    }
}

#[test]
fn a_tool_call_per_report_is_one_change_not_one_per_call() {
    let mut rig = Rig::new();
    rig.say("prompt_submitted", S);
    for _ in 0..20 {
        rig.say("tool_finished", S);
    }
    assert_eq!(
        rig.told(),
        vec![(S.to_string(), "prompt_submitted", "working")]
    );
}

#[test]
fn only_a_change_of_state_is_told_and_each_is_told_with_its_event() {
    let mut rig = Rig::new();
    for event in [
        "prompt_submitted",
        "tool_finished",
        "permission_request",
        "permission_request",
        "tool_finished",
        "turn_finished",
        "turn_finished",
        "session_ended",
        "session_ended",
    ] {
        rig.say(event, S);
    }
    let states: Vec<_> = rig
        .told()
        .into_iter()
        .map(|(_, event, state)| (event, state))
        .collect();
    assert_eq!(
        states,
        vec![
            ("prompt_submitted", "working"),
            ("permission_request", "waiting_you"),
            ("tool_finished", "working"),
            ("turn_finished", "idle"),
            ("session_ended", "ended"),
        ]
    );
}

#[test]
fn a_different_event_that_leaves_the_same_state_is_not_a_change() {
    let mut rig = Rig::new();
    rig.say("permission_request", S);
    // The banner that follows the request six seconds later says the same thing.
    rig.say("permission_prompt", S);
    rig.say("idle_prompt", S);
    assert_eq!(rig.told().len(), 1);
}

#[test]
fn a_new_run_under_a_reused_key_records_its_first_state() {
    let mut rig = Rig::new();
    rig.say("session_ended", S);
    rig.say("prompt_submitted", S);
    let events: Vec<_> = rig.told().into_iter().map(|(_, event, _)| event).collect();
    assert_eq!(events, vec!["session_ended", "prompt_submitted"]);
}

#[test]
fn sessions_are_told_apart() {
    let mut rig = Rig::new();
    rig.say("prompt_submitted", "term-1-aa");
    rig.say("prompt_submitted", "term-2-bb");
    rig.say("tool_finished", "term-1-aa");
    assert_eq!(rig.told().len(), 2);
}

#[test]
fn a_subagents_report_is_not_its_sessions_state() {
    let mut rig = Rig::new();
    rig.say_json(&format!(
        r#"{{"v":1,"event":"permission_request","agent":"claude","session":"{S}","subagent":true}}"#
    ));
    assert!(rig.told().is_empty());
    // The session's own report afterwards still records.
    rig.say("prompt_submitted", S);
    assert_eq!(rig.told().len(), 1);
}

#[test]
fn a_report_from_outside_any_terminal_names_no_session_and_is_not_told() {
    let mut rig = Rig::new();
    // Keyed `hook-claude-elsewhere`: it names no terminal the registry could
    // place in a repository, and a peer chose its place.
    let offer = rig
        .say_json(r#"{"v":1,"event":"permission_prompt","agent":"claude","cwd":"/tmp/elsewhere"}"#);
    assert_eq!(offer, Offer::Accepted, "the banner itself is still offered");
    assert!(rig.told().is_empty());
}

#[test]
fn a_stale_report_is_not_told() {
    let mut rig = Rig::new();
    rig.say("prompt_submitted", S);
    let older = bridge::parse_report_stamped(
        format!(r#"{{"v":1,"event":"permission_request","agent":"claude","session":"{S}"}}"#)
            .as_bytes(),
    )
    .unwrap()
    .0;
    assert_eq!(rig.intake.offer_stamped(older, 1), Offer::Stale);
    assert_eq!(rig.told().len(), 1);
}

#[test]
fn a_report_the_queue_refused_is_not_told() {
    let counters = Arc::new(Counters::default());
    let told = Arc::new(Told::default());
    let (intake, receiver) = Intake::channel(counters);
    let intake = intake.guarded_by(told.clone());
    // Nobody will ever drain it.
    drop(receiver);
    let notice = bridge::parse_report_stamped(
        format!(r#"{{"v":1,"event":"prompt_submitted","agent":"claude","session":"{S}"}}"#)
            .as_bytes(),
    )
    .unwrap()
    .0;
    assert_eq!(intake.offer_stamped(notice, 5_000 * MS), Offer::NotRunning);
    assert!(told.changes.lock().unwrap().is_empty());
}

#[test]
fn an_intake_with_no_host_tells_nobody_and_still_orders_reports() {
    let counters = Arc::new(Counters::default());
    let (intake, receiver) = Intake::channel(counters);
    let notice = bridge::parse_report_stamped(
        format!(r#"{{"v":1,"event":"prompt_submitted","agent":"claude","session":"{S}"}}"#)
            .as_bytes(),
    )
    .unwrap()
    .0;
    assert_eq!(intake.offer_stamped(notice, 5_000 * MS), Offer::Accepted);
    assert!(receiver.try_recv().is_ok());
}
