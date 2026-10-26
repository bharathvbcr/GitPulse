//! The hook's half of the report protocol: what it sends when the text is
//! awkward, and what it says when the app refuses.

use super::*;
#[cfg(unix)]
use crate::alerts::bridge::Reason;
use crate::alerts::bridge::{self, MAX_REPORT_BYTES};

fn sent(event: &str, input: &HookInput) -> String {
    let serial = crate::test_support::env::env_serial();
    let _env = crate::test_support::env::bind_env(&serial)
        .set(AGENT_KIND_ENV, "claude")
        .set(bridge::SESSION_ENV, "term-4-1")
        .set(bridge::SOCKET_ENV, "/tmp/gitpulse-test.sock");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
    let sink = seen.clone();
    let result = run_notify(Some(event), input, move |_, payload| {
        *sink.lock().unwrap() = Some(payload.to_owned());
        Ok(())
    });
    assert!(result.is_ok(), "{result:?}");
    let payload = seen.lock().unwrap().clone().expect("a report was sent");
    payload
}

/// A report whose message is all control characters and whose cwd is as long
/// as a cwd may be. The raw sizes are 1024 and 4096, both inside their limits;
/// serialized, the message alone is 6144. It is delivered clipped, not refused.
#[test]
fn a_message_that_escapes_wide_is_clipped_to_fit_not_refused() {
    let input = HookInput {
        cwd: format!("/w/{}", "d".repeat(4096 - 3)),
        message: "\u{1b}".repeat(1024),
        ..HookInput::default()
    };
    assert_eq!(input.cwd.len(), 4096);
    let payload = sent("agent_completed", &input);
    assert!(
        payload.len() <= MAX_REPORT_BYTES,
        "{} bytes serialized, over the {MAX_REPORT_BYTES} the socket takes",
        payload.len()
    );
    let notice = bridge::parse_report(payload.as_bytes()).expect("the socket accepts it");
    assert!(notice.place.is_some(), "the place was dropped to make room");
    let json: Value = serde_json::from_str(&payload).unwrap();
    let message = json["message"]
        .as_str()
        .expect("some of the message is kept");
    assert!(
        !message.is_empty() && message.chars().count() < 1024,
        "{}",
        message.len()
    );
}

/// A place whose escaping alone is over the cap costs the report its place,
/// not the report.
#[test]
fn a_place_that_cannot_fit_is_dropped_rather_than_the_report() {
    let input = HookInput {
        cwd: "\u{1b}".repeat(4096),
        message: "done".into(),
        ..HookInput::default()
    };
    let payload = sent("agent_completed", &input);
    assert!(payload.len() <= MAX_REPORT_BYTES, "{}", payload.len());
    let json: Value = serde_json::from_str(&payload).unwrap();
    assert!(json.get("cwd").is_none());
    assert_eq!(json["message"], "done");
    assert!(bridge::parse_report(payload.as_bytes()).is_ok());
}

/// Text that fits is sent whole, whatever it contains: the clip is a bound,
/// not a rewrite.
#[test]
fn a_message_that_fits_is_sent_whole() {
    let input = HookInput {
        cwd: "/Users/me/GitPulse".into(),
        message: "line one\nline \"two\" \\ \u{1b}[31m ünïcode ✓".into(),
        ..HookInput::default()
    };
    let json: Value = serde_json::from_str(&sent("agent_completed", &input)).unwrap();
    assert_eq!(json["message"], input.message.as_str());
}

#[test]
fn the_clip_never_splits_a_character_and_never_exceeds_either_bound() {
    for text in [
        "ü".repeat(1000),
        "\u{1b}".repeat(1000),
        "\"".repeat(1000),
        "✓x".repeat(500),
    ] {
        for json in [0usize, 1, 5, 6, 7, 100, 3000] {
            let clipped = clip_to_json(&text, NOTIFY_MESSAGE_BYTES, json);
            assert!(clipped.len() <= NOTIFY_MESSAGE_BYTES);
            let serialized = serde_json::to_string(clipped).unwrap().len() - 2;
            assert!(serialized <= json, "{serialized} > {json}");
        }
    }
}

/// Without `start_deadline` (the unit tests, an embedder) there is no entry
/// stamp, so a report carries no `seq` rather than a made-up one.
#[test]
fn a_report_carries_no_seq_when_the_process_never_took_an_entry_stamp() {
    let input = HookInput {
        message: "x".into(),
        ..HookInput::default()
    };
    let json: Value = serde_json::from_str(&sent("agent_completed", &input)).unwrap();
    assert!(json.get("seq").is_none());
}

/* ── what the hook says about a refusal ───────────────────────────────────── */

/// Every reason the app can send is printed with its own token, and none of
/// them blames a version difference.
#[cfg(unix)]
#[test]
fn every_reason_is_printed_verbatim_and_none_blames_a_version() {
    for reason in Reason::ALL {
        if reason == Reason::Stale {
            continue;
        }
        let reply = format!(r#"{{"ok":false,"reason":"{}"}}"#, reason.token());
        let said = acknowledged(reply.as_bytes()).unwrap_err();
        assert!(said.contains(reason.token()), "{said}");
        if reason != Reason::UnsupportedVersion {
            assert!(!said.contains("version"), "{said}");
        }
    }
}

/// A report superseded by a newer one from its session was delivered: the hook
/// does not warn. Every other reason still does.
#[cfg(unix)]
#[test]
fn a_superseded_report_is_delivered_not_a_warning() {
    assert_eq!(
        acknowledged(br#"{"ok":false,"reason":"stale"}"#),
        Ok(()),
        "stale is the system working"
    );
    for reason in Reason::ALL.into_iter().filter(|r| *r != Reason::Stale) {
        let reply = format!(r#"{{"ok":false,"reason":"{}"}}"#, reason.token());
        assert!(
            acknowledged(reply.as_bytes()).is_err(),
            "{}",
            reason.token()
        );
    }
    // A bare refusal and a reason this build does not know are still refusals.
    assert!(acknowledged(br#"{"ok":false}"#).is_err());
    assert!(acknowledged(br#"{"ok":false,"reason":"stale_ish"}"#).is_err());
}

/// Only a refusal about the version talks about versions, and it gives both.
#[cfg(unix)]
#[test]
fn a_version_refusal_names_both_versions() {
    let said =
        acknowledged(br#"{"ok":false,"reason":"unsupported_version","max_v":3}"#).unwrap_err();
    assert!(said.contains("unsupported_version"), "{said}");
    assert!(said.contains("sends protocol v1"), "{said}");
    assert!(said.contains("v3"), "{said}");
    let said = acknowledged(br#"{"ok":false,"reason":"unsupported_version"}"#).unwrap_err();
    assert!(said.contains("did not say"), "{said}");
}

/// An app older than reasons answers a bare `ok:false`. That is reported as
/// what it is, with no claim about versions it does not support.
#[cfg(unix)]
#[test]
fn an_app_that_gives_no_reason_is_not_assumed_to_be_a_different_version() {
    let said = acknowledged(b"{\"ok\":false}\n").unwrap_err();
    assert!(said.contains("no reason"), "{said}");
    assert!(!said.contains("version"), "{said}");
}

/// An app newer than this hook may send a reason this build has never heard
/// of. It is shown as sent, minus anything that is not printable.
#[cfg(unix)]
#[test]
fn a_reason_from_a_newer_app_is_shown_as_sent() {
    let said = acknowledged(br#"{"ok":false,"reason":"quota_exhausted_v2"}"#).unwrap_err();
    assert!(said.ends_with("quota_exhausted_v2"), "{said}");
    let said = acknowledged(b"{\"ok\":false,\"reason\":\"a\\u001b[31mb\"}").unwrap_err();
    assert!(!said.contains('\u{1b}'), "{said:?}");
    let long = format!(r#"{{"ok":false,"reason":"{}"}}"#, "z".repeat(500));
    assert!(acknowledged(long.as_bytes()).unwrap_err().len() < 200);
}

/// The two reasons the bridge gained for owner decision 6 are in the closed
/// set, so a hook prints GitPulse's own sentence for them.
#[cfg(unix)]
#[test]
fn the_session_and_rate_refusals_print_the_bridges_own_words() {
    let said = acknowledged(br#"{"ok":false,"reason":"unknown_session"}"#).unwrap_err();
    assert!(said.contains("no terminal session has that id"), "{said}");
    let said = acknowledged(br#"{"ok":false,"reason":"rate_limited"}"#).unwrap_err();
    assert!(
        said.contains("reporting faster than a session can"),
        "{said}"
    );
}

/// Fields the hook has not heard of change nothing on success, so a newer app
/// can add to its reply.
#[cfg(unix)]
#[test]
fn an_acknowledgement_with_extra_fields_is_still_one() {
    assert_eq!(acknowledged(br#"{"ok":true,"queued":3}"#), Ok(()));
}
