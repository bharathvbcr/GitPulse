import { describe, expect, it } from "vitest";
import {
  collectDockAttentionAndDone,
  dockRowSecondary,
  filterDockAgents,
  filterDockAgentsByStatus,
  groupDockAgentsByRepo,
  hasDockAttentionOrDone,
  isDockAgentRunning,
  sortDockTriageRows,
  type DockAgentRow,
} from "./dockAgents";

const baseHere: DockAgentRow = {
  id: "tab-1",
  launcher: "claude",
  title: "Fix auth flow",
  detail: "feat/login",
  repoPath: "/work/gitpulse",
  repoName: "GitPulse",
  checkout: "feat/login",
  status: { label: "Running", cls: "text-accent" },
  running: true,
};

const baseElse: DockAgentRow = {
  id: "reg-1",
  launcher: "codex",
  title: "Update docs",
  detail: "Alpha · main",
  repoPath: "/work/alpha",
  repoName: "Alpha",
  checkout: "main",
  status: { label: "Idle", cls: "text-textMuted" },
  running: false,
};

describe("isDockAgentRunning", () => {
  it("respects explicit running flag and falls back to status label when not attention or done", () => {
    expect(isDockAgentRunning(baseHere)).toBe(true);
    expect(isDockAgentRunning(baseElse)).toBe(false);
    expect(isDockAgentRunning({ id: "t", launcher: "claude", title: "Work", status: { label: "Working", cls: "text-accent" } })).toBe(true);
    expect(isDockAgentRunning({ id: "t", launcher: "claude", title: "Start", status: { label: "Starting", cls: "text-accent" } })).toBe(true);
    expect(isDockAgentRunning({ ...baseHere, attention: "Waiting" })).toBe(false);
    expect(isDockAgentRunning({ ...baseHere, done: true })).toBe(false);
    expect(isDockAgentRunning({ id: "t", launcher: "claude", title: "Idle", running: false, status: { label: "Working", cls: "text-accent" } })).toBe(false);
  });
});

describe("filterDockAgents", () => {
  const rows: DockAgentRow[] = [
    baseHere,
    {
      ...baseElse,
      launcher: "agy",
      attention: "Waiting",
      attentionDetail: "Confirm migration script",
    },
    {
      id: "reg-2",
      launcher: "manvi",
      title: "Refactor parser",
      detail: "Beta · fix/ast",
      repoPath: "/work/beta",
      repoName: "Beta",
      checkout: "fix/ast",
      status: { label: "Done", cls: "text-ok" },
      done: true,
    },
  ];

  it("returns all rows for empty or whitespace queries", () => {
    expect(filterDockAgents(rows, "   ")).toHaveLength(3);
  });

  it("matches multi-word queries across title, detail, repoName, checkout, launcher, attention, attentionDetail, and status", () => {
    expect(filterDockAgents(rows, "auth gitpulse").map((r) => r.id)).toEqual(["tab-1"]);
    expect(filterDockAgents(rows, "alpha migration").map((r) => r.id)).toEqual(["reg-1"]);
    expect(filterDockAgents(rows, "antigravity waiting").map((r) => r.id)).toEqual(["reg-1"]);
    expect(filterDockAgents(rows, "fix/ast done").map((r) => r.id)).toEqual(["reg-2"]);
    expect(filterDockAgents(rows, "nonexistent")).toEqual([]);
  });
});

describe("filterDockAgentsByStatus", () => {
  const rows: DockAgentRow[] = [
    baseHere,
    { ...baseElse, id: "reg-att", attention: "Failed" },
    { ...baseElse, id: "reg-done", done: true },
    { ...baseElse, id: "reg-idle" },
  ];

  it("filters by all, running, attention, and done", () => {
    expect(filterDockAgentsByStatus(rows, "all")).toHaveLength(4);
    expect(filterDockAgentsByStatus(rows, "running").map((r) => r.id)).toEqual(["tab-1"]);
    expect(filterDockAgentsByStatus(rows, "attention").map((r) => r.id)).toEqual(["reg-att"]);
    expect(filterDockAgentsByStatus(rows, "done").map((r) => r.id)).toEqual(["reg-done"]);
  });
});

describe("dockRowSecondary", () => {
  it("prefers checkout and strips redundant repoName prefixes when grouped", () => {
    expect(dockRowSecondary(baseHere, true)).toBe("feat/login");
    expect(
      dockRowSecondary({ ...baseElse, checkout: null, detail: "Alpha · feat/wt", repoName: "Alpha" }, true),
    ).toBe("feat/wt");
    expect(
      dockRowSecondary({ ...baseElse, checkout: null, detail: "Alpha/wt-2", repoName: "Alpha" }, true),
    ).toBe("wt-2");
    expect(
      dockRowSecondary({ ...baseElse, checkout: null, detail: "Alpha", repoName: "Alpha" }, true),
    ).toBeNull();
    expect(
      dockRowSecondary({ ...baseElse, checkout: null, detail: "Alpha · main", repoName: "Alpha" }, false),
    ).toBe("Alpha · main");
  });
});

describe("collectDockAttentionAndDone", () => {
  it("collects attention and done items preserving isHere and sorting urgency-first then recency", () => {
    const olderDone: DockAgentRow = { id: "d-old", launcher: "claude", title: "Old done", done: true, lastActivityAt: 100 };
    const newerDone: DockAgentRow = { id: "d-new", launcher: "codex", title: "New done", done: true, lastActivityAt: 300 };
    const warnRow: DockAgentRow = { id: "w-1", launcher: "claude", title: "Waiting", attention: "Waiting", lastActivityAt: 200 };
    const failRow: DockAgentRow = { id: "f-1", launcher: "codex", title: "Failed", attention: "Failed", lastActivityAt: 50 };
    const cleanRow: DockAgentRow = { ...baseHere, id: "c-1" };

    const items = collectDockAttentionAndDone([olderDone, cleanRow, warnRow], [failRow, newerDone]);
    expect(items.map((item) => ({ id: item.row.id, isHere: item.isHere }))).toEqual([
      { id: "f-1", isHere: false },
      { id: "w-1", isHere: true },
      { id: "d-new", isHere: false },
      { id: "d-old", isHere: true },
    ]);
  });
});

describe("hasDockAttentionOrDone", () => {
  it("returns false when all agents are running or idle", () => {
    expect(hasDockAttentionOrDone([baseHere], [baseElse])).toBe(false);
  });

  it("returns true when any row has attention or done", () => {
    expect(hasDockAttentionOrDone([{ ...baseHere, attention: "Waiting" }], [baseElse])).toBe(true);
    expect(hasDockAttentionOrDone([baseHere], [{ ...baseElse, done: true }])).toBe(true);
  });
});

describe("sortDockTriageRows", () => {
  it("orders failed -> attention -> done -> running -> idle while preserving tier order", () => {
    const idle: DockAgentRow = { id: "idle", launcher: "claude", title: "Idle" };
    const running: DockAgentRow = { id: "run", launcher: "claude", title: "Run", running: true };
    const done: DockAgentRow = { id: "done", launcher: "claude", title: "Done", done: true };
    const warn: DockAgentRow = { id: "warn", launcher: "claude", title: "Warn", attention: "Waiting" };
    const fail: DockAgentRow = { id: "fail", launcher: "claude", title: "Fail", attention: "Failed" };
    expect(sortDockTriageRows([idle, running, done, warn, fail]).map((r) => r.id)).toEqual([
      "fail",
      "warn",
      "done",
      "run",
      "idle",
    ]);
  });
});

describe("groupDockAgentsByRepo", () => {
  it("places current repo first and sorts other repos alphabetically with accurate counts", () => {
    const zebra: DockAgentRow = {
      id: "z-1",
      launcher: "claude",
      title: "Zebra task",
      repoPath: "/work/zebra",
      repoName: "Zebra",
      running: true,
    };
    const alpha1: DockAgentRow = {
      ...baseElse,
      id: "a-1",
      running: true,
    };
    const alpha2: DockAgentRow = {
      ...baseElse,
      id: "a-2",
      repoPath: "/work/alpha/",
      done: true,
      running: false,
    };
    const hereAtt: DockAgentRow = {
      ...baseHere,
      id: "tab-2",
      running: false,
      attention: "Needs input",
    };

    const groups = groupDockAgentsByRepo(
      [baseHere, hereAtt],
      [zebra, alpha1, alpha2],
      "/work/gitpulse",
      { pathOptions: { caseInsensitive: false }, collapsedKeys: new Set(["path:/work/alpha"]) },
    );

    expect(groups.map((g) => g.repoName)).toEqual(["GitPulse", "Alpha", "Zebra"]);
    expect(groups[0]).toMatchObject({
      isCurrent: true,
      runningCount: 1,
      attentionCount: 1,
      doneCount: 0,
      collapsed: false,
    });
    expect(groups[1]).toMatchObject({
      isCurrent: false,
      runningCount: 1,
      attentionCount: 0,
      doneCount: 1,
      collapsed: true,
    });
    expect(groups[1].items).toHaveLength(2);
    expect(groups[2]).toMatchObject({
      isCurrent: false,
      runningCount: 1,
      collapsed: false,
    });
  });

  it("handles case-insensitive path matching and legacy rows without repoPath", () => {
    const groups = groupDockAgentsByRepo(
      [{ id: "h-1", launcher: "claude", title: "Local" }],
      [{ id: "e-1", launcher: "codex", title: "Remote", repoPath: "/Work/Beta" }, { id: "e-2", launcher: "codex", title: "Remote 2", repoPath: "/work/beta/" }],
      "/work/main",
      { pathOptions: { caseInsensitive: true } },
    );
    expect(groups).toHaveLength(2);
    expect(groups[0].isCurrent).toBe(true);
    expect(groups[0].repoName).toBe("main");
    expect(groups[1].items).toHaveLength(2);
  });
});
