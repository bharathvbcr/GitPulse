import { describe, expect, it } from "vitest";
import { attemptTerminalView } from "./taskSessions";
import type { TerminalSessionRecord } from "../terminal/sessionRegistry";
import type { TaskTerminalRequest } from "../terminal/taskLaunches";

/**
 * The pane's join under arbitrary registry and queue contents.
 *
 * The registry is shared by every repository's panel and the queue by every
 * launch, so the pane sees whatever mixture the rest of the app produced:
 * records of other attempts, resumed conversations, adopted sessions, odd
 * statuses, requests for other runs and for reload attaches. These are the
 * properties that must hold for every such mixture, not for the handful a
 * unit test happens to build.
 */

function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), a | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const RUNS = ["a", "b", "c", ""];
const STATUSES = ["starting", "running", "exited", "", "Close failed; use Sessions to retry.", "x".repeat(400), "Still running — not shown since the window reloaded"];
const close = async () => {};

describe("attemptTerminalView under arbitrary mixtures", () => {
  it("holds its invariants for every generated registry and queue", { timeout: 20_000 }, () => {
    const seen = { sessions: 0, adopted: 0, resumed: 0, waitingAttempt: 0, waitingResumed: 0, capacity: 0, problem: 0 };
    for (let seed = 1; seed <= 400; seed += 1) {
      const random = mulberry32(seed);
      const pick = <T,>(items: readonly T[]) => items[Math.floor(random() * items.length)];
      const records: TerminalSessionRecord[] = Array.from({ length: Math.floor(random() * 12) }, (_, i) => {
        const link = random();
        return {
          key: random() < 0.15 ? `detached:term-${i}` : `tab-${seed}-${i}`,
          repoPath: "/work/x", label: "Claude Code", status: pick(STATUSES), close,
          ...(link < 0.45 ? { taskRunId: pick(RUNS) } : link < 0.75 ? { continuesRunId: pick(RUNS) } : {}),
        };
      });
      const requests: TaskTerminalRequest[] = Array.from({ length: Math.floor(random() * 6) }, () => {
        const kind = random();
        return {
          runId: pick(RUNS), repoPath: "/work/x", provider: "claude", title: "T",
          ...(kind < 0.3 ? { resume: { sessionId: "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c11", mode: "ask" as const } } : kind < 0.45 ? { attach: { sessionId: "term-9" } } : {}),
        };
      });
      const full = random() < 0.5;
      for (const runId of RUNS) {
        const view = attemptTerminalView(runId, records, requests, full);
        const where = `seed ${seed} run "${runId}"`;
        if (!runId) { expect(view, where).toEqual({ sessions: [], waiting: null }); continue; }
        // 1. Only this attempt's sessions, each exactly once.
        const expected = records.filter((r) => r.taskRunId === runId || r.continuesRunId === runId);
        expect(view.sessions.length, where).toBe(expected.length);
        expect(new Set(view.sessions.map((s) => s.record.key)).size, where).toBe(view.sessions.length);
        for (const s of view.sessions) {
          expect(s.role === "attempt" ? s.record.taskRunId : s.record.continuesRunId, where).toBe(runId);
          // 2. Every label is short enough for a row and never empty.
          expect(s.label.length, where).toBeGreaterThan(0);
          expect(s.label.length, where).toBeLessThanOrEqual(160);
          // 3. An adopted session is never called running here.
          if (s.record.key.startsWith("detached:")) expect(s.phase, where).toBe("adopted");
        }
        seen.sessions += view.sessions.length;
        seen.adopted += view.sessions.filter((s) => s.phase === "adopted").length;
        seen.resumed += view.sessions.filter((s) => s.role === "resumed").length;
        seen.problem += view.sessions.filter((s) => s.phase === "problem").length;
        if (view.waiting?.role === "attempt") seen.waitingAttempt += 1;
        if (view.waiting?.role === "resumed") seen.waitingResumed += 1;
        if (view.waiting?.reason === "capacity") seen.capacity += 1;
        // 4. The attempt's own sessions come before resumed ones.
        const roles = view.sessions.map((s) => s.role);
        expect(roles, where).toEqual([...roles].sort((x, y) => (x === y ? 0 : x === "attempt" ? -1 : 1)));
        // 5. Waiting exactly when a request of this run has no session of its role.
        const unserved = requests.filter((q) => q.runId === runId && !q.attach)
          .filter((q) => !view.sessions.some((s) => s.role === (q.resume ? "resumed" : "attempt")));
        expect(view.waiting !== null, where).toBe(unserved.length > 0);
        if (view.waiting) {
          expect(view.waiting.reason, where).toBe(full ? "capacity" : "checkout");
          if (unserved.some((q) => !q.resume)) expect(view.waiting.role, where).toBe("attempt");
        }
      }
    }
    // Not vacuous: every branch of the join was reached many times.
    for (const [branch, count] of Object.entries(seen)) expect(count, branch).toBeGreaterThan(20);
  });

  it("answers a large registry in linear time", () => {
    const records: TerminalSessionRecord[] = Array.from({ length: 50_000 }, (_, i) => ({
      key: `k${i}`, repoPath: "/w", label: "L", status: "running", close, taskRunId: i % 2 ? "a" : "b",
    }));
    const started = performance.now();
    const view = attemptTerminalView("a", records, [], false);
    expect(view.sessions).toHaveLength(25_000);
    expect(performance.now() - started).toBeLessThan(2_000);
  });
});
