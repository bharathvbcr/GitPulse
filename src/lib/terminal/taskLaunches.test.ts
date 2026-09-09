import { afterEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { consumeTaskTerminal, enqueueTaskTerminal, taskTerminalRequests } from "./taskLaunches";

afterEach(() => { for (const request of get(taskTerminalRequests)) consumeTaskTerminal(request.runId); });
describe("task terminal requests", () => {
  it("deduplicates by attempt across repositories and bounds the pending queue", () => {
    enqueueTaskTerminal({ runId: "a", repoPath: "/one", provider: "claude", title: "First" });
    enqueueTaskTerminal({ runId: "a", repoPath: "/other", provider: "codex", title: "Changed" });
    enqueueTaskTerminal({ runId: "b", repoPath: "/two", provider: "codex", title: "Second" });
    expect(get(taskTerminalRequests)).toHaveLength(2);
    expect(get(taskTerminalRequests)[0].repoPath).toBe("/one");
    expect(() => enqueueTaskTerminal({ runId: "c", repoPath: "/three", provider: "claude", title: "Third" })).toThrow("Two task terminals");
    consumeTaskTerminal("a"); consumeTaskTerminal("missing");
    expect(get(taskTerminalRequests).map((request) => request.runId)).toEqual(["b"]);
    enqueueTaskTerminal({ runId: "c", repoPath: "/three", provider: "claude", title: "Third" });
    expect(get(taskTerminalRequests)).toHaveLength(2);
  });
});
