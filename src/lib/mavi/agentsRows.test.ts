import { describe, expect, it } from "vitest";
import { maviAgentRows, maviHeadline } from "./agentsRows";
import type { ThreadRecord, ThreadStatus } from "./threads";
import type { MaviPhase } from "./types";

const record = (key: string, extra: Partial<ThreadRecord> = {}): ThreadRecord => ({
  key,
  sessionId: `s-${key}`,
  title: `Thread ${key}`,
  target: { kind: "local", root: "/code/app" },
  mode: "ask",
  model: null,
  conversationId: null,
  createdAt: 0,
  lastActivityAt: 0,
  seenAt: 0,
  movedToTerminalAt: null,
  ...extra,
});

const phases: Record<string, MaviPhase | null> = { a: "approval", b: "working", c: "ended", d: null, e: "idle" };
const statuses: Record<string, ThreadStatus> = { a: "needs_you", b: "working", c: "failed", d: "not_running", e: "idle" };
const records = [
  record("e", { lastActivityAt: 5 }),
  record("d", { conversationId: "x" }),
  record("c"),
  record("b"),
  record("a"),
  record("s", { target: { kind: "ssh", host: "gpu", dir: "~/x" } }),
];
const summaryOf = (r: ThreadRecord) => (phases[r.key] ? { phase: phases[r.key]!, touched: ["a.ts", "b.ts"] } : null);
const statusOf = (r: ThreadRecord) => statuses[r.key] ?? "not_running";
const same = (a: string, b: string) => a === b;

describe("maviAgentRows", () => {
  it("lists every thread, most urgent first, with presence and attention words", () => {
    const rows = maviAgentRows(records, summaryOf, statusOf, { filter: "all", scope: null, sameRepo: same });
    expect(rows.map((r) => r.key)).toEqual(["a", "b", "c", "e", "d", "s"]);
    expect(rows[0]).toMatchObject({ presence: "Waiting for you", attention: "Needs you", live: true, needsAction: true, edited: 2 });
    expect(rows.find((r) => r.key === "d")).toMatchObject({ presence: "Not running (can resume)", live: false, edited: null });
    expect(rows.find((r) => r.key === "s")).toMatchObject({ ssh: true, where: "gpu:~/x", root: null });
  });

  it("applies the window's filters the way the plane means them", () => {
    const keys = (filter: "attention" | "live" | "parallel") =>
      maviAgentRows(records, summaryOf, statusOf, { filter, scope: null, sameRepo: same }).map((r) => r.key);
    expect(keys("attention")).toEqual(["a", "c"]);
    expect(keys("live")).toEqual(["a", "b", "e"]);
    expect(keys("parallel")).toEqual([]);
  });

  it("keeps a repository scope, which an SSH thread never matches", () => {
    const scoped = maviAgentRows(records, summaryOf, statusOf, { filter: "all", scope: "/code/other", sameRepo: same });
    expect(scoped).toEqual([]);
    expect(maviAgentRows(records, summaryOf, statusOf, { filter: "all", scope: "/code/app", sameRepo: same }).some((r) => r.ssh)).toBe(false);
  });

  it("says a moved conversation is in a terminal tab", () => {
    const [row] = maviAgentRows([record("m", { movedToTerminalAt: 1 })], () => ({ phase: "ended", touched: [] }), () => "not_running", {
      filter: "all",
      scope: null,
      sameRepo: same,
    });
    expect(row!.presence).toBe("In a terminal tab");
  });
});

describe("maviHeadline", () => {
  it("counts threads, running ones and those waiting", () => {
    const rows = maviAgentRows(records, summaryOf, statusOf, { filter: "all", scope: null, sameRepo: same });
    expect(maviHeadline(rows)).toBe("6 Mavi threads, 3 running, 1 waiting for you");
    expect(maviHeadline([])).toBe("");
  });
});
