//! Byte budgets for what one MCP response may carry.
//!
//! The transport writes one JSON-RPC message per line and refuses to write one
//! larger than [`MAX_FRAME_BYTES`], which it can only answer with an error. A
//! tool or resource result that is merely *large* should not cost its caller
//! the answer, so the owners of the large things (tool results here, resource
//! documents in [`super::resources`], ledger pages in [`crate::ledger`]) stay
//! under a budget derived from that one constant, and say when they cut.
//!
//! A tool result is shortened by cutting lists, never by dropping keys: every
//! facet of a payload survives, a list keeps a prefix of its elements, and the
//! payload says what was cut ([`TRUNCATION_KEY`]) and flips the `truncated`
//! flags it already carries, so a reader that checks only those still learns
//! the result is partial.

use std::io;

use serde_json::{json, Map, Value};

use super::MAX_FRAME_BYTES;

/// Room the response around a tool result takes: `jsonrpc`, `resultType`,
/// `_meta`, the content block and `isError`. The id is measured separately.
pub(super) const ENVELOPE_RESERVE_BYTES: usize = 512;

/// Largest the compact text block of a modern result may be. A modern client
/// reads `structuredContent`; the text exists for clients that read nothing
/// else, and carrying both full-size doubled every large result on the wire.
pub(super) const TEXT_MIRROR_BYTES: usize = 256 * 1024;

/// The top-level key a cut payload gains, describing the cut.
pub(super) const TRUNCATION_KEY: &str = "result_truncation";

/// Rounds of cutting before a payload is given up on. Each round removes at
/// least the overshoot, so a payload that is still too big after this many has
/// nothing left that cutting a list can shorten.
const MAX_ROUNDS: usize = 64;

/// Bytes each round cuts beyond the overshoot, for the marker it adds and the
/// counts it rewrites.
const MARKER_SLACK_BYTES: usize = 512;

/// Cut lists the marker describes one by one, and the bytes each entry takes.
/// The rest are counted in `arrays_total`.
const MARKER_ENTRIES: usize = 64;
const MARKER_ENTRY_BYTES: usize = 96;

/// What a response with this request id may spend on one tool result.
pub(super) fn tool_budget(id: &Value) -> usize {
    let id_bytes = serde_json::to_string(id).map_or(0, |text| text.len());
    MAX_FRAME_BYTES.saturating_sub(id_bytes + ENVELOPE_RESERVE_BYTES)
}

/// Bytes `text` takes as a JSON string, quotes included.
///
/// The frame is measured on the wire, where a quote, a backslash or a control
/// character grows two to six times. A budget on raw length is not a budget.
pub(super) fn escaped_len(text: &str) -> usize {
    2 + text.chars().map(escape_cost).sum::<usize>()
}

/// Bytes `character` takes inside a JSON string, as `serde_json` writes it.
pub(super) fn escape_cost(character: char) -> usize {
    match character {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        c if (c as u32) < 0x20 => 6,
        c => c.len_utf8(),
    }
}

/// The longest prefix of `text`, ending on a character boundary, that takes at
/// most `budget` bytes inside a JSON string.
pub(super) fn escaped_prefix(text: &str, budget: usize) -> &str {
    let mut used = 0;
    for (index, character) in text.char_indices() {
        used += escape_cost(character);
        if used > budget {
            return &text[..index];
        }
    }
    text
}

/// What a tool result is sent as.
pub(super) struct Fitted {
    /// The text block: compact JSON.
    pub(super) text: String,
    /// `structuredContent`, present in the modern era only.
    pub(super) structured: Option<Value>,
}

/// Fit `payload` into `budget` wire bytes.
///
/// * **Legacy** (`modern` false): the text is the only copy and gets the whole
///   budget; it is shortened only as far as cutting the payload requires.
/// * **Modern**: `structuredContent` is the payload, cut to what the budget
///   leaves after the text. The text is the same compact JSON up to
///   [`TEXT_MIRROR_BYTES`] (never more than half the budget); past that it is the
///   compact form of the payload cut to that size, still JSON of the same shape.
///
/// `Err` is the message of a tool execution error: the payload is too large
/// and holds no list left to cut.
pub(super) fn fit(modern: bool, payload: Value, budget: usize) -> Result<Fitted, String> {
    let compact = serde_json::to_string(&payload)
        .map_err(|error| format!("result could not be serialized: {error}"))?;
    if !modern {
        if escaped_len(&compact) <= budget {
            return Ok(Fitted {
                text: compact,
                structured: None,
            });
        }
        let original = compact.len();
        drop(compact);
        let cut = truncate(payload, original, budget, escaped_compact_len)
            .ok_or_else(|| too_large(original, budget))?;
        return Ok(Fitted {
            text: to_compact(&cut)?,
            structured: None,
        });
    }

    let original = compact.len();
    // The mirror never takes more than half the budget, so the copy a modern
    // client reads always has at least the other half to itself.
    let mirror = TEXT_MIRROR_BYTES.min(budget / 2);
    let text = if escaped_len(&compact) <= mirror {
        compact
    } else {
        drop(compact);
        let cut = truncate(payload.clone(), original, mirror, escaped_compact_len)
            .ok_or_else(|| too_large(original, budget))?;
        to_compact(&cut)?
    };
    let room = budget.saturating_sub(escaped_len(&text));
    let structured = if original <= room {
        payload
    } else {
        truncate(payload, original, room, compact_len).ok_or_else(|| too_large(original, budget))?
    };
    Ok(Fitted {
        text,
        structured: Some(structured),
    })
}

fn to_compact(value: &Value) -> Result<String, String> {
    serde_json::to_string(value).map_err(|error| format!("result could not be serialized: {error}"))
}

/// Counts what is written, keeping nothing.
struct Counter(usize);

impl io::Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// [`Counter`], counting each byte as it costs inside a JSON string.
struct EscapedCounter(usize);

impl io::Write for EscapedCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 += bytes
            .iter()
            .map(|byte| match byte {
                b'"' | b'\\' => 2,
                // Compact JSON holds no raw control byte; counted for safety.
                0x08 | 0x09 | 0x0a | 0x0c | 0x0d => 2,
                0..=0x1f => 6,
                _ => 1,
            })
            .sum::<usize>();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The compact serialized size of `value`.
fn compact_len(value: &Value) -> usize {
    let mut counter = Counter(0);
    // A `Value` always serializes; the counter cannot fail.
    let _ = serde_json::to_writer(&mut counter, value);
    counter.0
}

/// The size of `value`'s compact text as a JSON string on the wire.
fn escaped_compact_len(value: &Value) -> usize {
    let mut counter = EscapedCounter(2);
    let _ = serde_json::to_writer(&mut counter, value);
    counter.0
}

/// One list that was cut, for the `arrays` member of [`TRUNCATION_KEY`].
struct Cut {
    pointer: String,
    kept: usize,
    total: usize,
}

/// `payload` with lists cut until `measure(payload) <= limit`.
///
/// Each round takes the lists with two or more elements that are not inside
/// another such list, and cuts them largest first, each to a prefix (never
/// fewer than one element, so a list never disappears), until the round has
/// removed the overshoot. A round therefore converges however many lists the
/// payload holds: a payload of four hundred two-element lists is one round, not
/// four hundred. A list of one element is descended into rather than cut, and
/// a list inside a cut list is reached once its parent is down to one element.
/// If every list is down to one element and the payload is still over, the
/// round empties lists, largest first: a result with empty lists and a marker
/// saying so beats a refusal for a payload whose lists could still be cut.
///
/// The work is bounded: at most [`MAX_ROUNDS`] rounds, each linear in the
/// payload, and a round that finds nothing to cut ends the loop.
fn truncate(
    mut payload: Value,
    original: usize,
    limit: usize,
    measure: fn(&Value) -> usize,
) -> Option<Value> {
    let mut cuts: Vec<Cut> = Vec::new();
    for _ in 0..MAX_ROUNDS {
        let size = measure(&payload);
        if size <= limit {
            return Some(payload);
        }
        // First keep a head of every list; only when every list is down to one
        // element and the payload is still over is a list emptied, largest
        // first, because a payload whose smallest form is over the budget is
        // otherwise refused although its lists could still be cut.
        let mut cut_any = false;
        let mut need = size - limit + MARKER_SLACK_BYTES;
        for (floor, min_len) in [(1, 2), (0, 1)] {
            let lists = cuttable_lists(&payload, min_len);
            if floor == 1 {
                need += MARKER_ENTRY_BYTES * lists.len().min(MARKER_ENTRIES);
            }
            for pointer in lists {
                if need == 0 {
                    break;
                }
                let removed = cut_list(&mut payload, &pointer, need, floor, &mut cuts);
                cut_any |= removed > 0;
                need = need.saturating_sub(removed);
            }
            if need == 0 {
                break;
            }
        }
        if !cut_any {
            break;
        }
        describe_cuts(&mut payload, limit, original, &cuts);
    }
    (measure(&payload) <= limit).then_some(payload)
}

/// The message of the tool execution error for a payload that cannot be cut
/// down: what it weighs, what it may, and what the caller can do.
fn too_large(original: usize, budget: usize) -> String {
    format!(
        "tool result is {original} bytes and cannot be shortened to its {budget} byte budget: \
         it holds no list left to cut. Ask for less (a narrower argument or a smaller limit)."
    )
}

/// JSON pointers to the lists with two or more elements that are not inside
/// another such list, largest (by serialized bytes) first.
fn cuttable_lists(root: &Value, min_len: usize) -> Vec<String> {
    fn walk(value: &Value, min_len: usize, pointer: &mut String, found: &mut Vec<(usize, String)>) {
        match value {
            // Not descended into: cutting it shortens whatever is inside.
            Value::Array(items) if items.len() >= min_len => {
                found.push((compact_len(value), pointer.clone()));
            }
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    let mark = pointer.len();
                    pointer.push('/');
                    pointer.push_str(&index.to_string());
                    walk(item, min_len, pointer, found);
                    pointer.truncate(mark);
                }
            }
            Value::Object(members) => {
                for (key, item) in members {
                    let mark = pointer.len();
                    pointer.push('/');
                    pointer.push_str(&key.replace('~', "~0").replace('/', "~1"));
                    walk(item, min_len, pointer, found);
                    pointer.truncate(mark);
                }
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    walk(root, min_len, &mut String::new(), &mut found);
    found.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    found.into_iter().map(|(_, pointer)| pointer).collect()
}

/// Cut the list at `pointer` to the longest prefix that removes at least
/// `overshoot` bytes (or to one element), and bring its parent's counts and
/// flags into line. Returns the bytes removed.
fn cut_list(
    root: &mut Value,
    pointer: &str,
    overshoot: usize,
    floor: usize,
    cuts: &mut Vec<Cut>,
) -> usize {
    let Some(Value::Array(items)) = root.pointer_mut(pointer) else {
        return 0;
    };
    let before = items.len();
    let mut keep = before;
    let mut removed = 0;
    while keep > floor && removed < overshoot {
        keep -= 1;
        removed += compact_len(&items[keep]) + 1;
    }
    items.truncate(keep);
    let last_id = items
        .last()
        .and_then(|item| item.get("id"))
        .and_then(Value::as_i64);

    match cuts.iter_mut().find(|cut| cut.pointer == pointer) {
        Some(cut) => cut.kept = keep,
        None => cuts.push(Cut {
            pointer: pointer.to_string(),
            kept: keep,
            total: before,
        }),
    }

    let removed_bytes = removed;
    let (parent_pointer, key) = pointer.rsplit_once('/').unwrap_or(("", pointer));
    let key = key.replace("~1", "/").replace("~0", "~");
    let Some(Value::Object(parent)) = root.pointer_mut(parent_pointer) else {
        return removed_bytes;
    };
    if keep == before {
        return removed_bytes;
    }
    for count in ["shown", "returned"] {
        if parent.get(count).is_some_and(Value::is_number) {
            parent.insert(count.into(), json!(keep));
        }
    }
    // Flags that already say "more exists" say it now, whatever they said.
    for flag in [
        "truncated".to_string(),
        "has_more".to_string(),
        format!("{key}_truncated"),
    ] {
        if parent.get(&flag).is_some_and(Value::is_boolean) {
            parent.insert(flag, json!(true));
        }
    }
    // A cursor says where the next page starts, and the rows this cut dropped
    // are the next page's first. A numeric cursor naming the last row of the
    // page is moved back to the last row that is left. Any other cursor (the
    // task board's is opaque) cannot be moved correctly, so it is withdrawn
    // rather than kept: a cursor that skips the dropped rows is the one
    // outcome a reader cannot detect. `has_more` and `result_truncation` say
    // rows were dropped, and a smaller request gets them.
    if let Some(cursor) = parent.get_mut("next_cursor") {
        match (last_id, cursor.is_i64() || cursor.is_u64()) {
            (Some(id), true) => *cursor = json!(id),
            _ if cursor.is_null() => {}
            _ => *cursor = Value::Null,
        }
    }
    removed_bytes
}

/// Write [`TRUNCATION_KEY`] and the top-level `truncated` flag.
///
/// `arrays` lists the first [`MARKER_ENTRIES`] cut lists; `arrays_total` is how
/// many were cut, so a long list of cuts is a counted sample, not a silent one.
fn describe_cuts(payload: &mut Value, limit: usize, original: usize, cuts: &[Cut]) {
    let Value::Object(root) = payload else {
        return;
    };
    let arrays: Vec<Value> = cuts
        .iter()
        .take(MARKER_ENTRIES)
        .map(|cut| json!({ "pointer": cut.pointer, "kept": cut.kept, "total": cut.total }))
        .collect();
    root.insert(
        TRUNCATION_KEY.into(),
        json!({
            "budget_bytes": limit,
            "original_bytes": original,
            "arrays": arrays,
            "arrays_total": cuts.len(),
        }),
    );
    set_truncated(root);
}

fn set_truncated(root: &mut Map<String, Value>) {
    if root.get("truncated").is_none_or(Value::is_boolean) {
        root.insert("truncated".into(), json!(true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(count: usize, size: usize) -> Vec<Value> {
        (0..count)
            .map(|id| json!({ "id": id + 1, "blob": "r".repeat(size) }))
            .collect()
    }

    fn wire(fitted: &Fitted) -> usize {
        escaped_len(&fitted.text) + fitted.structured.as_ref().map_or(0, compact_len)
    }

    #[test]
    fn a_payload_inside_its_budget_is_untouched_compact_json() {
        let payload = json!({ "ok": true, "items": [1, 2, 3] });
        let fitted = fit(true, payload.clone(), 10_000).unwrap();
        assert_eq!(fitted.text, serde_json::to_string(&payload).unwrap());
        assert!(!fitted.text.contains(' '), "compact, not pretty");
        assert_eq!(fitted.structured, Some(payload));
    }

    #[test]
    fn escaping_is_measured_on_the_wire_not_on_the_raw_text() {
        for text in ["\"\"\"\"", "\u{1}\u{1}", "a\\b\n", "é€😀", ""] {
            assert_eq!(
                escaped_len(text),
                serde_json::to_string(text).unwrap().len(),
                "{text:?}"
            );
            // The compact text of a string value is the string with its quotes,
            // and that text is then escaped once more to travel in a text block.
            assert_eq!(
                escaped_compact_len(&json!(text)),
                escaped_len(&serde_json::to_string(text).unwrap())
            );
        }
        let text = "\"é😀\u{1}x".repeat(50);
        let prefix = escaped_prefix(&text, 101);
        assert!(escaped_len(prefix) - 2 <= 101);
        let more = &text[..prefix.len() + text[prefix.len()..].chars().next().unwrap().len_utf8()];
        assert!(escaped_len(more) - 2 > 101, "the prefix is not the longest");
    }

    #[test]
    fn the_largest_list_is_cut_and_its_counts_and_flags_follow() {
        let payload = json!({
            "ok": true,
            "returned": 100,
            "truncated": false,
            "total": 100,
            "events": rows(100, 1000),
            "small": [1, 2, 3],
        });
        let fitted = fit(true, payload, 40_000).unwrap();
        let structured = fitted.structured.unwrap();
        let kept = structured["events"].as_array().unwrap().len();
        assert!((1..100).contains(&kept));
        assert_eq!(structured["returned"], kept);
        assert_eq!(structured["truncated"], true);
        assert_eq!(structured["total"], 100, "the total is never rewritten");
        assert_eq!(structured["small"], json!([1, 2, 3]));
        let cut = &structured[TRUNCATION_KEY];
        assert_eq!(cut["arrays"][0]["pointer"], "/events");
        assert_eq!(cut["arrays"][0]["kept"], kept);
        assert_eq!(cut["arrays"][0]["total"], 100);
        assert!(cut["original_bytes"].as_u64().unwrap() > 100_000);
        assert!(compact_len(&structured) <= 40_000 - escaped_len(&fitted.text));
    }

    #[test]
    fn a_cursor_that_names_the_last_row_follows_the_cut() {
        let payload = json!({
            "returned": 50, "truncated": false, "next_cursor": 50,
            "events": rows(50, 2000),
        });
        let structured = fit(true, payload, 30_000).unwrap().structured.unwrap();
        let kept = structured["events"].as_array().unwrap();
        assert_eq!(structured["next_cursor"], kept.last().unwrap()["id"]);
    }

    #[test]
    fn a_truncated_flag_is_added_only_where_it_cannot_clash() {
        let absent = fit(true, json!({ "xs": rows(20, 1000) }), 9_000).unwrap();
        assert_eq!(absent.structured.unwrap()["truncated"], true);
        let other = json!({ "truncated": "see below", "xs": rows(20, 1000) });
        let kept = fit(true, other, 9_000).unwrap().structured.unwrap();
        assert_eq!(
            kept["truncated"], "see below",
            "a non-bool is not overwritten"
        );
        assert!(kept[TRUNCATION_KEY].is_object());
    }

    #[test]
    fn a_named_flag_beside_the_list_is_set() {
        let payload = json!({
            "evidence": rows(30, 1000), "evidence_truncated": false,
            "gaps": [], "gaps_truncated": false,
        });
        let structured = fit(true, payload, 12_000).unwrap().structured.unwrap();
        assert_eq!(structured["evidence_truncated"], true);
        assert_eq!(structured["gaps_truncated"], false);
        assert_eq!(structured["gaps"], json!([]));
    }

    #[test]
    fn nested_lists_are_reached_and_no_key_is_ever_removed() {
        let payload = json!({
            "a": { "inner": [{ "deep": rows(40, 1000) }] },
            "b": 1,
            "c": { "d": [1, 2] },
        });
        let structured = fit(true, payload, 15_000).unwrap().structured.unwrap();
        assert!(
            structured["a"]["inner"][0]["deep"]
                .as_array()
                .unwrap()
                .len()
                < 40
        );
        assert_eq!(structured["b"], 1);
        assert_eq!(structured["c"]["d"], json!([1, 2]));
        let pointer = structured[TRUNCATION_KEY]["arrays"][0]["pointer"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(pointer, "/a/inner/0/deep");
    }

    #[test]
    fn pointers_escape_the_characters_a_key_may_hold() {
        let payload = json!({ "a/b~c": rows(30, 1000) });
        let structured = fit(true, payload, 8_000).unwrap().structured.unwrap();
        assert_eq!(
            structured[TRUNCATION_KEY]["arrays"][0]["pointer"],
            "/a~1b~0c"
        );
        assert!(structured["a/b~c"].as_array().unwrap().len() < 30);
    }

    #[test]
    fn a_modern_text_mirror_is_bounded_json_of_the_same_shape() {
        let payload = json!({ "ok": true, "events": rows(300, 4000) });
        let fitted = fit(true, payload, 4 * 1024 * 1024 - 1000).unwrap();
        assert!(
            fitted.text.len() <= TEXT_MIRROR_BYTES,
            "{}",
            fitted.text.len()
        );
        let mirror: Value = serde_json::from_str(&fitted.text).unwrap();
        assert!(mirror["events"].is_array());
        assert!(mirror[TRUNCATION_KEY].is_object());
        let structured = fitted.structured.unwrap();
        assert_eq!(structured["events"].as_array().unwrap().len(), 300);
        assert!(structured.get(TRUNCATION_KEY).is_none(), "one full copy");
    }

    #[test]
    fn a_legacy_text_gets_the_whole_budget() {
        let payload = json!({ "ok": true, "events": rows(300, 4000) });
        let fitted = fit(false, payload, 4 * 1024 * 1024 - 1000).unwrap();
        assert!(fitted.structured.is_none());
        let body: Value = serde_json::from_str(&fitted.text).unwrap();
        assert_eq!(body["events"].as_array().unwrap().len(), 300);
        assert!(body.get(TRUNCATION_KEY).is_none());
    }

    #[test]
    fn a_legacy_text_is_measured_escaped() {
        // Every quote in the payload doubles inside the text string.
        let payload = json!({ "xs": (0..40).map(|_| "\"".repeat(2000)).collect::<Vec<_>>() });
        let fitted = fit(false, payload, 50_000).unwrap();
        assert!(
            escaped_len(&fitted.text) <= 50_000,
            "{}",
            escaped_len(&fitted.text)
        );
        let body: Value = serde_json::from_str(&fitted.text).unwrap();
        assert!(body["xs"].as_array().unwrap().len() < 40);
    }

    #[test]
    fn the_result_never_exceeds_the_budget_for_any_split_point() {
        for count in [2, 3, 7, 64, 500] {
            for budget in [3_000, 10_000, 100_000] {
                let payload = json!({ "events": rows(count, 700), "n": count });
                match fit(true, payload, budget) {
                    Ok(fitted) => assert!(wire(&fitted) <= budget, "{count} rows, {budget}"),
                    Err(message) => assert!(message.contains("budget"), "{message}"),
                }
            }
        }
    }

    #[test]
    fn a_payload_with_no_list_to_cut_is_an_error_naming_size_and_cap() {
        let payload = json!({ "blob": "g".repeat(200_000) });
        let message = match fit(true, payload, 50_000) {
            Ok(_) => panic!("a giant string cannot be shortened by cutting a list"),
            Err(message) => message,
        };
        assert!(message.contains("200"), "{message}");
        assert!(message.contains("50000"), "{message}");
    }

    #[test]
    fn a_list_of_one_giant_element_is_emptied_rather_than_refused() {
        // Nothing smaller than one element can be kept, but the list can go.
        let payload = json!({ "xs": [{ "blob": "g".repeat(200_000) }], "n": 1 });
        let structured = fit(true, payload, 50_000).unwrap().structured.unwrap();
        assert_eq!(structured["xs"], json!([]));
        assert_eq!(structured["n"], 1);
        assert_eq!(structured[TRUNCATION_KEY]["arrays"][0]["kept"], 0);
    }

    #[test]
    fn hundreds_of_lists_converge_in_one_pass_and_the_marker_is_a_counted_sample() {
        let mut flat = Map::new();
        for n in 0..400 {
            flat.insert(
                format!("k{n}"),
                json!(["f".repeat(20_000), "g".repeat(20_000)]),
            );
        }
        let fitted = fit(true, Value::Object(flat), 4 * 1024 * 1024 - 512).unwrap();
        let structured = fitted.structured.unwrap();
        assert_eq!(
            structured.as_object().unwrap().len(),
            400 + 2,
            "no key removed"
        );
        let marker = &structured[TRUNCATION_KEY];
        assert!(marker["arrays"].as_array().unwrap().len() <= MARKER_ENTRIES);
        assert!(marker["arrays_total"].as_u64().unwrap() > MARKER_ENTRIES as u64);
        assert!(
            wire(&Fitted {
                text: fitted.text,
                structured: Some(structured)
            }) <= 4 * 1024 * 1024
        );
    }

    #[test]
    fn a_cursor_that_cannot_be_moved_is_withdrawn_and_has_more_is_set() {
        let tasks = json!({
            "has_more": false, "next_cursor": "opaque", "returned": 40,
            "tasks": rows(40, 2000),
        });
        let structured = fit(true, tasks, 20_000).unwrap().structured.unwrap();
        assert_eq!(structured["next_cursor"], Value::Null);
        assert_eq!(structured["has_more"], true);
        // A numeric cursor over rows without an `id` cannot be moved either.
        let numeric = json!({
            "next_cursor": 40, "truncated": false,
            "xs": (0..40).map(|_| json!({ "blob": "b".repeat(2000) })).collect::<Vec<_>>(),
        });
        let structured = fit(true, numeric, 20_000).unwrap().structured.unwrap();
        assert_eq!(structured["next_cursor"], Value::Null);
        assert_eq!(structured["truncated"], true);
    }

    #[test]
    fn the_budget_leaves_room_for_the_id_and_the_envelope() {
        let small = tool_budget(&json!(1));
        let long = tool_budget(&json!("x".repeat(1000)));
        assert_eq!(small - long, 1001);
        assert!(small <= MAX_FRAME_BYTES - ENVELOPE_RESERVE_BYTES);
    }
}
