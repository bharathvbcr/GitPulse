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
const MARKER_RESERVE_BYTES: usize = 512;

/// Names of cut members a `gitpulse_truncated` marker lists.
const MARKER_KEYS: usize = 64;

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
        if essential || (size <= MEMBER_CAP_BYTES && used + size <= room) {
            used += size;
            kept.insert(key, value);
        } else {
            cut_bytes += size;
            cut_keys.push(key);
        }
    }
    if !cut_keys.is_empty() {
        cut_keys.truncate(MARKER_KEYS);
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
