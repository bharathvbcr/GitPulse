use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub schema_version: u32,
    pub id: String,
    pub criteria: Vec<Criterion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Criterion {
    pub id: String,
    pub fact: String,
    pub required: bool,
    pub predicate: Predicate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Predicate {
    Exists,
    Equals { value: Value },
    NotEquals { value: Value },
    Contains { value: Value },
    Minimum { value: Value },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    pub schema_version: u32,
    pub run_id: String,
    pub session_id: String,
    pub epoch: u64,
    /// Ordered coordinator-acknowledged worker epochs after run admission.
    #[serde(default)]
    pub epoch_transitions: Vec<EpochTransition>,
    pub contract_sha256: String,
    pub capability_sha256: String,
    /// SHA-256 of the reviewed policy document admitted with the run, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_sha256: Option<String>,
    /// Terminal business or success outcome concluded by the capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    pub journal_complete: bool,
    pub degraded: Vec<String>,
    pub actions: Vec<Action>,
    pub observations: Vec<Observation>,
    pub artifacts: Vec<Artifact>,
    /// Typed intervention/handoff requests emitted while automation was stuck.
    #[serde(default)]
    pub interventions: Vec<Intervention>,
    /// Human inputs taken during an intervention window.
    #[serde(default)]
    pub human_actions: Vec<HumanAction>,
    /// Declared recoveries that matched and were applied during the run.
    #[serde(default)]
    pub recoveries: Vec<RecoveryApplied>,
    /// Locator ladder resolution hits (strategy_index > 0 is drift).
    #[serde(default)]
    pub locator_hits: Vec<LocatorHit>,
}

/// Concluded capability outcome. `kind` mirrors Manvi workflow outcomes:
/// `success` for completed goals, `business` for declared non-success terminals
/// such as `not_found` (never recorded as a hard failure).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    pub id: String,
    pub kind: OutcomeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    Success,
    Business,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Intervention {
    pub id: String,
    pub sequence: u64,
    pub reason_code: String,
    pub status: InterventionStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterventionStatus {
    Requested,
    Returned,
    Abandoned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanAction {
    pub id: String,
    pub sequence: u64,
    pub kind: String,
    pub intervention_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryApplied {
    pub id: String,
    pub sequence: u64,
    pub step_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocatorHit {
    pub target: String,
    pub strategy_index: u64,
    pub sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpochTransition {
    pub sequence: u64,
    pub from_epoch: u64,
    pub to_epoch: u64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub id: String,
    pub sequence: u64,
    pub disposition: Disposition,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Succeeded,
    NotDispatched,
    Failed,
    Unknown,
    Cancelled,
    Denied,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub sequence: u64,
    pub run_id: String,
    pub session_id: String,
    pub epoch: u64,
    pub after_action_id: String,
    pub facts: BTreeMap<String, Value>,
    pub artifact_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub id: String,
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

/// Trusted run admission values supplied separately from the evidence bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedRun {
    pub run_id: String,
    pub session_id: String,
    pub epoch: u64,
    pub contract_sha256: String,
    pub capability_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Passed,
    Failed,
    Incomplete,
}

impl Verdict {
    pub(crate) fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Failed, _) | (_, Self::Failed) => Self::Failed,
            (Self::Incomplete, _) | (_, Self::Incomplete) => Self::Incomplete,
            _ => Self::Passed,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CriterionResult {
    pub id: String,
    pub required: bool,
    pub verdict: Verdict,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Issue {
    pub code: String,
    pub verdict: Verdict,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub ok: bool,
    pub evidence_schema_version: u32,
    pub verdict: Verdict,
    pub contract_sha256: String,
    pub bundle_sha256: String,
    pub run_id: String,
    pub criteria: Vec<CriterionResult>,
    pub issues: Vec<Issue>,
}

impl Report {
    pub(crate) fn issue(&mut self, code: &str, verdict: Verdict) {
        if !self.issues.iter().any(|i| i.code == code) {
            self.issues.push(Issue {
                code: code.into(),
                verdict,
            });
        }
        self.verdict = self.verdict.combine(verdict);
    }
}
