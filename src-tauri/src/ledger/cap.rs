//! Byte caps for what the ledger stores and what a page of it carries.
//!
//! Two different bounds live here because they answer one question, "how much
//! of one field is worth keeping", at two moments:
//!
//! * **At append** ([`cap_object`]): an event's `object` is cut to
//!   [`OBJECT_CAP_BYTES`] *after* redaction, and the cut is recorded inside
//!   `detail_json` ([`ObjectCap`]) because the event contract is closed to a
//!   new top-level field. The hash covers the redacted full text, never the
//!   raw one, so it cannot fingerprint a secret the redactor removed.
//! * **At read** ([`LedgerEvent::bounded`]): a row stored before the append cap
//!   existed, or a field no append cap covers, is cut again for a page, so one
//!   heavy row cannot make a page the transport cannot carry. Markers go
//!   *inside* the JSON values for the same reason as above.

use serde::Serialize;
use serde_json::{json, Map, Value};

use super::LedgerEvent;

/// Longest `object` the ledger stores, in bytes.
pub(super) const OBJECT_CAP_BYTES: usize = 2048;

/// Longest a path field (`repo_path`, `worktree_path`) may be, in bytes: the
/// longest `PATH_MAX` of the platforms GitPulse runs on (Linux, 4096).
const PATH_FIELD_CAP_BYTES: usize = 4096;

/// Longest an identifier or label field (`ulid`, `ts_utc`, `actor_kind`,
/// `actor_id`, `session_id`, `task_id`, `action`, `outcome`) may be, in bytes.
/// A ULID is 26 bytes and a timestamp 24; the rest are short names.
const ID_FIELD_CAP_BYTES: usize = 256;

/// Longest a git ref field (`before_ref`, `after_ref`) may be, in bytes: a
/// ref name or object id, with room for a long branch name.
const REF_FIELD_CAP_BYTES: usize = 1024;

/// What a cut text field ends with, inside its cap. One convention for every
/// cut a stored or paged field makes without a count of its own (`object` at
/// append, and each bounded id, path and ref); the genuine-record check below
/// locates the `object` cut by this suffix's fixed length.
const OBJECT_CUT_SUFFIX: &str = "… [truncated]";

/// The `detail_json` key that records an `object` cut.
pub(super) const OBJECT_TRUNCATED_KEY: &str = "object_truncated";

/// Longest any one text field of a page row may be, in bytes.
pub(crate) const ROW_FIELD_CAP_BYTES: usize = 16 * 1024;

/// Longest `object` a page row carries.
const ROW_OBJECT_CAP_BYTES: usize = 2 * 1024;

/// Longest one `detail_json` member, or one string inside a verdict or argv
/// element, a page row keeps whole.
const MEMBER_CAP_BYTES: usize = 256;

/// Room a page row keeps back for the markers it adds to a cut field.
const MARKER_RESERVE_BYTES: usize = 2048;

/// Names of cut members a `gitpulse_truncated` marker lists.
const MARKER_KEYS: usize = 32;

/// Longest a cut member's name is listed, in bytes.
const MARKER_KEY_BYTES: usize = 48;

/// The `detail_json` members that carry event time and the append cut. They
/// are small, server-built and read by consumers, so a page never drops them.
const KEPT_MEMBERS: [&str; 4] = [
    "occurred_at",
    "occurred_at_basis",
    "task_basis",
    OBJECT_TRUNCATED_KEY,
];

/// The `detail_json` member that records elements a page cut from a
/// `verdict_json` array. The verdict contract admits no member the gate did not
/// write, so the record rides in the row's open `detail_json` instead.
const VERDICT_TRUNCATED_KEY: &str = "verdict_truncated";

/// The record of a cut `object`: where it was cut, how long it was, and the
/// FNV-1a 64 hash of the redacted full text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ObjectCap {
    pub(super) at: usize,
    pub(super) bytes: usize,
    pub(super) hash: String,
}

impl ObjectCap {
    /// Whether every value has the shape [`cap_object`] writes, so none can
    /// carry text a caller chose.
    pub(super) fn is_well_formed(&self) -> bool {
        self.at <= OBJECT_CAP_BYTES
            && self.at <= self.bytes
            && self
                .hash
                .strip_prefix("fnv1a64:")
                .is_some_and(|hex| hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
    }

    pub(super) fn as_value(&self) -> Value {
        json!({ "at": self.at, "bytes": self.bytes, "hash": self.hash })
    }
}

/// `redacted` cut to [`OBJECT_CAP_BYTES`] at a character boundary, and the
/// record of the cut when there was one. Exactly the cap is not a cut.
pub(super) fn cap_object(redacted: String) -> (String, Option<ObjectCap>) {
    if redacted.len() <= OBJECT_CAP_BYTES {
        return (redacted, None);
    }
    let (kept, at) = cut_with_suffix(&redacted, OBJECT_CAP_BYTES);
    let cap = ObjectCap {
        at,
        bytes: redacted.len(),
        hash: format!("fnv1a64:{:016x}", devmap_extract::content_hash(&redacted)),
    };
    (kept, Some(cap))
}

/// `text` cut to at most `cap` bytes at a character boundary, ending with
/// [`OBJECT_CUT_SUFFIX`], and where the kept text ends. Text within `cap` is
/// returned whole, with `at` its length.
fn cut_with_suffix(text: &str, cap: usize) -> (String, usize) {
    if text.len() <= cap {
        return (text.to_string(), text.len());
    }
    let at = text.floor_char_boundary(cap.saturating_sub(OBJECT_CUT_SUFFIX.len()));
    let mut kept = String::with_capacity(cap);
    kept.push_str(&text[..at]);
    kept.push_str(OBJECT_CUT_SUFFIX);
    (kept, at)
}

fn bound_field(text: String, cap: usize) -> String {
    if text.len() <= cap {
        text
    } else {
        cut_with_suffix(&text, cap).0
    }
}

/// Whether a stored (already redacted) `object` and its `detail_json` are a cut
/// that [`cap_object`] made: the record parses to a well-formed [`ObjectCap`],
/// the object is within the cap and ends with the cut suffix, and the record's
/// `at` is exactly where that suffix begins. Only such a row may carry its
/// record across a copy; a record that fails any of this was written by
/// someone else, and the copy drops it.
pub(super) fn recorded_cut_is_genuine(object: &str, detail: Option<&str>) -> bool {
    if object.len() > OBJECT_CAP_BYTES || !object.ends_with(OBJECT_CUT_SUFFIX) {
        return false;
    }
    let Some(Value::Object(members)) = detail.and_then(|text| serde_json::from_str(text).ok())
    else {
        return false;
    };
    let Some(Value::Object(record)) = members.get(OBJECT_TRUNCATED_KEY) else {
        return false;
    };
    let number = |key: &str| {
        record
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
    };
    let (Some(at), Some(bytes), Some(Value::String(hash))) =
        (number("at"), number("bytes"), record.get("hash"))
    else {
        return false;
    };
    ObjectCap {
        at,
        bytes,
        hash: hash.clone(),
    }
    .is_well_formed()
        && record.len() == 3
        && at + OBJECT_CUT_SUFFIX.len() == object.len()
}

/// What identifies a stored object across the cap: the stored (capped) text,
/// and for a cut one the FNV-1a hash of the redacted full text, so two long
/// objects that share their first 2 KiB are not the same object. An object
/// within the cap is its own key. A row stored before the cap holds the full
/// redacted object and gets the same key from it; a row stored after holds the
/// capped text and the hash it recorded ([`object_key_of_row`]).
pub(super) fn object_key(redacted_full: String) -> String {
    let (kept, cap) = cap_object(redacted_full);
    match cap {
        Some(cap) => format!("{kept}#{}", cap.hash),
        None => kept,
    }
}

/// [`object_key`] for a stored row: its `object` column, and the hash recorded
/// in its `detail_json` when it was cut. A row with no recorded hash holds the
/// full object (stored before the cap), whose key is computed from it.
pub(super) fn object_key_of_row(stored: String, recorded_hash: Option<String>) -> String {
    match recorded_hash {
        Some(hash) => format!("{stored}#{hash}"),
        None => object_key(stored),
    }
}

/// `text` cut to at most `cap` bytes, ending with how much was removed.
fn cut_text(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    // Room for `…[truncated 18446744073709551615 bytes]`.
    let keep = text.floor_char_boundary(cap.saturating_sub(40));
    format!("{}…[truncated {} bytes]", &text[..keep], text.len() - keep)
}

/// A JSON object's text cut to about `cap` bytes, as a JSON object.
///
/// Members past [`MEMBER_CAP_BYTES`], or past the room left, are removed and
/// named in a `gitpulse_truncated` member; text that is not a JSON object is
/// replaced by that member and a `head`. Event-time members are always kept.
pub(crate) fn bound_json_text(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let Ok(Value::Object(members)) = serde_json::from_str::<Value>(text) else {
        return json!({
            "gitpulse_truncated": { "keys": [], "bytes": text.len() },
            // An escaped character costs at most six bytes on the wire.
            "head": &text[..text.floor_char_boundary(cap.saturating_sub(MARKER_RESERVE_BYTES) / 6)],
        })
        .to_string();
    };
    let room = cap.saturating_sub(MARKER_RESERVE_BYTES);
    let mut kept = Map::new();
    let mut used = 0;
    let mut cut_keys: Vec<String> = Vec::new();
    let mut cut_bytes = 0;
    for (key, value) in members {
        let size = key.len() + value.to_string().len() + 4;
        let essential = KEPT_MEMBERS.contains(&key.as_str());
        if size <= MEMBER_CAP_BYTES && (essential || used + size <= room) {
            used += size;
            kept.insert(key, value);
        } else {
            cut_bytes += size;
            if cut_keys.len() < MARKER_KEYS {
                cut_keys.push(key[..key.floor_char_boundary(MARKER_KEY_BYTES)].to_string());
            }
        }
    }
    if !cut_keys.is_empty() {
        kept.insert(
            "gitpulse_truncated".into(),
            json!({ "keys": cut_keys, "bytes": cut_bytes }),
        );
    }
    Value::Object(kept).to_string()
}

/// A cut recorded beside the field it cut, for a row's JSON text field
/// (`data_json_truncated` on evidence and gap rows): how many top-level
/// elements (array) or members (object) were kept of how many, and the text's
/// length before the cut. The text itself carries no marker, so it stays what
/// its readers parse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct JsonCut {
    pub kept: usize,
    pub total: usize,
    pub bytes: usize,
}

/// What a page cut from a `verdict_json`, recorded as the row's top-level
/// `verdict_truncated`. The verdict contract admits no member the gate did not
/// write, so the record is a sibling of the verdict, never inside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValueCut {
    /// Array elements removed, across every array.
    pub elements_dropped: usize,
    /// Object members removed, across every object.
    pub members_dropped: usize,
    /// Long strings shortened (each also ends with how much it lost).
    pub strings_cut: usize,
    /// The verdict's length before any cut, in bytes.
    pub bytes: usize,
}

/// What [`shrink_to`] removed.
#[derive(Default)]
struct Shrunk {
    elements: usize,
    members: usize,
    strings: usize,
}

/// When [`shrink_to`] shortens long strings.
#[derive(PartialEq)]
enum Strings {
    /// Before anything is removed: a verdict keeps every element and member it
    /// can, so its long strings go first.
    First,
    /// Only when nothing else can give: evidence keeps what it has whole.
    LastResort,
}

fn escape_pointer(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// Where [`shrink_to`] would cut, by what is left to give.
#[derive(Default)]
struct Plan {
    /// The largest array with more than one element and no array inside it.
    leaf: Option<(usize, String)>,
    /// The largest array with more than one element that holds an array.
    wrapper: Option<(usize, String)>,
    /// The longest string worth shortening.
    string: Option<(usize, String)>,
    /// The object with the most members.
    object: Option<(usize, String)>,
}

fn note(slot: &mut Option<(usize, String)>, size: usize, pointer: &str) {
    if slot.as_ref().is_none_or(|(best, _)| size > *best) {
        *slot = Some((size, pointer.to_string()));
    }
}

/// Fills `plan` from `value`; whether `value` is or holds an array.
fn survey(value: &Value, pointer: &str, plan: &mut Plan) -> bool {
    match value {
        Value::Array(items) => {
            let mut nested = false;
            for (n, item) in items.iter().enumerate() {
                nested |= survey(item, &format!("{pointer}/{n}"), plan);
            }
            if items.len() > 1 {
                let size = value.to_string().len();
                note(
                    if nested {
                        &mut plan.wrapper
                    } else {
                        &mut plan.leaf
                    },
                    size,
                    pointer,
                );
            }
            true
        }
        Value::Object(members) => {
            let mut nested = false;
            for (key, item) in members {
                nested |= survey(item, &format!("{pointer}/{}", escape_pointer(key)), plan);
            }
            if !members.is_empty() {
                note(&mut plan.object, members.len(), pointer);
            }
            nested
        }
        Value::String(text) if text.len() > MEMBER_CAP_BYTES / 2 => {
            note(&mut plan.string, text.len(), pointer);
            false
        }
        _ => false,
    }
}

fn cut_strings(value: &mut Value, shrunk: &mut Shrunk) {
    match value {
        Value::String(text) if text.len() > MEMBER_CAP_BYTES * 2 => {
            *text = cut_text(text, MEMBER_CAP_BYTES * 2);
            shrunk.strings += 1;
        }
        Value::Array(items) => items.iter_mut().for_each(|item| cut_strings(item, shrunk)),
        Value::Object(members) => members
            .values_mut()
            .for_each(|item| cut_strings(item, shrunk)),
        _ => {}
    }
}

/// `value` cut in place to at most `cap` serialized bytes, if it can be, without
/// changing the kind of any value and without adding anything.
///
/// Each pass takes at most half of one container, so several large ones share
/// the cut and a pass count stays logarithmic. In order of what is least lossy:
/// the tail of the largest array that holds no array (nested arrays are cut in
/// place, never dropped as a group), the tail of the largest array that does, a
/// long string, then the trailing members of the object with the most. An array
/// keeps at least its first element, so a cut never reads as "nothing".
fn shrink_to(value: &mut Value, cap: usize, strings: Strings) -> Shrunk {
    let mut shrunk = Shrunk::default();
    if strings == Strings::First {
        cut_strings(value, &mut shrunk);
    }
    loop {
        let total = value.to_string().len();
        if total <= cap {
            break;
        }
        let mut excess = total - cap;
        let mut plan = Plan::default();
        survey(value, "", &mut plan);
        if let Some((_, pointer)) = plan.leaf.or(plan.wrapper) {
            let Some(Value::Array(items)) = value.pointer_mut(&pointer) else {
                break;
            };
            let mut budget = (items.len() / 2).max(1).min(items.len() - 1);
            while excess > 0 && budget > 0 {
                budget -= 1;
                let Some(item) = items.pop() else { break };
                excess = excess.saturating_sub(item.to_string().len() + 1);
                shrunk.elements += 1;
            }
        } else if let Some((_, pointer)) = plan.string {
            let Some(Value::String(text)) = value.pointer_mut(&pointer) else {
                break;
            };
            *text = cut_text(
                text,
                text.len().saturating_sub(excess).max(MEMBER_CAP_BYTES / 2),
            );
            shrunk.strings += 1;
        } else if let Some((_, pointer)) = plan.object {
            let Some(Value::Object(members)) = value.pointer_mut(&pointer) else {
                break;
            };
            let mut budget = members.len().div_ceil(2);
            while excess > 0 && budget > 0 {
                budget -= 1;
                let Some(key) = members.keys().next_back().cloned() else {
                    break;
                };
                if let Some(item) = members.remove(&key) {
                    excess = excess.saturating_sub(key.len() + item.to_string().len() + 4);
                    shrunk.members += 1;
                }
            }
        } else {
            break;
        }
    }
    shrunk
}

/// Evidence JSON text (`gaps[].evidence_json`, `evidence[].data_json`) cut to
/// at most `cap` bytes without changing what kind of JSON value it is, and the
/// record of the cut for the row's `data_json_truncated`.
///
/// An array keeps its leading elements and loses the rest, and nothing is added
/// to it: no marker element, so every element still meets whatever schema its
/// readers hold. If the first element alone is over `cap` it is shortened in
/// place (its long strings cut, its inner arrays and members trimmed) rather
/// than dropped. A JSON string stays a string. An object loses members past the
/// room, named in its `gitpulse_truncated` member (see [`bound_json_text`]).
pub(crate) fn bound_evidence_json(text: &str, cap: usize) -> (String, Option<JsonCut>) {
    if text.len() <= cap {
        return (text.to_string(), None);
    }
    let bytes = text.len();
    let count = |value: &Value| match value {
        Value::Array(items) => items.len(),
        Value::Object(members) => members.len(),
        _ => 1,
    };
    match serde_json::from_str::<Value>(text) {
        Ok(mut value @ (Value::Array(_) | Value::String(_))) => {
            let total = count(&value);
            shrink_to(&mut value, cap, Strings::LastResort);
            let kept = count(&value);
            (value.to_string(), Some(JsonCut { kept, total, bytes }))
        }
        Ok(value) => {
            let total = count(&value);
            let cut = bound_json_text(text, cap);
            let kept = match serde_json::from_str::<Value>(&cut) {
                Ok(Value::Object(members)) => members
                    .keys()
                    .filter(|key| key.as_str() != "gitpulse_truncated")
                    .count(),
                _ => 0,
            };
            (cut, Some(JsonCut { kept, total, bytes }))
        }
        Err(_) => (
            bound_json_text(text, cap),
            Some(JsonCut {
                kept: 0,
                total: 1,
                bytes,
            }),
        ),
    }
}

/// `argv_json` cut to about `cap` bytes, still a JSON array.
fn bound_argv_json(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let Ok(Value::Array(elements)) = serde_json::from_str::<Value>(text) else {
        return json!([cut_text(text, ROW_OBJECT_CAP_BYTES)]).to_string();
    };
    let room = cap.saturating_sub(MARKER_RESERVE_BYTES);
    let total = elements.len();
    let mut kept: Vec<Value> = Vec::new();
    let mut used = 0;
    for element in elements {
        let element = match element {
            Value::String(text) => Value::String(cut_text(&text, MEMBER_CAP_BYTES * 2)),
            other => match other.to_string() {
                text if text.len() > MEMBER_CAP_BYTES * 2 => {
                    Value::String(cut_text(&text, MEMBER_CAP_BYTES * 2))
                }
                _ => other,
            },
        };
        let size = element.to_string().len() + 1;
        if used + size > room {
            break;
        }
        used += size;
        kept.push(element);
    }
    if kept.len() < total {
        kept.push(Value::String(format!("…[{} more]", total - kept.len())));
    }
    Value::Array(kept).to_string()
}

/// `verdict_json` cut to at most `cap` bytes, and the record of the cut for the
/// row's `verdict_truncated`.
///
/// Long strings are shortened first (each ends with how much it lost), then
/// array tails are dropped, then trailing object members, by [`shrink_to`]. No
/// member is added and no value changes kind: an array stays an array, an
/// object an object.
fn bound_verdict_json(text: &str, cap: usize) -> (String, Option<ValueCut>) {
    if text.len() <= cap {
        return (text.to_string(), None);
    }
    let Ok(mut value) = serde_json::from_str::<Value>(text) else {
        let cut = ValueCut {
            elements_dropped: 0,
            members_dropped: 0,
            strings_cut: 1,
            bytes: text.len(),
        };
        return (cut_text(text, cap), Some(cut));
    };
    let shrunk = shrink_to(&mut value, cap, Strings::First);
    let cut = ValueCut {
        elements_dropped: shrunk.elements,
        members_dropped: shrunk.members,
        strings_cut: shrunk.strings,
        bytes: text.len(),
    };
    let changed = shrunk.elements + shrunk.members + shrunk.strings > 0;
    (value.to_string(), changed.then_some(cut))
}

impl LedgerEvent {
    /// This event with every field that can be large cut for a page.
    ///
    /// A cut field stays the kind of thing its consumers parse: `detail_json`
    /// an object, `argv_json` an array, `verdict_json` the verdict with shorter
    /// strings. The row keeps its shape and its fields; only their length
    /// changes. `detail_json`, `argv_json` and `object` say so inside the value;
    /// a `verdict_json` cut is reported by [`LedgerEvent::bounded_recorded`].
    pub fn bounded(self) -> Self {
        self.bounded_recorded().0
    }

    /// [`LedgerEvent::bounded`], and the record of what it cut from
    /// `verdict_json`, for the page to carry as the row's `verdict_truncated`.
    /// The verdict itself has no room for a marker and `detail_json` is left as
    /// stored, so the record is the only sign.
    pub fn bounded_recorded(mut self) -> (Self, Option<ValueCut>) {
        self.ulid = bound_field(self.ulid, ID_FIELD_CAP_BYTES);
        self.ts_utc = bound_field(self.ts_utc, ID_FIELD_CAP_BYTES);
        self.repo_path = bound_field(self.repo_path, PATH_FIELD_CAP_BYTES);
        self.worktree_path = self
            .worktree_path
            .map(|text| bound_field(text, PATH_FIELD_CAP_BYTES));
        self.actor_kind = bound_field(self.actor_kind, ID_FIELD_CAP_BYTES);
        for id in [&mut self.actor_id, &mut self.session_id, &mut self.task_id] {
            *id = id.take().map(|text| bound_field(text, ID_FIELD_CAP_BYTES));
        }
        self.action = bound_field(self.action, ID_FIELD_CAP_BYTES);
        self.outcome = bound_field(self.outcome, ID_FIELD_CAP_BYTES);
        for reference in [&mut self.before_ref, &mut self.after_ref] {
            *reference = reference
                .take()
                .map(|text| bound_field(text, REF_FIELD_CAP_BYTES));
        }
        self.object = self
            .object
            .map(|object| cut_text(&object, ROW_OBJECT_CAP_BYTES));
        self.argv_json = self
            .argv_json
            .map(|text| bound_argv_json(&text, ROW_FIELD_CAP_BYTES));
        let mut verdict_cut = None;
        if let Some(text) = self.verdict_json.take() {
            let (text, cut) = bound_verdict_json(&text, ROW_FIELD_CAP_BYTES);
            self.verdict_json = Some(text);
            verdict_cut = cut;
        }
        self.detail_json = self
            .detail_json
            .map(|text| bound_json_text(&text, ROW_FIELD_CAP_BYTES));
        (self, verdict_cut)
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn a_row_under_every_cap_is_returned_exactly() {
        let mut row = event();
        row.object = Some("git status".into());
        row.argv_json = Some(r#"["git","status"]"#.into());
        row.verdict_json = Some(r#"{"status":"allow"}"#.into());
        row.detail_json = Some(r#"{"a":1}"#.into());
        assert_eq!(row.clone().bounded(), row);
    }

    #[test]
    fn a_long_detail_keeps_its_small_members_and_names_what_it_cut() {
        let mut row = event();
        let big = "b".repeat(40_000);
        row.detail_json = Some(
            serde_json::json!({
                "occurred_at": "2026-09-01T12:00:05.250Z",
                "occurred_at_basis": "transcript",
                "task_basis": "bound",
                "source": "transcript",
                "payload": big,
                "object_truncated": { "at": 1, "bytes": 9, "hash": "fnv1a64:0000000000000000" },
            })
            .to_string(),
        );
        let text = row.bounded().detail_json.unwrap();
        assert!(text.len() < ROW_FIELD_CAP_BYTES, "{}", text.len());
        let detail: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(detail["source"], "transcript");
        for key in KEPT_MEMBERS {
            assert!(detail.get(key).is_some(), "{key} was cut: {detail}");
        }
        assert!(detail.get("payload").is_none());
        assert_eq!(detail["gitpulse_truncated"]["keys"], json!(["payload"]));
        assert!(detail["gitpulse_truncated"]["bytes"].as_u64().unwrap() > 40_000);
    }

    #[test]
    fn many_small_members_are_kept_only_as_far_as_they_fit() {
        let members: Map<String, Value> = (0..2000)
            .map(|n| (format!("key{n:04}"), json!("v".repeat(100))))
            .collect();
        let mut row = event();
        row.detail_json = Some(Value::Object(members).to_string());
        let text = row.bounded().detail_json.unwrap();
        assert!(text.len() <= ROW_FIELD_CAP_BYTES, "{}", text.len());
        let detail: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            detail["gitpulse_truncated"]["keys"]
                .as_array()
                .unwrap()
                .len(),
            MARKER_KEYS,
            "the list of cut names is itself bounded"
        );
    }

    #[test]
    fn a_detail_that_is_not_an_object_is_wrapped_with_a_bounded_head() {
        for text in [
            "x".repeat(50_000),
            format!("[{}]", "1,".repeat(30_000)),
            "\"é😀\u{1}\"".repeat(9_000),
        ] {
            let mut row = event();
            row.detail_json = Some(text.clone());
            let cut = row.bounded().detail_json.unwrap();
            assert!(cut.len() <= ROW_FIELD_CAP_BYTES, "{} bytes", cut.len());
            let detail: Value = serde_json::from_str(&cut).unwrap();
            assert_eq!(detail["gitpulse_truncated"]["bytes"], text.len());
            assert!(text.starts_with(detail["head"].as_str().unwrap()));
        }
    }

    #[test]
    fn argv_stays_an_array_with_long_elements_cut_and_a_count_of_the_rest() {
        let mut argv: Vec<String> = vec!["tool".into(), "z".repeat(3000)];
        argv.extend((0..2000).map(|n| format!("--flag-{n}=value-{n}")));
        let mut row = event();
        row.argv_json = Some(serde_json::to_string(&argv).unwrap());
        let text = row.bounded().argv_json.unwrap();
        assert!(text.len() <= ROW_FIELD_CAP_BYTES, "{}", text.len());
        let cut: Vec<String> = serde_json::from_str(&text).unwrap();
        assert_eq!(cut[0], "tool");
        assert!(cut[1].len() <= MEMBER_CAP_BYTES * 2 && cut[1].contains("truncated"));
        let last = cut.last().unwrap();
        assert!(last.starts_with('…') && last.ends_with(" more]"), "{last}");
        assert!(cut.len() < argv.len());
    }

    #[test]
    fn a_verdict_gains_no_key_and_loses_no_key() {
        let verdict = json!({
            "status": "blocked",
            "checked": true,
            "target": "t".repeat(40_000),
            "nested": { "reason": "r".repeat(5_000), "n": 3 },
            "list": ["a", "b".repeat(5_000)],
        });
        let mut row = event();
        row.verdict_json = Some(verdict.to_string());
        let text = row.bounded().verdict_json.unwrap();
        assert!(text.len() < ROW_FIELD_CAP_BYTES, "{}", text.len());
        let cut: Value = serde_json::from_str(&text).unwrap();
        let keys =
            |value: &Value| -> Vec<String> { value.as_object().unwrap().keys().cloned().collect() };
        assert_eq!(keys(&cut), keys(&verdict), "a key was added or removed");
        assert_eq!(keys(&cut["nested"]), keys(&verdict["nested"]));
        assert_eq!(cut["nested"]["n"], 3);
        assert_eq!(cut["status"], "blocked");
        assert!(cut["target"].as_str().unwrap().contains("…[truncated "));
        assert_eq!(cut["list"][0], "a");
    }

    #[test]
    fn every_cut_lands_on_a_character_boundary() {
        for glyph in ["é", "€", "😀"] {
            for offset in 0..glyph.len() {
                let text = format!("{}{}", "a".repeat(100 + offset), glyph.repeat(3000));
                let cut = cut_text(&text, 200);
                assert!(cut.len() <= 200, "{glyph} +{offset}: {}", cut.len());
                assert!(cut.contains("…[truncated "));
            }
        }
        let mut row = event();
        row.object = Some("😀".repeat(2000));
        assert!(row.bounded().object.unwrap().len() <= ROW_OBJECT_CAP_BYTES);
    }

    // Bounds the tests hold the page to, written out so a change to a constant
    // is a visible change to the contract.
    const PATH_BOUND: usize = 4096;
    const ID_BOUND: usize = 256;
    const REF_BOUND: usize = 1024;

    fn oversized(glyph: &str, bound: usize) -> String {
        // Starts one byte short of the bound so the glyph straddles it.
        format!("{}{}", "a".repeat(bound - 1), glyph.repeat(bound))
    }

    #[test]
    fn every_string_field_of_a_row_is_bounded_in_bounded() {
        for glyph in ["a", "é", "€", "😀"] {
            let mut row = event();
            row.ulid = oversized(glyph, ID_BOUND * 4);
            row.ts_utc = oversized(glyph, ID_BOUND * 4);
            row.repo_path = oversized(glyph, PATH_BOUND * 4);
            row.worktree_path = Some(oversized(glyph, PATH_BOUND * 4));
            row.actor_kind = oversized(glyph, ID_BOUND * 4);
            row.actor_id = Some(oversized(glyph, ID_BOUND * 4));
            row.session_id = Some(oversized(glyph, ID_BOUND * 4));
            row.task_id = Some(oversized(glyph, ID_BOUND * 4));
            row.action = oversized(glyph, ID_BOUND * 4);
            row.outcome = oversized(glyph, ID_BOUND * 4);
            row.before_ref = Some(oversized(glyph, REF_BOUND * 4));
            row.after_ref = Some(oversized(glyph, REF_BOUND * 4));
            let cut = row.bounded();
            for (name, text, bound) in [
                ("ulid", &cut.ulid, ID_BOUND),
                ("ts_utc", &cut.ts_utc, ID_BOUND),
                ("repo_path", &cut.repo_path, PATH_BOUND),
                (
                    "worktree_path",
                    cut.worktree_path.as_ref().unwrap(),
                    PATH_BOUND,
                ),
                ("actor_kind", &cut.actor_kind, ID_BOUND),
                ("actor_id", cut.actor_id.as_ref().unwrap(), ID_BOUND),
                ("session_id", cut.session_id.as_ref().unwrap(), ID_BOUND),
                ("task_id", cut.task_id.as_ref().unwrap(), ID_BOUND),
                ("action", &cut.action, ID_BOUND),
                ("outcome", &cut.outcome, ID_BOUND),
                ("before_ref", cut.before_ref.as_ref().unwrap(), REF_BOUND),
                ("after_ref", cut.after_ref.as_ref().unwrap(), REF_BOUND),
            ] {
                assert!(text.len() <= bound, "{name} {glyph}: {} bytes", text.len());
                assert!(
                    text.ends_with(OBJECT_CUT_SUFFIX),
                    "{name} {glyph} was cut without the one cut suffix"
                );
            }
        }
    }

    #[test]
    fn a_field_at_its_bound_is_not_cut() {
        let mut row = event();
        row.repo_path = "p".repeat(PATH_BOUND);
        row.actor_id = Some("i".repeat(ID_BOUND));
        row.after_ref = Some("r".repeat(REF_BOUND));
        assert_eq!(row.clone().bounded(), row);
    }

    #[test]
    fn a_capped_object_row_still_passes_the_genuine_record_check() {
        let (kept, cap) = cap_object("é".repeat(5000));
        let cap = cap.expect("cut");
        let detail = json!({ OBJECT_TRUNCATED_KEY: cap.as_value() }).to_string();
        let mut row = event();
        row.object = Some(kept.clone());
        row.detail_json = Some(detail.clone());
        row.repo_path = oversized("é", PATH_BOUND * 2);
        row.actor_id = Some(oversized("😀", ID_BOUND * 2));
        let bounded = row.bounded();
        // `bounded` leaves the capped object and its record as they were...
        assert_eq!(bounded.object.as_deref(), Some(kept.as_str()));
        assert!(recorded_cut_is_genuine(
            bounded.object.as_deref().unwrap(),
            bounded.detail_json.as_deref()
        ));
        // ...and the read-side cut spelling is not accepted as an append cut.
        let read_cut = cut_text(&"é".repeat(5000), ROW_OBJECT_CAP_BYTES);
        assert!(!recorded_cut_is_genuine(&read_cut, Some(&detail)));
    }

    #[test]
    fn a_verdict_array_is_cut_from_the_end_and_stays_an_array_with_every_key() {
        let verdict = json!({
            "status": "blocked",
            "findings": (0..4000).map(|n| json!({ "n": n, "why": "reason" })).collect::<Vec<_>>(),
            "paths": (0..4000).map(|n| format!("src/file-{n}.rs")).collect::<Vec<_>>(),
            "small": [1, 2],
        });
        let mut row = event();
        row.verdict_json = Some(verdict.to_string());
        let (cut, record) = row.bounded_recorded();
        let text = cut.verdict_json.unwrap();
        assert!(text.len() <= ROW_FIELD_CAP_BYTES, "{}", text.len());
        let value: Value = serde_json::from_str(&text).unwrap();
        let keys =
            |value: &Value| -> Vec<String> { value.as_object().unwrap().keys().cloned().collect() };
        assert_eq!(keys(&value), keys(&verdict), "a key was added or removed");
        for key in ["findings", "paths", "small"] {
            assert!(value[key].is_array(), "{key} changed type");
        }
        assert_eq!(value["small"], json!([1, 2]));
        // Cut from the end: what is left is a prefix, with no sentinel element.
        let kept = value["findings"].as_array().unwrap();
        assert!(!kept.is_empty() && kept.len() < 4000);
        assert_eq!(
            kept[..],
            verdict["findings"].as_array().unwrap()[..kept.len()]
        );
        // The cut is recorded beside the verdict, which has no room for a marker.
        assert!(record.expect("recorded").elements_dropped > 0);
        assert_eq!(cut.detail_json, None);
    }

    #[test]
    fn an_oversized_evidence_array_stays_an_array() {
        let text = serde_json::to_string(
            &(0..3000)
                .map(|n| json!({ "exit": n, "stderr": "boom" }))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(text.len() > ROW_FIELD_CAP_BYTES);
        let (cut, record) = bound_evidence_json(&text, ROW_FIELD_CAP_BYTES);
        assert!(cut.len() <= ROW_FIELD_CAP_BYTES, "{}", cut.len());
        let value: Value = serde_json::from_str(&cut).unwrap();
        let items = value.as_array().expect("still an array");
        // Nothing is added: every element is one of the originals, in order.
        assert!(items
            .iter()
            .enumerate()
            .all(|(n, item)| *item == json!({ "exit": n, "stderr": "boom" })));
        let record = record.expect("the cut is recorded");
        assert_eq!(
            (record.kept, record.total, record.bytes),
            (items.len(), 3000, text.len())
        );
        // An array of strings is cut the same way.
        let strings = serde_json::to_string(&vec!["x".repeat(40); 2000]).unwrap();
        let (cut, _) = bound_evidence_json(&strings, 4096);
        let value: Value = serde_json::from_str(&cut).unwrap();
        assert!(value
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item == &json!("x".repeat(40))));
        // Small arrays are returned exactly, with no record.
        assert_eq!(
            bound_evidence_json("[1,2]", 4096),
            ("[1,2]".to_string(), None)
        );
    }

    #[test]
    fn the_two_cut_suffixes_are_kept_because_the_append_one_is_located_by_length() {
        // The genuine-record check finds the append cut by `at + suffix.len() ==
        // object.len()`, so that suffix is fixed. The read-side suffix carries a
        // removed-byte count and so has no fixed length; a read-cut text can
        // never satisfy the check, which is what lets the two coexist.
        let (append_cut, cap) = cap_object("x".repeat(OBJECT_CAP_BYTES + 500));
        let detail = json!({ OBJECT_TRUNCATED_KEY: cap.unwrap().as_value() }).to_string();
        assert!(recorded_cut_is_genuine(&append_cut, Some(&detail)));
        assert!(append_cut.ends_with("… [truncated]"));
        let read_cut = cut_text(&"x".repeat(OBJECT_CAP_BYTES + 500), 300);
        assert!(read_cut.contains("…[truncated ") && read_cut.ends_with(" bytes]"));
        assert!(!recorded_cut_is_genuine(&read_cut, Some(&detail)));
    }

    #[test]
    fn the_object_cap_helper_cuts_only_past_the_cap() {
        let (kept, cap) = cap_object("a".repeat(OBJECT_CAP_BYTES));
        assert_eq!(kept.len(), OBJECT_CAP_BYTES);
        assert!(cap.is_none());
        let (kept, cap) = cap_object("a".repeat(OBJECT_CAP_BYTES + 1));
        let cap = cap.expect("a cut");
        assert!(kept.len() <= OBJECT_CAP_BYTES && kept.ends_with(OBJECT_CUT_SUFFIX));
        assert!(cap.is_well_formed());
        assert_eq!(cap.bytes, OBJECT_CAP_BYTES + 1);
        let forged = ObjectCap {
            hash: "fnv1a64:\",\"x\":\"".into(),
            ..cap.clone()
        };
        assert!(!forged.is_well_formed());
        let past = ObjectCap {
            at: OBJECT_CAP_BYTES + 1,
            ..cap
        };
        assert!(!past.is_well_formed());
    }
}

#[cfg(test)]
#[path = "adversarial_bounds_tests.rs"]
mod adversarial_bounds;
