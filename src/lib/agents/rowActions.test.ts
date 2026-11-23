import { describe, expect, it } from "vitest";
import type { InsightsSnapshot, LiveSession } from "../insights/types";
import type { AgentRow } from "./plane";
import { agentRowActions, startLauncher } from "./rowActions";
import { listing, probe, project, snapshot, task, tasks, terminal, worktree } from "./__tests__/fixtures";

const ALPHA = "/repo/.claude/worktrees/alpha";
const LANE = "/repo/.gitpulse/worktrees/lane";

function withLive(sessions: LiveSession[], items = [worktree()]): InsightsSnapshot {
  const base = snapshot({ worktrees: listing(items) });
  return {
    ...base,
    agents: { ...base.agents, live: { ok: true, sessions: sessions.length, kinds: [], worktrees: [{ path: ALPHA, sessions }] } },
  };
}

function live(pid: number, fields: Partial<LiveSession> = {}): LiveSession {
  return { kind: "claude", pid, entrypoint: "cli", status: "busy", cwd: ALPHA, ...fields };
}

const only = (rows: readonly AgentRow[], predicate: (row: AgentRow) => boolean): AgentRow => {
  const found = rows.filter(predicate);
  if (found.length !== 1) throw new Error(`expected one row, found ${found.length}`);
  return found[0];
};

const kinds = (row: AgentRow, parent: AgentRow | null = null, launcher: "shell" | "claude" | "browser" = "shell") =>
  agentRowActions(row, parent, launcher).map((action) => action.kind);

describe("an agent checkout with nothing of this window in it", () => {
  it("offers to start its agent there, not only to open the checkout", () => {
    const row = only(project({ tasks: null }).rows, (item) => item.origin === "checkout");
    expect(row.liveKey).toBeNull();
    expect(row.taskRunId).toBeNull();
    const actions = agentRowActions(row, null, "shell");
    expect(actions.map((action) => action.kind)).toEqual(["start", "checkout"]);
    const start = actions[0];
    // The layout names Claude, so Claude starts, whatever the dock's default.
    expect(start.launcher).toBe("claude");
    expect(start.label).toBe("Start Claude Code");
    expect(start.ariaLabel).toContain(row.session);
    expect(start.target).toBe(row);
  });

  it("starts the dock's default launcher when its layout names no agent GitPulse can launch", () => {
    const plane = project({
      probes: [probe({ snapshot: snapshot({ worktrees: listing([worktree({ path: LANE, agent_kind: "gitpulse", session_slug: "lane", name: "lane" })]) }) })],
      tasks: null,
    });
    const row = only(plane.rows, (item) => item.origin === "checkout");
    expect(startLauncher(row, "shell")).toBe("shell");
    expect(agentRowActions(row, null, "shell")[0].label).toBe("Open terminal");
    expect(startLauncher(row, "codex")).toBe("codex");
    // A browser session is no way to work in a checkout.
    expect(startLauncher(row, "browser")).toBe("shell");
  });

  it("offers to review uncommitted work, and only when it was counted", () => {
    const dirty = project({ probes: [probe({ snapshot: snapshot({ worktrees: listing([worktree({ dirty_files: 3 })]) }) })], tasks: null });
    const row = only(dirty.rows, (item) => item.origin === "checkout");
    expect(kinds(row)).toEqual(["start", "review", "checkout"]);
    expect(agentRowActions(row, null, "shell")[1].label).toBe("Review 3 changes");

    const unread = project({ probes: [probe({ snapshot: snapshot({ worktrees: listing([worktree({ dirty_files: null })]) }) })], tasks: null });
    expect(kinds(only(unread.rows, (item) => item.origin === "checkout"))).toEqual(["start", "checkout"]);
  });

  it("says when an agent started outside this window is already working in it", () => {
    const plane = project({ probes: [probe({ snapshot: withLive([live(100)]) })], tasks: null });
    const row = only(plane.rows, (item) => item.origin === "checkout");
    expect(row.outsideSessions).toBe(1);
    expect(row.presence).toBe("on-disk");
    expect(row.presenceDetail).toContain("1 agent process running in it");
    const start = agentRowActions(row, null, "shell")[0];
    expect(start.label).toBe("Start another Claude Code");
    expect(start.title).toContain("already running there");
    // Counts are unchanged by it: it is not a live terminal of this window.
    expect(plane.live).toBe(0);
  });

  it("does not count a process a GitPulse launch accounts for, or a child process", () => {
    const plane = project({
      probes: [probe({ snapshot: withLive([live(100, { tree: "session:other" }), live(200), live(201, { parent_pid: 200 })]) })],
      tasks: null,
    });
    expect(only(plane.rows, (item) => item.origin === "checkout").outsideSessions).toBe(1);
  });
});

describe("rows with a terminal or a run keep their moves", () => {
  it("a checkout with this window's terminal shows it and offers no second start", () => {
    const plane = project({ terminals: [terminal({ cwd: ALPHA })], tasks: null });
    const row = only(plane.rows, (item) => item.liveKey === "t1");
    expect(kinds(row)).toEqual(["terminal", "checkout"]);
  });

  it("an attempt shows its terminal and task", () => {
    const plane = project({ tasks: tasks({ tasks: [task({ disconnected: true, tone: "problem" })] }) });
    const row = only(plane.rows, (item) => item.taskRunId === "run-1");
    expect(kinds(row)).toEqual(["terminal", "task", "checkout"]);
  });

  it("names each move for the row it acts on, so a screen reader can tell rows' buttons apart", () => {
    const plane = project({ tasks: tasks({ tasks: [task({ disconnected: true, tone: "problem" })] }) });
    const row = only(plane.rows, (item) => item.taskRunId === "run-1");
    const [terminal, taskAction] = agentRowActions(row, null, "shell");
    expect(terminal.label).toBe("Show terminal");
    expect(terminal.ariaLabel).toBe(`Show terminal for ${row.session}`);
    expect(taskAction.label).toBe("Open task");
    expect(taskAction.ariaLabel).toBe(`Open task for ${row.session}`);
  });
});

describe("a child agent process", () => {
  it("offers its parent's terminal and task, named as the parent's", () => {
    const plane = project({
      probes: [probe({ snapshot: withLive([live(100, { tree: "session:s1" }), live(101, { tree: "session:s1", parent_pid: 100 })]) })],
      terminals: [terminal({ key: "t1", sessionId: "s1", cwd: ALPHA, taskRunId: "run-1" })],
      tasks: tasks({ tasks: [task()] }),
    });
    const parent = only(plane.rows, (item) => item.liveKey === "t1");
    const child = parent.children[0];
    expect(child.origin).toBe("session");
    const actions = agentRowActions(child, parent, "shell");
    expect(actions.map((action) => action.kind)).toEqual(["terminal", "task", "checkout"]);
    expect(actions[0].label).toBe("Show parent terminal");
    expect(actions[0].target).toBe(parent);
    expect(actions[1].target).toBe(parent);
    // Never offered a start: the child is already an agent working there.
    expect(actions.some((action) => action.kind === "start")).toBe(false);
  });

  it("offers nothing of the parent's when the parent has neither terminal nor run", () => {
    const plane = project({ probes: [probe({ snapshot: withLive([live(100), live(101, { parent_pid: 100 })]) })], tasks: null });
    const parent = only(plane.rows, (item) => item.origin === "checkout");
    const child = parent.children[0];
    expect(kinds(child, parent)).toEqual(["checkout"]);
  });
});
