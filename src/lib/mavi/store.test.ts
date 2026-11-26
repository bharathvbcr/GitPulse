/**
 * The Mavi store against a fake host. The host's own rules (one live thread
 * per worktree, phases, sequence numbers) are the Rust side's to test; this
 * pins down what the page does with them: replays, the queue, drafts kept
 * across a quit, resume and adopt, forget, and what counts as seen.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MaviEntry, MaviSummary } from "./types";

type Handler<T> = (value: T) => void;

const host = vi.hoisted(() => {
  const state = {
    entries: null as null | ((id: string, entries: { seq: number; line: string }[]) => void),
    status: null as null | ((summary: unknown) => void),
    live: [] as unknown[],
    replays: new Map<string, { entries: { seq: number; line: string }[] }>(),
    nextId: 0,
    started: [] as unknown[],
    sent: [] as { id: string; text: string }[],
    sendError: null as Error | null,
  };
  return state;
});

vi.mock("./client", () => ({
  MAVI_MODES: [],
  onMaviEvents: async (handler: (id: string, entries: MaviEntry[]) => void) => {
    host.entries = handler;
    return () => {};
  },
  onMaviStatus: async (handler: Handler<MaviSummary>) => {
    host.status = handler as (summary: unknown) => void;
    return () => {};
  },
  maviClient: {
    list: vi.fn(async () => host.live),
    start: vi.fn(async (request: { repoPath?: string; host?: string; dir?: string; mode: string; resume?: string }) => {
      host.started.push(request);
      host.nextId += 1;
      const target = request.repoPath ? { kind: "local", root: request.repoPath } : { kind: "ssh", host: request.host, dir: request.dir };
      return summary(`s${host.nextId}`, { target, conversation_id: request.resume ?? null });
    }),
    events: vi.fn(async (id: string) => ({ entries: host.replays.get(id)?.entries ?? [], summary: summary(id) })),
    send: vi.fn(async (id: string, text: string) => {
      if (host.sendError) throw host.sendError;
      host.sent.push({ id, text });
      return 1;
    }),
    answer: vi.fn(async () => {}),
    interrupt: vi.fn(async () => {}),
    stop: vi.fn(async () => {}),
  },
}));

const closed = vi.hoisted(() => ({ keys: [] as string[] }));
vi.mock("./childTabs.svelte", () => ({
  childTabs: { forget: () => {} },
  closeThreadChildren: vi.fn(async (key: string) => {
    closed.keys.push(key);
    return { closed: 0, skippedProtected: 0 };
  }),
}));

function summary(id: string, extra: Record<string, unknown> = {}): MaviSummary {
  return {
    id,
    target: { kind: "local", root: "/repo" },
    mode: "ask",
    model: null,
    phase: "ready",
    conversation_id: null,
    pending: 0,
    turns: 0,
    cost_usd: 0,
    touched: [],
    background_tasks: 0,
    started_at_ms: 1,
    last_activity_ms: 1,
    exit: null,
    exit_ok: true,
    last_turn: null,
    ...extra,
  } as MaviSummary;
}

const line = (value: unknown) => JSON.stringify(value);

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

let storage: MemoryStorage;

async function freshStore() {
  vi.resetModules();
  const { maviStore } = await import("./store.svelte");
  await maviStore.init();
  return maviStore;
}

beforeEach(() => {
  storage = new MemoryStorage();
  vi.stubGlobal("localStorage", storage);
  host.entries = null;
  host.status = null;
  host.live = [];
  host.replays.clear();
  host.nextId = 0;
  host.started = [];
  host.sent = [];
  host.sendError = null;
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("maviStore", () => {
  it("starts a thread, sends its first message and replays what the host kept", async () => {
    const store = await freshStore();
    host.replays.set("s1", { entries: [{ seq: 1, line: line({ type: "mavi_user", text: "hello", turn: 1 }) }] });
    const key = await store.create({ target: { kind: "local", repoPath: "/repo" }, mode: "ask", firstMessage: "Fix the build" });
    expect(store.record(key)).toMatchObject({ sessionId: "s1", title: "Fix the build" });
    expect(host.sent).toEqual([{ id: "s1", text: "Fix the build" }]);
    expect(store.timelines[key]!.items.map((i) => i.kind)).toEqual(["user"]);
    expect(store.openKey).toBe(key);
    // Live lines go to the loaded timeline.
    host.entries!("s1", [{ seq: 2, line: line({ type: "mavi_user", text: "again", turn: 2 }) }]);
    expect(store.timelines[key]!.items).toHaveLength(2);
    expect(store.recordFor("s1")?.key).toBe(key);
  });

  it("keeps the thread when its first message cannot be sent, with the message as its draft", async () => {
    const store = await freshStore();
    host.sendError = new Error("The agent has ended.");
    const key = await store.create({ target: { kind: "ssh", host: "gpu", dir: "~/x" }, mode: "ask", firstMessage: "Profile it" });
    expect(store.records).toHaveLength(1);
    expect(store.drafts[key]).toBe("Profile it");
    expect(store.errors[key]).toBe("The agent has ended.");
  });

  it("queues while a turn runs, sends the queue as one turn when idle, and restores it if refused", async () => {
    const store = await freshStore();
    const key = await store.create({ target: { kind: "local", repoPath: "/repo" }, mode: "ask", firstMessage: "go" });
    host.status!(summary("s1", { phase: "working" }));
    await store.send(key, "one");
    await store.send(key, "two");
    expect(store.queues[key]).toEqual(["one", "two"]);
    store.unqueue(key, 5);
    expect(store.queues[key]).toEqual(["one", "two"]);

    host.sendError = new Error("A turn is still running; send this when it ends.");
    host.status!(summary("s1", { phase: "idle", last_activity_ms: 50 }));
    await vi.waitFor(() => expect(store.errors[key]).toContain("still running"));
    expect(store.queues[key]).toEqual(["one", "two"]);

    host.sendError = null;
    host.status!(summary("s1", { phase: "idle", last_activity_ms: 60 }));
    await vi.waitFor(() => expect(host.sent.at(-1)).toEqual({ id: "s1", text: "one\n\ntwo" }));
    expect(store.queues[key]).toEqual([]);
    expect(store.errors[key]).toBeUndefined();
  });

  it("turns a queue left when the agent ends into the draft, and keeps drafts across a restart", async () => {
    const store = await freshStore();
    const key = await store.create({ target: { kind: "local", repoPath: "/repo" }, mode: "ask", firstMessage: "go" });
    host.status!(summary("s1", { phase: "working" }));
    await store.send(key, "queued words");
    store.setDraft(key, "half typed");
    expect(JSON.parse(storage.getItem("gitpulse.mavi.unsent")!)).toEqual({ [key]: "queued words\n\nhalf typed" });
    host.status!(summary("s1", { phase: "ended", exit_ok: true }));
    expect(store.queues[key]).toBeUndefined();
    expect(store.drafts[key]).toBe("queued words\n\nhalf typed");

    // A relaunch: the host runs nothing, the record and its draft come back.
    const again = await freshStore();
    expect(again.record(key)?.sessionId).toBeNull();
    expect(again.drafts[key]).toBe("queued words\n\nhalf typed");
    again.setDraft(key, "");
    expect(storage.getItem("gitpulse.mavi.unsent")).toBeNull();
  });

  it("resumes a saved conversation from its replay, and a new one after this run's turns", async () => {
    const store = await freshStore();
    const key = await store.create({ target: { kind: "local", repoPath: "/repo" }, mode: "ask", firstMessage: "go" });
    host.entries!("s1", [{ seq: 1, line: line({ type: "mavi_user", text: "go", turn: 1 }) }]);
    host.status!(summary("s1", { phase: "ended" }));

    // No conversation saved: the next session's turn 1 follows this run's turn 1.
    await store.resume(key);
    expect(store.record(key)?.sessionId).toBe("s2");
    host.entries!("s2", [{ seq: 1, line: line({ type: "mavi_user", text: "next", turn: 1 }) }]);
    const turns = store.timelines[key]!.items.filter((i) => i.kind === "user").map((i) => i.turn);
    expect(turns).toEqual([1, 2]);

    // A saved conversation: the timeline is rebuilt from the host's replay.
    host.status!(summary("s2", { phase: "idle", conversation_id: "c-1" }));
    host.status!(summary("s2", { phase: "ended", conversation_id: "c-1" }));
    host.replays.set("s3", { entries: [{ seq: 1, line: line({ type: "mavi_history", available: true, turns: 2 }) }] });
    await store.resume(key, true);
    expect(host.started.at(-1)).toMatchObject({ resume: "c-1", acknowledged: true });
    expect(store.timelines[key]!.items.map((i) => i.kind)).toEqual(["notice"]);
  });

  it("adopts a terminal's conversation under its first prompt's title, and reuses the thread it came from", async () => {
    const store = await freshStore();
    host.replays.set("s1", { entries: [{ seq: 1, line: line({ type: "mavi_user", text: "Refactor the parser", turn: 1 }) }] });
    const key = await store.adopt({ root: "/repo", conversationId: "ABC", mode: "ask" });
    expect(store.record(key)).toMatchObject({ title: "Refactor the parser", conversationId: "abc", sessionId: "s1" });

    store.markMoved(key, true);
    expect(store.record(key)?.movedToTerminalAt).not.toBeNull();
    host.status!(summary("s1", { phase: "ended", conversation_id: "abc" }));
    const back = await store.adopt({ root: "/repo", conversationId: "abc", mode: "edit", title: "Named" });
    expect(back).toBe(key);
    expect(store.record(key)).toMatchObject({ mode: "edit", movedToTerminalAt: null, sessionId: "s2" });
    expect(store.records).toHaveLength(1);
  });

  it("answers, interrupts, stops, renames and forgets", async () => {
    const store = await freshStore();
    const { maviClient } = await import("./client");
    const key = await store.create({ target: { kind: "local", repoPath: "/repo" }, mode: "ask", firstMessage: "go" });
    await store.answer(key, "r1", "deny", "no");
    expect(maviClient.answer).toHaveBeenCalledWith("s1", "r1", "deny", "no");
    await store.interrupt(key);
    expect(maviClient.interrupt).toHaveBeenCalledWith("s1");
    await store.stop(key);
    expect(maviClient.stop).toHaveBeenCalledWith("s1");

    store.rename(key, "  A better name  ");
    expect(store.record(key)?.title).toBe("A better name");
    store.rename(key, "   ");
    expect(store.record(key)?.title).toBe("A better name");

    store.setDraft(key, "draft");
    store.forget(key);
    expect(store.records).toEqual([]);
    expect(store.drafts[key]).toBeUndefined();
    expect(store.summaries.s1).toBeUndefined();
    expect(store.openKey).toBeNull();
    // Nothing to act on once forgotten.
    await store.answer(key, "r", "allow");
    await expect(store.send(key, "x")).rejects.toThrow("Resume");
  });

  it("names the owning thread to the host on create, resume and adopt, and closes its tabs only on forget", async () => {
    closed.keys = [];
    const store = await freshStore();
    const key = await store.create({ target: { kind: "local", repoPath: "/repo" }, mode: "ask", firstMessage: "go" });
    expect(host.started[0]).toMatchObject({ threadKey: key });
    host.status!(summary("s1", { phase: "ended" }));
    await store.resume(key);
    expect(host.started[1]).toMatchObject({ threadKey: key });
    const adopted = await store.adopt({ root: "/other", conversationId: "abc", mode: "ask" });
    expect(host.started[2]).toMatchObject({ threadKey: adopted });
    expect(adopted).not.toBe(key);
    // Stopping or interrupting leaves the tabs: the person may still want them.
    await store.interrupt(key);
    await store.stop(key);
    expect(closed.keys).toEqual([]);
    store.forget(key);
    expect(closed.keys).toEqual([key]);
  });

  it("counts attention, lists touched files and marks a thread seen only while it is on screen", async () => {
    const store = await freshStore();
    const a = await store.create({ target: { kind: "local", repoPath: "/repo" }, mode: "ask", firstMessage: "a" });
    const b = await store.create({ target: { kind: "local", repoPath: "/repo/" }, mode: "ask", firstMessage: "b" });
    host.status!(summary("s1", { phase: "approval", touched: ["src/a.ts"] }));
    host.status!(summary("s2", { phase: "ended", exit_ok: false, last_activity_ms: Date.now() + 10_000 }));
    expect(store.attention()).toEqual({ needsYou: 1, failed: 1, firstNeedsYou: a });
    expect(store.touchedIn("/repo/")).toEqual([{ path: "src/a.ts", key: a, title: "a", working: true }]);

    // Not on screen: opening a thread is not seeing it.
    vi.useFakeTimers({ toFake: ["Date"] });
    const before = store.record(b)!.seenAt;
    store.setViewing(false);
    vi.setSystemTime(before + 1_000);
    await store.open(b);
    expect(store.record(b)!.seenAt).toBe(before);
    vi.setSystemTime(before + 2_000);
    store.setViewing(true);
    expect(store.record(b)!.seenAt).toBeGreaterThan(before);
    expect(store.status(store.record(b)!)).not.toBe("failed");
  });

  it("follows edits to files in a local thread for the dock, and stops when asked", async () => {
    const store = await freshStore();
    const key = await store.create({ target: { kind: "local", repoPath: "/repo" }, mode: "ask", firstMessage: "go" });
    const seen: string[] = [];
    const off = store.onFileChanged((record, path) => seen.push(`${record.key}:${path}`));
    host.entries!("s1", [{ seq: 5, line: line({ type: "user", tool_use_result: { filePath: "/repo/src/a.ts" } }) }]);
    off();
    host.entries!("s1", [{ seq: 6, line: line({ type: "user", tool_use_result: { filePath: "/repo/src/b.ts" } }) }]);
    expect(seen).toEqual([`${key}:src/a.ts`]);
    // Lines for a session no record holds are ignored.
    host.entries!("nobody", [{ seq: 1, line: line({ type: "mavi_user", text: "x", turn: 1 }) }]);
  });

  it("waits for an agent to end, and gives up after the limit", async () => {
    const store = await freshStore();
    const key = await store.create({ target: { kind: "local", repoPath: "/repo" }, mode: "ask", firstMessage: "go" });
    expect(await store.waitForEnd(key, 0)).toBe(false);
    host.status!(summary("s1", { phase: "ended" }));
    expect(await store.waitForEnd(key)).toBe(true);
  });

  it("asks for the new-thread form with a message, each request distinct", async () => {
    const store = await freshStore();
    store.requestCompose("Look at the tests");
    const first = store.composeRequest!.id;
    store.requestCompose("And the docs", { newWorktree: true });
    expect(store.composeRequest).toEqual({ id: first + 1, message: "And the docs", newWorktree: true });
  });

  it("keeps sessions as they were when the host cannot be asked, and can try again", async () => {
    const { THREADS_STORAGE_KEY } = await import("./threads");
    storage.setItem(
      THREADS_STORAGE_KEY,
      JSON.stringify([{ key: "k", sessionId: "mavi-9", title: "t", target: { kind: "local", root: "/r" }, mode: "ask", model: null, conversationId: null, createdAt: 1, lastActivityAt: 1, seenAt: 1, movedToTerminalAt: null }]),
    );
    vi.resetModules();
    const { maviClient } = await import("./client");
    const { maviStore } = await import("./store.svelte");
    vi.mocked(maviClient.list).mockRejectedValueOnce(new Error("no host"));
    await expect(maviStore.init()).rejects.toThrow("no host");
    // Not called "not running": that would offer a second agent on it.
    expect(maviStore.record("k")?.sessionId).toBe("mavi-9");
    host.live = [summary("mavi-9", { target: { kind: "local", root: "/r" } })];
    await maviStore.init();
    expect(maviStore.record("k")?.sessionId).toBe("mavi-9");
    expect(maviStore.summaries["mavi-9"]?.phase).toBe("ready");
  });
});
