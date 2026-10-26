//! Adversarial checks on the hook's half of the report protocol: replies from
//! an old or hostile app, and the size of the report whatever text it carries.
//!
//! Written by a reviewer who did not write the protocol. A failing test here
//! is a claim that is false.

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

/// Replies an old app (or something that is not the app) can give. Only
/// `{"ok":true}` is an acknowledgement; everything else is an error that names
/// no raw control character, whatever the bytes were.
#[cfg(unix)]
#[test]
fn only_an_ok_true_is_an_acknowledgement_and_no_error_carries_raw_control_bytes() {
    let replies: Vec<Vec<u8>> = vec![
        br#"{"ok":false}"#.to_vec(),
        b"{\"ok\":false}\n".to_vec(),
        b"".to_vec(),
        b"\n".to_vec(),
        b"garbage".to_vec(),
        b"\x1b[2J\x1b]0;pwned\x07".to_vec(),
        b"{\"ok\":\"\x1b[31mtrue\"}".to_vec(),
        br#"{"ok":"true"}"#.to_vec(),
        br#"{"ok":1}"#.to_vec(),
        br#"{"ok":null}"#.to_vec(),
        br#"[true]"#.to_vec(),
        br#"true"#.to_vec(),
        br#"{"ok":true}{"ok":true}"#.to_vec(),
        br#"{"ok":true} trailing"#.to_vec(),
        // Cut mid-document at 64 bytes, as the hook reads it.
        format!(r#"{{"ok":false,"reason":"{}"}}"#, "q".repeat(100)).as_bytes()[..64].to_vec(),
        vec![0xff, 0xfe, 0x00, 0x1b],
    ];
    for reply in &replies {
        match acknowledged(reply) {
            Ok(()) => panic!(
                "{:?} was taken as an acknowledgement",
                String::from_utf8_lossy(reply)
            ),
            Err(said) => assert!(
                !said.chars().any(char::is_control),
                "{:?} produced an error with a raw control character: {said:?}",
                String::from_utf8_lossy(reply)
            ),
        }
    }
    assert_eq!(acknowledged(b"{\"ok\":true}\n"), Ok(()));
}

/// A hostile reply cannot make the hook print anything but GitPulse's own
/// sentence for a known token, and for an unknown token only printable text.
/// Control characters go; so must the characters that reorder or hide text
/// (bidi overrides, zero-width, line and paragraph separators) because the
/// line lands on a terminal next to the user's own output.
#[cfg(unix)]
#[test]
fn a_hostile_reason_cannot_reorder_or_hide_what_the_hook_prints() {
    let hostile = [
        "a\u{202e}evil",
        "a\u{2066}b",
        "a\u{200b}b",
        "a\u{2028}b",
        "a\u{2029}b",
        "a\u{feff}b",
        "a\u{9b}31mb",
        "a\u{7f}b",
        "a\u{0}b",
    ];
    for reason in hostile {
        let reply = serde_json::json!({ "ok": false, "reason": reason }).to_string();
        let said = acknowledged(reply.as_bytes()).unwrap_err();
        let bad = |c: char| {
            c.is_control()
                || matches!(
                    c,
                    '\u{200b}'..='\u{200f}'
                        | '\u{2028}'..='\u{202e}'
                        | '\u{2060}'..='\u{2069}'
                        | '\u{feff}'
                )
        };
        assert!(
            !said.chars().any(bad),
            "{reason:?} reached the hook's output as {said:?}"
        );
    }
}

/// `max_v` is a number or it is not shown; no text from the reply reaches the
/// line through it.
#[cfg(unix)]
#[test]
fn max_v_can_only_ever_be_a_number_in_the_line() {
    for max_v in [
        r#""\u001b[2J""#,
        r#""3""#,
        "1e3",
        "-1",
        "1.5",
        "null",
        "[1]",
        "18446744073709551615",
    ] {
        let reply = format!(r#"{{"ok":false,"reason":"unsupported_version","max_v":{max_v}}}"#);
        let said = acknowledged(reply.as_bytes()).unwrap_err();
        assert!(!said.chars().any(char::is_control), "{max_v}: {said:?}");
        assert!(
            !said.contains("2J") && !said.contains("\"3\""),
            "{max_v}: text from max_v reached the line: {said:?}"
        );
    }
    let said = acknowledged(
        br#"{"ok":false,"reason":"unsupported_version","max_v":18446744073709551615}"#,
    )
    .unwrap_err();
    assert!(said.contains("v18446744073709551615"), "{said}");
}

/// A known token is looked up exactly: a token with anything appended is not
/// that token, and is shown as an unknown one, filtered.
#[cfg(unix)]
#[test]
fn a_known_token_with_trailing_text_is_not_that_token() {
    for token in ["stale\n", "stale ", "Stale", "stale\u{1b}[2J", " stale"] {
        assert!(Reason::from_token(token).is_none(), "{token:?}");
    }
    let said = acknowledged(b"{\"ok\":false,\"reason\":\"stale\\n\\u001b[2Jfake\"}").unwrap_err();
    assert!(!said.contains(Reason::Stale.message()), "{said:?}");
    assert!(!said.chars().any(char::is_control), "{said:?}");
}

/// A report is never over the socket's cap, never invalid, and the message in
/// it is a prefix of what was given, cut as late as the two limits allow, for
/// text built from every width and escape class and a cwd at, under and over
/// its own limit.
#[test]
fn the_report_fits_whatever_the_text_and_the_clip_is_tight() {
    const ALPHABET: &[&str] = &[
        "a", "é", "日", "😀", "\u{1b}", "\"", "\\", "\n", "\t", "\u{0}", "\u{7f}", "\u{2028}",
        "\u{8}", "\u{c}", "\r", "/",
    ];
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut cut = 0;
    for case in 0..400 {
        let pick = |count: usize, next: &mut dyn FnMut() -> u64| -> String {
            let narrow = next() % 3;
            (0..count)
                .map(|_| match narrow {
                    0 => ALPHABET[(next() % ALPHABET.len() as u64) as usize],
                    1 => "\u{1b}",
                    _ => ["日", "😀", "é"][(next() % 3) as usize],
                })
                .collect()
        };
        let message_chars = (next() % 1500) as usize;
        let cwd = match case % 4 {
            0 => String::new(),
            1 => format!("/w/{}", "d".repeat(4096 - 3)),
            2 => format!(
                "/w/{}",
                pick((next() % 600) as usize, &mut next).replace('/', "x")
            ),
            _ => format!("/w/{}", "d".repeat(4097)),
        };
        let message = pick(message_chars, &mut next);
        let input = HookInput {
            cwd: cwd.clone(),
            message: message.clone(),
            ..HookInput::default()
        };
        if message.trim().is_empty() {
            continue;
        }
        let payload = sent("agent_completed", &input);
        assert!(
            payload.len() <= MAX_REPORT_BYTES,
            "case {case}: {} bytes",
            payload.len()
        );
        let parsed = bridge::parse_report(payload.as_bytes());
        assert!(
            parsed.is_ok(),
            "case {case}: the socket refuses it: {parsed:?}"
        );
        let json: Value = serde_json::from_str(&payload).unwrap();
        let kept = json.get("message").and_then(Value::as_str).unwrap_or("");
        assert!(message.starts_with(kept), "case {case}: not a prefix");
        assert!(kept.len() <= NOTIFY_MESSAGE_BYTES, "case {case}");
        if kept.len() < message.len() && kept.len() < NOTIFY_MESSAGE_BYTES - 4 {
            // Cut by the serialized room, so the next character must not have
            // fit: the report is within one character's escape (6) of the cap,
            // or of the raw limit.
            cut += 1;
            let next_char = message[kept.len()..].chars().next().unwrap();
            let cost = json_len(next_char);
            assert!(
                payload.len() + cost > MAX_REPORT_BYTES
                    || kept.len() + next_char.len_utf8() > NOTIFY_MESSAGE_BYTES,
                "case {case}: clipped too early: {} bytes used, the next char ({next_char:?}) costs {cost}",
                payload.len()
            );
        }
    }
    assert!(cut > 20, "only {cut} cases exercised the serialized limit");
}

/// The report never carries more fields than the bridge reads (12), with every
/// optional field present, so an old bridge that stops at 12 never refuses it.
#[test]
fn a_report_with_every_field_stays_inside_the_field_limit() {
    let input = HookInput {
        cwd: "/w/repo".into(),
        message: "hello".into(),
        tool_subject: "abcdef0123456789".into(),
        agent_id: "sub-1".into(),
        ..HookInput::default()
    };
    let json: Value = serde_json::from_str(&sent("permission_request", &input)).unwrap();
    let fields = json.as_object().unwrap().len();
    assert!(fields <= 10, "{fields} fields: {json}");
    // `seq` is the one that would be added with a started deadline.
    assert!(fields < 12);
}
