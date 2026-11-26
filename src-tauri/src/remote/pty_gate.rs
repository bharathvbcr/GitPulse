//! `pty.input` RCE gate and read-only lossy PTY streaming buffer (Opus P0-7 / R7).
//!
//! All P-256 step-up signature, certificate, and mandatory revocation checks
//! delegate strictly to `waylode-protocol` (zero local crypto duplication).
//!
//! # Security Model (Opus P0-7)
//!
//! Writing bytes to a live PTY is remote code execution. Therefore:
//! 1. **Read-only PTY streaming is the default** (`LossyPtyStreamBuffer`, `<= 16 KiB`
//!    chunk cap, lossy drop-oldest so a slow mobile link never stalls `waylode-ptyd`).
//! 2. **`pty.input` is gated behind a separate entitlement and is OFF by default.**
//! 3. Even when entitled, every `pty.input` authorization MUST present:
//!    - an active, in-memory, Desktop-granted, time-boxed [`PtyInputConsentToken`]
//!      bound to `(session_id, mobile_device_id)` (never read from an agent-writable file);
//!    - a fresh mobile hardware **P-256** [`SignedApprovalDecision`] verified via
//!      `waylode_protocol::crypto::verify_approval_decision` against the
//!      Keychain-pinned Account `IdentityKey` and mandatory non-stale [`RevocationCache`];
//!    - an unspent nonce and valid payload digest (`<= 16 KiB`).
//! 4. Every attempt (both allowed and denied) is appended to [`PtyAuditEntry`]
//!    log with its SHA-256 digest and outcome (never logging raw keystrokes).
//!
//! # Accepted Residual Risk (Opus Review Board #23)
//!
//! Within an active Desktop-granted, time-boxed consent window (after initial
//! biometric step-up authorization), individual interactive keystrokes are
//! authenticated by the Noise `IK` tunnel and audited rather than prompting
//! biometrics per keystroke. Consent tokens remain strictly bounded in time
//! (`<= 15m`), session-scoped, device-scoped, and revocable immediately by Desktop.

use crate::remote::config::RemoteApprovalPolicy;
use crate::remote::revocations::RevocationCache;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use waylode_protocol::canonical::hex_encode;
use waylode_protocol::crypto::verify_approval_decision;
use waylode_protocol::wire::{
    ApprovalDecisionKind, DeviceCertificate, SignedApprovalDecision, MAX_PTY_FRAME_BYTES,
};

/// Re-exported `<= 16 KiB` PTY chunk ceiling from `waylode-protocol`.
pub const MAX_PTY_CHUNK_BYTES: usize = MAX_PTY_FRAME_BYTES;

/// Maximum time-box for a Desktop-granted `pty.input` consent token (15 min).
pub const MAX_PTY_CONSENT_TTL_SECS: i64 = 15 * 60;

/// Lossy drop-oldest buffer for read-only mobile PTY streaming (`<= 16 KiB` per chunk).
///
/// Guarantees a slow or disconnected mobile client can never apply backpressure
/// to `waylode-ptyd`.
#[derive(Debug)]
pub struct LossyPtyStreamBuffer {
    max_chunks: usize,
    queue: VecDeque<Vec<u8>>,
    dropped_chunks: u64,
    dropped_bytes: u64,
}

impl LossyPtyStreamBuffer {
    pub fn new(max_chunks: usize) -> Self {
        Self {
            max_chunks: max_chunks.max(1),
            queue: VecDeque::new(),
            dropped_chunks: 0,
            dropped_bytes: 0,
        }
    }

    /// Pushes PTY output bytes, slicing into `<= 16 KiB` chunks and dropping
    /// the oldest queued chunks when at capacity.
    pub fn push(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        for slice in data.chunks(MAX_PTY_CHUNK_BYTES) {
            if self.queue.len() >= self.max_chunks {
                if let Some(evicted) = self.queue.pop_front() {
                    self.dropped_chunks += 1;
                    self.dropped_bytes += evicted.len() as u64;
                }
            }
            self.queue.push_back(slice.to_vec());
        }
    }

    pub fn pop(&mut self) -> Option<Vec<u8>> {
        self.queue.pop_front()
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn dropped_chunks(&self) -> u64 {
        self.dropped_chunks
    }

    pub fn dropped_bytes(&self) -> u64 {
        self.dropped_bytes
    }
}

/// In-memory Desktop-granted, time-boxed consent token authorizing one mobile
/// device to send step-up-authorized `pty.input` to one specific terminal session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtyInputConsentToken {
    pub token_id: String,
    pub session_id: String,
    pub mobile_device_id: String,
    pub granted_at: i64,
    pub expires_at: i64,
}

/// Mobile request to inject keystrokes into a PTY session, carrying a hardware
/// P-256 [`SignedApprovalDecision`] (`ticket_id == consent_token_id`,
/// `input_sha256 == sha256(payload)`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedPtyInputRequest {
    pub session_id: String,
    pub consent_token_id: String,
    /// Base64-encoded raw bytes to write (`<= 16 KiB` decoded).
    pub payload_b64: String,
    /// Hardware P-256 step-up authorization verified via `waylode-protocol`.
    pub step_up: SignedApprovalDecision,
}

/// Why a `pty.input` request was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PtyGateDenial {
    EntitlementDisabled,
    EnterprisePolicyDenied,
    ConsentMissing,
    ConsentExpired,
    ConsentSessionMismatch,
    ConsentDeviceMismatch,
    PayloadInvalidBase64,
    PayloadEmpty,
    PayloadTooLarge,
    StepUpMustAllowOnce,
    NonceReplayed,
    ProtocolVerificationFailed(String),
}

impl std::fmt::Display for PtyGateDenial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PtyGateDenial::EntitlementDisabled => {
                write!(f, "pty.input is disabled by default (entitlement off)")
            }
            PtyGateDenial::EnterprisePolicyDenied => {
                write!(f, "enterprise policy forbids remote pty.input")
            }
            PtyGateDenial::ConsentMissing => {
                write!(f, "no active Desktop-granted pty.input consent token")
            }
            PtyGateDenial::ConsentExpired => write!(f, "Desktop pty.input consent token expired"),
            PtyGateDenial::ConsentSessionMismatch => {
                write!(f, "consent token is bound to a different terminal session")
            }
            PtyGateDenial::ConsentDeviceMismatch => {
                write!(f, "consent token is bound to a different mobile device")
            }
            PtyGateDenial::PayloadInvalidBase64 => write!(f, "payload_b64 is invalid base64"),
            PtyGateDenial::PayloadEmpty => write!(f, "pty.input payload is empty"),
            PtyGateDenial::PayloadTooLarge => {
                write!(f, "pty.input payload exceeds {MAX_PTY_CHUNK_BYTES} bytes")
            }
            PtyGateDenial::StepUpMustAllowOnce => {
                write!(f, "pty.input step-up decision must be allow_once")
            }
            PtyGateDenial::NonceReplayed => {
                write!(f, "pty.input step-up nonce already used")
            }
            PtyGateDenial::ProtocolVerificationFailed(reason) => {
                write!(f, "waylode-protocol verification failed: {reason}")
            }
        }
    }
}

impl std::error::Error for PtyGateDenial {}

/// Structured audit record emitted for every `pty.input` evaluation (Opus P0-7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtyAuditEntry {
    pub timestamp_secs: i64,
    pub host_id: String,
    pub session_id: String,
    pub device_id: String,
    pub consent_token_id: String,
    pub payload_sha256: String,
    pub payload_bytes: usize,
    pub allowed: bool,
    pub denial_reason: Option<String>,
}

/// Policy and state enforcing Opus P0-7 for `pty.input`.
pub struct PtyInputGate {
    host_id: String,
    /// Separate entitlement, OFF by default.
    pty_input_entitled: bool,
    enterprise_policy: RemoteApprovalPolicy,
    consents: Mutex<HashMap<String, PtyInputConsentToken>>,
    used_nonces: Mutex<HashSet<String>>,
    audit_log: Mutex<Vec<PtyAuditEntry>>,
}

impl PtyInputGate {
    pub fn new(
        host_id: impl Into<String>,
        pty_input_entitled: bool,
        enterprise_policy: RemoteApprovalPolicy,
    ) -> Self {
        Self {
            host_id: host_id.into(),
            pty_input_entitled,
            enterprise_policy,
            consents: Mutex::new(HashMap::new()),
            used_nonces: Mutex::new(HashSet::new()),
            audit_log: Mutex::new(Vec::new()),
        }
    }

    /// Safe default: read-only PTY streaming only (`pty_input_entitled = false`).
    pub fn default_readonly(host_id: impl Into<String>) -> Self {
        Self::new(host_id, false, RemoteApprovalPolicy::Allow)
    }

    pub fn is_entitled(&self) -> bool {
        self.pty_input_entitled && self.enterprise_policy != RemoteApprovalPolicy::Deny
    }

    /// Grants an in-memory, time-boxed Desktop consent token for one session and
    /// one mobile device. Refuses if `pty_input` entitlement is off.
    pub fn grant_desktop_consent(
        &self,
        session_id: impl Into<String>,
        mobile_device_id: impl Into<String>,
        ttl_secs: i64,
        now_secs: i64,
    ) -> Result<PtyInputConsentToken, PtyGateDenial> {
        if !self.pty_input_entitled {
            return Err(PtyGateDenial::EntitlementDisabled);
        }
        if self.enterprise_policy == RemoteApprovalPolicy::Deny {
            return Err(PtyGateDenial::EnterprisePolicyDenied);
        }
        let clamped_ttl = ttl_secs.clamp(1, MAX_PTY_CONSENT_TTL_SECS);
        let mut raw_id = [0u8; 16];
        let _ = getrandom::fill(&mut raw_id);
        let token_id = format!("ptyc_{}", hex_encode(&raw_id));
        let token = PtyInputConsentToken {
            token_id: token_id.clone(),
            session_id: session_id.into(),
            mobile_device_id: mobile_device_id.into(),
            granted_at: now_secs,
            expires_at: now_secs + clamped_ttl,
        };
        self.consents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(token_id, token.clone());
        Ok(token)
    }

    /// Immediately revokes a Desktop consent token.
    pub fn revoke_consent(&self, token_id: &str) -> bool {
        self.consents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(token_id)
            .is_some()
    }

    /// Validates a mobile `pty.input` request against all Opus P0-7 gates via
    /// `waylode-protocol`, records an audit entry regardless of outcome, and
    /// returns the verified raw payload bytes only if every check passes.
    pub fn authorize_input(
        &self,
        req: &SignedPtyInputRequest,
        device_cert: &DeviceCertificate,
        revocations: &RevocationCache,
        now_secs: i64,
    ) -> Result<Vec<u8>, PtyGateDenial> {
        let result = self.evaluate(req, device_cert, revocations, now_secs);
        let (allowed, payload_bytes, denial_reason) = match &result {
            Ok(bytes) => (true, bytes.len(), None),
            Err(denial) => (false, 0, Some(denial.to_string())),
        };
        self.audit_log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(PtyAuditEntry {
                timestamp_secs: now_secs,
                host_id: self.host_id.clone(),
                session_id: req.session_id.clone(),
                device_id: req.step_up.device_id.clone(),
                consent_token_id: req.consent_token_id.clone(),
                payload_sha256: req.step_up.input_sha256.clone(),
                payload_bytes,
                allowed,
                denial_reason,
            });
        result
    }

    fn evaluate(
        &self,
        req: &SignedPtyInputRequest,
        device_cert: &DeviceCertificate,
        revocations: &RevocationCache,
        now_secs: i64,
    ) -> Result<Vec<u8>, PtyGateDenial> {
        if !self.pty_input_entitled {
            return Err(PtyGateDenial::EntitlementDisabled);
        }
        if self.enterprise_policy == RemoteApprovalPolicy::Deny {
            return Err(PtyGateDenial::EnterprisePolicyDenied);
        }

        // 1. Validate Desktop-granted, in-memory, time-boxed consent token.
        let consent = {
            let guard = self
                .consents
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard
                .get(&req.consent_token_id)
                .cloned()
                .ok_or(PtyGateDenial::ConsentMissing)?
        };
        if now_secs >= consent.expires_at {
            return Err(PtyGateDenial::ConsentExpired);
        }
        if consent.session_id != req.session_id {
            return Err(PtyGateDenial::ConsentSessionMismatch);
        }
        if consent.mobile_device_id != req.step_up.device_id
            || device_cert.device_id != req.step_up.device_id
        {
            return Err(PtyGateDenial::ConsentDeviceMismatch);
        }

        // 2. Decode & bound payload bytes, computing exact SHA-256 binding.
        let payload = base64::engine::general_purpose::STANDARD
            .decode(req.payload_b64.trim())
            .map_err(|_| PtyGateDenial::PayloadInvalidBase64)?;
        if payload.is_empty() {
            return Err(PtyGateDenial::PayloadEmpty);
        }
        if payload.len() > MAX_PTY_CHUNK_BYTES {
            return Err(PtyGateDenial::PayloadTooLarge);
        }
        let payload_sha256 = sha256_hex(&payload);

        // 3. Delegate cert + mandatory RevocationPolicy::Enforced + P-256 DER
        //    signature verification strictly to `waylode-protocol`!
        let policy = revocations
            .enforced_policy(now_secs)
            .map_err(|e| PtyGateDenial::ProtocolVerificationFailed(e.to_string()))?;
        let kind = verify_approval_decision(
            &req.step_up,
            device_cert,
            revocations.identity_pub_ed25519(),
            &self.host_id,
            &req.consent_token_id,
            &payload_sha256,
            policy,
            now_secs,
        )
        .map_err(|e| PtyGateDenial::ProtocolVerificationFailed(e.to_string()))?;

        if kind != ApprovalDecisionKind::AllowOnce {
            return Err(PtyGateDenial::StepUpMustAllowOnce);
        }

        // 4. Commit nonce only after P-256 signature succeeds.
        let mut nonces = self
            .used_nonces
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !nonces.insert(req.step_up.nonce.clone()) {
            return Err(PtyGateDenial::NonceReplayed);
        }

        Ok(payload)
    }

    pub fn audit_entries(&self) -> Vec<PtyAuditEntry> {
        self.audit_log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex_encode(&h.finalize())
}
