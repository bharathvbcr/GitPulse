import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import type { VisualCommitRow } from "../../canvas/GraphRenderer";
import {
  createGraphStore,
  graphPayloadSignature,
  serverFetchableQuery,
  type InvokeFn,
  type RefDecoration,
} from "../graphStore";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}


function grow(id: string, overrides: Partial<VisualCommitRow> = {}): VisualCommitRow {
  return {
    id,
    parent_ids: [],
    summary: id,
    author_name: "ada",
    author_email: "ada@example.com",
    timestamp: 1,
    lane: 0,
    color_index: 0,
    active_lanes: [0],
    active_lane_colors: [0],
    connections: [],
    is_merge: false,
    is_root: false,
    ...overrides,
  };
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

  it("keeps an evicted repo's in-flight load dead after pruning its ordering state", async () => {
    // evict() deletes the generation/selection entries for the path. That is
    // only safe because tokens come from store-wide monotonic counters: a
    // per-path counter reset to 1 would hand the fresh load the same token
    // the orphaned fetch holds, and the stale payload would win.
    const gates = [deferred<ReturnType<typeof payload>>(), deferred<ReturnType<typeof payload>>()];
    let graphCall = 0;
    const invoke: InvokeFn = async (cmd, args) => {
      if (cmd === "cmd_get_commit_graph") {
        graphCall += 1;
        if (graphCall === 1) return gates[0].promise as never;
        return payload("fresh") as never;
      }
      if (cmd === "cmd_get_commit_details") {
        return { id: args?.commitId, summary: "d", changed_files: [], total_additions: 0, total_deletions: 0 } as never;
      }
      throw new Error(cmd);
    };
    const store = createGraphStore({ invoke });
    store.showRepo("/r/prune");
    const staleLoad = store.loadGraph("/r/prune");

    store.evict("/r/prune");
    store.showRepo("/r/prune");
    await store.loadGraph("/r/prune");
    expect(get(store).rows[0]?.id).toBe("fresh");

    // The pre-evict fetch resolves last; it must stay dead.
    gates[0].resolve(payload("stale"));
    await staleLoad;
    expect(get(store).rows[0]?.id).toBe("fresh");
  });

  it("surfaces backend read warnings in state and the diagnostics channel", async () => {
    const warned: string[] = [];
    const original = console.warn;
    console.warn = (msg?: unknown) => {
      warned.push(String(msg));
    };
    try {
      const withWarnings = {
        ...payload("w"),
        refs: [],
        has_more: false,
        warnings: ["ref decorations unavailable: fatal: broken ref"],
      };
      const invoke: InvokeFn = async (cmd) => {
        if (cmd === "cmd_get_commit_graph") return withWarnings as never;
        if (cmd === "cmd_get_commit_details")
          return { id: "x", summary: "d", changed_files: [] } as never;
        throw new Error(cmd);
      };
      const store = createGraphStore({ invoke });
      store.showRepo("/r/a");
      await store.loadGraph("/r/a");
      expect(get(store).warnings).toEqual([
        "ref decorations unavailable: fatal: broken ref",
      ]);
      expect(
        warned.some((w) => w.includes("ref decorations unavailable")),
        "warnings must reach the console/diagnostics channel"
      ).toBe(true);
    } finally {
      console.warn = original;
    }
  });

  it("defaults warnings to empty for payloads without them", async () => {
    const invoke: InvokeFn = async (cmd) => {
      if (cmd === "cmd_get_commit_graph") return payload("p") as never;
      if (cmd === "cmd_get_commit_details")
        return { id: "x", summary: "d", changed_files: [] } as never;
      throw new Error(cmd);
    };
    const store = createGraphStore({ invoke });
    store.showRepo("/r/a");
    await store.loadGraph("/r/a");
    expect(get(store).warnings).toEqual([]);
  });
});

describe("graphStore payload signature", () => {
  const full = () => ({
    rows: [
      grow("a"),
      grow("b", { lane: 1, parent_ids: ["a"] }),
    ],
    folds: [],
    head_id: "a",
    // Typed as the payload contract (RefDecoration) rather than left to local
    // inference: the "changes whenever rendered history changes" case adds a
    // tag ref, which the locally-inferred "local"-only literal type rejects.
    refs: [{ name: "main", kind: "local", commit_id: "a", is_head: true }] as RefDecoration[],
    has_more: true,
  });

  it("is equal for structurally identical payloads with fresh identities", () => {
    expect(graphPayloadSignature(full())).toBe(graphPayloadSignature(full()));
  });

  it("changes whenever rendered history changes", () => {
    const base = graphPayloadSignature(full());
    const headMoved = full();
    headMoved.head_id = "b";
    expect(graphPayloadSignature(headMoved)).not.toBe(base);

    const grew = full();
    grew.rows = [...grew.rows, grow("c", { parent_ids: ["b"] })];
    expect(graphPayloadSignature(grew)).not.toBe(base);

    const refAdded = full();
    refAdded.refs = [...refAdded.refs, { name: "v1", kind: "tag", commit_id: "b", is_head: false } satisfies RefDecoration];
    expect(graphPayloadSignature(refAdded)).not.toBe(base);
  });
});

describe("graphStore background reload stability", () => {
  function clone<T>(value: T): T {
    return JSON.parse(JSON.stringify(value)) as T;
  }
  const payload = () => ({
    rows: [grow("a")],
    folds: [],
    head_id: "a",
    refs: [{ name: "main", kind: "local" as const, commit_id: "a", is_head: true }],
    has_more: false,
  });
  const details: InvokeFn = async (_cmd, args) =>
    ({ id: args?.commitId, summary: "d", changed_files: [], total_additions: 0, total_deletions: 0 }) as never;
  const flushMicro = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

  it("an identical background reload emits nothing and preserves row identity", async () => {
    let graphCalls = 0;
    const slow = deferred<unknown>();
    const store = createGraphStore({
      invoke: async (cmd, args) => {
        if (cmd === "cmd_get_commit_graph") {
          graphCalls += 1;
          // Hold the SECOND load in flight to observe stale-while-revalidate.
          if (graphCalls === 2) return slow.promise as never;
          return clone(payload()) as never;
        }
        return details(cmd, args);
      },
    });
    store.showRepo("/r/hold");
    await store.loadGraph("/r/hold");
    const rowsBefore = get(store).rows;

    let emissions = 0;
    const unsub = store.subscribe(() => {
      emissions += 1;
    });
    await flushMicro(); // writable fires the subscriber once on subscribe
    const baseline = emissions;

    const reloading = store.loadGraph("/r/hold");
    await flushMicro();
    // Cached rows stay presented: no spinner flip, no subscriber churn.
    expect(get(store).isLoading).toBe(false);
    expect(emissions - baseline).toBe(0);

    slow.resolve(clone(payload()));
    await reloading;
    // Identical payload: still nothing, and row identities survive.
    expect(emissions - baseline).toBe(0);
    expect(get(store).rows).toBe(rowsBefore);
    unsub();
  });

  it("a genuinely changed payload still republishes", async () => {
    const firstPayload = {
      rows: [{ id: "a", summary: "a", author_name: "ada", timestamp: 1, lane: 0, parent_ids: [] }],
      folds: [],
      head_id: "a",
      refs: [] as Array<{ name: string; kind: "local"; commit_id: string; is_head: boolean }>,
      has_more: false,
    };
    const secondPayload = {
      ...firstPayload,
      head_id: "new",
      rows: [
        { id: "new", summary: "n", author_name: "ada", timestamp: 2, lane: 0, parent_ids: ["a"] },
        ...firstPayload.rows,
      ],
    };
    let calls = 0;
    const store = createGraphStore({
      invoke: async (cmd, args) => {
        if (cmd === "cmd_get_commit_graph") {
          calls += 1;
          return clone(calls === 1 ? firstPayload : secondPayload) as never;
        }
        return details(cmd, args);
      },
    });
    store.showRepo("/r/move");
    await store.loadGraph("/r/move");
    const rowsBefore = get(store).rows;
    await store.loadGraph("/r/move");
    expect(get(store).rows).not.toBe(rowsBefore);
    expect(get(store).headId).toBe("new");
  });
});

describe("graphStore background load failure breadcrumb", () => {
  function makeInvoke(failPaths: Set<string>): InvokeFn {
    return async (cmd, args) => {
      if (cmd === "cmd_get_commit_graph") {
        const path = String(args?.repoPath);
        if (failPaths.has(path)) throw new Error("index.lock held");
        return { rows: [], folds: [], head_id: null } as never;
      }
      if (cmd === "cmd_get_commit_details") {
        return { id: args?.commitId, summary: "d", changed_files: [] } as never;
      }
      throw new Error(cmd);
    };
  }

  it("leaves a diagnostics breadcrumb when a non-visible repo's load fails", async () => {
    const warnings: Array<{ source: string; detail: string }> = [];
    const store = createGraphStore({
      invoke: makeInvoke(new Set(["/r/broken-bg"])),
      diagnostics: {
        warn: (source, detail) => warnings.push({ source, detail: String(detail) }),
      },
    });
    // A different repo owns the pane; the failing load is pure background.
    store.showRepo("/r/watching");
    await store.loadGraph("/r/watching");
    await store.loadGraph("/r/broken-bg");

    expect(warnings).toHaveLength(1);
    expect(warnings[0].source).toBe("graph-load");
    expect(warnings[0].detail).toContain("/r/broken-bg");
    expect(warnings[0].detail).toContain("index.lock held");

    // The visible pane's state must be untouched by the invisible failure.
    expect(get(store).visiblePath).toBe("/r/watching");
    expect(get(store).error).toBe(null);
    expect(get(store).isLoading).toBe(false);
  });

  it("does not double-report a visible failure — the error banner is the breadcrumb", async () => {
    const warnings: Array<{ source: string; detail: string }> = [];
    const store = createGraphStore({
      invoke: makeInvoke(new Set(["/r/failed-visible"])),
      diagnostics: {
        warn: (source, detail) => warnings.push({ source, detail: String(detail) }),
      },
    });
    store.showRepo("/r/failed-visible");
    await store.loadGraph("/r/failed-visible");

    expect(warnings).toHaveLength(0);
    expect(get(store).error).toContain("index.lock held");
  });
});

describe("serverFetchableQuery", () => {
  it("lets only path-style filters through to the backend", () => {
    expect(serverFetchableQuery("path:src")).toBe("path:src");
    // A path token anywhere in the query makes the whole walk server-side.
    expect(serverFetchableQuery("author:ada path:src")).toBe("author:ada path:src");
  });

  it("blanks client-side queries so they cannot launder rows into the cache", () => {
    expect(serverFetchableQuery("author:x")).toBe("");
    expect(serverFetchableQuery("fix(ui)")).toBe("");
    expect(serverFetchableQuery("sha:abc123")).toBe("");
    expect(serverFetchableQuery("")).toBe("");
    expect(serverFetchableQuery("   ")).toBe("");
    // Whitespace around a real path token still counts.
    expect(serverFetchableQuery("  path:lib  ")).toBe("  path:lib  ");
  });
});

describe("graphStore payload signature", () => {
  const full = () => ({
    rows: [
      grow("a"),
      grow("b", { lane: 1, parent_ids: ["a"] }),
    ],
    folds: [],
    head_id: "a",
    // Typed as the payload contract (RefDecoration) rather than left to local
    // inference: the "changes whenever rendered history changes" case adds a
    // tag ref, which the locally-inferred "local"-only literal type rejects.
    refs: [{ name: "main", kind: "local", commit_id: "a", is_head: true }] as RefDecoration[],
    has_more: true,
  });

  it("is equal for structurally identical payloads with fresh identities", () => {
    expect(graphPayloadSignature(full())).toBe(graphPayloadSignature(full()));
  });

  it("changes whenever rendered history changes", () => {
    const base = graphPayloadSignature(full());
    const headMoved = full();
    headMoved.head_id = "b";
    expect(graphPayloadSignature(headMoved)).not.toBe(base);

    const grew = full();
    grew.rows = [...grew.rows, grow("c", { parent_ids: ["b"] })];
    expect(graphPayloadSignature(grew)).not.toBe(base);

    const refAdded = full();
    refAdded.refs = [...refAdded.refs, { name: "v1", kind: "tag", commit_id: "b", is_head: false } satisfies RefDecoration];
    expect(graphPayloadSignature(refAdded)).not.toBe(base);
  });
});

describe("graphStore background reload stability", () => {
  function clone<T>(value: T): T {
    return JSON.parse(JSON.stringify(value)) as T;
  }
  const payload = () => ({
    rows: [grow("a")],
    folds: [],
    head_id: "a",
    refs: [{ name: "main", kind: "local" as const, commit_id: "a", is_head: true }],
    has_more: false,
  });
  const details: InvokeFn = async (_cmd, args) =>
    ({ id: args?.commitId, summary: "d", changed_files: [], total_additions: 0, total_deletions: 0 }) as never;
  const flushMicro = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

  it("an identical background reload emits nothing and preserves row identity", async () => {
    let graphCalls = 0;
    const slow = deferred<unknown>();
    const store = createGraphStore({
      invoke: async (cmd, args) => {
        if (cmd === "cmd_get_commit_graph") {
          graphCalls += 1;
          // Hold the SECOND load in flight to observe stale-while-revalidate.
          if (graphCalls === 2) return slow.promise as never;
          return clone(payload()) as never;
        }
        return details(cmd, args);
      },
    });
    store.showRepo("/r/hold");
    await store.loadGraph("/r/hold");
    const rowsBefore = get(store).rows;

    let emissions = 0;
    const unsub = store.subscribe(() => {
      emissions += 1;
    });
    await flushMicro(); // writable fires the subscriber once on subscribe
    const baseline = emissions;

    const reloading = store.loadGraph("/r/hold");
    await flushMicro();
    // Cached rows stay presented: no spinner flip, no subscriber churn.
    expect(get(store).isLoading).toBe(false);
    expect(emissions - baseline).toBe(0);

    slow.resolve(clone(payload()));
    await reloading;
    // Identical payload: still nothing, and row identities survive.
    expect(emissions - baseline).toBe(0);
    expect(get(store).rows).toBe(rowsBefore);
    unsub();
  });

  it("a genuinely changed payload still republishes", async () => {
    const firstPayload = {
      rows: [{ id: "a", summary: "a", author_name: "ada", timestamp: 1, lane: 0, parent_ids: [] }],
      folds: [],
      head_id: "a",
      refs: [] as Array<{ name: string; kind: "local"; commit_id: string; is_head: boolean }>,
      has_more: false,
    };
    const secondPayload = {
      ...firstPayload,
      head_id: "new",
      rows: [
        { id: "new", summary: "n", author_name: "ada", timestamp: 2, lane: 0, parent_ids: ["a"] },
        ...firstPayload.rows,
      ],
    };
    let calls = 0;
    const store = createGraphStore({
      invoke: async (cmd, args) => {
        if (cmd === "cmd_get_commit_graph") {
          calls += 1;
          return clone(calls === 1 ? firstPayload : secondPayload) as never;
        }
        return details(cmd, args);
      },
    });
    store.showRepo("/r/move");
    await store.loadGraph("/r/move");
    const rowsBefore = get(store).rows;
    await store.loadGraph("/r/move");
    expect(get(store).rows).not.toBe(rowsBefore);
    expect(get(store).headId).toBe("new");
  });
});

describe("graphStore background load failure breadcrumb", () => {
  function makeInvoke(failPaths: Set<string>): InvokeFn {
    return async (cmd, args) => {
      if (cmd === "cmd_get_commit_graph") {
        const path = String(args?.repoPath);
        if (failPaths.has(path)) throw new Error("index.lock held");
        return { rows: [], folds: [], head_id: null } as never;
      }
      if (cmd === "cmd_get_commit_details") {
        return { id: args?.commitId, summary: "d", changed_files: [] } as never;
      }
      throw new Error(cmd);
    };
  }

  it("leaves a diagnostics breadcrumb when a non-visible repo's load fails", async () => {
    const warnings: Array<{ source: string; detail: string }> = [];
    const store = createGraphStore({
      invoke: makeInvoke(new Set(["/r/broken-bg"])),
      diagnostics: {
        warn: (source, detail) => warnings.push({ source, detail: String(detail) }),
      },
    });
    // A different repo owns the pane; the failing load is pure background.
    store.showRepo("/r/watching");
    await store.loadGraph("/r/watching");
    await store.loadGraph("/r/broken-bg");

    expect(warnings).toHaveLength(1);
    expect(warnings[0].source).toBe("graph-load");
    expect(warnings[0].detail).toContain("/r/broken-bg");
    expect(warnings[0].detail).toContain("index.lock held");

    // The visible pane's state must be untouched by the invisible failure.
    expect(get(store).visiblePath).toBe("/r/watching");
    expect(get(store).error).toBe(null);
    expect(get(store).isLoading).toBe(false);
  });

  it("does not double-report a visible failure — the error banner is the breadcrumb", async () => {
    const warnings: Array<{ source: string; detail: string }> = [];
    const store = createGraphStore({
      invoke: makeInvoke(new Set(["/r/failed-visible"])),
      diagnostics: {
        warn: (source, detail) => warnings.push({ source, detail: String(detail) }),
      },
    });
    store.showRepo("/r/failed-visible");
    await store.loadGraph("/r/failed-visible");

    expect(warnings).toHaveLength(0);
    expect(get(store).error).toContain("index.lock held");
  });
});

describe("serverFetchableQuery", () => {
  it("lets only path-style filters through to the backend", () => {
    expect(serverFetchableQuery("path:src")).toBe("path:src");
    // A path token anywhere in the query makes the whole walk server-side.
    expect(serverFetchableQuery("author:ada path:src")).toBe("author:ada path:src");
  });

  it("blanks client-side queries so they cannot launder rows into the cache", () => {
    expect(serverFetchableQuery("author:x")).toBe("");
    expect(serverFetchableQuery("fix(ui)")).toBe("");
    expect(serverFetchableQuery("sha:abc123")).toBe("");
    expect(serverFetchableQuery("")).toBe("");
    expect(serverFetchableQuery("   ")).toBe("");
    // Whitespace around a real path token still counts.
    expect(serverFetchableQuery("  path:lib  ")).toBe("  path:lib  ");
  });
});
