import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import type { VisualCommitRow } from "../../canvas/GraphRenderer";
import { DEFAULT_MAX_COMMITS, LOAD_MORE_STEP, MAX_LOAD_COMMITS } from "../graphLimits";
import {
  createGraphStore,
  type CommitDetails,
  type CommitGraphPayload,
  type InvokeFn,
} from "../graphStore";

function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), a | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/** Lets a test hold backend responses and settle them in any order. */
interface Gate {
  cmd: string;
  args: Record<string, unknown>;
  d: ReturnType<typeof deferred<unknown>>;
  settled: boolean;
}

function createGatedInvoke() {
  const gates: Gate[] = [];
  const invoke: InvokeFn = (cmd, args) => {
    const gate: Gate = { cmd, args: (args ?? {}) as Record<string, unknown>, d: deferred(), settled: false };
    gates.push(gate);
    return gate.d.promise as Promise<never>;
  };
  const live = () => gates.filter((g) => !g.settled);
  return { invoke, gates, live };
}

function row(id: string): VisualCommitRow {
  return {
    id,
    parent_ids: [],
    summary: `summary:${id}`,
    author_name: "ada",
    author_email: "ada@example.com",
    timestamp: 1,
    lane: 0,
    color_index: 0,
    active_lanes: [],
    active_lane_colors: [],
    connections: [],
    is_merge: false,
    is_root: false,
  };
}

let payloadSeq = 0;
function graphPayload(repoPath: string, tag: string, hasMore = true): CommitGraphPayload {
  payloadSeq += 1;
  void repoPath;
  const ids = [`${tag}-a`, `${tag}-b`];
  return { rows: ids.map(row), head_id: ids[0]!, refs: [], has_more: hasMore };
}

function detailsFor(commitId: string): CommitDetails {
  return {
    id: commitId,
    parent_ids: [],
    author_name: "ada",
    author_email: "ada@example.com",
    author_date: "2026-01-01T00:00:00Z",
    committer_name: "ada",
    committer_email: "ada@example.com",
    committer_date: "2026-01-01T00:00:00Z",
    summary: `details:${commitId}`,
    body: "",
    gpg_status: "N",
    co_authors: [],
    changed_files: [],
    total_additions: 0,
    total_deletions: 0,
  };
}

function gateValue(gate: Gate): unknown {
  return gate.cmd === "cmd_get_commit_graph"
    ? graphPayload(String(gate.args.repoPath), `p${gate.args.repoPath}`)
    : detailsFor(String(gate.args.commitId));
}

function settle(gate: Gate, mode: "ok" | "fail", value?: unknown): void {
  if (gate.settled) return;
  gate.settled = true;
  if (mode === "ok") gate.d.resolve(value ?? gateValue(gate));
  else gate.d.reject(value ?? new Error("backend exploded"));
}

/** Yields to every queued store continuation without using real timers. */
async function flush(rounds = 30): Promise<void> {
  for (let i = 0; i < rounds; i += 1) await Promise.resolve();
}

/**
 * Settles outstanding gates until the backend surface stays quiescent,
 * including follow-up details fetches spawned by page loads. Checking
 * liveness only AFTER a flush is essential: a gate settled by the caller
 * spawns its store-side continuations (and possibly a new gate) on
 * microtasks, so a pre-flush emptiness check would exit too early.
 */
async function drainAll(live: () => Gate[], maxPasses = 50): Promise<void> {
  for (let pass = 0; pass < maxPasses; pass += 1) {
    for (const gate of live()) settle(gate, "ok");
    await flush();
    if (live().length === 0) return;
  }
}

describe("graphStore stress", () => {
  it("300-op randomized storm over 4 repos never strands the loader or launders an empty graph", async () => {
    const rng = mulberry32(20260825);
    const pick = <T>(xs: readonly T[]): T => xs[Math.floor(rng() * xs.length)]!;
    const PATHS = ["/storm/a", "/storm/b", "/storm/c", "/storm/d"] as const;

    const backend = createGatedInvoke();
    const store = createGraphStore({ invoke: backend.invoke });

    let selSeq = 0;
    const openPair = (path: string) => {
      // Mirrors App wiring: opening a repo is always followed by its fetch.
      store.showRepo(path);
      void store.loadGraph(path);
    };

    for (let step = 0; step < 300; step += 1) {
      const roll = rng();
      if (roll < 0.18) {
        openPair(pick(PATHS));
      } else if (roll < 0.24) {
        store.showRepo(null);
      } else if (roll < 0.31) {
        store.evict(pick(PATHS));
      } else if (roll < 0.42) {
        void store.loadMore(pick(PATHS));
      } else if (roll < 0.56) {
        void store.selectCommit(row(`sel-${(selSeq += 1)}`), pick(PATHS));
      } else if (roll < 0.86) {
        const liveGates = backend.live();
        if (liveGates.length === 0) openPair(pick(PATHS));
        else {
          const gate = liveGates[Math.floor(rng() * liveGates.length)]!;
          settle(gate, "ok");
          await flush(); // let continuations register follow-up detail fetches
        }
      } else {
        const liveGates = backend.live();
        if (liveGates.length === 0) openPair(pick(PATHS));
        else {
          const gate = liveGates[Math.floor(rng() * liveGates.length)]!;
          settle(gate, "fail");
          await flush();
        }
      }

      // Mid-storm sampler: a visible spinner must always have a genuinely
      // outstanding page fetch behind it, and an error-free visible pane must
      // never sit on empty rows while idle.
      if (step % 25 === 0) {
        await flush();
        const snap = get(store);
        if (snap.visiblePath !== null && snap.isLoading) {
          const feeding = backend
            .live()
            .some((g) => g.cmd === "cmd_get_commit_graph" && String(g.args.repoPath) === snap.visiblePath);
          expect(feeding, `op ${step}: isLoading stuck with no outstanding fetch for ${snap.visiblePath}`).toBe(true);
        }
        if (snap.visiblePath !== null && !snap.isLoading && snap.error === null) {
          expect(snap.rows.length, `op ${step}: silent empty state for ${snap.visiblePath}`).toBeGreaterThan(0);
        }
      }
    }

    // Drain everything: every remaining gate resolves successfully, then any
    // follow-up details fetches those resolutions spawned, until quiescent.
    await drainAll(backend.live);
    expect(backend.live()).toHaveLength(0);
    await flush();

    // With zero outstanding invokes the loader must be released everywhere.
    expect(backend.live()).toHaveLength(0); // re-check after final flush
    const s = get(store);
    if (s.visiblePath !== null) {
      expect(
        s.isLoading,
        `loader stranded on ${s.visiblePath}: rows=${s.rows.length} error=${JSON.stringify(s.error)}`
      ).toBe(false);
      // rows=[] + idle + no error is only legal when nothing is visible
      // (showRepo(null)/evict semantics) — payloads here are never empty.
      if (s.error === null) {
        expect(s.rows.length, `silent empty graph for ${s.visiblePath}`).toBeGreaterThan(0);
      }
    }
  });

  it("out-of-order overlapping loads keep only the newest payload; eviction kills both", async () => {
    const backend = createGatedInvoke();
    const store = createGraphStore({ invoke: backend.invoke });

    // Two overlapping loads for one path, older resolves LAST.
    store.showRepo("/duel/x");
    const older = store.loadGraph("/duel/x");
    const newer = store.loadGraph("/duel/x");

    const [olderGate, newerGate] = backend.gates;
    expect(olderGate?.cmd).toBe("cmd_get_commit_graph");
    expect(newerGate?.cmd).toBe("cmd_get_commit_graph");

    settle(newerGate!, "ok", graphPayload("/duel/x", "new"));
    await flush();
    // State/cache updates happen before the load's internal details await,
    // so the winner is observable without draining yet.
    expect(get(store).rows.map((r) => r.id)).toEqual(["new-a", "new-b"]);

    settle(olderGate!, "ok", graphPayload("/duel/x", "old"));
    await flush();
    // The stale token must not overwrite the newer payload…
    expect(get(store).rows.map((r) => r.id)).toEqual(["new-a", "new-b"]);
    // …and the cache must hold only the winner.
    store.showRepo("/duel/y");
    store.showRepo("/duel/x");
    expect(get(store).rows.map((r) => r.id)).toEqual(["new-a", "new-b"]);
    expect(get(store).isLoading).toBe(false);

    // Settle the spawned details fetches so the overlapping bodies complete.
    await drainAll(backend.live);
    await Promise.all([older, newer]);

    // Evict landing between issue and resolve drops BOTH in-flight results.
    store.showRepo("/duel/y");
    const first = store.loadGraph("/duel/y");
    const second = store.loadGraph("/duel/y");
    store.evict("/duel/y"); // was visible → state resets, tokens orphaned
    expect(get(store).visiblePath).toBeNull();

    const [firstGate, secondGate] = backend.gates.slice(-2);
    settle(secondGate!, "ok", graphPayload("/duel/y", "second")); // newer resolves first
    settle(firstGate!, "ok", graphPayload("/duel/y", "first")); // older resolves last
    await flush();
    await drainAll(backend.live);
    await Promise.all([first, second]);

    expect(get(store).visiblePath).toBeNull();
    expect(get(store).rows).toHaveLength(0);

    // Neither dead token may have seeded the cache: reopening must be cold.
    store.showRepo("/duel/y");
    expect(get(store).isLoading).toBe(true);
    expect(get(store).rows).toHaveLength(0);

    const fresh = store.loadGraph("/duel/y");
    settle(backend.live()[0]!, "ok", graphPayload("/duel/y", "fresh"));
    await drainAll(backend.live);
    await fresh;
    expect(get(store).rows.map((r) => r.id)).toEqual(["fresh-a", "fresh-b"]);
    expect(get(store).isLoading).toBe(false);
    expect(get(store).rows.map((r) => r.id)).toEqual(["fresh-a", "fresh-b"]);
    expect(get(store).isLoading).toBe(false);
  });

  it("loadMore raises the per-repo ceiling coherently and cached repos render instantly on return", async () => {
    const calls: { path: string; maxCommits: number }[] = [];
    const invoke: InvokeFn = async (cmd, args) => {
      if (cmd === "cmd_get_commit_graph") {
        const rec = { path: String(args?.repoPath), maxCommits: Number(args?.maxCommits) };
        calls.push(rec);
        return graphPayload(rec.path, `${rec.path}#${calls.length}`, true) as never;
      }
      if (cmd === "cmd_get_commit_details") return detailsFor(String(args?.commitId)) as never;
      throw new Error(cmd);
    };
    const store = createGraphStore({ invoke });

    store.showRepo("/cap/a");
    await store.loadGraph("/cap/a");
    expect(calls[0]).toEqual({ path: "/cap/a", maxCommits: DEFAULT_MAX_COMMITS });
    expect(get(store).maxCommits).toBe(DEFAULT_MAX_COMMITS);
    expect(get(store).hasMore).toBe(true);

    // Contract is nextLoadLimit arithmetic (+LOAD_MORE_STEP), not literal ×2:
    // 5000 → 15000 on the first "load more".
    expect(await store.loadMore("/cap/a")).toBe(true);
    expect(calls[1]).toEqual({ path: "/cap/a", maxCommits: DEFAULT_MAX_COMMITS + LOAD_MORE_STEP });
    expect(get(store).maxCommits).toBe(DEFAULT_MAX_COMMITS + LOAD_MORE_STEP);

    // Exhaust the ceiling: repeated loadMore lands exactly on MAX_LOAD_COMMITS.
    for (;;) {
      const before = calls.length;
      const advanced = await store.loadMore("/cap/a");
      expect(calls.length).toBe(advanced ? before + 1 : before);
      if (!advanced) break;
      const prev = calls[calls.length - 2]!.maxCommits;
      const curr = calls[calls.length - 1]!.maxCommits;
      expect(curr).toBe(Math.min(prev + LOAD_MORE_STEP, MAX_LOAD_COMMITS));
    }
    expect(get(store).maxCommits).toBe(MAX_LOAD_COMMITS);

    // Per-path independence: another repo starts at the default again.
    store.showRepo("/cap/b");
    await store.loadGraph("/cap/b");
    expect(calls[calls.length - 1]).toEqual({ path: "/cap/b", maxCommits: DEFAULT_MAX_COMMITS });

    // Switching back serves the cached /cap/a graph synchronously: no await,
    // no refetch, raised ceiling intact.
    const cachedRows = get(store); // snapshot of b before switching
    void cachedRows;
    const callsBeforeSwitch = calls.length;
    store.showRepo("/cap/a");
    const s = get(store);
    expect(s.isLoading).toBe(false);
    expect(s.visiblePath).toBe("/cap/a");
    expect(s.maxCommits).toBe(MAX_LOAD_COMMITS);
    expect(s.hasMore).toBe(true);
    expect(s.error).toBeNull();
    expect(s.rows.length).toBeGreaterThan(0);
    expect(calls.length).toBe(callsBeforeSwitch); // cache hit issued NO invoke
  });

  it("rapid alternating selections land the FINAL commit's details regardless of resolution order", async () => {
    const gates: Gate[] = [];
    const invoke: InvokeFn = (cmd, args) => {
      if (cmd === "cmd_get_commit_graph") {
        return Promise.resolve(graphPayload("/sel/z", "base") as never);
      }
      if (cmd === "cmd_get_commit_details") {
        // The page load's auto-details resolve inline; only MANUAL selections
        // get slow, manually-resolved gates.
        if (String(args?.commitId).startsWith("base")) {
          return Promise.resolve(detailsFor(String(args?.commitId)) as never);
        }
        const gate: Gate = { cmd, args: (args ?? {}) as Record<string, unknown>, d: deferred(), settled: false };
        gates.push(gate);
        return gate.d.promise as Promise<never>;
      }
      return Promise.reject(new Error(cmd));
    };
    const store = createGraphStore({ invoke });
    store.showRepo("/sel/z");
    await store.loadGraph("/sel/z");
    expect(get(store).selectedCommitDetails?.id).toBe("base-a");

    const picks = [1, 2, 3, 4, 5, 6, 7, 8].map((n) => row(`c${n}`));
    const selections = picks.map((p) => store.selectCommit(p, "/sel/z"));
    expect(get(store).selectedCommit?.id).toBe("c8"); // final selection sticks synchronously
    expect(gates).toHaveLength(8);

    // Resolve NEWEST first, stalest ABSOLUTE LAST — the classic overwrite trap.
    for (let i = gates.length - 1; i >= 0; i -= 1) {
      const gate = gates[i]!;
      settle(gate, "ok", detailsFor(String(gate.args.commitId)));
      await flush();
      if (i === 0) {
        // c1's slow response arrived after everything else: it must lose.
        expect(get(store).selectedCommitDetails?.id).toBe("c8");
        expect(get(store).selectedCommitDetails?.summary).toBe("details:c8");
      }
    }
    await Promise.all(selections);
    await flush();
    expect(get(store).selectedCommit?.id).toBe("c8");
    expect(get(store).selectedCommitDetails?.id).toBe("c8");
    expect(get(store).selectedCommitDetails?.summary).toBe("details:c8");
  });
});
