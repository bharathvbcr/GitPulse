import { describe, expect, it } from "vitest";
import { DEFAULT_FAN_OUT } from "../async/pool";
import { loadWorktreeTasks } from "./bindings";

function deferredCalls() {
  let inFlight = 0;
  let peak = 0;
  const call = async <T,>(_command: string, args: { repoPath: string; worktreePath: string }): Promise<T> => {
    inFlight += 1;
    peak = Math.max(peak, inFlight);
    await new Promise((resolve) => setTimeout(resolve, 5));
    inFlight -= 1;
    if (args.worktreePath.endsWith("broken")) throw new Error("ledger is locked");
    return (args.worktreePath.endsWith("bound") ? `TASK-${args.worktreePath}` : null) as T;
  };
  return { call, peak: () => peak };
}

describe("loadWorktreeTasks", () => {
  it("reads several worktrees at once, never more than the fan-out", async () => {
    const { call, peak } = deferredCalls();
    const paths = Array.from({ length: 10 }, (_, i) => `/wt/${i}`);
    await loadWorktreeTasks(call, "/repo", paths);
    expect(peak()).toBeGreaterThan(1);
    expect(peak()).toBeLessThanOrEqual(DEFAULT_FAN_OUT);
  });

  it("keeps the given order and fails only the worktree whose read failed", async () => {
    const { call } = deferredCalls();
    const reads = await loadWorktreeTasks(call, "/repo", ["/wt/bound", "/wt/broken", "/wt/free"]);
    expect(reads).toEqual([
      { path: "/wt/bound", taskId: "TASK-/wt/bound", ok: true },
      { path: "/wt/broken", taskId: null, ok: false, detail: expect.stringContaining("ledger is locked") },
      { path: "/wt/free", taskId: null, ok: true },
    ]);
  });

  it("asks nothing for no worktrees", async () => {
    const { call, peak } = deferredCalls();
    expect(await loadWorktreeTasks(call, "/repo", [])).toEqual([]);
    expect(peak()).toBe(0);
  });
});
