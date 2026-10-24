//! The hook-report protocol as a peer sees it, over the real socket and the
//! real hub: the reply tells the truth about the report, every refusal carries
//! a reason from the closed set, and a report's own `seq` orders it.
//!
//! Wire compatibility is the constraint these keep: a peer that reads only
//! `ok` (every hook that shipped before reasons) must see what it always saw.
use super::*;
use crate::alerts::bridge::Reason;
use std::io::{Read, Write};

fn unique(tag: &str) -> String {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    format!(
        "proto-{tag}-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

fn reply_json(reply: &str) -> serde_json::Value {
    serde_json::from_str(reply.trim()).unwrap_or_else(|e| panic!("not JSON ({e}): {reply:?}"))
}

/// One payload per refusal a report can meet, and the token each must carry.
fn refusals() -> Vec<(&'static str, Vec<u8>, &'static str)> {
    let storm = {
        let mut json = String::from(r#"{"v":1,"event":"error""#);
        for i in 0..20 {
            json.push_str(&format!(r#","f{i}":1"#));
        }
        json.push('}');
        json.into_bytes()
    };
    let big = format!(
        r#"{{"v":1,"event":"error","message":"{}"}}"#,
        "x".repeat(MAX_REPORT_BYTES)
    )
    .into_bytes();
    vec![
        ("not json", b"not json".to_vec(), "not_json"),
        ("array", b"[1]".to_vec(), "not_an_object"),
        ("field storm", storm, "too_many_fields"),
        (
            "no version",
            br#"{"event":"error"}"#.to_vec(),
            "unsupported_version",
        ),
        (
            "future version",
            br#"{"v":2,"event":"error"}"#.to_vec(),
            "unsupported_version",
        ),
        ("no event", br#"{"v":1}"#.to_vec(), "no_event"),
        (
            "unknown event",
            br#"{"v":1,"event":"launch a missile"}"#.to_vec(),
            "unknown_event",
        ),
        (
            "bad subject",
            br#"{"v":1,"event":"tool_finished","session":"s","subject":"zz"}"#.to_vec(),
            "malformed_subject",
        ),
        (
            "bad subagent",
            br#"{"v":1,"event":"error","subagent":"yes"}"#.to_vec(),
            "malformed_subagent",
        ),
        (
            "zero seq",
            br#"{"v":1,"event":"error","seq":0}"#.to_vec(),
            "malformed_seq",
        ),
        (
            "unknown agent",
            br#"{"v":1,"event":"error","agent":"nobody"}"#.to_vec(),
            "unknown_agent",
        ),
        (
            "state without a session",
            br#"{"v":1,"event":"tool_finished"}"#.to_vec(),
            "state_outside_session",
        ),
        ("too big", big, "oversized"),
    ]
}

/// Every way a report can fail to parse comes back with its own token, and
/// the set of tokens is exactly the closed set.
#[test]
fn every_refusal_carries_its_own_closed_set_reason() {
    let _own = own_listener();
    let dir = ShortDir::new("protoreason");
    apply_at(true, Some(dir.socket()));
    assert!(status().listening, "{:?}", status().error);
    let mut seen = std::collections::BTreeSet::new();
    for (name, payload, token) in refusals() {
        let reply = send(&dir.socket(), &payload);
        let json = reply_json(&reply);
        assert_eq!(json["ok"], false, "{name}: {reply}");
        assert_eq!(json["reason"], token, "{name}: {reply}");
        assert!(
            Reason::from_token(token).is_some(),
            "{name}: {token} is not in the closed set"
        );
        seen.insert(token);
    }
    apply_at(false, None);
    // Every parse refusal is covered by a payload above; the rest of the set
    // is the connection's and the hub's, tested where they arise.
    let parse_reasons: std::collections::BTreeSet<&str> = Reason::ALL
        .into_iter()
        .filter(|r| {
            !matches!(
                r,
                Reason::Unreadable
                    | Reason::AtCapacity
                    | Reason::Stale
                    | Reason::QueueFull
                    | Reason::NotRunning
            )
        })
        .map(Reason::token)
        .collect();
    assert_eq!(seen, parse_reasons);
}

/// A version this bridge does not speak is answered with the newest it does.
#[test]
fn a_version_refusal_names_the_newest_version_the_bridge_speaks() {
    let _own = own_listener();
    let dir = ShortDir::new("protomaxv");
    apply_at(true, Some(dir.socket()));
    let reply = send(&dir.socket(), br#"{"v":2,"event":"error"}"#);
    let json = reply_json(&reply);
    assert_eq!(json["reason"], "unsupported_version", "{reply}");
    assert_eq!(json["max_v"], PROTOCOL_VERSION, "{reply}");
    // Only that refusal is about versions.
    let reply = send(&dir.socket(), br#"{"v":1}"#);
    assert!(reply_json(&reply).get("max_v").is_none(), "{reply}");
    apply_at(false, None);
}

/// What an old hook sees, which reads only `ok`: success is byte-for-byte the
/// reply it has always had, and a refusal still has `ok:false` first.
#[test]
fn an_old_hook_reading_only_ok_sees_what_it_always_saw() {
    let _own = own_listener();
    let dir = ShortDir::new("protoold");
    apply_at(true, Some(dir.socket()));
    // No `seq`, as every hook before this one sent.
    let session = unique("old");
    let report =
        format!(r#"{{"v":1,"event":"idle_prompt","agent":"claude","session":"{session}"}}"#);
    assert_eq!(send(&dir.socket(), report.as_bytes()), "{\"ok\":true}\n");
    let reply = send(&dir.socket(), b"not json");
    assert!(reply.starts_with("{\"ok\":false"), "{reply}");
    assert_eq!(reply_json(&reply)["ok"], false);
    apply_at(false, None);
}

/// A report that carries its `seq` is ordered by it: the `Stop` that began
/// first but was accepted last is not applied over the later `PostToolUse`,
/// and the reply says so instead of acknowledging it.
#[test]
fn a_report_older_by_seq_is_refused_as_stale_not_acknowledged() {
    let _own = own_listener();
    let dir = ShortDir::new("protostale");
    apply_at(true, Some(dir.socket()));
    assert!(status().listening, "{:?}", status().error);
    let session = unique("stale");
    // Two hook processes began in this order, and the second was accepted first.
    let began_first = order_stamp_ns();
    // The clock ticks in tens of nanoseconds, so two reads can tie; wait for it
    // to move so the two hooks have distinct stamps.
    let began_second = loop {
        let now = order_stamp_ns();
        if now > began_first {
            break now;
        }
    };
    let at = |event: &str, seq: u64| {
        format!(r#"{{"v":1,"event":"{event}","agent":"claude","session":"{session}","seq":{seq}}}"#)
    };
    let stale_before = crate::alerts::status().stale;
    assert_eq!(
        send(&dir.socket(), at("turn_finished", began_second).as_bytes()).trim(),
        r#"{"ok":true}"#
    );
    let reply = send(&dir.socket(), at("tool_finished", began_first).as_bytes());
    assert_eq!(reply.trim(), r#"{"ok":false,"reason":"stale"}"#);
    assert_eq!(crate::alerts::status().stale - stale_before, 1);
    assert!(
        within(std::time::Duration::from_secs(5), || {
            finished_stands_for(&session)
        }),
        "the late report erased the finished turn"
    );
    apply_at(false, None);
}

/// A `seq` ahead of the clock is capped at the accept stamp rather than
/// trusted, so one report cannot make every later one from its session stale.
#[test]
fn a_seq_from_the_future_does_not_poison_the_session() {
    let _own = own_listener();
    let dir = ShortDir::new("protofuture");
    apply_at(true, Some(dir.socket()));
    let session = unique("future");
    let report = |event: &str, seq: &str| {
        format!(r#"{{"v":1,"event":"{event}","agent":"claude","session":"{session}"{seq}}}"#)
    };
    assert_eq!(
        send(
            &dir.socket(),
            report("idle_prompt", &format!(r#","seq":{}"#, u64::MAX)).as_bytes()
        )
        .trim(),
        r#"{"ok":true}"#
    );
    assert_eq!(
        send(&dir.socket(), report("idle_prompt", "").as_bytes()).trim(),
        r#"{"ok":true}"#,
        "a report from the future silenced the session"
    );
    apply_at(false, None);
}

/// A report the hub could not take is not acknowledged: each hub refusal maps
/// to its own reason, and only a taken report is `ok:true`.
#[test]
fn the_reply_follows_what_the_hub_did_with_the_report() {
    use crate::alerts::Offer;
    let report = br#"{"v":1,"event":"idle_prompt","agent":"claude","session":"term-1"}"#;
    for (offer, expected) in [
        (Offer::Accepted, r#"{"ok":true}"#),
        (Offer::Coalesced, r#"{"ok":true}"#),
        (Offer::Stale, r#"{"ok":false,"reason":"stale"}"#),
        (Offer::QueueFull, r#"{"ok":false,"reason":"queue_full"}"#),
        (Offer::NotRunning, r#"{"ok":false,"reason":"not_running"}"#),
    ] {
        let (mut client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        client.write_all(report).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let outcome = handle_into(server, std::time::Duration::from_secs(5), &mut |_, _| offer);
        let mut reply = String::new();
        client.read_to_string(&mut reply).unwrap();
        assert_eq!(reply.trim(), expected, "{offer:?}");
        assert_eq!(outcome == Outcome::Accepted, offer.taken(), "{offer:?}");
    }
}

/// The tokens are the wire contract: distinct, stable, and each one found
/// again by its own name.
#[test]
fn the_reason_tokens_are_distinct_and_round_trip() {
    let mut tokens = std::collections::BTreeSet::new();
    for reason in Reason::ALL {
        assert!(tokens.insert(reason.token()), "{} repeats", reason.token());
        assert_eq!(Reason::from_token(reason.token()), Some(reason));
        assert!(
            reason
                .token()
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'_'),
            "{}",
            reason.token()
        );
        assert!(!reason.message().is_empty());
    }
    assert_eq!(Reason::from_token("not a reason"), None);
}

/// `parse_report` keeps the strings it has always returned.
#[test]
fn parse_report_still_returns_its_old_messages() {
    for (_, payload, token) in refusals() {
        let reason = Reason::from_token(token).unwrap();
        assert_eq!(parse_report(&payload), Err(reason.message()), "{token}");
    }
}
