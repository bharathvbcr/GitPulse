//! Adversarial wire checks, through the real socket and the real hub: an old
//! hook's bytes against this bridge, the size of every reply (both hooks read
//! at most 64 bytes of it), hostile `seq` values, and mixed fleets.
//!
//! Written by a reviewer who did not write the protocol. A failing test here
//! is a claim that is false.
use super::*;
use crate::alerts::bridge::Reason;

fn unique(tag: &str) -> String {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    format!(
        "wireadv-{tag}-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// What `gitpulse-hook` at 08875ad4 does with a reply, exactly: it reads at
/// most 64 bytes, parses them as one JSON document and looks at `ok`.
fn old_hook_reads(reply: &str) -> Option<bool> {
    let first: Vec<u8> = reply.bytes().take(64).collect();
    serde_json::from_slice::<serde_json::Value>(&first)
        .ok()
        .and_then(|value| value.get("ok").and_then(serde_json::Value::as_bool))
}

/// The bytes 08875ad4's `notify_payload` produced: no `seq`, and the message
/// cut by raw bytes.
fn old_payload(event: &Event, session: &str) -> String {
    let message = if event.role == Role::Resolve {
        String::new()
    } else {
        r#","message":"Claude needs your permission to use Bash""#.to_owned()
    };
    format!(
        r#"{{"v":1,"event":"{}","agent":"claude","session":"{session}","cwd":"/work/repo"{message}}}"#,
        event.name
    )
}

/// Every event, as an old hook sends it, is acknowledged in a way an old hook
/// reads as success.
#[test]
fn an_old_hook_is_acknowledged_for_every_event_it_can_send() {
    let _own = own_listener();
    let dir = ShortDir::new("wireold");
    apply_at(true, Some(dir.socket()));
    assert!(status().listening, "{:?}", status().error);
    for event in EVENTS {
        let session = unique(event.name);
        let reply = send(&dir.socket(), old_payload(event, &session).as_bytes());
        assert_eq!(
            old_hook_reads(&reply),
            Some(true),
            "{}: an old hook would read {reply:?}",
            event.name
        );
        assert_eq!(reply, "{\"ok\":true}\n", "{}", event.name);
    }
    apply_at(false, None);
}

/// Both hooks read at most 64 bytes of the reply. A reply longer than that is
/// cut mid-document, fails to parse, and the hook reports "something other
/// than an acknowledgement" for what was a refusal with a reason.
#[test]
fn no_reply_is_longer_than_the_sixty_four_bytes_a_hook_reads() {
    for reason in Reason::ALL {
        let reply = Outcome::Refused(reason).reply().expect("a reply");
        assert!(
            reply.len() <= 64,
            "{}: {} bytes: {reply:?}",
            reason.token(),
            reply.len()
        );
        assert_eq!(old_hook_reads(&reply), Some(false), "{reply:?}");
        assert!(reply.starts_with("{\"ok\":false"), "{reply:?}");
        assert!(reply.ends_with("}\n"), "{reply:?}");
    }
    let accepted = Outcome::Accepted.reply().unwrap();
    assert_eq!(old_hook_reads(&accepted), Some(true));
}

/// A new refusal reaches an old hook as a refusal over the socket too, not as
/// garbage: the old hook says "may be different versions", which is its own
/// text for every `ok:false`.
#[test]
fn an_old_hook_sees_a_refusal_as_a_refusal_over_the_socket() {
    let _own = own_listener();
    let dir = ShortDir::new("wireoldrefuse");
    apply_at(true, Some(dir.socket()));
    for payload in [
        &b"not json"[..],
        br#"{"v":2,"event":"error"}"#,
        br#"{"v":1,"event":"tool_finished"}"#,
        br#"{"v":1,"event":"error","seq":0}"#,
    ] {
        let reply = send(&dir.socket(), payload);
        assert_eq!(old_hook_reads(&reply), Some(false), "{reply:?}");
    }
    apply_at(false, None);
}

/// `seq` is a positive integer that fits u64, and nothing else: every other
/// shape is refused as `malformed_seq`, none is read as a number.
#[test]
fn a_seq_that_is_not_a_positive_integer_is_refused_over_the_socket() {
    let _own = own_listener();
    let dir = ShortDir::new("wireseqshape");
    apply_at(true, Some(dir.socket()));
    for seq in [
        "0",
        "-1",
        "1.5",
        "1.0",
        "1e3",
        "\"5\"",
        "true",
        "[5]",
        "{}",
        "18446744073709551616",
    ] {
        let session = unique("shape");
        let report = format!(
            r#"{{"v":1,"event":"idle_prompt","agent":"claude","session":"{session}","seq":{seq}}}"#
        );
        let reply = send(&dir.socket(), report.as_bytes());
        assert_eq!(
            reply.trim(),
            r#"{"ok":false,"reason":"malformed_seq"}"#,
            "seq {seq}"
        );
    }
    // The extremes of a valid one, and null, which means absent.
    for seq in ["1", "9223372036854775807", "18446744073709551615", "null"] {
        let session = unique("valid");
        let report = format!(
            r#"{{"v":1,"event":"idle_prompt","agent":"claude","session":"{session}","seq":{seq}}}"#
        );
        assert_eq!(
            send(&dir.socket(), report.as_bytes()).trim(),
            r#"{"ok":true}"#,
            "seq {seq}"
        );
    }
    apply_at(false, None);
}

/// A same-uid peer forges `seq: u64::MAX` for a session to pin its high-water
/// mark. The cap holds it at the accept stamp, so the reports that begin after
/// it are unaffected; what it still costs is any real report whose hook began
/// before the forged one was accepted and has not connected yet. This test
/// measures that window rather than denying it: the forged report ordered
/// exactly like a seq-less one would be.
#[test]
fn a_forged_seq_costs_only_what_was_already_in_flight() {
    let _own = own_listener();
    let dir = ShortDir::new("wireforge");
    apply_at(true, Some(dir.socket()));
    let session = unique("forge");
    let at = |event: &str, seq: u64| {
        format!(r#"{{"v":1,"event":"{event}","agent":"claude","session":"{session}","seq":{seq}}}"#)
    };
    // A real hook has begun (took its stamp) and is about to connect.
    let in_flight = order_stamp_ns();
    // The forgery is accepted first.
    assert_eq!(
        send(&dir.socket(), at("idle_prompt", u64::MAX).as_bytes()).trim(),
        r#"{"ok":true}"#
    );
    // The real report, begun before the forgery, is dropped: the window.
    let late = send(&dir.socket(), at("turn_finished", in_flight).as_bytes());
    assert_eq!(late.trim(), r#"{"ok":false,"reason":"stale"}"#);
    // But everything that begins after the forgery lands.
    let after = order_stamp_ns();
    assert_eq!(
        send(&dir.socket(), at("turn_finished", after).as_bytes()).trim(),
        r#"{"ok":true}"#,
        "the cap did not contain the forged seq"
    );
    apply_at(false, None);
}

/// A seq from before boot (1) or a failed clock (0 is refused) for a session
/// that already has a high-water mark is stale, not applied.
#[test]
fn an_ancient_seq_is_stale_for_a_session_with_history_and_harmless_for_a_new_one() {
    let _own = own_listener();
    let dir = ShortDir::new("wireancient");
    apply_at(true, Some(dir.socket()));
    let session = unique("ancient");
    let at = |seq: u64| {
        format!(
            r#"{{"v":1,"event":"idle_prompt","agent":"claude","session":"{session}","seq":{seq}}}"#
        )
    };
    assert_eq!(
        send(&dir.socket(), at(1).as_bytes()).trim(),
        r#"{"ok":true}"#
    );
    assert_eq!(
        send(&dir.socket(), at(order_stamp_ns()).as_bytes()).trim(),
        r#"{"ok":true}"#
    );
    assert_eq!(
        send(&dir.socket(), at(1).as_bytes()).trim(),
        r#"{"ok":false,"reason":"stale"}"#
    );
    apply_at(false, None);
}

/// Mixed fleet. A seq-less report is ordered by its accept stamp, which is
/// never earlier than when its hook began, so it can land *after* a newer
/// seq-bearing report and be applied over it. Nothing in the protocol can
/// tell: the seq-less hook's entry time is not on the wire. This characterises
/// the limit (it holds only while a seq-less hook and a new one share a
/// session) so a change to it is a visible decision.
#[test]
fn a_seqless_report_that_stalled_is_applied_over_a_newer_seq_report() {
    let mut book = crate::alerts::OrderBook::default();
    // New hook: began at 1_000, accepted promptly at 1_100.
    let newer = order_for(Some(1_000), 1_100);
    // Old hook: began at 900 (earlier), stalled, accepted at 1_400.
    let older = order_for(None, 1_400);
    assert!(book.admit("term-1-a", newer, false));
    assert!(
        book.admit("term-1-a", older, false),
        "the limit moved: a seq-less report is no longer applied after a newer seq one"
    );
    // And the reverse is dropped, rightly: a seq report that began before a
    // seq-less one was accepted.
    let mut book = crate::alerts::OrderBook::default();
    assert!(book.admit("term-1-a", order_for(None, 1_400), false));
    assert!(!book.admit("term-1-a", order_for(Some(1_000), 1_500), false));
}

/// A report with `agent` of the wrong type is not silently read as the default
/// agent: `subject`, `subagent` and `seq` are strict about type, and an agent
/// that is not a known name is refused. A number is not a known name.
#[test]
fn an_agent_of_the_wrong_type_is_refused_not_read_as_claude() {
    for agent in ["5", "true", "[\"claude\"]", "{}"] {
        let json = format!(r#"{{"v":1,"event":"error","agent":{agent}}}"#);
        assert_eq!(
            parse_report_stamped(json.as_bytes()).map(|_| ()),
            Err(Reason::UnknownAgent),
            "agent {agent} was accepted"
        );
    }
}

/// The two ways a version can be wrong are one token on the wire, and the
/// reply says "max_v 1" for both. A report with no `v` at all and one with
/// `v: 2` are told the same thing; a hook that sent `v: 1.0` is told the app
/// speaks v1 at most. This pins the collision so it is a decision.
#[test]
fn a_version_that_is_not_an_integer_gets_the_future_version_reply() {
    let none = parse_report_stamped(br#"{"event":"error"}"#).map(|_| ());
    let future = parse_report_stamped(br#"{"v":2,"event":"error"}"#).map(|_| ());
    let float = parse_report_stamped(br#"{"v":1.0,"event":"error"}"#).map(|_| ());
    assert_eq!(none, Err(Reason::UnsupportedVersion));
    assert_eq!(future, Err(Reason::UnsupportedVersion));
    assert_eq!(float, Err(Reason::UnsupportedVersion));
    let reply = Outcome::Refused(Reason::UnsupportedVersion)
        .reply()
        .unwrap();
    assert!(reply.contains("\"max_v\":1"), "{reply}");
}
