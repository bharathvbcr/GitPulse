import { describe, expect, it } from "vitest";
import {
  editorDraft,
  emptyEditorTabs,
  hasDirtyEditorTabs,
  openPreview,
  updateEditorDraft,
  type EditorTabState,
} from "../files/editorTabs";
import { createRepoPanelCache } from "./repoPanelCache";

describe("repoPanelCache", () => {
  it("round-trips set/get per repo path", () => {
    const cache = createRepoPanelCache<{ n: number }>();
    expect(cache.get("/repo/a")).toBeUndefined();
    cache.set("/repo/a", { n: 1 });
    expect(cache.get("/repo/a")).toEqual({ n: 1 });
    cache.set("/repo/b", { n: 2 });
    expect(cache.get("/repo/a")).toEqual({ n: 1 });
    expect(cache.get("/repo/b")).toEqual({ n: 2 });
  });

  it("overwrites the value for an existing path without growing", () => {
    const cache = createRepoPanelCache<number>();
    cache.set("/repo/a", 1);
    cache.set("/repo/a", 9);
    expect(cache.get("/repo/a")).toBe(9);
    expect(cache.size).toBe(1);
  });

  it("evicts least-recently-used entries beyond maxRepos (default 8)", () => {
    const cache = createRepoPanelCache<number>();
    for (let i = 0; i < 8; i++) cache.set(`/repo/${i}`, i);
    expect(cache.size).toBe(8);
    // Touch repo/0 so it becomes most-recently used; repo/1 is now the LRU head.
    expect(cache.get("/repo/0")).toBe(0);
    cache.set("/repo/new", 99);
    expect(cache.size).toBe(8);
    expect(cache.get("/repo/1")).toBeUndefined();
    expect(cache.get("/repo/0")).toBe(0);
    expect(cache.get("/repo/new")).toBe(99);
  });

  it("honors a custom maxRepos bound", () => {
    const cache = createRepoPanelCache<number>({ maxRepos: 2 });
    cache.set("/a", 1);
    cache.set("/b", 2);
    cache.set("/c", 3);
    expect(cache.size).toBe(2);
    expect(cache.get("/a")).toBeUndefined();
    expect(cache.get("/c")).toBe(3);
  });

  it("get() refreshes recency so re-read paths survive eviction", () => {
    const cache = createRepoPanelCache<number>({ maxRepos: 2 });
    cache.set("/a", 1);
    cache.set("/b", 2);
    cache.get("/a"); // /a is now MRU, /b is LRU
    cache.set("/c", 3);
    expect(cache.get("/b")).toBeUndefined();
    expect(cache.get("/a")).toBe(1);
    expect(cache.get("/c")).toBe(3);
  });

  it("clear() empties every entry", () => {
    const cache = createRepoPanelCache<number>();
    cache.set("/a", 1);
    cache.set("/b", 2);
    cache.clear();
    expect(cache.size).toBe(0);
    expect(cache.get("/a")).toBeUndefined();
    expect(cache.get("/b")).toBeUndefined();
  });

  it("instances are independent", () => {
    const health = createRepoPanelCache<string>();
    const storage = createRepoPanelCache<string>();
    health.set("/repo/a", "health");
    storage.set("/repo/a", "storage");
    expect(health.get("/repo/a")).toBe("health");
    expect(storage.get("/repo/a")).toBe("storage");
    health.clear();
    expect(health.size).toBe(0);
    expect(storage.get("/repo/a")).toBe("storage");
  });

  it("does not evict protected entries when the soft bound is full", () => {
    const cache = createRepoPanelCache<{ dirty: boolean }>({
      maxRepos: 2,
      canEvict: (value) => !value.dirty,
    });
    cache.set("/dirty/a", { dirty: true });
    cache.set("/dirty/b", { dirty: true });
    cache.set("/clean", { dirty: false });

    expect(cache.get("/dirty/a")).toEqual({ dirty: true });
    expect(cache.get("/dirty/b")).toEqual({ dirty: true });
    expect(cache.get("/clean")).toBeUndefined();
    expect(cache.size).toBe(2);
  });

  it("round-trips independent same-path drafts across repository switches and remounts", () => {
    const cache = createRepoPanelCache<EditorTabState>({
      canEvict: (state) => !hasDirtyEditorTabs(state),
    });
    const stateFor = (draft: string) => {
      let state = openPreview(emptyEditorTabs(), "src/shared.ts");
      state = updateEditorDraft(state, "src/shared.ts", draft, "disk");
      return state;
    };
    cache.set("/repo/a", stateFor("repo a draft"));
    cache.set("/repo/b", stateFor("repo b draft"));

    const repoA = cache.get("/repo/a");
    const repoB = cache.get("/repo/b");
    expect(repoA).toBeDefined();
    expect(repoB).toBeDefined();
    if (!repoA || !repoB) throw new Error("draft cache entry unexpectedly missing");
    expect(editorDraft(repoA, "src/shared.ts")?.content).toBe("repo a draft");
    expect(editorDraft(repoB, "src/shared.ts")?.content).toBe("repo b draft");
  });
});
