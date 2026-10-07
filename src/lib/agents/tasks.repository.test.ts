import { describe, expect, it } from "vitest";
import type { TaskRun } from "../workbench/client";
import { repositoryPaths, taskProbeFromBoard, type TaskBoardWatch } from "./tasks";

const NOW = 1_800_000_000_000;
const SECONDS = NOW / 1000;
const PATHS = { caseInsensitive: false };

function run(fields: Partial<TaskRun> & { id: string }): TaskRun {
  return {
    revision: 1, updated_at: SECONDS, kind: "external_terminal", task_id: "t1", source_revision: 1,
    task_title: "Fix the gate", repository_id: "r1", provider: "claude", permission_mode: "ask",
    state: "running", cwd: "/scratch/attempt-1", created_at: SECONDS, expires_at: SECONDS + 600,
    session_id: null, exit_code: null, reason: "", outcome_uncertain: false, ...fields,
  };
}

const watch: TaskBoardWatch = {
  records: [],
  requests: [],
  activity: () => undefined,
  sessionLimit: 8,
  now: NOW,
};

const board = (runs: TaskRun[]) => ({ runs, complete: true, pending: new Map(), readAt: NOW, error: null });

describe("P6: a task attempt's repository comes from its repository id", () => {
  it("uses the resolved repository path, never the attempt's directory", () => {
    const probe = taskProbeFromBoard(board([run({ id: "run-1" })]), {
      ...watch,
      repositoryPath: (id) => (id === "r1" ? "/repo" : null),
    });
    expect(probe.tasks[0]).toMatchObject({ repoPath: "/repo", cwd: "/scratch/attempt-1" });
  });

  it("leaves an unresolved repository empty rather than borrowing the directory", () => {
    const probe = taskProbeFromBoard(board([run({ id: "run-1", repository_id: "gone" })]), {
      ...watch,
      repositoryPath: () => null,
    });
    expect(probe.tasks[0].repoPath).toBe("");
    expect(taskProbeFromBoard(board([run({ id: "run-1" })]), watch).tasks[0].repoPath).toBe("");
  });
});

describe("repositoryPaths", () => {
  const registered = [
    { id: "r1", identity_key: "local:/code/app/.git" },
    { id: "r2", identity_key: "local:/code/lib/.git" },
    { id: "r3", identity_key: "remote:github.com/x/y" },
  ];

  it("prefers the open tab that is the registered checkout", () => {
    const paths = repositoryPaths(registered, [{ path: "/code/app" }], PATHS);
    expect(paths.get("r1")).toBe("/code/app");
  });

  it("falls back to where the store says the repository lives, and skips an identity with no location", () => {
    const paths = repositoryPaths(registered, [], PATHS);
    expect(paths.get("r2")).toBe("/code/lib");
    expect(paths.has("r3")).toBe(false);
  });
});
