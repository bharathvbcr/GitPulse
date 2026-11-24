//! Manvi policy approval ticket transport (Opus §4 & R9).
//!
//! **Manvi remains the policy authority.** GitPulse transports Manvi policy
//! approval requests to mobile and turns `gitpulse-protocol`-verified mobile
//! decisions into **single-use, hash-bound, TTL'd grants**:
//! - **Never cached from mobile:** any decision requesting a reusable/cached
//!   grant is refused loudly (`once: true` always).
//! - **Hash-bound:** every grant is cryptographically bound to
//!   `action_sha256(repo_root, task_id, rule, target)` and fails closed if
//!   redeemed against any different target or rule.
//! - **Hard denies are non-overridable:** rule severities are loaded from
//!   GitPulse's shared `contracts/verdict.schema.json` (same contract as
//!   `src-tauri/src/grants/mod.rs`). Any `"hard"` rule OR unknown rule is
//!   strictly refused.
//! - **Enterprise `remote_approval` policy honored:** `Deny` blocks all remote
//!   Manvi approvals; `StepUp` requires fresh hardware P-256 step-up auth;
//!   `Allow` still requires a valid hardware P-256 signature verified through
//!   `gitpulse_protocol::crypto::verify_approval_decision` (Opus R2 & #31).
//! - **Audit export:** every ticket, decision, grant, redemption, and refusal
//!   is recorded for Enterprise `audit_export`.

use crate::remote::config::RemoteApprovalPolicy;
use crate::remote::pty_gate::sha256_hex;
use crate::remote::revocations::RevocationCache;
use gitpulse_protocol::canonical::hex_encode;
use gitpulse_protocol::crypto::verify_approval_decision;
use gitpulse_protocol::wire::{ApprovalDecisionKind, DeviceCertificate, SignedApprovalDecision};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

/// Maximum TTL for a remotely issued Manvi single-use grant (5 minutes).
pub const MAX_MANVI_GRANT_TTL_SECS: i64 = 5 * 60;

const MANVI_ACTION_DOMAIN: &[u8] = b"gitpulse-manvi-action/v1\0";

fn rule_severities() -> Result<&'static HashMap<String, String>, String> {
    static SEVERITIES: OnceLock<Result<HashMap<String, String>, String>> = OnceLock::new();
    match SEVERITIES.get_or_init(|| {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../contracts/verdict.schema.json"))
                .map_err(|e| format!("shared verdict contract unreadable: {e}"))?;
        let properties = schema
            .pointer("/$defs/severityByRule/properties")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| "shared verdict contract missing severityByRule".to_string())?;
        let mut map = HashMap::with_capacity(properties.len());
        for (rule, desc) in properties {
            let sev = desc
                .get("const")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("missing severity const for {rule}"))?;
            map.insert(rule.clone(), sev.to_string());
        }
        Ok(map)
    }) {
        Ok(m) => Ok(m),
        Err(e) => Err(e.clone()),
    }
}

/// Computes the canonical SHA-256 hash binding a Manvi ticket & single-use grant
/// to its exact `(repo_root, task_id, rule, target)`.
pub fn compute_manvi_action_sha256(
    repo_root: &str,
    task_id: &str,
    rule: &str,
    target: &str,
) -> String {
    let mut payload = Vec::with_capacity(256);
    payload.extend_from_slice(MANVI_ACTION_DOMAIN);
    let canonical = format!(
        "repo_root={repo_root}\ntask_id={task_id}\nrule={rule}\ntarget={target}\n"
    );
    payload.extend_from_slice(canonical.as_bytes());
    sha256_hex(&payload)
}

/// Open Manvi policy approval ticket transported to mobile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManviPolicyTicket {
    pub ticket_id: String,
    pub host_id: String,
    pub repo_root: String,
    pub task_id: String,
    pub rule: String,
    pub severity: String,
    pub target: String,
    pub reason: String,
    pub action_sha256: String,
    pub created_at: i64,
    pub expires_at: i64,
}

/// Mobile decision for a Manvi policy ticket wrapping `gitpulse-protocol`'s
/// hardware P-256 [`SignedApprovalDecision`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManviMobileDecision {
    pub signed: SignedApprovalDecision,
    /// Must always be `false`; mobile is forbidden from caching Manvi grants.
    #[serde(default)]
    pub cache_requested: bool,
    /// True when the mobile device performed biometric step-up verification.
    #[serde(default)]
    pub step_up_confirmed: bool,
}

/// Single-use, hash-bound, TTL'd Manvi override grant minted from a verified
/// mobile decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManviSingleUseGrant {
    pub grant_id: String,
    pub ticket_id: String,
    pub repo_root: String,
    pub task_id: String,
    pub rule: String,
    pub target: String,
    pub action_sha256: String,
    /// Always `true`: mobile grants are strictly single-use and never cached.
    pub once: bool,
    pub granted_by_device: String,
    pub issued_at: i64,
    pub expires_at: i64,
    pub consumed: bool,
}

/// Outcome of submitting a mobile decision to [`ManviTicketBridge`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManviDecisionOutcome {
    Granted(ManviSingleUseGrant),
    Denied { ticket_id: String, interrupt: bool },
}

/// Structured audit log record for Enterprise `audit_export`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManviAuditEntry {
    pub timestamp_secs: i64,
    pub event: String,
    pub ticket_id: String,
    pub grant_id: Option<String>,
    pub device_id: Option<String>,
    pub rule: String,
    pub action_sha256: String,
    pub outcome: String,
}

/// Failure modes for Manvi policy tickets & grant redemption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManviTicketError {
    ContractUnavailable(String),
    HardDenyNonOverridable { rule: String, severity: String },
    EnterpriseRemoteApprovalDenied,
    StepUpRequired,
    CachedGrantForbidden,
    TicketNotFound(String),
    TicketExpired(String),
    ActionHashMismatch,
    NonceReplayed,
    ProtocolVerificationFailed(String),
    GrantNotFound(String),
    GrantAlreadyConsumed(String),
    GrantExpired(String),
}

impl std::fmt::Display for ManviTicketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManviTicketError::ContractUnavailable(m) => {
                write!(f, "Manvi verdict contract unavailable: {m}")
            }
            ManviTicketError::HardDenyNonOverridable { rule, severity } => write!(
                f,
                "Manvi rule '{rule}' (severity '{severity}') is non-overridable; hard denies cannot be waived"
            ),
            ManviTicketError::EnterpriseRemoteApprovalDenied => {
                write!(f, "enterprise policy sets remote_approval=deny for Manvi tickets")
            }
            ManviTicketError::StepUpRequired => {
                write!(f, "enterprise policy requires step-up biometric confirmation")
            }
            ManviTicketError::CachedGrantForbidden => {
                write!(f, "Manvi mobile grants must be single-use and can never be cached")
            }
            ManviTicketError::TicketNotFound(id) => write!(f, "Manvi ticket '{id}' not found"),
            ManviTicketError::TicketExpired(id) => write!(f, "Manvi ticket '{id}' expired"),
            ManviTicketError::ActionHashMismatch => {
                write!(f, "action_sha256 does not match the ticket or grant binding")
            }
            ManviTicketError::NonceReplayed => {
                write!(f, "Manvi decision nonce already used")
            }
            ManviTicketError::ProtocolVerificationFailed(m) => {
                write!(f, "gitpulse-protocol verification failed: {m}")
            }
            ManviTicketError::GrantNotFound(id) => write!(f, "Manvi grant '{id}' not found"),
            ManviTicketError::GrantAlreadyConsumed(id) => {
                write!(f, "Manvi single-use grant '{id}' was already consumed")
            }
            ManviTicketError::GrantExpired(id) => write!(f, "Manvi grant '{id}' expired"),
        }
    }
}

impl std::error::Error for ManviTicketError {}

/// Stateful host bridge transporting Manvi soft-deny approval requests and
/// redeeming single-use, hash-bound, TTL'd grants.
pub struct ManviTicketBridge {
    host_id: String,
    policy: RemoteApprovalPolicy,
    open_tickets: Mutex<HashMap<String, ManviPolicyTicket>>,
    grants: Mutex<HashMap<String, ManviSingleUseGrant>>,
    used_nonces: Mutex<HashSet<String>>,
    audit: Mutex<Vec<ManviAuditEntry>>,
}

impl ManviTicketBridge {
    pub fn new(host_id: impl Into<String>, policy: RemoteApprovalPolicy) -> Self {
        Self {
            host_id: host_id.into(),
            policy,
            open_tickets: Mutex::new(HashMap::new()),
            grants: Mutex::new(HashMap::new()),
            used_nonces: Mutex::new(HashSet::new()),
            audit: Mutex::new(Vec::new()),
        }
    }

    pub fn policy(&self) -> RemoteApprovalPolicy {
        self.policy
    }

    /// Creates a transportable Manvi approval ticket for a soft/warn policy
    /// denial. Strictly refuses `"hard"` rules, unknown rules, or Enterprise
    /// `remote_approval: deny`.
    #[allow(clippy::too_many_arguments)]
    pub fn open_ticket(
        &self,
        repo_root: impl Into<String>,
        task_id: impl Into<String>,
        rule: impl Into<String>,
        target: impl Into<String>,
        reason: impl Into<String>,
        ttl_secs: i64,
        now_secs: i64,
    ) -> Result<ManviPolicyTicket, ManviTicketError> {
        let repo_root = repo_root.into();
        let task_id = task_id.into();
        let rule = rule.into();
        let target = target.into();
        let reason = reason.into();
        let action_sha256 = compute_manvi_action_sha256(&repo_root, &task_id, &rule, &target);

        if self.policy == RemoteApprovalPolicy::Deny {
            self.record_audit(
                now_secs,
                "ticket_refused",
                "",
                None,
                None,
                &rule,
                &action_sha256,
                "enterprise_remote_approval_deny",
            );
            return Err(ManviTicketError::EnterpriseRemoteApprovalDenied);
        }

        let severities =
            rule_severities().map_err(ManviTicketError::ContractUnavailable)?;
        let severity = match severities.get(&rule).map(String::as_str) {
            Some("soft") => "soft".to_string(),
            Some("warn") => "warn".to_string(),
            Some(other) => {
                self.record_audit(
                    now_secs,
                    "ticket_refused",
                    "",
                    None,
                    None,
                    &rule,
                    &action_sha256,
                    &format!("hard_deny_{other}"),
                );
                return Err(ManviTicketError::HardDenyNonOverridable {
                    rule,
                    severity: other.to_string(),
                });
            }
            None => {
                self.record_audit(
                    now_secs,
                    "ticket_refused",
                    "",
                    None,
                    None,
                    &rule,
                    &action_sha256,
                    "hard_deny_unknown_rule",
                );
                return Err(ManviTicketError::HardDenyNonOverridable {
                    rule,
                    severity: "unknown (hard by default)".into(),
                });
            }
        };

        let clamped_ttl = ttl_secs.clamp(1, MAX_MANVI_GRANT_TTL_SECS);
        let mut raw_id = [0u8; 12];
        let _ = getrandom::fill(&mut raw_id);
        let ticket_id = format!("mvtk_{}", hex_encode(&raw_id));

        let ticket = ManviPolicyTicket {
            ticket_id: ticket_id.clone(),
            host_id: self.host_id.clone(),
            repo_root,
            task_id,
            rule: rule.clone(),
            severity,
            target,
            reason,
            action_sha256: action_sha256.clone(),
            created_at: now_secs,
            expires_at: now_secs + clamped_ttl,
        };
        self.open_tickets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(ticket_id.clone(), ticket.clone());
        self.record_audit(
            now_secs,
            "ticket_opened",
            &ticket_id,
            None,
            None,
            &rule,
            &action_sha256,
            "open",
        );
        Ok(ticket)
    }

    /// Resolves an open Manvi ticket via `gitpulse_protocol::crypto::verify_approval_decision`,
    /// minting a single-use, hash-bound, TTL'd grant on `AllowOnce`.
    pub fn apply_mobile_decision(
        &self,
        decision: &ManviMobileDecision,
        device_cert: &DeviceCertificate,
        revocations: &RevocationCache,
        now_secs: i64,
    ) -> Result<ManviDecisionOutcome, ManviTicketError> {
        if self.policy == RemoteApprovalPolicy::Deny {
            return Err(ManviTicketError::EnterpriseRemoteApprovalDenied);
        }
        if decision.cache_requested {
            self.record_audit(
                now_secs,
                "decision_refused",
                &decision.signed.ticket_id,
                None,
                Some(&decision.signed.device_id),
                "",
                &decision.signed.input_sha256,
                "cached_grant_forbidden",
            );
            return Err(ManviTicketError::CachedGrantForbidden);
        }
        if self.policy == RemoteApprovalPolicy::StepUp
            && decision.signed.decision == ApprovalDecisionKind::AllowOnce
            && !decision.step_up_confirmed
        {
            self.record_audit(
                now_secs,
                "decision_refused",
                &decision.signed.ticket_id,
                None,
                Some(&decision.signed.device_id),
                "",
                &decision.signed.input_sha256,
                "step_up_required",
            );
            return Err(ManviTicketError::StepUpRequired);
        }

        // Peek open ticket to get expected hash & expiry before verifying signature.
        let expected_action_sha256 = {
            let guard = self
                .open_tickets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let existing = guard
                .get(&decision.signed.ticket_id)
                .ok_or_else(|| ManviTicketError::TicketNotFound(decision.signed.ticket_id.clone()))?;
            if now_secs >= existing.expires_at {
                return Err(ManviTicketError::TicketExpired(decision.signed.ticket_id.clone()));
            }
            existing.action_sha256.clone()
        };

        // Delegate 100% of cert + mandatory RevocationPolicy::Enforced + P-256
        // DER signature verification to `gitpulse-protocol`!
        let rev_policy = revocations
            .enforced_policy(now_secs)
            .map_err(|e| ManviTicketError::ProtocolVerificationFailed(e.to_string()))?;
        let verified_kind = verify_approval_decision(
            &decision.signed,
            device_cert,
            revocations.identity_pub_ed25519(),
            &self.host_id,
            &decision.signed.ticket_id,
            &expected_action_sha256,
            rev_policy,
            now_secs,
        )
        .map_err(|e| ManviTicketError::ProtocolVerificationFailed(e.to_string()))?;

        // Commit nonce to prevent replays.
        {
            let mut nonces = self
                .used_nonces
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !nonces.insert(decision.signed.nonce.clone()) {
                return Err(ManviTicketError::NonceReplayed);
            }
        }

        // Consume the open ticket.
        let ticket = {
            let mut guard = self
                .open_tickets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard
                .remove(&decision.signed.ticket_id)
                .ok_or_else(|| ManviTicketError::TicketNotFound(decision.signed.ticket_id.clone()))?
        };

        match verified_kind {
            ApprovalDecisionKind::AllowOnce => {
                let mut raw_id = [0u8; 12];
                let _ = getrandom::fill(&mut raw_id);
                let grant_id = format!("mvgr_{}", hex_encode(&raw_id));
                let grant_expires = ticket.expires_at.min(now_secs + MAX_MANVI_GRANT_TTL_SECS);
                let grant = ManviSingleUseGrant {
                    grant_id: grant_id.clone(),
                    ticket_id: ticket.ticket_id.clone(),
                    repo_root: ticket.repo_root,
                    task_id: ticket.task_id,
                    rule: ticket.rule.clone(),
                    target: ticket.target,
                    action_sha256: ticket.action_sha256.clone(),
                    once: true,
                    granted_by_device: decision.signed.device_id.clone(),
                    issued_at: now_secs,
                    expires_at: grant_expires,
                    consumed: false,
                };
                self.grants
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(grant_id.clone(), grant.clone());
                self.record_audit(
                    now_secs,
                    "grant_issued",
                    &ticket.ticket_id,
                    Some(&grant_id),
                    Some(&decision.signed.device_id),
                    &ticket.rule,
                    &ticket.action_sha256,
                    "allow_once",
                );
                Ok(ManviDecisionOutcome::Granted(grant))
            }
            ApprovalDecisionKind::Deny => {
                self.record_audit(
                    now_secs,
                    "ticket_denied",
                    &ticket.ticket_id,
                    None,
                    Some(&decision.signed.device_id),
                    &ticket.rule,
                    &ticket.action_sha256,
                    "deny",
                );
                Ok(ManviDecisionOutcome::Denied {
                    ticket_id: ticket.ticket_id,
                    interrupt: false,
                })
            }
            ApprovalDecisionKind::DenyAndInterrupt => {
                self.record_audit(
                    now_secs,
                    "ticket_denied",
                    &ticket.ticket_id,
                    None,
                    Some(&decision.signed.device_id),
                    &ticket.rule,
                    &ticket.action_sha256,
                    "deny_and_interrupt",
                );
                Ok(ManviDecisionOutcome::Denied {
                    ticket_id: ticket.ticket_id,
                    interrupt: true,
                })
            }
        }
    }

    /// Redeems a single-use, hash-bound, TTL'd grant for one exact Manvi action.
    pub fn redeem_grant(
        &self,
        grant_id: &str,
        repo_root: &str,
        task_id: &str,
        rule: &str,
        target: &str,
        now_secs: i64,
    ) -> Result<ManviSingleUseGrant, ManviTicketError> {
        let candidate_hash = compute_manvi_action_sha256(repo_root, task_id, rule, target);
        let mut grants = self
            .grants
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let grant = grants
            .get_mut(grant_id)
            .ok_or_else(|| ManviTicketError::GrantNotFound(grant_id.to_string()))?;

        if grant.consumed {
            return Err(ManviTicketError::GrantAlreadyConsumed(grant_id.to_string()));
        }
        if now_secs >= grant.expires_at {
            return Err(ManviTicketError::GrantExpired(grant_id.to_string()));
        }
        if grant.action_sha256 != candidate_hash {
            return Err(ManviTicketError::ActionHashMismatch);
        }

        grant.consumed = true;
        let redeemed = grant.clone();
        drop(grants);

        self.record_audit(
            now_secs,
            "grant_redeemed",
            &redeemed.ticket_id,
            Some(&redeemed.grant_id),
            Some(&redeemed.granted_by_device),
            &redeemed.rule,
            &redeemed.action_sha256,
            "consumed",
        );
        Ok(redeemed)
    }

    pub fn open_tickets(&self) -> Vec<ManviPolicyTicket> {
        self.open_tickets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
    }

    pub fn audit_export(&self) -> Vec<ManviAuditEntry> {
        self.audit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[allow(clippy::too_many_arguments)]
    fn record_audit(
        &self,
        timestamp_secs: i64,
        event: &str,
        ticket_id: &str,
        grant_id: Option<&str>,
        device_id: Option<&str>,
        rule: &str,
        action_sha256: &str,
        outcome: &str,
    ) {
        self.audit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(ManviAuditEntry {
                timestamp_secs,
                event: event.to_string(),
                ticket_id: ticket_id.to_string(),
                grant_id: grant_id.map(str::to_string),
                device_id: device_id.map(str::to_string),
                rule: rule.to_string(),
                action_sha256: action_sha256.to_string(),
                outcome: outcome.to_string(),
            });
    }
}
