//! `notify` reads the fields it uses and skips the rest of the payload.

use super::notify_input::{read_notify_input, KEPT_FIELDS, MAX_READ_BYTES};
use super::*;
use serde_json::json;
use std::io::{Cursor, Read};

fn post_tool_use(response_bytes: usize) -> String {
    json!({
        "session_id": "abc",
        "hook_event_name": "PostToolUse",
        "cwd": "/work/repo",
        "tool_name": "Bash",
        "tool_input": {"command": "cargo test", "description": "run"},
        "tool_use_id": "toolu_1",
        "tool_response": {"stdout": "x".repeat(response_bytes)},
    })
    .to_string()
}

/// The walk keeps what `from_value` reads: a payload with every kept field
/// and a large unknown one reads as the whole-document parse of the same
/// payload without the unknown one.
#[test]
fn the_walk_reads_what_a_whole_parse_reads() {
    let mut object = serde_json::Map::new();
    for field in KEPT_FIELDS {
        let value = match field {
            "tool_input" => json!({"command": "ls", "file_path": "/a/b", "old_string": "q"}),
            "workspace_roots" | "workspaceRoots" => json!(["/ws/one"]),
            "cursor_version" => json!("1.2.3"),
            _ => json!(format!("v-{field}")),
        };
        object.insert(field.to_owned(), value);
    }
    let small = Value::Object(object.clone()).to_string();
    object.insert(
        "tool_response".into(),
        json!({"stdout": "y".repeat(300_000)}),
    );
    object.insert("unknown_field".into(), json!([1, 2, {"deep": "z"}]));
    let big = Value::Object(object).to_string();

    let whole = parse_input(&small).unwrap();
    let walked = read_notify_input(Cursor::new(big.into_bytes()), MAX_READ_BYTES).unwrap();
    assert_eq!(walked, whole);
    assert!(!walked.tool_subject.is_empty());
    assert_eq!(walked.message, "v-message");
}

/// The incident: a 5 MiB `tool_response` made the whole payload refuse, and the
/// `tool_finished` that would have cleared the session's question was lost.
#[test]
fn a_five_mib_tool_response_still_reads_its_subject() {
    let body = post_tool_use(5 * 1024 * 1024);
    assert!(body.len() > MAX_INPUT_BYTES);
    assert!(
        read_input(Cursor::new(body.clone().into_bytes())).is_err(),
        "the whole-payload reader still refuses it, which is why notify does not use it"
    );
    let input = read_notify_input(Cursor::new(body.into_bytes()), MAX_READ_BYTES).unwrap();
    assert_eq!(input.tool_name, "Bash");
    assert_eq!(input.tool_use_id, "toolu_1");
    assert_eq!(input.cwd, "/work/repo");
    assert!(!input.tool_subject.is_empty());
    assert_eq!(input.message, "");
}

#[test]
fn a_payload_past_its_limit_reports_with_no_fields() {
    let body = post_tool_use(64 * 1024);
    let input = read_notify_input(Cursor::new(body.into_bytes()), 4096).unwrap();
    assert_eq!(input, HookInput::default());
}

#[test]
fn what_is_not_a_json_object_is_refused() {
    for text in [
        "",
        "   ",
        "{",
        r#"{"cwd":"#,
        "[]",
        "null",
        "42",
        r#""s""#,
        "{]",
    ] {
        assert!(
            read_notify_input(Cursor::new(text.as_bytes().to_vec()), MAX_READ_BYTES).is_err(),
            "accepted {text:?}"
        );
    }
}

/// Accept and reject are the old path's: whitespace after the object is fine,
/// anything else (a second document, a NUL) refuses the payload.
#[test]
fn only_whitespace_may_follow_the_object() {
    for tail in ["\n", "  \t\r\n", "\u{3000}"] {
        let mut body = post_tool_use(10).into_bytes();
        body.extend_from_slice(tail.as_bytes());
        let input = read_notify_input(Cursor::new(body), MAX_READ_BYTES).unwrap();
        assert_eq!(input.tool_use_id, "toolu_1", "{tail:?}");
    }
    for tail in [&b"\n{not json at all"[..], b"\0", b" x", b"{}"] {
        let mut body = post_tool_use(10).into_bytes();
        body.extend_from_slice(tail);
        assert!(
            read_notify_input(Cursor::new(body), MAX_READ_BYTES).is_err(),
            "{tail:?}"
        );
    }
}

/// The least the host needs between the hook's last act and its own timeout to
/// see the process exit.
const EXIT_MARGIN: Duration = Duration::from_millis(250);

/// Every `(notify argument, timeout in seconds)` among `hooks`, a `hooks`
/// object as the plugin manifest or the generated settings carry it.
fn notify_timeouts(hooks: &Value) -> Vec<(String, f64)> {
    let mut found = Vec::new();
    for groups in hooks.as_object().unwrap().values() {
        for group in groups.as_array().unwrap() {
            for hook in group["hooks"].as_array().unwrap() {
                if let Some((_, argument)) =
                    hook["command"].as_str().unwrap().split_once(" notify ")
                {
                    found.push((
                        argument.to_owned(),
                        hook["timeout"].as_f64().expect("timeout"),
                    ));
                }
            }
        }
    }
    found
}

/// The read and the socket report end a margin before the host's `timeout`
/// for the event, so a writer that dribbles to the last moment cannot get the
/// hook killed mid-report. Checked against every notify hook the launch
/// injects, read from the generated settings, and against the plugin's own
/// copies of the same events, read from the manifest: neither is restated.
#[test]
fn the_whole_run_ends_a_margin_before_the_host_timeout() {
    use super::notify_input::{budgets_for, host_timeout, NOTIFY_HOST_TIMEOUT, NOTIFY_READ_BUDGET};
    assert_eq!(
        NOTIFY_READ_BUDGET, NOTIFY_BUDGET,
        "the 5 s case is the old budgets"
    );
    assert!(
        NOTIFY_READ_BUDGET < BUDGET,
        "the general read budget is no longer what notify uses"
    );
    let fits = |argument: &str, timeout: f64, whose: &str| {
        let (read, send) = budgets_for(argument);
        assert!(read > Duration::ZERO && send > Duration::ZERO);
        let run = read + send;
        let timeout = Duration::from_secs_f64(timeout);
        assert!(
            run + EXIT_MARGIN <= timeout,
            "{whose} notify {argument}: read {read:?} + send {send:?} leaves under {EXIT_MARGIN:?} of its {timeout:?}"
        );
    };

    let injected = crate::workbench::claude_hook::settings_hooks(std::path::Path::new(
        "/opt/gitpulse/gitpulse-hook",
    ))
    .unwrap();
    let entries = notify_timeouts(&injected);
    assert!(entries.len() >= 6, "{entries:?}");
    for (argument, timeout) in &entries {
        // What the launch asks for is what the hook budgets for.
        assert!(
            *timeout <= host_timeout(argument).as_secs_f64(),
            "injected notify {argument} is given {timeout} s, more than the {:?} its run is held to",
            host_timeout(argument)
        );
        fits(argument, *timeout, "injected");
    }
    assert!(entries
        .iter()
        .any(|(a, t)| a == "session_ended" && *t <= 1.0));

    let manifest: Value =
        serde_json::from_str(include_str!("../../../plugins/gitpulse/hooks/hooks.json")).unwrap();
    let shipped = notify_timeouts(&manifest["hooks"]);
    assert!(
        shipped.len() >= 10,
        "only {} notify entries read",
        shipped.len()
    );
    for (argument, timeout) in &shipped {
        // The plugin's copy of an event may allow more than the hook holds
        // itself to (`session_ended` is held to the injected copy's 1 s), never
        // less.
        assert_eq!(
            *timeout,
            NOTIFY_HOST_TIMEOUT.as_secs_f64(),
            "plugin notify {argument}"
        );
        fits(argument, *timeout, "plugin");
    }
}

/// A reader that fails is an error, not an empty payload.
#[test]
fn a_reader_that_fails_is_refused() {
    struct Fails;
    impl Read for Fails {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("pipe broke"))
        }
    }
    assert!(read_notify_input(Fails, MAX_READ_BYTES).is_err());
}
