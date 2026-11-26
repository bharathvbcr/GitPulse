/**
 * Per-session runtime ControlCapability matrix and honest badge derivation (Opus §4).
 *
 * Key invariants:
 * - Never treat harness capabilities as a static table; prefer the runtime
 *   `CapabilitySet` reported by the host, falling back to conservative defaults.
 * - Antigravity sessions are explicitly marked `"ungated"` (Manvi does NOT gate
 *   Antigravity's own tool calls) and idle Antigravity steering surfaces
 *   honestly as `"Queued"` / `"Queued (next turn)"` with a 2h TTL.
 * - `pty_input_enabled` defaults to `false` (Opus P0-7).
 */

import type { ApprovalChannel, CapabilitySet } from "../protocol/types";

export const DEFAULT_QUEUED_STEERING_TTL_SEC = 2 * 60 * 60;

export type CapabilityBadge =
  | "ungated"
  | "Queued"
  | "Presence-Gated"
  | "Codex App-Server"
  | "Codex Hook"
  | "Manvi Policy"
  | "Native Only"
  | "PTY Human"
  | "Observe Only";

export interface ControlCapabilityDescriptor {
  channel: ApprovalChannel;
  badges: CapabilityBadge[];
  approvalLabel: string;
  steeringLabel: string;
  idleWakeLabel: string;
  isUngated: boolean;
  isQueuedNextTurn: boolean;
  canRemoteApprove: boolean;
  canPtyInput: boolean;
}

export interface DefaultCapabilityOptions {
  approvalChannel?: ApprovalChannel;
  ptyInputEnabled?: boolean;
}

/**
 * Returns conservative runtime capability defaults when a local plane row has not
 * yet received an explicit host `CapabilitySet`.
 */
export function defaultCapabilitiesForHarness(
  harness: string,
  opts: DefaultCapabilityOptions = {},
): CapabilitySet {
  const normalized = harness.trim().toLowerCase();
  const ptyInput = Boolean(opts.ptyInputEnabled);

  if (normalized === "claude" || normalized === "claude-code" || normalized === "claude-desktop") {
    const channel = opts.approvalChannel ?? "presence_gated_hook";
    return {
      harness: "claude",
      approval_channel: channel,
      supports_remote_approval: channel === "presence_gated_hook",
      supports_active_steering: true,
      supports_idle_wake: true,
      ungated: channel === "native_only" || channel === "observe_only",
      pty_input_enabled: ptyInput,
    };
  }

  if (normalized === "agy" || normalized === "antigravity") {
    const channel = opts.approvalChannel ?? "native_only";
    return {
      harness: "agy",
      approval_channel: channel,
      supports_remote_approval: false,
      supports_active_steering: true,
      supports_idle_wake: false,
      ungated: true,
      queued_steering_ttl_sec: DEFAULT_QUEUED_STEERING_TTL_SEC,
      pty_input_enabled: ptyInput,
    };
  }

  if (normalized === "codex") {
    const channel = opts.approvalChannel ?? "observe_only";
    return {
      harness: "codex",
      approval_channel: channel,
      supports_remote_approval: channel === "codex_app_server" || channel === "codex_tui_hook",
      supports_active_steering: channel === "codex_app_server",
      supports_idle_wake: channel === "codex_app_server",
      ungated: channel === "observe_only" || channel === "native_only",
      pty_input_enabled: ptyInput,
    };
  }

  if (normalized === "manvi") {
    const channel = opts.approvalChannel ?? "manvi_policy";
    return {
      harness: "manvi",
      approval_channel: channel,
      supports_remote_approval: channel === "manvi_policy",
      supports_active_steering: true,
      supports_idle_wake: true,
      ungated: false,
      pty_input_enabled: ptyInput,
    };
  }

  const channel = opts.approvalChannel ?? "observe_only";
  return {
    harness: normalized || "agent",
    approval_channel: channel,
    supports_remote_approval: false,
    supports_active_steering: false,
    supports_idle_wake: false,
    ungated: true,
    pty_input_enabled: ptyInput,
  };
}

export interface DescribeCapabilityContext {
  fullyIdle?: boolean;
  queuedSteeringCount?: number;
}

/**
 * Derives UI-ready labels and honest capability badges (including `"ungated"` and
 * `"Queued"`) for both Desktop and Mobile surfaces.
 */
export function describeControlCapabilities(
  caps: CapabilitySet,
  ctx: DescribeCapabilityContext = {},
): ControlCapabilityDescriptor {
  const badges: CapabilityBadge[] = [];

  switch (caps.approval_channel) {
    case "presence_gated_hook":
      badges.push("Presence-Gated");
      break;
    case "codex_app_server":
      badges.push("Codex App-Server");
      break;
    case "codex_tui_hook":
      badges.push("Codex Hook");
      break;
    case "manvi_policy":
      badges.push("Manvi Policy");
      break;
    case "native_only":
      badges.push("Native Only");
      break;
    case "pty_human":
      badges.push("PTY Human");
      break;
    case "observe_only":
      badges.push("Observe Only");
      break;
  }

  if (caps.ungated) {
    badges.push("ungated");
  }

  const queuedCount = ctx.queuedSteeringCount ?? 0;
  const isQueuedNextTurn =
    queuedCount > 0 ||
    (Boolean(ctx.fullyIdle) && caps.supports_active_steering && !caps.supports_idle_wake);
  if (isQueuedNextTurn) {
    badges.push("Queued");
  }

  const approvalLabel = (() => {
    switch (caps.approval_channel) {
      case "presence_gated_hook":
        return "Universal (presence-gated)";
      case "codex_app_server":
        return "Codex App-Server (item-bound)";
      case "codex_tui_hook":
        return "Codex TUI hook (user-trusted)";
      case "manvi_policy":
        return "Manvi policy authority";
      case "native_only":
        return "Native only (ungated)";
      case "pty_human":
        return "PTY human step-up";
      case "observe_only":
        return "Observe only";
    }
  })();

  const steeringLabel = !caps.supports_active_steering
    ? "Unavailable"
    : isQueuedNextTurn
      ? "Queued (next turn)"
      : "Active steering";

  const idleWakeLabel = caps.supports_idle_wake
    ? "Supported"
    : "Queued (next turn)";

  return {
    channel: caps.approval_channel,
    badges,
    approvalLabel,
    steeringLabel,
    idleWakeLabel,
    isUngated: caps.ungated,
    isQueuedNextTurn,
    canRemoteApprove: caps.supports_remote_approval,
    canPtyInput: caps.pty_input_enabled,
  };
}
