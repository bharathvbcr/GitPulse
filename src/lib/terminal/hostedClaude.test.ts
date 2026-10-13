import { describe, expect, it } from "vitest";
import {
  MAX_HOSTED_CLAUDE,
  carriesSession,
  createHostedClaudeStore,
  createResumers,
  readHostedClaude,
  withSessionId,
  type HostedStorage,
} from "./hostedClaude";

const ID = "6f0a6d1e-1111-4222-8333-444455556666";
const OTHER = "0a0a0a0a-2222-4333-8444-555566667777";

function memory(initial: Record<string, string> = {}): HostedStorage & { data: Record<string, string> } {
  const data = { ...initial };
  return {
    data,
    getItem: (key) => data[key] ?? null,
    setItem: (key, value) => {
      data[key] = value;
    },
  };
}

const launch = (sessionId: string, startedAt = 1) => ({
  sessionId,
  repoPath: "/work/repo",
  startDir: null,
  mode: "edit" as const,
  startedAt,
});

describe("withSessionId", () => {
  it("gives a fresh launch a session id in front of its arguments", () => {
    const { args, sessionId } = withSessionId(["--", "fix it"], () => ID.toUpperCase());
    expect(sessionId).toBe(ID);
    expect(args).toEqual(["--session-id", ID, "--", "fix it"]);
  });

  it("gives a launch with no arguments one too", () => {
    expect(withSessionId(undefined, () => ID)).toEqual({ args: ["--session-id", ID], sessionId: ID });
  });

  it("leaves a launch that already names a conversation exactly as it is", () => {
    for (const given of [["--resume", OTHER], ["-r", OTHER], [`--resume=${OTHER}`], ["--session-id", OTHER]]) {
      const out = withSessionId(given, () => ID);
      expect(out.args).toEqual(given);
      expect(out.sessionId).toBe(OTHER);
    }
  });

  it("knows no id for --continue or a resume by name, and does not add one", () => {
    expect(withSessionId(["--continue"], () => ID)).toEqual({ args: ["--continue"], sessionId: null });
    expect(withSessionId(["-c"], () => ID)).toEqual({ args: ["-c"], sessionId: null });
    expect(withSessionId(["--resume", "auth-refactor"], () => ID)).toEqual({
      args: ["--resume", "auth-refactor"],
      sessionId: null,
    });
    expect(withSessionId(["--resume"], () => ID).sessionId).toBeNull();
  });

  it("does not read a flag out of the prompt", () => {
    expect(carriesSession(["--", "--resume", OTHER])).toBe(false);
    const out = withSessionId(["--", "--resume", OTHER], () => ID);
    expect(out.args).toEqual(["--session-id", ID, "--", "--resume", OTHER]);
    expect(out.sessionId).toBe(ID);
  });

  it("does not change the caller's array", () => {
    const given = ["--", "x"];
    withSessionId(given, () => ID);
    expect(given).toEqual(["--", "x"]);
  });
});

describe("the hosted conversation store", () => {
  it("keeps what a launch recorded across a restart of the app", () => {
    const storage = memory();
    const first = createHostedClaudeStore(storage);
    first.begin(launch(ID));
    first.bind(ID, "term-1");
    first.ended(ID, "Terminated: 15");
    const second = createHostedClaudeStore(storage);
    expect(second.get(ID)).toMatchObject({
      sessionId: ID,
      terminalId: "term-1",
      repoPath: "/work/repo",
      mode: "edit",
      endedSignal: "Terminated: 15",
    });
    expect(second.resumable().map((r) => r.sessionId)).toEqual([ID]);
  });

  it("finds a conversation by the PTY that ran it, taking the newest when an id repeats", () => {
    const store = createHostedClaudeStore(null);
    store.begin(launch(ID, 1));
    store.bind(ID, "term-1");
    store.begin(launch(OTHER, 2));
    store.bind(OTHER, "term-1");
    expect(store.byTerminal("term-1")?.sessionId).toBe(OTHER);
    expect(store.byTerminal("term-9")).toBeUndefined();
  });

  it("a conversation that ended cleanly is not resumable, and a resumed one stops being so", () => {
    const store = createHostedClaudeStore(null);
    store.begin(launch(ID));
    store.ended(ID, null);
    expect(store.resumable()).toEqual([]);
    store.ended(ID, "Killed: 9");
    expect(store.resumable()).toHaveLength(1);
    store.ended(ID, null);
    expect(store.resumable()).toEqual([]);
  });

  it("ignores a bind or end for a conversation it never recorded", () => {
    const store = createHostedClaudeStore(null);
    store.bind(ID, "term-1");
    store.ended(ID, "Terminated: 15");
    expect(store.list()).toEqual([]);
  });

  it("starts empty from storage that is corrupt, wrong-shaped or holds bad records", () => {
    for (const raw of ["{ nope", "{}", "null", JSON.stringify([null, 3, { sessionId: "x" }, { sessionId: ID }])]) {
      const store = createHostedClaudeStore(memory({ "gitpulse.hostedClaude.v1": raw }));
      expect(store.list()).toEqual([]);
    }
  });

  it("survives storage that throws", () => {
    const store = createHostedClaudeStore({
      getItem: () => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("full");
      },
    });
    store.begin(launch(ID));
    expect(store.get(ID)?.sessionId).toBe(ID);
  });

  it("is bounded: the oldest conversations go first", () => {
    const storage = memory();
    const store = createHostedClaudeStore(storage);
    for (let i = 0; i < MAX_HOSTED_CLAUDE + 25; i++) {
      const id = `00000000-0000-4000-8000-${String(i).padStart(12, "0")}`;
      store.begin(launch(id, i));
    }
    expect(store.list()).toHaveLength(MAX_HOSTED_CLAUDE);
    expect(store.get("00000000-0000-4000-8000-000000000000")).toBeUndefined();
    expect(JSON.parse(storage.data["gitpulse.hostedClaude.v1"])).toHaveLength(MAX_HOSTED_CLAUDE);
  });

  it("reads a record's text fields with bounds", () => {
    expect(readHostedClaude({ sessionId: ID, repoPath: "x".repeat(5000), startedAt: 1 })).toBeNull();
    expect(readHostedClaude({ sessionId: ID, repoPath: "/r", startedAt: Number.NaN })).toBeNull();
    expect(readHostedClaude({ sessionId: ID, repoPath: "/r", startedAt: 1 })).toMatchObject({
      terminalId: null,
      startDir: null,
      mode: null,
      endedSignal: null,
    });
  });
});

describe("resumers", () => {
  it("offers a relaunch per dead tab until the tab takes it back", () => {
    const resumers = createResumers();
    resumers.register("term-1", () => {});
    expect(resumers.has("term-1")).toBe(true);
    resumers.unregister("term-1");
    expect(resumers.has("term-1")).toBe(false);
    expect(resumers.size()).toBe(0);
  });
});
