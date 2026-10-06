import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { createBoardAgents, type BoardAgentsDeps } from "./boardAgents";
import { monitorAttempt, taskAgentSummaries, type MonitorContext } from "./taskSessions";
import { TASK_RUN_POLL_MS } from "./runPoll";
import type { TaskRun } from "./client";
import type { SessionActivity } from "../terminal/sessionActivity";
import type { TerminalSessionRecord } from "../terminal/sessionRegistry";

const NOW = 1_800_000_000_000;
const SECONDS = NOW / 1000;
const run = (fields: Partial<TaskRun> & { id: string }): TaskRun => ({
  revision: 1, updated_at: SECONDS, kind: "external_terminal", task_id: "t1", source_revision: 1, task_title: "Fix",
  repository_id: "r1", provider: "claude", permission_mode: "ask", state: "running", cwd: "/work/repo", created_at: SECONDS,
  expires_at: SECONDS + 600, session_id: null, exit_code: null, reason: "", outcome_uncertain: false, ...fields,
});
const record = (fields: Partial<TerminalSessionRecord> & { key: string }): TerminalSessionRecord =>
  ({ repoPath: "/work/repo", label: "Claude Code", status: "running", close: async () => {}, ...fields });
const context = (fields: Partial<MonitorContext> = {}): MonitorContext => ({
  records: [], requests: [], capacityFull: false, activity: () => undefined, pending: () => undefined, now: NOW, clock: NOW, ...fields,
});
const asking = (label = "Needs your permission"): SessionActivity =>
  ({ lastOutputAt: NOW, title: null, attention: { kind: "needs-you", label, detail: null, at: NOW } });

describe("taskAgentSummaries", () => {
  it("counts each task's working attempts and the ones that need the reader", () => {
    const runs = [
      run({ id: "a", task_id: "t1" }),
      run({ id: "b", task_id: "t1", kind: "managed", session_id: "managed-1" }),
      run({ id: "c", task_id: "t2" }),
      run({ id: "d", task_id: "t2", state: "exited" }),
      run({ id: "e", task_id: "t3", state: "prepared", expires_at: SECONDS - 1 }),
    ];
    const summaries = taskAgentSummaries(runs, context({
      records: [record({ key: "k-a", taskRunId: "a", sessionId: "term-a" }), record({ key: "k-c", taskRunId: "c", sessionId: "term-c" })],
      activity: (id) => (id === "term-a" ? asking() : undefined),
      pending: (id) => (id === "b" ? { count: 2, more: false } : undefined),
    }));
    expect(summaries.get("t1")).toEqual({ working: 2, asking: 2, tone: "needs-you" });
    expect(summaries.get("t2")).toEqual({ working: 1, asking: 0, tone: "quiet" });
    // Ended and expired attempts are not working on anything.
    expect(summaries.has("t3")).toBe(false);
  });

  it("judges an attempt exactly as the task's own pane does", () => {
    const disconnected = run({ id: "x" });
    expect(monitorAttempt(disconnected, context())).toMatchObject({ disconnected: true, tone: "problem" });
    expect(taskAgentSummaries([disconnected], context()).get("t1")).toEqual({ working: 1, asking: 0, tone: "problem" });
    const failing: SessionActivity = { lastOutputAt: NOW, title: null, attention: { kind: "error", label: "Stopped on an error", detail: null, at: NOW } };
    const ctx = context({ records: [record({ key: "k", taskRunId: "x", sessionId: "s" })], activity: () => failing });
    expect(taskAgentSummaries([disconnected], ctx).get("t1")).toEqual({ working: 1, asking: 1, tone: "error" });
  });
});

/** A fake host: reads resolve when the test says, timers fire when it says. */
function harness(fields: Partial<BoardAgentsDeps> = {}) {
  const timers: { run: () => void; ms: number; id: number }[] = [];
  let next = 0;
  let background = false;
  const reads: { resolve: (value: { runs: TaskRun[]; complete: boolean }) => void; reject: (cause: unknown) => void }[] = [];
  const deps: Partial<BoardAgentsDeps> = {
    listRuns: () => new Promise((resolve, reject) => { reads.push({ resolve, reject }); }),
    listPending: async () => ({ items: [], has_more: false }),
    clock: () => NOW,
    background: () => background,
    lag: () => 0,
    setTimer: (fn, ms) => { const id = ++next; timers.push({ run: fn, ms, id }); return id as unknown as ReturnType<typeof setTimeout>; },
    clearTimer: (handle) => { const index = timers.findIndex((timer) => timer.id === (handle as unknown as number)); if (index !== -1) timers.splice(index, 1); },
    ...fields,
  };
  const watch = createBoardAgents(deps);
  const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
  return {
    watch, timers, reads, flush,
    setBackground(value: boolean) { background = value; },
    async answer(runs: TaskRun[], complete = true) { reads.shift()?.resolve({ runs, complete }); await flush(); },
    async fail(cause: unknown) { reads.shift()?.reject(cause); await flush(); },
    async tick() { const timer = timers.shift(); timer?.run(); await flush(); },
  };
}

describe("createBoardAgents", () => {
  it("reads on start, keeps reading while an agent works, and stops when none does", async () => {
    const host = harness();
    host.watch.start();
    host.watch.start();
    expect(host.reads).toHaveLength(1);
    await host.answer([run({ id: "a" }), run({ id: "z", state: "exited" })]);
    const first = get(host.watch);
    expect(first.runs.map((item) => item.id)).toEqual(["a"]);
    expect(first.readAt).toBe(NOW);
    expect(host.timers).toHaveLength(1);
    expect(host.timers[0].ms).toBeGreaterThanOrEqual(TASK_RUN_POLL_MS);
    await host.tick();
    expect(host.reads).toHaveLength(1);
    await host.answer([]);
    expect(get(host.watch).runs).toEqual([]);
    expect(host.timers).toHaveLength(0);
  });

  it("does not poll behind the reader's back, and reads when the window comes forward", async () => {
    const host = harness();
    host.watch.start();
    host.setBackground(true);
    await host.answer([run({ id: "a" })]);
    expect(host.timers).toHaveLength(0);
    host.watch.wake();
    expect(host.reads).toHaveLength(0);
    host.setBackground(false);
    host.watch.wake();
    expect(host.reads).toHaveLength(1);
  });

  it("folds changes during a read into one more read, not one per change", async () => {
    const host = harness();
    host.watch.start();
    void host.watch.refresh();
    void host.watch.refresh();
    void host.watch.refresh();
    expect(host.reads).toHaveLength(1);
    await host.answer([run({ id: "a" })]);
    expect(host.reads).toHaveLength(1);
    await host.answer([run({ id: "a" }), run({ id: "b" })]);
    expect(get(host.watch).runs).toHaveLength(2);
    expect(host.reads).toHaveLength(0);
  });

  it("discards a read that finishes after stop, and a restart reads again", async () => {
    const host = harness();
    host.watch.start();
    host.watch.stop();
    host.watch.start();
    await host.answer([run({ id: "stale" })]);
    expect(get(host.watch).runs).toEqual([]);
    expect(host.reads).toHaveLength(1);
    await host.answer([run({ id: "fresh" })]);
    expect(get(host.watch).runs.map((item) => item.id)).toEqual(["fresh"]);
  });

  it("clears every card when a read fails, rather than keep claiming agents nobody checked", async () => {
    const host = harness();
    host.watch.start();
    await host.answer([run({ id: "a" })]);
    await host.tick();
    await host.fail(new Error("store unavailable"));
    const state = get(host.watch);
    expect(state.runs).toEqual([]);
    expect(state.error).toContain("store unavailable");
    expect(host.timers).toHaveLength(0);
  });

  it("reads requests only for managed attempts with a session, and omits a count it could not read", async () => {
    const asked: string[] = [];
    const host = harness({
      listPending: async (id) => { asked.push(id); if (id === "m2") throw new Error("denied"); return { items: [{ state: "pending", actionable: true }], has_more: false }; },
    });
    host.watch.start();
    await host.answer([
      run({ id: "m1", kind: "managed", session_id: "s1" }),
      run({ id: "m2", kind: "managed", session_id: "s2" }),
      run({ id: "m3", kind: "managed", session_id: null, state: "prepared" }),
      run({ id: "t1" }),
    ]);
    expect(asked.sort()).toEqual(["m1", "m2"]);
    const { pending } = get(host.watch);
    expect(pending.get("m1")).toEqual({ count: 1, more: false });
    expect(pending.has("m2")).toBe(false);
  });

  it("says when the read was a floor", async () => {
    const host = harness();
    host.watch.start();
    await host.answer([run({ id: "a" })], false);
    expect(get(host.watch).complete).toBe(false);
  });
});
