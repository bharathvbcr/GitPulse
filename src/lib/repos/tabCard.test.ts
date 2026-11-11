import { describe, expect, it } from "vitest";
import { tabCardRows, type TabCardInput } from "./tabCard";

const QUIET = { state: "none", live: 0, reason: null, tone: null, phrase: "", why: "" } as const;

const input = (over: Partial<TabCardInput> = {}): TabCardInput => ({
  tab: { path: "/code/app", currentBranch: "main", changedCount: 0, conflictedCount: 0, countsKnown: true, isDirty: false },
  sync: { ahead: 0, behind: 0 },
  fetchedAt: 1000,
  terminals: 0,
  agents: QUIET,
  nowSec: 1000 + 7200,
  ...over,
});

const value = (rows: ReturnType<typeof tabCardRows>, label: string) => rows.find((row) => row.label === label);

describe("tabCardRows", () => {
  it("names path, branch with sync, uncommitted, agents, terminals and last fetch", () => {
    const rows = tabCardRows(input({ sync: { ahead: 2, behind: 1 }, terminals: 2 }));
    expect(rows.map((row) => row.label)).toEqual(["Path", "Branch", "Uncommitted", "Agents", "Terminals", "Last fetch"]);
    expect(value(rows, "Branch")?.value).toBe("main · 2 ahead, 1 behind");
    expect(value(rows, "Terminals")?.value).toBe("2 sessions");
    expect(value(rows, "Last fetch")?.value).toBe("2h ago");
  });

  it("says Not read, never zero or in sync, for a repository whose sync was never loaded", () => {
    const rows = tabCardRows(input({ sync: null, fetchedAt: undefined }));
    expect(value(rows, "Branch")).toMatchObject({ value: "main · Not read yet", unknown: true });
    expect(value(rows, "Last fetch")).toMatchObject({ value: "Not read yet", unknown: true });
  });

  it("does not turn an unread uncommitted count into None", () => {
    const rows = tabCardRows(input({ tab: { ...input().tab, countsKnown: false } }));
    expect(value(rows, "Uncommitted")).toMatchObject({ value: "Not read yet", unknown: true });
  });

  it("counts files and conflicts", () => {
    const rows = tabCardRows(input({ tab: { ...input().tab, changedCount: 3, conflictedCount: 1, isDirty: true } }));
    expect(value(rows, "Uncommitted")?.value).toBe("3 files, 1 conflict");
  });

  it("reports agents by the pill's own state and keeps unknown unknown", () => {
    const attention = tabCardRows(input({ agents: { state: "attention", live: 1, reason: "needs-you", tone: "warn", phrase: "Needs you", why: "" } }));
    expect(value(attention, "Agents")?.value).toBe("Needs you · 1 running");
    const unknown = tabCardRows(input({ agents: { state: "unknown", live: 0, reason: null, tone: null, phrase: "Not read", why: "never read" } }));
    expect(value(unknown, "Agents")).toMatchObject({ value: "Not read yet", unknown: true });
    expect(value(tabCardRows(input()), "Agents")?.value).toBe("None");
  });

  it("distinguishes a recorded fetch from none recorded", () => {
    expect(value(tabCardRows(input({ fetchedAt: null })), "Last fetch")?.value).toBe("No fetch recorded");
  });
});
