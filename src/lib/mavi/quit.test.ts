import { describe, expect, it } from "vitest";
import { maviQuitRisks } from "./quit";
import type { ThreadRecord } from "./threads";

const record = (key: string, extra: Partial<ThreadRecord> = {}): ThreadRecord => ({
  key,
  sessionId: `s-${key}`,
  title: `T ${key}`,
  target: { kind: "local", root: "/r" },
  mode: "ask",
  model: null,
  conversationId: null,
  createdAt: 0,
  lastActivityAt: 0,
  seenAt: 0,
  movedToTerminalAt: null,
  ...extra,
});

describe("maviQuitRisks", () => {
  it("names threads in the middle of a turn, not idle or ended ones", () => {
    const phases: Record<string, "working" | "approval" | "idle" | "ended"> = { a: "working", b: "approval", c: "idle", d: "ended" };
    const risks = maviQuitRisks(
      [record("a", { conversationId: "x" }), record("b", { target: { kind: "ssh", host: "h", dir: "~/d" } }), record("c"), record("d")],
      (r) => ({ phase: phases[r.key]! }),
    );
    expect(risks).toEqual([
      { label: "Mavi: T a", repoPath: "/r", resumable: true },
      { label: "Mavi: T b", repoPath: "h:~/d", resumable: false },
    ]);
  });
});
