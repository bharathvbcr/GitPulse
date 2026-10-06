//! The one request GitPulse sends: `gitpulse.commit_type`.
//!
//! The shape is pinned by `docs/caller-contract.md` §4 in Lappi-decision. Task,
//! question and slot name are rendered into the model's prompt, so a renamed
//! one would serve a prompt the model never saw; the option list is
//! [`crate::ai::commit_brief::KNOWN_TYPES`], in its order, and nowhere else.
//! No release trains this family yet, so today every request is refused
//! `task_not_trained`; that refusal is what the wiring shows until one does.

use base64::Engine as _;
use serde::Serialize;

use super::client::{NotAskedReason, MAX_PAYLOAD_BYTES, SCHEMA_VERSION};
use crate::ai::commit_brief::KNOWN_TYPES;

pub const APP: &str = "gitpulse";
pub const COMMIT_TYPE_TASK: &str = "gitpulse.commit_type";
pub const COMMIT_TYPE_QUESTION: &str =
    "Which conventional commit type describes this staged change?";
pub const COMMIT_TYPE_SLOT: &str = "commit_type";

/// Field order is the order `docs/schema-api.md` shows a request in. A derived
/// struct keeps it whatever features `serde_json` was built with.
#[derive(Serialize)]
struct Request<'a> {
    schema_version: u64,
    task: &'static str,
    context_b64: String,
    context_len: usize,
    question: &'static str,
    slots: [ChoiceSlot<'a>; 1],
    route: &'static str,
    example_id: String,
    metadata: serde_json::Map<String, serde_json::Value>,
}

#[derive(Serialize)]
struct ChoiceSlot<'a> {
    name: &'static str,
    #[serde(rename = "type")]
    kind: &'static str,
    options: &'a [&'a str],
}

/// The request line for `diff` (the staged unified diff, as bytes), or why it
/// is not sent.
///
/// `context_b64` is standard, padded base64 of exactly these bytes and
/// `context_len` is their count, so a payload truncated in transit that still
/// decodes is refused by the agent rather than answered. A line over the
/// agent's cap is not built for sending at all.
pub fn commit_type_request(diff: &[u8], record_id: &str) -> Result<Vec<u8>, NotAskedReason> {
    let request = Request {
        schema_version: SCHEMA_VERSION,
        task: COMMIT_TYPE_TASK,
        context_b64: base64::engine::general_purpose::STANDARD.encode(diff),
        context_len: diff.len(),
        question: COMMIT_TYPE_QUESTION,
        slots: [ChoiceSlot {
            name: COMMIT_TYPE_SLOT,
            kind: "choice",
            options: KNOWN_TYPES,
        }],
        route: "generic",
        example_id: format!("{APP}:{COMMIT_TYPE_TASK}:{record_id}"),
        metadata: serde_json::Map::new(),
    };
    // Serialising a struct of strings, integers and an empty map does not fail;
    // were it ever to, the request is not sent rather than sent malformed.
    let line = serde_json::to_vec(&request).map_err(|_| NotAskedReason::RequestNotBuilt)?;
    if line.len() > MAX_PAYLOAD_BYTES {
        return Err(NotAskedReason::OverPayloadCap);
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_is_exactly_the_pinned_shape() {
        let line = commit_type_request(b"diff --git a/x b/x\n", "0123456789abcdef0123456789abcdef")
            .expect("a small diff is sent");
        let text = String::from_utf8(line).expect("a JSON line is UTF-8");
        assert_eq!(
            text,
            concat!(
                r#"{"schema_version":1,"task":"gitpulse.commit_type","#,
                r#""context_b64":"ZGlmZiAtLWdpdCBhL3ggYi94Cg==","context_len":19,"#,
                r#""question":"Which conventional commit type describes this staged change?","#,
                r#""slots":[{"name":"commit_type","type":"choice","options":["feat","fix","refactor","docs","test","chore","perf","build","ci","style","revert"]}],"#,
                r#""route":"generic","example_id":"gitpulse:gitpulse.commit_type:0123456789abcdef0123456789abcdef","metadata":{}}"#
            )
        );
        assert!(!text.contains('\n'), "a request is one line");
    }

    #[test]
    fn context_is_the_bytes_not_a_reencoded_string() {
        // Invalid UTF-8 and a NUL survive byte-exact, and the length counts bytes.
        let bytes = [0xff_u8, 0x00, b'a', 0xc3];
        let line = commit_type_request(&bytes, "00000000000000000000000000000000")
            .expect("small context is sent");
        let value: serde_json::Value = serde_json::from_slice(&line).expect("valid JSON");
        let encoded = value["context_b64"].as_str().expect("a string");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("canonical base64");
        assert_eq!(decoded, bytes);
        assert_eq!(value["context_len"], 4);
        assert_eq!(encoded.len() % 4, 0, "padded");
    }

    #[test]
    fn a_line_over_the_cap_is_not_asked() {
        // Base64 inflates by 4/3: this many bytes cannot fit under the cap.
        let diff = vec![b'+'; MAX_PAYLOAD_BYTES / 4 * 3 + 1];
        assert_eq!(
            commit_type_request(&diff, "00000000000000000000000000000000"),
            Err(NotAskedReason::OverPayloadCap)
        );
    }

    #[test]
    fn the_option_list_is_the_classifiers_own() {
        assert_eq!(
            KNOWN_TYPES,
            &[
                "feat", "fix", "refactor", "docs", "test", "chore", "perf", "build", "ci", "style",
                "revert"
            ],
            "caller-contract §4 pins this order; changing it changes the served prompt"
        );
    }
}
