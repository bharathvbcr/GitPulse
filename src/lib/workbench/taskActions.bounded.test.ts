import { readFileSync } from "node:fs";
import { afterEach, describe, expect, it, vi } from "vitest";
import { bounded, TASK_ACTION_TIMEOUT_MS, WORKTREE_PREPARE_TIMEOUT_MS } from "./taskActions";

/**
 * A worktree attempt is prepared in one host call that also runs the
 * repository's setup (15 minutes per command). The renderer used to give up
 * after 30 s and invite a retry, which the host then refused as "still
 * setting up its worktree".
 */
afterEach(() => vi.useRealTimers());

describe("bounded", () => {
  it("keeps the 30 s bound for ordinary task actions", async () => {
    vi.useFakeTimers();
    const never = new Promise<never>(() => {});
    const outcome = bounded(never).then(() => "done", (error: Error) => error.message);
    await vi.advanceTimersByTimeAsync(TASK_ACTION_TIMEOUT_MS + 1);
    expect(await outcome).toMatch(/timed out/);
  });

  it("waits out a long worktree setup when given the setup's bound", async () => {
    vi.useFakeTimers();
    let finish: (value: string) => void = () => {};
    const setup = new Promise<string>((resolve) => { finish = resolve; });
    const outcome = bounded(setup, WORKTREE_PREPARE_TIMEOUT_MS).then((value) => value, (error: Error) => error.message);
    // Ten minutes of post_create: past the old 30 s cap, inside one hook's 15.
    await vi.advanceTimersByTimeAsync(10 * 60_000);
    finish("prepared");
    expect(await outcome).toBe("prepared");
    expect(WORKTREE_PREPARE_TIMEOUT_MS).toBeGreaterThanOrEqual(15 * 60_000);
  });
});

describe("TaskHandoffForm", () => {
  it("gives a worktree preparation the setup's bound, and says setup is running", () => {
    const source = readFileSync(new URL("../components/TaskHandoffForm.svelte", import.meta.url), "utf8");
    expect(source).toContain("bounded(prepareTaskRun(preparation), preparation.worktree ? WORKTREE_PREPARE_TIMEOUT_MS : undefined)");
    expect(source).toContain("running this repository's setup");
  });
});
