import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
vi.mock("../ipc/invoke", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));

import { RUN_ACTIVITY_ROWS, readRunRows, runSessionIds, runWindowOf } from "./runActivity";

describe("runSessionIds", () => {
  it("is the agent's own id and the terminal's, once each", () => {
    expect(runSessionIds({ id: "claude-uuid", session_id: "pty-1" })).toEqual(["claude-uuid", "pty-1"]);
    expect(runSessionIds({ id: "same", session_id: "same" })).toEqual(["same"]);
  });

  it("leaves out a terminal id the run does not have", () => {
    expect(runSessionIds({ id: "claude-uuid", session_id: null })).toEqual(["claude-uuid"]);
    expect(runSessionIds({ id: "claude-uuid", session_id: "" })).toEqual(["claude-uuid"]);
  });
});

describe("runWindowOf", () => {
  it("carries the run's own clock and whether it is going", () => {
    expect(runWindowOf({ state: "running", claimed_at: 10, started_at: 12, finished_at: null })).toEqual({
      claimedAt: 10,
      startedAt: 12,
      finishedAt: null,
      live: true,
    });
    expect(runWindowOf({ state: "exited", claimed_at: undefined, started_at: undefined, finished_at: 99 })).toEqual({
      claimedAt: null,
      startedAt: null,
      finishedAt: 99,
      live: false,
    });
  });

  it("treats every ended or unresolved state as not live", () => {
    for (const state of ["prepared", "exited", "failed", "cancelled", "unresolved"] as const) {
      expect(runWindowOf({ state }).live, state).toBe(false);
    }
    for (const state of ["starting", "running"] as const) {
      expect(runWindowOf({ state }).live, state).toBe(true);
    }
  });
});

describe("readRunRows", () => {
  beforeEach(() => invoke.mockReset());

  it("asks the one tail command for the newest rows of both ids", async () => {
    invoke.mockResolvedValue([]);
    await readRunRows({ id: "claude-uuid", session_id: "pty-1", cwd: "/work/tree" });
    expect(invoke).toHaveBeenCalledWith("cmd_ledger_tail", {
      repoPath: "/work/tree",
      cursor: 0,
      limit: RUN_ACTIVITY_ROWS,
      newest: true,
      sessionIds: ["claude-uuid", "pty-1"],
      action: null,
    });
  });
});
