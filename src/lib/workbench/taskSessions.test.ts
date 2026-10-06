import { describe, expect, it } from "vitest";
import { ACTIVE_WINDOW_MS, GLANCE_RANK, agentGlance, asksForReader, attemptTerminalView, attemptUrgency, checkoutChanges, mostUrgent, pendingRequests, requestsLine, shortSpan, waitingLabel, type AttemptSession } from "./taskSessions";
import type { SessionActivity } from "../terminal/sessionActivity";
import type { TerminalSessionRecord } from "../terminal/sessionRegistry";
import type { TaskTerminalRequest } from "../terminal/taskLaunches";

const close = async () => {};
const record = (fields: Partial<TerminalSessionRecord> & { key: string }): TerminalSessionRecord =>
  ({ repoPath: "/work/a", label: "Claude Code", status: "running", close, ...fields });
const request = (fields: Partial<TaskTerminalRequest>): TaskTerminalRequest =>
  ({ runId: "a", repoPath: "/work/a", provider: "claude", title: "Fix", ...fields });
const SESSION = "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c11";

describe("attemptTerminalView", () => {
  it("lists the attempt's own session before a conversation resumed from it", () => {
    const view = attemptTerminalView("a", [
      record({ key: "r", continuesRunId: "a" }),
      record({ key: "x", taskRunId: "b" }),
      record({ key: "t", taskRunId: "a", status: "starting" }),
    ], [], false);
    expect(view.sessions.map((s) => [s.record.key, s.role, s.phase])).toEqual([["t", "attempt", "starting"], ["r", "resumed", "running"]]);
    expect(view.waiting).toBeNull();
  });

  it("says a session adopted after a reload is running but not shown, not that it runs here", () => {
    const [session] = attemptTerminalView("a", [record({ key: "detached:term-1", taskRunId: "a", status: "Still running" })], [], false).sessions;
    expect(session.phase).toBe("adopted");
    expect(session.label).toContain("not shown");
  });

  it("shows any other status as the message it is, bounded", () => {
    const [pending] = attemptTerminalView("a", [record({ key: "t", taskRunId: "a", status: "Task terminal launch is still pending." })], [], false).sessions;
    expect(pending).toMatchObject({ phase: "problem", label: "Task terminal launch is still pending." });
    const [long] = attemptTerminalView("a", [record({ key: "t", taskRunId: "a", status: "e".repeat(5000) })], [], false).sessions;
    expect(long.label.length).toBeLessThanOrEqual(160);
    const [blank] = attemptTerminalView("a", [record({ key: "t", taskRunId: "a", status: "  " })], [], false).sessions;
    expect(blank.label).toBe("Unknown terminal state");
  });

  it("reports a request no session has taken, and why it waits", () => {
    expect(attemptTerminalView("a", [], [request({})], false).waiting).toEqual({ role: "attempt", reason: "checkout" });
    expect(attemptTerminalView("a", [], [request({})], true).waiting).toEqual({ role: "attempt", reason: "capacity" });
    expect(attemptTerminalView("a", [], [request({ resume: { sessionId: SESSION, mode: "ask", runId: "a" } })], false).waiting)
      .toEqual({ role: "resumed", reason: "checkout" });
  });

  it("does not call a request waiting once its session exists", () => {
    expect(attemptTerminalView("a", [record({ key: "t", taskRunId: "a" })], [request({})], true).waiting).toBeNull();
    // The attempt's own session does not serve a resume request, nor the reverse.
    expect(attemptTerminalView("a", [record({ key: "t", taskRunId: "a" })], [request({ resume: { sessionId: SESSION, mode: "ask" } })], false).waiting)
      .toEqual({ role: "resumed", reason: "checkout" });
  });

  it("ignores another attempt's requests, a reload's attach requests, and an empty id", () => {
    expect(attemptTerminalView("a", [], [request({ runId: "b" })], false).waiting).toBeNull();
    expect(attemptTerminalView("a", [], [request({ attach: { sessionId: "term-1" } })], false).waiting).toBeNull();
    expect(attemptTerminalView("", [record({ key: "t", taskRunId: "" })], [request({ runId: "" })], false)).toEqual({ sessions: [], waiting: null });
  });

  it("prefers the attempt's own waiting request when both wait", () => {
    const view = attemptTerminalView("a", [], [request({ resume: { sessionId: SESSION, mode: "ask" } }), request({})], false);
    expect(view.waiting?.role).toBe("attempt");
  });
});

describe("waitingLabel", () => {
  it("sends the reader to the limit when slots are full, and to the checkout otherwise", () => {
    expect(waitingLabel({ role: "attempt", reason: "capacity" }, 32)).toContain("all 32 are in use");
    expect(waitingLabel({ role: "resumed", reason: "checkout" }, 32)).toMatch(/^The resumed conversation is waiting for its checkout/);
  });
});

describe("agentGlance", () => {
  const NOW = 10_000_000;
  const running = (phase: AttemptSession["phase"] = "running", label = "Running in this window"): AttemptSession =>
    ({ record: record({ key: "t", taskRunId: "a", sessionId: "term-1" }), role: "attempt", phase, label });
  const activity = (fields: Partial<SessionActivity>): SessionActivity => ({ lastOutputAt: null, title: null, attention: null, ...fields });

  it("leads with what the agent asked for, however recently it printed", () => {
    const glance = agentGlance(running(), activity({
      lastOutputAt: NOW - 100, title: "✳ Fix importer",
      attention: { kind: "needs-you", label: "Needs your permission", detail: "Bash: rm -rf build", at: NOW - 65_000 },
    }), NOW);
    expect(glance).toEqual({ tone: "needs-you", headline: "Needs your permission · 1m ago", detail: "Bash: rm -rf build", title: "✳ Fix importer" });
  });

  it("says output recency and nothing more when the agent has asked for nothing", () => {
    expect(agentGlance(running(), activity({ lastOutputAt: NOW - 200 }), NOW).headline).toBe("Output just now");
    expect(agentGlance(running(), activity({ lastOutputAt: NOW - 4_000 }), NOW)).toMatchObject({ tone: "active", headline: "Output 4s ago" });
    expect(agentGlance(running(), activity({ lastOutputAt: NOW - ACTIVE_WINDOW_MS }), NOW)).toMatchObject({ tone: "quiet", headline: "Quiet for 10s" });
    expect(agentGlance(running(), activity({ lastOutputAt: NOW - 3 * 3600_000 - 60_000 }), NOW).headline).toBe("Quiet for 3h 1m");
    expect(agentGlance(running(), undefined, NOW)).toMatchObject({ tone: "quiet", headline: "Running · no output yet" });
    // Never the words a timestamp cannot support.
    for (const ago of [0, 5_000, 600_000]) {
      expect(agentGlance(running(), activity({ lastOutputAt: NOW - ago }), NOW).headline).not.toMatch(/working|stuck|idle|done/i);
    }
  });

  it("lets a start or a problem speak before any activity", () => {
    const busy = activity({ attention: { kind: "needs-you", label: "Needs your input", detail: null, at: NOW } });
    expect(agentGlance(running("starting", "Starting…"), busy, NOW).tone).toBe("starting");
    expect(agentGlance(running("problem", "Close failed"), busy, NOW)).toMatchObject({ tone: "problem", headline: "Close failed" });
  });

  it("says what an agent the reader cannot see asked for, and why it is not on screen", () => {
    // The session adopted after a reload has no terminal in view, which is
    // exactly when a question from it most needs to reach the pane. This
    // used to answer "adopted" and drop the question.
    const adopted = running("adopted", "Running · not shown since the window reloaded");
    const busy = activity({ attention: { kind: "needs-you", label: "Needs your input", detail: "Which branch?", at: NOW - 2_000 } });
    expect(agentGlance(adopted, busy, NOW)).toMatchObject({
      tone: "needs-you", headline: "Needs your input · 2s ago · not shown since the window reloaded", detail: "Which branch?",
    });
    expect(agentGlance(adopted, activity({ lastOutputAt: NOW }), NOW)).toMatchObject({ tone: "adopted", headline: adopted.label });
  });

  it("orders the most urgent first, and a clock that ran backwards still renders", () => {
    const tones = Object.keys(GLANCE_RANK) as (keyof typeof GLANCE_RANK)[];
    const glances = tones.map((tone) => ({ tone, headline: tone, detail: null, title: null }));
    expect(mostUrgent(glances.slice().reverse())?.tone).toBe("needs-you");
    expect(mostUrgent([])).toBeNull();
    expect(agentGlance(running(), activity({ lastOutputAt: NOW + 50_000 }), NOW).headline).toBe("Output just now");
    expect(shortSpan(Number.NaN)).toBe("0s");
  });
});

describe("attemptUrgency", () => {
  const glance = (tone: keyof typeof GLANCE_RANK) => ({ tone, headline: tone, detail: null, title: null });
  it("takes the most urgent of the attempt's sessions and its own state", () => {
    expect(attemptUrgency([glance("quiet"), glance("finished")])).toBe("finished");
    expect(attemptUrgency([glance("active")], { pendingRequests: 2 })).toBe("needs-you");
    expect(attemptUrgency([], { disconnected: true })).toBe("problem");
    expect(attemptUrgency([], { waiting: true })).toBe("starting");
    expect(attemptUrgency([])).toBeNull();
    expect(attemptUrgency([glance("error")], { pendingRequests: 0 })).toBe("error");
  });
  it("puts only a request or an error on the reader's plate", () => {
    expect(asksForReader("needs-you")).toBe(true);
    expect(asksForReader("error")).toBe(true);
    for (const tone of ["finished", "signalled", "active", "quiet", "problem", "starting", "adopted", null] as const) expect(asksForReader(tone)).toBe(false);
  });
});

describe("checkoutChanges", () => {
  const tab = (fields: Record<string, unknown>) => ({ path: "/work/repo/.gitpulse/worktrees/fix-1", isLoading: false, error: null, trustRequired: false, changedCount: 4, currentBranch: "gitpulse/fix-1", ...fields });
  const opts = { caseInsensitive: true };

  it("reads the attempt's own worktree, matched by checkout identity", () => {
    expect(checkoutChanges("/Work/Repo/.gitpulse/worktrees/fix-1/", [tab({})], opts)).toEqual({ files: 4, branch: "gitpulse/fix-1", shared: false });
    expect(checkoutChanges("/work/repo", [tab({ path: "/work/repo" })], opts)).toMatchObject({ shared: true });
  });

  it("reports nothing rather than a zero it never read", () => {
    for (const fields of [{ isLoading: true }, { error: "denied" }, { trustRequired: true }, { changedCount: Number.NaN }, { changedCount: -1 }]) {
      expect(checkoutChanges("/work/repo/.gitpulse/worktrees/fix-1", [tab(fields)], opts), JSON.stringify(fields)).toBeNull();
    }
    expect(checkoutChanges("/work/elsewhere", [tab({})], opts)).toBeNull();
    expect(checkoutChanges("", [tab({ path: "" })], opts)).toBeNull();
  });
});

describe("pendingRequests", () => {
  it("counts only requests still waiting on the reader, and says when the page was full", () => {
    const items = [{ state: "pending", actionable: true }, { state: "pending", actionable: false }, { state: "decided", actionable: false }];
    expect(pendingRequests({ items, has_more: false })).toEqual({ count: 1, more: false });
    expect(pendingRequests({ items: [], has_more: true })).toEqual({ count: 0, more: true });
  });

  it("says a count only when there is one, as a floor when the page was full", () => {
    expect(requestsLine(undefined)).toBeNull();
    expect(requestsLine({ count: 0, more: false })).toBeNull();
    expect(requestsLine({ count: 1, more: false })).toEqual({ tone: "needs-you", text: "1 request is waiting for you" });
    expect(requestsLine({ count: 1, more: true })).toEqual({ tone: "needs-you", text: "1+ requests are waiting for you" });
    expect(requestsLine({ count: 3, more: false })?.text).toBe("3 requests are waiting for you");
    // A full page with nothing answerable on it: never "0+ waiting", never a request.
    const unread = requestsLine({ count: 0, more: true });
    expect(unread?.tone).not.toBe("needs-you");
    expect(unread?.text).not.toMatch(/^0/);
  });
});
