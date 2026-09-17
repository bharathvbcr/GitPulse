import { describe, expect, it } from "vitest";
import {
  afterPoll,
  backoffDelayMs,
  closedPollSession,
  decidePoll,
  DEFAULT_POLL_LIMITS,
  idlePollState,
  resolveLimits,
  type PollLimits,
  type PollState,
} from "./pollSchedule";

const LIMITS: PollLimits = {
  baseIntervalMs: 1_000,
  maxIntervalMs: 16_000,
  maxDurationMs: 60_000,
  maxPolls: 10,
  maxFailures: 3,
};

const state = (overrides: Partial<PollState> = {}): PollState => ({
  ...idlePollState(),
  ...overrides,
});

describe("the poll gate", () => {
  it("schedules nothing while nothing is in flight", () => {
    // The normal state of a repository nobody is deploying. A timer here is a
    // subprocess every few seconds for the lifetime of the window.
    const decision = decidePoll({
      anyInFlight: false,
      state: state({ startedAt: 0, polls: 3 }),
      now: 1_000,
      limits: LIMITS,
    });
    expect(decision.kind).toBe("idle");
    expect(decision.delayMs).toBe(0);
  });

  it("polls at the base interval while work is in flight and healthy", () => {
    const decision = decidePoll({ anyInFlight: true, state: state(), now: 0, limits: LIMITS });
    expect(decision.kind).toBe("poll");
    expect(decision.delayMs).toBe(LIMITS.baseIntervalMs);
  });

  it("always gives a reason, whatever it decided", () => {
    // An exhausted session that says nothing is indistinguishable on screen
    // from a live one, which is the failure this module exists to prevent.
    for (const anyInFlight of [true, false]) {
      for (const s of [state(), state({ failures: 99 }), state({ polls: 999 })]) {
        const decision = decidePoll({ anyInFlight, state: s, now: 10, limits: LIMITS });
        expect(decision.reason.trim(), JSON.stringify({ anyInFlight, s })).not.toBe("");
      }
    }
  });
});

describe("backoff", () => {
  it("doubles per consecutive failure and saturates at the ceiling", () => {
    expect(backoffDelayMs(0, LIMITS)).toBe(1_000);
    expect(backoffDelayMs(1, LIMITS)).toBe(2_000);
    expect(backoffDelayMs(2, LIMITS)).toBe(4_000);
    expect(backoffDelayMs(3, LIMITS)).toBe(8_000);
    expect(backoffDelayMs(4, LIMITS)).toBe(16_000);
    // Saturated, not overflowing past the ceiling.
    expect(backoffDelayMs(5, LIMITS)).toBe(16_000);
  });

  it("stays a finite number at absurd failure counts", () => {
    // `base * 2 ** 1e9` is Infinity, and `Math.min(ceiling, Infinity)` happens
    // to be correct — but `2 ** 1e9` first leaves the safe integer range, and
    // an unguarded shift elsewhere in that expression would not be so lucky.
    for (const failures of [31, 64, 1_000, 1e9, Number.MAX_SAFE_INTEGER]) {
      const delay = backoffDelayMs(failures, LIMITS);
      expect(Number.isFinite(delay), `failures=${failures}`).toBe(true);
      expect(delay).toBe(LIMITS.maxIntervalMs);
    }
  });

  it("returns the base interval for values that are not counts", () => {
    for (const failures of [Number.NaN, Number.POSITIVE_INFINITY, -1, -1e9]) {
      expect(backoffDelayMs(failures, LIMITS), `failures=${failures}`).toBe(
        LIMITS.baseIntervalMs,
      );
    }
  });

  it("never returns a delay below one millisecond, even on nonsense limits", () => {
    // A zero or negative interval is an unthrottled loop: setInterval(fn, 0).
    for (const baseIntervalMs of [0, -1, -60_000, Number.NaN]) {
      const delay = backoffDelayMs(0, { ...LIMITS, baseIntervalMs });
      expect(delay, `base=${baseIntervalMs}`).toBeGreaterThanOrEqual(1);
    }
  });

  it("never returns less than the base when the ceiling is below it", () => {
    // A misconfigured ceiling must not invert the backoff into a faster poll.
    const inverted = { ...LIMITS, baseIntervalMs: 10_000, maxIntervalMs: 1_000 };
    expect(backoffDelayMs(0, inverted)).toBe(10_000);
    expect(backoffDelayMs(5, inverted)).toBe(10_000);
  });
});

describe("budget exhaustion", () => {
  it("gives up after the failure limit instead of backing off forever", () => {
    const decision = decidePoll({
      anyInFlight: true,
      state: state({ failures: LIMITS.maxFailures }),
      now: 0,
      limits: LIMITS,
    });
    expect(decision.kind).toBe("exhausted");
    expect(decision.reason).toContain("consecutive failures");
  });

  it("gives up after the poll count limit", () => {
    const decision = decidePoll({
      anyInFlight: true,
      state: state({ startedAt: 0, polls: LIMITS.maxPolls }),
      now: 1_000,
      limits: LIMITS,
    });
    expect(decision.kind).toBe("exhausted");
    expect(decision.reason).toContain("polls");
  });

  it("gives up once the session has run longer than its ceiling", () => {
    const decision = decidePoll({
      anyInFlight: true,
      state: state({ startedAt: 0, polls: 1 }),
      now: LIMITS.maxDurationMs,
      limits: LIMITS,
    });
    expect(decision.kind).toBe("exhausted");
    expect(decision.reason).toContain("minutes");
  });

  it("keeps polling one tick below every ceiling", () => {
    // The boundary in the other direction: an off-by-one here silently halves
    // the budget, and nothing would ever report it.
    const decision = decidePoll({
      anyInFlight: true,
      state: state({ startedAt: 0, polls: LIMITS.maxPolls - 1, failures: LIMITS.maxFailures - 1 }),
      now: LIMITS.maxDurationMs - 1,
      limits: LIMITS,
    });
    expect(decision.kind).toBe("poll");
  });

  it("does not expire a session when the clock moves backwards", () => {
    // Suspend/resume and NTP corrections both do this. Expiring on a negative
    // elapsed would kill a healthy poll the moment the clock was adjusted.
    const decision = decidePoll({
      anyInFlight: true,
      state: state({ startedAt: 10_000_000, polls: 1 }),
      now: 5_000_000,
      limits: LIMITS,
    });
    expect(decision.kind).toBe("poll");
  });

  it("does not expire a session on an unreadable clock", () => {
    // NaN >= anything is false, so this already passes — pinned because the
    // obvious refactor to `Math.abs(elapsed) >= max` would break it, and the
    // failure mode is a poll that stops for no stated reason.
    const decision = decidePoll({
      anyInFlight: true,
      state: state({ startedAt: 0, polls: 1 }),
      now: Number.NaN,
      limits: LIMITS,
    });
    expect(decision.kind).toBe("poll");
  });

  it("cannot be talked out of exhaustion by a fresh clock", () => {
    // Failure and poll budgets are checked before elapsed time, so a session
    // that has burned them stays exhausted even at `now === startedAt`.
    const decision = decidePoll({
      anyInFlight: true,
      state: state({ startedAt: 0, polls: LIMITS.maxPolls, failures: 0 }),
      now: 0,
      limits: LIMITS,
    });
    expect(decision.kind).toBe("exhausted");
  });
});

describe("session accounting", () => {
  it("opens the session on the first poll and keeps that instant", () => {
    const first = afterPoll(idlePollState(), { ok: true }, 5_000);
    expect(first.startedAt).toBe(5_000);
    expect(first.polls).toBe(1);
    const second = afterPoll(first, { ok: true }, 9_000);
    expect(second.startedAt, "the session start must not slide forward").toBe(5_000);
    expect(second.polls).toBe(2);
  });

  it("resets failures on success and counts them on failure", () => {
    let s = afterPoll(idlePollState(), { ok: false }, 0);
    expect(s.failures).toBe(1);
    s = afterPoll(s, { ok: false }, 1);
    expect(s.failures).toBe(2);
    s = afterPoll(s, { ok: true }, 2);
    expect(s.failures, "one success clears the backoff").toBe(0);
    expect(s.polls).toBe(3);
  });

  it("saturates its counters instead of overflowing", () => {
    // A counter that wraps is a budget that silently renews itself.
    const huge = state({
      startedAt: 0,
      polls: Number.MAX_SAFE_INTEGER,
      failures: Number.MAX_SAFE_INTEGER,
    });
    const next = afterPoll(huge, { ok: false }, 1);
    expect(next.polls).toBe(Number.MAX_SAFE_INTEGER);
    expect(next.failures).toBe(Number.MAX_SAFE_INTEGER);
    expect(Number.isSafeInteger(next.polls)).toBe(true);
  });

  it("does not renew the budget when a row flickers out of flight and back", () => {
    // The budget must be spent by the work, not by the sampling. Without
    // this, a listing that briefly shows nothing in flight hands the session
    // a fresh ceiling every time it flickers.
    let s = idlePollState();
    for (let i = 0; i < LIMITS.maxPolls; i += 1) s = afterPoll(s, { ok: true }, i);
    expect(decidePoll({ anyInFlight: true, state: s, now: 100, limits: LIMITS }).kind).toBe(
      "exhausted",
    );
    // A single idle decision does not itself reset anything...
    decidePoll({ anyInFlight: false, state: s, now: 101, limits: LIMITS });
    expect(decidePoll({ anyInFlight: true, state: s, now: 102, limits: LIMITS }).kind).toBe(
      "exhausted",
    );
    // ...only closing the session does, which the caller does when the work
    // has actually finished.
    expect(
      decidePoll({ anyInFlight: true, state: closedPollSession(), now: 103, limits: LIMITS }).kind,
    ).toBe("poll");
  });
});

describe("a malformed limit cannot remove a ceiling", () => {
  // Every ceiling is compared with `>=`, and every comparison against NaN is
  // false. An unsanitized non-finite limit therefore does not misbehave
  // loudly — it deletes that ceiling while the struct still looks configured,
  // and the session polls until the window closes.
  it("still exhausts on failures when the failure limit is not a number", () => {
    const decision = decidePoll({
      anyInFlight: true,
      state: state({ failures: 10_000 }),
      now: 0,
      limits: { ...LIMITS, maxFailures: Number.NaN },
    });
    expect(decision.kind).toBe("exhausted");
  });

  it("still exhausts on poll count when the poll limit is not a number", () => {
    const decision = decidePoll({
      anyInFlight: true,
      state: state({ startedAt: 0, polls: 10_000 }),
      now: 1,
      limits: { ...LIMITS, maxPolls: Number.POSITIVE_INFINITY },
    });
    expect(decision.kind).toBe("exhausted");
  });

  it("still exhausts on elapsed time when the duration limit is not a number", () => {
    const decision = decidePoll({
      anyInFlight: true,
      state: state({ startedAt: 0, polls: 1 }),
      now: 24 * 60 * 60_000,
      limits: { ...LIMITS, maxDurationMs: Number.NaN },
    });
    expect(decision.kind).toBe("exhausted");
  });

  it("resolves every limit to a finite, bindable number", () => {
    const resolved = resolveLimits({
      baseIntervalMs: Number.NaN,
      maxIntervalMs: -5,
      maxDurationMs: Number.POSITIVE_INFINITY,
      maxPolls: 0,
      maxFailures: Number.NEGATIVE_INFINITY,
    });
    for (const [key, value] of Object.entries(resolved)) {
      expect(Number.isFinite(value), `${key} must be finite`).toBe(true);
      expect(value, `${key} must be able to bind`).toBeGreaterThanOrEqual(1);
    }
    expect(resolved.maxIntervalMs).toBeGreaterThanOrEqual(resolved.baseIntervalMs);
  });
});

describe("shipped defaults", () => {
  it("are internally consistent", () => {
    const d = DEFAULT_POLL_LIMITS;
    expect(d.baseIntervalMs).toBeGreaterThan(0);
    expect(d.maxIntervalMs).toBeGreaterThanOrEqual(d.baseIntervalMs);
    expect(d.maxPolls).toBeGreaterThan(0);
    expect(d.maxFailures).toBeGreaterThan(0);
    // The two ceilings guard different things and must not be set so that the
    // count one fires first under a healthy clock. `maxDurationMs` is the
    // intended bound on a live session; `maxPolls` is the backstop for a clock
    // that suspends, resumes or steps backwards, where elapsed time proves
    // nothing. If the count bound bit first, a normal session would be cut
    // short by the mechanism meant to catch a broken one.
    expect(d.maxPolls * d.baseIntervalMs).toBeGreaterThanOrEqual(d.maxDurationMs);
  });

  it("bounds the worst case a single session can cost", () => {
    // The number that matters for a 45s-capped subprocess on a rate limit: at
    // most this many gh calls per session, and the session cannot outlive the
    // duration ceiling.
    expect(DEFAULT_POLL_LIMITS.maxPolls).toBeLessThanOrEqual(200);
    expect(DEFAULT_POLL_LIMITS.maxDurationMs).toBeLessThanOrEqual(30 * 60_000);
  });
});
