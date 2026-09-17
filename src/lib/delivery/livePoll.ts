/**
 * The loop that turns [`decidePoll`] into an actual timer.
 *
 * Built on `createVisibleInterval` rather than a bare `setInterval`, and
 * shaped the way the work-tree status poll already is: a fixed cadence with a
 * gate that skips ticks. The novel part is that the interval itself only
 * exists while something is in flight — a repository nobody is deploying runs
 * no timer at all, which is the difference between a renderer the OS can leave
 * alone and one it cannot.
 *
 * Both panels drive this rather than owning a loop each. Two copies of a
 * retry-and-give-up loop is two places for the give-up to be forgotten.
 */
import { createVisibleInterval, type IntervalHost } from "../dom/visibleInterval";
import {
  afterPoll,
  closedPollSession,
  decidePoll,
  idlePollState,
  resolveLimits,
  type PollLimits,
  type PollState,
} from "./pollSchedule";

/** How the poll is doing, for the badge that says so. */
export interface LiveState {
  kind: "live" | "idle" | "paused";
  /** Empty only when idle; always populated when paused. */
  reason: string;
}

export interface LivePoll {
  /**
   * Tell the loop whether anything is moving. Call whenever the rows change.
   *
   * Idempotent: calling it with the same answer repeatedly neither restarts
   * the timer nor re-opens a closed session.
   */
  sync(anyInFlight: boolean): void;
  /**
   * Forget an exhausted session and start over.
   *
   * What the manual Refresh button is for: a session that gave up must be
   * revivable by an explicit human action, and only by one.
   */
  reset(): void;
  /** Stop everything. Any poll still in flight is ignored when it settles. */
  dispose(): void;
}

export function createLivePoll(options: {
  /** Runs one poll. Resolves true when it succeeded, false when it failed. */
  poll: () => Promise<boolean>;
  /** Called whenever the display state changes, and only then. */
  onState: (state: LiveState) => void;
  limits?: PollLimits;
  /** Injected for tests; the browser host by default. */
  host?: IntervalHost | null;
  now?: () => number;
}): LivePoll {
  const limits = resolveLimits(options.limits);
  const now = options.now ?? (() => Date.now());

  let state: PollState = idlePollState();
  let stopInterval: (() => void) | null = null;
  /** True between issuing a poll and its settling. */
  let inflight = false;
  /**
   * Earliest instant the next poll may run.
   *
   * This is how a variable backoff rides a fixed-cadence timer: the tick fires
   * at the base interval and returns immediately until the backoff has
   * elapsed. Cheaper and simpler than a chain of timeouts, and it cannot leak
   * a pending handle on teardown.
   */
  let nextAllowedAt = 0;
  let lastInFlight = false;
  let disposed = false;
  let published: LiveState = { kind: "idle", reason: "" };

  function publish(next: LiveState): void {
    if (next.kind === published.kind && next.reason === published.reason) return;
    published = next;
    options.onState(next);
  }

  function stopTimer(): void {
    if (stopInterval === null) return;
    stopInterval();
    stopInterval = null;
  }

  function startTimer(): void {
    if (stopInterval !== null || disposed) return;
    stopInterval = createVisibleInterval(tick, limits.baseIntervalMs, options.host);
  }

  function tick(): void {
    if (disposed) return;
    // Never overlap. `gh run list` is capped at 45s and the cadence is 6s, so
    // without this a slow call stacks a new subprocess every tick.
    if (inflight) return;
    const decision = decidePoll({ anyInFlight: lastInFlight, state, now: now(), limits });
    if (decision.kind !== "poll") {
      // Idle or exhausted: stop spending ticks and say which.
      stopTimer();
      publish(
        decision.kind === "idle"
          ? { kind: "idle", reason: "" }
          : { kind: "paused", reason: decision.reason },
      );
      return;
    }
    if (now() < nextAllowedAt) return;
    inflight = true;
    // Two-argument `then` rather than `.then().catch()`: a throw from inside
    // `settle` — an `onState` callback failing, say — would route into a
    // trailing `.catch` and settle the same poll a second time, double-counting
    // it against the budget. This form rejects only for the poll itself.
    void options.poll().then(
      (ok) => settle(ok),
      () => settle(false),
    );
  }

  function settle(ok: boolean): void {
    inflight = false;
    // A poll that resolves after teardown must change nothing: the panel may
    // have switched repositories, and applying this would reopen a session
    // against a repository nobody is looking at.
    if (disposed) return;
    state = afterPoll(state, { ok }, now());
    const next = decidePoll({ anyInFlight: lastInFlight, state, now: now(), limits });
    nextAllowedAt = now() + next.delayMs;
    if (next.kind === "poll") {
      publish({ kind: "live", reason: next.reason });
      startTimer();
      return;
    }
    stopTimer();
    publish(
      next.kind === "idle"
        ? { kind: "idle", reason: "" }
        : { kind: "paused", reason: next.reason },
    );
  }

  function sync(anyInFlight: boolean): void {
    if (disposed) return;
    const was = lastInFlight;
    lastInFlight = anyInFlight;

    if (!anyInFlight) {
      // Nothing is moving. Close the session — this is the only place the
      // budget is refunded, and it is earned by the work finishing rather
      // than by the sampling happening to show a lull.
      if (was) state = closedPollSession();
      stopTimer();
      publish({ kind: "idle", reason: "" });
      return;
    }

    const decision = decidePoll({ anyInFlight: true, state, now: now(), limits });
    if (decision.kind !== "poll") {
      stopTimer();
      publish({ kind: "paused", reason: decision.reason });
      return;
    }
    publish({ kind: "live", reason: decision.reason });
    startTimer();
  }

  function reset(): void {
    if (disposed) return;
    state = closedPollSession();
    nextAllowedAt = 0;
    // Re-evaluate against the rows we were last told about, so a reset while
    // something is in flight starts polling again immediately.
    sync(lastInFlight);
  }

  function dispose(): void {
    disposed = true;
    stopTimer();
  }

  return { sync, reset, dispose };
}
