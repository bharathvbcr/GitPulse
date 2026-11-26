import { describe, expect, it, vi } from "vitest";
import { legacySessionLabel, legacyStopQuestion, legacyView, parseLegacyHosts, readLegacyHosts, readLegacyTranscript, stopLegacySession } from "./legacySessions";

const session = (patch: Record<string, unknown> = {}) => ({
  id: "term-1-a", program: "/usr/local/bin/claude", cwd: "/work", repo: "/work", launcher: "claude", run_id: null,
  title: "fix the flake", parent_session_id: null, detached: true, ending: false, quiet_ms: 12, ...patch,
});
const listed = (sessions: unknown[], protocol = 4) => ({
  protocol, socket: "/s", status: { state: "listed", info: { protocol, build: "1.0.0", pid: 7, sessions: sessions.length }, sessions },
});

describe("parseLegacyHosts", () => {
  it("reads a listed host's sessions", () => {
    const hosts = parseLegacyHosts([listed([session(), session({ id: "term-2-b", title: null, launcher: null, program: "/bin/zsh" })])]);
    expect(hosts).toHaveLength(1);
    expect(hosts?.[0]).toMatchObject({ state: "listed", protocol: 4, build: "1.0.0" });
    const rows = legacyView({ kind: "ok", hosts: hosts ?? [] }).rows;
    expect(rows.map((row) => row.key)).toEqual(["legacy:4:term-1-a", "legacy:4:term-2-b"]);
    expect(legacySessionLabel(rows[1].session)).toBe("zsh");
  });

  it("keys by protocol and id, so the same id on two hosts is two sessions", () => {
    const hosts = parseLegacyHosts([listed([session()], 4), listed([session()], 3)]) ?? [];
    const keys = legacyView({ kind: "ok", hosts }).rows.map((row) => row.key);
    expect(new Set(keys).size).toBe(2);
  });

  it("never reads a host that could not be listed as empty", () => {
    for (const status of [{ state: "no_compat" }, { state: "unreachable", reason: "no host" }, { state: "refused", reason: "another window" }, { state: "failed", reason: "slow" }]) {
      const hosts = parseLegacyHosts([{ protocol: 3, socket: "/s", status }]) ?? [];
      expect(hosts[0].state).toBe("unlisted");
      const view = legacyView({ kind: "ok", hosts });
      expect(view.rows).toEqual([]);
      expect(view.problems).toHaveLength(1);
      expect(view.problems[0]).toContain("couldn't be listed");
    }
  });

  it("makes a host with one malformed session unlisted rather than half-listed", () => {
    const hosts = parseLegacyHosts([listed([session(), { id: 5 }])]) ?? [];
    expect(hosts[0].state).toBe("unlisted");
  });

  it("is null for an answer that is not a list of reports", () => {
    expect(parseLegacyHosts({})).toBeNull();
    expect(parseLegacyHosts([null])).toBeNull();
    expect(parseLegacyHosts([{ protocol: "4", status: {} }])).toBeNull();
    expect(parseLegacyHosts([])).toEqual([]);
  });
});

describe("readLegacyHosts", () => {
  it("answers a failed question as failed, not as no hosts", async () => {
    const read = await readLegacyHosts((async () => { throw new Error("host busy"); }) as never);
    expect(read.kind).toBe("failed");
    expect(legacyView(read).problems[0]).toContain("couldn't be listed");
    expect(legacyView(read).rows).toEqual([]);
  });
  it("answers a malformed list as failed", async () => {
    expect((await readLegacyHosts((async () => ({ nope: 1 })) as never)).kind).toBe("failed");
  });
});

describe("stopping a legacy session", () => {
  const target = parseLegacyHosts([listed([session()])]) ?? [];
  const found = target[0].state === "listed" ? target[0].sessions[0] : (() => { throw new Error("fixture"); })();

  it("asks first, and a declined question sends nothing", async () => {
    const invoke = vi.fn(async () => null);
    const confirm = vi.fn(async () => false);
    expect(await stopLegacySession(4, found, { invoke: invoke as never, confirm })).toBe(false);
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ destructive: true, confirmLabel: "Stop" }));
    expect(invoke).not.toHaveBeenCalled();
    expect(legacyStopQuestion(found).message).toContain("previous Waylode version's terminal host");
  });

  it("stops through its own command with the protocol, never through cmd_terminal_kill", async () => {
    const invoke = vi.fn(async () => null);
    expect(await stopLegacySession(4, found, { invoke: invoke as never, confirm: async () => true })).toBe(true);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("cmd_terminal_legacy_kill", { protocol: 4, sessionId: "term-1-a" });
  });

  it("rejects when the host refuses, so the caller can say why", async () => {
    const invoke = vi.fn(async () => { throw new Error("refused"); });
    await expect(stopLegacySession(4, found, { invoke: invoke as never, confirm: async () => true })).rejects.toThrow("refused");
  });
});

describe("reading a legacy session's output", () => {
  it("asks the legacy command with the protocol and masking on, and parses like any saved transcript", async () => {
    const invoke = vi.fn(async () => ({ state: "missing" }));
    const read = await readLegacyTranscript(4, "term-1-a", invoke as never);
    expect(read).toEqual({ kind: "missing" });
    expect(invoke).toHaveBeenCalledWith("cmd_terminal_legacy_transcript", { protocol: 4, sessionId: "term-1-a", maxBytes: 131072, redact: true });
  });
  it("is failed, not missing, when the host cannot be asked", async () => {
    const invoke = vi.fn(async () => { throw new Error("refused"); });
    expect((await readLegacyTranscript(4, "term-1-a", invoke as never)).kind).toBe("failed");
  });
});
