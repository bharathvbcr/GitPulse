//! Attacks on the `notify` stdin reader and the tool-call digest (W7-L3
//! adversarial review). The old path is `parse_input`, the whole-document
//! parse `notify` used before the change; the differential tests hold the new
//! reader to it.

use super::notify_input::{read_notify_input, MAX_READ_BYTES, NOTIFY_READ_BUDGET};
use super::*;
use serde_json::json;
use std::io::Cursor;
use std::time::Instant;

fn new_path(body: &[u8]) -> Result<HookInput, String> {
    read_notify_input(Cursor::new(body.to_vec()), MAX_READ_BYTES)
}

fn old_path(body: &[u8]) -> Result<HookInput, String> {
    let text = std::str::from_utf8(body).map_err(|e| e.to_string())?;
    parse_input(text)
}

/// A payload of exactly `total` bytes: a kept field then padding in an ignored
/// one.
fn payload_of(total: usize) -> Vec<u8> {
    let head =
        br#"{"session_id":"s-exact","tool_name":"Bash","tool_input":{"command":"ls"},"pad":""#;
    let tail = br#""}"#;
    let mut body = head.to_vec();
    body.extend(std::iter::repeat_n(b'p', total - head.len() - tail.len()));
    body.extend_from_slice(tail);
    assert_eq!(body.len(), total);
    body
}

#[test]
fn exactly_sixteen_mib_is_read_and_one_byte_more_reports_empty() {
    let exact = new_path(&payload_of(MAX_READ_BYTES as usize)).unwrap();
    assert_eq!(exact.session_id, "s-exact");
    assert_eq!(exact.tool_name, "Bash");
    let over = new_path(&payload_of(MAX_READ_BYTES as usize + 1)).unwrap();
    assert_eq!(over, HookInput::default());
}

/// A pipe that yields one byte then sleeps: a dribbling writer that never
/// finishes inside the budget must end in an error, not a hang.
struct Dribble;

impl Read for Dribble {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::thread::sleep(Duration::from_millis(200));
        buf[0] = b' ';
        Ok(1)
    }
}

#[test]
fn a_writer_that_dribbles_past_the_deadline_is_an_error_at_the_deadline() {
    let started = Instant::now();
    let result = read_notify_input(Dribble, MAX_READ_BYTES);
    let elapsed = started.elapsed();
    assert!(result.is_err(), "{result:?}");
    assert!(
        elapsed
            < crate::test_support::env::coverage_relaxed(
                NOTIFY_READ_BUDGET + Duration::from_secs(2)
            ),
        "took {elapsed:?} for a {NOTIFY_READ_BUDGET:?} budget"
    );
}

/// Differential: the same bytes through both paths, equal answer or equal
/// refusal. Each case is a way the two walks could differ.
#[test]
fn the_new_reader_agrees_with_the_old_path_on_edge_payloads() {
    let mut cases: Vec<(&str, Vec<u8>)> = vec![
        (
            "nul byte outside a string",
            b"{\"session_id\":\"a\"}\0".to_vec(),
        ),
        ("nul inside a string", b"{\"session_id\":\"a\0b\"}".to_vec()),
        ("escaped nul", br#"{"session_id":"a\u0000b"}"#.to_vec()),
        (
            "duplicate kept key, last wins",
            br#"{"session_id":"first","session_id":"second"}"#.to_vec(),
        ),
        (
            "duplicate, second is a non-string",
            br#"{"session_id":"first","session_id":7}"#.to_vec(),
        ),
        ("escaped key spelling", br#"{"session_id":"esc"}"#.to_vec()),
        (
            "workspaceRoots only",
            br#"{"workspaceRoots":[" ","/r"]}"#.to_vec(),
        ),
        (
            "both workspace spellings",
            br#"{"workspace_roots":["/a"],"workspaceRoots":["/b"]}"#.to_vec(),
        ),
        (
            "cursor_version null",
            br#"{"cursor_version":null,"cwd":"/c"}"#.to_vec(),
        ),
        ("leading whitespace", b"  \n\t{\"cwd\":\"/c\"}\n".to_vec()),
        ("bom", b"\xef\xbb\xbf{\"cwd\":\"/c\"}".to_vec()),
        ("array", b"[1,2]".to_vec()),
        ("string", b"\"x\"".to_vec()),
        ("null", b"null".to_vec()),
        ("empty", Vec::new()),
        ("whitespace only", b"  ".to_vec()),
        (
            "invalid utf8 in an ignored string",
            b"{\"x\":\"\xff\",\"cwd\":\"/c\"}".to_vec(),
        ),
        (
            "huge exponent in an ignored field",
            br#"{"x":1e999,"cwd":"/c"}"#.to_vec(),
        ),
        (
            "tool_input is a scalar",
            br#"{"tool_name":"Bash","tool_input":5}"#.to_vec(),
        ),
        (
            "tool_input null",
            br#"{"tool_name":"Bash","tool_input":null}"#.to_vec(),
        ),
        (
            "float keys order",
            br#"{"tool_name":"X","tool_input":{"b":1.50,"a":[1e2,-0.0]}}"#.to_vec(),
        ),
    ];
    // What the old path did that the new one is documented to differ on are
    // asserted separately below.
    let mut differing = Vec::new();
    for (name, body) in cases.drain(..) {
        let (old, new) = (old_path(&body), new_path(&body));
        if old.is_ok() != new.is_ok() || (old.is_ok() && old != new) {
            differing.push(format!(
                "{name}: old ok={} new ok={}",
                old.is_ok(),
                new.is_ok()
            ));
        }
    }
    assert!(differing.is_empty(), "{differing:#?}");
}

/// Unicode whitespace around the document: `str::trim` strips it, the
/// serde slice walk does not, so a payload the old path read is refused.
#[test]
fn unicode_whitespace_around_the_document_reads_as_it_did() {
    for pad in ["\u{a0}", "\u{2028}", "\u{3000}", "\u{feff}"] {
        let body = format!("{pad}{{\"cwd\":\"/c\"}}{pad}").into_bytes();
        let (old, new) = (old_path(&body), new_path(&body));
        assert_eq!(
            old.is_ok(),
            new.is_ok(),
            "pad {pad:?}: old={old:?} new={new:?}"
        );
    }
}

/// Text after the object: documented as ignored by the new walk, an error
/// before. A payload with trailing garbage used to report nothing.
#[test]
fn trailing_garbage_after_the_object_is_still_refused() {
    let body = br#"{"session_id":"a"} trailing"#;
    assert!(old_path(body).is_err());
    assert!(new_path(body).is_err(), "now reads: {:?}", new_path(body));
}

#[test]
fn nesting_is_limited_the_same_way_and_never_overflows_the_stack() {
    // `deep` is skipped, `tool_input` is kept: built into a value and walked
    // recursively by the digest.
    let mut differing = Vec::new();
    for (field, depths) in [
        (
            "deep",
            vec![50usize, 120, 126, 127, 128, 129, 200, 5_000, 2_000_000],
        ),
        ("tool_input", vec![100usize, 126, 127, 128, 129, 5_000]),
    ] {
        for depth in depths {
            let mut body = format!(r#"{{"tool_name":"Bash","{field}":"#).into_bytes();
            body.extend(std::iter::repeat_n(b'[', depth));
            body.extend(std::iter::repeat_n(b']', depth));
            body.push(b'}');
            let (old, new) = (old_path_unbounded(&body), new_path(&body));
            if old.is_ok() != new.is_ok() {
                differing.push(format!(
                    "{field} depth {depth}: old ok={} new ok={}",
                    old.is_ok(),
                    new.is_ok()
                ));
            }
        }
    }
    assert!(differing.is_empty(), "{differing:#?}");
}

/// `parse_input` without its 4 MiB refusal, which is the one intended change.
fn old_path_unbounded(body: &[u8]) -> Result<HookInput, String> {
    let text = std::str::from_utf8(body).map_err(|e| e.to_string())?;
    let value: Value = serde_json::from_str(text.trim()).map_err(|e| e.to_string())?;
    if !value.is_object() {
        return Err("not an object".into());
    }
    Ok(HookInput::from_value(&value))
}

/// A corpus of real-shaped payloads (each Claude Code event), with unknown
/// fields, large responses and unicode, run through both paths.
#[test]
fn the_new_reader_equals_from_value_over_a_corpus_of_real_shaped_payloads() {
    let long = "é漢🙂 line one\nline two \"quoted\" \\ back".repeat(50);
    let mut corpus: Vec<Value> = Vec::new();
    for tool in [
        "Bash",
        "Edit",
        "MultiEdit",
        "Write",
        "NotebookEdit",
        "Read",
        "Task",
        "WebFetch",
        "mcp__x__y",
        "",
    ] {
        for input in [
            json!({"command": "cargo test\nsecond", "description": long}),
            json!({"file_path": "/a/b.rs", "old_string": "a", "new_string": long}),
            json!({"notebook_path": "/n.ipynb", "edits": [{"old_string": "x"}, {"old_string": 3}]}),
            json!({"url": "https://x", "prompt": "p", "unicode_key_é": [null, true, 1.5e-3]}),
            json!(null),
            json!("scalar"),
            json!({}),
        ] {
            for event in [
                "PreToolUse",
                "PostToolUse",
                "PostToolUseFailure",
                "PermissionRequest",
            ] {
                corpus.push(json!({
                    "session_id": "sess-1", "transcript_path": "/t.jsonl", "cwd": "/w",
                    "permission_mode": "default", "hook_event_name": event,
                    "tool_name": tool, "tool_input": input, "tool_use_id": "toolu_9",
                    "agent_id": "a1", "tool_response": {"stdout": long, "n": [1, 2, {"k": null}]},
                    "unknown": {"deeper": [[[1]]]}
                }));
            }
        }
    }
    for extra in [
        json!({"hook_event_name": "Stop", "session_id": "s", "last_assistant_message": long, "stop_hook_active": false}),
        json!({"hook_event_name": "StopFailure", "error": "rate_limit", "session_id": "s"}),
        json!({"hook_event_name": "UserPromptSubmit", "prompt": long, "session_id": "s", "cwd": "/w"}),
        json!({"hook_event_name": "SessionEnd", "reason": "logout", "session_id": "s"}),
        json!({"hook_event_name": "Notification", "message": long, "notification_type": "permission_prompt"}),
        json!({"hook_event_name": "SessionStart", "source": "resume", "workspace_roots": ["", "/r1", "/r2"]}),
        json!({"cursor_version": "1.0", "workspaceRoots": ["/r"]}),
    ] {
        corpus.push(extra);
    }
    assert!(corpus.len() > 250);
    for (index, value) in corpus.iter().enumerate() {
        for text in [
            value.to_string(),
            serde_json::to_string_pretty(value).unwrap(),
        ] {
            let (old, new) = (old_path(text.as_bytes()), new_path(text.as_bytes()));
            assert_eq!(old, new, "payload {index}: {text:.200}");
        }
    }
}

/// The digest is FNV over `tool_name`, NUL and the canonical input. A tool
/// name containing the separator and part of an input can alias another call.
#[test]
fn a_digest_does_not_alias_a_tool_name_that_contains_the_separator() {
    let call = |name: &str, input: Option<Value>| {
        let mut value = json!({"tool_name": name});
        if let Some(input) = input {
            value["tool_input"] = input;
        }
        HookInput::from_value(&value).tool_subject
    };
    let honest = call("a", Some(json!({"x": 1})));
    let aliased = call("a\0{\"x\":1}", None);
    assert_ne!(honest, aliased);
}

/// Different inputs of the same tool must not collide: sample 200k distinct
/// small calls and require 200k distinct digests (a 64-bit hash, so any
/// collision here is a structural flaw in the canonical form).
#[test]
fn distinct_tool_inputs_get_distinct_digests() {
    let mut seen = std::collections::HashMap::new();
    for n in 0..200_000u32 {
        let input = match n % 4 {
            0 => json!({"command": format!("echo {n}")}),
            1 => json!({"file_path": format!("/f/{n}"), "old_string": "a"}),
            2 => json!({"command": "x", "n": n}),
            _ => json!({"a": [n, n + 1], "b": {"c": n.to_string()}}),
        };
        let value = json!({"tool_name": "T", "tool_input": input});
        let digest = HookInput::from_value(&value).tool_subject;
        let key = value.to_string();
        if let Some(previous) = seen.insert(digest.clone(), key.clone()) {
            panic!("digest {digest} for {previous} and {key}");
        }
    }
}

/// Ints and floats that read the same must digest differently from distinct
/// values, and key order must not matter. (1 vs 1.0 are different tool calls
/// to a tool that checks the type; if they digest the same a request is
/// cleared early.)
#[test]
fn digest_separates_one_from_one_point_zero_and_ignores_key_order() {
    let d = |input: Value| {
        HookInput::from_value(&json!({"tool_name": "T", "tool_input": input})).tool_subject
    };
    assert_ne!(d(json!({"n": 1})), d(json!({"n": 1.0})));
    assert_eq!(
        d(serde_json::from_str(r#"{"a":1,"b":2}"#).unwrap()),
        d(serde_json::from_str(r#"{"b":2,"a":1}"#).unwrap())
    );
}

/// Cost: a 5 MiB `tool_input` (a Write of a big file) through the new reader.
/// Bounded in work, not in wall time: the whole reader (the read and the parse,
/// on the worker thread `within_budget` runs them on) is timed as the CPU that
/// worker spent, which a loaded host barely stretches, where its wall clock has
/// been stretched past the budget by tens of times. A CPU spin anywhere in the
/// reader, as well as a slow parse, fails it.
#[test]
fn a_five_mib_tool_input_costs_well_under_the_hook_timeout() {
    let body = json!({
        "session_id": "s", "tool_name": "Write", "hook_event_name": "PostToolUse",
        "tool_input": {"file_path": "/f", "content": "x".repeat(5 * 1024 * 1024)},
        "tool_response": {"ok": true},
    })
    .to_string();
    if crate::sysload::try_thread_cpu().is_none() {
        // No per-thread CPU clock on this platform (there is one on macOS and
        // Linux): the work of the reader is not measurable, and this says so
        // instead of passing.
        eprintln!("5 MiB tool_input: skipped, no thread CPU clock on this platform");
        return;
    }
    let mut best = Duration::MAX;
    for _ in 0..5 {
        // Cleared first, so a reading that never arrives (the reader no longer
        // runs on `within_budget`'s worker, or the seam stopped recording)
        // cannot be mistaken for the previous round's.
        super::LAST_WORKER_CPU.set(None);
        let input = new_path(body.as_bytes()).unwrap();
        assert_eq!(input.tool_subject.len(), 16);
        let cpu = super::LAST_WORKER_CPU.get().expect(
            "the reader reported no worker CPU although this platform has a thread CPU clock",
        );
        best = best.min(cpu);
    }
    eprintln!("5 MiB tool_input: min of 5 = {best:?} of worker CPU");
    assert!(best < Duration::from_millis(1_000), "{best:?}");
}
