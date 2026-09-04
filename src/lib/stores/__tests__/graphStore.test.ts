import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import type { VisualCommitRow } from "../../canvas/GraphRenderer";
import {
  createGraphStore,
  graphPayloadSignature,
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

  it("surfaces backend read warnings in state and the diagnostics channel, once, named by repo", async () => {
    // Asserted on the injected diagnostics seam rather than by spying on
    // console.warn: the store used to do BOTH, so every warning was recorded
    // twice — and the console copy fired on every load, not only when the
    // warning set changed. Spying on the global console is also why that
    // duplication went unnoticed; this seam is the channel that matters.
    const warned: Array<[string, unknown]> = [];
    const consoleWarned: string[] = [];
    const originalWarn = console.warn;
    console.warn = (msg?: unknown) => {
      consoleWarned.push(String(msg));
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
      const store = createGraphStore({
        invoke,
        diagnostics: { warn: (source, detail) => warned.push([source, detail]) },
      });
      store.showRepo("/r/a");
      await store.loadGraph("/r/a");
      expect(get(store).warnings).toEqual([
        "ref decorations unavailable: fatal: broken ref",
      ]);
      expect(warned).toEqual([
        ["graph", "/r/a: ref decorations unavailable: fatal: broken ref"],
      ]);
      // A second load of the same warning set is not news, and must not add a
      // second breadcrumb through any channel.
      await store.loadGraph("/r/a");
      expect(warned).toHaveLength(1);
      expect(
        consoleWarned.filter((w) => w.includes("ref decorations unavailable")),
        "the graph warning must reach diagnostics only, never a second console channel"
      ).toEqual([]);
    } finally {
      console.warn = originalWarn;
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

  it("changes when the pinned mainline moves, is renamed, or a row joins it", () => {
    // The straight column-0 rail is rendered state: a payload whose rows
    // are byte-identical except for which chain is pinned must repaint.
    const base = graphPayloadSignature(full());
    const anchored = { ...full(), mainline_id: "a", mainline_name: "main" };
    expect(graphPayloadSignature(anchored)).not.toBe(base);
    expect(graphPayloadSignature({ ...anchored })).toBe(graphPayloadSignature(anchored));

    const renamed = { ...anchored, mainline_name: "origin/main" };
    expect(graphPayloadSignature(renamed)).not.toBe(graphPayloadSignature(anchored));

    const moved = { ...anchored, mainline_id: "b" };
    expect(graphPayloadSignature(moved)).not.toBe(graphPayloadSignature(anchored));

    const rowJoined = full();
    rowJoined.rows = [{ ...rowJoined.rows[0], is_mainline: true }, rowJoined.rows[1]];
    expect(graphPayloadSignature(rowJoined)).not.toBe(base);

    // Absent and null spell the same "unnamed" mainline, as legacy payloads
    // omit the fields entirely.
    expect(graphPayloadSignature({ ...full(), mainline_id: null, mainline_name: null })).toBe(base);
  });
});

describe("graphStore mainline fields", () => {
  const details = { id: "a", summary: "d", changed_files: [] };

  it("carries the pinned mainline into state and back out of the cache", async () => {
    const anchored = {
      rows: [grow("a", { is_mainline: true }), grow("b", { lane: 1, parent_ids: ["a"] })],
      head_id: "b",
      refs: [],
      has_more: false,
      mainline_id: "a",
      mainline_name: "main",
    };
    const invoke: InvokeFn = async (cmd) => {
      if (cmd === "cmd_get_commit_graph") return anchored as never;
      if (cmd === "cmd_get_commit_details") return details as never;
      throw new Error(cmd);
    };
    const store = createGraphStore({ invoke });
    store.showRepo("/r/a");
    await store.loadGraph("/r/a");
    expect(get(store).mainlineId).toBe("a");
    expect(get(store).mainlineName).toBe("main");

    // Switching away and back serves the cached mainline, not a blank one.
    store.showRepo(null);
    expect(get(store).mainlineId).toBeNull();
    store.showRepo("/r/a");
    expect(get(store).mainlineId).toBe("a");
    expect(get(store).mainlineName).toBe("main");
  });

  it("normalizes a legacy payload without mainline fields to an unnamed mainline", async () => {
    const legacy = { rows: [grow("a")], head_id: "a", refs: [], has_more: false };
    const invoke: InvokeFn = async (cmd) => {
      if (cmd === "cmd_get_commit_graph") return legacy as never;
      if (cmd === "cmd_get_commit_details") return details as never;
      throw new Error(cmd);
    };
    const store = createGraphStore({ invoke });
    store.showRepo("/r/a");
    await store.loadGraph("/r/a");
    expect(get(store).mainlineId).toBeNull();
    expect(get(store).mainlineName).toBeNull();
  });
});

describe("graphStore background reload stability", () => {
  function clone<T>(value: T): T {
    return JSON.parse(JSON.stringify(value)) as T;
  }
  const payload = () => ({
    rows: [grow("a")],
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
        return { rows: [], head_id: null } as never;
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


describe("graphStore loadGraph query forwarding", () => {
  const details = { id: "a", summary: "d", changed_files: [], total_additions: 0, total_deletions: 0 };

  it("forwards every query to cmd_get_commit_graph in canonical form", async () => {
    // The backend owns the filter language (author, sha, type, text, path)
    // and rewrites history so filtered graphs stay connected; the client
    // only normalizes whitespace so the cache and the scheduler key agree.
    const forwarded: unknown[] = [];
    const invoke: InvokeFn = async (cmd, args) => {
      if (cmd === "cmd_get_commit_graph") {
        forwarded.push(args?.query);
        return payload("a") as never;
      }
      if (cmd === "cmd_get_commit_details") return details as never;
      throw new Error(cmd);
    };
    const store = createGraphStore({ invoke });
    store.showRepo("/r/a");
    await store.loadGraph("/r/a", "author:ada");
    expect(forwarded).toEqual(["author:ada"]);
    await store.loadGraph("/r/a", "  path:src   fix:  ");
    expect(forwarded[1]).toBe("path:src fix:");
    await store.loadGraph("/r/a", "   ");
    expect(forwarded[2]).toBeNull();
  });

  it("shows the loading state while a different query or branch loads over cached rows", async () => {
    // Stale-while-revalidate is for reloads of the SAME view. When the rows
    // on screen answer a different query, the user just typed a filter and
    // must see it working; a silent row swap later reads as a broken filter.
    const gate = deferred<void>();
    let calls = 0;
    const invoke: InvokeFn = async (cmd) => {
      if (cmd === "cmd_get_commit_graph") {
        calls += 1;
        if (calls === 1) return payload("a") as never;
        await gate.promise;
        return payload("b") as never;
      }
      if (cmd === "cmd_get_commit_details") return details as never;
      throw new Error(cmd);
    };
    const store = createGraphStore({ invoke });
    store.showRepo("/r/a");
    await store.loadGraph("/r/a");
    expect(get(store).isLoading).toBe(false);

    const refine = store.loadGraph("/r/a", "author:ada");
    expect(get(store).isLoading).toBe(true);
    // The cached rows stay up while the filtered graph is fetched.
    expect(get(store).rows.map((r) => r.id)).toEqual(["a"]);
    gate.resolve();
    await refine;
    expect(get(store).isLoading).toBe(false);
    expect(get(store).rows.map((r) => r.id)).toEqual(["b"]);

    // A same-view background reload still never flashes the bar.
    const again = store.loadGraph("/r/a", "author:ada");
    expect(get(store).isLoading).toBe(false);
    await again;

    // Switching branches over cached rows is a different view too.
    const branch = store.loadGraph("/r/a", "author:ada", "dev");
    expect(get(store).isLoading).toBe(true);
    await branch;
    expect(get(store).isLoading).toBe(false);
  });
});


describe("graphStore background reload stability", () => {
  function clone<T>(value: T): T {
    return JSON.parse(JSON.stringify(value)) as T;
  }
  const payload = () => ({
    rows: [grow("a")],
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
        return { rows: [], head_id: null } as never;
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


describe("graph ref scope", () => {
  /**
   * The scope decides which refs the backend walks, so two scopes answer the
   * same repository with different rows. Sending it is not optional: without
   * it every load would silently take the backend's default, and the setting
   * would appear to do nothing.
   */
  it("sends the current ref scope with every load", async () => {
    const seen: unknown[] = [];
    const invoke: InvokeFn = async (cmd, args) => {
      if (cmd === "cmd_get_commit_graph") {
        seen.push(args?.refScope);
        return payload("a") as never;
      }
      if (cmd === "cmd_get_commit_details") {
        return { id: args?.commitId, changed_files: [] } as never;
      }
      throw new Error(cmd);
    };
    let scope: "named" | "all" = "named";
    const store = createGraphStore({ invoke, refScope: () => scope });
    store.showRepo("/r/a");
    await store.loadGraph("/r/a");
    scope = "all";
    await store.loadGraph("/r/a");
    expect(seen).toEqual(["named", "all"]);
  });

  /**
   * A scope change is a different question, not a background refresh of the
   * same one. The store shows the loading state for a view it has no answer
   * for yet — the user just changed what is being asked, and must see it
   * working rather than get a silent row swap half a second later. Without
   * the scope in the cache key this reads as a plain reload and the graph
   * sits on the old scope's lanes with no indication.
   */
  it("shows the loading state while a scope change is in flight", async () => {
    const slow = deferred<ReturnType<typeof payload>>();
    let scope: "named" | "all" = "named";
    const invoke: InvokeFn = async (cmd, args) => {
      if (cmd === "cmd_get_commit_graph") {
        return scope === "named" ? (payload("a") as never) : (slow.promise as never);
      }
      if (cmd === "cmd_get_commit_details") {
        return { id: args?.commitId, changed_files: [] } as never;
      }
      throw new Error(cmd);
    };
    const store = createGraphStore({ invoke, refScope: () => scope });
    store.showRepo("/r/a");
    await store.loadGraph("/r/a");
    expect(get(store).isLoading).toBe(false);

    scope = "all";
    const pending = store.loadGraph("/r/a");
    expect(get(store).isLoading).toBe(true);

    slow.resolve(payload("b"));
    await pending;
    expect(get(store).isLoading).toBe(false);
    expect(get(store).rows.map((r) => r.id)).toEqual(["b"]);
  });
});

describe("graph warning breadcrumbs", () => {
  function storeWith(warnings: () => string[]) {
    const warned: string[] = [];
    const invoke: InvokeFn = async (cmd, args) => {
      if (cmd === "cmd_get_commit_graph") {
        return { ...payload("a"), warnings: warnings() } as never;
      }
      if (cmd === "cmd_get_commit_details") {
        return { id: args?.commitId, changed_files: [] } as never;
      }
      throw new Error(cmd);
    };
    const store = createGraphStore({
      invoke,
      diagnostics: { warn: (_source, detail) => void warned.push(String(detail)) },
    });
    return { store, warned };
  }

  /**
   * A repository whose refs sit outside the walked scope reports the same
   * sentence on every load, and the watcher reloads on every settled write.
   * The diagnostics ring coalesces identical CONSECUTIVE entries, so one
   * persistent warning is fine — but two alternate, each displacing the other
   * as newest, and the ring fills with the same pair forever.
   */
  it("logs a persistent warning set once, not on every reload", async () => {
    const { store, warned } = storeWith(() => [
      "36 commit(s) ... are not drawn",
      "Ref labels are capped for this repository",
    ]);
    store.showRepo("/r/a");
    for (let i = 0; i < 5; i += 1) await store.loadGraph("/r/a");
    expect(warned).toHaveLength(2);
  });

  /**
   * The rows are byte-identical across these loads; only the warning set
   * moves. Warnings are not part of the rendered-history signature, so the
   * identical-payload short-circuit used to return before they were reported
   * — a degradation that appeared while history stood still (a ref listing
   * starting to fail, a namespace starting to hide commits) was never logged
   * at all. Whether history changed and whether the load degraded are two
   * different questions.
   */
  it("logs again as soon as the set actually changes", async () => {
    let current = ["first degradation"];
    const { store, warned } = storeWith(() => current);
    store.showRepo("/r/a");
    await store.loadGraph("/r/a");
    await store.loadGraph("/r/a");
    expect(warned).toEqual(["/r/a: first degradation"]);

    current = ["first degradation", "second degradation"];
    await store.loadGraph("/r/a");
    expect(warned).toEqual([
      "/r/a: first degradation",
      "/r/a: first degradation",
      "/r/a: second degradation",
    ]);
  });

  /** A degradation that clears and returns must be reported again. */
  it("re-reports a warning that went away and came back", async () => {
    let current: string[] = ["flaky degradation"];
    const { store, warned } = storeWith(() => current);
    store.showRepo("/r/a");
    await store.loadGraph("/r/a");
    current = [];
    await store.loadGraph("/r/a");
    current = ["flaky degradation"];
    await store.loadGraph("/r/a");
    expect(warned).toEqual(["/r/a: flaky degradation", "/r/a: flaky degradation"]);
  });
});
