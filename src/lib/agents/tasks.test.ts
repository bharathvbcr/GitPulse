import { describe, expect, it } from "vitest";
import type { TaskRun } from "../workbench/client";
import type { TerminalSessionRecord } from "../terminal/sessionRegistry";
import { taskProbeFromBoard, type TaskBoardWatch } from "./tasks";

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
});
