import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { countAgentsWaiting } from "../desktop/menuState";
import { interfaceStore } from "../stores/interfaceStore";
import { asksForReader as sharedAsks, type AttentionKind, type SessionActivity } from "../terminal/sessionActivity";
import type { TerminalSessionRecord } from "../terminal/sessionRegistry";
import { project, task, tasks, terminal } from "../agents/__tests__/fixtures";
import { asksForReader as paneAsks, attemptStage, monitorAttempt, taskAgentSummaries, type MonitorContext } from "./taskSessions";
import type { TaskRun } from "./client";

// ADVERSARY (W5a): every surface that counts "an agent is waiting for you" must
// give one answer for every state a session can be in and both values of the
// "count a terminal bell as waiting for you" setting. The setting is flipped
// through the interface store, the way the settings modal does, not through the
// module flag, so a store that failed to push the flag in would show here.

const KINDS: readonly (AttentionKind | null)[] = ["needs-you", "error", "signalled", "finished", null];
const NOW = 1_700_000_000;

const attempt = {
  id: "run-1", task_id: "t1", kind: "external_terminal", state: "running", session_id: "term-a",
  created_at: NOW, expires_at: NOW + 600, updated_at: NOW, revision: 1, cwd: "/work/a", exit_code: null,
  outcome_uncertain: false,
} as unknown as TaskRun;

const activityFor = (kind: AttentionKind | null): SessionActivity => ({
  lastOutputAt: null,
  title: null,
  attention: kind === null ? null : { kind, label: kind, detail: null, at: NOW },
});

const context = (kind: AttentionKind | null): MonitorContext => ({
  records: [{ key: "k", repoPath: "/work/a", label: "Claude Code", status: "running", close: async () => {}, taskRunId: "run-1", sessionId: "term-a" } as TerminalSessionRecord],
  requests: [], capacityFull: false,
  activity: () => activityFor(kind), pending: () => undefined, now: NOW, clock: NOW,
});

const surfaces = (kind: AttentionKind | null) => {
  const monitored = monitorAttempt(attempt, context(kind));
  return {
    rule: sharedAsks(kind),
    pane: kind === null ? false : paneAsks(kind),
    paneStage: attemptStage(monitored, NOW).stage === "needs-you",
    chip: (taskAgentSummaries([attempt], context(kind)).get("t1")?.asking ?? 0) > 0,
    tray: countAgentsWaiting(new Map([["s", activityFor(kind)]])) > 0,
    agentsTerminal: project({ terminals: [terminal({ cwd: "/repo/.claude/worktrees/alpha", attention: kind })] }).attention.needing > 0,
    agentsTask: project({ tasks: tasks({ tasks: [task({ tone: kind ?? "quiet" })] }) }).attention.needing > 0,
  };
};

describe("every surface counts a waiting agent by the one rule, for every state and both settings", () => {
  const saved = get(interfaceStore).bellAsksForReader;
  beforeEach(() => interfaceStore.setBellAsksForReader(true));
  afterEach(() => interfaceStore.setBellAsksForReader(saved));

  for (const setting of [true, false]) {
    for (const kind of KINDS) {
      it(`bell setting ${setting ? "on" : "off"}, attention ${kind ?? "none"}`, () => {
        interfaceStore.setBellAsksForReader(setting);
        const expected = kind === "needs-you" || kind === "error" || (kind === "signalled" && setting);
        const got = surfaces(kind);
        expect(got.rule, "sessionActivity.asksForReader").toBe(expected);
        expect(got.pane, "task pane rule").toBe(expected);
        expect(got.paneStage, "task pane stage").toBe(expected);
        expect(got.chip, "board chip").toBe(expected);
        expect(got.tray, "tray count").toBe(expected);
        // The Agents view also lists a finished agent as waiting for its next
        // instruction, which the other surfaces do not chase; every other state
        // agrees with them.
        const agents = kind === "finished" ? true : expected;
        expect(got.agentsTerminal, "Agents view, terminal row").toBe(agents);
        expect(got.agentsTask, "Agents view, task row").toBe(agents);
      });
    }
  }

  it("a hooked ask and a hook-less bell beside each other: off drops exactly the bell", () => {
    const two = (): number => {
      const runs = [attempt, { ...attempt, id: "run-2", session_id: "term-b" } as TaskRun];
      const records = ["run-1", "run-2"].map((runId, i) => ({ key: `k${i}`, repoPath: "/work/a", label: "Claude Code", status: "running", close: async () => {}, taskRunId: runId, sessionId: i ? "term-b" : "term-a" }) as TerminalSessionRecord);
      return taskAgentSummaries(runs, { ...context(null), records, activity: (id) => activityFor(id === "term-a" ? "signalled" : "needs-you") }).get("t1")?.asking ?? -1;
    };
    interfaceStore.setBellAsksForReader(true);
    expect(two()).toBe(2);
    interfaceStore.setBellAsksForReader(false);
    expect(two()).toBe(1);
  });

  it("an unchanged write of the setting leaves the flag where it was", () => {
    interfaceStore.setBellAsksForReader(false);
    interfaceStore.setBellAsksForReader(false);
    expect(sharedAsks("signalled")).toBe(false);
    interfaceStore.setBellAsksForReader(true);
    interfaceStore.setBellAsksForReader(true);
    expect(sharedAsks("signalled")).toBe(true);
  });
});
