import { describe, expect, it, vi } from "vitest";
import type {
  CommitGraphReport,
  CommitGraphState,
  CommitGraphWriteOutcome,
  CommitGraphWritePlan,
} from "../storage/types";
import {
  createCommitGraphWriteFlow,
  offersCommitGraphWrite,
  type CommitGraphWriteStep,
} from "./commitGraphWrite";

function report(state: CommitGraphState): CommitGraphReport {
  return { state, layout: "none", layers: 0, commits: null, bloom: null, head_covered: null, note: null };
}

const PLAN: CommitGraphWritePlan = {
  argv: ["git", "commit-graph", "write", "--reachable", "--changed-paths"],
  common_dir: "/repo/.git",
  state: "missing",
  timeout_secs: 600,
};

const OUTCOME: CommitGraphWriteOutcome = {
  cancelled: false,
  joined: false,
  elapsed_ms: 12,
  note: null,
  report: report("present"),
};

function harness(overrides: Partial<Record<string, (args: Record<string, unknown>) => unknown>> = {}) {
  const calls: Array<{ command: string; args: Record<string, unknown> }> = [];
  const invoke = vi.fn(async (command: string, args: Record<string, unknown>) => {
    calls.push({ command, args });
    const handler = overrides[command];
    if (handler) return handler(args);
    if (command === "cmd_commit_graph_write_plan") return PLAN;
    if (command === "cmd_commit_graph_write") return { policy: {}, output: OUTCOME };
    if (command === "cmd_commit_graph_write_cancel") return true;
    throw new Error(`unexpected ${command}`);
  });
  const onWritten = vi.fn();
  const flow = createCommitGraphWriteFlow({
    repoPath: () => "/repo",
    invoke: invoke as never,
    onWritten,
  });
  let step: CommitGraphWriteStep = { phase: "idle" };
  flow.subscribe((next) => (step = next));
  return { flow, calls, onWritten, step: () => step };
}

describe("offersCommitGraphWrite", () => {
  it.each(["missing", "stale", "no_bloom"] as const)("offers the button for %s", (state) => {
    expect(offersCommitGraphWrite(report(state))).toBe(true);
  });

  it.each(["present", "no_history", "unknown"] as const)("hides the button for %s", (state) => {
    expect(offersCommitGraphWrite(report(state))).toBe(false);
  });

  it("hides the button while a write is already running", () => {
    expect(offersCommitGraphWrite({ ...report("missing"), write_in_flight: true })).toBe(false);
    expect(offersCommitGraphWrite({ ...report("missing"), write_in_flight: false })).toBe(true);
  });

  it("hides the button for a payload from before the detector", () => {
    expect(offersCommitGraphWrite(undefined)).toBe(false);
    expect(offersCommitGraphWrite(null)).toBe(false);
  });
});

describe("the write flow", () => {
  it("shows the exact argv and directory, and writes nothing, until confirmed", async () => {
    const { flow, calls, step } = harness();
    await flow.open();
    expect(step()).toEqual({ phase: "confirming", plan: PLAN });
    expect(calls.map((call) => call.command)).toEqual(["cmd_commit_graph_write_plan"]);
  });

  it("does nothing when confirm is called without a preview", async () => {
    const { flow, calls, step } = harness();
    await flow.confirm();
    expect(step()).toEqual({ phase: "idle" });
    expect(calls).toEqual([]);
  });

  it("echoes the previewed argv back on confirm, then reports the outcome", async () => {
    const { flow, calls, onWritten, step } = harness();
    await flow.open();
    await flow.confirm();
    expect(calls[1]).toEqual({
      command: "cmd_commit_graph_write",
      args: { repoPath: "/repo", confirmedArgv: PLAN.argv },
    });
    expect(step()).toEqual({ phase: "done", outcome: OUTCOME });
    expect(onWritten).toHaveBeenCalledWith(OUTCOME);
  });

  it("a second confirm after the write ran does not write again", async () => {
    const { flow, calls } = harness();
    await flow.open();
    await flow.confirm();
    await flow.confirm();
    expect(calls.filter((call) => call.command === "cmd_commit_graph_write")).toHaveLength(1);
  });

  it("backing out of the preview writes nothing and calls nothing more", async () => {
    const { flow, calls, step } = harness();
    await flow.open();
    await flow.cancel();
    expect(step()).toEqual({ phase: "idle" });
    await flow.confirm();
    expect(calls.map((call) => call.command)).toEqual(["cmd_commit_graph_write_plan"]);
  });

  it("a preview that arrives after the person backed out is dropped", async () => {
    let release!: (plan: CommitGraphWritePlan) => void;
    const { flow, step } = harness({
      cmd_commit_graph_write_plan: () => new Promise((resolve) => (release = resolve)),
    });
    const opening = flow.open();
    await flow.cancel();
    release(PLAN);
    await opening;
    expect(step()).toEqual({ phase: "idle" });
  });

  it("asks the backend to stop a running write", async () => {
    let finish!: (value: unknown) => void;
    const { flow, calls, step } = harness({
      cmd_commit_graph_write: () => new Promise((resolve) => (finish = resolve)),
    });
    await flow.open();
    const running = flow.confirm();
    expect(step()).toMatchObject({ phase: "running", cancelling: false });
    await flow.cancel();
    expect(step()).toMatchObject({ phase: "running", cancelling: true });
    expect(calls.at(-1)).toEqual({ command: "cmd_commit_graph_write_cancel", args: { repoPath: "/repo" } });
    finish({ policy: {}, output: { ...OUTCOME, cancelled: true } });
    await running;
    expect(step()).toMatchObject({ phase: "done", outcome: { cancelled: true } });
  });

  it("shows a refusal as a failure and offers nothing to retry blindly", async () => {
    const { flow, calls, step } = harness({
      cmd_commit_graph_write_plan: () => {
        throw new Error("Repository trust is required");
      },
    });
    await flow.open();
    expect(step()).toEqual({ phase: "failed", message: "Repository trust is required" });
    await flow.confirm();
    expect(calls.map((call) => call.command)).toEqual(["cmd_commit_graph_write_plan"]);
    flow.dismiss();
    expect(step()).toEqual({ phase: "idle" });
  });

  it("a remounted flow shows a write found running, stops it, and leaves when it ends", async () => {
    const { flow, calls, step } = harness();
    flow.syncInFlight(true);
    expect(step()).toEqual({ phase: "running", cancelling: false });
    // No fresh write is offered from here: open() does nothing while running.
    await flow.open();
    expect(calls).toEqual([]);
    await flow.cancel();
    expect(calls).toEqual([{ command: "cmd_commit_graph_write_cancel", args: { repoPath: "/repo" } }]);
    flow.syncInFlight(false);
    expect(step()).toEqual({ phase: "idle" });
  });

  it("the scan never ends a write this flow started itself", async () => {
    let finish!: (value: unknown) => void;
    const { flow, step } = harness({
      cmd_commit_graph_write: () => new Promise((resolve) => (finish = resolve)),
    });
    await flow.open();
    const running = flow.confirm();
    flow.syncInFlight(false);
    expect(step()).toMatchObject({ phase: "running" });
    finish({ policy: {}, output: OUTCOME });
    await running;
    expect(step()).toMatchObject({ phase: "done" });
  });

  it("a failed write reports the failure and does not call onWritten", async () => {
    const { flow, onWritten, step } = harness({
      cmd_commit_graph_write: () => {
        throw new Error("git commit-graph write timed out after 600s");
      },
    });
    await flow.open();
    await flow.confirm();
    expect(step()).toMatchObject({ phase: "failed", message: expect.stringContaining("timed out") });
    expect(onWritten).not.toHaveBeenCalled();
  });
});
