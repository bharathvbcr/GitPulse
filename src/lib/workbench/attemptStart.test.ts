import { describe, expect, it } from "vitest";
import { attemptStage, monitorAttempt, taskAgentSummaries, type MonitorContext } from "./taskSessions";
import type { TaskRun } from "./client";
import type { AttemptNotice, TaskTerminalRequest } from "../terminal/taskLaunches";
import type { TerminalSessionRecord } from "../terminal/sessionRegistry";
import type { SessionActivity } from "../terminal/sessionActivity";

/*
 * What an attempt's row says about its start, joined from what this window
 * recorded (`attemptNotices`) with the store's run and the renderer registry.
 * The defect: a spawn the host refused (CLI missing or too old) was said only
 * inside the hidden task tab, and the failed start released its registry
 * slot, so the row read a quiet "Not connected…" and the toast had already
 * said "started".
 */

const NOW = 1_800_000_000_000;
const SECONDS = NOW / 1000;
const run = (fields: Partial<TaskRun> & { id: string }): TaskRun => ({
  revision: 1, updated_at: SECONDS, kind: "external_terminal", task_id: "t1", source_revision: 1, task_title: "Fix",
  repository_id: "r1", provider: "claude", permission_mode: "ask", state: "prepared", cwd: "/work/repo", created_at: SECONDS,
  expires_at: SECONDS + 300, session_id: null, exit_code: null, reason: "", outcome_uncertain: false, ...fields,
});
const notice = (runId: string, phase: AttemptNotice["phase"], text: string): AttemptNotice =>
  ({ runId, phase, text, tone: phase === "failed" ? "error" : phase === "running" ? "ok" : "progress", at: NOW });
const record = (fields: Partial<TerminalSessionRecord> & { key: string }): TerminalSessionRecord =>
  ({ repoPath: "/work/repo", label: "Claude Code", status: "running", close: async () => {}, ...fields });
const request = (fields: Partial<TaskTerminalRequest> = {}): TaskTerminalRequest =>
  ({ runId: "a", repoPath: "/work/repo", provider: "claude", title: "Fix", ...fields });
const context = (fields: Partial<MonitorContext> = {}): MonitorContext => ({
  records: [], requests: [], capacityFull: false, activity: () => undefined, pending: () => undefined, now: NOW, clock: NOW, ...fields,
});

describe("a start that failed out of sight", () => {
  it("is an error on the attempt's row, with its cause — not a quiet 'not connected'", () => {
    const cause = "Claude Code is not installed or not on PATH (claude: command not found).";
    const row = monitorAttempt(run({ id: "a" }), context({ notices: (id) => (id === "a" ? notice("a", "failed", cause) : undefined) }));
    expect(row.failure).toBe(cause);
    expect(row.tone).toBe("error");
    expect(row.unstarted).toBe(false);
    expect(attemptStage(row, NOW)).toEqual({ stage: "failed", label: "Failed to start", tone: "bad" });
    // The board's card says the same: it asks for the reader.
    expect(taskAgentSummaries([run({ id: "a" })], context({ notices: () => notice("a", "failed", cause) })).get("t1"))
      .toEqual({ working: 1, asking: 1, tone: "error" });
  });

  it("stops being the row's story once anything has replaced it", () => {
    const failed = () => notice("a", "failed", "refused");
    // A session of its own (Show terminal retried, and it is starting).
    expect(monitorAttempt(run({ id: "a" }), context({ notices: failed, records: [record({ key: "k", taskRunId: "a", status: "starting" })] })).failure).toBeNull();
    // A request waiting again.
    expect(monitorAttempt(run({ id: "a" }), context({ notices: failed, requests: [request()] })).failure).toBeNull();
    // A managed attempt the store now records running.
    expect(monitorAttempt(run({ id: "a", kind: "managed", state: "running", session_id: "m" }), context({ notices: failed })).failure).toBeNull();
    // A terminal attempt the store saw claimed: the process exists, and only
    // this window's reply was lost. That is "not connected here", which
    // Show terminal reconnects — not a failed start.
    expect(monitorAttempt(run({ id: "a", state: "running" }), context({ notices: failed }))).toMatchObject({ failure: null, disconnected: true });
    // Progress and success are not failures.
    expect(monitorAttempt(run({ id: "a" }), context({ notices: () => notice("a", "running", "ok") })).failure).toBeNull();
  });

  it("is unchanged for a surface that passes no notices", () => {
    const row = monitorAttempt(run({ id: "a" }), context());
    expect(row).toMatchObject({ failure: null, unstarted: true });
  });
});

describe("attemptStage — one ladder for every row", () => {
  const stage = (r: TaskRun, fields: Partial<MonitorContext> = {}) => attemptStage(monitorAttempt(r, context(fields)), NOW);
  const asking: SessionActivity = { lastOutputAt: NOW, title: null, attention: { kind: "needs-you", label: "Permission", detail: null, at: NOW } };

  it("walks Starting agent → Running → Needs you → Exited / Failed / Expired", () => {
    expect(stage(run({ id: "a" }), { requests: [request()] }).label).toBe("Starting agent");
    expect(stage(run({ id: "a" }), { records: [record({ key: "k", taskRunId: "a", status: "starting" })] }).label).toBe("Starting agent");
    expect(stage(run({ id: "a", state: "starting" }), { records: [record({ key: "k", taskRunId: "a", status: "running" })] }).label).toBe("Starting agent");
    expect(stage(run({ id: "a", state: "running" }), { records: [record({ key: "k", taskRunId: "a", sessionId: "s" })] }).label).toBe("Running");
    expect(stage(run({ id: "a", state: "running" }), { records: [record({ key: "k", taskRunId: "a", sessionId: "s" })], activity: () => asking }))
      .toEqual({ stage: "needs-you", label: "Needs you", tone: "ask" });
    expect(stage(run({ id: "a", state: "exited", exit_code: 0 })).label).toBe("Exited");
    expect(stage(run({ id: "a", state: "failed" })).tone).toBe("bad");
    expect(stage(run({ id: "a", expires_at: SECONDS - 1 }))).toEqual({ stage: "expired", label: "Expired", tone: "done" });
  });

  it("names trust as what a start waits on, and asks for the reader", () => {
    const untrusted = (path: string) => path === "/work/repo";
    const row = monitorAttempt(run({ id: "a" }), context({ requests: [request()], trustPending: untrusted }));
    expect(row.view.waiting?.reason).toBe("trust");
    expect(attemptStage(row, NOW)).toEqual({ stage: "waiting", label: "Needs trust", tone: "ask" });
  });

  it("never calls an attempt running here when no session here is attached", () => {
    expect(stage(run({ id: "a", state: "running" })).label).toBe("Running elsewhere");
    expect(stage(run({ id: "a" })).label).toBe("Not started");
  });
});
