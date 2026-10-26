//! Reading a `notify` payload for the few fields it uses.
//!
//! A `PostToolUse` payload carries the tool's whole `tool_response`: a `cat` of
//! a large file, a test log. `notify` uses a handful of top-level fields and
//! never the response, so it has no reason to refuse a payload because the
//! response is large. Every other subcommand judges the call from the payload
//! and stays on [`super::read_input`] and its 4 MiB limit.
//!
//! Accept and reject are the old path's (`parse_input`): the bytes must be
//! UTF-8, `str::trim` is stripped from both ends, and one JSON object must fill
//! the rest, to the same nesting limit of 128. Only the 4 MiB refusal is
//! gone, because nothing here is built from the whole payload: fields
//! `HookInput::from_value` reads are kept and everything else is walked and
//! dropped. A skipped field is still validated (a number out of range or a
//! string with a bad escape refuses the payload as it always did); it is only
//! not built, which is what makes it linear and allocation-free. What is kept
//! is then read by `from_value`, the one owner of what each field means.
//!
//! The bytes are read first, bounded by [`MAX_READ_BYTES`], and the walk is
//! over the slice rather than the pipe: `serde_json`'s reader walk takes the
//! bytes one at a time, which measured at twice the cost of the whole process
//! at 4 MiB (see `docs/PERFORMANCE.md`).
//!
//! Two bounds, and they differ on purpose:
//!
//! * Past [`MAX_READ_BYTES`] nothing more is read and the report goes with no
//!   fields, which is an empty-message report: the session and the event are
//!   still true, and they are what state and resolution need.
//! * Malformed JSON is not that. A payload that does not parse says nothing
//!   about what the host meant, so nothing is reported, as before.
//!
//! The read is also bounded in time, by a share of the timeout the event is
//! run with ([`budgets_for`]), so that the read and the socket report together
//! end a margin before the host would kill the hook.

use super::{within_budget, HookInput, NOTIFY_BUDGET, SESSION_END_NOTIFY_BUDGET};
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use std::io::Read;
use std::time::Duration;

/// The `timeout` the plugin manifest gives every `notify` hook, in the host's
/// terms: when Claude Code kills the hook, and with it any report not yet
/// delivered. The one owner for every event but `session_ended`; the manifest
/// must say it (a test reads both).
#[cfg(test)]
pub const NOTIFY_HOST_TIMEOUT: Duration = Duration::from_secs(5);

/// The `timeout` of `SessionEnd`'s `notify`, in both copies: the plugin
/// manifest's and the one injected (`workbench::claude_hook`). Claude Code
/// raises its 1.5 s exit budget to any longer per-hook `SessionEnd` timeout, so
/// neither may ask for more than the default. A test reads the manifest and
/// `plugin-contract.test.ts` checks it.
pub const SESSION_END_HOST_TIMEOUT: Duration = Duration::from_secs(1);

/// The host timeout a `notify` for `event` must finish inside.
#[cfg(test)]
pub fn host_timeout(event: &str) -> Duration {
    if event == "session_ended" {
        SESSION_END_HOST_TIMEOUT
    } else {
        NOTIFY_HOST_TIMEOUT
    }
}

/// The read budget under [`NOTIFY_HOST_TIMEOUT`].
#[cfg(test)]
pub const NOTIFY_READ_BUDGET: Duration = NOTIFY_BUDGET;

/// What the read and, separately, the socket report may each take for a
/// `notify` of `event`: one plain constant per class of event, declared next to
/// [`NOTIFY_BUDGET`] so `scripts/plugin-contract.test.ts` reads the same two
/// numbers and checks every `hooks.json` entry against the one for its event.
/// The run is the two together, over a margin before the host's timeout.
pub fn budgets_for(event: &str) -> (Duration, Duration) {
    let share = match event {
        "session_ended" => SESSION_END_NOTIFY_BUDGET,
        _ => NOTIFY_BUDGET,
    };
    (share, share)
}

/// The most of one payload that is read. Four times the limit the other
/// subcommands hold, because only what `notify` keeps is built.
pub const MAX_READ_BYTES: u64 = 16 * 1024 * 1024;

/// The top-level fields `HookInput::from_value` reads. A field added there and
/// not here is read as empty; `the_walk_reads_what_a_whole_parse_reads` fails
/// on it.
pub(super) const KEPT_FIELDS: [&str; 16] = [
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
    "permission_mode",
    "permissionMode",
];

/// A value that is checked and not built.
///
/// `IgnoredAny` would skip without checking, so a number out of range or a
/// malformed escape inside a skipped field would be accepted where the whole
/// parse refused it. This goes through the same `deserialize_any` the whole
/// parse does, so the same checks (and the same nesting limit) apply, and
/// builds nothing.
struct Skipped;

impl<'de> Deserialize<'de> for Skipped {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(Skipped)
    }
}

impl<'de> Visitor<'de> for Skipped {
    type Value = Skipped;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_bool<E>(self, _: bool) -> Result<Skipped, E> {
        Ok(Skipped)
    }
    fn visit_i64<E>(self, _: i64) -> Result<Skipped, E> {
        Ok(Skipped)
    }
    fn visit_u64<E>(self, _: u64) -> Result<Skipped, E> {
        Ok(Skipped)
    }
    fn visit_f64<E>(self, _: f64) -> Result<Skipped, E> {
        Ok(Skipped)
    }
    fn visit_str<E>(self, _: &str) -> Result<Skipped, E> {
        Ok(Skipped)
    }
    fn visit_unit<E>(self) -> Result<Skipped, E> {
        Ok(Skipped)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Skipped, A::Error> {
        while seq.next_element::<Skipped>()?.is_some() {}
        Ok(Skipped)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Skipped, A::Error> {
        while map.next_entry::<Skipped, Skipped>()?.is_some() {}
        Ok(Skipped)
    }
}

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
                map.next_value::<Skipped>()?;
            }
        }
        Ok(Value::Object(kept))
    }
}

/// The fields `notify` uses, out of `bytes`, accepting and refusing what the
/// whole-document parse did.
fn kept_fields(bytes: &[u8]) -> Result<HookInput, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!("hook stdin is not UTF-8: {e}"))?;
    let mut de = serde_json::Deserializer::from_str(text.trim());
    let value = de
        .deserialize_map(Kept)
        .and_then(|value| de.end().map(|()| value))
        .map_err(|e| format!("hook stdin is not valid JSON: {e}"))?;
    Ok(HookInput::from_value(&value))
}

/// [`read_notify_input_within`] under [`NOTIFY_READ_BUDGET`].
#[cfg(test)]
pub fn read_notify_input(
    reader: impl Read + Send + 'static,
    limit: u64,
) -> Result<HookInput, String> {
    read_notify_input_within(reader, limit, NOTIFY_READ_BUDGET)
}

/// Reads the fields `notify` uses from `reader`, within `budget`, reading at
/// most `limit` bytes.
///
/// `Ok` with an empty input when the payload is longer than `limit`. `Err` for
/// one that is not a JSON object, cannot be read, or did not arrive in time.
pub fn read_notify_input_within(
    reader: impl Read + Send + 'static,
    limit: u64,
    budget: Duration,
) -> Result<HookInput, String> {
    within_budget(budget, move || {
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
