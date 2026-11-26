import { describe, expect, it } from "vitest";
import {
  computeReconnectDelayMs,
  ControlPlaneClient,
  RECONNECT_MAX_MS,
  RECONNECT_MIN_MS,
  RELAY_CLOSE_REVOKED_OR_DOWNGRADED,
  type WebSocketLike,
} from "../src/client/ControlPlaneClient";
import type { ApprovalTicket, SessionSnapshot } from "../src/protocol/types";

function makeTicket(id: string): ApprovalTicket {
  return {
    ticket_id: id,
    host_id: "host-1",
    session_id: "cc-sess-1",
    dedup_key: `cc-sess-1:${"a".repeat(64)}`,
    harness: "claude",
    channel: "presence_gated_hook",
    tool_name: "Bash",
    tool_input: { command: "cargo test" },
    cwd: "/Users/dev/GitPulse",
    input_sha256: "a".repeat(64),
    summary: "Run cargo test",
    repo_label: "GitPulse",
    issued_at: 1_700_000_000,
    expires_at: 1_700_001_800,
  };
}

function makeAntigravityIdleSession(): SessionSnapshot {
  return {
    session_key: "agy:repo:1",
    repo_path: "/Users/dev/GitPulse",
    repo_label: "GitPulse",
    worktree_path: "/Users/dev/GitPulse",
    branch: "main",
    harness: "antigravity",
    title: "Antigravity IDE Session",
    presence: "idle",
    fully_idle: true,
    capabilities: {
      harness: "antigravity",
      approval_channel: "native_only",
      supports_remote_approval: false,
      supports_active_steering: true,
      supports_idle_wake: false,
      ungated: true,
      queued_steering_ttl_sec: 7200,
      pty_input_enabled: false,
    },
    active_subagents: [],
    pending_approval_ids: [],
    updated_at_ms: 1_700_000_000_000,
  };
}

function createHarness() {
  const sent: Array<Record<string, unknown>> = [];
  const urls: string[] = [];
  let activeSocket: WebSocketLike | null = null;
  let scheduledTimers: Array<{ fn: () => void; ms: number }> = [];

  const client = new ControlPlaneClient({
    relayWsBaseUrl: "wss://relay.gitpulse.dev/v1/tunnel/ws",
    deviceId: "ios-device-1",
    now: () => 1_700_000_000_000,
    random: () => 0.5,
    scheduleTimer: (fn, ms) => {
      scheduledTimers.push({ fn, ms });
      return scheduledTimers.length as unknown as ReturnType<typeof setTimeout>;
    },
    clearTimer: () => {
      scheduledTimers = [];
    },
    transport: {
      connect(url) {
        urls.push(url);
        const ws: WebSocketLike = {
          send(data) {
            sent.push(JSON.parse(data) as Record<string, unknown>);
          },
          close() {},
          onopen: null,
          onmessage: null,
          onclose: null,
          onerror: null,
        };
        activeSocket = ws;
        return ws;
      },
    },
  });

  return {
    client,
    sent,
    urls,
    getSocket: () => activeSocket!,
    getTimers: () => scheduledTimers,
  };
}

describe("ControlPlaneClient state machine", () => {
  it("handles cursor_gap by requesting snapshot_resync_req and replacing state on inbox_snapshot", () => {
    const { client, sent, getSocket } = createHarness();
    client.connect();
    getSocket().onopen?.();

    // Initial connect requests snapshot
    expect(sent.some((m) => m.type === "snapshot_resync_req")).toBe(true);
    sent.length = 0;

    // Seed a stale ticket
    client.applyEvent({
      type: "approval_created",
      host_epoch: 1,
      seq: 5,
      ticket: makeTicket("tkt-stale"),
    });
    expect(client.getState().openTickets.map((t) => t.ticket_id)).toEqual(["tkt-stale"]);

    // Relay reports cursor_gap
    client.applyEvent({
      type: "cursor_gap",
      current_epoch: 2,
      oldest_available_seq: 40,
      latest_seq: 55,
    });

    expect(client.getState().resyncing).toBe(true);
    expect(client.getState().hostEpoch).toBe(2);
    expect(sent).toContainEqual({
      type: "snapshot_resync_req",
      after_epoch: 2,
      after_seq: 5,
    });
    expect(client.getState().banners[0]?.kind).toBe("cursor_gap");

    // Authoritative host inbox_snapshot arrives with current open ticket
    client.applyEvent({
      type: "inbox_snapshot",
      host_epoch: 2,
      seq: 55,
      sessions: [makeAntigravityIdleSession()],
      open_tickets: [makeTicket("tkt-fresh")],
      probe_gaps: [],
    });

    expect(client.getState().resyncing).toBe(false);
    expect(client.getState().lastSeq).toBe(55);
    expect(client.getState().openTickets.map((t) => t.ticket_id)).toEqual(["tkt-fresh"]);
  });

  it("emits a not_applied banner when an approval decision arrives late (Opus P0-1)", () => {
    const { client } = createHarness();
    client.applyEvent({
      type: "approval_created",
      host_epoch: 1,
      seq: 10,
      ticket: makeTicket("tkt-late"),
    });

    client.applyEvent({
      type: "approval_resolved",
      host_epoch: 1,
      seq: 11,
      ticket_id: "tkt-late",
      host_id: "host-1",
      outcome: "not_applied",
      reason: "User returned to desktop keyboard; prompt released to native terminal.",
      resolved_at: 1_700_000_050,
    });

    expect(client.getState().openTickets).toHaveLength(0);
    const banner = client.getState().banners[0];
    expect(banner?.kind).toBe("not_applied");
    expect(banner?.ticket_id).toBe("tkt-late");
    expect(banner?.message).toContain("User returned to desktop");
  });

  it("supports 2-step preview -> confirm steering and surfaces steering_expired for idle Antigravity", async () => {
    const { client, sent, getSocket } = createHarness();
    client.connect();
    getSocket().onopen?.();
    sent.length = 0;

    client.applyEvent({
      type: "inbox_snapshot",
      host_epoch: 1,
      seq: 1,
      sessions: [makeAntigravityIdleSession()],
      open_tickets: [],
      probe_gaps: [],
    });

    const preview = await client.previewSteering({
      request_id: "req-agy-1",
      session_key: "agy:repo:1",
      text: "Check failing test",
    });
    expect(preview.expect_route).toBe("queued_next_turn");
    expect(preview.delivery_mode).toBe("queued_next_turn");
    expect(preview.queued_ttl_sec).toBe(7200);

    const confirm = await client.confirmSteering({
      request_id: preview.request_id,
      session_key: preview.session_key,
      expect_route: preview.expect_route,
      text: "Check failing test",
      signed: {
        host_id: "host-1",
        session_key: preview.session_key,
        request_id: preview.request_id,
        expect_route: preview.expect_route,
        text_sha256: "e".repeat(64),
        issued_at: 1_700_000_000,
        expires_at: 1_700_000_300,
        nonce: "nonce-agy-1",
        device_id: "ios-device-1",
        sig_p256_der_b64: "MEUCIQDagy==",
      },
    });
    expect(confirm.status).toBe("queued");
    expect(sent).toContainEqual(
      expect.objectContaining({
        type: "steering_confirm_req",
        request_id: "req-agy-1",
        expect_route: "queued_next_turn",
        signed: expect.objectContaining({
          text_sha256: "e".repeat(64),
          device_id: "ios-device-1",
        }),
      }),
    );

    // Rejects unsigned or mismatched steering confirm (Opus #23 P1)
    await expect(
      client.confirmSteering({
        request_id: "req-unsigned",
        session_key: "agy:repo:1",
        expect_route: "queued_next_turn",
        text: "Injected text",
        signed: undefined as unknown as Parameters<ControlPlaneClient["confirmSteering"]>[0]["signed"],
      }),
    ).rejects.toThrow(/Opus #23 P1/i);

    client.applyEvent({
      type: "steering_expired",
      host_epoch: 1,
      seq: 2,
      request_id: "req-agy-1",
      session_key: "agy:repo:1",
      harness: "antigravity",
      message: "Queued steering message expired after 2h TTL without a new turn.",
    });

    expect(client.getState().banners[0]?.kind).toBe("steering_expired");
    expect(client.getState().banners[0]?.session_key).toBe("agy:repo:1");
  });

  it("evicts state and stops reconnecting immediately on close code 4003", () => {
    const { client, getSocket, getTimers } = createHarness();
    client.connect();
    getSocket().onopen?.();
    client.applyEvent({
      type: "approval_created",
      host_epoch: 1,
      seq: 1,
      ticket: makeTicket("tkt-1"),
    });

    getSocket().onclose?.({ code: RELAY_CLOSE_REVOKED_OR_DOWNGRADED, reason: "device_revoked" });

    expect(client.getState().connectionState).toBe("revoked");
    expect(client.getState().openTickets).toHaveLength(0);
    expect(getTimers()).toHaveLength(0);
    expect(client.getState().banners[0]?.kind).toBe("revoked");
  });

  it("bounds jittered reconnect backoff strictly inside [250ms, 15000ms]", () => {
    for (let attempt = 0; attempt < 20; attempt++) {
      for (const r of [0, 0.25, 0.5, 0.99, 1]) {
        const delay = computeReconnectDelayMs(attempt, () => r);
        expect(delay).toBeGreaterThanOrEqual(RECONNECT_MIN_MS);
        expect(delay).toBeLessThanOrEqual(RECONNECT_MAX_MS);
      }
    }
  });
});
