import {
  assertValidSignedApprovalDecision,
  assertValidSignedSteeringConfirm,
  type ApprovalDecisionKind,
  type DevicePublicKeys,
  type SignedApprovalDecision,
  type SignedSteeringConfirm,
} from "@gitpulse/crypto";

export {
  assertValidSignedApprovalDecision,
  assertValidSignedSteeringConfirm,
  type ApprovalDecisionKind,
  type DevicePublicKeys,
  type SignedApprovalDecision,
  type SignedSteeringConfirm,
};

export type HarnessKind = "claude" | "antigravity" | "codex" | "manvi";

/** Matches `crates/gitpulse-protocol/src/wire.rs` `ApprovalChannel` (board #18). */
export type ApprovalChannel =
  | "presence_gated_hook"
  | "codex_app_server"
  | "codex_tui_hook"
  | "manvi_policy"
  | "native_only"
  | "pty_human"
  | "observe_only";

/** Matches `crates/gitpulse-protocol/src/wire.rs` `CapabilitySet` (board #18). */
export interface CapabilitySet {
  readonly harness: HarnessKind;
  readonly approval_channel: ApprovalChannel;
  readonly supports_remote_approval: boolean;
  readonly supports_active_steering: boolean;
  readonly supports_idle_wake: boolean;
  readonly ungated: boolean;
  readonly queued_steering_ttl_sec?: number;
  readonly pty_input_enabled: boolean;
}

export type CapabilityBadgeLabel =
  | "Universal Presence-Gated"
  | "Ungated / Native Only"
  | "Queued — Next Turn"
  | "Codex App-Server"
  | "Codex TUI Hook"
  | "Manvi Policy Authority"
  | "PTY Human Step-Up"
  | "Observe Only";

export interface CapabilityBadge {
  readonly label: CapabilityBadgeLabel;
  readonly tone: "success" | "warning" | "info" | "neutral";
  readonly detail: string;
}

export type ProbeGapKind = "failed" | "skipped" | "truncated" | "stale";

export interface ProbeGap {
  readonly id: string;
  readonly repo_path: string;
  readonly label: string;
  readonly kind: ProbeGapKind;
  readonly reason: string;
}

/** Matches `crates/gitpulse-protocol/src/wire.rs` `SubagentSummary` (board #18). */
export interface SubagentSummary {
  readonly agent_id: string;
  readonly parent_session_id: string;
  readonly kind: string;
  readonly status: "working" | "waiting" | "idle" | "error" | string;
  readonly label?: string;
  readonly started_at_ms: number;
  readonly updated_at_ms: number;
  readonly attention_observed?: boolean;
}

export interface BlastRadiusSummary {
  readonly target_symbol: string;
  readonly impacted_files: readonly string[];
  readonly direct_callers: number;
  readonly affected_tests: readonly string[];
  readonly unattributed_count: number;
  readonly walk_incomplete: boolean;
  readonly truncated: boolean;
}

export interface DiffHunkFile {
  readonly path: string;
  readonly additions: number;
  readonly deletions: number;
  readonly unified_diff: string;
}

/** Matches `crates/gitpulse-protocol/src/wire.rs` `SessionSnapshot` (board #18). */
export interface SessionSnapshot {
  readonly session_key: string;
  readonly host_id?: string;
  readonly repo_path: string;
  readonly repo_label: string;
  readonly worktree_path: string;
  readonly branch?: string;
  readonly harness: HarnessKind;
  readonly title: string;
  readonly presence: "running" | "idle" | "parked" | "waiting_approval" | string;
  readonly fully_idle: boolean;
  readonly provider_session_id?: string;
  readonly attention?: string;
  readonly attention_detail?: string;
  readonly capabilities: CapabilitySet;
  readonly active_subagents: readonly SubagentSummary[];
  readonly pending_approval_ids: readonly string[];
  readonly dirty_files?: number;
  readonly updated_at_ms: number;
  // Optional mobile detail enrichments hydrated on session detail view
  readonly blast_radius?: BlastRadiusSummary;
  readonly diff_files?: readonly DiffHunkFile[];
}

/** Matches `crates/gitpulse-protocol/src/wire.rs` `ApprovalTicket` (board #18). */
export interface ApprovalTicket {
  readonly ticket_id: string;
  readonly host_id: string;
  readonly session_id: string;
  readonly dedup_key: string;
  readonly harness: HarnessKind;
  readonly channel: ApprovalChannel;
  readonly tool_name: string;
  readonly tool_input: unknown;
  readonly cwd: string;
  readonly input_sha256: string;
  readonly summary?: string;
  readonly repo_label?: string;
  /** Unix seconds */
  readonly issued_at: number;
  /** Unix seconds */
  readonly expires_at: number;
}

/** Matches `crates/gitpulse-protocol/src/wire.rs` `ApprovalResolvedEvent` (board #18). */
export interface ApprovalResolvedEvent {
  readonly ticket_id: string;
  readonly host_id: string;
  readonly outcome:
    | "applied"
    | "not_applied"
    | "expired"
    | "cancelled"
    | "rejected"
    | "revoked";
  readonly applied_decision?: ApprovalDecisionKind;
  readonly resolved_by?: string;
  readonly reason?: string;
  readonly resolved_at: number;
}

export type SteeringDeliveryMode =
  | "immediate"
  | "queued_next_turn"
  | "wake_resume"
  | "unsupported";

/** Matches `crates/gitpulse-protocol/src/wire.rs` `SteeringPreviewReq/Resp` (board #18). */
export interface SteeringPreviewReq {
  readonly request_id: string;
  readonly session_key: string;
  readonly text: string;
}

export interface SteeringPreviewResp {
  readonly request_id: string;
  readonly session_key: string;
  readonly expect_route: string;
  readonly delivery_mode: SteeringDeliveryMode;
  readonly queued_ttl_sec?: number;
  readonly confirm_token?: string;
  readonly warning?: string;
}

/** Matches `crates/gitpulse-protocol/src/wire.rs` `SteeringConfirmReq/Resp` (board #18). */
/**
 * Matches `SteeringConfirmReq` (board #18 + Opus #23 P1 steering authenticity).
 * Carries a hardware P-256 `SignedSteeringConfirm` signed from the native steering sheet
 * so the host verifies human confirmation before depositing `HumanConfirmed` into mailbox.
 */
export interface SteeringConfirmReq {
  readonly request_id: string;
  readonly session_key: string;
  readonly expect_route: string;
  readonly text: string;
  readonly confirm_token?: string;
  readonly signed: SignedSteeringConfirm;
}

export interface SteeringConfirmResp {
  readonly request_id: string;
  readonly session_key: string;
  readonly status: "delivered" | "queued" | "expired" | "rejected";
  readonly route_used?: string;
  readonly expires_at?: number;
  readonly reason?: string;
}

export interface PtyConsentGrant {
  readonly session_key: string;
  readonly host_id: string;
  readonly device_id: string;
  readonly scope: "pty.input";
  readonly granted_at_ms: number;
  readonly expires_at_ms: number;
  readonly audit_id: string;
}

/** Matches `DeviceCertificate` v2 (board #18). */
export interface DeviceCertificateV2 {
  readonly version: 2;
  readonly device_id: string;
  readonly kind: "desktop" | "mobile";
  readonly name: string;
  readonly ed25519_pub_b64: string;
  readonly x25519_pub_b64: string;
  readonly approval_pub_p256_b64?: string;
  readonly created_at: number;
  readonly expires_at: number;
  readonly identity_sig_b64: string;
}

/** Matches `SignedRevocationList` (board #18). */
export interface SignedRevocationList {
  readonly version: number;
  readonly issued_at: number;
  readonly expires_at: number;
  readonly revoked_device_ids: readonly string[];
  readonly identity_sig_b64: string;
}

export interface PairedDeviceInfo {
  readonly device_id: string;
  readonly name: string;
  readonly kind: "desktop" | "mobile";
  readonly approval_pub_p256_b64?: string;
  readonly hardware_backed: boolean;
  readonly expires_at: number;
  readonly revoked: boolean;
}

export interface PairingQrPayload {
  readonly v: 1;
  readonly host_id: string;
  readonly relay_url: string;
  readonly host_x25519_pub_b64: string;
  readonly pairing_token: string;
  readonly safety_code: string;
  readonly expires_at_ms: number;
}

/** Matches `OuterFrame` (`GPTF` v1, board #18). */
export interface OuterFrame {
  readonly v: 1;
  readonly frame_id: string;
  readonly stream_id: string;
  readonly seq: number;
  readonly from_device: string;
  readonly to_devices: readonly string[];
  readonly ttl_sec: number;
  readonly class: "control" | "approval" | "pty";
  readonly ciphertext_b64: string;
}

/** Matches `WakePushFrame` (board #18). */
export interface WakePushFrame {
  readonly v: 1;
  readonly kind: "approval" | "attention";
  readonly host_device: string;
  readonly stream_seq: number;
  readonly expires_at: number;
  readonly preview_ct_b64?: string;
}

/**
 * Matches `crates/gitpulse-protocol/src/wire.rs` `ControlPlaneEvent` (`tag = "type"`, snake_case, board #18).
 */
export type ControlPlaneEvent =
  | {
      readonly type: "inbox_snapshot";
      readonly host_epoch: number;
      readonly seq: number;
      readonly sessions: readonly SessionSnapshot[];
      readonly open_tickets: readonly ApprovalTicket[];
      readonly probe_gaps: readonly ProbeGap[];
      readonly pty_grants?: readonly PtyConsentGrant[];
      readonly paired_devices?: readonly PairedDeviceInfo[];
    }
  | {
      readonly type: "session_delta";
      readonly host_epoch: number;
      readonly seq: number;
      readonly session: SessionSnapshot;
    }
  | {
      readonly type: "approval_created";
      readonly host_epoch: number;
      readonly seq: number;
      readonly ticket: ApprovalTicket;
    }
  | {
      readonly type: "approval_decide";
      readonly decision: SignedApprovalDecision;
    }
  | ({
      readonly type: "approval_resolved";
      readonly host_epoch?: number;
      readonly seq?: number;
    } & ApprovalResolvedEvent)
  | {
      readonly type: "steering_preview_resp";
      readonly payload: SteeringPreviewResp;
    }
  | {
      readonly type: "steering_confirm_resp";
      readonly payload: SteeringConfirmResp;
    }
  | {
      readonly type: "steering_expired";
      readonly host_epoch: number;
      readonly seq: number;
      readonly request_id: string;
      readonly session_key: string;
      readonly harness: HarnessKind;
      readonly message: string;
    }
  | {
      readonly type: "cursor_gap";
      readonly current_epoch: number;
      readonly oldest_available_seq: number;
      readonly latest_seq: number;
    }
  | {
      readonly type: "snapshot_resync_req";
      readonly after_epoch: number;
      readonly after_seq: number;
    }
  | {
      readonly type: "pty_consent_granted";
      readonly host_epoch: number;
      readonly seq: number;
      readonly grant: PtyConsentGrant;
    }
  | {
      readonly type: "pty_consent_revoked";
      readonly host_epoch: number;
      readonly seq: number;
      readonly session_key: string;
      readonly reason: string;
    }
  | {
      readonly type: "pty_output";
      readonly host_epoch: number;
      readonly seq: number;
      readonly session_key: string;
      readonly chunk_b64: string;
    }
  | {
      readonly type: "revocation_list_update";
      readonly revocation_list: SignedRevocationList;
    }
  | { readonly type: "ping"; readonly ts_ms: number }
  | { readonly type: "pong"; readonly ts_ms: number };

export interface BannerAlert {
  readonly id: string;
  readonly severity: "info" | "warning" | "error";
  readonly kind:
    | "not_applied"
    | "steering_expired"
    | "cursor_gap"
    | "revoked"
    | "pty_locked";
  readonly title: string;
  readonly message: string;
  readonly created_at_ms: number;
  readonly ticket_id?: string;
  readonly session_key?: string;
}

/**
 * Derives honest, runtime-grounded capability badges for a session (Opus Section 4).
 * Never pretends Antigravity is gated or has native idle wake.
 */
export function deriveCapabilityBadges(session: SessionSnapshot): CapabilityBadge[] {
  const badges: CapabilityBadge[] = [];
  const cap = session.capabilities;

  switch (cap.approval_channel) {
    case "presence_gated_hook":
      badges.push({
        label: "Universal Presence-Gated",
        tone: "success",
        detail: "Instant <1ms bypass while at desk; holds for mobile approval when away/locked.",
      });
      break;
    case "codex_app_server":
      badges.push({
        label: "Codex App-Server",
        tone: "info",
        detail: "Item-bound approvals with no hook timeout and true idle wake via turn/start.",
      });
      break;
    case "codex_tui_hook":
      badges.push({
        label: "Codex TUI Hook",
        tone: "info",
        detail: "User-trusted TUI PermissionRequest hook bridge.",
      });
      break;
    case "manvi_policy":
      badges.push({
        label: "Manvi Policy Authority",
        tone: "info",
        detail: "Single-use, hash-bound, TTL'd grants; hard denies remain non-overridable.",
      });
      break;
    case "pty_human":
      badges.push({
        label: "PTY Human Step-Up",
        tone: "warning",
        detail: "Hosted CLI session; requires explicit Desktop consent + biometric step-up.",
      });
      break;
    case "native_only":
      badges.push({
        label: "Ungated / Native Only",
        tone: "warning",
        detail:
          session.harness === "antigravity"
            ? "Antigravity tool calls are NOT gated remotely or by Manvi; approve natively in IDE."
            : "Approvals stay native on desktop.",
      });
      break;
    case "observe_only":
      badges.push({
        label: "Observe Only",
        tone: "neutral",
        detail: "Live telemetry and diffs only.",
      });
      break;
  }

  if (cap.ungated && !badges.some((b) => b.label === "Ungated / Native Only")) {
    badges.push({
      label: "Ungated / Native Only",
      tone: "warning",
      detail: "Tool calls are ungated remotely; approve natively on desktop.",
    });
  }

  if (!cap.supports_idle_wake && cap.queued_steering_ttl_sec && cap.queued_steering_ttl_sec > 0) {
    const hours = Math.round(cap.queued_steering_ttl_sec / 3600);
    badges.push({
      label: "Queued — Next Turn",
      tone: "warning",
      detail: `No native idle wake. Steering waits in mailbox (${hours}h TTL) until next turn or alerts on expiry.`,
    });
  }

  return badges;
}

/**
 * Validates and parses a QR pairing payload scanned on Pair screen.
 */
export function parsePairingQrPayload(raw: string): PairingQrPayload {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    throw new Error("Invalid QR code: payload is not valid JSON.");
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error("Invalid QR code: expected JSON object.");
  }
  const obj = parsed as Record<string, unknown>;
  if (obj.v !== 1) {
    throw new Error(`Unsupported pairing QR version: ${String(obj.v)}`);
  }
  const required = [
    "host_id",
    "relay_url",
    "host_x25519_pub_b64",
    "pairing_token",
    "safety_code",
  ] as const;
  for (const field of required) {
    if (typeof obj[field] !== "string" || (obj[field] as string).length === 0) {
      throw new Error(`Invalid pairing QR payload: missing "${field}".`);
    }
  }
  if (typeof obj.expires_at_ms !== "number") {
    throw new Error("Invalid pairing QR payload: missing numeric expires_at_ms.");
  }
  return obj as unknown as PairingQrPayload;
}
