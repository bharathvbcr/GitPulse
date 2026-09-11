//! Pure acceptance checks over immutable bytes and trusted caller expectations.
//!
//! No filesystem, native UI, provider, process or database operations occur here.
//! Digests establish byte identity, not authenticity of an observation producer.
//! Callers own that trust boundary; replay proves recorded consistency only.

mod strict_json;
mod types;

pub use types::{
    Action, Artifact, Bundle, Contract, Criterion, CriterionResult, Disposition, EpochTransition,
    ExpectedRun, HumanAction, Intervention, InterventionStatus, Issue, LocatorHit, Observation,
    Outcome, OutcomeKind, Predicate, RecoveryApplied, Report, Verdict,
};

use serde::de::DeserializeOwned;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_CONTRACT_BYTES: usize = 1024 * 1024;
pub const MAX_BUNDLE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_TOTAL_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_ARTIFACTS: usize = 128;
pub const MAX_EVENTS: usize = 4096;
pub const MAX_CRITERIA: usize = 256;

/// Invalid protocol input is distinct from a evaluated failed/incomplete run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}

/// SHA-256 of exact bytes; never hash independently reserialized JSON.
pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn parse<T: DeserializeOwned>(bytes: &[u8], limit: usize) -> Result<T, Error> {
    if bytes.len() > limit {
        return Err(Error(format!("input exceeds {limit} byte limit")));
    }
    strict_json::validate(bytes).map_err(|e| Error(format!("invalid JSON: {e}")))?;
    serde_json::from_slice(bytes).map_err(|e| Error(format!("invalid protocol shape: {e}")))
}

pub fn parse_contract(bytes: &[u8]) -> Result<Contract, Error> {
    let contract: Contract = parse(bytes, MAX_CONTRACT_BYTES)?;
    if contract.schema_version != SCHEMA_VERSION {
        return Err(Error("unsupported contract schema_version".into()));
    }
    identifier(&contract.id)?;
    if contract.criteria.is_empty() || contract.criteria.len() > MAX_CRITERIA {
        return Err(Error("contract must contain 1..256 criteria".into()));
    }
    if !contract.criteria.iter().any(|c| c.required) {
        return Err(Error("contract must contain a required criterion".into()));
    }
    let mut ids = BTreeSet::new();
    for criterion in &contract.criteria {
        identifier(&criterion.id)?;
        identifier(&criterion.fact)?;
        if !ids.insert(&criterion.id) {
            return Err(Error("duplicate criterion id".into()));
        }
        match &criterion.predicate {
            Predicate::Minimum { value } if !safe_number(value) => {
                return Err(Error(
                    "minimum requires a finite number within the exact JSON integer range".into(),
                ));
            }
            Predicate::Contains { value } if !value.is_string() && !value.is_array() => {
                return Err(Error("contains requires a string or array".into()));
            }
            _ => {}
        }
    }
    Ok(contract)
}

pub fn parse_bundle(bytes: &[u8]) -> Result<Bundle, Error> {
    let bundle: Bundle = parse(bytes, MAX_BUNDLE_BYTES)?;
    if bundle.schema_version != SCHEMA_VERSION {
        return Err(Error("unsupported bundle schema_version".into()));
    }
    identifier(&bundle.run_id)?;
    identifier(&bundle.session_id)?;
    if bundle.epoch == 0 {
        return Err(Error("epoch must be positive".into()));
    }
    digest(&bundle.contract_sha256)?;
    digest(&bundle.capability_sha256)?;
    if let Some(policy) = &bundle.policy_sha256 {
        digest(policy)?;
    }
    if let Some(outcome) = &bundle.outcome {
        identifier(&outcome.id)?;
    }
    if bundle.actions.len() + bundle.observations.len() + bundle.epoch_transitions.len()
        > MAX_EVENTS
    {
        return Err(Error("journal exceeds 4096 event limit".into()));
    }
    if bundle.artifacts.len() > MAX_ARTIFACTS {
        return Err(Error("artifact count exceeds 128".into()));
    }
    if bundle.degraded.len() > MAX_CRITERIA {
        return Err(Error("degraded diagnostic count exceeds 256".into()));
    }
    if bundle.interventions.len() > MAX_CRITERIA
        || bundle.human_actions.len() > MAX_CRITERIA
        || bundle.recoveries.len() > MAX_CRITERIA
        || bundle.locator_hits.len() > MAX_CRITERIA
    {
        return Err(Error(
            "interventions, human_actions, recoveries, or locator_hits exceed 256".into(),
        ));
    }
    validate_side_records(&bundle)?;
    let mut ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let mut total_bytes = 0_u64;
    for artifact in &bundle.artifacts {
        identifier(&artifact.id)?;
        digest(&artifact.sha256)?;
        validate_artifact_path(&artifact.path)?;
        if !ids.insert(&artifact.id) || !paths.insert(&artifact.path) {
            return Err(Error("duplicate artifact id or path".into()));
        }
        if artifact.size_bytes > MAX_ARTIFACT_BYTES as u64 {
            return Err(Error("artifact exceeds 16 MiB limit".into()));
        }
        total_bytes += artifact.size_bytes;
    }
    if total_bytes > MAX_TOTAL_ARTIFACT_BYTES as u64 {
        return Err(Error("artifacts exceed 64 MiB aggregate limit".into()));
    }
    Ok(bundle)
}

fn validate_side_records(bundle: &Bundle) -> Result<(), Error> {
    let mut intervention_ids = BTreeSet::new();
    let mut human_ids = BTreeSet::new();
    let mut prior = 0_u64;
    for intervention in &bundle.interventions {
        identifier(&intervention.id)?;
        identifier(&intervention.reason_code)?;
        if intervention.sequence == 0 || intervention.sequence <= prior {
            return Err(Error(
                "intervention sequences must be positive and strictly increasing".into(),
            ));
        }
        if !intervention_ids.insert(intervention.id.as_str()) {
            return Err(Error("duplicate intervention id".into()));
        }
        prior = intervention.sequence;
    }
    prior = 0;
    for action in &bundle.human_actions {
        identifier(&action.id)?;
        identifier(&action.kind)?;
        identifier(&action.intervention_id)?;
        if action.sequence == 0 || action.sequence <= prior {
            return Err(Error(
                "human_action sequences must be positive and strictly increasing".into(),
            ));
        }
        if !human_ids.insert(action.id.as_str()) {
            return Err(Error("duplicate human_action id".into()));
        }
        if !intervention_ids.contains(action.intervention_id.as_str()) {
            return Err(Error(
                "human_action references unknown intervention_id".into(),
            ));
        }
        prior = action.sequence;
    }
    prior = 0;
    let mut recovery_keys = BTreeSet::new();
    for recovery in &bundle.recoveries {
        identifier(&recovery.id)?;
        identifier(&recovery.step_id)?;
        if recovery.sequence == 0 || recovery.sequence <= prior {
            return Err(Error(
                "recovery sequences must be positive and strictly increasing".into(),
            ));
        }
        if !recovery_keys.insert((recovery.id.as_str(), recovery.sequence)) {
            return Err(Error("duplicate recovery id/sequence".into()));
        }
        prior = recovery.sequence;
    }
    prior = 0;
    for hit in &bundle.locator_hits {
        identifier(&hit.target)?;
        if hit.sequence == 0 || hit.sequence <= prior {
            return Err(Error(
                "locator_hit sequences must be positive and strictly increasing".into(),
            ));
        }
        prior = hit.sequence;
    }
    Ok(())
}

/// Portable relative paths only. The CLI additionally rejects symlinks and
/// non-regular files. Paths have no URI, drive, parent or platform alias syntax.
pub fn validate_artifact_path(path: &str) -> Result<(), Error> {
    if path.is_empty() || path.len() > 1024 || path.contains(['\\', ':', '\0']) {
        return Err(Error("invalid artifact relative path".into()));
    }
    for component in path.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.ends_with(['.', ' '])
            || component.chars().any(char::is_control)
        {
            return Err(Error("invalid artifact path component".into()));
        }
        let stem = component
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit())
        {
            return Err(Error("reserved platform artifact path".into()));
        }
    }
    Ok(())
}

fn identifier(value: &str) -> Result<(), Error> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(Error(
            "identifier must contain 1..256 bytes without control characters".into(),
        ));
    }
    Ok(())
}

fn digest(value: &str) -> Result<(), Error> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error(
            "digest must be 64 lowercase hexadecimal characters".into(),
        ));
    }
    Ok(())
}

/// Artifact bytes already bounded and read by the host. Err means unavailable,
/// not an empty artifact. Metadata/hash checks still happen inside this library.
pub type ArtifactInputs<'a> = BTreeMap<&'a str, Result<&'a [u8], &'a str>>;

pub fn verify(
    contract_bytes: &[u8],
    bundle_bytes: &[u8],
    expected: &ExpectedRun,
    artifacts: &ArtifactInputs<'_>,
) -> Result<Report, Error> {
    let contract = parse_contract(contract_bytes)?;
    let bundle = parse_bundle(bundle_bytes)?;
    identifier(&expected.run_id)?;
    identifier(&expected.session_id)?;
    digest(&expected.contract_sha256)?;
    digest(&expected.capability_sha256)?;
    if expected.epoch == 0 {
        return Err(Error("expected epoch must be positive".into()));
    }
    let contract_hash = sha256(contract_bytes);
    let mut report = Report {
        ok: true,
        evidence_schema_version: SCHEMA_VERSION,
        verdict: Verdict::Passed,
        contract_sha256: contract_hash.clone(),
        bundle_sha256: sha256(bundle_bytes),
        run_id: expected.run_id.clone(),
        criteria: Vec::new(),
        issues: Vec::new(),
    };
    if contract_hash != expected.contract_sha256
        || bundle.contract_sha256 != expected.contract_sha256
        || bundle.capability_sha256 != expected.capability_sha256
        || bundle.run_id != expected.run_id
        || bundle.session_id != expected.session_id
        || bundle.epoch != expected.epoch
    {
        report.issue("binding_mismatch", Verdict::Failed);
    }
    if !bundle.journal_complete || !bundle.degraded.is_empty() {
        report.issue("journal_incomplete_or_degraded", Verdict::Incomplete);
    }
    validate_journal(&bundle, &mut report)?;
    let mut seen = BTreeSet::new();
    let mut total = 0_usize;
    for artifact in &bundle.artifacts {
        seen.insert(artifact.id.as_str());
        match artifacts.get(artifact.id.as_str()) {
            Some(Ok(bytes)) => {
                if bytes.len() > MAX_ARTIFACT_BYTES {
                    return Err(Error("provided artifact exceeds byte limit".into()));
                }
                total += bytes.len();
                if total > MAX_TOTAL_ARTIFACT_BYTES {
                    return Err(Error(
                        "provided artifacts exceed aggregate byte limit".into(),
                    ));
                }
                if bytes.len() as u64 != artifact.size_bytes || sha256(bytes) != artifact.sha256 {
                    report.issue("artifact_integrity_mismatch", Verdict::Failed);
                }
            }
            Some(Err(_)) | None => report.issue("artifact_unavailable", Verdict::Incomplete),
        }
    }
    if artifacts.keys().any(|id| !seen.contains(id)) {
        return Err(Error("provided artifact is not declared in bundle".into()));
    }
    // Only the final observation can establish the final state. Earlier success
    // cannot survive a later action or a later contradictory/missing fact.
    // Bundle-level facts `outcome.id`, `outcome.kind`, and
    // `human_control_returned` are resolved from the evidence record itself so
    // contracts can accept declared business outcomes (e.g. not_found → passed).
    let latest = bundle.observations.last();
    let journal_valid = report.issues.is_empty();
    for criterion in &contract.criteria {
        let (verdict, reason) = if !journal_valid {
            (Verdict::Incomplete, "evidence prerequisites not satisfied")
        } else if let Some(value) = resolve_fact(&bundle, latest, &criterion.fact) {
            predicate(&criterion.predicate, &value)
        } else {
            (Verdict::Incomplete, "required observation fact is missing")
        };
        report.criteria.push(CriterionResult {
            id: criterion.id.clone(),
            required: criterion.required,
            verdict,
            reason: reason.into(),
        });
        if criterion.required {
            report.verdict = report.verdict.combine(verdict);
        }
    }
    Ok(report)
}

/// Resolve a contract fact from bundle-level outcome/handoff state or the
/// final observation. Bundle facts always win over identically named observation
/// keys so producers cannot override `outcome.*` via forged observation text.
fn resolve_fact(
    bundle: &Bundle,
    latest: Option<&Observation>,
    fact: &str,
) -> Option<Value> {
    match fact {
        "outcome.id" => bundle
            .outcome
            .as_ref()
            .map(|outcome| Value::String(outcome.id.clone())),
        "outcome.kind" => bundle.outcome.as_ref().map(|outcome| {
            Value::String(
                match outcome.kind {
                    OutcomeKind::Success => "success",
                    OutcomeKind::Business => "business",
                }
                .into(),
            )
        }),
        "human_control_returned" => Some(Value::Bool(human_control_returned(bundle))),
        _ => latest.and_then(|o| o.facts.get(fact).cloned()),
    }
}

fn human_control_returned(bundle: &Bundle) -> bool {
    bundle
        .interventions
        .iter()
        .any(|i| matches!(i.status, InterventionStatus::Returned))
}

fn validate_journal(bundle: &Bundle, report: &mut Report) -> Result<(), Error> {
    if bundle.actions.is_empty() || bundle.observations.is_empty() {
        report.issue("empty_action_or_observation_journal", Verdict::Incomplete);
    }
    let mut sequences = BTreeSet::new();
    let mut epoch_at_sequence = BTreeMap::from([(0, bundle.epoch)]);
    let mut prior = 0;
    let mut active_epoch = bundle.epoch;
    for transition in &bundle.epoch_transitions {
        identifier(&transition.reason)?;
        if transition.sequence <= prior
            || !sequences.insert(transition.sequence)
            || transition.from_epoch != active_epoch
            || transition.to_epoch <= active_epoch
        {
            return Err(Error("invalid epoch transition sequence or chain".into()));
        }
        prior = transition.sequence;
        active_epoch = transition.to_epoch;
        epoch_at_sequence.insert(transition.sequence, active_epoch);
    }
    if let Some(transition) = bundle.epoch_transitions.last()
        && bundle
            .observations
            .last()
            .is_none_or(|o| o.sequence <= transition.sequence)
    {
        report.issue("final_epoch_unobserved", Verdict::Incomplete);
    }
    if bundle.actions.is_empty() {
        return Ok(());
    }
    let mut actions = BTreeMap::new();
    prior = 0;
    for action in &bundle.actions {
        identifier(&action.id)?;
        if actions.insert(&action.id, action.sequence).is_some()
            || action.sequence <= prior
            || !sequences.insert(action.sequence)
        {
            return Err(Error("duplicate or unordered action id/sequence".into()));
        }
        prior = action.sequence;
        match action.disposition {
            Disposition::Succeeded | Disposition::NotDispatched => {}
            Disposition::Failed => report.issue("action_failed", Verdict::Failed),
            Disposition::Unknown | Disposition::Cancelled | Disposition::Denied => {
                report.issue("action_not_completed", Verdict::Incomplete);
            }
        }
    }
    let artifact_ids: BTreeSet<_> = bundle.artifacts.iter().map(|a| &a.id).collect();
    let mut observed_actions = BTreeSet::new();
    prior = 0;
    for observation in &bundle.observations {
        if observation.sequence <= prior || !sequences.insert(observation.sequence) {
            return Err(Error("duplicate or unordered observation sequence".into()));
        }
        prior = observation.sequence;
        if observation.run_id != bundle.run_id
            || observation.session_id != bundle.session_id
            || Some(&observation.epoch)
                != epoch_at_sequence
                    .range(..observation.sequence)
                    .next_back()
                    .map(|(_, epoch)| epoch)
        {
            report.issue("observation_binding_mismatch", Verdict::Failed);
        }
        let Some(action_sequence) = actions.get(&observation.after_action_id) else {
            return Err(Error("observation references unknown action".into()));
        };
        if bundle.actions.iter().any(|a| {
            a.id == observation.after_action_id
                && matches!(a.disposition, Disposition::NotDispatched)
        }) {
            report.issue("observation_claims_undispatched_action", Verdict::Failed);
        }
        if observation.sequence <= *action_sequence {
            report.issue("observation_precedes_action", Verdict::Failed);
        }
        // An observation is of the most recent action at its sequence, not an
        // arbitrary earlier action whose success the runner wants to reuse.
        if bundle.actions.iter().any(|a| {
            !matches!(a.disposition, Disposition::NotDispatched)
                && a.sequence > *action_sequence
                && a.sequence < observation.sequence
        }) {
            report.issue("observation_references_stale_action", Verdict::Failed);
        }
        observed_actions.insert(&observation.after_action_id);
        let mut observation_artifacts = BTreeSet::new();
        for id in &observation.artifact_ids {
            if !artifact_ids.contains(id) || !observation_artifacts.insert(id) {
                return Err(Error(
                    "unknown or duplicate observation artifact reference".into(),
                ));
            }
        }
        if observation.facts.len() > MAX_CRITERIA {
            return Err(Error("observation exceeds 256 facts".into()));
        }
        for key in observation.facts.keys() {
            identifier(key)?;
        }
    }
    if bundle.actions.iter().any(|a| {
        !matches!(a.disposition, Disposition::NotDispatched) && !observed_actions.contains(&a.id)
    }) {
        report.issue("action_has_no_post_observation", Verdict::Incomplete);
    }
    if let (Some(action), Some(observation)) = (bundle.actions.last(), bundle.observations.last())
        && (observation.after_action_id != action.id || observation.sequence <= action.sequence)
    {
        report.issue("final_action_unobserved", Verdict::Incomplete);
    }
    Ok(())
}

fn safe_number(value: &Value) -> bool {
    value
        .as_f64()
        .is_some_and(|n| n.is_finite() && n.abs() <= 9_007_199_254_740_991.0)
}

fn predicate(predicate: &Predicate, actual: &Value) -> (Verdict, &'static str) {
    let result = match predicate {
        Predicate::Exists => !actual.is_null(),
        Predicate::Equals { value } => actual == value,
        Predicate::NotEquals { value } => actual != value,
        Predicate::Contains { value } => match (actual, value) {
            (Value::String(actual), Value::String(expected)) => actual.contains(expected),
            (Value::Array(actual), Value::Array(expected)) => {
                expected.iter().all(|v| actual.contains(v))
            }
            _ => return (Verdict::Incomplete, "contains observation type mismatch"),
        },
        Predicate::Minimum { value } => {
            if !safe_number(actual) {
                return (
                    Verdict::Incomplete,
                    "minimum observation is not an exact-range finite number",
                );
            }
            match (actual.as_f64(), value.as_f64()) {
                (Some(actual), Some(expected)) => actual >= expected,
                _ => return (Verdict::Incomplete, "minimum observation type mismatch"),
            }
        }
    };
    if result {
        (Verdict::Passed, "predicate satisfied")
    } else {
        (Verdict::Failed, "predicate not satisfied")
    }
}
