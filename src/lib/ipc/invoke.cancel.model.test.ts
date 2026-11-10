import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { MAX_DEFERRED_RETRIES } from "../async/deferral";
import { configureReadCancellation, configureSharedReads, ipcErrorKind, withDeferralRetry } from "./invoke";
import { cancelRead, CANCELLABLE_READ_COMMANDS, SHARED_READ_COMMANDS } from "./sharedReads";

/**
 * A model of the cancellable shared read, driven by random interleavings.
 *
 * Hand-picked sequences closed the paths someone thought of and missed the next.
 * This generates the interleavings instead: callers with and without an abort
 * signal asking for one of a few reads, callers aborting, a command that is not a
 * shared read ending the join generation, the backend answering a request
 * (deferred, data, or the cancellation it gives a request it was told to stop),
 * and a backoff timer firing, with the callers' own "has the caller left" check
 * running as a timer between them.
 *
 * Properties, checked after every step and again once everything has drained:
 *  (i)   a caller that did not abort never resolves or rejects with a cancellation;
 *  (ii)  a caller that did not abort ends with data (the deferrals the model hands
 *        out are fewer than the retries, so a deferral is never a final answer);
 *  (iii) no request is sent under an id a cancel was already sent for;
 *  (iv)  a cancel is sent only when no live caller is attached to that request:
 *        no caller that did not abort is ever answered from a request that was
 *        told to stop.
 *
 * The seeds are fixed; a failure names its seed and prints the steps taken.
 */

const CMD = "cmd_get_commit_graph";
const CANCELLATION = { kind: "read_cancelled", message: "git read cancelled: its caller is gone" };
const DEFERRED = { kind: "deferred_under_load", message: "git log deferred under load after 1s" };
const SEEDS = Array.from({ length: 400 }, (_, i) => i);
const STEPS = 40;

/** mulberry32: a small seeded generator, so a run is a function of its seed. */
function rng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

type Caller = {
  n: number;
  key: number;
  controller: AbortController | null;
  aborted: boolean;
  outcome: { state: "pending" } | { state: "data"; value: unknown } | { state: "error"; error: unknown };
};
type Request = { tag: string; id: string | null; answer: (value: unknown) => void; fail: (error: unknown) => void };

const isCancellation = (error: unknown) => ipcErrorKind(error) === "read_cancelled";

async function run(seed: number): Promise<void> {
  const next = rng(seed);
  const pick = <T>(items: readonly T[]): T => items[Math.floor(next() * items.length)];
  const trace: string[] = [];
  const fail = (what: string): never => {
    throw new Error(`seed ${seed}: ${what}\n  steps:\n    ${trace.join("\n    ")}`);
  };

  const pending: Request[] = [];
  const naps: Array<() => void> = [];
  const cancelled = new Set<string>();
  let tags = 0;
  let deferrals = MAX_DEFERRED_RETRIES;

  const raw = (cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "cmd_cancel_read") {
      cancelled.add(String(args?.requestId));
      return Promise.resolve(true);
    }
    if (cmd !== CMD) return Promise.resolve(null);
    const id = typeof args?.requestId === "string" ? args.requestId : null;
    if (id !== null && cancelled.has(id)) fail(`(iii) a request was sent under the cancelled id ${id}`);
    return new Promise<unknown>((answer, failWith) => {
      pending.push({ tag: `req${(tags += 1)}`, id, answer, fail: failWith });
    });
  };
  const invoke = withDeferralRetry(raw as never, {
    random: () => 0,
    whenShown: () => Promise.resolve(),
    sleep: () => new Promise<void>((wake) => naps.push(wake)),
  });

  const callers: Caller[] = [];
  const micro = async () => {
    for (let i = 0; i < 6; i += 1) await Promise.resolve();
  };
  const tick = () => vi.advanceTimersByTimeAsync(0);

  const check = () => {
    for (const caller of callers) {
      if (caller.aborted) continue;
      const o = caller.outcome;
      if (o.state === "error" && isCancellation(o.error)) fail(`(i) caller ${caller.n} (never aborted) was handed the cancellation`);
      if (o.state === "data" && cancelled.has(String(o.value))) fail(`(iv) caller ${caller.n} (never aborted) was answered by request ${String(o.value)} that was told to stop`);
    }
  };

  const answerOne = (request: Request, how: "deferred" | "data" | "cancellation") => {
    pending.splice(pending.indexOf(request), 1);
    trace.push(`answer ${request.tag}${request.id ? `(${request.id.slice(-3)})` : ""} ${how}`);
    if (how === "deferred") request.fail(DEFERRED);
    else if (how === "cancellation") request.fail(CANCELLATION);
    else request.answer(request.tag);
  };
  const mayCancelAnswer = (request: Request) => request.id !== null && cancelled.has(request.id);

  for (let step = 0; step < STEPS; step += 1) {
    const roll = next();
    if (roll < 0.28) {
      const caller: Caller = {
        n: callers.length,
        key: Math.floor(next() * 3),
        controller: next() < 0.65 ? new AbortController() : null,
        aborted: false,
        outcome: { state: "pending" },
      };
      callers.push(caller);
      trace.push(`load #${caller.n} key${caller.key} ${caller.controller ? "with" : "without"} signal`);
      const given = { repoPath: "/r", query: `k${caller.key}` };
      const promise = caller.controller ? invoke(CMD, given, { signal: caller.controller.signal }) : invoke(CMD, given);
      promise.then(
        (value) => (caller.outcome = { state: "data", value }),
        (error) => (caller.outcome = { state: "error", error }),
      );
    } else if (roll < 0.46) {
      const open = callers.filter((c) => c.controller && !c.aborted);
      if (open.length === 0) continue;
      const caller = pick(open);
      trace.push(`abort #${caller.n}`);
      caller.aborted = true;
      caller.controller?.abort();
    } else if (roll < 0.58) {
      trace.push("a command that is not a shared read");
      void invoke("cmd_some_write", { x: step });
    } else if (roll < 0.76) {
      if (pending.length === 0) continue;
      const request = pick(pending);
      if (mayCancelAnswer(request)) answerOne(request, next() < 0.7 ? "cancellation" : "data");
      else if (deferrals > 0 && next() < 0.5) {
        deferrals -= 1;
        answerOne(request, "deferred");
      } else answerOne(request, "data");
    } else if (roll < 0.88) {
      if (naps.length === 0) continue;
      trace.push("a backoff timer fires");
      naps.splice(Math.floor(next() * naps.length), 1)[0]();
    } else {
      trace.push("callers' leave check runs");
      await tick();
    }
    await micro();
    check();
  }

  // Everything drains: timers fire, the backend answers what is left, the way it
  // would (a request it was told to stop ends as the cancellation).
  for (let round = 0; round < 60 && (pending.length > 0 || naps.length > 0); round += 1) {
    await tick();
    while (naps.length > 0) naps.shift()?.();
    await tick();
    for (const request of [...pending]) answerOne(request, mayCancelAnswer(request) ? "cancellation" : "data");
    await tick();
    check();
  }
  await tick();
  check();
  if (pending.length > 0 || naps.length > 0) fail("the model did not drain");
  for (const caller of callers) {
    if (caller.aborted) continue;
    if (caller.outcome.state !== "data") fail(`(ii) caller ${caller.n} (never aborted) ended ${JSON.stringify(caller.outcome)}`);
  }
}

describe("a cancellable shared read, under random interleavings", () => {
  beforeAll(() => {
    configureSharedReads(SHARED_READ_COMMANDS);
    configureReadCancellation(CANCELLABLE_READ_COMMANDS, cancelRead);
  });
  afterAll(() => {
    configureSharedReads(new Set());
    configureReadCancellation(new Set(), null);
  });
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it(`holds (i) to (iv) on ${SEEDS.length} seeded runs of ${STEPS} steps`, async () => {
    const failures: string[] = [];
    for (const seed of SEEDS) {
      try {
        await run(seed);
      } catch (error) {
        failures.push((error as Error).message);
        if (failures.length >= 3) break;
      }
    }
    expect(failures.join("\n\n"), "seeds that broke an invariant").toBe("");
  });
});
