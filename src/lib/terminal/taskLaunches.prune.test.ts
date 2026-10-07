import { afterEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  attemptNotices,
  awaitedTabIds,
  clearAttemptNotice,
  consumeTaskTerminalRequest,
  enqueueTaskTerminal,
  noteAttempt,
  pruneTaskTerminals,
  taskTerminalRequests,
} from "./taskLaunches";
import { MAX_LIVE_RUNS } from "../workbench/vocabulary";

const NOW = 1_800_000_000_000;
const SECONDS = NOW / 1000;
const SESSION = "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c11";

afterEach(() => {
  for (const request of get(taskTerminalRequests)) consumeTaskTerminalRequest(request);
  for (const runId of get(attemptNotices).keys()) clearAttemptNotice(runId);
});

const queue = (runId: string) => enqueueTaskTerminal({ runId, repoPath: `/work/${runId}`, provider: "claude", title: runId });

describe("pruneTaskTerminals", () => {
  it("drops a request whose run, as read, can no longer take a terminal", () => {
    for (const id of ["ended", "cancelled", "expired", "unresolved", "live", "starting", "running"]) queue(id);
    const dropped = pruneTaskTerminals([
      { id: "ended", state: "exited", expires_at: SECONDS + 100 },
      { id: "cancelled", state: "cancelled", expires_at: SECONDS + 100 },
      { id: "expired", state: "prepared", expires_at: SECONDS - 1 },
      { id: "unresolved", state: "unresolved", expires_at: SECONDS + 100 },
      { id: "live", state: "prepared", expires_at: SECONDS + 100 },
      { id: "starting", state: "starting", expires_at: SECONDS + 100 },
      { id: "running", state: "running", expires_at: SECONDS + 100 },
    ], NOW);
    expect(dropped.sort()).toEqual(["cancelled", "ended", "expired", "unresolved"]);
    expect(get(taskTerminalRequests).map((request) => request.runId)).toEqual(["live", "starting", "running"]);
  });

  it("never judges a run the read did not return, nor a resume or a reattachment", () => {
    queue("unread");
    enqueueTaskTerminal({ runId: "ended", repoPath: "/work/x", provider: "claude", title: "x", resume: { sessionId: SESSION, mode: "ask", runId: "ended" } });
    enqueueTaskTerminal({ runId: "ended", repoPath: "/work/x", provider: "shell", title: "x", attach: { sessionId: "term-1" } });
    expect(pruneTaskTerminals([{ id: "ended", state: "exited", expires_at: 0 }], NOW)).toEqual([]);
    expect(get(taskTerminalRequests)).toHaveLength(3);
  });

  it("drops what a dead run's row said about a start in progress, and keeps why it failed", () => {
    noteAttempt("a", "waiting", "Waiting for a to open.", NOW);
    noteAttempt("b", "failed", "claude: command not found", NOW);
    pruneTaskTerminals([{ id: "a", state: "exited", expires_at: 0 }, { id: "b", state: "exited", expires_at: 0 }], NOW);
    expect([...get(attemptNotices).keys()]).toEqual(["b"]);
  });
});

describe("attemptNotices", () => {
  it("keeps one latest fact per run, bounded, with a bounded text", () => {
    noteAttempt("a", "starting", "Starting…", NOW);
    noteAttempt("a", "running", "Running", NOW + 1);
    expect(get(attemptNotices).get("a")).toMatchObject({ phase: "running", tone: "ok" });
    noteAttempt("b", "failed", "x".repeat(5000), NOW);
    expect(get(attemptNotices).get("b")?.text.length).toBeLessThanOrEqual(400);
    for (let i = 0; i < MAX_LIVE_RUNS * 3; i += 1) noteAttempt(`r${i}`, "starting", "…", NOW);
    expect(get(attemptNotices).size).toBe(MAX_LIVE_RUNS * 2);
    // Oldest evicted first.
    expect(get(attemptNotices).has("a")).toBe(false);
    expect(get(attemptNotices).has(`r${MAX_LIVE_RUNS * 3 - 1}`)).toBe(true);
  });
});

describe("awaitedTabIds and trust", () => {
  it("hosts no panel for a checkout that opened waiting to be trusted", () => {
    // A background open no longer asks (deferTrust), so the first sign of an
    // untrusted checkout is a tab that says so. Hosting it would start the
    // agent in a repository nobody agreed to.
    const requests = [{ runId: "a", repoPath: "/work/a", provider: "claude" as const, title: "a" }];
    const options = { caseInsensitive: false };
    expect([...awaitedTabIds([{ id: "t", path: "/work/a", trustRequired: true }], requests, options)]).toEqual([]);
    expect([...awaitedTabIds([{ id: "t", path: "/work/a", trustRequired: false }], requests, options)]).toEqual(["t"]);
  });
});
