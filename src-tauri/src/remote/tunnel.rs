//! Outbound-only zero-knowledge E2EE WebSocket relay tunnel (`gitpulsed --relay`).
//!
//! All Noise `IK` handshakes, 1024-bit sliding replay protection, `GPTF` v1
//! outer frames, Ed25519 WS request signing, and hardware P-256 approval &
//! steering verification delegate directly to the single audited
//! `gitpulse-protocol` crate (Opus R1/R2/R6, #20, #23, #31).

use crate::remote::config::{KeychainStatus, REMOTE_APPROVAL_STATE_FILE};
use crate::remote::revocations::{RevocationCache, TrustAnchorError, TrustAnchorProvider};
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
pub use gitpulse_protocol::canonical::steering_text_sha256_hex;
pub use gitpulse_protocol::crypto::{
    sign_relay_request, verify_approval_decision, verify_steering_confirm,
    EpochHandshakeReplayGuard, NoiseHandshake, RelayAuthHeaders, RevocationPolicy, TunnelSession,
    DEVICE_HEADER, NOISE_PARAMS, REPLAY_WINDOW_SIZE, SIGNATURE_HEADER, TIMESTAMP_HEADER,
};
pub use gitpulse_protocol::wire::{
    ApprovalChannel, ApprovalDecisionKind, ApprovalOutcome, ApprovalResolvedEvent, ApprovalTicket,
    CapabilitySet, ControlPlaneEvent, DeviceCertificate, FrameClass, OuterFrame, SessionSnapshot,
    SignedApprovalDecision, SignedSteeringConfirm, SubagentSummary, WakePushFrame, GPTF_VERSION,
    MAX_CONTROL_FRAME_BYTES, MAX_PTY_FRAME_BYTES,
};
use gitpulse_protocol::ProtocolError;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::Message;

/// WS close code sent by `gitpulse-relay` on device revocation or tier downgrade.
pub const WS_CLOSE_REVOKED_OR_DOWNGRADED: u16 = 4003;

/// WS close code sent by `gitpulse-relay` on graceful server drain/restart.
pub const WS_CLOSE_GOING_AWAY: u16 = 1001;

/// Minimum reconnect backoff delay (`250ms`).
pub const RECONNECT_BACKOFF_MIN: Duration = Duration::from_millis(250);

/// Maximum reconnect backoff delay (`15s`).
pub const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(15);

/// Maximum per-epoch cached msg1 digests in [`EpochHandshakeReplayGuard`].
const MSG1_REPLAY_GUARD_CAPACITY: usize = 256;

/// Wire message multiplexed on `/v1/tunnel/ws` text frames.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RelayWireMessage {
    /// Relay cursor-gap event (`{type: "cursor_gap", current_epoch, oldest_available_seq, latest_seq}`).
    CursorGap(Box<ControlPlaneEvent>),
    /// Metadata-free wake push (`{v: 1, kind, host_device, stream_seq, expires_at, preview_ct_b64?}`).
    WakePush(WakePushFrame),
    /// Zero-knowledge encrypted GPTF v1 outer frame (`{v: 1, frame_id, stream_id, ...}`).
    Frame(OuterFrame),
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
///
/// Delegates P-256 + `DeviceCertificate` + mandatory `RevocationPolicy::Enforced`
/// verification directly to `gitpulse_protocol::crypto::verify_steering_confirm`
/// and tracks single-use nonces in memory.
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

    /// Strictly verifies a remote [`SignedSteeringConfirm`] via `gitpulse-protocol`
    /// with a mandatory non-stale [`RevocationCache`].
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
    ) -> Result<SteeringIngressVerdict, ProtocolError> {
        let policy = revocations.enforced_policy(now_secs)?;
        verify_steering_confirm(
            signed,
            text,
            device_cert,
            revocations.identity_pub_ed25519(),
            &self.host_id,
            expected_session_key,
            expected_request_id,
            expected_route,
            policy,
            now_secs,
        )?;

        // Commit nonce only after P-256 + cert + revocation verification succeeds.
        let mut nonces = self
            .used_nonces
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !nonces.insert(signed.nonce.clone()) {
            return Err(ProtocolError::ReplayDetected {
                got: 0,
                high_water: 0,
            });
        }

        Ok(SteeringIngressVerdict::HumanConfirmed {
            device_id: signed.device_id.clone(),
            request_id: signed.request_id.clone(),
            text_sha256: steering_text_sha256_hex(text),
        })
    }

    /// Classifies an incoming remote steering message: returns `HumanConfirmed`
    /// ONLY if a valid [`SignedSteeringConfirm`] passes every `gitpulse-protocol`
    /// check; otherwise downgrades cleanly to `DowngradedToPeerAgent`.
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
            Err(err) => SteeringIngressVerdict::DowngradedToPeerAgent {
                reason: err.to_string(),
            },
        }
    }
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
        Ok(raw) => serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(RemoteApprovalState::ReleasedToNative {
                reason: "state_missing".into(),
                updated_at_ms: now_unix_ms(),
            })
        }
        Err(e) => Err(format!("read {}: {e}", path.display())),
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

/// Manages per-mobile-device `gitpulse_protocol::crypto::TunnelSession`s on the
/// host, tied to a single `host_epoch`, `EpochHandshakeReplayGuard`, and
/// mandatory [`RevocationCache`].
pub struct HostNoiseManager {
    host_device_id: String,
    host_epoch: u64,
    sessions: HashMap<String, TunnelSession>,
    msg1_guard: EpochHandshakeReplayGuard,
}

impl HostNoiseManager {
    pub fn new(host_device_id: impl Into<String>, host_epoch: u64) -> Self {
        Self {
            host_device_id: host_device_id.into(),
            host_epoch,
            sessions: HashMap::new(),
            msg1_guard: EpochHandshakeReplayGuard::new(host_epoch, MSG1_REPLAY_GUARD_CAPACITY),
        }
    }

    pub fn host_device_id(&self) -> &str {
        &self.host_device_id
    }

    pub fn host_epoch(&self) -> u64 {
        self.host_epoch
    }

    pub fn stream_id(&self) -> String {
        format!("{}:{}", self.host_device_id, self.host_epoch)
    }

    /// Bumps `host_epoch`, drops all prior Noise sessions, and resets the
    /// per-epoch msg1 replay guard so every device performs a fresh Noise `IK`
    /// handshake (Opus R1).
    pub fn advance_epoch(&mut self) -> u64 {
        self.host_epoch += 1;
        self.sessions.clear();
        self.msg1_guard =
            EpochHandshakeReplayGuard::new(self.host_epoch, MSG1_REPLAY_GUARD_CAPACITY);
        self.host_epoch
    }

    /// Completes a Noise `IK` responder handshake via `gitpulse-protocol`,
    /// enforcing mandatory `RevocationPolicy::Enforced`, `DeviceCertificate`
    /// static-key pinning, empty msg1 payload, and per-epoch msg1 replay guard.
    pub fn accept_mobile(
        &mut self,
        host_noise_static_x25519: &[u8; 32],
        cert: &DeviceCertificate,
        revocations: &RevocationCache,
        msg1: &[u8],
        now_secs: i64,
    ) -> Result<Vec<u8>, ProtocolError> {
        let policy = revocations.enforced_policy(now_secs)?;
        let stream_id = self.stream_id();
        let mut hs = NoiseHandshake::new_responder(
            host_noise_static_x25519,
            cert,
            revocations.identity_pub_ed25519(),
            policy,
            now_secs,
            &stream_id,
            self.host_epoch,
        )?;
        let _ = hs.read_message_guarded(msg1, &mut self.msg1_guard)?;
        let msg2 = hs.write_message(&[])?;
        let session = hs.into_transport()?;
        self.sessions.insert(cert.device_id.clone(), session);
        Ok(msg2)
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

    /// Encrypts a [`ControlPlaneEvent`] into `GPTF` v1 [`OuterFrame`]s for
    /// every active paired mobile session using `TunnelSession::encrypt`.
    pub fn broadcast_event(
        &mut self,
        class: FrameClass,
        ttl_sec: u32,
        event: &ControlPlaneEvent,
    ) -> Result<Vec<OuterFrame>, ProtocolError> {
        let plaintext =
            serde_json::to_vec(event).map_err(|e| ProtocolError::Json(e.to_string()))?;
        let stream_id = self.stream_id();
        let mut frames = Vec::with_capacity(self.sessions.len());
        for (peer_id, session) in self.sessions.iter_mut() {
            let wire_ct = session.encrypt(&plaintext)?;
            let max_wire = match class {
                FrameClass::Pty => MAX_PTY_FRAME_BYTES,
                FrameClass::Control | FrameClass::Approval => MAX_CONTROL_FRAME_BYTES,
            };
            if wire_ct.len() > max_wire {
                return Err(ProtocolError::TooLarge(format!(
                    "encrypted frame {} bytes exceeds {max_wire}",
                    wire_ct.len()
                )));
            }
            let seq = session.send_seq();
            frames.push(OuterFrame {
                v: GPTF_VERSION,
                frame_id: format!("{stream_id}:{seq}"),
                stream_id: stream_id.clone(),
                seq,
                from_device: self.host_device_id.clone(),
                to_devices: vec![peer_id.clone()],
                ttl_sec,
                class,
                ciphertext_b64: base64::engine::general_purpose::STANDARD.encode(&wire_ct),
            });
        }
        Ok(frames)
    }

    /// Decrypts an inbound `GPTF` v1 [`OuterFrame`] from `frame.from_device`
    /// via `TunnelSession::decrypt` (1024-bit sliding replay window + Poly1305 AEAD).
    pub fn open_from_mobile(
        &mut self,
        frame: &OuterFrame,
        revocations: &RevocationCache,
    ) -> Result<ControlPlaneEvent, ProtocolError> {
        if frame.v != GPTF_VERSION {
            return Err(ProtocolError::Malformed(format!(
                "unsupported GPTF version {}",
                frame.v
            )));
        }
        if revocations.is_revoked(&frame.from_device) {
            self.sessions.remove(&frame.from_device);
            return Err(ProtocolError::Revoked(frame.from_device.clone()));
        }
        if let Some((_, epoch_str)) = frame.stream_id.rsplit_once(':') {
            if let Ok(epoch) = epoch_str.parse::<u64>() {
                if epoch != self.host_epoch {
                    return Err(ProtocolError::BindingMismatch(format!(
                        "host_epoch mismatch: expected {}, got {epoch}",
                        self.host_epoch
                    )));
                }
            }
        }
        let session = self.sessions.get_mut(&frame.from_device).ok_or_else(|| {
            ProtocolError::Noise(format!("no Noise session for '{}'", frame.from_device))
        })?;
        let wire_ct = base64::engine::general_purpose::STANDARD
            .decode(frame.ciphertext_b64.trim())
            .map_err(|e| ProtocolError::Malformed(format!("ciphertext_b64: {e}")))?;
        let max_wire = match frame.class {
            FrameClass::Pty => MAX_PTY_FRAME_BYTES,
            FrameClass::Control | FrameClass::Approval => MAX_CONTROL_FRAME_BYTES,
        };
        if wire_ct.len() > max_wire {
            return Err(ProtocolError::TooLarge(format!(
                "frame size {} exceeds {max_wire}",
                wire_ct.len()
            )));
        }
        if wire_ct.len() < 8 {
            return Err(ProtocolError::Malformed("truncated wire seq prefix".into()));
        }
        let wire_seq = u64::from_be_bytes(
            wire_ct[..8]
                .try_into()
                .map_err(|_| ProtocolError::Malformed("wire seq slice".into()))?,
        );
        if wire_seq != frame.seq {
            return Err(ProtocolError::BindingMismatch(format!(
                "outer seq {} != wire seq {wire_seq}",
                frame.seq
            )));
        }
        let plain = session.decrypt(&wire_ct)?;
        serde_json::from_slice(&plain).map_err(|e| ProtocolError::Json(e.to_string()))
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
    fn open_approvals(&self) -> Vec<ApprovalTicket>;
    fn sessions(&self) -> Vec<SessionSnapshot> {
        Vec::new()
    }
}

/// Thread-safe snapshot provider backed by an in-memory ticket list.
#[derive(Debug, Default)]
pub struct StaticInboxSnapshotProvider {
    tickets: Mutex<Vec<ApprovalTicket>>,
}

impl StaticInboxSnapshotProvider {
    pub fn new(tickets: Vec<ApprovalTicket>) -> Self {
        Self {
            tickets: Mutex::new(tickets),
        }
    }

    pub fn set_tickets(&self, next: Vec<ApprovalTicket>) {
        if let Ok(mut guard) = self.tickets.lock() {
            *guard = next;
        }
    }
}

impl InboxSnapshotProvider for StaticInboxSnapshotProvider {
    fn open_approvals(&self) -> Vec<ApprovalTicket> {
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
    /// Keychain is locked or trust anchor unavailable — never hangs.
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
/// 2. Signs WS upgrade headers via `gitpulse_protocol::crypto::sign_relay_request`.
/// 3. Connects outbound-only via `tokio-tungstenite`.
/// 4. Writes `Active` to `remote-approval.state` and emits `inbox_snapshot` to
///    all active paired Noise sessions (Opus R6).
/// 5. Processes inbound frames, `cursor_gap` (re-emitting `inbox_snapshot`),
///    and WS close `4003` (immediately flipping `remote-approval.state` to
///    `ReleasedToNative`).
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
                    reason: format!("trust_anchor_unavailable: {reason}"),
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

    let headers = match sign_relay_request(
        &params.host_device_id,
        &seed,
        "GET",
        &request_uri,
        now_unix_ms(),
        &[],
    ) {
        Ok(h) => h,
        Err(e) => return TunnelSessionEnd::TransportError(format!("sign ws upgrade: {e}")),
    };

    let mut req = match full_url.as_str().into_client_request() {
        Ok(r) => r,
        Err(e) => return TunnelSessionEnd::TransportError(format!("build ws request: {e}")),
    };
    let hmap = req.headers_mut();
    for (name, value) in headers.pairs() {
        if let (Ok(hname), Ok(hval)) = (
            http::header::HeaderName::from_bytes(name.as_bytes()),
            value.parse(),
        ) {
            hmap.insert(hname, hval);
        }
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
    let mut snapshot_seq: u64 = 0;

    // Opus R6: Host is source of truth — emit full `inbox_snapshot` on every
    // connect/reconnect re-announcing all open approval tickets.
    snapshot_seq += 1;
    if let Err(err) = emit_inbox_snapshot(
        &mut ws_sink,
        &params.host_device_id,
        snapshot_seq,
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
                                RelayWireMessage::CursorGap(event)
                                    if matches!(*event, ControlPlaneEvent::CursorGap { .. }) =>
                                {
                                    snapshot_seq += 1;
                                    if let Err(err) = emit_inbox_snapshot(
                                        &mut ws_sink,
                                        &params.host_device_id,
                                        snapshot_seq,
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
                                            snapshot_seq += 1;
                                            let _ = emit_inbox_snapshot(
                                                &mut ws_sink,
                                                &params.host_device_id,
                                                snapshot_seq,
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
                                RelayWireMessage::CursorGap(_) | RelayWireMessage::WakePush(_) => {}
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
    seq: u64,
    noise_mgr: &Arc<Mutex<HostNoiseManager>>,
    snapshot_provider: &dyn InboxSnapshotProvider,
) -> Result<(), String>
where
    S: futures_util::Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
{
    let open_approvals = snapshot_provider.open_approvals();
    let sessions = snapshot_provider.sessions();
    let frames = {
        let mut mgr = noise_mgr
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let snapshot = ControlPlaneEvent::InboxSnapshot {
            host_id: host_device_id.to_string(),
            host_epoch: mgr.host_epoch(),
            seq,
            sessions,
            open_approvals,
            generated_at_ms: now_unix_ms(),
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
