import { describe, expect, it } from "vitest";
import { createHostedClaudeStore, createResumers } from "./hostedClaude";
import {
  MAX_INCIDENT_SESSIONS,
  createIncidentController,
  describeIncident,
  parseIncident,
  planIncident,
  type IncidentNotice,
  type TerminalIncident,
} from "./incident";

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

function hostTabs(count: number, opts: { withRecord?: (i: number) => boolean } = {}) {
  const store = createHostedClaudeStore(null);
  const resumers = createResumers();
  const relaunched: string[] = [];
  for (let i = 1; i <= count; i++) {
    if (opts.withRecord && !opts.withRecord(i)) continue;
    store.begin({ sessionId: uuid(i), repoPath: "/r", startDir: null, mode: null, startedAt: i });
    store.bind(uuid(i), `term-${i}`);
    store.ended(uuid(i), "Terminated: 15");
    resumers.register(`term-${i}`, () => {
      relaunched.push(`term-${i}`);
    });
  }
  return { store, resumers, relaunched };
}

function controller(parts: ReturnType<typeof hostTabs>) {
  const notices: IncidentNotice[] = [];
  const done: [number, number][] = [];
  const failed: string[] = [];
  const c = createIncidentController({
    store: parts.store,
    resumers: parts.resumers,
    notify: (n) => notices.push(n),
    fail: (id) => failed.push(id),
    done: (started, offered) => done.push([started, offered]),
  });
  return { c, notices, done, failed };
}

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
});

describe("planIncident", () => {
  it("lists the tabs with a recorded conversation and counts the rest", () => {
    const { store } = hostTabs(3, { withRecord: (i) => i !== 2 });
    const plan = planIncident(incident(), store);
    expect(plan.resumable).toEqual([
      { terminalId: "term-1", sessionId: uuid(1) },
      { terminalId: "term-3", sessionId: uuid(3) },
    ]);
    expect(plan.skipped).toBe(1);
  });

  it("an incident naming sessions this page never hosted resumes nothing", () => {
    const store = createHostedClaudeStore(null);
    expect(planIncident(incident(), store)).toEqual({ resumable: [], skipped: 3 });
  });

  it("two PTYs recorded against one conversation resume it once", () => {
    const store = createHostedClaudeStore(null);
    store.begin({ sessionId: uuid(1), repoPath: "/r", startDir: null, mode: null, startedAt: 1 });
    store.bind(uuid(1), "term-1");
    const plan = planIncident(incident({ session_ids: ["term-1", "term-1"] }), store);
    expect(plan.resumable).toHaveLength(1);
  });
});

describe("the incident controller", () => {
  it("offers Resume N for the listed tabs with a conversation", () => {
    const { c, notices } = controller(hostTabs(3));
    c.handle(incident());
    expect(notices).toHaveLength(1);
    expect(notices[0].action?.label).toBe("Resume 3");
    expect(notices[0].message).toContain("sent by pkill (pid 89829)");
    expect(notices[0].message).toContain("3 terminal sessions ended by Terminated: 15");
  });

  it("says sender unknown rather than guessing, and offers nothing when nothing can resume", () => {
    const { c, notices } = controller({ store: createHostedClaudeStore(null), resumers: createResumers(), relaunched: [] });
    c.handle(incident({ sender: null, sender_note: "sender unknown" }));
    expect(notices[0].action).toBeNull();
    expect(notices[0].message).toContain("sender unknown");
    expect(notices[0].message).toContain("None has a Claude conversation to resume.");
  });

  it("a repeated event for one incident is offered once", () => {
    const { c, notices } = controller(hostTabs(3));
    c.handle(incident());
    c.handle(incident());
    c.handle(incident({ session_ids: ["term-1"] }));
    expect(notices).toHaveLength(1);
  });

  it("a different incident is its own offer", () => {
    const { c, notices } = controller(hostTabs(3));
    c.handle(incident());
    c.handle(incident({ id: "inc-2" }));
    expect(notices).toHaveLength(2);
  });

  it("ignores a payload that is not an incident", () => {
    const { c, notices } = controller(hostTabs(3));
    expect(c.handle({ nope: true })).toBeNull();
    expect(notices).toEqual([]);
  });

  it("taking the offer relaunches each listed tab once, and a second press relaunches none", async () => {
    const parts = hostTabs(3);
    const { c, notices, done } = controller(parts);
    c.handle(incident());
    await notices[0].action?.onClick();
    expect(parts.relaunched).toEqual(["term-1", "term-2", "term-3"]);
    await notices[0].action?.onClick();
    expect(parts.relaunched).toHaveLength(3);
    expect(done).toEqual([
      [3, 3],
      [0, 3],
    ]);
  });

  it("a tab the user closed meanwhile is skipped and the others still relaunch", async () => {
    const parts = hostTabs(3);
    const { c, notices, done } = controller(parts);
    c.handle(incident());
    parts.resumers.unregister("term-2");
    await notices[0].action?.onClick();
    expect(parts.relaunched).toEqual(["term-1", "term-3"]);
    expect(done).toEqual([[2, 3]]);
  });

  it("one relaunch that fails does not stop the rest", async () => {
    const parts = hostTabs(3);
    parts.resumers.register("term-2", () => {
      throw new Error("no slot");
    });
    const { c, notices, failed, done } = controller(parts);
    c.handle(incident());
    await notices[0].action?.onClick();
    expect(failed).toEqual(["term-2"]);
    expect(parts.relaunched).toEqual(["term-1", "term-3"]);
    expect(done).toEqual([[2, 3]]);
  });

  it("handles a 50-tab incident: every tab relaunched exactly once, in order", async () => {
    const parts = hostTabs(50);
    const { c, notices } = controller(parts);
    const ids = Array.from({ length: 50 }, (_, i) => `term-${i + 1}`);
    c.handle(incident({ session_ids: ids }));
    expect(notices[0].action?.label).toBe("Resume 50");
    await Promise.all([notices[0].action?.onClick(), notices[0].action?.onClick()]);
    expect(parts.relaunched).toEqual(ids);
  });

  it("describes a single session without a plural", () => {
    const store = createHostedClaudeStore(null);
    const one = incident({ session_ids: ["term-1"] });
    expect(describeIncident(one, planIncident(one, store))).toContain("1 terminal session ended");
  });
});
