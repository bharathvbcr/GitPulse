import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { createGraphStore, type InvokeFn } from "../graphStore";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

function payload(id: string) {
  return {
    rows: [{ id, summary: id, author_name: "ada", timestamp: 1, lane: 0, parent_ids: [] }],
    folds: [],
    head_id: id,
  };
}

describe("graphStore generation", () => {
  it("does not apply a slower load from repo A after repo B is visible", async () => {
    const slow = deferred<ReturnType<typeof payload>>();
    const invoke: InvokeFn = async (cmd, args) => {
      if (cmd === "cmd_get_commit_graph") {
        const path = String(args?.repoPath);
        if (path === "/r/a") return slow.promise as never;
        return payload("b") as never;
      }
      if (cmd === "cmd_get_commit_details") {
        return { id: args?.commitId, summary: "d", changed_files: [], total_additions: 0, total_deletions: 0 } as never;
      }
      throw new Error(cmd);
    };
    const store = createGraphStore({ invoke });
    store.showRepo("/r/a");
    const loadA = store.loadGraph("/r/a");
    store.showRepo("/r/b");
    await store.loadGraph("/r/b");
    expect(get(store).rows[0]?.id).toBe("b");
    slow.resolve(payload("a"));
    await loadA;
    expect(get(store).rows[0]?.id).toBe("b");
    expect(get(store).visiblePath).toBe("/r/b");
  });

  it("restores a cached graph immediately on showRepo", async () => {
    const invoke: InvokeFn = async (cmd, args) => {
      if (cmd === "cmd_get_commit_graph") return payload(String(args?.repoPath)) as never;
      if (cmd === "cmd_get_commit_details") return { id: "x", summary: "d", changed_files: [] } as never;
      throw new Error(cmd);
    };
    const store = createGraphStore({ invoke });
    store.showRepo("/r/a");
    await store.loadGraph("/r/a");
    store.showRepo("/r/b");
    store.showRepo("/r/a");
    expect(get(store).rows[0]?.id).toBe("/r/a");
    expect(get(store).isLoading).toBe(false);
  });

  it("keeps the previous cache when a refresh fails instead of wiping it", async () => {
    let failNext = false;
    const invoke: InvokeFn = async (cmd, args) => {
      if (cmd === "cmd_get_commit_graph") {
        if (failNext) throw new Error("index.lock held");
        return payload(String(args?.repoPath)) as never;
      }
      if (cmd === "cmd_get_commit_details") return { id: "x", summary: "d", changed_files: [] } as never;
      throw new Error(cmd);
    };
    const store = createGraphStore({ invoke });
    store.showRepo("/r/a");
    await store.loadGraph("/r/a");
    expect(get(store).rows.length).toBe(1);

    failNext = true;
    await store.loadGraph("/r/a");
    expect(get(store).error).toContain("index.lock");
    expect(get(store).rows.length).toBe(1);

    // Tab away and back: the poisoned-empty result must not be served.
    store.showRepo("/r/b");
    store.showRepo("/r/a");
    expect(get(store).rows.length).toBe(1);
    expect(get(store).error).toBe(null);

    failNext = false;
    await store.loadGraph("/r/a");
    expect(get(store).error).toBe(null);
  });

  it("never lets a stale commit selection overwrite a newer one", async () => {
    const detailGates = new Map<string, ReturnType<typeof deferred<unknown>>>();
    const invoke: InvokeFn = async (cmd, args) => {
      if (cmd === "cmd_get_commit_graph") return payload(String(args?.repoPath)) as never;
      if (cmd === "cmd_get_commit_details") {
        const id = String(args?.commitId);
        if (id !== "commit-a" && id !== "commit-b") {
          return { id, summary: "auto", changed_files: [] } as never;
        }
        if (!detailGates.has(id)) detailGates.set(id, deferred());
        return detailGates.get(id)!.promise as never;
      }
      throw new Error(cmd);
    };
    const store = createGraphStore({ invoke });
    store.showRepo("/r/a");
    await store.loadGraph("/r/a");

    const rowA = { ...get(store).rows[0], id: "commit-a", summary: "commit-a" };
    const rowB = { ...rowA, id: "commit-b", summary: "B" };
    const selA = store.selectCommit(rowA, "/r/a");
    const selB = store.selectCommit(rowB, "/r/a");

    // A's details resolve LAST but must lose to B.
    detailGates.get("commit-a")!.resolve({ id: "commit-a", summary: "A", changed_files: [] });
    detailGates.get("commit-b")!.resolve({ id: "commit-b", summary: "B", changed_files: [] });
    await Promise.all([selA, selB]);

    expect(get(store).selectedCommitDetails?.summary).toBe("B");
  });
});
