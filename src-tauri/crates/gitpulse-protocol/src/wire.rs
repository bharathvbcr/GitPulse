//! Canonical wire types for GitPulse's unified control plane (`GPTF` v1).
//!
//! All field names and enum tags are frozen across:
//! - `src-tauri/crates/gitpulse-protocol` (Rust desktop, daemon, iOS/Android UniFFI)
//! - `gitpulse-relay` (Go outer frame & wake push routing)
//! - `packages/gitpulse-core` & `apps/mobile` (TypeScript)
//!
//! Security invariants enforced at the type level (Opus review):
//! - `ApprovalDecisionKind` is strictly `allow_once | deny | deny_and_interrupt`
//!   — NEVER `updatedInput` or `updatedPermissions` (P0-4).
//! - `SignedApprovalDecision` uses `#[serde(deny_unknown_fields)]` so smuggling
//!   extra keys fails closed at deserialization.
//! - `DeviceCertificate` enforces `<= 30 day` TTL and binds optional hardware
//!   P-256 `approval_pub_p256` alongside Ed25519 + X25519 keys (M2, R3).
//! - `SignedRevocationList` enforces a monotonic `version` signed by Account
//!   `IdentityKey` (R3).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Current `GPTF` outer tunnel frame protocol version.
pub const GPTF_VERSION: u8 = 1;

/// Maximum `DeviceCertificate` lifetime: 30 days in seconds (Opus R3/M2).
pub const MAX_CERT_TTL_SECS: i64 = 30 * 24 * 60 * 60;

/// Maximum clock skew tolerated when validating `created_at` / `issued_at` timestamps.
pub const CLOCK_SKEW_SECS: i64 = 300;

/// Maximum `GPTF` outer frame ciphertext payload for `control` and `approval` frames (64 KiB).
pub const MAX_CONTROL_FRAME_BYTES: usize = 64 * 1024;

/// Maximum `GPTF` outer frame ciphertext payload for `pty` frames (16 KiB).
pub const MAX_PTY_FRAME_BYTES: usize = 16 * 1024;

/// Maximum total size of an encrypted `preview_ct` blob for APNs/FCM NSE wake pushes (2 KiB, Opus R8).
pub const MAX_PREVIEW_CT_BYTES: usize = 2048;

/// Traffic class carried by an opaque `GPTF` v1 [`OuterFrame`].
///
/// Visible to `gitpulse-relay` strictly for ring-buffer retention and rate-limiting:
/// `pty` frames are never buffered; `control` and `approval` frames may be buffered
/// in memory with bounded TTL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameClass {
    /// Control-plane state snapshots, deltas, steering, and revocations.
    Control,
    /// Approval ticket announcements, signed decisions, and resolution outcomes.
    Approval,
    /// Lossy terminal screen/input frames (never buffered in relay ring).
    Pty,
}

/// Zero-knowledge `GPTF` v1 outer routing envelope visible to `gitpulse-relay`.
///
/// Carries zero repository names, paths, commands, or diffs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OuterFrame {
    /// Protocol version (`1`).
    pub v: u8,
    /// Unique frame identifier (UUID / ULID).
    pub frame_id: String,
    /// Stream identifier formatted as `"<host_device_id>:<host_epoch>"`.
    pub stream_id: String,
    /// Per-epoch monotonic sequence number assigned by the sender.
    pub seq: u64,
    /// Sender device identifier.
    pub from_device: String,
    /// Recipient device identifiers (empty means broadcast to account peer devices).
    #[serde(default)]
    pub to_devices: Vec<String>,
    /// Relay memory retention TTL in seconds (`0` for non-buffered frames such as `pty`).
    pub ttl_sec: u32,
    /// Frame class (`control`, `approval`, or `pty`).
    pub class: FrameClass,
    /// Standard base64-encoded Noise `IK` handshake or transport ciphertext.
    pub ciphertext_b64: String,
}

/// Metadata-free APNs/FCM wake push descriptor emitted by host to relay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakePushFrame {
    /// Protocol version (`1`).
    pub v: u8,
    /// Generic wake kind (`"approval"` or `"attention"`).
    pub kind: WakePushKind,
    /// Host device identifier that originated the wake.
    pub host_device: String,
    /// Stream sequence number on the host's current epoch.
    pub stream_seq: u64,
    /// Unix epoch seconds after which the wake is stale.
    pub expires_at: i64,
    /// Optional `<= 2 KiB` sealed preview blob (`seal_preview`), standard base64.
    /// Never contains full command bodies (Opus R8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_ct_b64: Option<String>,
}

/// Generic wake notification kind visible to APNs/FCM (zero metadata leakage).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakePushKind {
    /// A pending tool approval ticket requires attention.
    Approval,
    /// General agent attention / steering notification.
    Attention,
}

/// Per-session runtime approval channel (Opus §4: never a static per-harness table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalChannel {
    /// Universal presence-gated `PermissionRequest` bridge (Claude Code).
    PresenceGatedHook,
    /// Codex Tier 1 `codex app-server` item-bound approvals (no hook timeout).
    CodexAppServer,
    /// Codex Tier 2 TUI `PermissionRequest` hook (user-trusted natively).
    CodexTuiHook,
    /// Manvi policy engine authority (single-use hash-bound grants).
    ManviPolicy,
    /// Native terminal/IDE only — remote approval disabled (e.g. Antigravity `<= 2.32.1`).
    NativeOnly,
    /// Hosted CLI PTY where human types after explicit step-up consent.
    PtyHuman,
    /// Observe-only session with no remote approval or steering path.
    ObserveOnly,
}

/// Runtime capability matrix advertised per session (Opus §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySet {
    /// Harness identifier (`"claude"`, `"agy"`, `"codex"`, `"manvi"`, etc.).
    pub harness: String,
    /// Active runtime approval channel for this session.
    pub approval_channel: ApprovalChannel,
    /// True when mobile can submit cryptographically signed `ApprovalDecisionKind`s.
    pub supports_remote_approval: bool,
    /// True when active steering messages can be delivered mid-session.
    pub supports_active_steering: bool,
    /// True when an idle session can be woken remotely without manual user action.
    pub supports_idle_wake: bool,
    /// True when tool calls are NOT gated by remote/Manvi approval (MUST render `"ungated"` badge on mobile, e.g. Antigravity).
    pub ungated: bool,
    /// Steering queue TTL in seconds when steering is queued for next turn (e.g. `7200` for idle Antigravity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_steering_ttl_sec: Option<u32>,
    /// True ONLY when Desktop has granted time-boxed `pty.input` RCE consent (Opus P0-7; default false).
    #[serde(default)]
    pub pty_input_enabled: bool,
}

/// Active sub-agent or swarm worker summary promoted from `alerts::bridge` into session snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentSummary {
    /// Unique sub-agent identifier (`agent_id` from hook/bridge).
    pub agent_id: String,
    /// Parent session key or provider session identifier.
    pub parent_session_id: String,
    /// Sub-agent kind/role (`"claude"`, `"agy"`, `"worker"`, etc.).
    pub kind: String,
    /// Current observed status (`"running"`, `"waiting"`, `"idle"`, `"needs_you"`, `"error"`, `"completed"`).
    pub status: String,
    /// Human-readable short label or task title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Unix epoch milliseconds when the sub-agent was first observed.
    pub started_at_ms: i64,
    /// Unix epoch milliseconds of the last observed event.
    pub updated_at_ms: i64,
}

/// Wire summary of an agent session in `inbox_snapshot` / `session_delta`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// Stable GitPulse session key.
    pub session_key: String,
    /// Absolute repository root path on the host.
    pub repo_path: String,
    /// Disambiguated display label for the repository.
    pub repo_label: String,
    /// Active worktree path for this session.
    pub worktree_path: String,
    /// Current branch name, or `None` when detached/unread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Harness identifier (`"claude"`, `"agy"`, `"codex"`, `"manvi"`, ...).
    pub harness: String,
    /// Session title or checkout slug.
    pub title: String,
    /// Presence state (`"live"`, `"exited"`, `"on-disk"`, `"missing"`).
    pub presence: String,
    /// True when the agent reported full turn-idle (`NO_TOOL_CALL` / idle).
    #[serde(default)]
    pub fully_idle: bool,
    /// Provider-native session UUID when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    /// Primary standing attention reason (`"needs-you"`, `"error"`, `"idle-waiting"`, etc.), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<String>,
    /// Short human-readable attention detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_detail: Option<String>,
    /// Runtime per-session capabilities (never static per harness).
    pub capabilities: CapabilitySet,
    /// Active sub-agents / swarm workers nested under this session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_subagents: Vec<SubagentSummary>,
    /// Currently open approval ticket IDs belonging to this session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_approval_ids: Vec<String>,
    /// Dirty file count if measured (`None` = unmeasured, never treated as 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty_files: Option<u32>,
    /// Unix epoch milliseconds of the snapshot/update.
    pub updated_at_ms: i64,
}

/// Pending tool approval ticket emitted by `ApprovalBroker`.
///
/// Keyed and deduplicated by `(session_id, input_sha256)` — Claude's
/// `PermissionRequest` payload omits `tool_use_id` (Opus P0-2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalTicket {
    /// Unique ticket identifier minted by the host `ApprovalBroker`.
    pub ticket_id: String,
    /// Host device identifier that owns the pending hook/approval.
    pub host_id: String,
    /// Session identifier (GitPulse session key or provider session id).
    pub session_id: String,
    /// Canonical deduplication key formatted as `"<session_id>:<input_sha256>"` (Opus P0-2).
    pub dedup_key: String,
    /// Harness identifier (`"claude"`, `"codex"`, `"manvi"`).
    pub harness: String,
    /// Approval channel that originated this ticket.
    pub channel: ApprovalChannel,
    /// Tool name requested by the agent (e.g. `"Bash"`, `"Edit"`).
    pub tool_name: String,
    /// Full unredacted tool input JSON rendered natively before signing (Opus P0-5/M1).
    pub tool_input: Value,
    /// Working directory in which the tool executes.
    pub cwd: String,
    /// Lowercase hex SHA-256 of canonical `{"cwd","tool_input","tool_name"}`.
    pub input_sha256: String,
    /// Short human-readable summary for list views (NOT what is signed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Repository display label when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_label: Option<String>,
    /// Unix epoch seconds when the ticket was created.
    pub issued_at: i64,
    /// Unix epoch seconds when the ticket expires.
    pub expires_at: i64,
}

/// Minimal approval verdict vocabulary permitted from mobile (Opus P0-4).
///
/// STRICTLY `allow_once | deny | deny_and_interrupt` — NEVER `updatedInput` or
/// `updatedPermissions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecisionKind {
    /// Allow this exact canonical tool invocation once.
    AllowOnce,
    /// Deny this tool invocation and let the agent continue.
    Deny,
    /// Deny this tool invocation and interrupt the current turn.
    DenyAndInterrupt,
}

impl ApprovalDecisionKind {
    /// Canonical lowercase snake_case wire representation used in signing bytes.
    pub const fn as_wire_str(self) -> &'static str {
        match self {
            Self::AllowOnce => "allow_once",
            Self::Deny => "deny",
            Self::DenyAndInterrupt => "deny_and_interrupt",
        }
    }
}

/// Cryptographically signed mobile approval verdict (Opus P0-4, P0-5, R2).
///
/// Uses `#[serde(deny_unknown_fields)]` so any attempt to inject `updatedInput`,
/// `updatedPermissions`, or unrecognized keys is rejected at deserialization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedApprovalDecision {
    /// Target host device identifier.
    pub host_id: String,
    /// Target approval ticket identifier.
    pub ticket_id: String,
    /// Lowercase hex SHA-256 of the rendered canonical tool invocation bytes.
    pub input_sha256: String,
    /// Strictly one of `allow_once`, `deny`, or `deny_and_interrupt`.
    pub decision: ApprovalDecisionKind,
    /// Fresh per-decision nonce to prevent replay across retries.
    pub nonce: String,
    /// Unix epoch seconds when the decision was signed.
    pub issued_at: i64,
    /// Unix epoch seconds after which the decision is no longer valid.
    pub expires_at: i64,
    /// Mobile signer device identifier (must match verified `DeviceCertificate`).
    pub device_id: String,
    /// Standard base64-encoded ASN.1 DER ECDSA-P256-SHA256 signature from hardware key.
    pub sig_p256_der_b64: String,
}

/// Terminal disposition of an [`ApprovalTicket`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOutcome {
    /// Verdict was applied to the waiting hook/harness.
    Applied,
    /// Late mobile decision arrived after user returned, answered locally, or interrupted (Opus P0-1).
    NotApplied,
    /// Ticket expired before any decision arrived.
    Expired,
    /// Ticket cancelled (e.g., Esc/Ctrl-C closed hook socket or user returned to desk).
    Cancelled,
    /// Decision failed cryptographic, policy, or binding verification.
    Rejected,
    /// Signer device certificate was revoked.
    Revoked,
}

/// Event emitted when an approval ticket resolves or a late decision is rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalResolvedEvent {
    /// Ticket identifier.
    pub ticket_id: String,
    /// Host device identifier.
    pub host_id: String,
    /// Outcome disposition.
    pub outcome: ApprovalOutcome,
    /// Decision that was applied (when `outcome == Applied`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_decision: Option<ApprovalDecisionKind>,
    /// Device or `"desktop"` that resolved the ticket, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<String>,
    /// Human-readable reason (e.g., `"user_returned_to_desk"`, `"hook_eof"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Unix epoch seconds when resolved.
    pub resolved_at: i64,
}

/// Expected steering delivery route computed by step 1 (`steering_preview`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SteeringDeliveryMode {
    /// Delivered immediately into an active turn or `codex app-server` `turn/start`.
    Immediate,
    /// Queued in `agent_mailbox` for the next turn (honest `"Queued (next turn)"` for idle Antigravity).
    QueuedNextTurn,
    /// Delivered via idle-wake hook (`asyncRewake`) or headless resume after liveness check.
    WakeResume,
    /// Session cannot accept remote steering.
    Unsupported,
}

/// Step 1 of two-step confirm steering: ask the host how a steering message would route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteeringPreviewReq {
    /// Client-generated correlation identifier.
    pub request_id: String,
    /// Target session key.
    pub session_key: String,
    /// Proposed steering message text.
    pub text: String,
}

/// Host response to [`SteeringPreviewReq`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteeringPreviewResp {
    /// Correlation identifier echoing [`SteeringPreviewReq::request_id`].
    pub request_id: String,
    /// Target session key.
    pub session_key: String,
    /// Concrete route string that [`SteeringConfirmReq::expect_route`] must echo.
    pub expect_route: String,
    /// Honest delivery mode classification.
    pub delivery_mode: SteeringDeliveryMode,
    /// Queue TTL in seconds when `delivery_mode == QueuedNextTurn` (e.g., `7200`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_ttl_sec: Option<u32>,
    /// Optional opaque preview confirmation token bound to `(session_key, expect_route, text)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_token: Option<String>,
    /// Optional human-readable warning (e.g. `"Session is idle; message will wait until next turn."`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// Cryptographically signed mobile steering confirmation (Opus review #23).
///
/// Prevents a compromised JS/OTA layer from injecting steering text that the host
/// deposits as `HumanConfirmed` (which equals arbitrary execution in bypass/acceptEdits
/// modes). Mobile renders the exact UTF-8 steering `text` natively, computes
/// `text_sha256 = hex(sha256(text))`, prompts biometrics, and signs
/// `b"gitpulse-steering/v1\n" || canonical_json({expect_route, expires_at, host_id,
/// issued_at, nonce, request_id, session_key, text_sha256})` using the hardware P-256
/// approval key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedSteeringConfirm {
    /// Target host device identifier.
    pub host_id: String,
    /// Target session key.
    pub session_key: String,
    /// Idempotent request identifier matching [`SteeringConfirmReq::request_id`].
    pub request_id: String,
    /// Expected route matching [`SteeringConfirmReq::expect_route`].
    pub expect_route: String,
    /// Lowercase 64-char hex SHA-256 of the exact UTF-8 steering `text`.
    pub text_sha256: String,
    /// Unix epoch seconds when the confirmation was signed.
    pub issued_at: i64,
    /// Unix epoch seconds after which the confirmation expires.
    pub expires_at: i64,
    /// Fresh per-confirmation nonce to prevent replay.
    pub nonce: String,
    /// Mobile signer device identifier (must match verified [`DeviceCertificate`]).
    pub device_id: String,
    /// Standard base64-encoded ASN.1 DER ECDSA-P256-SHA256 signature from hardware key.
    pub sig_p256_der_b64: String,
}

/// Step 2 of two-step confirm steering: commit the previewed steering delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteeringConfirmReq {
    /// Idempotent client request identifier.
    pub request_id: String,
    /// Target session key.
    pub session_key: String,
    /// Expected route returned by [`SteeringPreviewResp::expect_route`].
    pub expect_route: String,
    /// Steering message text.
    pub text: String,
    /// Optional confirmation token returned by [`SteeringPreviewResp::confirm_token`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_token: Option<String>,
    /// Hardware P-256 signed steering confirmation required before the host may
    /// deposit `text` with `HumanConfirmed` provenance (Opus review #23).
    /// Unverifiable or absent signatures MUST be refused or downgraded to untrusted framing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed: Option<SignedSteeringConfirm>,
}

/// Delivery status returned by [`SteeringConfirmResp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SteeringStatus {
    /// Delivered immediately to the target session.
    Delivered,
    /// Queued for the next turn (honest `"Queued"` state with TTL).
    Queued,
    /// Queued steering message expired before delivery.
    Expired,
    /// Delivery refused (route changed, session gone, or policy denied).
    Rejected,
}

/// Host response to [`SteeringConfirmReq`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteeringConfirmResp {
    /// Echoed idempotent request identifier.
    pub request_id: String,
    /// Target session key.
    pub session_key: String,
    /// Resulting delivery status.
    pub status: SteeringStatus,
    /// Actual route taken, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_used: Option<String>,
    /// Unix epoch seconds when a `queued` message will expire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// Error or rejection reason when `status == Rejected`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Device category enrolled in a [`DeviceCertificate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    /// Desktop / headless host running `gitpulsed --relay`.
    Desktop,
    /// Mobile companion app (iOS / Android) carrying hardware P-256 approval key.
    Mobile,
}

/// Short-lived (`<= 30 day`) Account IdentityKey-signed device certificate (Opus M2, R3).
///
/// Binds `device_id`, `ed25519_pub` (relay HTTP/WS auth + preview signing),
/// `x25519_pub` (Noise `IK` static key + NSE `preview_ct` decryption), and
/// optional hardware `approval_pub_p256` (Secure Enclave / StrongBox P-256 key
/// used exclusively for `SignedApprovalDecision`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCertificate {
    /// Certificate schema version (`2` supports optional P-256 approval key).
    pub version: u8,
    /// Stable device identifier.
    pub device_id: String,
    /// Device kind (`desktop` or `mobile`).
    pub kind: DeviceKind,
    /// Human-readable device name (`<= 128` bytes, no control chars).
    pub name: String,
    /// Base64-encoded 32-byte Ed25519 public key.
    pub ed25519_pub_b64: String,
    /// Base64-encoded 32-byte X25519 static public key (Noise `IK` pinned key).
    pub x25519_pub_b64: String,
    /// Optional base64-encoded SEC1 (33B compressed or 65B uncompressed) NIST P-256
    /// public key for hardware-backed approval signing (REQUIRED for mobile approvers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_pub_p256_b64: Option<String>,
    /// Unix epoch seconds when issued.
    pub created_at: i64,
    /// Unix epoch seconds when expired (`expires_at - created_at <= 30 days`).
    pub expires_at: i64,
    /// Base64-encoded 64-byte Ed25519 signature by the Account `IdentityKey`.
    pub identity_sig_b64: String,
}

/// Monotonic Account IdentityKey-signed device revocation list (Opus R3).
///
/// Prevents a compromised relay from hiding device revocations. Hosts cache the
/// latest verified list and bypass remote approval holds if it becomes stale past
/// `expires_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRevocationList {
    /// Strictly monotonic revocation list version (rollback rejected).
    pub version: u64,
    /// Unix epoch seconds when issued.
    pub issued_at: i64,
    /// Unix epoch seconds after which hosts MUST refresh or fail closed (bypass remote holds).
    pub expires_at: i64,
    /// Sorted unique list of revoked `device_id` values.
    #[serde(default)]
    pub revoked_device_ids: Vec<String>,
    /// Base64-encoded 64-byte Ed25519 signature by the Account `IdentityKey`.
    pub identity_sig_b64: String,
}

/// Inner E2EE control-plane event / RPC envelope carried inside Noise `IK` ciphertext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlPlaneEvent {
    /// Full state snapshot re-announcing every session and open approval ticket (Opus R6).
    InboxSnapshot {
        /// Host device identifier.
        host_id: String,
        /// Monotonic host epoch.
        host_epoch: u64,
        /// Sequence number of this snapshot on `host_epoch`.
        seq: u64,
        /// All visible sessions on this host.
        sessions: Vec<SessionSnapshot>,
        /// All currently open approval tickets (source of truth on reconnect).
        open_approvals: Vec<ApprovalTicket>,
        /// Unix epoch milliseconds when generated.
        generated_at_ms: i64,
    },
    /// Incremental session update.
    SessionDelta {
        /// Host device identifier.
        host_id: String,
        /// Updated session snapshot.
        session: SessionSnapshot,
    },
    /// Newly created or re-announced pending approval ticket.
    ApprovalCreated {
        /// Ticket payload.
        ticket: ApprovalTicket,
    },
    /// Signed mobile approval decision sent to host.
    ApprovalDecide {
        /// P-256 signed decision payload.
        decision: SignedApprovalDecision,
    },
    /// Resolution notification (including `not_applied` for late mobile decisions).
    ApprovalResolved {
        /// Resolution details.
        resolved: ApprovalResolvedEvent,
    },
    /// Request a two-step steering preview.
    SteeringPreviewReq {
        /// Preview request.
        req: SteeringPreviewReq,
    },
    /// Response to a steering preview request.
    SteeringPreviewResp {
        /// Preview response.
        resp: SteeringPreviewResp,
    },
    /// Confirm a previously previewed steering message.
    SteeringConfirmReq {
        /// Confirm request.
        req: SteeringConfirmReq,
    },
    /// Response to a steering confirmation.
    SteeringConfirmResp {
        /// Confirm response.
        resp: SteeringConfirmResp,
    },
    /// Notification that a queued steering message expired undelivered (Opus §4).
    SteeringExpired {
        /// Original request identifier.
        request_id: String,
        /// Target session key.
        session_key: String,
        /// Unix epoch seconds when expired.
        expired_at: i64,
    },
    /// Relay/host signal that requested `(after_epoch, after_seq)` fell outside the ring buffer.
    CursorGap {
        /// Expected/current host epoch.
        current_epoch: u64,
        /// Oldest available sequence in the ring buffer.
        oldest_available_seq: u64,
        /// Latest sequence emitted on `current_epoch`.
        latest_seq: u64,
    },
    /// Request a fresh `inbox_snapshot` resync (sent by client after `cursor_gap` or reconnect).
    SnapshotResyncReq {
        /// Client-generated correlation identifier.
        request_id: String,
    },
    /// Read-only PTY output chunk (`FrameClass::Pty`, never buffered).
    PtyOutput {
        /// Target session key.
        session_key: String,
        /// Monotonic PTY output chunk sequence.
        chunk_seq: u64,
        /// Standard base64-encoded UTF-8/VT100 bytes.
        data_b64: String,
    },
    /// Opt-in PTY keyboard input (Opus P0-7: requires Desktop-granted time-boxed consent + step-up).
    ///
    /// Accepted residual risk (Opus review #23): individual `pty_input` keystrokes within
    /// an active Desktop-granted consent window are authenticated by the Noise `IK` tunnel
    /// rather than signed per keystroke by the hardware P-256 key.
    PtyInput {
        /// Target session key.
        session_key: String,
        /// Consent grant identifier issued by Desktop.
        consent_grant_id: String,
        /// Standard base64-encoded input bytes.
        data_b64: String,
    },
    /// Updated IdentityKey-signed revocation list pushed over the tunnel.
    RevocationListUpdate {
        /// Signed revocation list.
        list: SignedRevocationList,
    },
    /// Application-level keepalive ping.
    Ping {
        /// Monotonic ping nonce/timestamp in ms.
        ts_ms: i64,
    },
    /// Application-level keepalive pong.
    Pong {
        /// Echoed ping timestamp in ms.
        ts_ms: i64,
    },
}
