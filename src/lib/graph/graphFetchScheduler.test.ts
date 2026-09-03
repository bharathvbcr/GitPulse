import { describe, expect, it } from "vitest";
import {
  GRAPH_FETCH_DEBOUNCE_MS,
  createGraphFetchScheduler,
  graphRequestKey,
  normalizeGraphQuery,
  type GraphFetchRequest,
  type ScheduledLoad,
} from "./graphFetchScheduler";

/**
 * Deterministic fake clock: timeouts live in an ordered queue; `advance` runs
 * every callback whose deadline has passed, in schedule order.
 */
function createClock() {
  type Job = { at: number; fn: () => void; seq: number };
  let jobs: Job[] = [];
  let seq = 0;
  let now = 0;
  const setTimeoutFn = (fn: () => void, ms: number) => {
    const job: Job = { at: now + ms, fn, seq: seq++ };
    jobs.push(job);
    return job;
  };
  const clearTimeoutFn = (job: unknown) => {
    jobs = jobs.filter((j) => j !== job);
  };
  /** Runs all due jobs (a job scheduled by another job fires in the same tick). */
  function advance(ms: number): void {
    const target = now + ms;
    for (;;) {
      const due = jobs.filter((j) => j.at <= target).sort((a, b) => a.at - b.at || a.seq - b.seq)[0];
      if (!due) break;
      jobs = jobs.filter((j) => j !== due);
      now = Math.max(now, due.at);
      due.fn();
    }
    now = target;
  }
  return { setTimeoutFn, clearTimeoutFn, advance, get pending() { return jobs.length; } };
}

function req(path: string | null, query = "", revision: string | null = null): GraphFetchRequest {
  return { path, query, revision };
}

function harness() {
  const clock = createClock();
  const loads: ScheduledLoad[] = [];
  const scheduler = createGraphFetchScheduler({
    load: (r) => loads.push({ ...r }),
    setTimeoutFn: clock.setTimeoutFn,
    clearTimeoutFn: clock.clearTimeoutFn,
  });
  return { clock, loads, scheduler };
}

describe("graphRequestKey", () => {
  it("keys on path and revision", () => {
    expect(graphRequestKey(req("/a", "", "main"))).toBe("/a\u241fmain");
    expect(graphRequestKey(req("/a"))).toBe("/a\u241f");
    expect(graphRequestKey(req("/a", "", "dev"))).not.toBe(graphRequestKey(req("/a")));
  });

  it("includes every non-blank query, normalized: each filter edit is its own request", () => {
    // Every term runs in the backend (author, sha, type, free text, path),
    // so a different query is a different graph and must refetch.
    expect(graphRequestKey(req("/a", "path:src/**/*.ts"))).toBe("/a\u241f\u241fpath:src/**/*.ts");
    expect(graphRequestKey(req("/a", "author:ada"))).toBe("/a\u241f\u241fauthor:ada");
    expect(graphRequestKey(req("/a", "dead"))).not.toBe(graphRequestKey(req("/a", "beef")));
    // Whitespace is not a different request: a stray space never re-walks.
    expect(graphRequestKey(req("/a", "  author:ada   fix:  "))).toBe(
      graphRequestKey(req("/a", "author:ada fix:")),
    );
    expect(graphRequestKey(req("/a", "   "))).toBe(graphRequestKey(req("/a", "")));
  });
});

describe("normalizeGraphQuery", () => {
  it("collapses whitespace and keeps token order", () => {
    expect(normalizeGraphQuery("  a   b\tc \n")).toBe("a b c");
    expect(normalizeGraphQuery("author:ada")).toBe("author:ada");
    expect(normalizeGraphQuery("")).toBe("");
    expect(normalizeGraphQuery("   ")).toBe("");
  });
});

describe("createGraphFetchScheduler", () => {
  it("fires once after the debounce window with the presented arguments", () => {
    const { clock, loads, scheduler } = harness();
    scheduler.sync(req("/repo", "", "main"));
    expect(scheduler.armed).toBe(true);
    clock.advance(GRAPH_FETCH_DEBOUNCE_MS - 1);
    expect(loads).toEqual([]);
    clock.advance(1);
    expect(loads).toEqual([{ path: "/repo", query: "", revision: "main" }]);
    expect(scheduler.armed).toBe(false);
  });

  /**
   * THE REGRESSION: on a freshly opened repo, hydrate completion, branch-stats
   * batches and status-poll ticks all land inside the first 200 ms window.
   * The pre-fix inline effect cleared its timer on every emission and then
   * skipped rescheduling (`key === lastGraphKey`), so the only fetch the pane
   * needed never ran and the loader spun indefinitely. Identical re-presented
   * requests must leave the armed timer untouched instead.
   */
  it("survives unrelated same-request churn inside the window and keeps the original deadline", () => {
    const { clock, loads, scheduler } = harness();
    scheduler.sync(req("/repo"));
    for (let tick = 20; tick <= 160; tick += 20) {
      clock.advance(20);
      scheduler.sync(req("/repo")); // poll tick / stats flush re-runs the effect
      expect(scheduler.armed).toBe(true);
    }
    clock.advance(GRAPH_FETCH_DEBOUNCE_MS - 160);
    expect(loads).toEqual([{ path: "/repo", query: "", revision: null }]);
  });

  it("restarts the window only when the request genuinely changes (trailing debounce)", () => {
    const { clock, loads, scheduler } = harness();
    scheduler.sync(req("/repo", "path:src"));
    clock.advance(150);
    scheduler.sync(req("/repo", "path:src/lib")); // keystroke → new key → re-arm
    clock.advance(150); // 300 total, but window restarted at 150
    expect(loads).toEqual([]);
    clock.advance(GRAPH_FETCH_DEBOUNCE_MS - 150);
    expect(loads).toEqual([{ path: "/repo", query: "path:src/lib", revision: null }]);
  });

  it("does not refetch an already-served request on later emissions", () => {
    const { clock, loads, scheduler } = harness();
    scheduler.sync(req("/repo"));
    clock.advance(GRAPH_FETCH_DEBOUNCE_MS);
    expect(loads).toHaveLength(1);
    for (let i = 0; i < 10; i += 1) {
      scheduler.sync(req("/repo")); // poll ticks keep re-running the effect
      clock.advance(GRAPH_FETCH_DEBOUNCE_MS * 2);
    }
    expect(loads).toHaveLength(1);
  });

  it("refetches after reset() (repo closed/reopened or remount)", () => {
    const { clock, loads, scheduler } = harness();
    scheduler.sync(req("/repo"));
    clock.advance(GRAPH_FETCH_DEBOUNCE_MS);
    expect(loads).toHaveLength(1);
    scheduler.reset();
    scheduler.sync(req("/repo"));
    clock.advance(GRAPH_FETCH_DEBOUNCE_MS);
    expect(loads).toHaveLength(2);
  });

  it("cancels the pending fetch when the path goes null, and allows it again later", () => {
    const { clock, loads, scheduler } = harness();
    scheduler.sync(req("/repo"));
    clock.advance(100);
    scheduler.sync(req(null));
    expect(scheduler.armed).toBe(false);
    clock.advance(GRAPH_FETCH_DEBOUNCE_MS * 5);
    expect(loads).toEqual([]);
    scheduler.sync(req("/repo"));
    clock.advance(GRAPH_FETCH_DEBOUNCE_MS);
    expect(loads).toEqual([{ path: "/repo", query: "", revision: null }]);
  });

  it("fetches again when a superseding key resolves back to an older state", () => {
    const { clock, loads, scheduler } = harness();
    scheduler.sync(req("/repo", "", "main"));
    clock.advance(GRAPH_FETCH_DEBOUNCE_MS);
    scheduler.sync(req("/repo", "", "dev"));
    clock.advance(GRAPH_FETCH_DEBOUNCE_MS);
    scheduler.sync(req("/repo", "", "main")); // switched back
    clock.advance(GRAPH_FETCH_DEBOUNCE_MS);
    expect(loads.map((l) => l.revision)).toEqual(["main", "dev", "main"]);
  });

  it("lets a query edit inside the window supersede the armed request, fired normalized", () => {
    // Every filter term is a backend request: the newer query wins the
    // window and only it fires (trailing debounce), in canonical form.
    const { clock, loads, scheduler } = harness();
    scheduler.sync(req("/repo", ""));
    scheduler.sync(req("/repo", "  author:me  "));
    clock.advance(GRAPH_FETCH_DEBOUNCE_MS);
    expect(loads).toEqual([{ path: "/repo", query: "author:me", revision: null }]);
  });

  it("does not refetch when the query only changed in whitespace", () => {
    const { clock, loads, scheduler } = harness();
    scheduler.sync(req("/repo", "author:me"));
    clock.advance(GRAPH_FETCH_DEBOUNCE_MS);
    scheduler.sync(req("/repo", " author:me "));
    clock.advance(GRAPH_FETCH_DEBOUNCE_MS);
    expect(loads).toEqual([{ path: "/repo", query: "author:me", revision: null }]);
  });
});

/**
 * Documents the pre-fix failure mode end-to-end: this replicates the exact
 * algorithm that used to live inline in App.svelte (memo written before the
 * debounced work runs, teardown clearing the timer on every effect re-run),
 * driven by the same stimulus as the regression test above. The fetch is
 * silently lost. Kept as executable documentation for WHY the scheduler
 * exists — delete together with this file's history if the scheduler is ever
 * replaced.
 */
describe("legacy inline pattern (pre-fix)", () => {
  it("drops the fetch when a store emission lands inside the window", () => {
    const clock = createClock();
    const loads: ScheduledLoad[] = [];
    let lastGraphKey: string | null = null;
    let handle: unknown = null;
    // One Svelte effect re-run = teardown (cleanup) followed by body.
    const effectRun = (r: GraphFetchRequest) => {
      if (handle !== null) {
        clock.clearTimeoutFn(handle);
        handle = null;
      }
      if (!r.path) {
        lastGraphKey = null;
        return;
      }
      const key = graphRequestKey(r);
      if (key === lastGraphKey) return;
      lastGraphKey = key;
      handle = clock.setTimeoutFn(() => {
        void loads.push({ path: r.path!, query: r.query, revision: r.revision });
      }, GRAPH_FETCH_DEBOUNCE_MS);
    };

    effectRun(req("/fresh-repo")); // open publishes currentPath → arms fetch
    clock.advance(50); // hydrate lands → publish
    effectRun(req("/fresh-repo"));
    clock.advance(60); // branch-stats batch flushes → publish
    effectRun(req("/fresh-repo"));
    clock.advance(GRAPH_FETCH_DEBOUNCE_MS * 5);

    expect(loads).toEqual([]); // ← the loader-spins-forever bug
  });
});
