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

/// What follows the object is not this hook's business, whatever it is.
#[test]
fn what_follows_the_object_is_ignored() {
    let mut body = post_tool_use(10).into_bytes();
    body.extend_from_slice(b"\n{not json at all");
    let input = read_notify_input(Cursor::new(body), MAX_READ_BYTES).unwrap();
    assert_eq!(input.tool_use_id, "toolu_1");
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
