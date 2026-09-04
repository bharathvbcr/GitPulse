import { describe, expect, it, vi } from "vitest";
import {
  createMetric,
  createMetricRegistry,
  describeStaleness,
  formatAge,
  type MetricClock,
  type MetricSnapshot,
} from "./freshness";

/**
 * A clock the test drives. Timers fire only when `advance` reaches their
 * deadline, so debounce and throttle behaviour is asserted exactly rather than
 * waited for.
 */
function fakeClock() {
  let now = 1_000;
  let nextId = 1;
  const timers = new Map<number, { at: number; fn: () => void }>();
  const clock: MetricClock = {
    now: () => now,
    setTimeout: (fn, ms) => {
      const id = nextId++;
      timers.set(id, { at: now + ms, fn });
      return id;
    },
    clearTimeout: (handle) => {
      timers.delete(handle as number);
    },
  };
  return {
    clock,
    get now() {
      return now;
    },
    /** Moves time forward, firing every timer that comes due in order. */
    advance(ms: number) {
      const target = now + ms;
      for (;;) {
        let due: [number, { at: number; fn: () => void }] | null = null;
        for (const entry of timers) {
          if (entry[1].at <= target && (due === null || entry[1].at < due[1].at)) due = entry;
        }
        if (!due) break;
        timers.delete(due[0]);
        now = due[1].at;
        due[1].fn();
      }
      now = target;
    },
    get pendingTimers() {
      return timers.size;
    },
  };
}

/** A measure function whose resolution the test controls. */
function deferredMeasure<T>() {
  const calls: string[] = [];
  let resolve!: (value: T) => void;
  let reject!: (err: unknown) => void;
  let pending: Promise<T> | null = null;
  return {
    calls,
    measure: (repoPath: string) => {
      calls.push(repoPath);
      pending = new Promise<T>((res, rej) => {
        resolve = res;
        reject = rej;
      });
      return pending;
    },
    settle: (value: T) => {
      resolve(value);
      return pending;
    },
    fail: (err: unknown) => {
      reject(err);
      return pending?.catch(() => undefined);
    },
  };
}

const REPO = "/repos/app";

function counting(values: number[] = []) {
  let i = 0;
  const calls: string[] = [];
  return {
    calls,
    measure: async (repoPath: string) => {
      calls.push(repoPath);
      return values[i++] ?? i;
    },
  };
}

describe("metric freshness", () => {
  it("measures once for many subscribers and shares the result", async () => {
    const c = fakeClock();
    const source = counting([42]);
    const metric = createMetric(
      { name: "loc", measure: source.measure, debounceMs: 100, minIntervalMs: 500 },
      c.clock,
    );

    const a: MetricSnapshot<number>[] = [];
    const b: MetricSnapshot<number>[] = [];
    metric.subscribe(REPO, (s) => a.push(s));
    metric.subscribe(REPO, (s) => b.push(s));
    await Promise.resolve();
    await Promise.resolve();

    expect(source.calls).toEqual([REPO]);
    expect(metric.snapshot(REPO).value).toBe(42);
    expect(a.at(-1)?.value).toBe(42);
    expect(b.at(-1)?.value).toBe(42);
  });

  it("a concurrent refresh joins the in-flight measurement instead of starting a second", async () => {
    const c = fakeClock();
    const d = deferredMeasure<number>();
    const metric = createMetric(
      { name: "storage", measure: d.measure, debounceMs: 0, minIntervalMs: 0 },
      c.clock,
    );

    const first = metric.refresh(REPO);
    const second = metric.refresh(REPO);
    const third = metric.refresh(REPO);
    expect(d.calls).toEqual([REPO]);

    await d.settle(7);
    await Promise.all([first, second, third]);
    expect(d.calls).toEqual([REPO]);
    expect(metric.snapshot(REPO).value).toBe(7);
  });

  /** THE HONESTY RULE: a failed refresh keeps the value but never calls it current. */
  it("keeps the last value on failure and marks it stale rather than fresh", async () => {
    const c = fakeClock();
    let attempt = 0;
    const metric = createMetric(
      {
        name: "coverage",
        measure: async () => {
          attempt += 1;
          if (attempt === 1) return 90;
          throw new Error("scan exploded");
        },
        debounceMs: 0,
        minIntervalMs: 0,
      },
      c.clock,
    );

    await metric.refresh(REPO);
    expect(metric.snapshot(REPO)).toMatchObject({ state: "ready", value: 90, stale: null });

    await metric.refresh(REPO, { force: true });
    const after = metric.snapshot(REPO);
    expect(after.state).toBe("failed");
    expect(after.value).toBe(90);
    expect(after.stale).toBe("refresh-failed");
    expect(after.error).toContain("scan exploded");
    expect(describeStaleness(after, c.now)).toContain("last refresh failed");
  });

  it("a first-ever failure reports never-measured, not a stale value", async () => {
    const c = fakeClock();
    const metric = createMetric(
      {
        name: "loc",
        measure: async () => {
          throw new Error("nope");
        },
        debounceMs: 0,
        minIntervalMs: 0,
      },
      c.clock,
    );
    await metric.refresh(REPO);
    expect(metric.snapshot(REPO)).toMatchObject({
      state: "failed",
      value: null,
      stale: "never-measured",
    });
  });

  it("marks a snapshot stale the instant the repository changes, before any refresh runs", async () => {
    const c = fakeClock();
    const source = counting([1, 2]);
    const metric = createMetric(
      { name: "loc", measure: source.measure, debounceMs: 300, minIntervalMs: 0 },
      c.clock,
    );
    metric.subscribe(REPO, () => {});
    await Promise.resolve();
    await Promise.resolve();
    expect(metric.snapshot(REPO).stale).toBeNull();

    metric.invalidate(REPO);
    // Synchronously stale: the UI says "out of date" now, not when the
    // replacement measurement lands.
    expect(metric.snapshot(REPO).stale).toBe("repository-changed");
    expect(metric.snapshot(REPO).value).toBe(1);
    expect(source.calls).toHaveLength(1);
  });

  it("coalesces a storm of change events into one refresh", async () => {
    const c = fakeClock();
    const source = counting();
    const metric = createMetric(
      { name: "loc", measure: source.measure, debounceMs: 200, minIntervalMs: 0 },
      c.clock,
    );
    metric.subscribe(REPO, () => {});
    await Promise.resolve();
    await Promise.resolve();
    expect(source.calls).toHaveLength(1);

    for (let i = 0; i < 50; i += 1) {
      metric.invalidate(REPO);
      c.advance(10);
    }
    c.advance(500);
    await Promise.resolve();
    await Promise.resolve();

    // One initial measurement plus the coalesced follow-ups — nowhere near 50.
    expect(source.calls.length).toBeLessThanOrEqual(4);
    expect(source.calls.length).toBeGreaterThanOrEqual(2);
  });

  /** The cost floor: an expensive scan cannot be pinned at 100% duty cycle. */
  it("honours the minimum interval between completed measurements", async () => {
    const c = fakeClock();
    const source = counting();
    const metric = createMetric(
      { name: "storage", measure: source.measure, debounceMs: 0, minIntervalMs: 10_000 },
      c.clock,
    );

    await metric.refresh(REPO);
    expect(source.calls).toHaveLength(1);

    // Well inside the floor: scheduled, not run.
    c.advance(1_000);
    await metric.refresh(REPO);
    await Promise.resolve();
    expect(source.calls).toHaveLength(1);

    // Past the floor, the scheduled refresh fires.
    c.advance(10_000);
    await Promise.resolve();
    await Promise.resolve();
    expect(source.calls).toHaveLength(2);
  });

  it("force bypasses the interval floor, because Rescan must always do something", async () => {
    const c = fakeClock();
    const source = counting();
    const metric = createMetric(
      { name: "storage", measure: source.measure, debounceMs: 0, minIntervalMs: 60_000 },
      c.clock,
    );
    await metric.refresh(REPO);
    await metric.refresh(REPO, { force: true });
    expect(source.calls).toHaveLength(2);
  });

  it("reports a measurement the metric itself calls incomplete as partial", async () => {
    const c = fakeClock();
    const metric = createMetric(
      {
        name: "storage",
        measure: async () => ({ bytes: 10, truncated: true }),
        debounceMs: 0,
        minIntervalMs: 0,
        isPartial: (v) => v.truncated,
      },
      c.clock,
    );
    await metric.refresh(REPO);
    const snap = metric.snapshot(REPO);
    expect(snap.state).toBe("ready");
    expect(snap.stale).toBe("partial");
    expect(describeStaleness(snap, c.now)).toContain("floor, not a total");
  });

  it("discards a result whose measurement was superseded", async () => {
    const c = fakeClock();
    const first = deferredMeasure<number>();
    let call = 0;
    const metric = createMetric<number>(
      {
        name: "loc",
        measure: (repo) => {
          call += 1;
          if (call === 1) return first.measure(repo);
          return Promise.resolve(999);
        },
        debounceMs: 0,
        minIntervalMs: 0,
      },
      c.clock,
    );

    const stale = metric.refresh(REPO);
    await metric.refresh(REPO, { force: true });
    expect(metric.snapshot(REPO).value).toBe(999);

    // The first, slower measurement now finishes. It must not overwrite the
    // newer answer.
    await first.settle(111);
    await stale;
    await Promise.resolve();
    expect(metric.snapshot(REPO).value).toBe(999);
  });

  it("a listener that throws does not stop the other listeners updating", async () => {
    const c = fakeClock();
    const metric = createMetric(
      { name: "loc", measure: async () => 5, debounceMs: 0, minIntervalMs: 0 },
      c.clock,
    );
    const seen: number[] = [];
    metric.subscribe(REPO, () => {
      throw new Error("render blew up");
    });
    metric.subscribe(REPO, (s) => {
      if (s.value !== null) seen.push(s.value);
    });
    await metric.refresh(REPO, { force: true });
    expect(seen).toContain(5);
  });

  it("unsubscribing stops delivery and does not disturb other subscribers", async () => {
    const c = fakeClock();
    const metric = createMetric(
      { name: "loc", measure: async () => 3, debounceMs: 0, minIntervalMs: 0 },
      c.clock,
    );
    const gone: unknown[] = [];
    const kept: unknown[] = [];
    const off = metric.subscribe(REPO, (s) => gone.push(s));
    metric.subscribe(REPO, (s) => kept.push(s));
    await Promise.resolve();
    await Promise.resolve();
    const seenBefore = gone.length;

    off();
    off(); // idempotent
    await metric.refresh(REPO, { force: true });
    expect(gone.length).toBe(seenBefore);
    expect(kept.length).toBeGreaterThan(seenBefore - 1);
  });

  it("invalidating an untracked repository does not create state from watcher noise", () => {
    const c = fakeClock();
    const source = counting();
    const metric = createMetric(
      { name: "loc", measure: source.measure, debounceMs: 0, minIntervalMs: 0 },
      c.clock,
    );
    for (let i = 0; i < 1_000; i += 1) metric.invalidate(`/repos/noise-${i}`);
    expect(metric.trackedRepos).toHaveLength(0);
    expect(source.calls).toHaveLength(0);
  });

  it("bounds tracked repositories but never evicts one a panel is watching", async () => {
    const c = fakeClock();
    const metric = createMetric(
      { name: "loc", measure: async () => 1, debounceMs: 0, minIntervalMs: 0, maxRepos: 3 },
      c.clock,
    );
    const watched = "/repos/watched";
    metric.subscribe(watched, () => {});
    await Promise.resolve();
    await Promise.resolve();

    for (let i = 0; i < 20; i += 1) {
      await metric.refresh(`/repos/other-${i}`, { force: true });
    }
    expect(metric.trackedRepos.length).toBeLessThanOrEqual(4);
    expect(metric.trackedRepos).toContain(watched);
  });

  /**
   * THE SOAK FINDING: opening one more repository than the bound, while every
   * existing cell was watched, evicted the cell `touch` had just created — the
   * only one with no listeners yet. The subscriber then attached to a cell no
   * longer in the map: it never fired again, and `snapshot()` read idle
   * forever. The bound is soft precisely so this cannot happen.
   */
  it("never evicts the cell it just created, even when every other one is watched", async () => {
    const c = fakeClock();
    const metric = createMetric(
      { name: "loc", measure: async () => 1, debounceMs: 0, minIntervalMs: 0, maxRepos: 3 },
      c.clock,
    );
    const watched = ["/repos/a", "/repos/b", "/repos/c"];
    for (const repo of watched) metric.subscribe(repo, () => {});
    await Promise.resolve();
    await Promise.resolve();

    const overflow = "/repos/d";
    const seen: (number | null)[] = [];
    metric.subscribe(overflow, (snap) => seen.push(snap.value));
    await Promise.resolve();
    await Promise.resolve();

    expect(metric.trackedRepos, "the new cell must survive").toContain(overflow);
    for (const repo of watched) {
      expect(metric.trackedRepos, "a watched cell must survive").toContain(repo);
    }
    // And the subscription is live, not attached to an orphan.
    expect(metric.snapshot(overflow).value).toBe(1);
    expect(seen).toContain(1);

    await metric.refresh(overflow, { force: true });
    expect(seen.length).toBeGreaterThan(1);
  });

  it("forget drops a repository and discards its in-flight measurement", async () => {
    const c = fakeClock();
    const d = deferredMeasure<number>();
    const metric = createMetric(
      { name: "loc", measure: d.measure, debounceMs: 0, minIntervalMs: 0 },
      c.clock,
    );
    const run = metric.refresh(REPO);
    metric.forget(REPO);
    await d.settle(5);
    await run;
    expect(metric.trackedRepos).not.toContain(REPO);
    expect(metric.snapshot(REPO).value).toBeNull();
  });

  it("dispose cancels pending timers and ignores late results", async () => {
    const c = fakeClock();
    const d = deferredMeasure<number>();
    const metric = createMetric(
      { name: "loc", measure: d.measure, debounceMs: 100, minIntervalMs: 0 },
      c.clock,
    );
    const run = metric.refresh(REPO);
    metric.dispose();
    await d.settle(5);
    await run;
    expect(metric.snapshot(REPO).value).toBeNull();
    expect(c.pendingTimers).toBe(0);

    // Post-dispose calls are inert rather than throwing.
    metric.invalidate(REPO);
    await metric.refresh(REPO, { force: true });
    expect(metric.snapshot(REPO).value).toBeNull();
  });

  it("a rejection with no message still produces a readable error", async () => {
    const c = fakeClock();
    const metric = createMetric(
      {
        name: "coverage",
        measure: async () => {
          throw new Error("");
        },
        debounceMs: 0,
        minIntervalMs: 0,
      },
      c.clock,
    );
    await metric.refresh(REPO);
    expect(metric.snapshot(REPO).error).toBe("coverage measurement failed with no message");
  });

  it("keeps repositories independent: churn in one does not starve another", async () => {
    const c = fakeClock();
    const source = counting();
    const metric = createMetric(
      { name: "loc", measure: source.measure, debounceMs: 50, minIntervalMs: 0 },
      c.clock,
    );
    const busy = "/repos/busy";
    const quiet = "/repos/quiet";
    metric.subscribe(busy, () => {});
    metric.subscribe(quiet, () => {});
    await Promise.resolve();
    await Promise.resolve();
    source.calls.length = 0;

    for (let i = 0; i < 20; i += 1) {
      metric.invalidate(busy);
      c.advance(10);
    }
    metric.invalidate(quiet);
    c.advance(200);
    await Promise.resolve();
    await Promise.resolve();
    expect(source.calls).toContain(quiet);
  });
});

describe("metric registry", () => {
  it("routes one repo-changed event to every registered metric", async () => {
    const c = fakeClock();
    const loc = counting();
    const storage = counting();
    const locMetric = createMetric(
      { name: "loc", measure: loc.measure, debounceMs: 0, minIntervalMs: 0 },
      c.clock,
    );
    const storageMetric = createMetric(
      { name: "storage", measure: storage.measure, debounceMs: 0, minIntervalMs: 0 },
      c.clock,
    );
    const registry = createMetricRegistry();
    registry.register(locMetric as never);
    registry.register(storageMetric as never);
    registry.register(locMetric as never); // idempotent
    expect(registry.metrics).toHaveLength(2);

    locMetric.subscribe(REPO, () => {});
    storageMetric.subscribe(REPO, () => {});
    await Promise.resolve();
    await Promise.resolve();
    loc.calls.length = 0;
    storage.calls.length = 0;

    registry.invalidate(REPO);
    c.advance(10);
    await Promise.resolve();
    await Promise.resolve();
    expect(loc.calls).toEqual([REPO]);
    expect(storage.calls).toEqual([REPO]);
  });

  it("forget clears a closed repository from every metric", async () => {
    const c = fakeClock();
    const metric = createMetric(
      { name: "loc", measure: async () => 1, debounceMs: 0, minIntervalMs: 0 },
      c.clock,
    );
    const registry = createMetricRegistry();
    registry.register(metric as never);
    await metric.refresh(REPO, { force: true });
    expect(metric.trackedRepos).toContain(REPO);
    registry.forget(REPO);
    expect(metric.trackedRepos).not.toContain(REPO);
  });
});

describe("staleness rendering", () => {
  it("says nothing at all when the value is genuinely current", () => {
    expect(
      describeStaleness(
        { state: "ready", value: 1, measuredAt: 10, stale: null, error: null },
        20,
      ),
    ).toBeNull();
  });

  it("covers every stale reason", () => {
    const reasons = ["never-measured", "repository-changed", "refresh-failed", "partial"] as const;
    for (const stale of reasons) {
      const text = describeStaleness(
        { state: "ready", value: 1, measuredAt: 0, stale, error: null },
        60_000,
      );
      expect(text, stale).toBeTruthy();
    }
  });

  it("formats ages across every unit boundary", () => {
    expect(formatAge(0)).toBe("0s");
    expect(formatAge(-500)).toBe("0s");
    expect(formatAge(12_000)).toBe("12s");
    expect(formatAge(4 * 60_000)).toBe("4m");
    expect(formatAge(2 * 3_600_000)).toBe("2h");
    expect(formatAge(3 * 86_400_000)).toBe("3d");
  });
});

describe("real timers", () => {
  it("works against the default clock, not only the injected one", async () => {
    vi.useFakeTimers();
    try {
      const source = counting();
      const metric = createMetric({
        name: "loc",
        measure: source.measure,
        debounceMs: 20,
        minIntervalMs: 0,
      });
      metric.subscribe(REPO, () => {});
      await vi.advanceTimersByTimeAsync(0);
      expect(source.calls).toHaveLength(1);

      metric.invalidate(REPO);
      await vi.advanceTimersByTimeAsync(50);
      expect(source.calls).toHaveLength(2);
      metric.dispose();
    } finally {
      vi.useRealTimers();
    }
  });
});

/**
 * Randomized soak: hammer one metric with an arbitrary interleaving of every
 * operation and assert the invariants that must hold no matter the order.
 *
 * The state machine carries generation counters, an in-flight promise, a
 * debounce timer, a throttle floor and an LRU — five pieces of mutable state
 * that a hand-written test exercises one path at a time. This exercises them
 * against sequences nobody would think to write, which is the point.
 *
 * Seeded and deterministic: a failure here reproduces exactly rather than
 * being a story about a flake.
 */
describe("freshness soak", () => {
  function seededRandom(seed: number) {
    let state = seed >>> 0;
    return () => {
      // xorshift32 — small, deterministic, good enough to shuffle operations.
      state ^= state << 13;
      state ^= state >>> 17;
      state ^= state << 5;
      state >>>= 0;
      return state / 0x1_0000_0000;
    };
  }

  it("holds its invariants under 2000 random operations across many repos", async () => {
    const c = fakeClock();
    const rand = seededRandom(0x5eed);
    const repos = Array.from({ length: 12 }, (_, i) => `/repos/r${i}`);
    let calls = 0;
    let failNext = false;

    const metric = createMetric<number>(
      {
        name: "soak",
        measure: async () => {
          calls += 1;
          if (failNext) throw new Error("induced failure");
          return calls;
        },
        debounceMs: 50,
        minIntervalMs: 200,
        maxRepos: 4,
        isPartial: (v) => v % 7 === 0,
      },
      c.clock,
    );

    const unsubscribes = new Map<string, () => void>();
    const seen = new Map<string, number>();

    for (let step = 0; step < 2_000; step += 1) {
      const repo = repos[Math.floor(rand() * repos.length)];
      const op = Math.floor(rand() * 7);
      failNext = rand() < 0.15;
      switch (op) {
        case 0:
          if (!unsubscribes.has(repo)) {
            unsubscribes.set(
              repo,
              metric.subscribe(repo, (snap) => {
                // A subscriber must never be handed an incoherent snapshot.
                if (snap.value === null) {
                  expect(snap.stale, "a valueless snapshot is never fresh").not.toBeNull();
                } else {
                  expect(snap.measuredAt, "a value always has a measurement time").not.toBeNull();
                }
                if (snap.state === "failed") {
                  expect(snap.error, "a failure always carries a message").toBeTruthy();
                }
                seen.set(repo, (seen.get(repo) ?? 0) + 1);
              }),
            );
          }
          break;
        case 1:
          unsubscribes.get(repo)?.();
          unsubscribes.delete(repo);
          break;
        case 2:
          metric.invalidate(repo);
          break;
        case 3:
          await metric.refresh(repo);
          break;
        case 4:
          await metric.refresh(repo, { force: true });
          break;
        case 5:
          metric.forget(repo);
          unsubscribes.get(repo)?.();
          unsubscribes.delete(repo);
          break;
        default:
          c.advance(Math.floor(rand() * 400));
          break;
      }

      // The LRU bound holds continuously, but never at the cost of dropping a
      // repository a subscriber is still watching.
      const tracked = metric.trackedRepos;
      expect(new Set(tracked).size, "no duplicate cells").toBe(tracked.length);
      for (const watched of unsubscribes.keys()) {
        expect(tracked, `evicted a watched repo at step ${step}`).toContain(watched);
      }
    }

    // Drain and assert the machine is still coherent rather than wedged.
    failNext = false;
    c.advance(10_000);
    await Promise.resolve();
    await Promise.resolve();
    for (const repo of repos) {
      const snap = metric.snapshot(repo);
      expect(["idle", "loading", "ready", "failed"]).toContain(snap.state);
      if (snap.value !== null) expect(snap.measuredAt).not.toBeNull();
    }

    metric.dispose();
    expect(c.pendingTimers, "dispose leaves no timer behind").toBe(0);
    expect(metric.trackedRepos).toHaveLength(0);
  });

  it("never leaks a timer across an arbitrary subscribe/unsubscribe storm", () => {
    const c = fakeClock();
    const metric = createMetric(
      { name: "soak", measure: async () => 1, debounceMs: 100, minIntervalMs: 1_000 },
      c.clock,
    );
    for (let i = 0; i < 500; i += 1) {
      const off = metric.subscribe(`/repos/r${i % 5}`, () => {});
      metric.invalidate(`/repos/r${i % 5}`);
      off();
    }
    metric.dispose();
    expect(c.pendingTimers).toBe(0);
  });
});
