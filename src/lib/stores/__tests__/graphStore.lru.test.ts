import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { createGraphStore, type InvokeFn } from "../graphStore";
import { DEFAULT_MAX_COMMITS, GRAPH_CACHE_LIMIT } from "../graphLimits";

function row(id: string) {
  return { id, summary: id, author_name: "ada", timestamp: 1, lane: 0, parent_ids: [] };
}

/** Two commits a repository, named after it, so a restored graph proves which repository it is. */
function fakeInvoke(calls: { graph: { path: string; maxCommits: number }[] } = { graph: [] }): InvokeFn {
  return async (cmd, args) => {
    if (cmd === "cmd_get_commit_graph") {
      const path = String(args?.repoPath);
      calls.graph.push({ path, maxCommits: Number(args?.maxCommits) });
      return { rows: [row(`${path}#1`), row(`${path}#2`)], head_id: `${path}#1` } as never;
    }
    if (cmd === "cmd_get_commit_details") {
      return { id: args?.commitId, summary: "d", changed_files: [] } as never;
    }
    throw new Error(cmd);
  };
}

async function visit(store: ReturnType<typeof createGraphStore>, path: string) {
  store.showRepo(path);
  await store.loadGraph(path);
}

describe("graphStore keeps rows for a bounded number of repositories", () => {
  it("with 30 visited tabs, at most GRAPH_CACHE_LIMIT render from memory", async () => {
    const store = createGraphStore({ invoke: fakeInvoke() });
    const paths = Array.from({ length: 30 }, (_, i) => `/r/${i}`);
    for (const path of paths) await visit(store, path);

    // The observable consequence: switching back shows a populated pane from
    // memory only for the retained repositories; the rest show the loading
    // state until their reload lands.
    let fromMemory = 0;
    for (const path of paths) {
      store.showRepo(path);
      if (!get(store).isLoading && get(store).rows.length > 0) fromMemory += 1;
    }
    expect(fromMemory).toBeLessThanOrEqual(6);
    expect(store.retainedGraphs()).toBeLessThanOrEqual(GRAPH_CACHE_LIMIT);
  });

  it("sheds the least recently used repository, not the oldest one opened", async () => {
    const store = createGraphStore({ invoke: fakeInvoke(), cacheLimit: 3 });
    for (const path of ["/r/a", "/r/b", "/r/c"]) await visit(store, path);
    store.showRepo("/r/a"); // a is now the most recently used
    await visit(store, "/r/d"); // sheds b, the least recently used

    store.showRepo("/r/a");
    expect(get(store).isLoading).toBe(false);
    expect(get(store).rows).toHaveLength(2);
    store.showRepo("/r/b");
    expect(get(store).isLoading).toBe(true);
    expect(get(store).rows).toHaveLength(0);
  });

  it("never sheds the repository on screen, however long ago it loaded", async () => {
    const store = createGraphStore({ invoke: fakeInvoke(), cacheLimit: 2 });
    await visit(store, "/r/a");
    // a stays visible while other repositories finish loading in the background.
    await store.loadGraph("/r/b");
    await store.loadGraph("/r/c");
    await store.loadGraph("/r/d");

    expect(get(store).visiblePath).toBe("/r/a");
    expect(get(store).rows).toHaveLength(2);
    store.showRepo("/r/a");
    expect(get(store).isLoading).toBe(false);
  });

  it("reloads a shed repository at the page size and selection it had", async () => {
    const calls = { graph: [] as { path: string; maxCommits: number }[] };
    const store = createGraphStore({ invoke: fakeInvoke(calls), cacheLimit: 1 });
    store.showRepo("/r/a");
    await store.loadGraph("/r/a", "", null, { forceLimit: 10_000 });
    const second = get(store).rows[1];
    expect(second).toBeDefined();
    await store.selectCommit(second!, "/r/a");

    await visit(store, "/r/b"); // sheds a

    store.showRepo("/r/a");
    expect(get(store).isLoading).toBe(true);
    await store.loadGraph("/r/a");

    // Same page size as before it was shed, not the default.
    expect(calls.graph.at(-1)).toEqual({ path: "/r/a", maxCommits: 10_000 });
    expect(get(store).rows).toHaveLength(2);
    expect(get(store).selectedCommit?.id).toBe("/r/a#2");
  });

  it("forgets a closed tab's shed view with the rest of what it held", async () => {
    const calls = { graph: [] as { path: string; maxCommits: number }[] };
    const store = createGraphStore({ invoke: fakeInvoke(calls), cacheLimit: 1 });
    store.showRepo("/r/a");
    await store.loadGraph("/r/a", "", null, { forceLimit: 10_000 });
    await visit(store, "/r/b"); // sheds a, keeping its view

    store.evict("/r/a");
    await store.loadGraph("/r/a");

    expect(calls.graph.at(-1)).toEqual({ path: "/r/a", maxCommits: DEFAULT_MAX_COMMITS });
  });
});
