//! Outbound-only zero-knowledge E2EE WebSocket relay tunnel (`gitpulsed --relay`),
//! aligned byte-for-byte with protocol contract #18 (`crates/gitpulse-protocol`).
//!
//! - Outbound-only WSS (`tokio-tungstenite`) connecting to
//!   `/v1/tunnel/ws?role=host&after_epoch=<u64>&after_seq=<u64>` with Ed25519
//!   request-signing headers (`X-GitPulse-Device`, `X-GitPulse-Timestamp`,
//!   `X-GitPulse-Signature`).
//! - Noise `IK` (`Noise_IK_25519_ChaChaPoly_SHA256`), prologue
//!   `b"gitpulse-tunnel/v1\0" || stream_id || b"\0" || host_epoch(u64 BE)`,
//!   transport wire `seq(u64 BE, >=1) || noise_aead_ct`, strict `seq > recv_high_water`
//!   replay/reorder rejection, 100k-frame rekey ceiling (Opus R1 & contract #18).
//! - Host is the source of truth: every connect/reconnect emits a full
//!   `inbox_snapshot` re-announcing all open approval tickets (Opus R6).
//! - WS close `4003` (revoked/downgraded) immediately flips
//!   `remote-approval.state` to `released_to_native` so active holds release to
//!   the local terminal prompt (Opus R6).
//! - Mandatory jittered reconnect backoff (`250ms..15s`) on `CloseGoingAway`
//!   (`1001`) or transport drop.
//! - Explicit non-blocking `keychain_locked` status reporting (never hangs on a
//!   locked macOS Keychain).

use crate::remote::codex_app_server::canonicalize_json;
use crate::remote::config::{KeychainStatus, REMOTE_APPROVAL_STATE_FILE};
use crate::remote::pty_gate::sha256_hex;
use crate::remote::revocations::{
    DeviceCertificate, RevocationCache, RevocationError, TrustAnchorError, TrustAnchorProvider,
    VerifiedDevice,
};
use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use futures_util::{SinkExt, StreamExt};
use p256::ecdsa::signature::Verifier as P256Verifier;
use p256::ecdsa::Signature as P256Signature;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::Message;

/// Noise IK cipher suite shared with `crates/gitpulse-protocol` (Opus R1, #18).
pub const NOISE_IK_PARAMS: &str = "Noise_IK_25519_ChaChaPoly_SHA256";

/// Noise IK prologue domain prefix from protocol contract #18.
const TUNNEL_PROLOGUE_DOMAIN: &[u8] = b"gitpulse-tunnel/v1\0";

/// WS close code sent by `gitpulse-relay` on device revocation or tier downgrade.
pub const WS_CLOSE_REVOKED_OR_DOWNGRADED: u16 = 4003;

/// WS close code sent by `gitpulse-relay` on graceful server drain/restart.
pub const WS_CLOSE_GOING_AWAY: u16 = 1001;

/// Minimum reconnect backoff delay (`250ms`).
pub const RECONNECT_BACKOFF_MIN: Duration = Duration::from_millis(250);

/// Maximum reconnect backoff delay (`15s`).
pub const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(15);

/// Maximum outer frame payload size (`64 KiB` for control/approval).
pub const MAX_OUTER_FRAME_BYTES: usize = 64 * 1024;

/// Maximum `pty` outer frame size (`16 KiB`, never buffered by relay).
pub const MAX_PTY_OUTER_BYTES: usize = 16 * 1024;

/// Rekey threshold per Noise session (100k frames, contract #18).
pub const NOISE_REKEY_FRAME_LIMIT: u64 = 100_000;

/// WireGuard / RFC 6479-style sliding replay window size (1024 frames, Opus #20).
pub const REPLAY_WINDOW_SIZE: u64 = 1024;

/// Domain prefix for hardware P-256 `SignedSteeringConfirm` (Opus #23 / contract #24).
const STEERING_SIGNING_DOMAIN: &[u8] = b"gitpulse-steering/v1\n";

/// Maximum clock skew accepted on `SignedSteeringConfirm`.
const STEERING_SKEW_SECS: i64 = 60;

/// SHA-256 hex of empty body `""`, matching `gitpulse-relay` WS upgrade signing.
pub const EMPTY_BODY_SHA256_HEX: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// Outer frame traffic class visible to `gitpulse-relay` (zero code/paths/commands).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameClass {
    Control,
    Approval,
    Pty,
}

/// Zero-knowledge `GPTF` v1 outer routing envelope visible to `gitpulse-relay`
/// (exact fields from protocol contract #18).
///
/// Must NEVER contain repository names, file paths, diffs, or commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OuterFrame {
    pub v: u8,
    pub frame_id: String,
    /// `"<host_device_id>:<host_epoch>"`
    pub stream_id: String,
    pub seq: u64,
    pub from_device: String,
    pub to_devices: Vec<String>,
    pub ttl_sec: u32,
    pub class: FrameClass,
    pub ciphertext_b64: String,
}

/// Metadata-free wake push frame sent from host to relay (contract #18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakePushFrame {
    pub v: u8,
    /// `"approval"` or `"attention"`
    pub kind: String,
    pub host_device: String,
    pub stream_seq: u64,
    pub expires_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_ct_b64: Option<String>,
}

/// Wire message multiplexed on `/v1/tunnel/ws` text frames.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RelayWireMessage {
    /// Relay cursor-gap notification (`{type: "cursor_gap", current_epoch, oldest_available_seq, latest_seq}`).
    CursorGap(CursorGapEvent),
    /// Metadata-free wake push (`{v: 1, kind, host_device, stream_seq, expires_at, preview_ct_b64?}`).
    WakePush(WakePushFrame),
    /// Zero-knowledge encrypted GPTF v1 outer frame (`{v: 1, frame_id, stream_id, ...}`).
    Frame(OuterFrame),
    /// Noise IK handshake message.
    Handshake(HandshakeWireMessage),
}

/// Exact `cursor_gap` JSON shape from protocol contract #18.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorGapEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub current_epoch: u64,
    pub oldest_available_seq: u64,
    pub latest_seq: u64,
}

impl CursorGapEvent {
    pub fn new(current_epoch: u64, oldest_available_seq: u64, latest_seq: u64) -> Self {
        Self {
            event_type: "cursor_gap".to_string(),
            current_epoch,
            oldest_available_seq,
            latest_seq,
        }
    }

    pub fn is_cursor_gap(&self) -> bool {
        self.event_type == "cursor_gap"
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandshakeWireMessage {
    #[serde(rename = "type")]
    pub event_type: String,
    pub from_device: String,
    pub to_device: String,
    pub stream_id: String,
    pub host_epoch: u64,
    pub phase: String,
    pub payload_b64: String,
}

/// Approval decision kinds permitted from mobile (Opus P0-4 & contract #18).
///
/// `updatedInput` and `updatedPermissions` are deliberately unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecisionKind {
    AllowOnce,
    Deny,
    DenyAndInterrupt,
}

/// Signed approval decision from mobile (contract #18, `deny_unknown_fields`
/// rejects any `updatedInput` / `updatedPermissions` injection!).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedApprovalDecision {
    pub host_id: String,
    pub ticket_id: String,
    pub input_sha256: String,
    pub decision: ApprovalDecisionKind,
    pub nonce: String,
    pub issued_at: i64,
    pub expires_at: i64,
    pub device_id: String,
    pub sig_p256_der_b64: String,
}

/// Hardware P-256 signed steering confirmation from mobile (Opus #23 / contract #24).
///
/// Prevents compromised mobile JS/OTA from injecting arbitrary steering text
/// that the host would otherwise deposit as `HumanConfirmed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedSteeringConfirm {
    pub host_id: String,
    pub session_key: String,
    pub request_id: String,
    pub expect_route: String,
    /// Lowercase hex SHA-256 of the UTF-8 steering text rendered in the native sheet.
    pub text_sha256: String,
    pub issued_at: i64,
    pub expires_at: i64,
    pub nonce: String,
    pub device_id: String,
    pub sig_p256_der_b64: String,
}

impl SignedSteeringConfirm {
    /// Exact signing preimage from Opus #23 / protocol contract #24:
    /// `b"gitpulse-steering/v1\n"` + sorted-key canonical JSON
    /// `{expect_route, expires_at, host_id, issued_at, nonce, request_id, session_key, text_sha256}`.
    pub fn signing_bytes(&self) -> Vec<u8> {
        let canonical = canonicalize_json(&serde_json::json!({
            "expect_route": self.expect_route,
            "expires_at": self.expires_at,
            "host_id": self.host_id,
            "issued_at": self.issued_at,
            "nonce": self.nonce,
            "request_id": self.request_id,
            "session_key": self.session_key,
            "text_sha256": self.text_sha256,
        }));
        let mut out = Vec::with_capacity(STEERING_SIGNING_DOMAIN.len() + canonical.len());
        out.extend_from_slice(STEERING_SIGNING_DOMAIN);
        out.extend_from_slice(canonical.as_bytes());
        out
    }
}

/// Remote steering ingress classification (Opus #23).
///
/// Unverifiable remote text MUST NEVER be deposited as `HumanConfirmed`: it is
/// either refused or downgraded to untrusted `PeerAgent` framing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SteeringIngressVerdict {
    HumanConfirmed {
        device_id: String,
        request_id: String,
        text_sha256: String,
    },
    DowngradedToPeerAgent {
        reason: String,
    },
}

/// Host-side remote steering ingress verifier (Opus #23).
pub struct SteeringIngressGate {
    host_id: String,
    used_nonces: Mutex<HashSet<String>>,
}

impl SteeringIngressGate {
    pub fn new(host_id: impl Into<String>) -> Self {
        Self {
            host_id: host_id.into(),
            used_nonces: Mutex::new(HashSet::new()),
        }
    }

    /// Strictly verifies a remote `SignedSteeringConfirm` against the exact
    /// UTF-8 `text`, Keychain-pinned `DeviceCertificate`, mandatory non-stale
    /// `RevocationCache`, and hardware P-256 signature.
    #[allow(clippy::too_many_arguments)]
    pub fn verify_strict(
        &self,
        signed: &SignedSteeringConfirm,
        text: &str,
        expected_session_key: &str,
        expected_request_id: &str,
        expected_route: &str,
        device_cert: &DeviceCertificate,
        revocations: &RevocationCache,
        now_secs: i64,
    ) -> Result<SteeringIngressVerdict, String> {
        if signed.host_id != self.host_id {
            return Err("steering confirm host_id mismatch".into());
        }
        if signed.session_key != expected_session_key {
            return Err("steering confirm session_key mismatch".into());
        }
        if signed.request_id != expected_request_id {
            return Err("steering confirm request_id mismatch".into());
        }
        if signed.expect_route != expected_route {
            return Err("steering confirm expect_route mismatch".into());
        }
        let actual_hash = sha256_hex(text.as_bytes());
        if actual_hash != signed.text_sha256.to_ascii_lowercase() {
            return Err("steering confirm text_sha256 does not match text".into());
        }
        if signed.nonce.trim().is_empty() || signed.nonce.len() > 128 {
            return Err("steering confirm nonce is empty or oversized".into());
        }
        if signed.issued_at > now_secs + STEERING_SKEW_SECS
            || signed.expires_at <= signed.issued_at
            || now_secs >= signed.expires_at
        {
            return Err("steering confirm timestamp window expired or invalid".into());
        }
        if device_cert.device_id != signed.device_id {
            return Err("steering confirm device_id does not match certificate".into());
        }

        // Mandatory revocation list check (fails closed if missing/stale/revoked).
        let verified = revocations
            .verify_device_certificate(device_cert, now_secs)
            .map_err(|e| format!("certificate/revocation check failed: {e}"))?;
        let approval_key = verified
            .require_approval_key()
            .map_err(|e| format!("missing P-256 approval key: {e}"))?;

        let sig_der = base64::engine::general_purpose::STANDARD
            .decode(signed.sig_p256_der_b64.trim())
            .map_err(|_| "invalid base64 in sig_p256_der_b64".to_string())?;
        let sig = P256Signature::from_der(&sig_der)
            .map_err(|_| "invalid DER P-256 signature".to_string())?;
        approval_key
            .verify(&signed.signing_bytes(), &sig)
            .map_err(|_| "P-256 steering signature verification failed".to_string())?;

        // Commit nonce only after signature verifies.
        let mut nonces = self
            .used_nonces
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !nonces.insert(signed.nonce.clone()) {
            return Err("steering confirm nonce already used (replay)".into());
        }

        Ok(SteeringIngressVerdict::HumanConfirmed {
            device_id: signed.device_id.clone(),
            request_id: signed.request_id.clone(),
            text_sha256: actual_hash,
        })
    }

    /// Classifies an incoming remote steering message: returns `HumanConfirmed`
    /// ONLY if a valid `SignedSteeringConfirm` passes every check; otherwise
    /// downgrades cleanly to `DowngradedToPeerAgent` (never HumanConfirmed!).
    #[allow(clippy::too_many_arguments)]
    pub fn classify_remote_steering(
        &self,
        signed: Option<&SignedSteeringConfirm>,
        text: &str,
        expected_session_key: &str,
        expected_request_id: &str,
        expected_route: &str,
        device_cert: Option<&DeviceCertificate>,
        revocations: &RevocationCache,
        now_secs: i64,
    ) -> SteeringIngressVerdict {
        let (Some(signed), Some(cert)) = (signed, device_cert) else {
            return SteeringIngressVerdict::DowngradedToPeerAgent {
                reason: "missing SignedSteeringConfirm or device certificate".into(),
            };
        };
        match self.verify_strict(
            signed,
            text,
            expected_session_key,
            expected_request_id,
            expected_route,
            cert,
            revocations,
            now_secs,
        ) {
            Ok(verdict) => verdict,
            Err(reason) => SteeringIngressVerdict::DowngradedToPeerAgent { reason },
        }
    }
}

/// Summary of an open approval ticket announced in `inbox_snapshot` (Opus R6, #18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenApprovalTicket {
    pub ticket_id: String,
    pub host_id: String,
    pub session_id: String,
    pub dedup_key: String,
    pub harness: String,
    pub channel: String,
    pub tool_name: String,
    pub input_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_label: Option<String>,
    pub issued_at: i64,
    pub expires_at: i64,
}

/// Inner E2EE `ControlPlaneEvent` carried inside [`OuterFrame::ciphertext_b64`]
/// (`tag = "type"`, snake_case, matching protocol contract #18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlPlaneEvent {
    /// Full source-of-truth re-announcement of all open approval tickets on
    /// every connect/reconnect or `cursor_gap` (Opus R6).
    InboxSnapshot {
        host_device: String,
        host_epoch: u64,
        emitted_at: i64,
        open_tickets: Vec<OpenApprovalTicket>,
        keychain_status: KeychainStatus,
    },
    /// Approval decision from mobile (still carries its own P-256 signature;
    /// tunnel E2EE alone never authorizes `allow_once`, Opus R2).
    ApprovalDecide {
        decision: SignedApprovalDecision,
    },
    /// Host acknowledgement of an approval decision (including `not_applied`).
    ApprovalResolved {
        ticket_id: String,
        host_id: String,
        outcome: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        resolved_at: i64,
    },
    /// Mobile resync request after `cursor_gap`.
    SnapshotResyncReq {
        since_epoch: u64,
        since_seq: u64,
    },
    /// Generic ping/pong keepalive inside the E2EE channel.
    Ping {
        ts_ms: i64,
    },
    Pong {
        ts_ms: i64,
    },
}

/// Persisted state file (`remote-approval.state`) read by hooks/broker to know
/// whether remote approval holds are active or must immediately release to
/// native terminal prompts (Opus R6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RemoteApprovalState {
    Active {
        host_device: String,
        host_epoch: u64,
        revocation_version: u64,
        updated_at_ms: i64,
    },
    ReleasedToNative {
        reason: String,
        updated_at_ms: i64,
    },
}

impl RemoteApprovalState {
    pub fn is_active(&self) -> bool {
        matches!(self, RemoteApprovalState::Active { .. })
    }

    pub fn is_released(&self) -> bool {
        matches!(self, RemoteApprovalState::ReleasedToNative { .. })
    }
}

/// Atomically persists `remote-approval.state` in `state_dir`.
pub fn write_remote_approval_state(
    state_dir: &Path,
    state: &RemoteApprovalState,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(state_dir)
        .map_err(|e| format!("create {}: {e}", state_dir.display()))?;
    let target = state_dir.join(REMOTE_APPROVAL_STATE_FILE);
    let tmp = state_dir.join(format!(
        "{REMOTE_APPROVAL_STATE_FILE}.tmp.{}",
        std::process::id()
    ));
    let bytes = serde_json::to_vec_pretty(state).map_err(|e| format!("serialize state: {e}"))?;
    std::fs::write(&tmp, bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &target).map_err(|e| format!("rename to {}: {e}", target.display()))?;
    Ok(target)
}

/// Reads `remote-approval.state` from `state_dir`. Missing file fails closed as
/// `ReleasedToNative`.
pub fn read_remote_approval_state(state_dir: &Path) -> Result<RemoteApprovalState, String> {
    let path = state_dir.join(REMOTE_APPROVAL_STATE_FILE);
    match std::fs::read_to_string(&path) {
        Ok(raw) => {
            serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(RemoteApprovalState::ReleasedToNative {
                reason: "state_missing".into(),
                updated_at_ms: now_unix_ms(),
            })
        }
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

/// Ed25519 request-signing headers matching `gitpulse-relay/internal/server/deviceauth.go`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedRelayHeaders {
    pub device_id: String,
    pub timestamp_ms: String,
    pub signature_b64: String,
}

/// Builds the canonical signing string `METHOD\nRequestURI\nts\nhex(sha256(body))`
/// and signs it with the host device's Ed25519 key.
pub fn sign_relay_request(
    signing_key: &SigningKey,
    device_id: &str,
    method: &str,
    request_uri: &str,
    timestamp_ms: i64,
    body: &[u8],
) -> SignedRelayHeaders {
    let body_hash_hex = if body.is_empty() {
        EMPTY_BODY_SHA256_HEX.to_string()
    } else {
        let mut h = Sha256::new();
        h.update(body);
        hex_lower(&h.finalize())
    };
    let signing_input = format!("{method}\n{request_uri}\n{timestamp_ms}\n{body_hash_hex}");
    let sig = signing_key.sign(signing_input.as_bytes());
    SignedRelayHeaders {
        device_id: device_id.to_string(),
        timestamp_ms: timestamp_ms.to_string(),
        signature_b64: base64::engine::general_purpose::STANDARD.encode(sig.to_bytes()),
    }
}

/// Builds `/v1/tunnel/ws?role=host&after_epoch=...&after_seq=...` and full WS URL.
pub fn build_host_tunnel_url(
    base_relay_url: &str,
    after_epoch: u64,
    after_seq: u64,
) -> Result<(String, String), String> {
    let base = base_relay_url.trim_end_matches('/');
    let request_uri =
        format!("/v1/tunnel/ws?role=host&after_epoch={after_epoch}&after_seq={after_seq}");
    let full = format!("{base}{request_uri}");
    url::Url::parse(&full).map_err(|e| format!("invalid tunnel url {full:?}: {e}"))?;
    Ok((full, request_uri))
}

/// Builds the exact Noise IK prologue from protocol contract #18:
/// `b"gitpulse-tunnel/v1\0" || stream_id || b"\0" || host_epoch(u64 BE)`.
pub fn noise_tunnel_prologue(stream_id: &str, host_epoch: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(TUNNEL_PROLOGUE_DOMAIN.len() + stream_id.len() + 1 + 8);
    out.extend_from_slice(TUNNEL_PROLOGUE_DOMAIN);
    out.extend_from_slice(stream_id.as_bytes());
    out.push(0u8);
    out.extend_from_slice(&host_epoch.to_be_bytes());
    out
}

/// Errors from Noise IK E2EE session operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelCryptoError {
    Noise(String),
    ReplayOrReorder { high_water: u64, incoming: u64 },
    RekeyRequired,
    EpochMismatch { expected: u64, got: u64 },
    DeviceRevoked(String),
    Revocation(RevocationError),
    PayloadTooLarge(usize),
    InvalidBase64(String),
    Json(String),
}

impl std::fmt::Display for TunnelCryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TunnelCryptoError::Noise(m) => write!(f, "noise error: {m}"),
            TunnelCryptoError::ReplayOrReorder {
                high_water,
                incoming,
            } => write!(
                f,
                "replay/reorder rejected (incoming seq {incoming} <= high-water {high_water})"
            ),
            TunnelCryptoError::RekeyRequired => {
                write!(f, "noise session frame limit reached; fresh handshake required")
            }
            TunnelCryptoError::EpochMismatch { expected, got } => {
                write!(f, "host_epoch mismatch (expected {expected}, got {got})")
            }
            TunnelCryptoError::DeviceRevoked(id) => write!(f, "device '{id}' is revoked"),
            TunnelCryptoError::Revocation(e) => write!(f, "{e}"),
            TunnelCryptoError::PayloadTooLarge(n) => {
                write!(f, "payload size {n} exceeds frame ceiling")
            }
            TunnelCryptoError::InvalidBase64(m) => write!(f, "invalid base64: {m}"),
            TunnelCryptoError::Json(m) => write!(f, "json error: {m}"),
        }
    }
}

impl std::error::Error for TunnelCryptoError {}

/// WireGuard / RFC 6479-style 1024-bit sliding replay window (Opus #20).
///
/// Allows cross-class out-of-order frames within `[high_water - 1023, high_water]`
/// (so e.g. an `approval` frame with `seq=10` arriving after a `pty` frame with
/// `seq=11` is accepted once) while rejecting duplicates and frames below the
/// window. Call [`Self::record`] ONLY after AEAD authentication succeeds.
#[derive(Debug, Clone, Default)]
pub struct ReplayWindow {
    high_water: u64,
    bitmap: [u64; 16], // 16 * 64 = 1024 bits
}

impl ReplayWindow {
    pub fn check(&self, seq: u64) -> Result<(), TunnelCryptoError> {
        if seq == 0 {
            return Err(TunnelCryptoError::ReplayOrReorder {
                high_water: self.high_water,
                incoming: seq,
            });
        }
        if seq > self.high_water {
            return Ok(());
        }
        let diff = self.high_water - seq;
        if diff >= REPLAY_WINDOW_SIZE {
            return Err(TunnelCryptoError::ReplayOrReorder {
                high_water: self.high_water,
                incoming: seq,
            });
        }
        let word = (diff / 64) as usize;
        let bit = diff % 64;
        if (self.bitmap[word] & (1u64 << bit)) != 0 {
            return Err(TunnelCryptoError::ReplayOrReorder {
                high_water: self.high_water,
                incoming: seq,
            });
        }
        Ok(())
    }

    /// Commits `seq` into the window; MUST be called only after AEAD verifies.
    pub fn record(&mut self, seq: u64) {
        if seq == 0 {
            return;
        }
        if seq > self.high_water {
            let shift = seq - self.high_water;
            if shift >= REPLAY_WINDOW_SIZE {
                self.bitmap = [0u64; 16];
            } else {
                let word_shift = (shift / 64) as usize;
                let bit_shift = (shift % 64) as u32;
                let mut next = [0u64; 16];
                for i in 0..16 {
                    let dst = i + word_shift;
                    if dst < 16 {
                        next[dst] |= self.bitmap[i] << bit_shift;
                        if bit_shift > 0 && dst + 1 < 16 {
                            next[dst + 1] |= self.bitmap[i] >> (64 - bit_shift);
                        }
                    }
                }
                self.bitmap = next;
            }
            self.high_water = seq;
            self.bitmap[0] |= 1u64;
        } else {
            let diff = self.high_water - seq;
            if diff < REPLAY_WINDOW_SIZE {
                let word = (diff / 64) as usize;
                let bit = diff % 64;
                self.bitmap[word] |= 1u64 << bit;
            }
        }
    }

    pub fn high_water(&self) -> u64 {
        self.high_water
    }
}

/// Authenticated Noise `IK` transport session bound to one paired mobile device,
/// `stream_id`, and `host_epoch`, with a 1024-bit sliding replay window
/// (exact contract #18/#20 wire format: `seq(u64 BE, >=1) || noise_aead_ct`).
pub struct NoiseTunnelSession {
    peer_device_id: String,
    stream_id: String,
    host_epoch: u64,
    transport: snow::StatelessTransportState,
    send_seq: u64,
    recv_window: ReplayWindow,
}

impl NoiseTunnelSession {
    /// Initiator side (static X25519 key pinned from peer's `DeviceCertificate`):
    /// returns `(handshake_msg_1, pending)`.
    pub fn start_initiator(
        local_static_x25519: &[u8; 32],
        peer_static_x25519: &[u8; 32],
        peer_device_id: impl Into<String>,
        stream_id: impl Into<String>,
        host_epoch: u64,
    ) -> Result<(Vec<u8>, PendingInitiator), TunnelCryptoError> {
        let stream_id = stream_id.into();
        let prologue = noise_tunnel_prologue(&stream_id, host_epoch);
        let params: snow::params::NoiseParams = NOISE_IK_PARAMS
            .parse()
            .map_err(|e| TunnelCryptoError::Noise(format!("parse params: {e}")))?;
        let mut hs = snow::Builder::new(params)
            .local_private_key(local_static_x25519)
            .map_err(|e| TunnelCryptoError::Noise(format!("local key: {e}")))?
            .remote_public_key(peer_static_x25519)
            .map_err(|e| TunnelCryptoError::Noise(format!("remote key: {e}")))?
            .prologue(&prologue)
            .map_err(|e| TunnelCryptoError::Noise(format!("prologue: {e}")))?
            .build_initiator()
            .map_err(|e| TunnelCryptoError::Noise(format!("build initiator: {e}")))?;

        let mut buf = vec![0u8; 256];
        let len = hs
            .write_message(&[], &mut buf)
            .map_err(|e| TunnelCryptoError::Noise(format!("write msg1: {e}")))?;
        buf.truncate(len);
        Ok((
            buf,
            PendingInitiator {
                peer_device_id: peer_device_id.into(),
                stream_id,
                host_epoch,
                hs,
            },
        ))
    }

    /// Responder side (host accepting mobile's Noise `IK` message 1): verifies
    /// the peer's static X25519 key revealed by `IK` matches the
    /// `DeviceCertificate`-pinned `expected_peer.x25519_pub`.
    pub fn accept_responder(
        local_static_x25519: &[u8; 32],
        expected_peer: &VerifiedDevice,
        stream_id: impl Into<String>,
        host_epoch: u64,
        msg1: &[u8],
    ) -> Result<(Vec<u8>, Self), TunnelCryptoError> {
        let stream_id = stream_id.into();
        let prologue = noise_tunnel_prologue(&stream_id, host_epoch);
        let params: snow::params::NoiseParams = NOISE_IK_PARAMS
            .parse()
            .map_err(|e| TunnelCryptoError::Noise(format!("parse params: {e}")))?;
        let mut hs = snow::Builder::new(params)
            .local_private_key(local_static_x25519)
            .map_err(|e| TunnelCryptoError::Noise(format!("local key: {e}")))?
            .prologue(&prologue)
            .map_err(|e| TunnelCryptoError::Noise(format!("prologue: {e}")))?
            .build_responder()
            .map_err(|e| TunnelCryptoError::Noise(format!("build responder: {e}")))?;

        // Noise IK msg1 has only weak forward secrecy (Opus #20 item 5): enforce
        // empty payload so no secret can ever be placed in msg1.
        let mut payload_buf = vec![0u8; 256];
        let payload_len = hs
            .read_message(msg1, &mut payload_buf)
            .map_err(|e| TunnelCryptoError::Noise(format!("read msg1: {e}")))?;
        if payload_len != 0 {
            return Err(TunnelCryptoError::Noise(
                "Noise IK msg1 payload must be empty".into(),
            ));
        }

        let remote_pub = hs
            .get_remote_static()
            .ok_or_else(|| TunnelCryptoError::Noise("missing remote static key in Noise IK".into()))?;
        if remote_pub != expected_peer.x25519_pub.as_slice() {
            return Err(TunnelCryptoError::Noise(
                "Noise IK remote static key does not match pinned DeviceCertificate".into(),
            ));
        }

        let mut resp_buf = vec![0u8; 256];
        let len = hs
            .write_message(&[], &mut resp_buf)
            .map_err(|e| TunnelCryptoError::Noise(format!("write msg2: {e}")))?;
        resp_buf.truncate(len);

        let transport = hs
            .into_stateless_transport_mode()
            .map_err(|e| TunnelCryptoError::Noise(format!("transport mode: {e}")))?;
        Ok((
            resp_buf,
            Self {
                peer_device_id: expected_peer.device_id.clone(),
                stream_id,
                host_epoch,
                transport,
                send_seq: 0,
                recv_window: ReplayWindow::default(),
            },
        ))
    }

    pub fn peer_device_id(&self) -> &str {
        &self.peer_device_id
    }

    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    pub fn host_epoch(&self) -> u64 {
        self.host_epoch
    }

    /// Encrypts plaintext into an outbound `GPTF` v1 [`OuterFrame`] using
    /// contract #18 transport wire format `seq(u64 BE, >=1) || noise_aead_ct`.
    pub fn seal_frame(
        &mut self,
        from_device: &str,
        class: FrameClass,
        ttl_sec: u32,
        plaintext: &[u8],
    ) -> Result<OuterFrame, TunnelCryptoError> {
        if self.send_seq >= NOISE_REKEY_FRAME_LIMIT {
            return Err(TunnelCryptoError::RekeyRequired);
        }
        let max_plain = match class {
            FrameClass::Pty => MAX_PTY_OUTER_BYTES.saturating_sub(32),
            FrameClass::Control | FrameClass::Approval => MAX_OUTER_FRAME_BYTES.saturating_sub(64),
        };
        if plaintext.len() > max_plain {
            return Err(TunnelCryptoError::PayloadTooLarge(plaintext.len()));
        }
        self.send_seq += 1;
        let seq = self.send_seq;

        let mut aead_ct = vec![0u8; plaintext.len() + 32];
        let n = self
            .transport
            .write_message(seq, plaintext, &mut aead_ct)
            .map_err(|e| TunnelCryptoError::Noise(format!("encrypt: {e}")))?;
        aead_ct.truncate(n);

        // Wire ciphertext: seq(u64 BE, >=1) || noise_aead_ct (contract #18)
        let mut wire_ct = Vec::with_capacity(8 + aead_ct.len());
        wire_ct.extend_from_slice(&seq.to_be_bytes());
        wire_ct.extend_from_slice(&aead_ct);

        Ok(OuterFrame {
            v: 1,
            frame_id: format!("{}:{seq}", self.stream_id),
            stream_id: self.stream_id.clone(),
            seq,
            from_device: from_device.to_string(),
            to_devices: vec![self.peer_device_id.clone()],
            ttl_sec,
            class,
            ciphertext_b64: base64::engine::general_purpose::STANDARD.encode(&wire_ct),
        })
    }

    /// Decrypts an inbound `GPTF` v1 [`OuterFrame`] (`seq(u64 BE, >=1) || noise_aead_ct`),
    /// enforcing strict monotonic `seq > recv_high_water` replay/reorder protection
    /// and `host_epoch` binding.
    pub fn open_frame(&mut self, frame: &OuterFrame) -> Result<Vec<u8>, TunnelCryptoError> {
        if frame.v != 1 {
            return Err(TunnelCryptoError::Noise(format!(
                "unsupported GPTF version {}",
                frame.v
            )));
        }
        if let Some((_, epoch_str)) = frame.stream_id.rsplit_once(':') {
            if let Ok(epoch) = epoch_str.parse::<u64>() {
                if epoch != self.host_epoch {
                    return Err(TunnelCryptoError::EpochMismatch {
                        expected: self.host_epoch,
                        got: epoch,
                    });
                }
            }
        }
        self.recv_window.check(frame.seq)?;

        let wire_ct = base64::engine::general_purpose::STANDARD
            .decode(frame.ciphertext_b64.trim())
            .map_err(|e| TunnelCryptoError::InvalidBase64(e.to_string()))?;
        let max_wire = match frame.class {
            FrameClass::Pty => MAX_PTY_OUTER_BYTES,
            FrameClass::Control | FrameClass::Approval => MAX_OUTER_FRAME_BYTES,
        };
        if wire_ct.len() > max_wire {
            return Err(TunnelCryptoError::PayloadTooLarge(wire_ct.len()));
        }
        if wire_ct.len() < 8 + 16 {
            return Err(TunnelCryptoError::Noise("truncated transport wire frame".into()));
        }

        let wire_seq = u64::from_be_bytes(
            wire_ct[..8]
                .try_into()
                .map_err(|_| TunnelCryptoError::Noise("invalid wire seq prefix".into()))?,
        );
        if wire_seq != frame.seq {
            return Err(TunnelCryptoError::Noise(format!(
                "outer seq {} does not match wire seq prefix {wire_seq}",
                frame.seq
            )));
        }
        self.recv_window.check(wire_seq)?;

        let aead_ct = &wire_ct[8..];
        let mut pt = vec![0u8; aead_ct.len()];
        let n = self
            .transport
            .read_message(wire_seq, aead_ct, &mut pt)
            .map_err(|e| TunnelCryptoError::Noise(format!("decrypt: {e}")))?;
        pt.truncate(n);

        // Advance replay window ONLY AFTER AEAD verification succeeds (Opus #20).
        self.recv_window.record(wire_seq);
        Ok(pt)
    }
}

/// Pending Noise IK initiator awaiting responder message 2.
pub struct PendingInitiator {
    peer_device_id: String,
    stream_id: String,
    host_epoch: u64,
    hs: snow::HandshakeState,
}

impl PendingInitiator {
    pub fn finish(mut self, msg2: &[u8]) -> Result<NoiseTunnelSession, TunnelCryptoError> {
        let mut buf = vec![0u8; 256];
        let payload_len = self
            .hs
            .read_message(msg2, &mut buf)
            .map_err(|e| TunnelCryptoError::Noise(format!("read msg2: {e}")))?;
        if payload_len != 0 {
            return Err(TunnelCryptoError::Noise(
                "Noise IK msg2 payload must be empty".into(),
            ));
        }
        let transport = self
            .hs
            .into_stateless_transport_mode()
            .map_err(|e| TunnelCryptoError::Noise(format!("transport mode: {e}")))?;
        Ok(NoiseTunnelSession {
            peer_device_id: self.peer_device_id,
            stream_id: self.stream_id,
            host_epoch: self.host_epoch,
            transport,
            send_seq: 0,
            recv_window: ReplayWindow::default(),
        })
    }
}

/// Manages per-mobile-device Noise IK sessions on the host, tied to a single
/// `host_epoch` and a [`RevocationCache`].
pub struct HostNoiseManager {
    host_device_id: String,
    host_epoch: u64,
    sessions: HashMap<String, NoiseTunnelSession>,
    /// Cheap per-epoch replay guard for Noise IK msg1 (Opus #20 item 5).
    seen_msg1_hashes: HashSet<[u8; 32]>,
}

impl HostNoiseManager {
    pub fn new(host_device_id: impl Into<String>, host_epoch: u64) -> Self {
        Self {
            host_device_id: host_device_id.into(),
            host_epoch,
            sessions: HashMap::new(),
            seen_msg1_hashes: HashSet::new(),
        }
    }

    pub fn host_epoch(&self) -> u64 {
        self.host_epoch
    }

    pub fn stream_id(&self) -> String {
        format!("{}:{}", self.host_device_id, self.host_epoch)
    }

    /// Bumps `host_epoch` and drops all prior Noise sessions so every device
    /// must perform a fresh Noise IK handshake (Opus R1).
    pub fn advance_epoch(&mut self) -> u64 {
        self.host_epoch += 1;
        self.sessions.clear();
        self.seen_msg1_hashes.clear();
        self.host_epoch
    }

    /// Completes a responder Noise IK handshake for `cert`, verifying it against
    /// `revocations` first and cheaply dropping duplicate msg1 replays within
    /// the current epoch before DH.
    pub fn accept_mobile(
        &mut self,
        host_noise_static_x25519: &[u8; 32],
        cert: &DeviceCertificate,
        revocations: &RevocationCache,
        msg1: &[u8],
        now_secs: i64,
    ) -> Result<Vec<u8>, TunnelCryptoError> {
        let mut h = Sha256::new();
        h.update(msg1);
        let digest: [u8; 32] = h.finalize().into();
        if self.seen_msg1_hashes.contains(&digest) {
            return Err(TunnelCryptoError::Noise(
                "duplicate Noise IK msg1 dropped within current host_epoch".into(),
            ));
        }
        let verified = revocations
            .verify_device_certificate(cert, now_secs)
            .map_err(TunnelCryptoError::Revocation)?;
        let stream_id = self.stream_id();
        let (resp, session) = NoiseTunnelSession::accept_responder(
            host_noise_static_x25519,
            &verified,
            stream_id,
            self.host_epoch,
            msg1,
        )?;
        self.seen_msg1_hashes.insert(digest);
        self.sessions.insert(verified.device_id, session);
        Ok(resp)
    }

    /// Inserts an already-established session (useful for tests or initiator flow).
    pub fn insert_session(&mut self, session: NoiseTunnelSession) {
        self.sessions
            .insert(session.peer_device_id().to_string(), session);
    }

    /// Drops any session whose device ID appears in `revocations`.
    pub fn evict_revoked(&mut self, revocations: &RevocationCache) -> Vec<String> {
        let revoked: Vec<String> = self
            .sessions
            .keys()
            .filter(|id| revocations.is_revoked(id))
            .cloned()
            .collect();
        for id in &revoked {
            self.sessions.remove(id);
        }
        revoked
    }

    /// Encrypts a [`ControlPlaneEvent`] for every currently active paired session.
    pub fn broadcast_event(
        &mut self,
        class: FrameClass,
        ttl_sec: u32,
        event: &ControlPlaneEvent,
    ) -> Result<Vec<OuterFrame>, TunnelCryptoError> {
        let bytes =
            serde_json::to_vec(event).map_err(|e| TunnelCryptoError::Json(e.to_string()))?;
        let mut frames = Vec::with_capacity(self.sessions.len());
        for session in self.sessions.values_mut() {
            frames.push(session.seal_frame(&self.host_device_id, class, ttl_sec, &bytes)?);
        }
        Ok(frames)
    }

    /// Decrypts an inbound [`OuterFrame`] from `frame.from_device`.
    pub fn open_from_mobile(
        &mut self,
        frame: &OuterFrame,
        revocations: &RevocationCache,
    ) -> Result<ControlPlaneEvent, TunnelCryptoError> {
        if revocations.is_revoked(&frame.from_device) {
            self.sessions.remove(&frame.from_device);
            return Err(TunnelCryptoError::DeviceRevoked(frame.from_device.clone()));
        }
        let session = self.sessions.get_mut(&frame.from_device).ok_or_else(|| {
            TunnelCryptoError::Noise(format!("no Noise session for '{}'", frame.from_device))
        })?;
        let plain = session.open_frame(frame)?;
        serde_json::from_slice(&plain).map_err(|e| TunnelCryptoError::Json(e.to_string()))
    }
}

/// Mandatory jittered exponential reconnect backoff (`250ms..15s`).
#[derive(Debug, Clone)]
pub struct ReconnectBackoff {
    attempt: u32,
    min: Duration,
    max: Duration,
}

impl Default for ReconnectBackoff {
    fn default() -> Self {
        Self {
            attempt: 0,
            min: RECONNECT_BACKOFF_MIN,
            max: RECONNECT_BACKOFF_MAX,
        }
    }
}

impl ReconnectBackoff {
    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// Returns the next jittered delay strictly within `[250ms, 15s]`.
    pub fn next_delay(&mut self) -> Duration {
        let mut entropy = [0u8; 8];
        let _ = getrandom::fill(&mut entropy);
        let rand_u64 = u64::from_le_bytes(entropy);
        self.next_delay_with_entropy(rand_u64)
    }

    /// Deterministic variant for unit testing jitter bounds.
    pub fn next_delay_with_entropy(&mut self, entropy: u64) -> Duration {
        let min_ms = self.min.as_millis() as u64;
        let max_ms = self.max.as_millis() as u64;
        let shift = self.attempt.min(16);
        let exp_ceiling = min_ms.saturating_mul(1u64 << shift).min(max_ms);
        self.attempt = self.attempt.saturating_add(1);
        let span = exp_ceiling.saturating_sub(min_ms);
        let jittered = if span == 0 {
            min_ms
        } else {
            min_ms + (entropy % (span + 1))
        };
        Duration::from_millis(jittered.clamp(min_ms, max_ms))
    }
}

/// Supplies current open approval tickets so the host can re-announce them on
/// every connect/reconnect (`inbox_snapshot`, Opus R6).
pub trait InboxSnapshotProvider: Send + Sync {
    fn open_tickets(&self) -> Vec<OpenApprovalTicket>;
}

/// Simple thread-safe snapshot provider backed by an in-memory ticket list.
#[derive(Debug, Default)]
pub struct StaticInboxSnapshotProvider {
    tickets: Mutex<Vec<OpenApprovalTicket>>,
}

impl StaticInboxSnapshotProvider {
    pub fn new(tickets: Vec<OpenApprovalTicket>) -> Self {
        Self {
            tickets: Mutex::new(tickets),
        }
    }

    pub fn set_tickets(&self, next: Vec<OpenApprovalTicket>) {
        if let Ok(mut guard) = self.tickets.lock() {
            *guard = next;
        }
    }
}

impl InboxSnapshotProvider for StaticInboxSnapshotProvider {
    fn open_tickets(&self) -> Vec<OpenApprovalTicket> {
        self.tickets.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

/// Outcome of a single outbound tunnel connection attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelSessionEnd {
    /// Relay closed with `4003` (revoked/downgraded); `remote-approval.state`
    /// has already been flipped to `ReleasedToNative`.
    RevokedOrDowngraded(String),
    /// Relay sent `CloseGoingAway (1001)` — caller must reconnect with jittered backoff.
    GoingAway(String),
    /// Keychain is locked — caller surfaces `keychain_locked` without hanging.
    KeychainLocked(String),
    /// Revocation cache is stale past TTL — remote holds released.
    RevocationStale,
    /// Graceful shutdown requested via watch channel.
    Shutdown,
    /// Transient transport/network error — caller may reconnect with jittered backoff.
    TransportError(String),
}

/// Configuration for running an outbound host WSS tunnel connection.
pub struct HostTunnelParams {
    pub relay_url: String,
    pub host_device_id: String,
    pub state_dir: PathBuf,
    pub after_epoch: u64,
    pub after_seq: u64,
}

/// Runs one outbound WSS connection to `gitpulse-relay`:
/// 1. Verifies Keychain is unlocked (non-blocking) and revocation cache is fresh.
/// 2. Signs WS upgrade headers with host Ed25519 key.
/// 3. Connects outbound-only via `tokio-tungstenite`.
/// 4. Writes `Active` to `remote-approval.state` and emits `inbox_snapshot` to
///    all active paired Noise sessions (Opus R6).
/// 5. Processes inbound frames, `cursor_gap` (re-emitting `inbox_snapshot`),
///    and WS close `4003` (immediately flipping `remote-approval.state` to
///    `ReleasedToNative`!).
#[allow(clippy::too_many_arguments)]
pub async fn run_host_tunnel_once(
    params: &HostTunnelParams,
    trust: &dyn TrustAnchorProvider,
    revocations: &Arc<Mutex<RevocationCache>>,
    noise_mgr: &Arc<Mutex<HostNoiseManager>>,
    snapshot_provider: &dyn InboxSnapshotProvider,
    inbound_tx: Option<&mpsc::Sender<(String, ControlPlaneEvent)>>,
    mut outbound_rx: Option<&mut mpsc::Receiver<RelayWireMessage>>,
    mut shutdown: watch::Receiver<bool>,
) -> TunnelSessionEnd {
    match trust.keychain_status() {
        KeychainStatus::KeychainLocked { reason } => {
            let _ = write_remote_approval_state(
                &params.state_dir,
                &RemoteApprovalState::ReleasedToNative {
                    reason: format!("keychain_locked: {reason}"),
                    updated_at_ms: now_unix_ms(),
                },
            );
            return TunnelSessionEnd::KeychainLocked(reason);
        }
        KeychainStatus::Unavailable { reason } => {
            let _ = write_remote_approval_state(
                &params.state_dir,
                &RemoteApprovalState::ReleasedToNative {
                    reason: format!("keychain_unavailable: {reason}"),
                    updated_at_ms: now_unix_ms(),
                },
            );
            return TunnelSessionEnd::KeychainLocked(reason);
        }
        KeychainStatus::Unlocked { .. } => {}
    }

    let now_secs = now_unix_secs();
    let rev_version = {
        let guard = revocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.should_bypass_remote_holds(now_secs) {
            drop(guard);
            let _ = write_remote_approval_state(
                &params.state_dir,
                &RemoteApprovalState::ReleasedToNative {
                    reason: "revocation_list_stale".into(),
                    updated_at_ms: now_unix_ms(),
                },
            );
            return TunnelSessionEnd::RevocationStale;
        }
        guard.current_version()
    };

    let seed = match trust.host_device_ed25519_seed() {
        Ok(seed) => seed,
        Err(TrustAnchorError::KeychainLocked(reason))
        | Err(TrustAnchorError::Unavailable(reason)) => {
            let _ = write_remote_approval_state(
                &params.state_dir,
                &RemoteApprovalState::ReleasedToNative {
                    reason: format!("keychain_locked: {reason}"),
                    updated_at_ms: now_unix_ms(),
                },
            );
            return TunnelSessionEnd::KeychainLocked(reason);
        }
        Err(err) => return TunnelSessionEnd::TransportError(err.to_string()),
    };

    let (full_url, request_uri) =
        match build_host_tunnel_url(&params.relay_url, params.after_epoch, params.after_seq) {
            Ok(pair) => pair,
            Err(err) => return TunnelSessionEnd::TransportError(err),
        };

    let signing_key = SigningKey::from_bytes(&seed);
    let headers = sign_relay_request(
        &signing_key,
        &params.host_device_id,
        "GET",
        &request_uri,
        now_unix_ms(),
        &[],
    );

    let mut req = match full_url.as_str().into_client_request() {
        Ok(r) => r,
        Err(e) => return TunnelSessionEnd::TransportError(format!("build ws request: {e}")),
    };
    let hmap = req.headers_mut();
    if let (Ok(dev), Ok(ts), Ok(sig)) = (
        headers.device_id.parse(),
        headers.timestamp_ms.parse(),
        headers.signature_b64.parse(),
    ) {
        hmap.insert("X-GitPulse-Device", dev);
        hmap.insert("X-GitPulse-Timestamp", ts);
        hmap.insert("X-GitPulse-Signature", sig);
    }

    let (ws_stream, _resp) = match tokio_tungstenite::connect_async(req).await {
        Ok(pair) => pair,
        Err(e) => return TunnelSessionEnd::TransportError(format!("ws connect failed: {e}")),
    };

    let host_epoch = noise_mgr
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .host_epoch();

    let _ = write_remote_approval_state(
        &params.state_dir,
        &RemoteApprovalState::Active {
            host_device: params.host_device_id.clone(),
            host_epoch,
            revocation_version: rev_version,
            updated_at_ms: now_unix_ms(),
        },
    );

    let (mut ws_sink, mut ws_source) = ws_stream.split();

    // Opus R6: Host is source of truth — emit full `inbox_snapshot` on every
    // connect/reconnect re-announcing all open approval tickets.
    if let Err(err) = emit_inbox_snapshot(
        &mut ws_sink,
        &params.host_device_id,
        trust.keychain_status(),
        noise_mgr,
        snapshot_provider,
    )
    .await
    {
        return TunnelSessionEnd::TransportError(err);
    }

    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    let _ = ws_sink.send(Message::Close(None)).await;
                    let _ = write_remote_approval_state(
                        &params.state_dir,
                        &RemoteApprovalState::ReleasedToNative {
                            reason: "daemon_shutdown".into(),
                            updated_at_ms: now_unix_ms(),
                        },
                    );
                    return TunnelSessionEnd::Shutdown;
                }
            }
            maybe_out = async {
                match outbound_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if let Some(msg) = maybe_out {
                    if let Ok(json) = serde_json::to_string(&msg) {
                        if let Err(e) = ws_sink.send(Message::Text(json.into())).await {
                            return TunnelSessionEnd::TransportError(format!("ws send: {e}"));
                        }
                    }
                }
            }
            maybe_in = ws_source.next() => {
                match maybe_in {
                    None => {
                        return TunnelSessionEnd::TransportError("ws stream closed".into());
                    }
                    Some(Err(e)) => {
                        return TunnelSessionEnd::TransportError(format!("ws read: {e}"));
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if let Err(e) = ws_sink.send(Message::Pong(payload)).await {
                            return TunnelSessionEnd::TransportError(format!("ws pong: {e}"));
                        }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        let (code, reason) = frame
                            .map(|f| (u16::from(f.code), f.reason.to_string()))
                            .unwrap_or((u16::from(CloseCode::Normal), String::new()));
                        if code == WS_CLOSE_REVOKED_OR_DOWNGRADED {
                            let _ = write_remote_approval_state(
                                &params.state_dir,
                                &RemoteApprovalState::ReleasedToNative {
                                    reason: if reason.is_empty() {
                                        "relay_close_4003".into()
                                    } else {
                                        format!("relay_close_4003: {reason}")
                                    },
                                    updated_at_ms: now_unix_ms(),
                                },
                            );
                            return TunnelSessionEnd::RevokedOrDowngraded(reason);
                        }
                        if code == WS_CLOSE_GOING_AWAY {
                            return TunnelSessionEnd::GoingAway(reason);
                        }
                        return TunnelSessionEnd::TransportError(format!(
                            "ws closed ({code}): {reason}"
                        ));
                    }
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(wire) = serde_json::from_str::<RelayWireMessage>(&text) {
                            match wire {
                                RelayWireMessage::CursorGap(gap) if gap.is_cursor_gap() => {
                                    if let Err(err) = emit_inbox_snapshot(
                                        &mut ws_sink,
                                        &params.host_device_id,
                                        trust.keychain_status(),
                                        noise_mgr,
                                        snapshot_provider,
                                    )
                                    .await
                                    {
                                        return TunnelSessionEnd::TransportError(err);
                                    }
                                }
                                RelayWireMessage::Frame(outer) => {
                                    let opened = {
                                        let rev = revocations
                                            .lock()
                                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                                        let mut mgr = noise_mgr
                                            .lock()
                                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                                        mgr.open_from_mobile(&outer, &rev)
                                    };
                                    if let Ok(event) = opened {
                                        if matches!(event, ControlPlaneEvent::SnapshotResyncReq { .. }) {
                                            let _ = emit_inbox_snapshot(
                                                &mut ws_sink,
                                                &params.host_device_id,
                                                trust.keychain_status(),
                                                noise_mgr,
                                                snapshot_provider,
                                            )
                                            .await;
                                        }
                                        if let Some(tx) = inbound_tx {
                                            let _ = tx.send((outer.from_device, event)).await;
                                        }
                                    }
                                }
                                RelayWireMessage::CursorGap(_)
                                | RelayWireMessage::Handshake(_)
                                | RelayWireMessage::WakePush(_) => {}
                            }
                        }
                    }
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

async fn emit_inbox_snapshot<S, E>(
    ws_sink: &mut S,
    host_device_id: &str,
    keychain_status: KeychainStatus,
    noise_mgr: &Arc<Mutex<HostNoiseManager>>,
    snapshot_provider: &dyn InboxSnapshotProvider,
) -> Result<(), String>
where
    S: futures_util::Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
{
    let open_tickets = snapshot_provider.open_tickets();
    let frames = {
        let mut mgr = noise_mgr
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let snapshot = ControlPlaneEvent::InboxSnapshot {
            host_device: host_device_id.to_string(),
            host_epoch: mgr.host_epoch(),
            emitted_at: now_unix_secs(),
            open_tickets,
            keychain_status,
        };
        mgr.broadcast_event(FrameClass::Control, 300, &snapshot)
            .map_err(|e| e.to_string())?
    };

    for frame in frames {
        let text = serde_json::to_string(&frame).map_err(|e| e.to_string())?;
        ws_sink
            .send(Message::Text(text.into()))
            .await
            .map_err(|e| format!("send inbox_snapshot: {e}"))?;
    }
    Ok(())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

pub(crate) fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
