import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { emptyThread } from "./reduce";
import { readThreadRecords, THREADS_STORAGE_KEY, writeThreadRecords, type ThreadRecord } from "./threads";
import type { MaviWorkSummary, SummarizeRequest } from "./types";

const host = vi.hoisted(() => ({
  summarizeCalls: [] as SummarizeRequest[],
  summarizeImpl: null as null | ((req: SummarizeRequest) => Promise<MaviWorkSummary>),
}));

vi.mock("./client", () => ({
  MAVI_MODES: [],
  onMaviEvents: async () => () => {},
  onMaviStatus: async () => () => {},
  maviClient: {
    list: vi.fn(async () => []),
    start: vi.fn(async (req: { repoPath?: string; host?: string; dir?: string; mode: string; resume?: string }) => ({
      id: "s1",
      target: req.repoPath ? { kind: "local", root: req.repoPath } : { kind: "ssh", host: req.host ?? "h", dir: req.dir ?? "~" },
      mode: req.mode,
      model: null,
      phase: "ready",
      conversation_id: null,
      pending: 0,
      turns: 0,
      cost_usd: 0,
      touched: ["src/a.ts"],
      background_tasks: 0,
      started_at_ms: 1,
      last_activity_ms: 1,
      exit: null,
      exit_ok: true,
      last_turn: null,
    })),
    events: vi.fn(async () => ({ entries: [], next_seq: 1, summary: null })),
    send: vi.fn(async () => 1),
    answer: vi.fn(async () => {}),
    interrupt: vi.fn(async () => {}),
    stop: vi.fn(async () => {}),
    summarize: vi.fn(async (request: SummarizeRequest) => {
      host.summarizeCalls.push(request);
      if (host.summarizeImpl) return host.summarizeImpl(request);
      return {
        text: "Fixed the auth check.\n\n### What changed\n- Updated `src/a.ts`.",
        model: "haiku",
        engine: "claude",
        elapsedMs: 120,
        truncated: false,
        warnings: [],
      };
    }),
  },
}));

class MemoryStorage {
  map = new Map<string, string>();
  getItem(key: string) {
    return this.map.get(key) ?? null;
  }
  setItem(key: string, value: string) {
    this.map.set(key, value);
  }
  removeItem(key: string) {
    this.map.delete(key);
  }
}

const makeRecord = (key: string, extra: Partial<ThreadRecord> = {}): ThreadRecord => ({
  key,
  sessionId: "mavi-1",
  title: "Fix auth",
  target: { kind: "local", root: "/repo" },
  mode: "ask",
  model: null,
  conversationId: null,
  createdAt: 1,
  lastActivityAt: 1,
  seenAt: 1,
  movedToTerminalAt: null,
  ...extra,
});

describe("thread workSummary persistence", () => {
  it("round-trips workSummary and bounds text and warnings defensively", () => {
    const storage = new MemoryStorage();
    const longText = "x".repeat(9_000);
    const warnings = ["w1", "w2", "w3", "w4", "w5"];
    writeThreadRecords(storage, [
      makeRecord("k1", {
        workSummary: {
          text: longText,
          model: "haiku",
          engine: "claude",
          createdAt: 42,
          truncated: true,
          warnings,
        },
      }),
      makeRecord("k2", {
        workSummary: null,
      }),
    ]);

    const parsed = readThreadRecords(storage);
    expect(parsed).toHaveLength(2);
    expect(parsed[0]!.workSummary).toEqual({
      text: "x".repeat(8000),
      model: "haiku",
      engine: "claude",
      createdAt: 42,
      truncated: true,
      warnings: ["w1", "w2", "w3", "w4"],
    });
    expect(parsed[1]!.workSummary).toBeUndefined();

    // Drops malformed workSummary objects without dropping the record.
    storage.setItem(
      THREADS_STORAGE_KEY,
      JSON.stringify([
        makeRecord("bad", {
          workSummary: { text: "   ", model: "haiku", engine: "claude", createdAt: 1 },
        }),
      ]),
    );
    expect(readThreadRecords(storage)[0]!.workSummary).toBeUndefined();
  });
});

describe("buildClientTranscript", () => {
  it("extracts user, assistant, tool, turn_end, and error notices while skipping thinking", async () => {
    const { buildClientTranscript } = await import("./store.svelte");
    expect(buildClientTranscript(null)).toBe("");

    const timeline = emptyThread();
    timeline.items.push(
      { kind: "user", id: "u1", turn: 1, text: "Check tests", at: 1 },
      { kind: "thinking", id: "h1", turn: 1, messageId: "m1", streaming: false, final: true, text: "secret reasoning" },
      { kind: "assistant", id: "a1", turn: 1, messageId: "m1", text: "Running vitest.", streaming: false, final: true },
      {
        kind: "tool",
        id: "t1",
        turn: 1,
        name: "Bash",
        input: { command: "bunx vitest run" },
        partialInput: "",
        status: "ok",
        result: "huge output",
        patch: null,
        parentId: null,
        approval: null,
        background: false,
      },
      {
        kind: "turn_end",
        id: "e1",
        turn: 1,
        subtype: "success",
        ok: true,
        interrupted: false,
        durationMs: 10,
        costUsd: 0.01,
        summary: { category: "completed", detail: "Tests passed.", needsAction: "" },
        history: false,
      },
      { kind: "notice", id: "n1", turn: 1, tone: "info", text: "The agent ended." },
      { kind: "notice", id: "n2", turn: 1, tone: "error", text: "Exit code 1" },
    );

    const out = buildClientTranscript(timeline);
    expect(out).toContain("User: Check tests");
    expect(out).toContain("Assistant: Running vitest.");
    expect(out).toContain("Tool Bash (ok): bunx vitest run");
    expect(out).toContain("Turn summary [completed]: Tests passed.");
    expect(out).toContain("Notice: Exit code 1");
    expect(out).not.toContain("secret reasoning");
    expect(out).not.toContain("huge output");
    expect(out).not.toContain("The agent ended.");
  });
});

describe("maviStore.summarize", () => {
  let storage: MemoryStorage;

  beforeEach(() => {
    storage = new MemoryStorage();
    vi.stubGlobal("localStorage", storage);
    host.summarizeCalls.length = 0;
    host.summarizeImpl = null;
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("guards double-runs, persists workSummary, records errors, and clears on clearSummary/forget", async () => {
    vi.resetModules();
    const { maviStore } = await import("./store.svelte");
    await maviStore.init();
    const key = await maviStore.create({
      target: { kind: "local", repoPath: "/repo" },
      mode: "ask",
      firstMessage: "Fix auth",
    });

    let resolvePending!: (value: MaviWorkSummary) => void;
    host.summarizeImpl = () =>
      new Promise<MaviWorkSummary>((resolve) => {
        resolvePending = resolve;
      });

    const first = maviStore.summarize(key, { engine: "claude", model: "haiku" });
    expect(maviStore.summarizing[key]).toBe(true);
    // Double-run guard returns null without invoking a second IPC call.
    expect(await maviStore.summarize(key)).toBeNull();
    expect(host.summarizeCalls).toHaveLength(1);

    resolvePending({
      text: "Fixed auth.",
      model: "haiku",
      engine: "claude",
      elapsedMs: 50,
      truncated: false,
      warnings: ["note"],
    });
    const saved = await first;
    expect(saved).toMatchObject({ text: "Fixed auth.", model: "haiku", engine: "claude", warnings: ["note"] });
    expect(maviStore.summarizing[key]).toBeUndefined();
    expect(maviStore.record(key)?.workSummary?.text).toBe("Fixed auth.");

    // Failure populates summaryErrors[key].
    host.summarizeImpl = async () => {
      throw new Error("Claude failed");
    };
    expect(await maviStore.summarize(key)).toBeNull();
    expect(maviStore.summaryErrors[key]).toBe("Claude failed");

    maviStore.clearSummary(key);
    expect(maviStore.summaryErrors[key]).toBeUndefined();
    expect(maviStore.record(key)?.workSummary).toBeNull();

    // Error state is cleared on forget too.
    expect(await maviStore.summarize(key)).toBeNull();
    expect(maviStore.summaryErrors[key]).toBe("Claude failed");
    maviStore.forget(key);
    expect(maviStore.summaryErrors[key]).toBeUndefined();
    expect(maviStore.summarizing[key]).toBeUndefined();
  });
});
