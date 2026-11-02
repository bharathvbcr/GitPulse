import { describe, expect, it } from "vitest";
import type { InsightsSnapshot, LiveSession } from "../insights/types";
import {
  applyAgentFilter,
  applyAgentTree,
  countAgentRows,
  flattenAgentTree,
  planeHeadline,
  rollupSummary,
  type AgentRow,
} from "./plane";
import { listing, probe, project, snapshot, task, tasks, terminal, worktree } from "./__tests__/fixtures";

const ALPHA = "/repo/.claude/worktrees/alpha";
const BETA = "/repo/.claude/worktrees/beta";

function live(pid: number, fields: Partial<LiveSession> = {}): LiveSession {
  return { kind: "claude", pid, entrypoint: "cli", status: "busy", cwd: ALPHA, ...fields };
}

/** A snapshot of the usual repository whose live-session facet holds `sessions`, by worktree. */
function withLive(byWorktree: Record<string, LiveSession[]>): InsightsSnapshot {
  const base = snapshot({
    worktrees: listing([
      worktree({ path: "/repo", name: "repo", agent_kind: "", session_slug: "", is_main: true, branch: "main" }),
      worktree(),
      worktree({ path: BETA, session_slug: "beta", name: "beta", branch: "agent/beta" }),
    ]),
  });
  const worktrees = Object.entries(byWorktree).map(([path, sessions]) => ({ path, sessions }));
  return {
    ...base,
    agents: {
      ...base.agents,
      live: { ok: true, sessions: worktrees.reduce((n, w) => n + w.sessions.length, 0), kinds: [], worktrees },
    },
  };
}

const find = (rows: readonly AgentRow[], predicate: (row: AgentRow) => boolean): AgentRow => {
  const row = rows.find(predicate);
  if (!row) throw new Error("no such row");
  return row;
};

describe("a terminal that continues an attempt", () => {
  const input = {
    tasks: tasks({ tasks: [task({ runId: "run-1" })] }),
    terminals: [
      terminal({ key: "t1", sessionId: "s1", taskRunId: "run-1", cwd: ALPHA, title: "Attempt" }),
      terminal({ key: "t2", sessionId: "s2", continuesRunId: "run-1", cwd: ALPHA, title: "Follow-up" }),
    ],
  };

  it("nests under the attempt's row, and every count stays what it was", () => {
    const plane = project(input);
    const followUp = find(plane.rows, (row) => row.liveKey === "t2");
    const attempt = find(plane.rows, (row) => row.liveKey === "t1");
    expect(followUp.parentId).toBe(attempt.id);
    expect(attempt.children).toEqual([followUp]);
    // `rows` is still every row once, so a count over it is the count it was.
    expect(plane.rows.filter((row) => row.presence === "live")).toHaveLength(2);
    expect(plane.live).toBe(2);
    expect(plane.roots.map((row) => row.liveKey)).not.toContain("t2");
    expect(plane.roots).toContain(attempt);
    expect(attempt.rollup).toMatchObject({ descendants: 1, live: 1, unobserved: 0 });
  });

  it("rolls the child's request up to the parent without making it the parent's own", () => {
    const plane = project({
      ...input,
      terminals: [input.terminals[0], { ...input.terminals[1], attention: "needs-you" }],
    });
    const attempt = find(plane.rows, (row) => row.liveKey === "t1");
    expect(attempt.attention).not.toContain("needs-you");
    expect(attempt.rollup.attention).toEqual(["needs-you"]);
    expect(rollupSummary(attempt).attention).toBe("Under it: Needs you");
    // The row asking is counted once, as before.
    expect(plane.attention.needing).toBe(1);
  });

  it("is not nested when the attempt it continues has no row of its own to hold it", () => {
    const plane = project({ terminals: [input.terminals[1]], tasks: tasks({ tasks: [] }) });
    expect(plane.rows.every((row) => row.parentId === null)).toBe(true);
    expect(plane.roots).toEqual(plane.rows);
  });
});

describe("child agent processes", () => {
  const session = (extra: Partial<PlaneTerminalFields> = {}) => terminal({ key: "t1", sessionId: "s1", cwd: ALPHA, ...extra });
  type PlaneTerminalFields = Parameters<typeof terminal>[0];

  it("nest under the terminal whose launch they carry, and are rows only there", () => {
    const processes = [
      live(100, { tree: "session:s1" }),
      live(101, { tree: "session:s1", parent_pid: 100, kind: "codex", status: "idle" }),
    ];
    const plane = project({
      probes: [probe({ snapshot: withLive({ [ALPHA]: processes }) })],
      terminals: [session()],
    });
    const parent = find(plane.rows, (row) => row.liveKey === "t1");
    expect(parent.children.map((row) => [row.origin, row.kind])).toEqual([["session", "codex"]]);
    const child = parent.children[0];
    expect(child.parentId).toBe(parent.id);
    expect(child.session).toContain("101");
    expect(child.presenceDetail).toContain("idle");
    expect(plane.childSessions).toBe(1);
    expect(plane.unplacedSessions).toBe(0);
    // Not a checkout, terminal or attempt: it is in no count of those.
    expect(plane.rows.some((row) => row.origin === "session")).toBe(false);
    expect(plane.live).toBe(1);
    expect(countAgentRows(plane.roots)).toBe(plane.rows.length);
    expect(parent.rollup).toMatchObject({ descendants: 1, live: 1, unobserved: 1 });
    expect(rollupSummary(parent).attention).toBe("1 not observed");
  });

  it("report that nothing observes what they ask for, never that they ask nothing", () => {
    const plane = project({
      probes: [probe({ snapshot: withLive({ [ALPHA]: [live(100, { tree: "session:s1" }), live(101, { tree: "session:s1", parent_pid: 100 })] }) })],
      terminals: [session()],
    });
    const child = find(plane.rows, (row) => row.liveKey === "t1").children[0];
    expect(child.attention).toEqual([]);
    expect(child.attentionObserved).toBe(false);
  });

  it("nest as deep as the process tree does", () => {
    const plane = project({
      probes: [probe({ snapshot: withLive({ [ALPHA]: [
        live(100, { tree: "session:s1" }),
        live(101, { parent_pid: 100, tree: "session:s1" }),
        live(102, { parent_pid: 101, tree: "session:s1" }),
      ] }) })],
      terminals: [session()],
    });
    const top = find(plane.rows, (row) => row.liveKey === "t1");
    expect(top.children).toHaveLength(1);
    expect(top.children[0].children).toHaveLength(1);
    expect(top.rollup.descendants).toBe(2);
    expect(plane.childSessions).toBe(2);
    const depth = flattenAgentTree([top], new Set()).map((entry) => entry.level);
    expect(depth).toEqual([1, 2, 3]);
  });

  it("find their place by directory when nothing of this window launched the session above", () => {
    const plane = project({
      probes: [probe({ snapshot: withLive({ [BETA]: [live(200, { cwd: BETA }), live(201, { cwd: BETA, parent_pid: 200 })] }) })],
    });
    const checkout = find(plane.rows, (row) => row.worktreePath === BETA);
    expect(checkout.children.map((row) => row.origin)).toEqual(["session"]);
    expect(plane.unplacedSessions).toBe(0);
  });

  it("are counted as unplaced, with a note, when no row is the place for the session above", () => {
    const plane = project({
      probes: [probe({ snapshot: withLive({ "/elsewhere": [live(300, { cwd: "/elsewhere" }), live(301, { cwd: "/elsewhere", parent_pid: 300 })] }) })],
    });
    expect(plane.childSessions).toBe(0);
    expect(plane.unplacedSessions).toBe(1);
    expect(plane.gaps.some((gap) => gap.label === "Child sessions" && gap.kind === "partial")).toBe(true);
  });

  it("are counted as unplaced, with a note, when the session above them is in no probe read", () => {
    // Pid 812's parent is 9_999, an agent that runs outside every probed worktree.
    const plane = project({
      probes: [probe({ snapshot: withLive({ [ALPHA]: [live(812, { tree: "session:s1", parent_pid: 9_999 })] }) })],
      terminals: [session()],
    });
    expect(find(plane.rows, (row) => row.liveKey === "t1").children).toEqual([]);
    expect(plane.childSessions).toBe(0);
    expect(plane.unplacedSessions).toBe(1);
    const note = plane.gaps.find((gap) => gap.label === "Child sessions");
    expect(note?.kind).toBe("partial");
    expect(note?.reason).toContain("1 child agent session");
  });

  it("terminate on a lineage that loops, and attach to nothing", () => {
    const plane = project({
      probes: [probe({ snapshot: withLive({ [ALPHA]: [
        live(400, { parent_pid: 401, tree: "session:s1" }),
        live(401, { parent_pid: 400, tree: "session:s1" }),
        live(402, { parent_pid: 402, tree: "session:s1" }),
      ] }) })],
      terminals: [session()],
    });
    expect(find(plane.rows, (row) => row.liveKey === "t1").children).toEqual([]);
    expect(plane.childSessions).toBe(0);
    expect(plane.unplacedSessions).toBe(3);
  });

  it("are one process when two probes of one repository both list it", () => {
    const shared = withLive({ [ALPHA]: [live(100, { tree: "session:s1" }), live(101, { tree: "session:s1", parent_pid: 100 })] });
    const plane = project({
      probes: [probe({ snapshot: shared }), probe({ path: ALPHA, label: "alpha", snapshot: shared })],
      terminals: [session()],
    });
    expect(plane.childSessions).toBe(1);
  });
});

describe("the tree as a view draws it", () => {
  const plane = project({
    probes: [probe({ snapshot: withLive({ [ALPHA]: [live(100, { tree: "session:s1" }), live(101, { tree: "session:s1", parent_pid: 100 })] }) })],
    terminals: [
      terminal({ key: "t1", sessionId: "s1", taskRunId: "run-1", cwd: ALPHA }),
      terminal({ key: "t2", sessionId: "s2", continuesRunId: "run-1", cwd: ALPHA, attention: "needs-you" }),
    ],
    tasks: tasks({ tasks: [task({ runId: "run-1" })] }),
  });

  it("lists a parent before what is under it, with its place among its siblings", () => {
    const entries = flattenAgentTree(plane.roots, new Set());
    const parent = find(plane.rows, (row) => row.liveKey === "t1");
    const at = entries.findIndex((entry) => entry.row === parent);
    expect(entries[at]).toMatchObject({ level: 1, expandable: true, expanded: true });
    expect(entries.slice(at + 1, at + 3).map((entry) => entry.level)).toEqual([2, 2]);
    expect(entries[at + 1].setSize).toBe(2);
    expect([entries[at + 1].posInSet, entries[at + 2].posInSet]).toEqual([1, 2]);
  });

  it("hides what is under a collapsed row and keeps the row", () => {
    const parent = find(plane.rows, (row) => row.liveKey === "t1");
    const all = flattenAgentTree(plane.roots, new Set());
    const collapsed = flattenAgentTree(plane.roots, new Set([parent.id]));
    expect(collapsed).toHaveLength(all.length - 2);
    expect(collapsed.find((entry) => entry.row === parent)).toMatchObject({ expandable: true, expanded: false });
  });

  it("keeps a quiet parent for the child that asked, where the flat filter would not", () => {
    const parent = find(plane.rows, (row) => row.liveKey === "t1");
    expect(applyAgentFilter([parent], "attention")).toEqual([]);
    expect(applyAgentTree(plane.roots, "attention")).toContain(parent);
    expect(applyAgentTree(plane.roots, "attention").every((row) => row.parentId === null)).toBe(true);
    expect(applyAgentTree(plane.roots, "all")).toEqual(plane.roots);
  });

  it("names the child sessions in the headline", () => {
    expect(planeHeadline(plane, countAgentRows(plane.roots))).toContain("1 child session");
  });
});
