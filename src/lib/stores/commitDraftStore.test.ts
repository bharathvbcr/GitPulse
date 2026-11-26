import { describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import {
  createCommitDraftStore,
  STORAGE_KEY_COMMIT_DRAFTS,
  type CommitDraftStorage,
} from "./commitDraftStore";
import { flushPendingWrites } from "../async/deferredWrite";

function memoryStorage(initial: Record<string, string> = {}): CommitDraftStorage & { map: Map<string, string> } {
  const map = new Map<string, string>(Object.entries(initial));
  return {
    map,
    getItem: (key) => map.get(key) ?? null,
    setItem: (key, value) => {
      map.set(key, value);
    },
    removeItem: (key) => {
      map.delete(key);
    },
  };
}

describe("commitDraftStore", () => {
  it("edits only the active tab's draft and ignores edits with no tab", () => {
    const drafts = createCommitDraftStore();
    drafts.set("lost");
    expect(get(drafts)).toBe("");
    drafts.setActiveTab("a");
    drafts.set("for a");
    drafts.setActiveTab("b");
    expect(get(drafts)).toBe("");
    drafts.set("for b");
    expect(drafts.read("a")).toBe("for a");
    drafts.setActiveTab("a");
    expect(get(drafts)).toBe("for a");
    drafts.setActiveTab(null);
    expect(get(drafts)).toBe("");
  });

  it("notifies only when the visible draft changes", () => {
    const drafts = createCommitDraftStore();
    let notified = 0;
    const unsubscribe = drafts.subscribe(() => (notified += 1));
    notified = 0;
    // Two tabs with empty drafts: switching between them shows nothing new.
    drafts.setActiveTab("a");
    drafts.setActiveTab("b");
    drafts.set("");
    expect(notified).toBe(0);
    drafts.set("x");
    drafts.set("x");
    expect(notified).toBe(1);
    unsubscribe();
  });

  it("moves a draft to a successor without overwriting the successor's own", () => {
    const drafts = createCommitDraftStore();
    drafts.setActiveTab("alias");
    drafts.set("alias draft");
    drafts.move("alias", "canonical");
    expect(drafts.read("alias")).toBe("");
    expect(drafts.read("canonical")).toBe("alias draft");

    drafts.setActiveTab("other-alias");
    drafts.set("second alias draft");
    drafts.move("other-alias", "canonical");
    expect(drafts.read("canonical")).toBe("alias draft");
    expect(drafts.read("other-alias")).toBe("");
  });

  it("forgets the drafts of tabs that are no longer open", () => {
    const drafts = createCommitDraftStore();
    for (const id of ["a", "b", "c"]) {
      drafts.setActiveTab(id);
      drafts.set(`draft ${id}`);
    }
    drafts.retain(["a", "c"]);
    expect(drafts.read("a")).toBe("draft a");
    expect(drafts.read("b")).toBe("");
    expect(get(drafts)).toBe("draft c");
    drafts.retain([]);
    expect(get(drafts)).toBe("");
  });

  it("persists drafts via deferred write, flushes on demand / flushPendingWrites, and restores across reloads", () => {
    vi.useFakeTimers();
    try {
      const storage = memoryStorage();
      const first = createCommitDraftStore({ storage, flushDelayMs: 400 });
      expect(first.hasDrafts()).toBe(false);

      first.setActiveTab("tab-1");
      first.set("feat: preserve commit drafts across hibernation");
      expect(first.hasDrafts()).toBe(true);
      // Deferred: not yet written before timer fires.
      expect(storage.map.has(STORAGE_KEY_COMMIT_DRAFTS)).toBe(false);

      vi.advanceTimersByTime(400);
      expect(JSON.parse(storage.map.get(STORAGE_KEY_COMMIT_DRAFTS)!)).toEqual({
        "tab-1": "feat: preserve commit drafts across hibernation",
      });

      first.setActiveTab("tab-2");
      first.set("wip: second tab");
      // Explicit flush persists immediately without waiting for timer.
      first.flush();
      expect(JSON.parse(storage.map.get(STORAGE_KEY_COMMIT_DRAFTS)!)).toEqual({
        "tab-1": "feat: preserve commit drafts across hibernation",
        "tab-2": "wip: second tab",
      });

      // Global flushPendingWrites flushes dirty drafts too.
      first.set("wip: updated via pagehide flush");
      flushPendingWrites();

      const restored = createCommitDraftStore({ storage });
      expect(restored.hasDrafts()).toBe(true);
      expect(restored.read("tab-1")).toBe("feat: preserve commit drafts across hibernation");
      expect(restored.read("tab-2")).toBe("wip: updated via pagehide flush");
      restored.setActiveTab("tab-2");
      expect(get(restored)).toBe("wip: updated via pagehide flush");

      // Clearing all drafts removes the persisted key on flush.
      restored.retain([]);
      restored.flush();
      expect(restored.hasDrafts()).toBe(false);
      expect(storage.map.has(STORAGE_KEY_COMMIT_DRAFTS)).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it("ignores corrupt or oversized persisted draft payloads safely", () => {
    const corrupt = memoryStorage({ [STORAGE_KEY_COMMIT_DRAFTS]: "{not-valid-json" });
    const storeA = createCommitDraftStore({ storage: corrupt });
    expect(storeA.hasDrafts()).toBe(false);

    const wrongShape = memoryStorage({ [STORAGE_KEY_COMMIT_DRAFTS]: '["array-not-object"]' });
    const storeB = createCommitDraftStore({ storage: wrongShape });
    expect(storeB.hasDrafts()).toBe(false);

    const mixed = memoryStorage({
      [STORAGE_KEY_COMMIT_DRAFTS]: JSON.stringify({
        "": "empty-id-skipped",
        "empty-val": "",
        "non-string": 42,
        valid: "kept draft",
      }),
    });
    const storeC = createCommitDraftStore({ storage: mixed });
    expect(storeC.read("valid")).toBe("kept draft");
    expect(storeC.read("empty-val")).toBe("");
    expect(storeC.read("non-string")).toBe("");
  });
});
