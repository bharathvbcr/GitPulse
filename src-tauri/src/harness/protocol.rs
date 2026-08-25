//! Wire types for the MANVI host plane (`manvi serve`).
//!
//! The transport is NDJSON in both directions: one request object per line on
//! the child's stdin, exactly one response object per line on its stdout. The
//! harness ignores unknown request fields and we ignore unknown result fields,
//! so a newer sidecar can add result fields without breaking this build.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Protocol major version this client was written against.
pub const PROTOCOL_VERSION: i64 = 1;

pub const OP_HELLO: &str = "hello";
pub const OP_POLICY_CHECK_FILE: &str = "policy.check.file";
pub const OP_POLICY_CHECK_COMMAND: &str = "policy.check.command";
pub const OP_CAPABILITY_PROBE: &str = "capability.probe";
pub const OP_CHAT_PREPARE: &str = "chat.prepare";
pub const OP_CHAT_SETTLE: &str = "chat.settle";
pub const OP_CHAT_FORGET: &str = "chat.forget";

#[derive(Debug, Clone, Serialize)]
pub struct Request {
    pub id: String,
    pub op: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Response {
    pub id: String,
    pub ok: bool,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<WireError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HelloResult {
    #[serde(default)]
    pub protocol: i64,
    #[serde(default)]
    pub ops: Vec<String>,
    #[serde(default)]
    pub posture: String,
}

/// One policy decision, exactly as the gate rendered it.
///
/// The field names mirror the harness's own decision record rather than the
/// prose in its docs: `action`/`severity`/`demoted` is what the wire carries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RawDecision {
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub rule: String,
    #[serde(default)]
    pub severity: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub task_id: String,
    /// Non-empty when a soft denial was demoted to an allow by the host
    /// posture. A demoted allow is not a clean pass and must not be shown as
    /// one.
    #[serde(default)]
    pub demoted: String,
}

/// A model's discovered dimensions, and where each answer came from.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProbeResult {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub context_window: i64,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub discovered: bool,
    #[serde(default)]
    pub describe: String,
    #[serde(default)]
    pub max_output_tokens: i64,
    #[serde(default)]
    pub capabilities_known: bool,
    #[serde(default)]
    pub supports_tools: bool,
    #[serde(default)]
    pub supports_vision: bool,
    #[serde(default)]
    pub supports_reasoning: bool,
    #[serde(default)]
    pub embedding: bool,
    #[serde(default)]
    pub served: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrepareResult {
    #[serde(default)]
    pub steps: Vec<PrepareStep>,
    #[serde(default)]
    pub before_tokens: i64,
    #[serde(default)]
    pub after_tokens: i64,
    #[serde(default)]
    pub threshold_tokens: i64,
    #[serde(default)]
    pub target_tokens: i64,
    /// True when compaction ran out of room: the request will overflow the
    /// server's window. Surfaced, never swallowed.
    #[serde(default)]
    pub insufficient: bool,
    #[serde(default)]
    pub calibration_ratio: f64,
    #[serde(default)]
    pub calibration_samples: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrepareStep {
    #[serde(default)]
    pub tool_call_id: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub from_bytes: i64,
    #[serde(default)]
    pub to_bytes: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SettleResult {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub reasoning: String,
    #[serde(default)]
    pub prefill_disproved: bool,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub reclassified: bool,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub truncated_mid_call: bool,
    #[serde(default)]
    pub retry_message: String,
}
