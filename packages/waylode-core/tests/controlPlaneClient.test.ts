import { describe, expect, it } from "vitest";
import {
  buildTunnelRequestUri,
  computeReconnectDelayMs,
  ControlPlaneClient,
  RECONNECT_MAX_MS,
  RECONNECT_MIN_MS,
  WS_CLOSE_GOING_AWAY,
  WS_CLOSE_REVOKED,
  type ApprovalResolvedEvent,
  type ControlPlaneEvent,
  type ControlPlaneTransport,
  type SessionSnapshot,
  type SignedApprovalDecision,
  type WebSocketLike,
} from "../src/index";

class FakeSocket implements WebSocketLike {
  public readonly sent: string[] = [];
  public closedWith: { code?: number; reason?: string } | null = null;
  private openHandler: (() => void) | null = null;
  private messageHandler: ((data: string) => void) | null = null;
  private closeHandler: ((code: number, reason: string) => void) | null = null;
  private errorHandler: ((err: unknown) => void) | null = null;

  send(data: string): void {
    this.sent.push(data);
  }
  close(code?: number, reason?: string): void {
    this.closedWith = { code, reason };
  }
  onOpen(handler: () => void): void {
    this.openHandler = handler;
  }
  onMessage(handler: (data: string) => void): void {
    this.messageHandler = handler;
  }
  onClose(handler: (code: number, reason: string) => void): void {
    this.closeHandler = handler;
  }
  onError(handler: (err: unknown) => void): void {
    this.errorHandler = handler;
  }

  emitOpen(): void {
    this.openHandler?.();
  }
  emitEvent(ev: ControlPlaneEvent): void {
    this.messageHandler?.(JSON.stringify(ev));
  }
  emitClose(code: number, reason = ""): void {
    this.closeHandler?.(code, reason);
  }
  emitError(err: unknown): void {
    this.errorHandler?.(err);
  }
}

class FakeTimers {
  private nextHandle = 1;
  public readonly pending = new Map<number, { fn: () => void; ms: number }>();

  setTimeout(fn: () => void, ms: number): number {
    const id = this.nextHandle++;
    this.pending.set(id, { fn, ms });
    return id;
  }
  clearTimeout(handle: unknown): void {
    this.pending.delete(handle as number);
  }
  flushNext(): number | null {
    const first = this.pending.entries().next();
    if (first.done) return null;
    const [id, entry] = first.value;
    this.pending.delete(id);
    entry.fn();
    return entry.ms;
  }
}

function sampleSession(sessionKey: string): SessionSnapshot {
  return {
    session_key: sessionKey,
    repo_path: "/repo",
    repo_label: "repo",
    worktree_path: "/repo",
    harness: "claude",
    title: "Claude",
    presence: "live",
    fully_idle: false,
    capabilities: {
      harness: "claude",
      approval_channel: "presence_gated_hook",
      supports_remote_approval: true,
      supports_active_steering: true,
      supports_idle_wake: true,
      ungated: false,
      pty_input_enabled: false,
    },
    active_subagents: [],
    pending_approval_ids: [],
    updated_at_ms: 1_800_000_000_000,
  };
}

describe("packages/waylode-core ControlPlaneClient", () => {
  it("bounds jittered reconnect backoff strictly to 250ms..15000ms", () => {
    for (let attempt = 0; attempt <= 20; attempt += 1) {
      for (const r of [0, 0.25, 0.5, 0.75, 1]) {
        const delay = computeReconnectDelayMs(attempt, r);
        expect(delay).toBeGreaterThanOrEqual(RECONNECT_MIN_MS);
        expect(delay).toBeLessThanOrEqual(RECONNECT_MAX_MS);
      }
    }
    expect(computeReconnectDelayMs(0, 1)).toBe(250);
    expect(computeReconnectDelayMs(1, 1)).toBe(500);
    expect(computeReconnectDelayMs(10, 1)).toBe(15_000);
    expect(computeReconnectDelayMs(10, 0)).toBe(250);
  });

  it("handles cursor_gap by requesting snapshot_resync_req and replacing state on inbox_snapshot", () => {
    const openedUris: string[] = [];
    const sockets: FakeSocket[] = [];
    const transport: ControlPlaneTransport = {
      async fetchJson() {
        throw new Error("unexpected rest");
      },
      openWebSocket(uri) {
        openedUris.push(uri);
        const s = new FakeSocket();
        sockets.push(s);
        return s;
      },
    };

    const client = new ControlPlaneClient({
      transport,
      initialCursor: { after_epoch: 5, after_seq: 10 },
    });
    client.connect();
    expect(openedUris[0]).toBe("/v1/tunnel/ws?role=client&after_epoch=5&after_seq=10");

    const ws = sockets[0];
    ws.emitOpen();
    expect(client.getState().connection).toBe("connected");

    // Seed stale session that should be cleared by authoritative snapshot.
    ws.emitEvent({
      type: "session_delta",
      host_id: "host-1",
      session: sampleSession("stale-session"),
    });
    expect(client.getState().sessions.has("stale-session")).toBe(true);

    // Relay signals cursor_gap -> client enters resyncing & sends snapshot_resync_req.
    ws.emitEvent({
      type: "cursor_gap",
      current_epoch: 6,
      oldest_available_seq: 40,
      latest_seq: 99,
    });
    expect(client.getState().connection).toBe("resyncing");
    expect(ws.sent).toHaveLength(1);
    expect(JSON.parse(ws.sent[0])).toEqual({
      type: "snapshot_resync_req",
      request_id: "resync_1",
    });

    // Host replies with authoritative inbox_snapshot re-announcing open approvals.
    ws.emitEvent({
      type: "inbox_snapshot",
      host_id: "host-1",
      host_epoch: 6,
      seq: 100,
      sessions: [sampleSession("fresh-session")],
      open_approvals: [
        {
          ticket_id: "tkt-1",
          host_id: "host-1",
          session_id: "fresh-session",
          dedup_key: "fresh-session:" + "a".repeat(64),
          harness: "claude",
          channel: "presence_gated_hook",
          tool_name: "Bash",
          tool_input: { command: "cargo test" },
          cwd: "/repo",
          input_sha256: "a".repeat(64),
          issued_at: 1000,
          expires_at: 2800,
        },
      ],
      generated_at_ms: 1_800_000_000_000,
    });

    const state = client.getState();
    expect(state.connection).toBe("connected");
    expect(state.cursor).toEqual({ after_epoch: 6, after_seq: 100 });
    expect(state.sessions.has("stale-session")).toBe(false);
    expect(state.sessions.has("fresh-session")).toBe(true);
    expect(state.openApprovals.has("tkt-1")).toBe(true);
  });

  it("reconnects with updated cursor after GoingAway (1001) and stops permanently on 4003 revoked", () => {
    const openedUris: string[] = [];
    const sockets: FakeSocket[] = [];
    const timers = new FakeTimers();
    const revokedReasons: string[] = [];
    const resolvedEvents: ApprovalResolvedEvent[] = [];
    const expiredSteering: string[] = [];

    const transport: ControlPlaneTransport = {
      async fetchJson() {
        throw new Error("unexpected rest");
      },
      openWebSocket(uri) {
        openedUris.push(uri);
        const s = new FakeSocket();
        sockets.push(s);
        return s;
      },
    };

    const client = new ControlPlaneClient({
      transport,
      timers,
      random: () => 0.5,
      callbacks: {
        onRevoked: (r) => revokedReasons.push(r),
        onApprovalResolved: (ev) => resolvedEvents.push(ev),
        onSteeringExpired: (info) => expiredSteering.push(info.request_id),
      },
    });

    client.connect();
    sockets[0].emitOpen();
    client.advanceCursor(3, 42);

    // Test queued steering -> expired transition.
    sockets[0].emitEvent({
      type: "steering_confirm_resp",
      resp: {
        request_id: "steer-1",
        session_key: "agy-1",
        status: "queued",
        route_used: "mailbox_queue",
        expires_at: 9999,
      },
    });
    expect(client.getState().steering.get("steer-1")?.status).toBe("queued");

    sockets[0].emitEvent({
      type: "steering_expired",
      request_id: "steer-1",
      session_key: "agy-1",
      expired_at: 10000,
    });
    expect(client.getState().steering.get("steer-1")?.status).toBe("expired");
    expect(expiredSteering).toEqual(["steer-1"]);

    // Test late approval decision not_applied surfacing.
    sockets[0].emitEvent({
      type: "approval_resolved",
      resolved: {
        ticket_id: "tkt-late",
        host_id: "host-1",
        outcome: "not_applied",
        reason: "user_returned_to_desk",
        resolved_at: 10001,
      },
    });
    expect(resolvedEvents).toHaveLength(1);
    expect(resolvedEvents[0].outcome).toBe("not_applied");

    // GoingAway (1001) schedules jittered reconnect carrying advanced cursor.
    sockets[0].emitClose(WS_CLOSE_GOING_AWAY, "relay_restart");
    expect(client.getState().connection).toBe("reconnecting");
    const delay = timers.flushNext();
    expect(delay).toBe(RECONNECT_MIN_MS);
    expect(openedUris[1]).toBe(
      buildTunnelRequestUri("client", { after_epoch: 3, after_seq: 42 }),
    );

    sockets[1].emitOpen();
    expect(client.getState().connection).toBe("connected");
    expect(client.getState().reconnectAttempt).toBe(0);

    // Close 4003 (revoked/downgraded) fails closed and schedules zero reconnects.
    sockets[1].emitClose(WS_CLOSE_REVOKED, "device_revoked");
    expect(client.getState().connection).toBe("revoked");
    expect(timers.pending.size).toBe(0);
    expect(revokedReasons).toEqual(["device_revoked"]);

    // Subsequent connect() calls are ignored once revoked.
    client.connect();
    expect(sockets).toHaveLength(2);
  });

  it("rejects SignedApprovalDecision carrying updatedInput or updatedPermissions (Opus P0-4)", () => {
    const ws = new FakeSocket();
    const client = new ControlPlaneClient({
      transport: {
        async fetchJson() {
          throw new Error("unused");
        },
        openWebSocket() {
          return ws;
        },
      },
    });
    client.connect();
    ws.emitOpen();

    const base: SignedApprovalDecision = {
      host_id: "host-1",
      ticket_id: "tkt-1",
      input_sha256: "b".repeat(64),
      decision: "allow_once",
      nonce: "n1",
      issued_at: 1000,
      expires_at: 1120,
      device_id: "mob-1",
      sig_p256_der_b64: "AAAA",
    };
    client.sendApprovalDecision(base);
    expect(ws.sent).toHaveLength(1);

    const poisoned = {
      ...base,
      updatedInput: { command: "rm -rf /" },
    } as unknown as SignedApprovalDecision;
    expect(() => client.sendApprovalDecision(poisoned)).toThrow(/Forbidden field/);

    const poisonedPerms = {
      ...base,
      updatedPermissions: ["allow_all"],
    } as unknown as SignedApprovalDecision;
    expect(() => client.sendApprovalDecision(poisonedPerms)).toThrow(/Forbidden field/);
  });

  it("validates SignedSteeringConfirm boundary fields before sending (Opus #23)", () => {
    const ws = new FakeSocket();
    const client = new ControlPlaneClient({
      transport: {
        async fetchJson() {
          throw new Error("unused");
        },
        openWebSocket() {
          return ws;
        },
      },
    });
    client.connect();
    ws.emitOpen();

    client.sendSteeringConfirm({
      request_id: "steer-1",
      session_key: "sess-1",
      expect_route: "peer_inbox",
      text: "run tests",
      signed: {
        host_id: "host-1",
        session_key: "sess-1",
        request_id: "steer-1",
        expect_route: "peer_inbox",
        text_sha256: "c".repeat(64),
        issued_at: 1000,
        expires_at: 1120,
        nonce: "n-1",
        device_id: "mob-1",
        sig_p256_der_b64: "AAAA",
      },
    });
    expect(ws.sent).toHaveLength(1);

    expect(() =>
      client.sendSteeringConfirm({
        request_id: "steer-2",
        session_key: "sess-1",
        expect_route: "peer_inbox",
        text: "run tests",
        signed: {
          host_id: "host-1",
          session_key: "sess-1",
          request_id: "steer-2",
          expect_route: "peer_inbox",
          text_sha256: "not-a-hex-digest",
          issued_at: 1000,
          expires_at: 1120,
          nonce: "n-2",
          device_id: "mob-1",
          sig_p256_der_b64: "AAAA",
        },
      }),
    ).toThrow(/text_sha256/);
  });
});
