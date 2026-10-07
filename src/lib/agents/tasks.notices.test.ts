import { describe, expect, it } from "vitest";
import type { TaskRun } from "../workbench/client";
import { taskProbeFromBoard, type TaskBoardWatch } from "./tasks";

/**
 * A start that failed out of sight (the CLI missing, a refused open) is
 * recorded once, in `attemptNotices`. The task sheet's pane read it; the
 * Agents view and the board cards did not, so the same attempt read red in
 * one place and as quietly waiting in the others.
 */
const NOW = 1_800_000_000_000;
const SECONDS = NOW / 1000;

const run: TaskRun = {
  id: "run-1", revision: 1, updated_at: SECONDS, kind: "external_terminal", task_id: "t1", source_revision: 1,
  task_title: "Fix the gate", repository_id: "r1", provider: "claude", permission_mode: "ask",
  state: "prepared", cwd: "/repo/.gitpulse/worktrees/fix-gate-1a2b3c4d", created_at: SECONDS, expires_at: SECONDS + 300,
  session_id: null, exit_code: null, reason: "", outcome_uncertain: false,
};

const board = { runs: [run], complete: true, pending: new Map(), readAt: NOW, error: null };
const watch: TaskBoardWatch = { records: [], requests: [], activity: () => undefined, sessionLimit: 8, now: NOW };

describe("a failed start reaches every surface that reads the board", () => {
  it("turns the attempt's tone to error when the launch owner recorded a failure", () => {
    const quiet = taskProbeFromBoard(board, watch).tasks[0];
    const failed = taskProbeFromBoard(board, {
      ...watch,
      notices: (id) => (id === "run-1"
        ? { runId: "run-1", phase: "failed", tone: "error", text: "claude is not installed", at: NOW }
        : undefined),
    }).tasks[0];
    expect(failed.tone).not.toBe(quiet.tone);
    expect(failed.tone).toBe("error");
  });
});
