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

use serde_json::{json, Map, Value};

use super::LedgerEvent;

/// Longest `object` the ledger stores, in bytes.
pub(super) const OBJECT_CAP_BYTES: usize = 2048;

/// What a cut `object` ends with, inside the cap.
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
    let at = redacted.floor_char_boundary(OBJECT_CAP_BYTES - OBJECT_CUT_SUFFIX.len());
    let cap = ObjectCap {
        at,
        bytes: redacted.len(),
        hash: format!("fnv1a64:{:016x}", devmap_extract::content_hash(&redacted)),
    };
    let mut kept = String::with_capacity(OBJECT_CAP_BYTES);
    kept.push_str(&redacted[..at]);
    kept.push_str(OBJECT_CUT_SUFFIX);
    (kept, Some(cap))
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

/// `verdict_json` with only its long string values cut.
///
/// The verdict contract admits no member the gate did not write, so nothing is
/// added, removed or retyped: a long string stays a string, shorter, ending
/// with how much was removed.
fn bound_verdict_json(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    fn cut_strings(value: &mut Value) {
        match value {
            Value::String(text) if text.len() > MEMBER_CAP_BYTES * 2 => {
                *text = cut_text(text, MEMBER_CAP_BYTES * 2);
            }
            Value::Array(items) => items.iter_mut().for_each(cut_strings),
            Value::Object(members) => members.values_mut().for_each(cut_strings),
            _ => {}
        }
    }
    match serde_json::from_str::<Value>(text) {
        Ok(mut value) => {
            cut_strings(&mut value);
            value.to_string()
        }
        Err(_) => cut_text(text, cap),
    }
}

impl LedgerEvent {
    /// This event with every field that can be large cut for a page.
    ///
    /// A cut field stays the kind of thing its consumers parse: `detail_json`
    /// an object, `argv_json` an array, `verdict_json` the verdict with shorter
    /// strings. The row keeps its shape and its fields; only their length
    /// changes, and each cut says so inside the value.
    pub fn bounded(mut self) -> Self {
        self.object = self
            .object
            .map(|object| cut_text(&object, ROW_OBJECT_CAP_BYTES));
        self.detail_json = self
            .detail_json
            .map(|text| bound_json_text(&text, ROW_FIELD_CAP_BYTES));
        self.argv_json = self
            .argv_json
            .map(|text| bound_argv_json(&text, ROW_FIELD_CAP_BYTES));
        self.verdict_json = self
            .verdict_json
            .map(|text| bound_verdict_json(&text, ROW_FIELD_CAP_BYTES));
        self
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
