import { get } from "svelte/store";
import { describe, expect, it, vi } from "vitest";
import { createCommitHover, type CommitHoverDeps } from "./commitHover";

/** Hand-driven timers and reads, so every ordering is the test's to choose. */
function harness(over: Partial<CommitHoverDeps> = {}) {
  const timers: Array<{ fn: () => void; cleared: boolean }> = [];
  const bodies = new Map<string, { resolve: (v: string) => void; reject: (e: unknown) => void }>();
  const notes = new Map<string, { resolve: (v: string | null) => void; reject: (e: unknown) => void }>();
  const readBody = vi.fn(
    (_repo: string, id: string) => new Promise<string>((resolve, reject) => bodies.set(id, { resolve, reject })),
  );
  const readNote = vi.fn(
    (_repo: string, id: string) =>
      new Promise<string | null>((resolve, reject) => notes.set(id, { resolve, reject })),
  );
  const hover = createCommitHover({
    readBody,
    readNote,
    delayMs: 250,
    setTimer: (fn) => {
      const t = { fn, cleared: false };
      timers.push(t);
      return t;
    },
    clearTimer: (handle) => {
      (handle as { cleared: boolean }).cleared = true;
    },
    ...over,
  });
  const fire = () => {
    for (const t of timers.splice(0)) if (!t.cleared) t.fn();
  };
  const settle = () => new Promise((r) => setTimeout(r, 0));
  return { hover, readBody, readNote, bodies, notes, fire, settle };
}

describe("createCommitHover", () => {
  it("shows the row at once and reads only after the pointer rests", async () => {
    const h = harness();
    h.hover.show("/r", "c1");
    expect(get(h.hover)).toMatchObject({ commitId: "c1", body: { state: "loading" }, note: { state: "loading" } });
    expect(h.readBody).not.toHaveBeenCalled();
    h.fire();
    expect(h.readBody).toHaveBeenCalledWith("/r", "c1");
    expect(h.readNote).toHaveBeenCalledWith("/r", "c1");
    h.bodies.get("c1")!.resolve("the body");
    h.notes.get("c1")!.resolve(null);
    await h.settle();
    expect(get(h.hover)).toMatchObject({
      body: { state: "ready", value: "the body" },
      note: { state: "ready", value: null },
    });
  });

  it("does not read a row the pointer only swept across", () => {
    const h = harness();
    h.hover.show("/r", "c1");
    h.hover.show("/r", "c2");
    h.hover.show("/r", "c3");
    h.fire();
    expect(h.readBody.mock.calls.map((c) => c[1])).toEqual(["c3"]);
  });

  it("never shows an answer for a commit the pointer has left, but keeps it", async () => {
    const h = harness();
    h.hover.show("/r", "c1");
    h.fire();
    h.hover.show("/r", "c2");
    h.bodies.get("c1")!.resolve("late body for c1");
    h.notes.get("c1")!.resolve("late note");
    await h.settle();
    expect(get(h.hover)).toMatchObject({ commitId: "c2", body: { state: "loading" } });
    // Back on c1: answered from the cache, with no second read.
    h.hover.show("/r", "c1");
    expect(get(h.hover)).toMatchObject({
      commitId: "c1",
      body: { state: "ready", value: "late body for c1" },
      note: { state: "ready", value: "late note" },
    });
    h.fire();
    expect(h.readBody).toHaveBeenCalledTimes(1);
  });

  it("reports a failed note apart from the body, and never as 'no note'", async () => {
    const h = harness();
    h.hover.show("/r", "c1");
    h.fire();
    h.bodies.get("c1")!.resolve("body");
    h.notes.get("c1")!.reject(new Error("notes ref unreadable"));
    await h.settle();
    const info = get(h.hover)!;
    expect(info.body).toEqual({ state: "ready", value: "body" });
    expect(info.note.state).toBe("error");
    expect(info.note.state === "error" && info.note.error).toContain("notes ref unreadable");
  });

  it("does not cache a failure, so the next hover asks again", async () => {
    const h = harness();
    h.hover.show("/r", "c1");
    h.fire();
    h.bodies.get("c1")!.reject(new Error("boom"));
    h.notes.get("c1")!.resolve(null);
    await h.settle();
    h.hover.hide();
    h.hover.show("/r", "c1");
    h.fire();
    expect(h.readBody).toHaveBeenCalledTimes(2);
    // The note was answered, so it is not read twice.
    expect(h.readNote).toHaveBeenCalledTimes(1);
  });

  it("keys answers by repository, so one repo's commit never answers for another's", async () => {
    const h = harness();
    h.hover.show("/a", "c1");
    h.fire();
    h.bodies.get("c1")!.resolve("from a");
    h.notes.get("c1")!.resolve(null);
    await h.settle();
    h.hover.show("/b", "c1");
    expect(get(h.hover)).toMatchObject({ repoPath: "/b", body: { state: "loading" } });
  });

  it("hides on leave and drops the pending read", () => {
    const h = harness();
    h.hover.show("/r", "c1");
    h.hover.hide();
    h.fire();
    expect(get(h.hover)).toBeNull();
    expect(h.readBody).not.toHaveBeenCalled();
  });

  it("forgets answers on reset but keeps the card up and reads it again", async () => {
    const h = harness();
    h.hover.show("/r", "c1");
    h.fire();
    h.bodies.get("c1")!.resolve("old body");
    h.notes.get("c1")!.resolve("old note");
    await h.settle();
    h.hover.reset();
    expect(get(h.hover)).toMatchObject({ commitId: "c1", note: { state: "loading" } });
    h.fire();
    expect(h.readNote).toHaveBeenCalledTimes(2);
    h.notes.get("c1")!.resolve("new note");
    h.bodies.get("c1")!.resolve("old body");
    await h.settle();
    expect(get(h.hover)).toMatchObject({ note: { state: "ready", value: "new note" } });
  });

  it("evicts the oldest answers past its capacity", async () => {
    const h = harness({ capacity: 2 });
    for (const id of ["c1", "c2", "c3"]) {
      h.hover.show("/r", id);
      h.fire();
      h.bodies.get(id)!.resolve(id);
      h.notes.get(id)!.resolve(null);
      await h.settle();
    }
    h.hover.show("/r", "c1");
    expect(get(h.hover)).toMatchObject({ body: { state: "loading" } });
    h.hover.show("/r", "c3");
    expect(get(h.hover)).toMatchObject({ body: { state: "ready", value: "c3" } });
  });
});
