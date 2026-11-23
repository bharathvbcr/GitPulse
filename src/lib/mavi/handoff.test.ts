import { describe, expect, it } from "vitest";
import { canContinueInTerminal, terminalHolding } from "./handoff";
import type { ThreadRecord } from "./threads";
import type { TerminalSessionRecord } from "../terminal/sessionRegistry";

const UUID = "0d235a3d-03cf-44a9-ad04-effb59321456";

const tab = (key: string, extra: Partial<TerminalSessionRecord> = {}): TerminalSessionRecord => ({
  key,
  repoPath: "/r",
  label: "Claude",
  launcher: "claude",
  status: "running",
  sessionId: `pty-${key}`,
  close: async () => {},
  ...extra,
});

const record = (extra: Partial<ThreadRecord> = {}): ThreadRecord => ({
  key: "k",
  sessionId: null,
  title: "t",
  target: { kind: "local", root: "/r" },
  mode: "ask",
  model: null,
  conversationId: UUID,
  createdAt: 0,
  lastActivityAt: 0,
  seenAt: 0,
  movedToTerminalAt: null,
  ...extra,
});

describe("terminalHolding", () => {
  it("finds the live Claude tab running a conversation, by its hosted record", () => {
    const hosted = new Map([["pty-b", { sessionId: UUID.toUpperCase() }]]);
    const tabs = [tab("a"), tab("b"), tab("c", { launcher: "shell", sessionId: "pty-c" })];
    expect(terminalHolding(UUID, tabs, (id) => hosted.get(id))?.key).toBe("b");
  });

  it("ignores tabs that have not spawned, shells, and other conversations", () => {
    const hosted = new Map([["pty-a", { sessionId: "11111111-1111-4111-8111-111111111111" }]]);
    const tabs = [tab("a"), tab("b", { sessionId: undefined })];
    expect(terminalHolding(UUID, tabs, (id) => hosted.get(id))).toBeNull();
  });
});

describe("canContinueInTerminal", () => {
  it("offers the move only for a local thread with a saved conversation that is not already moved", () => {
    expect(canContinueInTerminal(record())).toEqual({ ok: true });
    expect(canContinueInTerminal(record({ target: { kind: "ssh", host: "h", dir: "~" } })).ok).toBe(false);
    expect(canContinueInTerminal(record({ conversationId: null })).ok).toBe(false);
    expect(canContinueInTerminal(record({ movedToTerminalAt: 1 })).ok).toBe(false);
  });
});
