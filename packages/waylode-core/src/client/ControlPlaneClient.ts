/**
 * Transport-injected REST + WebSocket control-plane client state machine.
 *
 * Implements:
 * - Cursor tracking (`after_epoch`, `after_seq`) in the `/v1/tunnel/ws` query string.
 * - Jittered exponential reconnect backoff strictly bounded to `250ms..15_000ms` (Opus R8 / relay gap 8).
 * - Immediate stop on close code `4003` (device revoked / tier downgraded — fail closed).
 * - `cursor_gap` -> `snapshot_resync_req` -> authoritative `inbox_snapshot` resync (Opus R6).
 * - Defense-in-depth Opus P0-4 validation before sending `approval_decide`.
 * - Late decision `not_applied` surfacing and queued steering `expired` notifications.
 *
 * Pure TypeScript — zero DOM, Svelte, or Tauri imports.
 */

import {
  assertCleanSignedApprovalDecision,
  assertCleanSignedSteeringConfirm,
  type ApprovalResolvedEvent,
  type ApprovalTicket,
  type ControlPlaneEvent,
  type SessionSnapshot,
  type SignedApprovalDecision,
  type SteeringConfirmReq,
  type SteeringConfirmResp,
  type SteeringPreviewReq,
  type SteeringPreviewResp,
  type SteeringStatus,
} from "../protocol/types";

export const RECONNECT_MIN_MS = 250;
export const RECONNECT_MAX_MS = 15_000;
export const WS_CLOSE_REVOKED = 4003;
export const WS_CLOSE_GOING_AWAY = 1001;

export type ConnectionState =
  | "disconnected"
  | "connecting"
  | "connected"
  | "reconnecting"
  | "resyncing"
  | "revoked";

export interface CursorPosition {
  after_epoch: number;
  after_seq: number;
}

export interface WebSocketLike {
  send(data: string): void;
  close(code?: number, reason?: string): void;
  onOpen(handler: () => void): void;
  onMessage(handler: (data: string) => void): void;
  onClose(handler: (code: number, reason: string) => void): void;
  onError(handler: (error: unknown) => void): void;
}

export interface ControlPlaneTransport {
  /** Signed REST request helper (used for push-token, devices, or REST snapshot fallback). */
  fetchJson<TResponse>(
    method: "GET" | "POST" | "PUT" | "DELETE",
    path: string,
    body?: unknown,
  ): Promise<TResponse>;

  /** Opens an authenticated WebSocket tunnel for the given relative URI. */
  openWebSocket(
    requestUri: string,
    headers?: Record<string, string>,
  ): WebSocketLike;
}

export interface TimerAdapter {
  setTimeout(fn: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
}

export interface SteeringRecord {
  request_id: string;
  session_key: string;
  status: SteeringStatus;
  route_used?: string;
  expires_at?: number;
  reason?: string;
}

export interface ControlPlaneState {
  connection: ConnectionState;
  cursor: CursorPosition;
  reconnectAttempt: number;
  lastResyncRequestId: string | null;
  sessions: ReadonlyMap<string, SessionSnapshot>;
  openApprovals: ReadonlyMap<string, ApprovalTicket>;
  steering: ReadonlyMap<string, SteeringRecord>;
  lastError: string | null;
}

export interface ControlPlaneClientCallbacks {
  onStateChange?: (state: ControlPlaneState) => void;
  onEvent?: (event: ControlPlaneEvent) => void;
  onApprovalResolved?: (resolved: ApprovalResolvedEvent) => void;
  onSteeringExpired?: (info: {
    request_id: string;
    session_key: string;
    expired_at: number;
  }) => void;
  onRevoked?: (reason: string) => void;
}

export interface ControlPlaneClientOptions {
  transport: ControlPlaneTransport;
  role?: "client" | "host";
  initialCursor?: CursorPosition;
  callbacks?: ControlPlaneClientCallbacks;
  /** Injectable PRNG in `[0, 1)` for deterministic jitter tests. */
  random?: () => number;
  timers?: TimerAdapter;
  /** Optional header provider (e.g. Ed25519 signed headers from native Rust UniFFI). */
  authHeaders?: (
    method: string,
    requestUri: string,
  ) => Record<string, string> | undefined;
  /** Optional id generator for `snapshot_resync_req` correlation IDs. */
  nextId?: () => string;
}

/**
 * Computes jittered exponential backoff strictly bounded to `[250ms, 15_000ms]`.
 *
 * Uses full jitter around the capped exponential ceiling (`250 * 2^attempt`,
 * clamped to `15_000`), guaranteeing a minimum floor of `250ms` and ceiling of
 * `15_000ms` so server restarts (`1001`) never trigger a thundering herd.
 */
export function computeReconnectDelayMs(
  attempt: number,
  randomValue: number = Math.random(),
): number {
  const safeAttempt = Math.max(0, Math.floor(attempt));
  const clampedRand = Number.isFinite(randomValue)
    ? Math.min(1, Math.max(0, randomValue))
    : 0.5;
  const expCeiling = Math.min(
    RECONNECT_MAX_MS,
    RECONNECT_MIN_MS * 2 ** Math.min(safeAttempt, 10),
  );
  const spread = expCeiling - RECONNECT_MIN_MS;
  return Math.round(RECONNECT_MIN_MS + spread * clampedRand);
}

/** Builds `/v1/tunnel/ws?role=...&after_epoch=...&after_seq=...`. */
export function buildTunnelRequestUri(
  role: "client" | "host",
  cursor: CursorPosition,
): string {
  const epoch = Math.max(0, Math.floor(cursor.after_epoch));
  const seq = Math.max(0, Math.floor(cursor.after_seq));
  return `/v1/tunnel/ws?role=${role}&after_epoch=${epoch}&after_seq=${seq}`;
}

function defaultTimers(): TimerAdapter {
  const g = globalThis as unknown as {
    setTimeout?: (fn: () => void, ms: number) => unknown;
    clearTimeout?: (handle: unknown) => void;
  };
  return {
    setTimeout: (fn, ms) => {
      if (typeof g.setTimeout !== "function") {
        throw new Error("TimerAdapter.setTimeout unavailable in this runtime");
      }
      return g.setTimeout(fn, ms);
    },
    clearTimeout: (handle) => {
      if (typeof g.clearTimeout === "function") {
        g.clearTimeout(handle);
      }
    },
  };
}

export class ControlPlaneClient {
  private readonly transport: ControlPlaneTransport;
  private readonly role: "client" | "host";
  private readonly callbacks: ControlPlaneClientCallbacks;
  private readonly random: () => number;
  private readonly timers: TimerAdapter;
  private readonly authHeaders?: (
    method: string,
    requestUri: string,
  ) => Record<string, string> | undefined;
  private readonly nextId: () => string;

  private ws: WebSocketLike | null = null;
  private reconnectTimer: unknown | null = null;
  private manuallyStopped = false;
  private idCounter = 0;

  private connection: ConnectionState = "disconnected";
  private cursor: CursorPosition;
  private reconnectAttempt = 0;
  private lastResyncRequestId: string | null = null;
  private sessions = new Map<string, SessionSnapshot>();
  private openApprovals = new Map<string, ApprovalTicket>();
  private steering = new Map<string, SteeringRecord>();
  private lastError: string | null = null;

  constructor(options: ControlPlaneClientOptions) {
    this.transport = options.transport;
    this.role = options.role ?? "client";
    this.callbacks = options.callbacks ?? {};
    this.random = options.random ?? Math.random;
    this.timers = options.timers ?? defaultTimers();
    this.authHeaders = options.authHeaders;
    this.nextId =
      options.nextId ??
      (() => {
        this.idCounter += 1;
        return `resync_${this.idCounter}`;
      });
    this.cursor = {
      after_epoch: options.initialCursor?.after_epoch ?? 0,
      after_seq: options.initialCursor?.after_seq ?? 0,
    };
  }

  public getState(): ControlPlaneState {
    return {
      connection: this.connection,
      cursor: { ...this.cursor },
      reconnectAttempt: this.reconnectAttempt,
      lastResyncRequestId: this.lastResyncRequestId,
      sessions: new Map(this.sessions),
      openApprovals: new Map(this.openApprovals),
      steering: new Map(this.steering),
      lastError: this.lastError,
    };
  }

  public connect(): void {
    if (this.connection === "revoked") return;
    this.manuallyStopped = false;
    this.clearReconnectTimer();
    this.openSocket();
  }

  public disconnect(): void {
    this.manuallyStopped = true;
    this.clearReconnectTimer();
    if (this.ws) {
      const active = this.ws;
      this.ws = null;
      active.close(1000, "client_disconnect");
    }
    this.connection = "disconnected";
    this.emitState();
  }

  /**
   * Advances the tracked relay cursor when an authenticated outer frame sequence
   * is accepted by the Noise `IK` transport layer.
   */
  public advanceCursor(epoch: number, seq: number): void {
    if (epoch > this.cursor.after_epoch) {
      this.cursor = { after_epoch: epoch, after_seq: seq };
    } else if (epoch === this.cursor.after_epoch && seq > this.cursor.after_seq) {
      this.cursor = { after_epoch: epoch, after_seq: seq };
    }
  }

  /** Sends an arbitrary `ControlPlaneEvent` over the active WebSocket. */
  public sendEvent(event: ControlPlaneEvent): void {
    if (!this.ws) {
      throw new Error("ControlPlaneClient is not connected");
    }
    if (event.type === "approval_decide") {
      assertCleanSignedApprovalDecision(event.decision);
    }
    if (event.type === "steering_confirm_req" && event.req.signed !== undefined) {
      assertCleanSignedSteeringConfirm(event.req.signed);
    }
    this.ws.send(JSON.stringify(event));
  }

  /** Submits a hardware-signed P-256 `SignedApprovalDecision` (enforces Opus P0-4). */
  public sendApprovalDecision(decision: SignedApprovalDecision): void {
    assertCleanSignedApprovalDecision(decision);
    this.sendEvent({ type: "approval_decide", decision });
  }

  /** Step 1 of two-step confirm steering. */
  public sendSteeringPreview(req: SteeringPreviewReq): void {
    this.sendEvent({ type: "steering_preview_req", req });
  }

  /** Step 2 of two-step confirm steering. */
  public sendSteeringConfirm(req: SteeringConfirmReq): void {
    this.sendEvent({ type: "steering_confirm_req", req });
  }

  /** Requests a fresh authoritative `inbox_snapshot` from the host. */
  public requestSnapshotResync(): string {
    const requestId = this.nextId();
    this.lastResyncRequestId = requestId;
    this.connection = "resyncing";
    this.emitState();
    this.sendEvent({ type: "snapshot_resync_req", request_id: requestId });
    return requestId;
  }

  /** Convenience REST wrapper through the injected transport. */
  public rest<TResponse>(
    method: "GET" | "POST" | "PUT" | "DELETE",
    path: string,
    body?: unknown,
  ): Promise<TResponse> {
    return this.transport.fetchJson<TResponse>(method, path, body);
  }

  /** Feeds a decrypted `ControlPlaneEvent` directly into the state machine. */
  public handleEvent(event: ControlPlaneEvent): void {
    switch (event.type) {
      case "inbox_snapshot": {
        this.sessions.clear();
        for (const session of event.sessions) {
          this.sessions.set(session.session_key, session);
        }
        this.openApprovals.clear();
        for (const ticket of event.open_approvals) {
          this.openApprovals.set(ticket.ticket_id, ticket);
        }
        this.advanceCursor(event.host_epoch, event.seq);
        if (this.connection === "resyncing") {
          this.connection = "connected";
        }
        break;
      }
      case "session_delta": {
        this.sessions.set(event.session.session_key, event.session);
        break;
      }
      case "approval_created": {
        this.openApprovals.set(event.ticket.ticket_id, event.ticket);
        break;
      }
      case "approval_resolved": {
        this.openApprovals.delete(event.resolved.ticket_id);
        this.callbacks.onApprovalResolved?.(event.resolved);
        break;
      }
      case "steering_confirm_resp": {
        const r = event.resp;
        this.steering.set(r.request_id, {
          request_id: r.request_id,
          session_key: r.session_key,
          status: r.status,
          route_used: r.route_used,
          expires_at: r.expires_at,
          reason: r.reason,
        });
        break;
      }
      case "steering_expired": {
        const prev = this.steering.get(event.request_id);
        this.steering.set(event.request_id, {
          request_id: event.request_id,
          session_key: event.session_key,
          status: "expired",
          route_used: prev?.route_used,
          expires_at: event.expired_at,
          reason: "Queued steering message expired before the next turn",
        });
        this.callbacks.onSteeringExpired?.({
          request_id: event.request_id,
          session_key: event.session_key,
          expired_at: event.expired_at,
        });
        break;
      }
      case "cursor_gap": {
        // Relay ring buffer did not retain frames from our cursor -> request full
        // authoritative `inbox_snapshot` from host (Opus R6).
        this.cursor = {
          after_epoch: event.current_epoch,
          after_seq: event.latest_seq,
        };
        if (this.ws) {
          this.requestSnapshotResync();
        } else {
          this.connection = "resyncing";
        }
        break;
      }
      default:
        break;
    }

    this.callbacks.onEvent?.(event);
    this.emitState();
  }

  private openSocket(): void {
    const uri = buildTunnelRequestUri(this.role, this.cursor);
    const headers = this.authHeaders?.("GET", uri);
    this.connection =
      this.reconnectAttempt > 0 ? "reconnecting" : "connecting";
    this.emitState();

    const socket = this.transport.openWebSocket(uri, headers);
    this.ws = socket;

    socket.onOpen(() => {
      if (this.ws !== socket) return;
      this.reconnectAttempt = 0;
      this.lastError = null;
      this.connection = "connected";
      this.emitState();
    });

    socket.onMessage((raw) => {
      if (this.ws !== socket) return;
      try {
        const parsed = JSON.parse(raw) as ControlPlaneEvent;
        if (parsed && typeof parsed === "object" && typeof parsed.type === "string") {
          this.handleEvent(parsed);
        }
      } catch (err) {
        this.lastError = err instanceof Error ? err.message : String(err);
        this.emitState();
      }
    });

    socket.onError((err) => {
      if (this.ws !== socket) return;
      this.lastError = err instanceof Error ? err.message : String(err);
      this.emitState();
    });

    socket.onClose((code, reason) => {
      if (this.ws !== socket) return;
      this.ws = null;

      if (code === WS_CLOSE_REVOKED) {
        // Device revoked or entitlement downgraded — fail closed, never reconnect.
        this.connection = "revoked";
        this.lastError = reason || "Device revoked or entitlement removed";
        this.clearReconnectTimer();
        this.callbacks.onRevoked?.(this.lastError);
        this.emitState();
        return;
      }

      if (this.manuallyStopped) {
        this.connection = "disconnected";
        this.emitState();
        return;
      }

      this.scheduleReconnect();
    });
  }

  private scheduleReconnect(): void {
    this.clearReconnectTimer();
    const delayMs = computeReconnectDelayMs(
      this.reconnectAttempt,
      this.random(),
    );
    this.reconnectAttempt += 1;
    this.connection = "reconnecting";
    this.emitState();
    this.reconnectTimer = this.timers.setTimeout(() => {
      this.reconnectTimer = null;
      if (!this.manuallyStopped && this.connection !== "revoked") {
        this.openSocket();
      }
    }, delayMs);
  }

  private clearReconnectTimer(): void {
    if (this.reconnectTimer !== null) {
      this.timers.clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
  }

  private emitState(): void {
    this.callbacks.onStateChange?.(this.getState());
  }
}

export type { SteeringPreviewResp, SteeringConfirmResp };
