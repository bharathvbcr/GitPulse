import { afterEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { consumeTaskTerminal, enqueueTaskTerminal, requestFor, taskTerminalRequests } from "./taskLaunches";
import { MAX_LIVE_RUNS } from "../workbench/vocabulary";

afterEach(() => { for (const request of get(taskTerminalRequests)) consumeTaskTerminal(request.runId); });
describe("task terminal requests", () => {
  it("deduplicates by attempt across repositories", () => {
    enqueueTaskTerminal({ runId: "a", repoPath: "/one", provider: "claude", title: "First" });
    enqueueTaskTerminal({ runId: "a", repoPath: "/other", provider: "codex", title: "Changed" });
    enqueueTaskTerminal({ runId: "b", repoPath: "/two", provider: "codex", title: "Second" });
    expect(get(taskTerminalRequests)).toHaveLength(2);
    expect(get(taskTerminalRequests)[0].repoPath).toBe("/one");
    consumeTaskTerminal("a"); consumeTaskTerminal("missing");
    expect(get(taskTerminalRequests).map((request) => request.runId)).toEqual(["b"]);
  });

  it("holds as many waiting terminals as the store can hold attempts, and no more", () => {
    // The old bound was two, so a third concurrent task was refused here even
    // though the store had admitted it.
    for (let i = 0; i < MAX_LIVE_RUNS; i += 1) {
      enqueueTaskTerminal({ runId: `r${i}`, repoPath: `/repo/${i}`, provider: "claude", title: `Task ${i}` });
    }
    expect(get(taskTerminalRequests)).toHaveLength(MAX_LIVE_RUNS);
    expect(() => enqueueTaskTerminal({ runId: "over", repoPath: "/x", provider: "claude", title: "Over" })).toThrow(`${MAX_LIVE_RUNS} task terminals`);
    consumeTaskTerminal("r0");
    enqueueTaskTerminal({ runId: "over", repoPath: "/x", provider: "claude", title: "Over" });
    expect(get(taskTerminalRequests)).toHaveLength(MAX_LIVE_RUNS);
  });

  it("finds a request by checkout identity, not by the exact spelling of its path", () => {
    const requests = [
      { runId: "w", repoPath: "/Work/Repo/.gitpulse/worktrees/fix-a1b2c3d4", provider: "claude" as const, title: "Fix" },
      { runId: "m", repoPath: "/work/other", provider: "codex" as const, title: "Other" },
    ];
    const insensitive = { caseInsensitive: true };
    expect(requestFor(requests, "/work/repo/.gitpulse/worktrees/fix-a1b2c3d4/", insensitive)?.runId).toBe("w");
    expect(requestFor(requests, "/work/repo/.gitpulse/worktrees/fix-a1b2c3d4", { caseInsensitive: false })).toBeUndefined();
    expect(requestFor(requests, "/work/other", insensitive)?.runId).toBe("m");
    // A parent or sibling is not the same checkout.
    expect(requestFor(requests, "/work/repo", insensitive)).toBeUndefined();
    expect(requestFor(requests, null, insensitive)).toBeUndefined();
    expect(requestFor(requests, "", insensitive)).toBeUndefined();
  });
});
