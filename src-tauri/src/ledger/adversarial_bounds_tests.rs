//! Adversarial probes of `LedgerEvent::bounded`, `bound_verdict_json` and
//! `bound_evidence_json` (task ft-541eafaf). A passing test is a regression
//! pin; an `#[ignore = "break: ..."]` test demonstrates a defect and is meant
//! to fail until the implementation is fixed.

use super::*;

fn event() -> LedgerEvent {
    LedgerEvent {
        id: 7,
        ulid: "01J00000000000000000000000".into(),
        ts_utc: "2026-10-14T00:00:00.000Z".into(),
        schema_version: 1,
        repo_path: "/r".into(),
        worktree_path: None,
        actor_kind: "agent".into(),
        actor_id: None,
        session_id: None,
        task_id: None,
        action: "session.command".into(),
        object: None,
        argv_json: None,
        outcome: "ok".into(),
        verdict_json: None,
        before_ref: None,
        after_ref: None,
        duration_ms: None,
        detail_json: None,
    }
}

const GLYPHS: [&str; 4] = ["a", "é", "€", "😀"];

/// `len` bytes of `glyph`s padded with ASCII so the text is exactly `len`
/// bytes when possible; the multi-byte glyph starts at `lead` bytes in.
fn text_of(len: usize, lead: usize, glyph: &str) -> String {
    let mut text = "a".repeat(lead.min(len));
    while text.len() + glyph.len() <= len {
        text.push_str(glyph);
    }
    while text.len() < len {
        text.push('b');
    }
    assert_eq!(text.len(), len);
    text
}

/// Every string field, set through `set`, read back through `get`.
type Setter = fn(&mut LedgerEvent, String);
type Getter = fn(&LedgerEvent) -> String;

fn fields() -> Vec<(&'static str, usize, Setter, Getter)> {
    vec![
        ("ulid", 256, |e, t| e.ulid = t, |e| e.ulid.clone()),
        ("ts_utc", 256, |e, t| e.ts_utc = t, |e| e.ts_utc.clone()),
        (
            "repo_path",
            4096,
            |e, t| e.repo_path = t,
            |e| e.repo_path.clone(),
        ),
        (
            "worktree_path",
            4096,
            |e, t| e.worktree_path = Some(t),
            |e| e.worktree_path.clone().unwrap(),
        ),
        (
            "actor_kind",
            256,
            |e, t| e.actor_kind = t,
            |e| e.actor_kind.clone(),
        ),
        (
            "actor_id",
            256,
            |e, t| e.actor_id = Some(t),
            |e| e.actor_id.clone().unwrap(),
        ),
        (
            "session_id",
            256,
            |e, t| e.session_id = Some(t),
            |e| e.session_id.clone().unwrap(),
        ),
        (
            "task_id",
            256,
            |e, t| e.task_id = Some(t),
            |e| e.task_id.clone().unwrap(),
        ),
        ("action", 256, |e, t| e.action = t, |e| e.action.clone()),
        ("outcome", 256, |e, t| e.outcome = t, |e| e.outcome.clone()),
        (
            "before_ref",
            1024,
            |e, t| e.before_ref = Some(t),
            |e| e.before_ref.clone().unwrap(),
        ),
        (
            "after_ref",
            1024,
            |e, t| e.after_ref = Some(t),
            |e| e.after_ref.clone().unwrap(),
        ),
    ]
}

#[test]
fn every_field_every_glyph_every_straddle_stays_within_its_bound_and_is_valid() {
    for (name, bound, set, get) in fields() {
        for glyph in GLYPHS {
            // Straddle: the glyph starts 0..=3 bytes before the cut point of
            // the suffix budget, and the total is bound-1, bound, bound+1.
            for lead in 0..8 {
                for total in [bound - 1, bound, bound + 1, bound + 5, bound * 3] {
                    let original = text_of(total, bound - 14 - lead, glyph);
                    let mut row = event();
                    set(&mut row, original.clone());
                    let got = get(&row.bounded());
                    assert!(
                        got.len() <= bound,
                        "{name} {glyph:?} lead {lead} total {total}: {} bytes",
                        got.len()
                    );
                    if total <= bound {
                        assert_eq!(got, original, "{name}: within the bound, cut anyway");
                    } else {
                        assert!(got.ends_with("… [truncated]"), "{name}: no suffix");
                        let head = got.trim_end_matches("… [truncated]");
                        assert!(original.starts_with(head), "{name}: not a prefix");
                    }
                }
            }
        }
    }
}

#[test]
fn an_empty_string_field_stays_empty() {
    let mut row = event();
    row.repo_path = String::new();
    row.action = String::new();
    row.actor_id = Some(String::new());
    row.after_ref = Some(String::new());
    assert_eq!(row.clone().bounded(), row);
}

#[test]
fn an_already_suffixed_field_is_not_double_cut() {
    // Within the bound: untouched, suffix and all.
    let mut row = event();
    row.actor_id = Some(format!("{}… [truncated]", "i".repeat(200)));
    assert_eq!(row.clone().bounded(), row);
    // Over the bound and already carrying the suffix: one suffix at the end,
    // exactly the bound or less, and the kept head holds no second suffix when
    // the first sat at the very end of the over-long text.
    let mut row = event();
    row.actor_id = Some(format!("{}… [truncated]", "i".repeat(300)));
    let got = row.bounded().actor_id.unwrap();
    assert!(got.len() <= 256);
    assert_eq!(got.matches("… [truncated]").count(), 1, "{got}");
}

#[test]
fn bounding_is_idempotent_for_every_field() {
    for (name, bound, set, get) in fields() {
        for glyph in GLYPHS {
            let mut row = event();
            set(&mut row, text_of(bound * 2, 0, glyph));
            let once = row.bounded();
            let twice = once.clone().bounded();
            assert_eq!(get(&once), get(&twice), "{name} {glyph:?}");
        }
    }
}

/// Number arrays are cut with no marker. A reader of `data_json` cannot tell
/// the cut array from a complete one of that many elements. This pins the
/// silent behaviour.
#[test]
fn a_cut_number_evidence_array_is_indistinguishable_from_a_complete_one() {
    let numbers: Vec<u32> = (0..20_000).collect();
    let text = serde_json::to_string(&numbers).unwrap();
    let cut = bound_evidence_json(&text, ROW_FIELD_CAP_BYTES).0;
    let kept: Vec<u32> = serde_json::from_str(&cut).expect("a plain array of numbers");
    assert!(kept.len() < numbers.len());
    assert_eq!(kept[..], numbers[..kept.len()]);
    // Any complete array of `kept.len()` numbers serializes identically.
    assert_eq!(cut, serde_json::to_string(&kept).unwrap());
}

#[test]
fn a_cut_number_evidence_array_says_it_was_cut() {
    let numbers: Vec<u32> = (0..20_000).collect();
    let text = serde_json::to_string(&numbers).unwrap();
    // Changed with the root fix: an array carries no marker element, so the
    // cut is recorded in the row's sibling `data_json_truncated` instead.
    let (cut, record) = bound_evidence_json(&text, ROW_FIELD_CAP_BYTES);
    let kept: Vec<u32> = serde_json::from_str(&cut).unwrap();
    let record = record.expect("a capped sample reads as complete coverage");
    assert_eq!(record.kept, kept.len());
    assert_eq!(record.total, 20_000);
    assert_eq!(record.bytes, text.len());
}

#[test]
fn an_evidence_array_whose_first_element_exceeds_the_cap_is_not_silently_emptied() {
    for first in [
        json!({ "stderr": "x".repeat(40_000) }),
        json!("y".repeat(40_000)),
        json!(["z".repeat(40_000)]),
    ] {
        let text = serde_json::to_string(&json!([first, { "small": 1 }])).unwrap();
        let (cut, record) = bound_evidence_json(&text, ROW_FIELD_CAP_BYTES);
        let value: Value = serde_json::from_str(&cut).unwrap();
        assert!(
            !value.as_array().unwrap().is_empty(),
            "a 2-element array became {cut}"
        );
        assert!(cut.len() <= ROW_FIELD_CAP_BYTES);
        let record = record.expect("the cut is recorded");
        assert_eq!(record.total, 2);
        assert_eq!(record.kept, value.as_array().unwrap().len());
    }
}

#[test]
fn an_oversized_single_element_array_characterization() {
    let text = serde_json::to_string(&json!([{ "stderr": "x".repeat(40_000) }])).unwrap();
    let cut = bound_evidence_json(&text, ROW_FIELD_CAP_BYTES);
    // Changed with the fix: the element is shortened in place, not dropped,
    // and the row records the cut.
    let value: Value = serde_json::from_str(&cut.0).unwrap();
    assert_eq!(value.as_array().unwrap().len(), 1);
    assert!(value[0]["stderr"].as_str().unwrap().contains("truncated"));
    assert!(cut.0.len() <= ROW_FIELD_CAP_BYTES);
    let record = cut.1.expect("recorded");
    assert_eq!((record.kept, record.total), (1, 1));
}

#[test]
fn the_object_marker_satisfies_the_element_schema() {
    // An element schema that requires `exit`, as a typed evidence row would.
    let text = serde_json::to_string(
        &(0..3000)
            .map(|n| json!({ "exit": n, "stderr": "boom" }))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let cut = bound_evidence_json(&text, ROW_FIELD_CAP_BYTES).0;
    let value: Value = serde_json::from_str(&cut).unwrap();
    for item in value.as_array().unwrap() {
        assert!(item.get("exit").is_some(), "element without `exit`: {item}");
    }
}

#[test]
fn a_mixed_evidence_array_gets_no_marker_element_of_any_kind() {
    // Changed with the root fix: no element is added to an array, whatever it
    // holds, so the cut leaves a prefix of the original and a sibling record.
    for items in [
        {
            let mut items: Vec<Value> = (0..400)
                .map(|n| json!({ "n": n, "pad": "p".repeat(30) }))
                .collect();
            items.extend((0..4000).map(|n| json!(n)));
            items
        },
        {
            let mut items: Vec<Value> = (0..30).map(|n| json!({ "n": n })).collect();
            items.extend((0..4000).map(|n| json!(n)));
            items
        },
    ] {
        let (cut, record) =
            bound_evidence_json(&serde_json::to_string(&items).unwrap(), ROW_FIELD_CAP_BYTES);
        let value: Value = serde_json::from_str(&cut).unwrap();
        let kept = value.as_array().unwrap();
        assert_eq!(
            kept[..],
            items[..kept.len()],
            "an element was added or changed"
        );
        let record = record.expect("recorded");
        assert_eq!((record.kept, record.total), (kept.len(), items.len()));
    }
}

#[test]
fn a_big_object_evidence_with_a_nested_array_loses_the_member_not_the_shape() {
    let text = json!({ "keep": 1, "rows": (0..5000).collect::<Vec<_>>() }).to_string();
    let cut = bound_evidence_json(&text, ROW_FIELD_CAP_BYTES).0;
    let value: Value = serde_json::from_str(&cut).unwrap();
    assert!(cut.len() <= ROW_FIELD_CAP_BYTES);
    assert!(value.get("rows").is_none());
    assert!(value["gitpulse_truncated"]["keys"]
        .as_array()
        .unwrap()
        .iter()
        .any(|k| k == "rows"));
}

#[test]
fn a_nested_array_inside_an_object_element_is_cut_in_place() {
    // Changed with the fix: the inner array is trimmed where it stands, so
    // neither element is dropped as a group.
    let text = serde_json::to_string(&json!([
        { "id": 1 },
        { "id": 2, "rows": (0..6000).collect::<Vec<_>>() },
    ]))
    .unwrap();
    let cut = bound_evidence_json(&text, ROW_FIELD_CAP_BYTES).0;
    let value: Value = serde_json::from_str(&cut).unwrap();
    let items = value.as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0], json!({ "id": 1 }));
    let rows = items[1]["rows"].as_array().expect("still an array");
    assert!(!rows.is_empty() && rows.len() < 6000);
    assert!(cut.len() <= ROW_FIELD_CAP_BYTES);
}

#[test]
fn evidence_edge_inputs_keep_their_kind() {
    assert_eq!(bound_evidence_json("[]", 16).0, "[]");
    let long = serde_json::to_string(&"s".repeat(40_000)).unwrap();
    let cut = bound_evidence_json(&long, ROW_FIELD_CAP_BYTES).0;
    assert!(serde_json::from_str::<Value>(&cut).unwrap().is_string());
    assert!(cut.len() <= ROW_FIELD_CAP_BYTES);
    // Invalid JSON becomes a JSON object with a head (the one kind change).
    let cut = bound_evidence_json(&"{ not json ".repeat(5000), ROW_FIELD_CAP_BYTES).0;
    let value: Value = serde_json::from_str(&cut).unwrap();
    assert!(value.get("gitpulse_truncated").is_some());
    // Exactly the cap and one past it, for a string array.
    let at: String = serde_json::to_string(&vec!["a"; 5461]).unwrap();
    assert_eq!(bound_evidence_json(&at, at.len()).0, at);
    let cut = bound_evidence_json(&at, at.len() - 1).0;
    assert!(cut.len() < at.len());
}

fn verdict_with(members: Value) -> LedgerEvent {
    let mut row = event();
    row.verdict_json = Some(members.to_string());
    row
}

#[test]
fn nested_verdict_arrays_inside_objects_inside_arrays_are_cut_in_place() {
    let verdict = json!({
        "status": "blocked",
        "groups": [
            { "name": "a/b~c", "inner": (0..3000).map(|n| format!("item-{n}")).collect::<Vec<_>>() },
            { "name": "second", "inner": (0..3000).map(|n| format!("item-{n}")).collect::<Vec<_>>() },
        ],
        "a/b~c": (0..3000).map(|n| n.to_string()).collect::<Vec<_>>(),
    });
    let (cut, record) = verdict_with(verdict.clone()).bounded_recorded();
    let text = cut.verdict_json.unwrap();
    assert!(text.len() <= ROW_FIELD_CAP_BYTES, "{}", text.len());
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        value["groups"].as_array().unwrap().len(),
        2,
        "wrapper lost an element"
    );
    for group in value["groups"].as_array().unwrap() {
        assert!(group["inner"].is_array());
    }
    assert!(value["a/b~c"].is_array());
    // Changed with the root fix: the record is the row's `verdict_truncated`.
    assert!(record.expect("recorded").elements_dropped > 0);
    // Cut in place and shared: both inner arrays lost a tail.
    let first = value["groups"][0]["inner"].as_array().unwrap().len();
    let second = value["groups"][1]["inner"].as_array().unwrap().len();
    assert!(first < 3000 && second < 3000, "{first} {second}");
}

#[test]
fn a_verdict_array_with_one_huge_element_is_emptied_and_recorded() {
    // Strings are cut to 512 first, so one array element cannot stay huge; an
    // array of nested arrays can. The record must exist.
    let verdict = json!({ "status": "blocked", "degraded": [ (0..8000).collect::<Vec<_>>() ] });
    let (cut, record) = verdict_with(verdict).bounded_recorded();
    assert!(cut.verdict_json.unwrap().len() <= ROW_FIELD_CAP_BYTES);
    assert!(record.expect("recorded").elements_dropped > 0);
    assert_eq!(cut.detail_json, None);
}

#[test]
fn a_verdict_made_large_by_many_small_members_is_still_bounded() {
    let members: serde_json::Map<String, Value> =
        (0..20_000).map(|n| (format!("k{n}"), json!(n))).collect();
    let (cut, record) = verdict_with(Value::Object(members)).bounded_recorded();
    let text = cut.verdict_json.unwrap();
    assert!(
        text.len() <= ROW_FIELD_CAP_BYTES,
        "verdict is {} bytes",
        text.len()
    );
    assert!(serde_json::from_str::<Value>(&text).unwrap().is_object());
    assert!(record.expect("recorded").members_dropped > 0);
}

#[test]
fn a_verdict_cut_is_recorded_whatever_the_detail_json_is() {
    for detail in ["[1,2,3]", "not json", "\"a string\"", "null", "42"] {
        let mut row = verdict_with(json!({
            "status": "blocked",
            "degraded": (0..6000).map(|n| format!("check-{n}")).collect::<Vec<_>>(),
        }));
        row.detail_json = Some(detail.to_string());
        let (cut, record) = row.bounded_recorded();
        // Changed with the root fix: the record is the row's own field, so no
        // `detail_json` shape can lose it, and the detail is left as stored.
        assert!(
            record.is_some_and(|record| record.elements_dropped > 0),
            "detail {detail:?}: the cut verdict array left no trace in the row"
        );
        assert_eq!(cut.detail_json.as_deref(), Some(detail));
    }
}

#[test]
fn a_verdict_cut_leaves_an_absent_detail_absent() {
    // Reverted with the root fix: no null-to-string change of `detail_json`.
    let (cut, record) = verdict_with(json!({
        "status": "blocked",
        "degraded": (0..6000).map(|n| format!("check-{n}")).collect::<Vec<_>>(),
    }))
    .bounded_recorded();
    assert_eq!(cut.detail_json, None);
    assert!(record.is_some());
}

#[test]
fn a_forged_verdict_truncated_in_the_stored_detail_is_neither_the_signal_nor_touched() {
    let forged = json!({ "verdict_truncated": "forged", "phase": "gate" }).to_string();
    let mut row = verdict_with(json!({
        "status": "blocked",
        "degraded": (0..6000).map(|n| format!("check-{n}")).collect::<Vec<_>>(),
    }));
    row.detail_json = Some(forged.clone());
    let (cut, record) = row.bounded_recorded();
    assert_eq!(cut.detail_json, Some(forged));
    assert!(record.is_some());
}

#[test]
fn an_uncut_verdict_has_no_record_whatever_its_stored_detail_says() {
    // Changed with the root fix: the signal is the returned record, not a
    // member of the stored detail, so a forged one claims nothing.
    let stored = json!({ "verdict_truncated": { "elements_dropped": 9 } }).to_string();
    let mut row = verdict_with(json!({ "status": "allowed" }));
    row.detail_json = Some(stored.clone());
    let (cut, record) = row.bounded_recorded();
    assert_eq!(record, None);
    assert_eq!(cut.detail_json, Some(stored));
}
