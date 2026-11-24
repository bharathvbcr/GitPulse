import { describe, expect, it } from "vitest";
import { computeWordDiff, parseUnifiedDiffToFlatLines } from "../src/diff/mobileDiff";
import {
  deriveCapabilityBadges,
  parsePairingQrPayload,
  type SessionSnapshot,
} from "../src/protocol/types";

function makeSession(overrides: Partial<SessionSnapshot>): SessionSnapshot {
  return {
    session_key: "sess-default",
    repo_path: "/repo",
    repo_label: "GitPulse",
    worktree_path: "/repo",
    branch: "main",
    harness: "claude",
    title: "Session",
    presence: "running",
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
    updated_at_ms: 1_700_000_000_000,
    ...overrides,
  };
}

describe("Honest capability badges across harnesses", () => {
  it("labels Claude as Universal Presence-Gated", () => {
    const badges = deriveCapabilityBadges(makeSession({}));
    expect(badges.map((b) => b.label)).toEqual(["Universal Presence-Gated"]);
  });

  it("labels Antigravity honestly as Ungated / Native Only + Queued — Next Turn", () => {
    const badges = deriveCapabilityBadges(
      makeSession({
        harness: "antigravity",
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
      }),
    );
    expect(badges.map((b) => b.label)).toEqual([
      "Ungated / Native Only",
      "Queued — Next Turn",
    ]);
  });

  it("labels Codex Tier 1 as Codex App-Server and Manvi as Manvi Policy Authority", () => {
    const codexBadges = deriveCapabilityBadges(
      makeSession({
        harness: "codex",
        capabilities: {
          harness: "codex",
          approval_channel: "codex_app_server",
          supports_remote_approval: true,
          supports_active_steering: true,
          supports_idle_wake: true,
          ungated: false,
          pty_input_enabled: false,
        },
      }),
    );
    expect(codexBadges.map((b) => b.label)).toEqual(["Codex App-Server"]);

    const manviBadges = deriveCapabilityBadges(
      makeSession({
        harness: "manvi",
        capabilities: {
          harness: "manvi",
          approval_channel: "manvi_policy",
          supports_remote_approval: true,
          supports_active_steering: true,
          supports_idle_wake: true,
          ungated: false,
          pty_input_enabled: false,
        },
      }),
    );
    expect(manviBadges.map((b) => b.label)).toEqual(["Manvi Policy Authority"]);
  });
});

describe("Flat-span mobile diff model & QR pairing parser", () => {
  it("computes intra-line word diff segments and hunk line numbers", () => {
    const intra = computeWordDiff("const mode = 'testing';", "const mode = 'enforced';");
    expect(intra.original_segments.some((s) => s.kind === "Removed" && s.text === "testing")).toBe(
      true,
    );
    expect(intra.modified_segments.some((s) => s.kind === "Added" && s.text === "enforced")).toBe(
      true,
    );

    const lines = parseUnifiedDiffToFlatLines(
      "main.go",
      [
        "@@ -118,3 +118,3 @@",
        " func main() {",
        "-  paywall := \"testing\"",
        "+  paywall := \"enforced\"",
        " }",
      ].join("\n"),
    );

    expect(lines[0]?.type).toBe("hdr");
    expect(lines[2]?.type).toBe("del");
    expect(lines[2]?.oldNo).toBe(119);
    expect(lines[2]?.segments?.some((s) => s.kind === "Removed")).toBe(true);
    expect(lines[3]?.type).toBe("add");
    expect(lines[3]?.newNo).toBe(119);
    expect(lines[3]?.segments?.some((s) => s.kind === "Added")).toBe(true);
  });

  it("validates pairing QR payloads and rejects malformed input", () => {
    const valid = parsePairingQrPayload(
      JSON.stringify({
        v: 1,
        host_id: "mac-1",
        relay_url: "wss://relay.gitpulse.dev/v1/tunnel/ws",
        host_x25519_pub_b64: "pub==",
        pairing_token: "tok-1",
        safety_code: "482-910",
        expires_at_ms: 1_900_000_000_000,
      }),
    );
    expect(valid.safety_code).toBe("482-910");

    expect(() => parsePairingQrPayload("not-json")).toThrow(/not valid JSON/i);
    expect(() => parsePairingQrPayload(JSON.stringify({ v: 2 }))).toThrow(/Unsupported/i);
  });
});
