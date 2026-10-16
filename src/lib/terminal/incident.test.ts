import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { createHostedClaudeStore, createResumers, type ResumeOutcome } from "./hostedClaude";
import {
  MAX_INCIDENT_SESSIONS,
  createIncidentController,
  describeIncident,
  parseIncident,
  planIncident,
  type IncidentNotice,
  type ResumeReport,
  type TerminalIncident,
  type TranscriptCheck,
} from "./incident";
import { describeResumeReport } from "./incidentWatch";
import type { TranscriptState } from "./transcripts";

const uuid = (i: number) => `00000000-0000-4000-8000-${String(i).padStart(12, "0")}`;

function incident(overrides: Partial<TerminalIncident> = {}): TerminalIncident {
  return {
    id: "inc-1",
    signal: "Terminated: 15",
    at_ms: 1_791_000_000_000,
    session_ids: ["term-1", "term-2", "term-3"],
    sender: { pid: 89829, name: "pkill" },
    sender_note: "launchd: sent by pkill[89829]",
    ...overrides,
  };
}

function hostTabs(
  count: number,
  opts: { withRecord?: (i: number) => boolean; outcome?: (i: number) => ResumeOutcome } = {},
) {
  const store = createHostedClaudeStore(null);
  const resumers = createResumers();
  const relaunched: string[] = [];
  for (let i = 1; i <= count; i++) {
    if (opts.withRecord && !opts.withRecord(i)) continue;
    store.begin({ sessionId: uuid(i), repoPath: "/r", startDir: null, mode: null, startedAt: i });
    store.bind(uuid(i), `term-${i}`);
    store.ended(uuid(i), "Terminated: 15");
    resumers.register(`term-${i}`, async () => {
      relaunched.push(`term-${i}`);
      return opts.outcome?.(i) ?? "started";
    });
  }
  return { store, resumers, relaunched };
}

/** A transcript check answering `state(i)` for the conversation `uuid(i)`; found for every one by default. */
function transcripts(state: (i: number) => TranscriptState = () => "found"): TranscriptCheck & { asked: string[][] } {
  const asked: string[][] = [];
  const check = async (ids: string[]) => {
    asked.push(ids);
    return new Map(ids.map((id) => {
      const i = Number(id.slice(-12));
      const answer = state(i);
      return [id, { state: answer, reason: answer === "unknown" ? "denied" : null }] as const;
    }));
  };
  return Object.assign(check, { asked });
}

function controller(parts: ReturnType<typeof hostTabs>, check: TranscriptCheck = transcripts()) {
  const notices: IncidentNotice[] = [];
  const done: ResumeReport[] = [];
  const failed: string[] = [];
  const c = createIncidentController({
    store: parts.store,
    resumers: parts.resumers,
    check,
    notify: (n) => notices.push(n),
    fail: (id) => failed.push(id),
    done: (report) => done.push(report),
  });
  return { c, notices, done, failed };
}

const report = (r: Partial<ResumeReport>): ResumeReport => ({ offered: 0, started: 0, deferred: 0, failed: 0, missing: 0, ...r });

describe("parseIncident", () => {
  it("reads the exact payload the backend emits", () => {
    const parsed = parseIncident(JSON.parse(JSON.stringify(incident())));
    expect(parsed).toEqual(incident());
  });

  it("accepts a null sender and drops duplicate session ids", () => {
    const parsed = parseIncident({ ...incident(), sender: null, session_ids: ["a", "a", "b"] });
    expect(parsed?.sender).toBeNull();
    expect(parsed?.session_ids).toEqual(["a", "b"]);
  });

  it("refuses payloads that are not incidents", () => {
    for (const bad of [
      null,
      "x",
      42,
      {},
      { ...incident(), id: "" },
      { ...incident(), signal: 15 },
      { ...incident(), at_ms: "now" },
      { ...incident(), session_ids: "term-1" },
    ]) {
      expect(parseIncident(bad)).toBeNull();
    }
  });

  it("drops session ids that are not text and bounds a huge list", () => {
    const many = Array.from({ length: 5000 }, (_, i) => `term-${i}`);
    const parsed = parseIncident({ ...incident(), session_ids: [1, null, "", ...many] });
    expect(parsed?.session_ids).toHaveLength(MAX_INCIDENT_SESSIONS);
    expect(parsed?.session_ids[0]).toBe("term-0");
  });

  it("keeps every session id the backend may send", () => {
    // Lockstep with the recorder: a smaller cap here silently drops sessions
    // the backend listed, and Resume N never offers them.
    const source = readFileSync(new URL("../../../src-tauri/src/terminal/incident.rs", import.meta.url), "utf8");
    const backend = /pub const MAX_MEMBERS: usize = (\d+);/.exec(source);
    expect(backend, "MAX_MEMBERS not found in incident.rs").not.toBeNull();
    expect(MAX_INCIDENT_SESSIONS).toBe(Number(backend?.[1]));
  });
});

describe("planIncident", () => {
  it("lists the tabs with a saved conversation and counts the rest", async () => {
    const { store } = hostTabs(3, { withRecord: (i) => i !== 2 });
    const check = transcripts();
    const plan = await planIncident(incident(), store, check);
    expect(plan.resumable).toEqual([
      { terminalId: "term-1", sessionId: uuid(1) },
      { terminalId: "term-3", sessionId: uuid(3) },
    ]);
    expect(plan.skipped).toBe(1);
    expect(plan.unknown).toBe(0);
    expect(check.asked).toEqual([[uuid(1), uuid(3)]]);
  });

  // S2-06: offered only when Claude Code saved it; unknown is said apart, absent never offered.
  it("offers only conversations whose transcript was found, and counts unknown apart from absent", async () => {
    const { store } = hostTabs(3);
    const plan = await planIncident(incident(), store, transcripts((i) => (i === 1 ? "found" : i === 2 ? "absent" : "unknown")));
    expect(plan.resumable).toEqual([{ terminalId: "term-1", sessionId: uuid(1) }]);
    expect(plan.skipped).toBe(1);
    expect(plan.unknown).toBe(1);
  });

  it("an incident naming sessions this page never hosted resumes nothing, and checks nothing", async () => {
    const store = createHostedClaudeStore(null);
    const check = transcripts();
    expect(await planIncident(incident(), store, check)).toEqual({ resumable: [], unknown: 0, skipped: 3 });
    expect(check.asked).toEqual([]);
  });

  it("two PTYs recorded against one conversation resume it once", async () => {
    const store = createHostedClaudeStore(null);
    store.begin({ sessionId: uuid(1), repoPath: "/r", startDir: null, mode: null, startedAt: 1 });
    store.bind(uuid(1), "term-1");
    const plan = await planIncident(incident({ session_ids: ["term-1", "term-1"] }), store, transcripts());
    expect(plan.resumable).toHaveLength(1);
  });
});

describe("the incident controller", () => {
  it("offers Resume N for the listed tabs with a saved conversation", async () => {
    const { c, notices } = controller(hostTabs(3));
    await c.handle(incident());
    expect(notices).toHaveLength(1);
    expect(notices[0].action?.label).toBe("Resume 3");
    expect(notices[0].message).toContain("sent by pkill (pid 89829)");
    expect(notices[0].message).toContain("3 terminal sessions ended by Terminated: 15");
  });

  it("says when conversations could not be checked, and does not offer them", async () => {
    const { c, notices } = controller(hostTabs(3), transcripts((i) => (i === 3 ? "unknown" : "found")));
    await c.handle(incident());
    expect(notices[0].action?.label).toBe("Resume 2");
    expect(notices[0].message).toContain("1 more could not be checked for a saved conversation");
  });

  it("says sender unknown rather than guessing, and offers nothing when nothing can resume", async () => {
    const { c, notices } = controller({ store: createHostedClaudeStore(null), resumers: createResumers(), relaunched: [] });
    await c.handle(incident({ sender: null, sender_note: "sender unknown" }));
    expect(notices[0].action).toBeNull();
    expect(notices[0].message).toContain("sender unknown");
    expect(notices[0].message).toContain("None has a saved Claude conversation to resume.");
  });

  it("a repeated event for one incident is offered once, even while the first is being checked", async () => {
    const { c, notices } = controller(hostTabs(3));
    await Promise.all([c.handle(incident()), c.handle(incident()), c.handle(incident({ session_ids: ["term-1"] }))]);
    expect(notices).toHaveLength(1);
  });

  it("a different incident is its own offer", async () => {
    const { c, notices } = controller(hostTabs(3));
    await c.handle(incident());
    await c.handle(incident({ id: "inc-2" }));
    expect(notices).toHaveLength(2);
  });

  it("ignores a payload that is not an incident", async () => {
    const { c, notices } = controller(hostTabs(3));
    expect(await c.handle({ nope: true })).toBeNull();
    expect(notices).toEqual([]);
  });

  it("taking the offer relaunches each listed tab once, and a second press relaunches none", async () => {
    const parts = hostTabs(3);
    const { c, notices, done } = controller(parts);
    await c.handle(incident());
    await notices[0].action?.onClick();
    expect(parts.relaunched).toEqual(["term-1", "term-2", "term-3"]);
    await notices[0].action?.onClick();
    expect(parts.relaunched).toHaveLength(3);
    expect(done).toEqual([report({ offered: 3, started: 3 }), report({ offered: 3, missing: 3 })]);
  });

  it("a tab the user closed meanwhile is counted as having no tab, and the others still relaunch", async () => {
    const parts = hostTabs(3);
    const { c, notices, done } = controller(parts);
    await c.handle(incident());
    parts.resumers.unregister("term-2");
    await notices[0].action?.onClick();
    expect(parts.relaunched).toEqual(["term-1", "term-3"]);
    expect(done).toEqual([report({ offered: 3, started: 2, missing: 1 })]);
  });

  // S2-04: only a process that started counts as resumed.
  it("a relaunch that failed or waits for a slot is never counted as resumed", async () => {
    const parts = hostTabs(4, { outcome: (i) => (i === 2 ? "failed" : i === 3 ? "deferred" : "started") });
    const { c, notices, failed, done } = controller(parts);
    await c.handle(incident({ session_ids: ["term-1", "term-2", "term-3", "term-4"] }));
    await notices[0].action?.onClick();
    expect(parts.relaunched).toEqual(["term-1", "term-2", "term-3", "term-4"]);
    expect(done).toEqual([report({ offered: 4, started: 2, failed: 1, deferred: 1 })]);
    // A tab that reports its own failure is not reported again.
    expect(failed).toEqual([]);
  });

  it("every relaunch failing reports none started", async () => {
    const parts = hostTabs(3, { outcome: () => "failed" });
    const { c, notices, done } = controller(parts);
    await c.handle(incident());
    await notices[0].action?.onClick();
    expect(done).toEqual([report({ offered: 3, failed: 3 })]);
  });

  it("a relaunch that throws is counted as failed and reported, and the rest still run", async () => {
    const parts = hostTabs(3);
    parts.resumers.register("term-2", async () => {
      throw new Error("broken tab");
    });
    const { c, notices, failed, done } = controller(parts);
    await c.handle(incident());
    await notices[0].action?.onClick();
    expect(failed).toEqual(["term-2"]);
    expect(parts.relaunched).toEqual(["term-1", "term-3"]);
    expect(done).toEqual([report({ offered: 3, started: 2, failed: 1 })]);
  });

  it("handles a 50-tab incident: every tab relaunched exactly once, in order", async () => {
    const parts = hostTabs(50);
    const { c, notices } = controller(parts);
    const ids = Array.from({ length: 50 }, (_, i) => `term-${i + 1}`);
    await c.handle(incident({ session_ids: ids }));
    expect(notices[0].action?.label).toBe("Resume 50");
    await Promise.all([notices[0].action?.onClick(), notices[0].action?.onClick()]);
    expect(parts.relaunched).toEqual(ids);
  });

  it("describes a single session without a plural", async () => {
    const store = createHostedClaudeStore(null);
    const one = incident({ session_ids: ["term-1"] });
    expect(describeIncident(one, await planIncident(one, store, transcripts()))).toContain("1 terminal session ended");
  });
});

describe("what taking the offer did, in words", () => {
  it("says nothing when every conversation offered was relaunched", () => {
    expect(describeResumeReport(report({ offered: 3, started: 3 }))).toBeNull();
  });

  it("names each kind of outcome instead of blaming a missing tab", () => {
    const line = describeResumeReport(report({ offered: 5, started: 1, deferred: 2, failed: 1, missing: 1 }));
    expect(line).toBe(
      "Resumed 1 of 5 Claude conversations: 2 waiting for a free terminal slot (each starts when one frees); " +
        "1 could not start (each tab says why); 1 no longer had a tab to resume in.",
    );
    expect(describeResumeReport(report({ offered: 2, failed: 2 }))).toBe("Resumed 0 of 2 Claude conversations: 2 could not start (each tab says why).");
  });
});
