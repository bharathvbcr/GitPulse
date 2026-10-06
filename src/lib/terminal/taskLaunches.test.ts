import { afterEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { awaitedTabIds, consumeTaskTerminal, consumeTaskTerminalRequest, enqueueTaskTerminal, requestFor, taskTerminalRequests } from "./taskLaunches";
import { MAX_LIVE_RUNS } from "../workbench/vocabulary";

afterEach(() => { for (const request of get(taskTerminalRequests)) consumeTaskTerminalRequest(request); });
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

  it("names exactly the open tabs whose panel would take a request", () => {
    // The dock hosts these. They must be the tabs `requestFor` matches inside
    // the panel, by the same identity rule: a tab hosted for a request it
    // will not consume spends a PTY slot on nothing, and a tab missed leaves
    // the agent never starting while the reader waits on the task sheet.
    const requests = [
      { runId: "w", repoPath: "/Work/Repo/.gitpulse/worktrees/fix-a1b2c3d4", provider: "claude" as const, title: "Fix" },
      { runId: "m", repoPath: "/work/other", provider: "codex" as const, title: "Other" },
    ];
    const tabs = [
      { id: "1", path: "/work/repo/.gitpulse/worktrees/fix-a1b2c3d4/" },
      { id: "2", path: "/work/repo" },
      { id: "3", path: "/work/other" },
      { id: "4", path: "" },
    ];
    const insensitive = { caseInsensitive: true };
    expect(awaitedTabIds(tabs, requests, insensitive)).toEqual(new Set(["1", "3"]));
    expect(awaitedTabIds(tabs, requests, { caseInsensitive: false })).toEqual(new Set(["3"]));
    expect(awaitedTabIds(tabs, [], insensitive)).toEqual(new Set());
    for (const tab of tabs) {
      expect(awaitedTabIds([tab], requests, insensitive).has(tab.id)).toBe(requestFor(requests, tab.path, insensitive) !== undefined);
    }
  });
});

describe("resumed conversations in the task terminal queue", () => {
  const SESSION = "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c11";
  it("keys a resumed conversation apart from the attempt's own terminal", () => {
    enqueueTaskTerminal({ runId: "a", repoPath: "/one", provider: "claude", title: "First" });
    enqueueTaskTerminal({ runId: "a", repoPath: "/one", provider: "claude", title: "First", resume: { sessionId: SESSION, mode: "inspect" } });
    // The same conversation twice is one request, however its id is cased.
    enqueueTaskTerminal({ runId: "a", repoPath: "/one", provider: "claude", title: "First", resume: { sessionId: SESSION.toUpperCase(), mode: "inspect" } });
    expect(get(taskTerminalRequests)).toHaveLength(2);
    // Cancelling the attempt withdraws its terminal, not the reader's resume.
    consumeTaskTerminal("a");
    expect(get(taskTerminalRequests).map((request) => request.resume?.sessionId)).toEqual([SESSION]);
    consumeTaskTerminalRequest(get(taskTerminalRequests)[0]);
    expect(get(taskTerminalRequests)).toEqual([]);
  });

  it("refuses a resume Claude Code could not act on", () => {
    for (const [provider, sessionId] of [["codex", SESSION], ["claude", "run-1"], ["claude", `${SESSION}x`], ["claude", ""]] as const) {
      expect(() => enqueueTaskTerminal({ runId: "a", repoPath: "/one", provider, title: "First", resume: { sessionId, mode: "edit" } })).toThrow(/resumed/);
    }
    expect(get(taskTerminalRequests)).toEqual([]);
  });
});

describe("a session a reload left running, in the queue", () => {
  it("is keyed by its session, survives an attempt's withdrawal, and refuses a bad id", () => {
    enqueueTaskTerminal({ runId: "detached:term-1-a", repoPath: "/one", provider: "shell", title: "Shell", attach: { sessionId: "term-1-a" } });
    enqueueTaskTerminal({ runId: "detached:term-1-a", repoPath: "/one", provider: "shell", title: "Shell", attach: { sessionId: "term-1-a" } });
    expect(get(taskTerminalRequests)).toHaveLength(1);
    consumeTaskTerminal("detached:term-1-a");
    expect(get(taskTerminalRequests)).toHaveLength(1);
    for (const sessionId of ["", "a b", "x".repeat(129), "../x"]) {
      expect(() => enqueueTaskTerminal({ runId: "r", repoPath: "/one", provider: "shell", title: "Shell", attach: { sessionId } })).toThrow(/cannot be shown/);
    }
  });
});
