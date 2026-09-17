/**
 * When a delivery poll may run, how often, and when it must stop.
 *
 * Both live sources reach an external CLI: `gh run list` is capped at 45s and
 * counts against the user's GitHub rate limit, and `firebase
 * apphosting:rollouts:list` is no cheaper. A naive `setInterval` over either
 * is a subprocess every few seconds for as long as the app is open, which is
 * why every decision here is a refusal by default: the poll runs only while
 * something is actually moving, and it gives up rather than run forever.
 *
 * Pure and clock-injected. The scheduling policy is the part that has to be
 * right — a timer is easy to test, a decision about whether to spend a
 * subprocess is not — so it lives here rather than inside a component.
 */

/** Bounds on a live poll. Every field is a ceiling, never a target. */
export interface PollLimits {
  /** Delay between polls while everything is healthy. */
  baseIntervalMs: number;
  /** Ceiling the exponential backoff saturates at. */
  maxIntervalMs: number;
  /**
   * How long one live-poll session may last before it gives up.
   *
   * A run can sit `queued` for hours waiting for a runner, and a rollout can
   * sit `PENDING_BUILD` just as long. Without this, one such row keeps a
   * subprocess timer alive for the lifetime of the window.
   */
  maxDurationMs: number;
  /**
   * How many polls one session may issue, whatever the elapsed time.
   *
   * `maxDurationMs` alone is not enough: a machine that suspends and resumes
   * sees elapsed time jump, and a clock that moves backwards makes elapsed
   * time useless. A monotonically increasing count cannot be confused by
   * either.
   */
  maxPolls: number;
  /** Consecutive failures after which the session gives up entirely. */
  maxFailures: number;
}

/**
 * Defaults, chosen against the cost of the call they schedule.
 *
 * 6s is the same cadence the work-tree status poll already runs at, so the
 * delivery poll adds no new rhythm to the app. The 15-minute ceiling is longer
 * than almost every CI run and shorter than a wait for a runner that is never
 * coming; a session that hits it has stopped being a live view of something
 * happening and become a background subprocess loop.
 */
export const DEFAULT_POLL_LIMITS: PollLimits = {
  baseIntervalMs: 6_000,
  maxIntervalMs: 60_000,
  maxDurationMs: 15 * 60_000,
  maxPolls: 200,
  maxFailures: 5,
};

/** Everything one live-poll session remembers. */
export interface PollState {
  /**
   * When this session started polling; null when no session is open.
   *
   * A session opens the first time something in flight is observed and closes
   * when nothing is.
   */
  startedAt: number | null;
  /** Polls issued in this session. Reset with the session, never mid-flight. */
  polls: number;
  /** Consecutive failures since the last success. */
  failures: number;
}

/** A session that has not started. */
export function idlePollState(): PollState {
  return { startedAt: null, polls: 0, failures: 0 };
}

export type PollDecisionKind =
  /** Poll again after `delayMs`. */
  | "poll"
  /** Nothing is moving; do not schedule anything. */
  | "idle"
  /** Something is still moving, but this session has spent its budget. */
  | "exhausted";

export interface PollDecision {
  kind: PollDecisionKind;
  /** Only meaningful when `kind` is `poll`; zero otherwise. */
  delayMs: number;
  /**
   * Why this decision was reached, always populated.
   *
   * An exhausted session is the case that must reach the screen: live updates
   * having silently stopped, while stale rows keep rendering as though they
   * were current, is exactly the failure this whole module exists to avoid.
   */
  reason: string;
}

/** Smallest delay this module will ever schedule. */
const MIN_DELAY_MS = 1;

/**
 * One limit, coerced into something that can actually bind.
 *
 * Every ceiling in this module is compared with `>=`, and every comparison
 * against `NaN` is false — so a single non-finite limit does not merely
 * misbehave, it silently *removes* that ceiling while the struct still looks
 * fully configured. That is the failure class this whole module is written
 * against: a check that could not run must never be indistinguishable from a
 * check that ran and passed.
 *
 * Non-finite falls back to the shipped default rather than to zero: zero is an
 * unthrottled loop in the interval fields and an instantly-exhausted session
 * in the budget fields, and neither is a safe reading of "unspecified".
 */
function resolveLimit(value: number, fallback: number): number {
  if (!Number.isFinite(value)) return fallback;
  return Math.max(MIN_DELAY_MS, Math.floor(value));
}

/**
 * The limits actually applied, with every field guaranteed finite and >= 1.
 *
 * The single place that decides what a valid limit is, so `decidePoll` and
 * `backoffDelayMs` cannot disagree about a malformed one.
 */
export function resolveLimits(limits: PollLimits = DEFAULT_POLL_LIMITS): PollLimits {
  const baseIntervalMs = resolveLimit(limits.baseIntervalMs, DEFAULT_POLL_LIMITS.baseIntervalMs);
  return {
    baseIntervalMs,
    // A ceiling below the base would invert the backoff into a *faster* poll
    // on failure, which is the opposite of backing off.
    maxIntervalMs: Math.max(
      baseIntervalMs,
      resolveLimit(limits.maxIntervalMs, DEFAULT_POLL_LIMITS.maxIntervalMs),
    ),
    maxDurationMs: resolveLimit(limits.maxDurationMs, DEFAULT_POLL_LIMITS.maxDurationMs),
    maxPolls: resolveLimit(limits.maxPolls, DEFAULT_POLL_LIMITS.maxPolls),
    maxFailures: resolveLimit(limits.maxFailures, DEFAULT_POLL_LIMITS.maxFailures),
  };
}

/**
 * Backoff delay for a given number of consecutive failures.
 *
 * Doubling, saturating at the ceiling, with no jitter. Jitter defends a shared
 * server against many clients waking together; this schedules one desktop
 * app's own subprocess against its own repository, and determinism here is
 * worth more than herd protection that has no herd.
 *
 * Guards the shift rather than trusting it: `2 ** 31` is not a number a delay
 * should ever be, and `Math.min` would happily return a ceiling computed from
 * `Infinity` while looking correct.
 */
export function backoffDelayMs(failures: number, limits: PollLimits): number {
  const { baseIntervalMs: base, maxIntervalMs: ceiling } = resolveLimits(limits);
  if (!Number.isFinite(failures) || failures <= 0) return base;
  // Cap the exponent before it is used. 2**30 * base already exceeds every
  // ceiling by orders of magnitude, so saturating here changes no answer while
  // keeping the arithmetic inside safe integers.
  const exponent = Math.min(Math.floor(failures), 30);
  const scaled = base * 2 ** exponent;
  return Math.min(ceiling, scaled);
}

/**
 * Whether to poll now, and how long to wait.
 *
 * `anyInFlight` is the gate: false means no timer at all, which is the normal
 * state of a repository nobody is deploying. Everything else is a budget
 * check, and every budget failure returns `exhausted` with a reason rather
 * than an ever-longer delay — a poll stretched to an hour is indistinguishable
 * from a dead one, and only one of those can be reported honestly.
 */
export function decidePoll(input: {
  anyInFlight: boolean;
  state: PollState;
  now: number;
  limits?: PollLimits;
}): PollDecision {
  const limits = resolveLimits(input.limits ?? DEFAULT_POLL_LIMITS);
  const { anyInFlight, state, now } = input;

  if (!anyInFlight) {
    return { kind: "idle", delayMs: 0, reason: "nothing in flight" };
  }
  if (state.failures >= limits.maxFailures) {
    return {
      kind: "exhausted",
      delayMs: 0,
      reason: `stopped after ${state.failures} consecutive failures`,
    };
  }
  if (state.polls >= limits.maxPolls) {
    return {
      kind: "exhausted",
      delayMs: 0,
      reason: `stopped after ${state.polls} polls`,
    };
  }
  if (state.startedAt !== null) {
    const elapsed = now - state.startedAt;
    // A negative elapsed means the clock moved backwards between the session
    // opening and this decision. That is not an expiry — expiring on it would
    // kill a healthy session — but it is also not evidence of remaining
    // budget, which is what `maxPolls` is for.
    if (Number.isFinite(elapsed) && elapsed >= limits.maxDurationMs) {
      return {
        kind: "exhausted",
        delayMs: 0,
        reason: `stopped after ${Math.round(elapsed / 60_000)} minutes of live polling`,
      };
    }
  }
  const delayMs = backoffDelayMs(state.failures, limits);
  return {
    kind: "poll",
    delayMs,
    reason:
      state.failures > 0
        ? `retrying after ${state.failures} failure(s)`
        : "watching work in flight",
  };
}

/**
 * The session state after one poll settled.
 *
 * Opens a session on the first poll and counts every one, so the budget can
 * never be reset by a transient lull: a row that flickers out of flight and
 * back does not buy a fresh 200 polls. The session is closed by
 * [`closedPollSession`] when nothing is in flight, and only then.
 */
export function afterPoll(state: PollState, outcome: { ok: boolean }, now: number): PollState {
  return {
    startedAt: state.startedAt ?? now,
    // Saturate rather than overflow. A counter that wraps is a budget that
    // silently renews itself.
    polls: Math.min(state.polls + 1, Number.MAX_SAFE_INTEGER),
    failures: outcome.ok ? 0 : Math.min(state.failures + 1, Number.MAX_SAFE_INTEGER),
  };
}

/**
 * The session state once nothing is in flight.
 *
 * This is the only reset. Returning to idle is what earns a fresh budget,
 * because it means the thing being watched finished — as opposed to a poll
 * that keeps failing, which must stay exhausted until the user asks again.
 */
export function closedPollSession(): PollState {
  return idlePollState();
}
