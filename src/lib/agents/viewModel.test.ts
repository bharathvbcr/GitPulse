import { describe, expect, it } from "vitest";
import type { AgentRow } from "./plane";
import type { MaviAgentRow } from "../mavi/agentsRows";
import {
  attentionTrayHeading,
  collectAttentionAndDoneRows,
  countAgentFilters,
  groupAgentTreeByRepo,
  hasAttentionOrDone,
  isAgentGroupMode,
  isRowActivelyRunning,
  isRowDone,
  searchAgentTree,
  searchMaviRows,
} from "./viewModel";

function makeRow(over: Partial<AgentRow> = {}): AgentRow {
  return {
    id: "row-1",
    parentId: null,
    children: [],
    rollup: { descendants: 0, live: 0, attention: [], unobserved: 0 },
    attentionObserved: true,
    repoPath: "/code/alpha",
    repoLabel: "alpha",
    kind: "claude",
    launcher: "claude",
    session: "feat-search",
    checkout: "agent/feat-search",
    worktreePath: "/code/alpha/.claude/worktrees/feat-search",
    checkoutPath: "/code/alpha/.claude/worktrees/feat-search",
    origin: "checkout",
    presence: "on-disk",
    presenceDetail: "On disk",
    attention: [],
    attentionLabel: "",
    attentionUrgency: null,
    attentionEvent: null,
    attentionChannel: null,
    attentionDetail: null,
    attentionAt: null,
    sessionId: null,
    fullyIdle: false,
    providerSessionId: null,
    parallelCount: 1,
    parallelFloor: false,
    dirtyFiles: 0,
    dirtyKnown: true,
    liveKey: null,
    taskRunId: null,
    outsideSessions: 0,
    ...over,
  };
}

function makeMavi(over: Partial<MaviAgentRow> = {}): MaviAgentRow {
  return {
    key: "m-1",
    title: "Refactor indexer",
    where: "alpha",
    root: "/code/alpha",
    ssh: false,
    live: true,
    presence: "Working",
    status: "working",
    attention: "",
    needsAction: false,
    edited: 2,
    lastActivityAt: 100,
    ...over,
  };
}

describe("isAgentGroupMode", () => {
  it("accepts auto, repo, and flat only", () => {
    expect(isAgentGroupMode("auto")).toBe(true);
    expect(isAgentGroupMode("repo")).toBe(true);
    expect(isAgentGroupMode("flat")).toBe(true);
    expect(isAgentGroupMode("grouped")).toBe(false);
    expect(isAgentGroupMode(null)).toBe(false);
  });
});

describe("searchAgentTree", () => {
  const child = makeRow({
    id: "row-child",
    parentId: "row-parent",
    origin: "session",
    kind: "codex",
    launcher: "codex",
    session: "Child session · pid 901",
    worktreePath: "/code/alpha/.claude/worktrees/parent/sub",
    attentionObserved: false,
  });
  const parent = makeRow({
    id: "row-parent",
    session: "parent-work",
    repoLabel: "AlphaRepo",
    checkout: "agent/parent",
    kind: "claude",
    launcher: "claude",
    attentionLabel: "Needs your permission",
    attentionDetail: "Allow cargo test",
    children: [child],
  });
  const other = makeRow({
    id: "row-other",
    repoPath: "/code/beta",
    repoLabel: "BetaRepo",
    session: "lint-fix",
    checkout: "fix/design-lint",
    kind: "agy",
    launcher: "agy",
    worktreePath: "/code/beta/.agy/worktrees/lint-fix",
  });

  it("returns all roots for empty or whitespace query", () => {
    expect(searchAgentTree([parent, other], "")).toEqual([parent, other]);
    expect(searchAgentTree([parent, other], "   ")).toEqual([parent, other]);
  });

  it("performs case-insensitive multi-word matching across all searchable fields", () => {
    expect(searchAgentTree([parent, other], "ALPHAREPO cargo")).toEqual([parent]);
    expect(searchAgentTree([parent, other], "fix/design-lint agy")).toEqual([other]);
    expect(searchAgentTree([parent, other], "antigravity lint")).toEqual([other]);
    expect(searchAgentTree([parent, other], "permission parent-work")).toEqual([parent]);
    expect(searchAgentTree([parent, other], "/code/beta/.agy")).toEqual([other]);
    expect(searchAgentTree([parent, other], "nonexistent token")).toEqual([]);
  });

  it("keeps a parent when any descendant matches", () => {
    expect(searchAgentTree([parent, other], "codex 901")).toEqual([parent]);
    expect(searchAgentTree([parent, other], "parent-work codex")).toEqual([parent]);
  });
});

describe("searchMaviRows", () => {
  const rows = [
    makeMavi({ key: "m1", title: "Fix auth bug", where: "GitPulse", presence: "Working", status: "working" }),
    makeMavi({ key: "m2", title: "Check deploy", where: "gpu:~/app", root: null, ssh: true, presence: "Waiting for you", attention: "Needs you", status: "needs_you", needsAction: true }),
  ];

  it("filters Mavi rows by multi-word case-insensitive match", () => {
    expect(searchMaviRows(rows, "")).toEqual(rows);
    expect(searchMaviRows(rows, "gitpulse auth")).toEqual([rows[0]]);
    expect(searchMaviRows(rows, "gpu:~/app needs")).toEqual([rows[1]]);
    expect(searchMaviRows(rows, "unknown-query")).toEqual([]);
  });
});

describe("hasAttentionOrDone", () => {
  it("returns false for quiet, dirty, and unread-only rows", () => {
    const clean = makeRow({ presence: "live" });
    const dirty = makeRow({ id: "r2", attention: ["dirty"], attentionLabel: "Uncommitted changes" });
    const unread = makeRow({ id: "r3", attention: ["unmeasured"], attentionLabel: "Changes not read" });
    expect(hasAttentionOrDone([clean, dirty, unread], [makeMavi()])).toBe(false);
  });

  it("returns true for any blocking, error, done, idle-waiting, or notice row or descendant", () => {
    expect(hasAttentionOrDone([makeRow({ attention: ["needs-you"], attentionUrgency: "blocking" })])).toBe(true);
    expect(hasAttentionOrDone([makeRow({ attention: ["error"], attentionUrgency: "error" })])).toBe(true);
    expect(hasAttentionOrDone([makeRow({ attention: ["blocked"] })])).toBe(true);
    expect(hasAttentionOrDone([makeRow({ attention: ["collision"] })])).toBe(true);
    expect(hasAttentionOrDone([makeRow({ attention: ["disconnected"] })])).toBe(true);
    expect(hasAttentionOrDone([makeRow({ attention: ["unstarted"] })])).toBe(true);
    expect(hasAttentionOrDone([makeRow({ attention: ["pending"] })])).toBe(true);
    expect(hasAttentionOrDone([makeRow({ attention: ["signalled"] })])).toBe(true);
    expect(hasAttentionOrDone([makeRow({ attention: ["finished"] })])).toBe(true);
    expect(hasAttentionOrDone([makeRow({ attention: ["idle-waiting"] })])).toBe(true);
    expect(hasAttentionOrDone([makeRow({ attention: ["notice"] })])).toBe(true);
    const parentWithAskingChild = makeRow({
      children: [makeRow({ id: "c1", parentId: "row-1", attention: ["finished"] })],
    });
    expect(hasAttentionOrDone([parentWithAskingChild])).toBe(true);
  });

  it("returns true when a Mavi row needs action or is ready for review", () => {
    expect(hasAttentionOrDone([], [makeMavi({ needsAction: true, status: "needs_you" })])).toBe(true);
    expect(hasAttentionOrDone([], [makeMavi({ needsAction: false, status: "review" })])).toBe(true);
  });
});

describe("isRowActivelyRunning and isRowDone", () => {
  it("identifies actively running live rows and excludes idle, blocked, error, and done rows", () => {
    expect(isRowActivelyRunning(makeRow({ presence: "live" }))).toBe(true);
    expect(isRowActivelyRunning(makeRow({ presence: "live", attention: ["dirty"] }))).toBe(true);
    expect(isRowActivelyRunning(makeRow({ presence: "live", fullyIdle: true }))).toBe(false);
    expect(isRowActivelyRunning(makeRow({ presence: "on-disk" }))).toBe(false);
    expect(isRowActivelyRunning(makeRow({ presence: "live", attention: ["needs-you"], attentionUrgency: "blocking" }))).toBe(false);
    expect(isRowActivelyRunning(makeRow({ presence: "live", attention: ["error"], attentionUrgency: "error" }))).toBe(false);
    expect(isRowActivelyRunning(makeRow({ presence: "live", attention: ["finished"] }))).toBe(false);
    expect(isRowActivelyRunning(makeRow({ presence: "live", attention: ["idle-waiting"] }))).toBe(false);
  });

  it("identifies done rows when finished or idle-waiting", () => {
    expect(isRowDone(makeRow({ attention: ["finished"] }))).toBe(true);
    expect(isRowDone(makeRow({ attention: ["idle-waiting"] }))).toBe(true);
    expect(isRowDone(makeRow({ attention: ["needs-you"] }))).toBe(false);
    expect(isRowDone(makeRow({ presence: "live" }))).toBe(false);
  });
});

describe("groupAgentTreeByRepo", () => {
  it("groups by normalized repository key, counts total, running, attention, and done, and sorts alphabetically", () => {
    const roots = [
      makeRow({ id: "z1", repoPath: "/code/Zebra", repoLabel: "Zebra", presence: "on-disk", attention: ["needs-you"], attentionUrgency: "blocking" }),
      makeRow({ id: "a1", repoPath: "/code/Alpha/", repoLabel: "Alpha", presence: "live" }),
      makeRow({ id: "a2", repoPath: "/code/alpha", repoLabel: "Alpha", presence: "live", fullyIdle: true, attention: ["finished"] }),
      makeRow({ id: "b1", repoPath: "/code/Beta", repoLabel: "Beta", presence: "live" }),
    ];
    const groups = groupAgentTreeByRepo(roots, { caseInsensitive: true });
    expect(groups.map((g) => g.repoLabel)).toEqual(["Alpha", "Beta", "Zebra"]);
    expect(groups[0]).toMatchObject({ total: 2, running: 1, attention: 0, done: 1 });
    expect(groups[1]).toMatchObject({ total: 1, running: 1, attention: 0, done: 0 });
    expect(groups[2]).toMatchObject({ total: 1, running: 0, attention: 1, done: 0 });
  });
});

describe("countAgentFilters", () => {
  it("computes counts for each filter from scoped rows", () => {
    const scoped = [
      makeRow({ id: "r1", presence: "live", attention: ["needs-you"], parallelCount: 2 }),
      makeRow({ id: "r2", presence: "live", attention: ["finished"], parallelCount: 2 }),
      makeRow({ id: "r3", presence: "on-disk", attention: ["unmeasured"], parallelCount: 1 }),
    ];
    expect(countAgentFilters(scoped)).toEqual({
      all: 3,
      attention: 2,
      notifications: 1,
      gaps: 1,
      live: 2,
      parallel: 2,
    });
  });
});

describe("collectAttentionAndDoneRows and attentionTrayHeading", () => {
  it("collects attention and done rows across roots and descendants sorted error -> blocking -> done, then recency", () => {
    const childBlocked = makeRow({
      id: "child-blocked",
      parentId: "root-quiet",
      repoLabel: "Alpha",
      session: "child-ask",
      attention: ["needs-you"],
      attentionUrgency: "blocking",
      attentionAt: 200,
    });
    const rootQuiet = makeRow({
      id: "root-quiet",
      repoLabel: "Alpha",
      session: "parent-quiet",
      attention: ["dirty"],
      children: [childBlocked],
    });
    const rootErr = makeRow({
      id: "root-err",
      repoLabel: "Beta",
      session: "err-session",
      attention: ["error"],
      attentionUrgency: "error",
      attentionAt: 100,
    });
    const rootDoneOlder = makeRow({
      id: "root-done-old",
      repoLabel: "Beta",
      session: "done-old",
      attention: ["finished"],
      attentionAt: 300,
    });
    const rootDoneNewer = makeRow({
      id: "root-done-new",
      repoLabel: "Alpha",
      session: "done-new",
      attention: ["idle-waiting"],
      attentionAt: 500,
    });

    const collected = collectAttentionAndDoneRows([rootQuiet, rootDoneOlder, rootErr, rootDoneNewer]);
    expect(collected.map((r) => r.id)).toEqual([
      "root-err",
      "child-blocked",
      "root-done-new",
      "root-done-old",
    ]);
    expect(attentionTrayHeading(collected)).toBe("Needs attention & finished");
    expect(attentionTrayHeading([rootErr, childBlocked])).toBe("Needs attention");
    expect(attentionTrayHeading([rootDoneNewer, rootDoneOlder])).toBe("Finished");
  });
});
