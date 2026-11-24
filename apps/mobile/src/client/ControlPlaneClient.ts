import {
  assertValidSignedApprovalDecision,
  assertValidSignedSteeringConfirm,
  type ApprovalTicket,
  type BannerAlert,
  type ControlPlaneEvent,
  type PairedDeviceInfo,
  type ProbeGap,
  type PtyConsentGrant,
  type SessionSnapshot,
  type SignedApprovalDecision,
  type SteeringConfirmReq,
  type SteeringConfirmResp,
  type SteeringPreviewReq,
  type SteeringPreviewResp,
} from "../protocol/types";

export const RECONNECT_MIN_MS = 250;
export const RECONNECT_MAX_MS = 15_000;
export const RELAY_CLOSE_REVOKED_OR_DOWNGRADED = 4003;

export type ConnectionState =
  | "disconnected"
  | "connecting"
  | "handshaking"
  | "connected"
  | "revoked";

export interface ControlPlaneState {
  readonly connectionState: ConnectionState;
  readonly hostEpoch: number;
  readonly lastSeq: number;
  readonly resyncing: boolean;
  readonly sessions: readonly SessionSnapshot[];
  readonly openTickets: readonly ApprovalTicket[];
  readonly probeGaps: readonly ProbeGap[];
  readonly ptyGrants: Readonly<Record<string, PtyConsentGrant>>;
  readonly pairedDevices: readonly PairedDeviceInfo[];
  readonly banners: readonly BannerAlert[];
}

export interface WebSocketLike {
  send(data: string): void;
  close(code?: number, reason?: string): void;
  onopen: (() => void) | null;
  onmessage: ((event: { data: string }) => void) | null;
  onclose: ((event: { code: number; reason?: string }) => void) | null;
  onerror: ((event: unknown) => void) | null;
}

export interface ControlPlaneTransport {
  connect(url: string): WebSocketLike;
  previewSteering?(req: SteeringPreviewReq): Promise<SteeringPreviewResp>;
  confirmSteering?(req: SteeringConfirmReq): Promise<SteeringConfirmResp>;
  revokeDevice?(deviceId: string): Promise<void>;
}

export interface ControlPlaneClientOptions {
  readonly relayWsBaseUrl: string;
  readonly deviceId: string;
  readonly transport: ControlPlaneTransport;
  readonly now?: () => number;
  readonly random?: () => number;
  readonly scheduleTimer?: (fn: () => void, ms: number) => ReturnType<typeof setTimeout>;
  readonly clearTimer?: (id: ReturnType<typeof setTimeout>) => void;
}

/**
 * Bounded exponential jittered reconnect delay (`250ms..15s`, Opus Relay spec).
 */
export function computeReconnectDelayMs(
  attempt: number,
  randomFn: () => number = Math.random,
): number {
  const safeAttempt = Math.max(0, Math.floor(attempt));
  const exp = Math.min(RECONNECT_MAX_MS, RECONNECT_MIN_MS * Math.pow(2, safeAttempt));
  const clampedRand = Math.min(1, Math.max(0, randomFn()));
  return Math.round(RECONNECT_MIN_MS + clampedRand * (exp - RECONNECT_MIN_MS));
}

/**
 * Opus P0-7 guard: PTY input is RCE and MUST remain strictly read-only unless:
 * 1. An unexpired Desktop-granted `PtyConsentGrant` matches `(sessionKey, deviceId)` with scope `pty.input`.
 * 2. Local hardware biometric step-up succeeded AFTER the grant was issued and before expiry.
 */
export function isPtyInputUnlocked(params: {
  grant: PtyConsentGrant | undefined;
  sessionKey: string;
  deviceId: string;
  biometricAttestedAtMs: number | null;
  nowMs?: number;
}): boolean {
  const now = params.nowMs ?? Date.now();
  const { grant, sessionKey, deviceId, biometricAttestedAtMs } = params;
  if (!grant) return false;
  if (grant.scope !== "pty.input") return false;
  if (grant.session_key !== sessionKey) return false;
  if (grant.device_id !== deviceId) return false;
  if (now >= grant.expires_at_ms) return false;
  if (biometricAttestedAtMs === null) return false;
  if (biometricAttestedAtMs < grant.granted_at_ms) return false;
  return true;
}

/**
 * Mobile Control-Plane State Machine & WebSocket Client.
 * Aligned 100% with `crates/gitpulse-protocol/src/wire.rs` (board #18).
 *
 * Implements:
 * - Cursor tracking (`after_epoch`, `after_seq`) + bounded jittered reconnect (`250ms..15s`)
 * - Authoritative `cursor_gap` -> `snapshot_resync_req` -> `inbox_snapshot` full resync (Opus R6)
 * - Late-decision `not_applied` banners from `approval_resolved` (Opus P0-1)
 * - Honest Antigravity `steering_expired` alerts (2h TTL)
 * - Immediate eviction & zero-reconnect on relay close `4003` or `revocation_list_update`
 * - Two-step `preview` -> `confirm` steering with `expect_route` & idempotent `request_id`
 * - Strict Opus P0-4 validation on outgoing `approval_decide` messages
 */
export class ControlPlaneClient {
  private state: ControlPlaneState = {
    connectionState: "disconnected",
    hostEpoch: 0,
    lastSeq: 0,
    resyncing: false,
    sessions: [],
    openTickets: [],
    probeGaps: [],
    ptyGrants: {},
    pairedDevices: [],
    banners: [],
  };

  private ws: WebSocketLike | null = null;
  private listeners = new Set<(state: ControlPlaneState) => void>();
  private ptyOutputListeners = new Set<(sessionKey: string, chunkB64: string) => void>();
  private reconnectAttempt = 0;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private closedByUser = false;
  private bannerCounter = 0;

  private readonly now: () => number;
  private readonly random: () => number;
  private readonly scheduleTimer: (fn: () => void, ms: number) => ReturnType<typeof setTimeout>;
  private readonly clearTimer: (id: ReturnType<typeof setTimeout>) => void;

  constructor(private readonly options: ControlPlaneClientOptions) {
    this.now = options.now ?? (() => Date.now());
    this.random = options.random ?? (() => Math.random());
    this.scheduleTimer = options.scheduleTimer ?? ((fn, ms) => setTimeout(fn, ms));
    this.clearTimer = options.clearTimer ?? ((id) => clearTimeout(id));
  }

  getState(): ControlPlaneState {
    return this.state;
  }

  subscribe(listener: (state: ControlPlaneState) => void): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  subscribePtyOutput(listener: (sessionKey: string, chunkB64: string) => void): () => void {
    this.ptyOutputListeners.add(listener);
    return () => {
      this.ptyOutputListeners.delete(listener);
    };
  }

  connect(): void {
    if (this.state.connectionState === "revoked") {
      return;
    }
    this.closedByUser = false;
    this.clearReconnectTimer();

    const separator = this.options.relayWsBaseUrl.includes("?") ? "&" : "?";
    const url = `${this.options.relayWsBaseUrl}${separator}role=client&after_epoch=${this.state.hostEpoch}&after_seq=${this.state.lastSeq}`;

    this.setState({ connectionState: "connecting" });
    const socket = this.options.transport.connect(url);
    this.ws = socket;

    socket.onopen = () => {
      this.reconnectAttempt = 0;
      this.setState({ connectionState: "connected" });
      // Opus R6: Host is source of truth; request full inbox_snapshot on connect/reconnect.
      this.requestInboxSnapshot();
    };

    socket.onmessage = (event) => {
      this.handleRawMessage(event.data);
    };

    socket.onclose = (event) => {
      this.ws = null;
      if (event.code === RELAY_CLOSE_REVOKED_OR_DOWNGRADED) {
        this.handleRevocationOrDowngrade(
          event.reason || "Device revoked or remote_control entitlement downgraded (close 4003).",
        );
        return;
      }
      this.setState({ connectionState: "disconnected", resyncing: false });
      if (!this.closedByUser) {
        this.scheduleReconnect();
      }
    };

    socket.onerror = () => {
      // onclose handles backoff & resync
    };
  }

  disconnect(): void {
    this.closedByUser = true;
    this.clearReconnectTimer();
    this.ws?.close(1000, "client_disconnect");
    this.ws = null;
    if (this.state.connectionState !== "revoked") {
      this.setState({ connectionState: "disconnected", resyncing: false });
    }
  }

  requestInboxSnapshot(): void {
    this.setState({ resyncing: true });
    this.sendControlFrame({
      type: "snapshot_resync_req",
      after_epoch: this.state.hostEpoch,
      after_seq: this.state.lastSeq,
    });
  }

  /**
   * Sends a hardware-signed P-256 `approval_decide` event over the tunnel.
   * Enforces Opus P0-4 before serialization (rejects `updatedInput`/`updatedPermissions`).
   */
  submitApprovalDecision(signedDecision: SignedApprovalDecision): void {
    assertValidSignedApprovalDecision(signedDecision);
    this.sendControlFrame({
      type: "approval_decide",
      decision: signedDecision,
    });
  }

  async previewSteering(req: SteeringPreviewReq): Promise<SteeringPreviewResp> {
    if (this.options.transport.previewSteering) {
      return this.options.transport.previewSteering(req);
    }
    const session = this.state.sessions.find((s) => s.session_key === req.session_key);
    if (!session) {
      throw new Error(`Session not found: ${req.session_key}`);
    }
    if (session.harness === "antigravity" && session.fully_idle) {
      return {
        request_id: req.request_id,
        session_key: req.session_key,
        expect_route: "queued_next_turn",
        delivery_mode: "queued_next_turn",
        queued_ttl_sec: session.capabilities.queued_steering_ttl_sec ?? 7200,
        warning:
          "Antigravity has no native idle wake. This message will wait in the mailbox up to 2 hours and alert if it expires.",
      };
    }
    if (session.capabilities.approval_channel === "codex_app_server") {
      return {
        request_id: req.request_id,
        session_key: req.session_key,
        expect_route: "codex_turn_start",
        delivery_mode: session.fully_idle ? "wake_resume" : "immediate",
      };
    }
    if (session.harness === "manvi") {
      return {
        request_id: req.request_id,
        session_key: req.session_key,
        expect_route: "manvi_continue",
        delivery_mode: session.fully_idle ? "wake_resume" : "immediate",
      };
    }
    if (session.fully_idle && session.harness === "claude") {
      return {
        request_id: req.request_id,
        session_key: req.session_key,
        expect_route: "async_rewake",
        delivery_mode: "wake_resume",
      };
    }
    return {
      request_id: req.request_id,
      session_key: req.session_key,
      expect_route: "peer_inbox",
      delivery_mode: "immediate",
    };
  }

  async confirmSteering(req: SteeringConfirmReq): Promise<SteeringConfirmResp> {
    if (!req.request_id || req.request_id.trim().length === 0) {
      throw new Error("Steering confirmation requires an idempotent request_id.");
    }
    // Opus #23 P1: Fail closed unless steering confirm carries a hardware P-256 signature
    // binding exact text_sha256, session_key, request_id, and expect_route.
    assertValidSignedSteeringConfirm(req.signed);
    if (
      req.signed.request_id !== req.request_id ||
      req.signed.session_key !== req.session_key ||
      req.signed.expect_route !== req.expect_route
    ) {
      throw new Error(
        "SECURITY VIOLATION (Opus #23 P1): SteeringConfirmReq fields do not match SignedSteeringConfirm envelope.",
      );
    }
    if (this.options.transport.confirmSteering) {
      return this.options.transport.confirmSteering(req);
    }
    this.sendControlFrame({
      type: "steering_confirm_req",
      ...req,
    });
    const isQueued = req.expect_route === "queued_next_turn";
    return {
      request_id: req.request_id,
      session_key: req.session_key,
      status: isQueued ? "queued" : "delivered",
      route_used: req.expect_route,
      expires_at: isQueued ? Math.floor(this.now() / 1000) + 7200 : undefined,
      reason: isQueued
        ? "Queued (next turn) — 2h TTL. You will be notified if it expires."
        : `Delivered via ${req.expect_route} (hardware P-256 signed).`,
    };
  }

  /**
   * Sends PTY input ONLY when Opus P0-7 Desktop time-boxed consent + local biometric step-up
   * are both valid. Fails closed otherwise.
   */
  sendPtyInput(
    sessionKey: string,
    dataB64: string,
    biometricAttestedAtMs: number | null,
  ): void {
    const grant = this.state.ptyGrants[sessionKey];
    const unlocked = isPtyInputUnlocked({
      grant,
      sessionKey,
      deviceId: this.options.deviceId,
      biometricAttestedAtMs,
      nowMs: this.now(),
    });
    if (!unlocked || !grant) {
      this.pushBanner({
        severity: "warning",
        kind: "pty_locked",
        title: "PTY Input Blocked (Read-Only)",
        message:
          "pty_input requires an active Desktop-granted time-boxed consent and biometric step-up.",
        session_key: sessionKey,
      });
      throw new Error(
        "SECURITY VIOLATION (Opus P0-7): pty_input refused because Desktop consent or biometric step-up is missing/expired.",
      );
    }
    this.sendControlFrame({
      type: "pty_input",
      session_key: sessionKey,
      audit_id: grant.audit_id,
      data_b64: dataB64,
    });
  }

  async revokeDevice(deviceId: string): Promise<void> {
    if (this.options.transport.revokeDevice) {
      await this.options.transport.revokeDevice(deviceId);
    } else {
      this.sendControlFrame({
        type: "device_revoke_req",
        device_id: deviceId,
      });
    }
    this.setState({
      pairedDevices: this.state.pairedDevices.map((d) =>
        d.device_id === deviceId ? { ...d, revoked: true } : d,
      ),
    });
  }

  dismissBanner(bannerId: string): void {
    this.setState({
      banners: this.state.banners.filter((b) => b.id !== bannerId),
    });
  }

  /** Exposed for deterministic unit tests and inbound decrypted event dispatch. */
  applyEvent(event: ControlPlaneEvent): void {
    switch (event.type) {
      case "inbox_snapshot": {
        this.setState({
          hostEpoch: event.host_epoch,
          lastSeq: event.seq,
          resyncing: false,
          sessions: [...event.sessions],
          openTickets: [...event.open_tickets],
          probeGaps: [...event.probe_gaps],
          ptyGrants: event.pty_grants
            ? Object.fromEntries(event.pty_grants.map((g) => [g.session_key, g]))
            : this.state.ptyGrants,
          pairedDevices: event.paired_devices
            ? [...event.paired_devices]
            : this.state.pairedDevices,
        });
        break;
      }

      case "session_delta": {
        const existingIdx = this.state.sessions.findIndex(
          (s) => s.session_key === event.session.session_key,
        );
        const nextSessions = [...this.state.sessions];
        if (existingIdx >= 0) {
          nextSessions[existingIdx] = event.session;
        } else {
          nextSessions.push(event.session);
        }
        this.setState({
          hostEpoch: event.host_epoch,
          lastSeq: Math.max(this.state.lastSeq, event.seq),
          sessions: nextSessions,
        });
        break;
      }

      case "approval_created": {
        const filtered = this.state.openTickets.filter(
          (t) => t.ticket_id !== event.ticket.ticket_id,
        );
        this.setState({
          hostEpoch: event.host_epoch,
          lastSeq: Math.max(this.state.lastSeq, event.seq),
          openTickets: [event.ticket, ...filtered],
        });
        break;
      }

      case "approval_resolved": {
        const remaining = this.state.openTickets.filter((t) => t.ticket_id !== event.ticket_id);
        this.setState({
          hostEpoch: event.host_epoch ?? this.state.hostEpoch,
          lastSeq: event.seq !== undefined ? Math.max(this.state.lastSeq, event.seq) : this.state.lastSeq,
          openTickets: remaining,
        });
        if (event.outcome === "not_applied") {
          this.pushBanner({
            severity: "warning",
            kind: "not_applied",
            title: "Decision Not Applied",
            message:
              event.reason ??
              `Ticket ${event.ticket_id} was already cancelled or handled locally on desktop (user returned or pressed Esc/Ctrl-C).`,
            ticket_id: event.ticket_id,
          });
        }
        break;
      }

      case "steering_expired": {
        this.setState({
          hostEpoch: event.host_epoch,
          lastSeq: Math.max(this.state.lastSeq, event.seq),
        });
        this.pushBanner({
          severity: "warning",
          kind: "steering_expired",
          title: "Queued Steering Expired",
          message: event.message,
          session_key: event.session_key,
        });
        break;
      }

      case "pty_consent_granted": {
        this.setState({
          hostEpoch: event.host_epoch,
          lastSeq: Math.max(this.state.lastSeq, event.seq),
          ptyGrants: {
            ...this.state.ptyGrants,
            [event.grant.session_key]: event.grant,
          },
        });
        break;
      }

      case "pty_consent_revoked": {
        const nextGrants = { ...this.state.ptyGrants };
        delete nextGrants[event.session_key];
        this.setState({
          hostEpoch: event.host_epoch,
          lastSeq: Math.max(this.state.lastSeq, event.seq),
          ptyGrants: nextGrants,
        });
        this.pushBanner({
          severity: "info",
          kind: "pty_locked",
          title: "PTY Returned to Read-Only",
          message: event.reason,
          session_key: event.session_key,
        });
        break;
      }

      case "pty_output": {
        for (const listener of this.ptyOutputListeners) {
          listener(event.session_key, event.chunk_b64);
        }
        break;
      }

      case "cursor_gap": {
        // Opus R6: Relay ring dropped frames; immediately request authoritative inbox_snapshot from host.
        this.setState({
          hostEpoch: event.current_epoch,
          resyncing: true,
        });
        this.pushBanner({
          severity: "info",
          kind: "cursor_gap",
          title: "Resyncing Control Plane",
          message: `Cursor gap detected (oldest available seq ${event.oldest_available_seq}, latest ${event.latest_seq}); requesting full inbox_snapshot from host.`,
        });
        this.requestInboxSnapshot();
        break;
      }

      case "revocation_list_update": {
        const revokedSet = new Set(event.revocation_list.revoked_device_ids);
        if (revokedSet.has(this.options.deviceId)) {
          this.handleRevocationOrDowngrade(
            `This device was revoked (SignedRevocationList v${event.revocation_list.version}).`,
          );
        } else {
          this.setState({
            pairedDevices: this.state.pairedDevices.map((d) =>
              revokedSet.has(d.device_id) ? { ...d, revoked: true } : d,
            ),
          });
        }
        break;
      }

      case "approval_decide":
      case "steering_preview_resp":
      case "steering_confirm_resp":
      case "snapshot_resync_req":
      case "ping":
      case "pong":
        break;
    }
  }

  private handleRawMessage(raw: string): void {
    let parsed: unknown;
    try {
      parsed = JSON.parse(raw);
    } catch {
      return;
    }
    if (parsed && typeof parsed === "object" && "type" in parsed) {
      this.applyEvent(parsed as ControlPlaneEvent);
    }
  }

  private handleRevocationOrDowngrade(reason: string): void {
    this.closedByUser = true;
    this.clearReconnectTimer();
    this.ws?.close(1000, "revoked");
    this.ws = null;
    this.setState({
      connectionState: "revoked",
      resyncing: false,
      openTickets: [],
      ptyGrants: {},
    });
    this.pushBanner({
      severity: "error",
      kind: "revoked",
      title: "Remote Control Disconnected",
      message: reason,
    });
  }

  private sendControlFrame(payload: Record<string, unknown>): void {
    if (!this.ws) {
      return;
    }
    this.ws.send(JSON.stringify(payload));
  }

  private scheduleReconnect(): void {
    const delay = computeReconnectDelayMs(this.reconnectAttempt, this.random);
    this.reconnectAttempt += 1;
    this.reconnectTimer = this.scheduleTimer(() => {
      this.reconnectTimer = null;
      this.connect();
    }, delay);
  }

  private clearReconnectTimer(): void {
    if (this.reconnectTimer !== null) {
      this.clearTimer(this.reconnectTimer);
      this.reconnectTimer = null;
    }
  }

  private pushBanner(input: Omit<BannerAlert, "id" | "created_at_ms">): void {
    this.bannerCounter += 1;
    const banner: BannerAlert = {
      ...input,
      id: `banner-${this.bannerCounter}`,
      created_at_ms: this.now(),
    };
    this.setState({
      banners: [banner, ...this.state.banners].slice(0, 10),
    });
  }

  private setState(partial: Partial<ControlPlaneState>): void {
    this.state = { ...this.state, ...partial };
    for (const listener of this.listeners) {
      listener(this.state);
    }
  }
}
