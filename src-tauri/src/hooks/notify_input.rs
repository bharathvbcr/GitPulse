//! Reading a `notify` payload for the few fields it uses.
//!
//! A `PostToolUse` payload carries the tool's whole `tool_response`: a `cat` of
//! a large file, a test log. `notify` uses a handful of top-level fields and
//! never the response, so it has no reason to refuse a payload because the
//! response is large. Every other subcommand judges the call from the payload
//! and stays on [`super::read_input`] and its 4 MiB limit.
//!
//! The payload is read once, bounded by [`MAX_READ_BYTES`], and walked as a
//! stream of tokens: fields `HookInput::from_value` reads are kept, everything
//! else is skipped where it lies and never built into a value. What is kept is
//! then read by `from_value`, the one owner of what each field means.
//!
//! Why the bytes are read first and the walk is over the slice rather than over
//! the pipe: `serde_json`'s reader walk takes the bytes one at a time, which
//! measured at twice the cost of the whole process at 4 MiB (see
//! `docs/PERFORMANCE.md`), where the slice walk costs nothing a process start
//! does not.
//!
//! Two bounds, and they differ on purpose:
//!
//! * Past [`MAX_READ_BYTES`] nothing more is read and the report goes with no
//!   fields, which is an empty-message report: the session and the event are
//!   still true, and they are what state and resolution need.
//! * Malformed JSON is not that. A payload that does not parse says nothing
//!   about what the host meant, so nothing is reported, as before.

use super::{within_budget, HookInput, BUDGET};
use serde::de::{Deserializer as _, IgnoredAny, MapAccess, Visitor};
use serde_json::Value;
use std::io::Read;

/// The most of one payload that is read. Four times the limit the other
/// subcommands hold, because only what `notify` keeps is built.
pub const MAX_READ_BYTES: u64 = 16 * 1024 * 1024;

/// The top-level fields `HookInput::from_value` reads. A field added there and
/// not here is read as empty; `the_walk_reads_what_a_whole_parse_reads` fails
/// on it.
pub(super) const KEPT_FIELDS: [&str; 14] = [
    "hook_event_name",
    "session_id",
    "cwd",
    "workspace_roots",
    "workspaceRoots",
    "tool_name",
    "tool_input",
    "source",
    "cursor_version",
    "message",
    "agent_id",
    "last_assistant_message",
    "error",
    "tool_use_id",
];

/// The kept fields of one JSON object, in a `Value`.
struct Kept;

impl<'de> Visitor<'de> for Kept {
    type Value = Value;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut kept = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if KEPT_FIELDS.contains(&key.as_str()) {
                kept.insert(key, map.next_value::<Value>()?);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(Value::Object(kept))
    }
}

/// The fields `notify` uses, out of `bytes`: one JSON object, anything after it
/// ignored.
fn kept_fields(bytes: &[u8]) -> Result<HookInput, String> {
    // Not `end()`: what follows the object is not this hook's business.
    serde_json::Deserializer::from_slice(bytes)
        .deserialize_map(Kept)
        .map(|value| HookInput::from_value(&value))
        .map_err(|error| format!("hook stdin is not a readable JSON object: {error}"))
}

/// Reads the fields `notify` uses from `reader`, within [`BUDGET`], reading at
/// most `limit` bytes.
///
/// `Ok` with an empty input when the payload is longer than `limit`. `Err` for
/// one that is not a JSON object, cannot be read, or did not arrive in time.
pub fn read_notify_input(
    reader: impl Read + Send + 'static,
    limit: u64,
) -> Result<HookInput, String> {
    within_budget(BUDGET, move || {
        let mut bytes = Vec::new();
        reader
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| format!("could not read hook stdin: {e}"))?;
        if bytes.len() as u64 > limit {
            log::warn!(
                target: "hook",
                "notify stdin is past {limit} bytes; reporting without its fields"
            );
            return Ok(HookInput::default());
        }
        kept_fields(&bytes)
    })
    .ok_or_else(|| "hook stdin deadline exceeded or reader could not run; no report".to_string())?
}
