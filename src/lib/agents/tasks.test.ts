import { describe, expect, it } from "vitest";
import type { TaskRun } from "../workbench/client";
import type { TerminalSessionRecord } from "../terminal/sessionRegistry";
import { get } from "svelte/store";
import {
  MAX_REPOSITORY_PAGES,
  REPOSITORY_RETRY_MS,
  createRegisteredRepositories,
  readRegisteredRepositories,
  repositoryPaths,
  taskProbeFromBoard,
  type TaskBoardWatch,
} from "./tasks";

const NOW = 1_800_000_000_000;
const SECONDS = NOW / 1000;

function run(fields: Partial<TaskRun> & { id: string }): TaskRun {
  return {
    revision: 1, updated_at: SECONDS, kind: "external_terminal", task_id: "t1", source_revision: 1,
    task_title: "Fix the gate", repository_id: "r1", provider: "claude", permission_mode: "ask",
    state: "running", cwd: "/repo/.claude/worktrees/alpha", created_at: SECONDS, expires_at: SECONDS + 600,
    session_id: null, exit_code: null, reason: "", outcome_uncertain: false, ...fields,
  };
}

const watch: TaskBoardWatch = {
  records: [],
  requests: [],
  activity: () => undefined,
  sessionLimit: 8,
  now: NOW,
  repositoryPath: () => null,
};

const record = (fields: Partial<TerminalSessionRecord> & { key: string }): TerminalSessionRecord =>
  ({ repoPath: "/repo/.claude/worktrees/alpha", label: "Claude", status: "running", close: async () => {}, ...fields });

describe("taskProbeFromBoard", () => {
  it("keeps an unread board distinct from a failed read and from no attempts", () => {
    expect(taskProbeFromBoard(
      { runs: [], complete: true, pending: new Map(), readAt: null, error: null },
      watch,
    )).toMatchObject({ unread: true, ok: false, tasks: [] });

    expect(taskProbeFromBoard(
      { runs: [], complete: true, pending: new Map(), readAt: null, error: "store down" },
      watch,
    )).toMatchObject({ unread: false, ok: false, error: "store down" });

    expect(taskProbeFromBoard(
      { runs: [], complete: true, pending: new Map(), readAt: NOW, error: null },
      watch,
    )).toMatchObject({ ok: true, unread: false, complete: true, tasks: [] });
  });

  it("carries the shared judgement and a capped pending page", () => {
    const probe = taskProbeFromBoard({
      runs: [run({ id: "run-1" })],
      complete: false,
      pending: new Map([["run-1", { count: 1, more: true }]]),
      readAt: NOW,
      error: null,
    }, {
      ...watch,
      records: [record({ key: "k", taskRunId: "run-1", sessionId: "term" })],
      activity: () => ({ lastOutputAt: NOW, title: null, attention: { kind: "needs-you", label: "Needs you", detail: null, at: NOW } }),
    });
    expect(probe.complete).toBe(false);
    expect(probe.tasks[0]).toMatchObject({
      runId: "run-1",
      tone: "needs-you",
      disconnected: false,
      pendingCount: 1,
      pendingMore: true,
      cwd: "/repo/.claude/worktrees/alpha",
    });
  });

  it("reports a running attempt with no terminal here as disconnected", () => {
    const probe = taskProbeFromBoard({
      runs: [run({ id: "run-2" })],
      complete: true,
      pending: new Map(),
      readAt: NOW,
      error: null,
    }, watch);
    expect(probe.tasks[0]).toMatchObject({ disconnected: true, tone: "problem" });
  });

  it("P6: names the run's repository from its repository_id, not from its cwd", () => {
    const board = {
      runs: [run({ id: "run-1", repository_id: "r1" }), run({ id: "run-2", repository_id: "r-gone", cwd: "/scratch/x" })],
      complete: true,
      pending: new Map(),
      readAt: NOW,
      error: null,
    };
    const probe = taskProbeFromBoard(board, {
      ...watch,
      repositoryPath: (id) => (id === "r1" ? "/repo" : null),
    });
    expect(probe.tasks[0]).toMatchObject({ repoPath: "/repo", cwd: "/repo/.claude/worktrees/alpha" });
    expect(probe.tasks[1]).toMatchObject({ repoPath: "", cwd: "/scratch/x" });
  });
});

describe("readRegisteredRepositories", () => {
  const repo = (id: string) => ({ id, revision: 1, updated_at: 0, name: id, identity_key: `local:/${id}/.git`, remote_url: null });

  it("follows the cursor to the end, and stops at the page cap saying so", async () => {
    const cursors: Array<string | undefined> = [];
    const whole = await readRegisteredRepositories(async (cursor) => {
      cursors.push(cursor);
      return cursor
        ? { items: [repo("b")], total: 2, shown: 1, has_more: false, next_cursor: null }
        : { items: [repo("a")], total: 2, shown: 1, has_more: true, next_cursor: "c1" };
    });
    expect(cursors).toEqual([undefined, "c1"]);
    expect(whole).toMatchObject({ complete: true });
    expect(whole.repositories.map((item) => item.id)).toEqual(["a", "b"]);

    let calls = 0;
    const capped = await readRegisteredRepositories(async () => {
      calls += 1;
      return { items: [repo(`r${calls}`)], total: 999, shown: 1, has_more: true, next_cursor: `c${calls}` };
    });
    expect(calls).toBe(MAX_REPOSITORY_PAGES);
    expect(capped.complete).toBe(false);
  });
});

describe("createRegisteredRepositories", () => {
  const repo = (id: string) => ({ id, revision: 1, updated_at: 0, name: id, identity_key: `local:/${id}/.git`, remote_url: null });
  const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

  it("reads once, again for a new id, and never twice for an id that stays unknown", async () => {
    let reads = 0;
    const registry = createRegisteredRepositories(async () => {
      reads += 1;
      return { repositories: [repo("r1")] };
    });
    registry.want([]);
    registry.want(["r1"]);
    await settle();
    expect(reads).toBe(1);
    expect(get(registry).map((item) => item.id)).toEqual(["r1"]);
    registry.want(["r1"]);
    expect(reads).toBe(1);
    registry.want(["r1", "gone"]);
    await settle();
    expect(reads).toBe(2);
    registry.want(["gone", "r1"]);
    registry.want(["gone"]);
    await settle();
    expect(reads).toBe(2);
  });

  it("keeps the last answer when a read fails", async () => {
    let fail = false;
    const registry = createRegisteredRepositories(async () => {
      if (fail) throw new Error("store down");
      return { repositories: [repo("r1")] };
    });
    registry.want([]);
    await settle();
    fail = true;
    registry.want(["r2"]);
    await settle();
    expect(get(registry).map((item) => item.id)).toEqual(["r1"]);
  });

  it("retries a failed read for the same ids once the wait has passed, not before", async () => {
    let time = 0;
    let reads = 0;
    let fail = true;
    const registry = createRegisteredRepositories(async () => {
      reads += 1;
      if (fail) throw new Error("store down");
      return { repositories: [repo("r1")] };
    }, () => time);
    registry.want(["r1"]);
    await settle();
    expect(reads).toBe(1);
    registry.want(["r1"]);
    await settle();
    expect(reads).toBe(1);
    time = REPOSITORY_RETRY_MS;
    fail = false;
    registry.want(["r1"]);
    await settle();
    expect(reads).toBe(2);
    expect(get(registry).map((item) => item.id)).toEqual(["r1"]);
    time = 10 * REPOSITORY_RETRY_MS;
    registry.want(["r1"]);
    await settle();
    expect(reads).toBe(2);
  });

  it("reads again at once when refreshed, and not while a read is running", async () => {
    let reads = 0;
    const registry = createRegisteredRepositories(async () => {
      reads += 1;
      return { repositories: [repo("r1")] };
    });
    registry.want(["r1"]);
    registry.refresh();
    await settle();
    expect(reads).toBe(1);
    registry.refresh();
    await settle();
    expect(reads).toBe(2);
  });
});

describe("repositoryPaths", () => {
  const PATHS = { caseInsensitive: false };
  const repositories = [
    { id: "r1", identity_key: "local:/repo/.git" },
    { id: "r2", identity_key: "local:/closed/.git" },
    { id: "r3", identity_key: "remote:github.com/x" },
  ];

  it("prefers the open checkout and falls back to the folder beside the common directory", () => {
    const paths = repositoryPaths(repositories, [{ path: "/repo" }], PATHS);
    expect(paths.get("r1")).toBe("/repo");
    expect(paths.get("r2")).toBe("/closed");
    expect(paths.has("r3")).toBe(false);
  });
});
