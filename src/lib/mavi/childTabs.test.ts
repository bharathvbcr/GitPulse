import { beforeEach, describe, expect, it, vi } from "vitest";

const calls = vi.hoisted(() => ({
  invoke: [] as [string, unknown][],
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
}));

vi.mock("../ipc/invoke", () => ({
  invoke: vi.fn(async (command: string, args?: unknown) => {
    calls.invoke.push([command, args]);
    return { closed: 2, skippedProtected: 1 };
  }),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: (event: { payload: unknown }) => void) => {
    calls.listeners.set(name, handler);
    return () => calls.listeners.delete(name);
  }),
}));

import { applyChild, childTabs, closeThreadChildren, MAVI_CHILD_EVENT, MAX_CARDS, type ChildStatePayload } from "./childTabs.svelte";
import { childTabNotice } from "./notify";

const T = "0d235a3d-03cf-44a9-ad04-effb59321456";
function payload(over: Partial<ChildStatePayload> = {}): ChildStatePayload {
  return {
    thread_id: T,
    child_session_id: "c1",
    title: "agy audit",
    state: "needs_input",
    detail: "Run this command? git -C /repo status",
    exit_code: null,
    ...over,
  };
}

beforeEach(() => {
  calls.invoke.length = 0;
  calls.listeners.clear();
  childTabs.stop();
  for (const key of Object.keys(childTabs.byThread)) childTabs.forget(key);
});

describe("applyChild", () => {
  it("adds a waiting card and announces it once", () => {
    const first = applyChild([], payload(), 1);
    expect(first.cards).toHaveLength(1);
    expect(first.cards[0]).toMatchObject({ sessionId: "c1", state: "needs_input", title: "agy audit", detail: expect.stringContaining("Run this") });
    expect(first.announce).toBe(true);
    const again = applyChild(first.cards, payload({ detail: "Run this command? other" }), 2);
    expect(again.cards).toHaveLength(1);
    expect(again.announce).toBe(false);
  });

  it("clears a waiting card but never one that already finished", () => {
    const waiting = applyChild([], payload(), 1).cards;
    expect(applyChild(waiting, payload({ state: "cleared" }), 2).cards).toEqual([]);
    const done = applyChild(waiting, payload({ state: "done", exit_code: 0, detail: null }), 2).cards;
    expect(applyChild(done, payload({ state: "cleared" }), 3).cards).toEqual(done);
    // A late needs_input does not reopen a finished card.
    expect(applyChild(done, payload(), 4).cards).toEqual(done);
  });

  it("keeps the earlier title when a later event has none, and names an untitled tab", () => {
    const a = applyChild([], payload(), 1).cards;
    expect(applyChild(a, payload({ state: "ended", title: null, exit_code: 1 }), 2).cards[0]!.title).toBe("agy audit");
    expect(applyChild([], payload({ title: null }), 1).cards[0]!.title).toBe("Terminal tab");
  });

  it("is bounded and sheds finished cards before waiting ones", () => {
    let cards: ReturnType<typeof applyChild>["cards"] = [];
    cards = applyChild(cards, payload({ child_session_id: "wait" }), 0).cards;
    for (let i = 0; i < MAX_CARDS + 5; i++) {
      cards = applyChild(cards, payload({ child_session_id: `d${i}`, state: "done", exit_code: 0 }), i + 1).cards;
    }
    expect(cards).toHaveLength(MAX_CARDS);
    expect(cards.some((c) => c.sessionId === "wait")).toBe(true);
  });
});

describe("childTabs", () => {
  it("subscribes to mavi-child-state and files cards under the owning thread", async () => {
    await childTabs.start();
    const emit = calls.listeners.get(MAVI_CHILD_EVENT);
    expect(emit).toBeDefined();
    const changes: string[] = [];
    childTabs.onChange((c) => changes.push(`${c.card.state}:${c.announce}`));
    emit!({ payload: payload() });
    expect(childTabs.forThread(T).map((c) => c.sessionId)).toEqual(["c1"]);
    expect(childTabs.waiting(T)).toBe(1);
    expect(childTabs.forThread("other")).toEqual([]);
    emit!({ payload: payload({ state: "ended", exit_code: 2, detail: null }) });
    expect(childTabs.waiting(T)).toBe(0);
    expect(childTabs.forThread(T)[0]).toMatchObject({ state: "ended", exitCode: 2 });
    expect(changes).toEqual(["needs_input:true", "ended:true"]);
  });

  it("ignores a malformed payload and listens once", async () => {
    await childTabs.start();
    await childTabs.start();
    const emit = calls.listeners.get(MAVI_CHILD_EVENT)!;
    emit({ payload: { thread_id: "", child_session_id: "x", state: "needs_input" } });
    emit({ payload: { thread_id: T, child_session_id: "x", state: "weird" } });
    emit({ payload: null });
    expect(childTabs.forThread(T)).toEqual([]);
  });

  it("dismisses a card and forgets a thread", () => {
    childTabs.handle(payload());
    childTabs.handle(payload({ child_session_id: "c2" }));
    childTabs.dismiss(T, "c1");
    expect(childTabs.forThread(T).map((c) => c.sessionId)).toEqual(["c2"]);
    childTabs.forget(T);
    expect(childTabs.forThread(T)).toEqual([]);
  });
});

describe("closeThreadChildren", () => {
  it("sends the contract's command and argument name", async () => {
    await expect(closeThreadChildren(T)).resolves.toEqual({ closed: 2, skippedProtected: 1 });
    expect(calls.invoke).toEqual([["mavi_close_thread_children", { threadKey: T }]]);
  });
});

describe("childTabNotice", () => {
  it("names the tab and what it needs", () => {
    const card = applyChild([], payload(), 1).cards[0]!;
    expect(childTabNotice(card, "Audit")).toBe("“agy audit” (from “Audit”) needs your approval.");
    expect(childTabNotice({ ...card, state: "done" }, "Audit")).toBe("“agy audit” (from “Audit”) finished.");
    expect(childTabNotice({ ...card, state: "ended" }, "Audit")).toBe("“agy audit” (from “Audit”) ended.");
  });
});
