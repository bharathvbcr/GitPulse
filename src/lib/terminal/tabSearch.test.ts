import { describe, expect, it } from "vitest";
import {
  crowdedTabStrip,
  fieldSearchRank,
  filterLaunchers,
  filterSessionRecords,
  filterTerminalTabs,
  matchesTerminalSearch,
  normalizeTerminalSearchQuery,
  stepSearchIndex,
  TERMINAL_SEARCH_QUERY_LIMIT,
  terminalSearchRank,
} from "./tabSearch";
import { createTab, LAUNCHERS, openTab, initialState, type TerminalTab } from "./tabs";
import type { TerminalSessionRecord } from "./sessionRegistry";

function manyTabs(count: number): TerminalTab[] {
  let state = initialState();
  while (state.tabs.length < count) state = openTab(state, "shell");
  return [...state.tabs];
}

describe("normalizeTerminalSearchQuery", () => {
  it("trims, strips control characters, and bounds length", () => {
    expect(normalizeTerminalSearchQuery("  Claude\u0000\n  ")).toBe("Claude");
    expect(normalizeTerminalSearchQuery("x".repeat(10_000))).toHaveLength(TERMINAL_SEARCH_QUERY_LIMIT);
    expect(normalizeTerminalSearchQuery("\x1b[31mred")).toBe("[31mred");
  });

  it("fails closed on hostile input instead of throwing", () => {
    expect(normalizeTerminalSearchQuery(null)).toBe("");
    expect(normalizeTerminalSearchQuery(undefined)).toBe("");
    expect(normalizeTerminalSearchQuery(12)).toBe("");
    expect(normalizeTerminalSearchQuery({ q: "shell" })).toBe("");
    expect(normalizeTerminalSearchQuery("ok", Number.NaN)).toBe("");
    expect(normalizeTerminalSearchQuery("ok", 0)).toBe("");
  });
});

describe("matchesTerminalSearch", () => {
  it("matches every row when the query is empty after normalisation", () => {
    expect(matchesTerminalSearch("", "Shell")).toBe(true);
    expect(matchesTerminalSearch("   ", "Shell")).toBe(true);
    expect(matchesTerminalSearch("\u0000", "Shell")).toBe(true);
  });

  it("is case-insensitive and accepts substring or subsequence matches", () => {
    expect(matchesTerminalSearch("build", "Build logs")).toBe(true);
    expect(matchesTerminalSearch("BLD", "Build logs")).toBe(true);
    expect(matchesTerminalSearch("世界", "repo 世界 café")).toBe(true);
    expect(matchesTerminalSearch("missing", "Build logs")).toBe(false);
  });

  it("matches multi-token queries across separate fields regardless of word order", () => {
    expect(matchesTerminalSearch("devcouncil claude", "DevCouncil", "Claude")).toBe(true);
    expect(matchesTerminalSearch("claude devcouncil", "DevCouncil", "Claude")).toBe(true);
    expect(matchesTerminalSearch("gitpulse sidebar claude", "GitPulse", "fix-sidebar", "Claude")).toBe(true);
    expect(matchesTerminalSearch("devcouncil codex", "DevCouncil", "Claude")).toBe(false);
    expect(matchesTerminalSearch("devcouncil claude", "", null, undefined)).toBe(false);
  });

  it("ranks exact, prefix, word/initials, substring, and subsequence cleanly", () => {
    expect(fieldSearchRank("claude", "Claude")).toBe(5);
    expect(fieldSearchRank("cl", "Claude")).toBe(4);
    expect(fieldSearchRank("dc", "DevCouncil")).toBe(3);
    expect(fieldSearchRank("gp", "GitPulse")).toBe(3);
    expect(fieldSearchRank("sidebar", "fix-sidebar")).toBe(3);
    expect(fieldSearchRank("bar", "xbar-bar")).toBe(3);
    expect(fieldSearchRank("ul", "GitPulse")).toBe(2);
    expect(fieldSearchRank("cl", "DevCouncil")).toBe(1);
    expect(fieldSearchRank("zz", "DevCouncil")).toBe(0);
    expect(terminalSearchRank("cl", "Claude")).toBeGreaterThan(terminalSearchRank("cl", "DevCouncil"));
    expect(terminalSearchRank("dc cl", "DevCouncil", "Claude")).toBe(3);
  });
});

describe("filterTerminalTabs", () => {
  it("returns a copy of every tab for an empty query", () => {
    const tabs = manyTabs(8);
    const found = filterTerminalTabs(tabs, "");
    expect(found).toHaveLength(8);
    expect(found).not.toBe(tabs);
  });

  it("finds one renamed tab among a crowded strip", () => {
    const tabs = manyTabs(24);
    tabs[17] = { ...tabs[17], name: "Build logs" };
    const found = filterTerminalTabs(tabs, "build log");
    expect(found).toHaveLength(1);
    expect(found[0].id).toBe(tabs[17].id);
  });

  it("matches launcher kind, status, launchName, live OSC title, agentDetail, and unread independently", () => {
    const claude = {
      ...createTab("claude"),
      name: "Preserve E42",
      launchName: "Preserve E42",
      title: "✳ Running cargo test",
    };
    const shell = createTab("shell");
    expect(filterTerminalTabs([claude, shell], "claude").map((tab) => tab.id)).toEqual([claude.id]);
    expect(filterTerminalTabs([claude, shell], "Preserve").map((tab) => tab.id)).toEqual([claude.id]);
    expect(filterTerminalTabs([claude, shell], "cargo").map((tab) => tab.id)).toEqual([claude.id]);
    expect(
      filterTerminalTabs([claude, shell], "Auditing splitTree", {
        [shell.id]: { agentDetail: "Auditing splitTree.ts" },
      }).map((tab) => tab.id),
    ).toEqual([shell.id]);
    expect(
      filterTerminalTabs([claude, shell], "unread", { [shell.id]: { unread: true } }).map(
        (tab) => tab.id,
      ),
    ).toEqual([shell.id]);
    expect(
      filterTerminalTabs([claude, shell], "error", { [claude.id]: { status: "error" } }).map(
        (tab) => tab.id,
      ),
    ).toEqual([claude.id]);
    expect(
      filterTerminalTabs([claude, shell], "Pair 2", { [claude.id]: { groupLabel: "Pair 2" } }).map(
        (tab) => tab.id,
      ),
    ).toEqual([claude.id]);
  });

  it("returns an empty list for no match and refuses a non-array", () => {
    expect(filterTerminalTabs([createTab("shell")], "zzzz")).toEqual([]);
    expect(filterTerminalTabs(null as never, "shell")).toEqual([]);
  });
});

describe("filterLaunchers / filterSessionRecords", () => {
  it("filters launchers by kind or label", () => {
    expect(filterLaunchers(LAUNCHERS, "shell").map((row) => row.kind)).toEqual(["shell"]);
    expect(filterLaunchers(LAUNCHERS, "zzzz")).toEqual([]);
    expect(filterLaunchers(null as never, "shell")).toEqual([]);
    for (const launcher of LAUNCHERS) {
      expect(filterLaunchers(LAUNCHERS, launcher.label)).toContainEqual(launcher);
    }
  });

  it("filters global sessions by repository, label, and status", () => {
    const sessions: TerminalSessionRecord[] = [
      { key: "a", repoPath: "/repos/GitPulse", label: "Shell", status: "ready", close: async () => {} },
      { key: "b", repoPath: "/repos/Other", label: "Claude", status: "error", close: async () => {} },
    ];
    expect(filterSessionRecords(sessions, "other").map((row) => row.key)).toEqual(["b"]);
    expect(filterSessionRecords(sessions, "error").map((row) => row.key)).toEqual(["b"]);
    expect(filterSessionRecords(sessions, "GitPulse").map((row) => row.key)).toEqual(["a"]);
    expect(filterSessionRecords(sessions, "nope")).toEqual([]);
  });

  it("finds a session by the task it is working on, not only by \"Claude\"", () => {
    const sessions: TerminalSessionRecord[] = [
      { key: "a", repoPath: "/repos/GitPulse", label: "Claude", title: "Fix the importer", taskRunId: "run-1", status: "running", close: async () => {} },
      { key: "b", repoPath: "/repos/GitPulse", label: "Claude", status: "running", close: async () => {} },
    ];
    expect(filterSessionRecords(sessions, "importer").map((row) => row.key)).toEqual(["a"]);
    expect(filterSessionRecords(sessions, "claude").map((row) => row.key)).toEqual(["a", "b"]);
  });
});

describe("stepSearchIndex / crowdedTabStrip", () => {
  it("wraps at both ends and fails closed on an empty list", () => {
    expect(stepSearchIndex(0, 0, 1)).toBe(-1);
    expect(stepSearchIndex(3, 2, 1)).toBe(0);
    expect(stepSearchIndex(3, 0, -1)).toBe(2);
    expect(stepSearchIndex(3, Number.NaN, 1)).toBe(0);
    expect(stepSearchIndex(3, -1, -1)).toBe(2);
  });

  it("treats five or more tabs as a crowded strip", () => {
    expect(crowdedTabStrip(4)).toBe(false);
    expect(crowdedTabStrip(5)).toBe(true);
    expect(crowdedTabStrip(32)).toBe(true);
    expect(crowdedTabStrip(Number.NaN)).toBe(false);
  });
});
