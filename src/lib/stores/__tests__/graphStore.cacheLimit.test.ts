import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { createGraphStore, type InvokeFn } from "../graphStore";

function row(id: string) {
  return { id, summary: id, author_name: "ada", timestamp: 1, lane: 0, parent_ids: [] };
}

function fakeInvoke(calls: string[] = []): InvokeFn {
  return async (cmd, args) => {
    if (cmd === "cmd_get_commit_graph") {
      const path = String(args?.repoPath);
      calls.push(path);
      return { rows: [row(`${path}#1`), row(`${path}#2`)], head_id: `${path}#1` } as never;
    }
    if (cmd === "cmd_get_commit_details") return { id: args?.commitId, summary: "d", changed_files: [] } as never;
    throw new Error(cmd);
  };
}

async function visit(store: ReturnType<typeof createGraphStore>, path: string) {
  store.showRepo(path);
  await store.loadGraph(path);
}

describe("the graph store's cache limit can change", () => {
  it("reports its limit", () => {
    const store = createGraphStore({ invoke: fakeInvoke(), cacheLimit: 4 });
    expect(store.cacheLimit()).toBe(4);
    store.setCacheLimit(3);
    expect(store.cacheLimit()).toBe(3);
  });

  it("sheds the least recently used repositories at once when the limit shrinks, never the visible one", async () => {
    const store = createGraphStore({ invoke: fakeInvoke(), cacheLimit: 5 });
    for (const path of ["/r/a", "/r/b", "/r/c", "/r/d", "/r/e"]) await visit(store, path);
    expect(store.retainedGraphs()).toBe(5);
    store.showRepo("/r/a");
    store.setCacheLimit(2);
    expect(store.retainedGraphs()).toBe(2);
    // a is visible and was touched last; e was the most recent of the rest.
    expect(get(store).visiblePath).toBe("/r/a");
    expect(get(store).rows.length).toBeGreaterThan(0);
    store.showRepo("/r/e");
    expect(get(store).isLoading).toBe(false);
    expect(get(store).rows.length).toBeGreaterThan(0);
    store.showRepo("/r/b");
    expect(get(store).isLoading).toBe(true);
  });

  it("a larger limit takes effect for the next repository without reloading anything", async () => {
    const calls: string[] = [];
    const store = createGraphStore({ invoke: fakeInvoke(calls), cacheLimit: 1 });
    await visit(store, "/r/a");
    store.setCacheLimit(3);
    await visit(store, "/r/b");
    await visit(store, "/r/c");
    expect(store.retainedGraphs()).toBe(3);
    expect(calls).toEqual(["/r/a", "/r/b", "/r/c"]);
  });

  it("is idempotent when the same limit arrives twice", async () => {
    const store = createGraphStore({ invoke: fakeInvoke(), cacheLimit: 3 });
    for (const path of ["/r/a", "/r/b", "/r/c"]) await visit(store, path);
    store.setCacheLimit(3);
    store.setCacheLimit(3);
    expect(store.retainedGraphs()).toBe(3);
  });

  it("ignores a limit that is not a number and never goes below one", async () => {
    const store = createGraphStore({ invoke: fakeInvoke(), cacheLimit: 3 });
    for (const path of ["/r/a", "/r/b", "/r/c"]) await visit(store, path);
    store.setCacheLimit(Number.NaN);
    store.setCacheLimit(Number.POSITIVE_INFINITY);
    expect(store.cacheLimit()).toBe(3);
    expect(store.retainedGraphs()).toBe(3);
    store.setCacheLimit(0);
    expect(store.cacheLimit()).toBe(1);
    expect(store.retainedGraphs()).toBe(1);
    store.setCacheLimit(-2);
    expect(store.cacheLimit()).toBe(1);
  });
});
