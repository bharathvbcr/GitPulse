import { describe, expect, it, vi } from "vitest";
import type { CommitGraphWritePlan } from "../storage/types";
import { createCommitGraphWriteFlow, type CommitGraphWriteStep } from "./commitGraphWrite";

const PLAN: CommitGraphWritePlan = {
  argv: ["git", "commit-graph", "write", "--reachable", "--changed-paths"],
  common_dir: "/a/.git",
  state: "missing",
  timeout_secs: 600,
};

const OUTCOME = {
  cancelled: false,
  joined: false,
  elapsed_ms: 1,
  report: { state: "present", layout: "single", layers: 1, commits: 1, bloom: true, head_covered: true, note: null },
};

type Handler = (args: Record<string, unknown>) => unknown;

function build(repo: { path: string }, handlers: Partial<Record<string, Handler>> = {}) {
  const calls: Array<{ command: string; args: Record<string, unknown> }> = [];
  const invoke = vi.fn(async (command: string, args: Record<string, unknown>) => {
    calls.push({ command, args });
    const handler = handlers[command];
    if (handler) return handler(args);
    if (command === "cmd_commit_graph_write_plan") return PLAN;
    if (command === "cmd_commit_graph_write") return { policy: {}, output: OUTCOME };
    if (command === "cmd_commit_graph_write_cancel") return true;
    throw new Error(`unexpected ${command}`);
  });
  const flow = createCommitGraphWriteFlow({ repoPath: () => repo.path, invoke: invoke as never });
  let step: CommitGraphWriteStep = { phase: "idle" };
  flow.subscribe((next) => (step = next));
  return { flow, calls, step: () => step };
}

const deferred = <T>() => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
};

describe("adv: commit-graph write flow", () => {
  it("a double click on confirm sends one write", async () => {
    const gate = deferred<unknown>();
    const run = build({ path: "/a" }, { cmd_commit_graph_write: () => gate.promise });
    await run.flow.open();
    const first = run.flow.confirm();
    const second = run.flow.confirm();
    gate.resolve({ policy: {}, output: OUTCOME });
    await Promise.all([first, second]);
    expect(run.calls.filter((c) => c.command === "cmd_commit_graph_write")).toHaveLength(1);
  });

  it("a double click on open sends one plan", async () => {
    const gate = deferred<unknown>();
    const t = build({ path: "/a" }, { cmd_commit_graph_write_plan: () => gate.promise });
    const first = t.flow.open();
    const second = t.flow.open();
    gate.resolve(PLAN);
    await Promise.all([first, second]);
    expect(t.calls.filter((c) => c.command === "cmd_commit_graph_write_plan")).toHaveLength(1);
  });

  it("a plan gone stale (graph appeared) fails closed with the backend's refusal", async () => {
    const t = build(
      { path: "/a" },
      { cmd_commit_graph_write: () => Promise.reject(new Error("No commit-graph write is offered here")) },
    );
    await t.flow.open();
    await t.flow.confirm();
    expect(t.step().phase).toBe("failed");
  });

  it("adv: a Stop that the backend answers `false` (no write registered yet) is not a permanent 'Stopping...'", async () => {
    const gate = deferred<unknown>();
    const t = build(
      { path: "/a" },
      {
        cmd_commit_graph_write: () => gate.promise,
        cmd_commit_graph_write_cancel: () => false,
      },
    );
    await t.flow.open();
    const running = t.flow.confirm();
    await t.flow.cancel();
    const step = t.step();
    // The stop did nothing (the write had not registered its cancel flag), and
    // the write runs on for up to 600 s. The person must be able to stop again.
    expect(step.phase === "running" && step.cancelling).toBe(false);
    gate.resolve({ policy: {}, output: OUTCOME });
    await running;
  });

  it("adv: a confirm after the repository changed does not write the other repository", async () => {
    const repo = { path: "/a" };
    const t = build(repo);
    await t.flow.open(); // previewed for /a (common_dir /a/.git)
    repo.path = "/b";
    await t.flow.confirm();
    const write = t.calls.find((c) => c.command === "cmd_commit_graph_write");
    expect(write?.args.repoPath).not.toBe("/b");
  });

  it("adv: a flow built after a remount can stop a write the old flow started", async () => {
    // The old component was destroyed mid-write (onDestroy only unsubscribes);
    // the backend write keeps running for up to 600 s. The new flow starts idle,
    // and cancel() on idle does nothing, so no UI path can reach the stop.
    const t = build({ path: "/a" });
    await t.flow.cancel();
    expect(t.calls.some((c) => c.command === "cmd_commit_graph_write_cancel")).toBe(true);
  });
});
