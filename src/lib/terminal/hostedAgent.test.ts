import { describe, expect, it } from "vitest";
import {
  MAX_HOSTED_AGENT,
  carriesSession,
  conversationForStart,
  createHostedAgentStore,
  createResumers,
  endedFromOutside,
  readHostedAgent,
  settleConversation,
  withSessionId,
  type AgentProvider,
  type HostedStorage,
} from "./hostedAgent";

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

const launch = (sessionId: string, provider: AgentProvider = "claude", startedAt = 1) => ({
  sessionId,
  provider,
  repoPath: "/work/repo",
  startDir: null,
  mode: "edit" as const,
  startedAt,
});

describe("hostedAgent session flag detection and argument generation", () => {
  it("gives a fresh Claude launch --session-id in front of its arguments by default", () => {
    const { args, sessionId } = withSessionId(["--", "fix it"], () => ID.toUpperCase());
    expect(sessionId).toBe(ID);
    expect(args).toEqual(["--session-id", ID, "--", "fix it"]);
  });

  it("does not inject resume-only --conversation on a fresh Antigravity launch", () => {
    const { args, sessionId } = withSessionId(["--prompt-interactive", "fix bug"], () => ID.toUpperCase(), "agy");
    expect(sessionId).toBeNull();
    expect(args).toEqual(["--prompt-interactive", "fix bug"]);
  });

  it("gives a Claude launch with no arguments a generated session id, while fresh Antigravity stays clean", () => {
    expect(withSessionId(undefined, () => ID, "claude")).toEqual({ args: ["--session-id", ID], sessionId: ID });
    expect(withSessionId(undefined, () => ID, "agy")).toEqual({ args: [], sessionId: null });
  });

  it("leaves a Claude launch that already names a conversation unchanged", () => {
    for (const given of [["--resume", OTHER], ["-r", OTHER], [`--resume=${OTHER}`], ["--session-id", OTHER]]) {
      const out = withSessionId(given, () => ID, "claude");
      expect(out.args).toEqual(given);
      expect(out.sessionId).toBe(OTHER);
    }
  });

  it("leaves an Antigravity launch that already names a conversation unchanged", () => {
    for (const given of [["--conversation", OTHER], [`--conversation=${OTHER}`]]) {
      const out = withSessionId(given, () => ID, "agy");
      expect(out.args).toEqual(given);
      expect(out.sessionId).toBe(OTHER);
    }
  });

  it("recognizes existing --continue or -c flag for Antigravity without adding another", () => {
    expect(carriesSession(["--continue"])).toBe(true);
    expect(carriesSession(["-c"])).toBe(true);
    expect(withSessionId(["--continue"], () => ID, "agy")).toEqual({ args: ["--continue"], sessionId: null });
    expect(withSessionId(["-c"], () => ID, "agy")).toEqual({ args: ["-c"], sessionId: null });
  });

  it("does not treat flags placed after the prompt boundary '--' as CLI session flags", () => {
    expect(carriesSession(["--", "--conversation", OTHER])).toBe(false);
    expect(carriesSession(["--", "--resume", OTHER])).toBe(false);
    const outClaude = withSessionId(["--", "--resume", OTHER], () => ID, "claude");
    expect(outClaude.args).toEqual(["--session-id", ID, "--", "--resume", OTHER]);
    expect(outClaude.sessionId).toBe(ID);

    const outAgy = withSessionId(["--", "--conversation", OTHER], () => ID, "agy");
    expect(outAgy.args).toEqual(["--", "--conversation", OTHER]);
    expect(outAgy.sessionId).toBeNull();
  });

  it("does not mutate caller argument arrays", () => {
    const given = ["--prompt-interactive", "deploy"];
    withSessionId(given, () => ID, "agy");
    expect(given).toEqual(["--prompt-interactive", "deploy"]);
  });
});

describe("conversationForStart provider routing", () => {
  const record = { repoPath: "/work/repo", startDir: null, mode: null, startedAt: 10 };

  it("handles conversationForStart for fresh Antigravity and Claude launches", () => {
    const store = createHostedAgentStore(null);

    const freshAgy = conversationForStart(store, {
      attach: null,
      resumeId: null,
      args: ["--mode", "default"],
      record,
      newId: () => ID,
      provider: "agy",
    });
    expect(freshAgy).toEqual({ args: ["--mode", "default"], sessionId: null });
    expect(store.get(ID)).toBeUndefined();

    const freshClaude = conversationForStart(store, {
      attach: null,
      resumeId: null,
      args: ["--mode", "code"],
      record,
      newId: () => OTHER,
      provider: "claude",
    });
    expect(freshClaude).toEqual({ args: ["--session-id", OTHER, "--mode", "code"], sessionId: OTHER });
    expect(store.get(OTHER)?.provider).toBe("claude");
  });

  it("handles conversationForStart for resumed Antigravity and Claude sessions", () => {
    const store = createHostedAgentStore(null);

    const resumedAgy = conversationForStart(store, {
      attach: null,
      resumeId: ID,
      args: undefined,
      record,
      newId: () => OTHER,
      provider: "agy",
    });
    expect(resumedAgy).toEqual({ args: ["--conversation", ID], sessionId: ID });
    expect(store.get(ID)?.provider).toBe("agy");

    const resumedClaude = conversationForStart(store, {
      attach: null,
      resumeId: OTHER,
      args: undefined,
      record,
      newId: () => ID,
      provider: "claude",
    });
    expect(resumedClaude).toEqual({ args: ["--resume", OTHER], sessionId: OTHER });
    expect(store.get(OTHER)?.provider).toBe("claude");
  });

  it("keeps existing session id when attaching to an active terminal", () => {
    const store = createHostedAgentStore(null);
    store.begin({ ...record, provider: "agy", sessionId: ID });
    store.bind(ID, "term-1");

    const attached = conversationForStart(store, {
      attach: "term-1",
      resumeId: null,
      args: ["--mode", "code"],
      record,
      provider: "agy",
    });
    expect(attached).toEqual({ args: ["--mode", "code"], sessionId: ID });
  });
});

describe("hostedAgent store persistence, bounds, and migration", () => {
  it("persists both Claude and Antigravity records across store instances", () => {
    const storage = memory();
    const first = createHostedAgentStore(storage);
    first.begin(launch(ID, "agy", 100));
    first.bind(ID, "term-agy");
    first.ended(ID, "Terminated: 15");

    first.begin(launch(OTHER, "claude", 200));
    first.bind(OTHER, "term-claude");

    const second = createHostedAgentStore(storage);
    const loadedAgy = second.get(ID);
    expect(loadedAgy).toBeDefined();
    expect(loadedAgy?.sessionId).toBe(ID);
    expect(loadedAgy?.provider).toBe("agy");
    expect(loadedAgy?.terminalId).toBe("term-agy");
    expect(loadedAgy?.endedSignal).toBe("Terminated: 15");

    const loadedClaude = second.get(OTHER);
    expect(loadedClaude).toBeDefined();
    expect(loadedClaude?.sessionId).toBe(OTHER);
    expect(loadedClaude?.provider).toBe("claude");
    expect(loadedClaude?.terminalId).toBe("term-claude");
  });

  it("writes to both gitpulse.hostedAgent.v1 and legacy gitpulse.hostedClaude.v1 storage keys", () => {
    const storage = memory();
    const store = createHostedAgentStore(storage);
    store.begin(launch(ID, "agy", 50));

    expect(storage.data["gitpulse.hostedAgent.v1"]).toBeDefined();
    expect(storage.data["gitpulse.hostedClaude.v1"]).toBeDefined();
    expect(JSON.parse(storage.data["gitpulse.hostedAgent.v1"])).toHaveLength(1);
    expect(JSON.parse(storage.data["gitpulse.hostedClaude.v1"])).toHaveLength(1);
  });

  it("migrates from legacy gitpulse.hostedClaude.v1 if modern key is absent", () => {
    const legacyRecord = {
      sessionId: ID,
      repoPath: "/work/repo",
      startDir: null,
      mode: "edit",
      startedAt: 42,
    };
    const storage = memory({ "gitpulse.hostedClaude.v1": JSON.stringify([legacyRecord]) });
    const store = createHostedAgentStore(storage);

    const loaded = store.get(ID);
    expect(loaded).toBeDefined();
    expect(loaded?.sessionId).toBe(ID);
    expect(loaded?.provider).toBe("claude"); // defaults legacy to claude
  });

  it("is bounded: oldest conversations are pruned when MAX_HOSTED_AGENT is exceeded", () => {
    const storage = memory();
    const store = createHostedAgentStore(storage);
    for (let i = 0; i < MAX_HOSTED_AGENT + 15; i++) {
      const id = `00000000-0000-4000-8000-${String(i).padStart(12, "0")}`;
      const provider: AgentProvider = i % 2 === 0 ? "agy" : "claude";
      store.begin(launch(id, provider, i));
    }
    expect(store.list()).toHaveLength(MAX_HOSTED_AGENT);
    // Oldest 15 should have been pruned
    expect(store.get("00000000-0000-4000-8000-000000000000")).toBeUndefined();
    expect(store.get("00000000-0000-4000-8000-000000000014")).toBeUndefined();
    // Newer entry should remain
    expect(store.get("00000000-0000-4000-8000-000000000020")).toBeDefined();
  });

  it("validates and enforces input bounds on stored records", () => {
    expect(readHostedAgent({ sessionId: ID, repoPath: "x".repeat(5000), startedAt: 1 })).toBeNull();
    expect(readHostedAgent({ sessionId: ID, repoPath: "/r", startedAt: Number.NaN })).toBeNull();
    expect(readHostedAgent({ sessionId: "not-a-uuid", repoPath: "/r", startedAt: 1 })).toBeNull();
    expect(readHostedAgent({ sessionId: ID, repoPath: "/r", startedAt: 1, provider: "agy" })).toMatchObject({
      sessionId: ID,
      provider: "agy",
      terminalId: null,
      startDir: null,
      mode: null,
      endedSignal: null,
    });
  });

  it("survives corrupt storage JSON without throwing", () => {
    for (const raw of ["{ broken", "null", "123", JSON.stringify([{ bad: "record" }])]) {
      const store = createHostedAgentStore(memory({ "gitpulse.hostedAgent.v1": raw }));
      expect(store.list()).toEqual([]);
    }
  });
});

describe("settleConversation and exit handling", () => {
  const exit = (overrides: Partial<{ signal: string; ended_by_gitpulse: boolean; error: string | null }> = {}) => ({
    signal: "",
    ended_by_gitpulse: false,
    error: null,
    ...overrides,
  });

  it("identifies external signal terminations as resumable", () => {
    expect(endedFromOutside(exit({ signal: "Terminated: 15" }))).toBe(true);
    expect(endedFromOutside(exit({ signal: "Killed: 9" }))).toBe(true);
    expect(endedFromOutside(exit({ signal: "Hangup: 1", ended_by_gitpulse: true }))).toBe(false);
    expect(endedFromOutside(exit())).toBe(false);
    expect(endedFromOutside(exit({ ended_by_gitpulse: true }))).toBe(false);
    // GitPulse ended due to failure/error: should be resumable
    expect(endedFromOutside(exit({ signal: "Hangup: 1", ended_by_gitpulse: true, error: "Stalled output" }))).toBe(true);
  });

  it("forgets on clean close and preserves on outside signal", () => {
    const store = createHostedAgentStore(null);
    store.begin(launch(ID, "agy"));
    store.bind(ID, "term-1");

    expect(settleConversation(store, ID, exit({ signal: "Terminated: 15" }))).toBe(true);
    expect(store.byTerminal("term-1")?.endedSignal).toBe("Terminated: 15");
    expect(store.resumable().map((r) => r.sessionId)).toEqual([ID]);

    // Clean exit forgets session
    store.begin(launch(OTHER, "claude"));
    expect(settleConversation(store, OTHER, "closed")).toBe(false);
    expect(store.get(OTHER)).toBeUndefined();
  });
});

describe("createResumers", () => {
  it("registers, tests, and unregisters resumer callbacks", () => {
    const registry = createResumers();
    registry.register("term-agy", async () => "started" as const);
    expect(registry.has("term-agy")).toBe(true);
    expect(registry.size()).toBe(1);
    registry.unregister("term-agy");
    expect(registry.has("term-agy")).toBe(false);
    expect(registry.size()).toBe(0);
  });
});

describe("resolveTranscriptAnswer and fresh Antigravity signal-exit recovery", () => {
  const exit = (overrides: Partial<{ signal: string; ended_by_gitpulse: boolean; error: string | null }> = {}) => ({
    signal: "",
    ended_by_gitpulse: false,
    error: null,
    ...overrides,
  });

  it("forgets stored conversation from memory and localStorage when transcript check answers absent", async () => {
    const mod = await import("./hostedAgent");
    const resolveTranscriptAnswer = (mod as Record<string, unknown>).resolveTranscriptAnswer as (
      store: ReturnType<typeof createHostedAgentStore>,
      conversation: string,
      provider: "claude" | "agy",
      answer: { state: "found" } | { state: "absent" } | { state: "unknown"; reason?: string },
    ) => { offer: string | null; note: string | null };
    expect(typeof resolveTranscriptAnswer).toBe("function");

    const storage = memory();
    const store = createHostedAgentStore(storage);
    store.begin(launch(ID, "claude"));
    store.bind(ID, "term-1");
    settleConversation(store, ID, exit({ signal: "Terminated: 15" }));
    expect(store.get(ID)).toBeDefined();

    const absent = resolveTranscriptAnswer(store, ID, "claude", { state: "absent" });
    expect(absent.offer).toBeNull();
    expect(absent.note).toContain("Claude Code saved no conversation");
    expect(store.get(ID)).toBeUndefined();
    expect(JSON.parse(storage.data["gitpulse.hostedAgent.v1"])).toEqual([]);

    // "unknown" keeps the stored record so a later check can still inspect it, without offering Resume
    store.begin(launch(OTHER, "agy"));
    settleConversation(store, OTHER, exit({ signal: "Hangup: 1" }));
    const unknown = resolveTranscriptAnswer(store, OTHER, "agy", { state: "unknown", reason: "disk busy" });
    expect(unknown.offer).toBeNull();
    expect(unknown.note).toContain("disk busy");
    expect(store.get(OTHER)).toBeDefined();
  });

  it("offers --continue recovery with --add-dir for fresh agy tabs ended by an external signal", async () => {
    const mod = await import("./hostedAgent");
    const sentinel = (mod as Record<string, unknown>).AGY_CONTINUE_SENTINEL as string;
    const unindexedExitOffer = (mod as Record<string, unknown>).unindexedExitOffer as (
      provider: "claude" | "agy" | null,
      hostedId: string | null,
      end: { signal: string; ended_by_gitpulse: boolean; error: string | null } | "closed",
    ) => string | null;
    expect(typeof sentinel).toBe("string");
    expect(typeof unindexedExitOffer).toBe("function");

    // External signal on fresh agy tab (hostedId === null) offers sentinel
    expect(unindexedExitOffer("agy", null, exit({ signal: "Terminated: 15" }))).toBe(sentinel);
    expect(unindexedExitOffer("agy", null, exit({ signal: "Hangup: 1" }))).toBe(sentinel);
    // Clean exit, GitPulse-owned close, explicit UUID session, or Claude never uses agy --continue sentinel
    expect(unindexedExitOffer("agy", null, "closed")).toBeNull();
    expect(unindexedExitOffer("agy", null, exit({ signal: "" }))).toBeNull();
    expect(unindexedExitOffer("agy", null, exit({ signal: "Terminated: 15", ended_by_gitpulse: true }))).toBeNull();
    expect(unindexedExitOffer("agy", ID, exit({ signal: "Terminated: 15" }))).toBeNull();
    expect(unindexedExitOffer("claude", null, exit({ signal: "Terminated: 15" }))).toBeNull();

    // conversationForStart expands sentinel into agy --continue --add-dir <dir> with sessionId: null
    const store = createHostedAgentStore(null);
    const continued = conversationForStart(store, {
      attach: null,
      resumeId: sentinel,
      args: ["--prompt-interactive", "initial prompt should not repeat"],
      record: { repoPath: "/work/repo", startDir: "packages/web", mode: "edit", startedAt: 10, provider: "agy" },
      provider: "agy",
    });
    expect(continued).toEqual({
      args: ["--continue", "--add-dir", "/work/repo/packages/web"],
      sessionId: null,
    });
    expect(store.list()).toEqual([]);

    // UUID-bound agy resume continues to use --conversation <uuid>
    const explicit = conversationForStart(store, {
      attach: null,
      resumeId: ID,
      args: [],
      record: { repoPath: "/work/repo", startDir: null, mode: "edit", startedAt: 20, provider: "agy" },
      provider: "agy",
    });
    expect(explicit).toEqual({
      args: ["--conversation", ID],
      sessionId: ID,
    });
  });
});
