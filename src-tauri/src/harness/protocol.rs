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
pub const OP_LOCAL_SCAN: &str = "local.scan";
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
    /// Coding agents the running harness has a *managed* adapter for.
    ///
    /// `None` means this build did not say — every Manvi that predates the
    /// field omits it — and must never be read as "it has none". `ops` cannot
    /// stand in: `work.runs.managed.prepare` is registered whenever a managed
    /// runner exists, so a codex-only harness and a codex+claude harness
    /// advertise the identical operation.
    #[serde(default)]
    pub managed_providers: Option<Vec<String>>,
}

/// What the installed harness says about its adapter for one provider.
///
/// Three outcomes, never two. Collapsing `Unknown` into `Absent` would let a
/// harness that was never asked refuse a lane that works, and collapsing it
/// into `Present` would put the repository's single run slot behind a launch
/// that cannot start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedAdapter {
    /// The harness published its adapter set and this provider is in it.
    Present,
    /// The harness published its adapter set and this provider is not in it.
    /// `published` is what it did list, for an error that names the truth.
    Absent { published: Vec<String> },
    /// The harness published no adapter set, so nothing was verified.
    /// `reason` says why, in the harness's own terms where there is one.
    Unknown { reason: String },
}

impl HelloResult {
    /// Classifies one provider against this handshake. The only place that
    /// decision is made; callers branch on the verdict rather than re-reading
    /// `managed_providers` and inventing a fourth opinion about it.
    pub fn managed_adapter(&self, provider: &str) -> ManagedAdapter {
        match &self.managed_providers {
            Some(published) if published.iter().any(|p| p == provider) => ManagedAdapter::Present,
            Some(published) => ManagedAdapter::Absent {
                published: published.clone(),
            },
            None => ManagedAdapter::Unknown {
                reason: "this Manvi build does not report which managed adapters it has".into(),
            },
        }
    }
}

#[cfg(test)]
mod managed_adapter_tests {
    use super::*;

    fn hello(managed: Option<&[&str]>) -> HelloResult {
        HelloResult {
            protocol: 1,
            ops: vec!["work.runs.managed.prepare".into()],
            posture: "host".into(),
            managed_providers: managed
                .map(|list| list.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>()),
        }
    }

    /// The three verdicts must stay three. A harness that was never asked and
    /// a harness that answered "none" look identical the moment they are
    /// merged, and merging them breaks in opposite directions: fold Unknown
    /// into Absent and every older build loses a managed lane that works; fold
    /// it into Present and the repository's one run slot goes to a launch that
    /// cannot start.
    #[test]
    fn an_unreported_adapter_set_is_neither_present_nor_absent() {
        assert_eq!(
            hello(Some(&["codex", "claude"])).managed_adapter("claude"),
            ManagedAdapter::Present
        );
        assert_eq!(
            hello(Some(&["codex"])).managed_adapter("claude"),
            ManagedAdapter::Absent {
                published: vec!["codex".into()]
            },
        );
        // The empty list is a real answer — "a managed lane with no adapters"
        // — and must not read as silence.
        assert_eq!(
            hello(Some(&[])).managed_adapter("claude"),
            ManagedAdapter::Absent { published: vec![] },
        );
        let silent = hello(None).managed_adapter("claude");
        assert!(
            matches!(&silent, ManagedAdapter::Unknown { reason } if reason.contains("does not report")),
            "a build that published nothing must say so, not answer for it: {silent:?}"
        );
    }

    /// The op list was the obvious place to look and cannot answer: a
    /// codex-only build and a codex+claude build register the identical
    /// operation. This is the whole reason the adapter set travels separately.
    #[test]
    fn serving_the_managed_op_says_nothing_about_which_providers_it_drives() {
        let codex_only = hello(Some(&["codex"]));
        let both = hello(Some(&["codex", "claude"]));
        assert_eq!(codex_only.ops, both.ops);
        assert_ne!(
            codex_only.managed_adapter("claude"),
            both.managed_adapter("claude")
        );
    }

    /// Hostile and malformed handshakes, because this field decides whether a
    /// launch is offered and the reply is one line of NDJSON from a child
    /// process that may be any version, corrupted, or half-written.
    #[test]
    fn a_malformed_adapter_set_never_silently_becomes_an_answer() {
        // `null` is silence, not "no adapters". A harness that sends it has
        // told us nothing, and Unknown is the only honest reading.
        let null: HelloResult = serde_json::from_str(
            r#"{"protocol":1,"ops":[],"posture":"host","managed_providers":null}"#,
        )
        .unwrap();
        assert!(matches!(
            null.managed_adapter("codex"),
            ManagedAdapter::Unknown { .. }
        ));

        // Wrong types are refused outright rather than coerced. A hello this
        // build cannot read is a protocol fault, and guessing at it is how a
        // gate comes to pass on a payload nobody understood. Loud beats quiet:
        // the caller already treats a failed handshake as "not verified".
        for hostile in [
            r#"{"protocol":1,"ops":[],"posture":"host","managed_providers":"codex"}"#,
            r#"{"protocol":1,"ops":[],"posture":"host","managed_providers":[1,2]}"#,
            r#"{"protocol":1,"ops":[],"posture":"host","managed_providers":{"codex":true}}"#,
            r#"{"protocol":1,"ops":[],"posture":"host","managed_providers":[null]}"#,
        ] {
            assert!(
                serde_json::from_str::<HelloResult>(hostile).is_err(),
                "a hello this build cannot read was accepted: {hostile}"
            );
        }

        // A field this build has never heard of is ignored, so a newer harness
        // does not break an older host. Only a *type* change on a field we do
        // read is a break, which is the case above.
        let newer: HelloResult = serde_json::from_str(
            r#"{"protocol":1,"ops":[],"posture":"host","managed_providers":["codex"],"future":{"x":1}}"#,
        )
        .unwrap();
        assert_eq!(newer.managed_adapter("codex"), ManagedAdapter::Present);

        // Adversarial-but-well-typed content stays data: no provider matches
        // by prefix, case, whitespace or substring, because a launch routed by
        // a near-miss would reach an adapter written for someone else.
        let tricky = hello(Some(&["CODEX", "codex ", " codex", "codexx", "cod"]));
        assert!(matches!(
            tricky.managed_adapter("codex"),
            ManagedAdapter::Absent { .. }
        ));
        assert_eq!(
            hello(Some(&["codex"])).managed_adapter("codex"),
            ManagedAdapter::Present
        );
        // Including the empty provider name, which is what an absent field in
        // a caller's own payload would degrade to.
        assert!(matches!(
            hello(Some(&["codex"])).managed_adapter(""),
            ManagedAdapter::Absent { .. }
        ));
    }

    /// A large or duplicated adapter set is carried, not truncated, at the
    /// decision boundary — the bound belongs where it is *rendered*, so a
    /// caller asking "is claude present?" still gets the true answer.
    #[test]
    fn a_large_adapter_set_still_answers_precisely() {
        let mut many: Vec<String> = (0..5_000).map(|i| format!("provider-{i}")).collect();
        many.push("claude".into());
        many.extend(std::iter::repeat_n("codex".to_string(), 100));
        let hello = HelloResult {
            protocol: 1,
            ops: vec![],
            posture: "host".into(),
            managed_providers: Some(many),
        };
        assert_eq!(hello.managed_adapter("claude"), ManagedAdapter::Present);
        assert_eq!(hello.managed_adapter("codex"), ManagedAdapter::Present);
        assert!(matches!(
            hello.managed_adapter("grok"),
            ManagedAdapter::Absent { .. }
        ));
    }

    /// A field absent from the wire decodes as Unknown rather than defaulting
    /// to an empty list — `#[serde(default)]` on a `Vec` would have produced
    /// exactly the collapse the type exists to prevent.
    #[test]
    fn an_older_harnesss_handshake_decodes_as_unknown() {
        let old: HelloResult =
            serde_json::from_str(r#"{"protocol":1,"ops":["hello"],"posture":"host"}"#).unwrap();
        assert!(matches!(
            old.managed_adapter("codex"),
            ManagedAdapter::Unknown { .. }
        ));
        let new: HelloResult = serde_json::from_str(
            r#"{"protocol":1,"ops":["hello"],"posture":"host","managed_providers":["codex"]}"#,
        )
        .unwrap();
        assert_eq!(new.managed_adapter("codex"), ManagedAdapter::Present);
    }
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
    /// Non-empty when an override grant cleared a soft block. The harness
    /// reports `allow`, but a granted allow is not a clean pass either: the
    /// rule fired and a human waived it.
    #[serde(default)]
    pub grant_id: String,
    /// Who issued the grant named by `grant_id`.
    #[serde(default)]
    pub granted_by: String,
    /// Non-empty when the write was authorised by scope the *executor appended
    /// to its own task*, rather than by the plan the task was created with.
    ///
    /// Reported apart from `grant_id` because a grant expires and appended
    /// scope does not. Without this field every later write against that scope
    /// reports as an ordinary planned write, in this run and every run after
    /// it — which is exactly how a self-granted widening launders into a plan.
    #[serde(default)]
    pub widened: String,
    /// Names checks that could not run. A decision reached without the repo
    /// map is not the same decision as one reached with it.
    #[serde(default)]
    pub degraded: Vec<String>,
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

/// One model a local server reports, from `local.scan`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ScanModel {
    #[serde(default)]
    pub id: String,
    /// Zero when the server reported no window. Zero is *unreported*, not
    /// "no context"; `context_window_source` says which.
    #[serde(default)]
    pub context_window: i64,
    #[serde(default)]
    pub context_window_source: String,
    /// A window the scanner read off the server and refused as implausible.
    /// Non-zero only when it was, so the refusal is visible rather than silent.
    #[serde(default)]
    pub implausible_window: i64,
    /// Whether the three capability flags below mean anything.
    ///
    /// Without this, "does not support tools" and "nobody asked" are the same
    /// `false`, and the UI renders a capable model as incapable — or offers a
    /// tool-calling feature on one that cannot, and the failure looks like the
    /// user's configuration.
    #[serde(default)]
    pub capabilities_known: bool,
    #[serde(default)]
    pub supports_tools: bool,
    #[serde(default)]
    pub supports_reasoning: bool,
    #[serde(default)]
    pub supports_vision: bool,
    /// Whether the model generates text at all. An embedding model answers the
    /// same listing as every chat model.
    #[serde(default)]
    pub supports_completion: bool,
}

/// One local model server that answered a scan.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ScanServer {
    #[serde(default)]
    pub base_url: String,
    /// How the server identified itself. `openai-compatible` means it answered
    /// `/v1/models` and nothing else the harness knows to ask — a working
    /// server, and more honest than naming a runtime from the port number.
    #[serde(default)]
    pub runtime: String,
    /// Only Ollama reports one. Never load-bearing.
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub models: Vec<ScanModel>,
}

/// What one discovery sweep found.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ScanResult {
    #[serde(default)]
    pub servers: Vec<ScanServer>,
    /// How many endpoints were probed. "Nothing is running" and "we only
    /// looked in one place" are different answers, and this is the difference.
    #[serde(default)]
    pub scanned: i64,
    /// Whether per-model capabilities were asked for at all.
    #[serde(default)]
    pub capabilities: bool,
}
