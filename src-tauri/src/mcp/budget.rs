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
    2 + text.bytes().map(byte_cost).sum::<usize>()
}

/// Bytes one byte of text takes inside a JSON string, as `serde_json` writes
/// it: a quote, a backslash and the named control characters take two, any
/// other control character six, everything else (every byte of a multi-byte
/// character included) one. The one owner of the figure: the per-character
/// prefix walk and the byte counter both read it.
fn byte_cost(byte: u8) -> usize {
    match byte {
        b'"' | b'\\' | 0x08 | 0x09 | 0x0a | 0x0c | 0x0d => 2,
        0..=0x1f => 6,
        _ => 1,
    }
}

/// [`byte_cost`] summed over the bytes of one character.
fn char_cost(character: char) -> usize {
    if character.is_ascii() {
        byte_cost(character as u8)
    } else {
        character.len_utf8()
    }
}

/// The longest prefix of `text`, ending on a character boundary, that takes at
/// most `budget` bytes inside a JSON string.
pub(super) fn escaped_prefix(text: &str, budget: usize) -> &str {
    let mut used = 0;
    for (index, character) in text.char_indices() {
        used += char_cost(character);
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
/// and holds no list left to cut, or what is left is a page (a list beside a
/// `next_cursor`) whose cursor cannot follow a cut. A page is cut only when its
/// cursor is numeric and its rows carry integer ids, so the cursor can be moved
/// to the last kept row; any other page is refused, with a message telling the
/// caller to retry with a smaller `limit`, rather than cut into a cursor that
/// skips rows or a `has_more` with no cursor to continue from.
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
            .map_err(|refusal| refusal_message(&refusal, original, budget))?;
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
        match truncate(payload.clone(), original, mirror, escaped_compact_len) {
            Ok(cut) => to_compact(&cut)?,
            // Whatever stops the mirror (a page that cannot be cut honestly, or
            // a payload of long text with no list to cut) is the mirror's
            // problem, not the call's: the text says so and points at
            // `structuredContent`, which carries the result whole when the
            // budget allows. The call fails only if that copy cannot fit,
            // below. (An escaped-twice log field, as in `gitpulse_get_task`,
            // passes the mirror's half-budget long before the budget.)
            Err(_) => to_compact(&json!({
                TRUNCATION_KEY: {
                    "budget_bytes": mirror,
                    "original_bytes": original,
                    "arrays": [],
                    "arrays_total": 0,
                    "note": "this result is too large to mirror as text; read structuredContent",
                }
            }))?,
        }
    };
    let room = budget.saturating_sub(escaped_len(&text));
    let structured = if original <= room {
        payload
    } else {
        truncate(payload, original, room, compact_len)
            .map_err(|refusal| refusal_message(&refusal, original, budget))?
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
        self.0 += bytes.iter().map(|byte| byte_cost(*byte)).sum::<usize>();
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

/// The lists cut so far, in the order they were first cut, indexed by pointer
/// so recording a cut of a list already cut is a lookup, not a scan.
#[derive(Default)]
struct Cuts {
    list: Vec<Cut>,
    index: std::collections::HashMap<String, usize>,
}

impl Cuts {
    fn record(&mut self, pointer: &str, kept: usize, total: usize) {
        match self.index.get(pointer) {
            Some(at) => self.list[*at].kept = kept,
            None => {
                self.index.insert(pointer.to_string(), self.list.len());
                self.list.push(Cut {
                    pointer: pointer.to_string(),
                    kept,
                    total,
                });
            }
        }
    }
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
) -> Result<Value, Refusal> {
    let mut cuts = Cuts::default();
    // Whether a paged list was left alone because cutting it could not keep its
    // cursor honest; if the payload then stays over, that is the reason.
    let mut paged_left_alone = false;
    for _ in 0..MAX_ROUNDS {
        let size = measure(&payload);
        if size <= limit {
            return Ok(payload);
        }
        // First keep a head of every list; only when every list is down to one
        // element and the payload is still over is a list emptied, largest
        // first, because a payload whose smallest form is over the budget is
        // otherwise refused although its lists could still be cut.
        let mut cut_any = false;
        let mut need = size - limit + MARKER_SLACK_BYTES;
        for (floor, min_len) in [(1, 2), (0, 1)] {
            let (lists, left_alone) = cuttable_lists(&payload, min_len, floor);
            paged_left_alone |= left_alone;
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
        describe_cuts(&mut payload, limit, original, &cuts.list);
    }
    if measure(&payload) <= limit {
        return Ok(payload);
    }
    Err(if paged_left_alone {
        Refusal::Paged
    } else {
        Refusal::NothingToCut
    })
}

/// Why a payload could not be brought under its budget.
enum Refusal {
    /// It holds no list left to cut.
    NothingToCut,
    /// What remains is a page whose cursor cannot be kept honest across a cut.
    Paged,
}

/// The message of the tool execution error for a payload that cannot be cut
/// down: what it weighs, what it may, and what the caller can do.
fn refusal_message(refusal: &Refusal, original: usize, budget: usize) -> String {
    match refusal {
        Refusal::NothingToCut => format!(
            "tool result is {original} bytes and cannot be shortened to its {budget} byte \
             budget: it holds no list left to cut. Ask for less (a narrower argument or a \
             smaller limit)."
        ),
        Refusal::Paged => format!(
            "tool result is {original} bytes, over its {budget} byte budget, and it is a page \
             whose cursor cannot be moved without skipping rows, so it is not cut. Retry with a \
             smaller `limit`."
        ),
    }
}

/// Whether the cursor beside a list can follow a cut of it: numeric, with an
/// integer `id` on every element, so it can be moved to the last kept one.
fn cursor_can_follow(cursor: &Value, items: &[Value]) -> bool {
    (cursor.is_i64() || cursor.is_u64()) && has_row_ids(items)
}

/// Whether a list is a list of rows, each with an integer `id`.
fn has_row_ids(items: &[Value]) -> bool {
    !items.is_empty()
        && items
            .iter()
            .all(|item| item.get("id").is_some_and(Value::is_i64))
}

/// JSON pointers to the lists with at least `min_len` elements that are not
/// inside another such list, largest (by serialized bytes) first, and whether a
/// list was left out because it is a page (its object holds a `next_cursor`)
/// whose cursor [cannot follow a cut](cursor_can_follow) or that `floor` would
/// empty. A page is cut only where its cursor can be moved, so a cut can never
/// leave a reader a cursor that skips the dropped rows, nor `has_more` with no
/// cursor to continue from.
fn cuttable_lists(root: &Value, min_len: usize, floor: usize) -> (Vec<String>, bool) {
    struct Walk {
        min_len: usize,
        floor: usize,
        found: Vec<(usize, String)>,
        left_alone: bool,
    }
    fn walk(value: &Value, pointer: &mut String, state: &mut Walk) {
        match value {
            // Not descended into: cutting it shortens whatever is inside.
            Value::Array(items) if items.len() >= state.min_len => {
                state.found.push((compact_len(value), pointer.clone()));
            }
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    let mark = pointer.len();
                    pointer.push('/');
                    pointer.push_str(&index.to_string());
                    walk(item, pointer, state);
                    pointer.truncate(mark);
                }
            }
            Value::Object(members) => {
                let cursor = members.get("next_cursor");
                // A cursor names the last row of one list. With two lists whose
                // rows both carry ids it cannot be told which, and moving it for
                // one would skip rows of the other's page: neither is cut.
                let id_lists = members
                    .values()
                    .filter(|member| matches!(member, Value::Array(items) if has_row_ids(items)))
                    .count();
                for (key, item) in members {
                    if let (Value::Array(items), Some(cursor)) = (item, cursor) {
                        if items.len() >= state.min_len
                            && !(state.floor >= 1
                                && id_lists <= 1
                                && cursor_can_follow(cursor, items))
                        {
                            state.left_alone = true;
                            continue;
                        }
                    }
                    let mark = pointer.len();
                    pointer.push('/');
                    pointer.push_str(&key.replace('~', "~0").replace('/', "~1"));
                    walk(item, pointer, state);
                    pointer.truncate(mark);
                }
            }
            _ => {}
        }
    }
    let mut state = Walk {
        min_len,
        floor,
        found: Vec::new(),
        left_alone: false,
    };
    walk(root, &mut String::new(), &mut state);
    state.found.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    (
        state
            .found
            .into_iter()
            .map(|(_, pointer)| pointer)
            .collect(),
        state.left_alone,
    )
}

/// Cut the list at `pointer` to the longest prefix that removes at least
/// `overshoot` bytes (or to one element), and bring its parent's counts and
/// flags into line. Returns the bytes removed.
fn cut_list(
    root: &mut Value,
    pointer: &str,
    overshoot: usize,
    floor: usize,
    cuts: &mut Cuts,
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

    cuts.record(pointer, keep, before);

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
    for flag in ["truncated".to_string(), format!("{key}_truncated")] {
        if parent.get(&flag).is_some_and(Value::is_boolean) {
            parent.insert(flag, json!(true));
        }
    }
    // A cursor says where the next page starts, and the rows this cut dropped
    // are the next page's first, so a numeric cursor naming the last row of the
    // page is moved back to the last row that is left. Only such a page was
    // chosen for cutting ([`cuttable_lists`]); any other (the task board's
    // cursor is opaque) is never cut, because a cursor that skips the dropped
    // rows, or `has_more` with no cursor, is an outcome a reader cannot detect.
    if let (Some(id), Some(cursor)) = (last_id, parent.get_mut("next_cursor")) {
        *cursor = json!(id);
        // And the page says that more follows, in whichever flag it has: the
        // dropped rows are what follows, whatever the flag said of the page it
        // was cut from.
        for flag in ["has_more", "truncated"] {
            if parent.get(flag).is_some_and(Value::is_boolean) {
                parent.insert(flag.into(), json!(true));
            }
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
    fn a_page_whose_cursor_cannot_be_moved_is_refused_not_cut() {
        // Never `has_more: true` with `next_cursor: null` (a client looping on
        // has_more would restart at page one), and never a cursor that skips
        // the dropped rows: the page is not cut, the call is told to ask less.
        for (has_more, cursor) in [
            (json!(true), json!("opaque")),
            (json!(false), json!("opaque")),
            (json!(false), Value::Null),
        ] {
            let tasks = json!({
                "has_more": has_more, "next_cursor": cursor, "returned": 40,
                "tasks": rows(40, 2000),
            });
            for modern in [true, false] {
                let message = match fit(modern, tasks.clone(), 20_000) {
                    Ok(_) => panic!("a page that cannot be cut was cut (modern={modern})"),
                    Err(message) => message,
                };
                assert!(message.contains("smaller `limit`"), "{message}");
                assert!(message.contains("20000"), "{message}");
            }
        }
        // A numeric cursor over rows without an `id` cannot be moved either.
        let numeric = json!({
            "next_cursor": 40, "truncated": false,
            "xs": (0..40).map(|_| json!({ "blob": "b".repeat(2000) })).collect::<Vec<_>>(),
        });
        assert!(fit(true, numeric, 20_000).is_err());
        // Nor can an emptied page: it would leave a cursor naming no row.
        let ledger = json!({
            "next_cursor": 1, "truncated": false,
            "events": [{ "id": 1, "blob": "b".repeat(100_000) }],
        });
        assert!(fit(true, ledger, 20_000).is_err());
    }

    #[test]
    fn a_page_inside_the_budget_is_whole_and_a_text_mirror_too_big_for_it_says_so() {
        let tasks = json!({
            "has_more": true, "next_cursor": "opaque", "returned": 40,
            "tasks": rows(40, 20_000),
        });
        let fitted = fit(true, tasks.clone(), 4 * 1024 * 1024 - 512).unwrap();
        assert_eq!(fitted.structured.unwrap(), tasks, "the page is whole");
        let mirror: Value = serde_json::from_str(&fitted.text).unwrap();
        assert!(fitted.text.len() <= TEXT_MIRROR_BYTES);
        assert!(
            mirror[TRUNCATION_KEY]["note"]
                .as_str()
                .is_some_and(|note| note.contains("structuredContent")),
            "{mirror}"
        );
        // Legacy has only the text, so the same page is whole in it.
        let legacy = fit(false, tasks.clone(), 4 * 1024 * 1024 - 512).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&legacy.text).unwrap(), tasks);
    }

    /// `gitpulse_get_task` carries up to 256 KiB of logs twice (in the task and
    /// in the brief), each newline escaped twice in the text block. That text
    /// passes its half-budget mirror long before the budget; the call must not
    /// fail for it while `structuredContent` fits whole.
    #[test]
    fn a_long_string_payload_too_big_to_mirror_is_whole_in_structured_content() {
        let payload = json!({ "ok": true, "brief": "x\n".repeat(150_000) });
        let fitted = fit(true, payload.clone(), tool_budget(&json!(1))).unwrap();
        assert_eq!(fitted.structured, Some(payload));
        assert!(
            fitted.text.len() < 1024,
            "the mirror is a note: {}",
            fitted.text
        );
        assert!(fitted.text.contains("structuredContent"));
        // Legacy clients have only the text, and keep the whole budget in it.
        let legacy = fit(
            false,
            json!({ "ok": true, "brief": "x\n".repeat(150_000) }),
            tool_budget(&json!(1)),
        )
        .unwrap();
        assert!(legacy.text.len() > 300_000);
        // Past the budget itself it is an error naming size and cap.
        let message = match fit(true, json!({ "brief": "x\n".repeat(2_500_000) }), 1_000_000) {
            Ok(_) => panic!("a payload past its budget with no list was returned"),
            Err(message) => message,
        };
        assert!(message.contains("1000000"), "{message}");
    }

    #[test]
    fn a_cursor_beside_two_id_bearing_lists_refuses_rather_than_guessing_which_it_pages() {
        let two = json!({
            "next_cursor": 40,
            "events": (1..=40).map(|id| json!({ "id": id, "blob": "b".repeat(2000) })).collect::<Vec<_>>(),
            "others": (1..=40).map(|id| json!({ "id": id, "blob": "b".repeat(2000) })).collect::<Vec<_>>(),
        });
        let message = fit(true, two, 20_000).err().expect("refused");
        assert!(message.contains("smaller `limit`"), "{message}");
        // One id-bearing list and one without ids: the cursor is that one's.
        let one = json!({
            "next_cursor": 40,
            "events": (1..=40).map(|id| json!({ "id": id, "blob": "b".repeat(2000) })).collect::<Vec<_>>(),
            "notes": ["a", "b"],
        });
        let structured = fit(true, one, 40_000).unwrap().structured.unwrap();
        assert!(structured["events"].as_array().unwrap().len() < 40);
    }

    #[test]
    fn a_cut_followable_page_says_more_follows_in_the_flag_it_has() {
        let rows = |n: i64| {
            (1..=n)
                .map(|id| json!({ "id": id, "blob": "b".repeat(2000) }))
                .collect::<Vec<_>>()
        };
        for flag in ["has_more", "truncated"] {
            let page =
                json!({ flag: false, "next_cursor": 40, "returned": 40, "events": rows(40) });
            let structured = fit(true, page, 30_000).unwrap().structured.unwrap();
            let kept = structured["events"].as_array().unwrap();
            assert!(kept.len() < 40);
            assert_eq!(structured[flag], true, "{flag}");
            assert_eq!(structured["next_cursor"], kept.last().unwrap()["id"]);
        }
        // An unrelated flag the page does not have is not invented.
        let page = json!({ "next_cursor": 40, "events": rows(40) });
        let structured = fit(true, page, 30_000).unwrap().structured.unwrap();
        assert!(structured.get("has_more").is_none());
    }

    #[test]
    fn a_page_is_left_alone_while_another_list_in_the_payload_is_cut() {
        let payload = json!({
            "page": { "has_more": true, "next_cursor": "opaque", "tasks": rows(3, 100) },
            "other": rows(60, 2000),
        });
        let structured = fit(true, payload, 30_000).unwrap().structured.unwrap();
        assert_eq!(structured["page"]["tasks"].as_array().unwrap().len(), 3);
        assert_eq!(structured["page"]["next_cursor"], "opaque");
        assert_eq!(structured["page"]["has_more"], true);
        assert!(structured["other"].as_array().unwrap().len() < 60);
        // Every list in an object that holds a cursor belongs to its page: the
        // cursor cannot say which one it pages, so none is cut.
        let one_object = json!({
            "next_cursor": "opaque", "tasks": rows(3, 100), "other": rows(60, 2000),
        });
        assert!(fit(true, one_object, 30_000).is_err());
    }

    #[test]
    fn the_budget_leaves_room_for_the_id_and_the_envelope() {
        let small = tool_budget(&json!(1));
        let long = tool_budget(&json!("x".repeat(1000)));
        assert_eq!(small - long, 1001);
        assert!(small <= MAX_FRAME_BYTES - ENVELOPE_RESERVE_BYTES);
    }
}
