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
 * Seeded PRNG (mulberry32) so every failure reproduces exactly from the seed
 * reported in the error message.
 */
function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), a | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** Deterministic fake clock (local reimplementation of the scenario harness's). */
function createFakeClock() {
  type Job = { at: number; fn: () => void; seq: number };
  let jobs: Job[] = [];
  let seq = 0;
  let now = 0;
  return {
    setTimeoutFn(fn: () => void, ms: number): unknown {
      const job: Job = { at: now + ms, fn, seq: seq++ };
      jobs.push(job);
      return job;
    },
    clearTimeoutFn(handle: unknown): void {
      jobs = jobs.filter((j) => j !== handle);
    },
    /** Runs every job whose deadline passed, in schedule order. */
    advance(ms: number): void {
      const target = now + ms;
      for (;;) {
        const due = jobs
          .filter((j) => j.at <= target)
          .sort((a, b) => a.at - b.at || a.seq - b.seq)[0];
        if (!due) break;
        jobs = jobs.filter((j) => j !== due);
        now = Math.max(now, due.at);
        due.fn();
      }
      now = target;
    },
    get now() {
      return now;
    },
    get pending() {
      return jobs.length;
    },
  };
}

const ITERATIONS = 200;
const OPS_PER_ITERATION = 60;

interface LoadRecord {
  req: ScheduledLoad;
  key: string;
  /** Fake-clock timestamp at which the debounce elapsed and load ran. */
  at: number;
}

/**
 * Drives one scheduler through a random op stream (re-presentations, path /
 * revision / ref-scope / server-query / client-query edits, null-path resets,
 * explicit resets, random clock advances) and enforces the four scheduling contract
 * invariants after every step:
 *
 * 1. NO-LOSS — once the latest key's deadline has passed with no reset or
 *    null-path in between, exactly one matching load has settled.
 * 2. DEDUP — the continuously-latest key never loads twice without an
 *    intervening reset or a different key being presented.
 * 3. RESET KILLS — reset()/null-path cancels every pending timer; none can
 *    fire afterwards.
 * 4. ARG INTEGRITY — every load's exact {path, query, revision, refScope} was
 *    presented since the last reset (nothing fabricated or mixed).
 */
function fuzzIteration(seed: number): void {
  const rng = mulberry32(seed);
  const int = (n: number) => Math.floor(rng() * n);
  const pick = <T>(xs: readonly T[]): T => xs[int(xs.length)]!;

  const clock = createFakeClock();
  const loads: LoadRecord[] = [];

  const PATHS = ["/repos/alpha", "/repos/beta", "/repos/gamma"] as const;
  const REVISIONS = [null, "main", "dev", "v2.1"] as const;
  // Every query term is a backend request and part of the key; "" is the
  // unfiltered graph. All entries are already canonical (normalized) so ARG
  // INTEGRITY can compare loaded and presented queries verbatim.
  const QUERIES = [
    "",
    "path:src",
    "path:lib/**/*.rs",
    "path:x author:ada",
    "dead beef",
    "author:grace",
    "type:fix",
    "sha:abc123",
  ] as const;
  // The ref scope is a fourth dimension of request identity: the same
  // repository at the same query answers with a different set of rows under
  // each scope, so a scope edit must re-arm exactly like a filter edit.
  const SCOPES = ["named", "all"] as const;

  let presentedExact: GraphFetchRequest[] = [];
  let latestKey: string | null = null;
  /**
   * The latest presented request as a TUPLE, independent of the key function.
   *
   * NO-LOSS used to compare the settled load's key against the latest key —
   * both produced by `graphRequestKey`, so a key that fails to distinguish two
   * genuinely different requests satisfies the check trivially. That is
   * exactly how an under-discriminating key (one that ignored the ref scope)
   * passed 200 fuzz iterations while the corresponding setting did nothing.
   * Comparing the request itself makes the harness independent of the thing
   * it is validating.
   */
  let latestTuple: ScheduledLoad | null = null;
  let latestArmTime: number | null = null;
  let mirror: { armed: boolean; key: string | null } = { armed: false, key: null };
  let prevLoad: LoadRecord | null = null;
  let presentationsSinceFire: string[] = [];
  let sawResetSinceFire = false;
  let poison = false; // a reset happened; any subsequent fire is a zombie timer
  const journal: string[] = [];
  let opIndex = -1;

  const fail = (msg: string): never => {
    throw new Error(
      `[seed=${seed} op=${opIndex}] ${msg}\nrecent ops: ${journal.slice(-8).join(" | ")}`
    );
  };

  function onLoad(rec: LoadRecord): void {
    if (poison) fail("RESET KILLS violated: a timer fired after reset()/null-path sync");
    loads.push(rec);

    const wasPresented = presentedExact.some(
      (p) =>
        p.path === rec.req.path &&
        p.query === rec.req.query &&
        p.revision === rec.req.revision &&
        (p.refScope ?? "named") === rec.req.refScope
    );
    if (!wasPresented) {
      fail(`ARG INTEGRITY violated: loaded ${JSON.stringify(rec.req)} was never presented`);
    }

    if (
      prevLoad &&
      prevLoad.key === rec.key &&
      !sawResetSinceFire &&
      presentationsSinceFire.every((k) => k === rec.key)
    ) {
      fail(`DEDUP violated: key ${JSON.stringify(rec.key)} loaded twice while continuously latest`);
    }

    prevLoad = rec;
    sawResetSinceFire = false;
    presentationsSinceFire = [];
    poison = false;
  }

  const scheduler = createGraphFetchScheduler({
    load: (req) => onLoad({ req, key: graphRequestKey(req), at: clock.now }),
    setTimeoutFn: clock.setTimeoutFn,
    clearTimeoutFn: clock.clearTimeoutFn,
  });

  function applyReset(label: string): void {
    if (scheduler.armed) fail(`${label}: scheduler still armed after reset`);
    if (clock.pending !== 0) fail(`${label}: ${clock.pending} pending timer(s) survived the reset`);
    presentedExact = [];
    latestKey = null;
    latestTuple = null;
    latestArmTime = null;
    mirror = { armed: false, key: null };
    sawResetSinceFire = true;
    poison = true;
    journal.push(`${opIndex}:${label}`);
  }

  function doSync(req: GraphFetchRequest): void {
    journal.push(
      `${opIndex}:sync(${req.path ?? "null"},${JSON.stringify(req.query)},${String(req.revision)},${req.refScope ?? "named"})`
    );
    if (!req.path) {
      scheduler.sync(req); // delegates to reset() internally
      applyReset("sync(null-path)");
      return;
    }
    const key = graphRequestKey(req);
    const wasArmed = mirror.armed;
    const prevMirrorKey = mirror.key;
    scheduler.sync(req);
    const armed = scheduler.armed;
    // Mirror the scheduler's decision from the observable `armed` bit: only a
    // genuine (re)arm moves the deadline anchor; anti-teardown and memo no-ops
    // must leave it untouched.
    if (armed && (!wasArmed || prevMirrorKey !== key)) latestArmTime = clock.now;
    mirror = { armed, key: armed ? key : null };
    presentedExact.push({
      path: req.path,
      query: req.query,
      revision: req.revision,
      refScope: req.refScope,
    });
    latestKey = key;
    latestTuple = {
      path: req.path,
      query: normalizeGraphQuery(req.query),
      revision: req.revision,
      refScope: req.refScope ?? "named",
    };
    presentationsSinceFire.push(key);
    poison = false;
  }

  function checkNoLoss(): void {
    if (latestKey === null || latestArmTime === null) return;
    if (clock.now - latestArmTime < GRAPH_FETCH_DEBOUNCE_MS) return;
    const sinceArm = loads.filter((l) => l.at > latestArmTime!);
    if (sinceArm.length !== 1) {
      fail(
        `NO-LOSS violated: deadline passed for latest key but ${sinceArm.length} loads settled since its arm (expected exactly 1)`
      );
    }
    if (sinceArm[0]!.key !== latestKey) {
      fail("NO-LOSS violated: the settled load does not match the latest presented key");
    }
    // Key-independent: catches an under-discriminating key, which the check
    // above cannot see because both sides of it come from that same key.
    const settled = sinceArm[0]!.req;
    const want = latestTuple!;
    if (
      settled.path !== want.path ||
      settled.query !== want.query ||
      settled.revision !== want.revision ||
      settled.refScope !== want.refScope
    ) {
      fail(
        `NO-LOSS violated: deadline passed for ${JSON.stringify(want)} but the settled load ` +
          `was ${JSON.stringify(settled)} — the request key does not distinguish them`
      );
    }
    if (scheduler.armed) {
      fail("NO-LOSS violated: deadline passed but the scheduler is still armed");
    }
  }

  let latest: GraphFetchRequest = {
    path: PATHS[0],
    query: "",
    revision: null,
    refScope: SCOPES[0],
  };

  for (let step = 0; step < OPS_PER_ITERATION; step += 1) {
    opIndex = step;
    const roll = rng();

    if (roll < 0.33) {
      const ms = int(2 * GRAPH_FETCH_DEBOUNCE_MS + 1); // 0 .. 2×debounce
      journal.push(`${step}:advance(${ms})`);
      clock.advance(ms);
      checkNoLoss();
    } else if (roll < 0.4) {
      scheduler.reset();
      applyReset("reset()");
    } else if (roll < 0.47) {
      doSync({ path: null, query: "", revision: null }); // pane closed
    } else if (roll < 0.58) {
      doSync({ ...latest }); // unrelated emission re-presenting the same request
    } else if (roll < 0.68) {
      latest = { ...latest, path: pick(PATHS) }; // repo switch
      doSync(latest);
    } else if (roll < 0.78) {
      latest = { ...latest, revision: pick(REVISIONS) }; // branch switch
      doSync(latest);
    } else if (roll < 0.92) {
      latest = { ...latest, query: pick(QUERIES) }; // filter edit
      doSync(latest);
    } else {
      latest = { ...latest, refScope: pick(SCOPES) }; // "Refs drawn" toggled
      doSync(latest);
    }
  }
}

describe("createGraphFetchScheduler fuzz", () => {
  it(
    `survives ${ITERATIONS} seeded random request streams (${OPS_PER_ITERATION} ops each) ` +
      "honoring no-loss, dedup, reset-kills and argument-integrity",
    () => {
      const startedAt = performance.now();
      for (let i = 0; i < ITERATIONS; i += 1) {
        fuzzIteration(1 + i * 7919); // distinct seed per iteration
      }
      // Tripwire against pathological blowups, not an SLA: this suite shares
      // the machine with Rust builds.
      expect(performance.now() - startedAt).toBeLessThan(5000);
    }
  );
});
