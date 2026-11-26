/**
 * Canonical wire types mirroring `src-tauri/crates/gitpulse-protocol/src/wire.rs` (`GPTF` v1).
 *
 * Pure TypeScript — zero Svelte, DOM, or Tauri imports.
 */

export const GPTF_VERSION = 1 as const;
export const MAX_CERT_TTL_SECS = 30 * 24 * 60 * 60;
export const MAX_CONTROL_FRAME_BYTES = 64 * 1024;
export const MAX_PTY_FRAME_BYTES = 16 * 1024;
export const MAX_PREVIEW_CT_BYTES = 2048;

export type FrameClass = "control" | "approval" | "pty";

export interface OuterFrame {
  v: 1;
  frame_id: string;
  stream_id: string;
  seq: number;
  from_device: string;
  to_devices: string[];
  ttl_sec: number;
  class: FrameClass;
  ciphertext_b64: string;
}

export type WakePushKind = "approval" | "attention";

export interface WakePushFrame {
  v: 1;
  kind: WakePushKind;
  host_device: string;
  stream_seq: number;
  expires_at: number;
  preview_ct_b64?: string;
}

export type ApprovalChannel =
  | "presence_gated_hook"
  | "codex_app_server"
  | "codex_tui_hook"
  | "manvi_policy"
  | "native_only"
  | "pty_human"
  | "observe_only";

export interface CapabilitySet {
  harness: string;
  approval_channel: ApprovalChannel;
  supports_remote_approval: boolean;
  supports_active_steering: boolean;
  supports_idle_wake: boolean;
  /** True when tool calls are not remotely gated (must show `"ungated"` badge on mobile). */
  ungated: boolean;
  queued_steering_ttl_sec?: number;
  /** Default false; true only after explicit Desktop-granted time-boxed RCE consent (Opus P0-7). */
  pty_input_enabled: boolean;
}

export type ControlCapability = CapabilitySet;

export interface SubagentSummary {
  agent_id: string;
  parent_session_id: string;
  kind: string;
  status: string;
  label?: string;
  started_at_ms: number;
  updated_at_ms: number;
}

export interface SessionSnapshot {
  session_key: string;
  repo_path: string;
  repo_label: string;
  worktree_path: string;
  branch?: string;
  harness: string;
  title: string;
  presence: string;
  fully_idle: boolean;
  provider_session_id?: string;
  attention?: string;
  attention_detail?: string;
  capabilities: CapabilitySet;
  active_subagents: SubagentSummary[];
  pending_approval_ids: string[];
  dirty_files?: number;
  updated_at_ms: number;
}

export interface ApprovalTicket {
  ticket_id: string;
  host_id: string;
  session_id: string;
  /** Canonical deduplication key `"<session_id>:<input_sha256>"` (Opus P0-2). */
  dedup_key: string;
  harness: string;
  channel: ApprovalChannel;
  tool_name: string;
  tool_input: unknown;
  cwd: string;
  input_sha256: string;
  summary?: string;
  repo_label?: string;
  issued_at: number;
  expires_at: number;
}

/**
 * STRICTLY `allow_once | deny | deny_and_interrupt` (Opus P0-4).
 * Never `updatedInput` or `updatedPermissions`.
 */
export const APPROVAL_DECISION_KINDS = [
  "allow_once",
  "deny",
  "deny_and_interrupt",
] as const;

export type ApprovalDecisionKind = (typeof APPROVAL_DECISION_KINDS)[number];

export interface SignedApprovalDecision {
  host_id: string;
  ticket_id: string;
  input_sha256: string;
  decision: ApprovalDecisionKind;
  nonce: string;
  issued_at: number;
  expires_at: number;
  device_id: string;
  sig_p256_der_b64: string;
}

const ALLOWED_DECISION_KEYS = new Set<string>([
  "host_id",
  "ticket_id",
  "input_sha256",
  "decision",
  "nonce",
  "issued_at",
  "expires_at",
  "device_id",
  "sig_p256_der_b64",
]);

export function isValidApprovalDecisionKind(
  value: unknown,
): value is ApprovalDecisionKind {
  return (
    value === "allow_once" ||
    value === "deny" ||
    value === "deny_and_interrupt"
  );
}

/**
 * Defense-in-depth boundary check rejecting `updatedInput`, `updatedPermissions`,
 * or any unknown key on a `SignedApprovalDecision` before transmission (Opus P0-4).
 */
export function assertCleanSignedApprovalDecision(
  candidate: unknown,
): asserts candidate is SignedApprovalDecision {
  if (!candidate || typeof candidate !== "object" || Array.isArray(candidate)) {
    throw new Error("SignedApprovalDecision must be a plain object");
  }
  const obj = candidate as Record<string, unknown>;
  for (const key of Object.keys(obj)) {
    if (!ALLOWED_DECISION_KEYS.has(key)) {
      throw new Error(`Forbidden field on SignedApprovalDecision: ${key}`);
    }
  }
  if (!isValidApprovalDecisionKind(obj.decision)) {
    throw new Error(`Invalid ApprovalDecisionKind: ${String(obj.decision)}`);
  }
}

export type ApprovalOutcome =
  | "applied"
  | "not_applied"
  | "expired"
  | "cancelled"
  | "rejected"
  | "revoked";

export interface ApprovalResolvedEvent {
  ticket_id: string;
  host_id: string;
  outcome: ApprovalOutcome;
  applied_decision?: ApprovalDecisionKind;
  resolved_by?: string;
  reason?: string;
  resolved_at: number;
}

export type SteeringDeliveryMode =
  | "immediate"
  | "queued_next_turn"
  | "wake_resume"
  | "unsupported";

export interface SteeringPreviewReq {
  request_id: string;
  session_key: string;
  text: string;
}

export interface SteeringPreviewResp {
  request_id: string;
  session_key: string;
  expect_route: string;
  delivery_mode: SteeringDeliveryMode;
  queued_ttl_sec?: number;
  confirm_token?: string;
  warning?: string;
}

export interface SignedSteeringConfirm {
  host_id: string;
  session_key: string;
  request_id: string;
  expect_route: string;
  text_sha256: string;
  issued_at: number;
  expires_at: number;
  nonce: string;
  device_id: string;
  sig_p256_der_b64: string;
}

const ALLOWED_STEERING_SIGN_KEYS = new Set<string>([
  "host_id",
  "session_key",
  "request_id",
  "expect_route",
  "text_sha256",
  "issued_at",
  "expires_at",
  "nonce",
  "device_id",
  "sig_p256_der_b64",
]);

/**
 * Defense-in-depth boundary check rejecting unexpected keys on `SignedSteeringConfirm`
 * before transmission (Opus review #23).
 */
export function assertCleanSignedSteeringConfirm(
  candidate: unknown,
): asserts candidate is SignedSteeringConfirm {
  if (!candidate || typeof candidate !== "object" || Array.isArray(candidate)) {
    throw new Error("SignedSteeringConfirm must be a plain object");
  }
  const obj = candidate as Record<string, unknown>;
  for (const key of Object.keys(obj)) {
    if (!ALLOWED_STEERING_SIGN_KEYS.has(key)) {
      throw new Error(`Forbidden field on SignedSteeringConfirm: ${key}`);
    }
  }
  if (
    typeof obj.text_sha256 !== "string" ||
    !/^[0-9a-f]{64}$/.test(obj.text_sha256)
  ) {
    throw new Error("SignedSteeringConfirm.text_sha256 must be 64 lowercase hex chars");
  }
}

export interface SteeringConfirmReq {
  request_id: string;
  session_key: string;
  expect_route: string;
  text: string;
  confirm_token?: string;
  /** Hardware P-256 signed steering confirmation required for `HumanConfirmed` provenance. */
  signed?: SignedSteeringConfirm;
}

export type SteeringStatus = "delivered" | "queued" | "expired" | "rejected";

export interface SteeringConfirmResp {
  request_id: string;
  session_key: string;
  status: SteeringStatus;
  route_used?: string;
  expires_at?: number;
  reason?: string;
}

export type DeviceKind = "desktop" | "mobile";

export interface DeviceCertificate {
  version: number;
  device_id: string;
  kind: DeviceKind;
  name: string;
  ed25519_pub_b64: string;
  x25519_pub_b64: string;
  approval_pub_p256_b64?: string;
  created_at: number;
  expires_at: number;
  identity_sig_b64: string;
}

export interface SignedRevocationList {
  version: number;
  issued_at: number;
  expires_at: number;
  revoked_device_ids: string[];
  identity_sig_b64: string;
}

export type ControlPlaneEvent =
  | {
      type: "inbox_snapshot";
      host_id: string;
      host_epoch: number;
      seq: number;
      sessions: SessionSnapshot[];
      open_approvals: ApprovalTicket[];
      generated_at_ms: number;
    }
  | {
      type: "session_delta";
      host_id: string;
      session: SessionSnapshot;
    }
  | {
      type: "approval_created";
      ticket: ApprovalTicket;
    }
  | {
      type: "approval_decide";
      decision: SignedApprovalDecision;
    }
  | {
      type: "approval_resolved";
      resolved: ApprovalResolvedEvent;
    }
  | {
      type: "steering_preview_req";
      req: SteeringPreviewReq;
    }
  | {
      type: "steering_preview_resp";
      resp: SteeringPreviewResp;
    }
  | {
      type: "steering_confirm_req";
      req: SteeringConfirmReq;
    }
  | {
      type: "steering_confirm_resp";
      resp: SteeringConfirmResp;
    }
  | {
      type: "steering_expired";
      request_id: string;
      session_key: string;
      expired_at: number;
    }
  | {
      type: "cursor_gap";
      current_epoch: number;
      oldest_available_seq: number;
      latest_seq: number;
    }
  | {
      type: "snapshot_resync_req";
      request_id: string;
    }
  | {
      type: "pty_output";
      session_key: string;
      chunk_seq: number;
      data_b64: string;
    }
  | {
      type: "pty_input";
      session_key: string;
      consent_grant_id: string;
      data_b64: string;
    }
  | {
      type: "revocation_list_update";
      list: SignedRevocationList;
    }
  | {
      type: "ping";
      ts_ms: number;
    }
  | {
      type: "pong";
      ts_ms: number;
    };
