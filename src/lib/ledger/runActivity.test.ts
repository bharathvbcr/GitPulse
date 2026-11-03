import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
vi.mock("../ipc/invoke", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));

import { RUN_ACTIVITY_ROWS, readRunRows, runSessionIds, runWindowOf } from "./runActivity";
import { segmentsFrom } from "./runTimeline";

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
    expect(runWindowOf({ state: "running", claimed_at: 10, started_at: 12, finished_at: null, outcome_uncertain: false })).toEqual({
      claimedAt: 10,
      startedAt: 12,
      finishedAt: null,
      live: true,
    });
    expect(runWindowOf({ state: "exited", claimed_at: undefined, started_at: undefined, finished_at: 99, outcome_uncertain: false })).toEqual({
      claimedAt: null,
      startedAt: null,
      finishedAt: 99,
      live: false,
    });
  });

  it("drops finished_at when the outcome is uncertain: a reconciler stamped it, the agent did not end then", () => {
    expect(
      runWindowOf({ state: "exited", claimed_at: 10, started_at: 12, finished_at: 99, outcome_uncertain: true }),
    ).toEqual({ claimedAt: 10, startedAt: 12, finishedAt: null, live: false });
    expect(
      runWindowOf({ state: "exited", claimed_at: 10, started_at: 12, finished_at: 99, outcome_uncertain: false })
        .finishedAt,
    ).toBe(99);
  });

  it("draws an uncertain run's last span as unknown with no length", () => {
    const transitions = [{ atMs: 20_000, event: "prompt_submitted" as const, id: 1 }];
    const uncertain = runWindowOf({ state: "exited", started_at: 12, finished_at: 400, outcome_uncertain: true });
    const last = segmentsFrom(transitions, uncertain, 1_000_000).at(-1);
    expect(last).toMatchObject({ state: "unknown", bound: "unknown", endMs: null, durationMs: null });
    const certain = runWindowOf({ state: "exited", started_at: 12, finished_at: 400, outcome_uncertain: false });
    expect(segmentsFrom(transitions, certain, 1_000_000).at(-1)).toMatchObject({ bound: "finished", durationMs: 380_000 });
  });

  it("treats every ended or unresolved state as not live", () => {
    for (const state of ["prepared", "exited", "failed", "cancelled", "unresolved"] as const) {
      expect(runWindowOf({ state, outcome_uncertain: false }).live, state).toBe(false);
    }
    for (const state of ["starting", "running"] as const) {
      expect(runWindowOf({ state, outcome_uncertain: false }).live, state).toBe(true);
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
