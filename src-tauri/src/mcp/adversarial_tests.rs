//! Adversarial probes of the response byte budget (card 10). Each test states
//! the invariant it attacks; a failing one is a demonstrated defect.

use super::budget::{self, TRUNCATION_KEY};
use super::*;

fn rows(count: usize, size: usize) -> Vec<Value> {
    (0..count)
        .map(|n| json!({ "item_id": format!("t-{n}"), "blob": "r".repeat(size) }))
        .collect()
}

fn wire_bytes(id: Value, result: Value) -> usize {
    serde_json::to_string(&ok(id, result))
        .expect("serializes")
        .len()
        + 1
}

/// `gitpulse_list_tasks` pages by an opaque offset cursor and says so with
/// `has_more`. Cutting its `tasks` for the byte budget must not leave a cursor
/// that points past the rows that were just dropped.
#[test]
fn a_cut_task_page_does_not_keep_a_cursor_that_skips_the_dropped_rows() {
    let payload = json!({
        "ok": true, "repository": null, "returned": 50, "total": 200,
        "has_more": true, "next_cursor": "opaque-offset-50",
        "tasks": rows(50, 2000),
    });
    let structured = budget::fit(true, payload, 30_000)
        .unwrap()
        .structured
        .unwrap();
    let kept = structured["tasks"].as_array().unwrap().len();
    assert!(kept < 50, "fixture was not cut");
    assert_ne!(
        structured["next_cursor"],
        json!("opaque-offset-50"),
        "tasks were cut to {kept} rows but next_cursor still resumes at row 50: rows {kept}..50 are skipped"
    );
}

/// On the last page `has_more` is false. After a cut it must say more follow
/// (the dropped rows), or a reader that trusts `has_more` stops early.
#[test]
fn a_cut_last_task_page_still_reports_has_more() {
    let payload = json!({
        "ok": true, "repository": null, "returned": 50, "total": 50,
        "has_more": false, "next_cursor": null,
        "tasks": rows(50, 2000),
    });
    let structured = budget::fit(true, payload, 30_000)
        .unwrap()
        .structured
        .unwrap();
    assert!(structured["tasks"].as_array().unwrap().len() < 50);
    assert_eq!(
        structured["has_more"], true,
        "rows were dropped but has_more is false: {structured:.200}"
    );
}

/// A payload made of many modest lists is over the budget but every list can
/// be cut. Truncation must produce a result, not an error, however many lists
/// there are.
#[test]
fn many_two_element_lists_are_truncated_not_refused() {
    let mut flat = serde_json::Map::new();
    for n in 0..400 {
        flat.insert(
            format!("k{n}"),
            json!(["f".repeat(20_000), "g".repeat(20_000)]),
        );
    }
    let payload = Value::Object(flat);
    let room = budget::tool_budget(&json!(1));
    match budget::fit(true, payload, room) {
        Ok(fitted) => assert!(fitted.structured.unwrap()[TRUNCATION_KEY].is_object()),
        Err(message) => panic!("a payload of 400 cuttable lists was refused: {message:.200}"),
    }
}

/// The whole JSON-RPC line, trailing newline included, for payloads sized
/// around the budget with escape-heavy content, in both eras and with a long
/// request id: never past the frame.
#[test]
fn a_tool_response_never_exceeds_the_frame_around_the_boundary() {
    let frame = MAX_FRAME_BYTES;
    let mut worst = 0usize;
    for id in [json!(1), json!("i".repeat(4000))] {
        for modern in [true, false] {
            for element in ["\"", "x", "\u{1}", "é"] {
                let room = budget::tool_budget(&id);
                let per_unit = match element {
                    "\"" | "é" => 2,
                    "\u{1}" => 6,
                    _ => 1,
                };
                let center = room / (40 * per_unit);
                for delta in (0..400).step_by(41).map(|d| d as isize - 200) {
                    let len = (center as isize + delta * 3).max(1) as usize;
                    let payload = json!({
                        "ok": true,
                        "items": (0..40).map(|_| element.repeat(len)).collect::<Vec<_>>(),
                    });
                    let result = complete_tool_result(modern, payload, room);
                    let bytes = wire_bytes(id.clone(), result);
                    worst = worst.max(bytes);
                    assert!(
                        bytes <= frame,
                        "modern={modern} element={element:?} len={len} id_len={}: {bytes} > {frame}",
                        id.to_string().len()
                    );
                }
            }
        }
    }
    assert!(
        worst > frame / 2,
        "the sweep never came near the frame: {worst}"
    );
}

/// `fit` must hold `budget` on the wire at every byte around the exact size of
/// an uncut result: cap-1, cap and cap+1.
#[test]
fn fit_holds_its_budget_at_every_byte_around_the_exact_size() {
    let payload = json!({ "ok": true, "returned": 30, "events": rows(30, 700) });
    let compact = serde_json::to_string(&payload).unwrap();
    let uncut = budget::escaped_len(&compact) + compact.len();
    for modern in [true, false] {
        for budget_bytes in uncut.saturating_sub(40)..uncut + 40 {
            match budget::fit(modern, payload.clone(), budget_bytes) {
                Ok(fitted) => {
                    let wire = budget::escaped_len(&fitted.text)
                        + fitted
                            .structured
                            .as_ref()
                            .map_or(0, |value| serde_json::to_string(value).unwrap().len());
                    assert!(
                        wire <= budget_bytes,
                        "modern={modern} budget={budget_bytes}: {wire} on the wire"
                    );
                }
                Err(message) => assert!(message.contains("budget"), "{message}"),
            }
        }
    }
}

/// A cut payload is still JSON that parses back to an object with every key
/// the full payload had, in both text and structured form.
#[test]
fn a_cut_payload_keeps_every_top_level_key_in_text_and_structure() {
    let payload = json!({
        "a": rows(200, 900), "b": { "c": [1, 2, 3] }, "d": "kept", "e": null,
        "f": rows(5, 10), "truncated": false,
    });
    let keys: Vec<String> = payload.as_object().unwrap().keys().cloned().collect();
    let fitted = budget::fit(true, payload, 20_000).unwrap();
    let mirror: Value = serde_json::from_str(&fitted.text).expect("text is JSON");
    let structured = fitted.structured.unwrap();
    for key in &keys {
        assert!(mirror.get(key).is_some(), "text lost {key}");
        assert!(
            structured.get(key).is_some(),
            "structuredContent lost {key}"
        );
    }
}
