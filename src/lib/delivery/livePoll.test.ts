import { describe, expect, it } from "vitest";
import type { IntervalHost } from "../dom/visibleInterval";
import { createLivePoll, type LiveState } from "./livePoll";
import type { PollLimits } from "./pollSchedule";

const LIMITS: PollLimits = {
  baseIntervalMs: 1_000,
  maxIntervalMs: 8_000,
  maxDurationMs: 100_000,
  maxPolls: 6,
  maxFailures: 3,
};

/**
 * A controllable stand-in for the browser's timers and visibility.
 *
 * Counts handles so a leaked interval is a test failure rather than a slow
 * memory climb nobody notices.
 */
function makeHost() {
  let hidden = false;
  const timers = new Map<number, () => void>();
  const listeners = new Set<() => void>();
  let nextHandle = 1;
  let clock = 0;

  const host: IntervalHost = {
    setInterval: (handler) => {
      const handle = nextHandle++;
      timers.set(handle, handler);
      return handle;
    },
    clearInterval: (handle) => {
      timers.delete(handle as number);
    },
    addEventListener: (_type, listener) => listeners.add(listener),
    removeEventListener: (_type, listener) => listeners.delete(listener),
    isHidden: () => hidden,
  };

  return {
    host,
    now: () => clock,
    advance(ms: number) {
      clock += ms;
    },
    /** Fire every live interval once, as the browser would. */
    fire() {
      for (const handler of [...timers.values()]) handler();
    },
    get liveTimers() {
      return timers.size;
    },
    get liveListeners() {
      return listeners.size;
    },
    setHidden(value: boolean) {
      hidden = value;
      for (const listener of [...listeners]) listener();
    },
  };
}

function setup(
  options: {
    poll?: () => Promise<boolean>;
    limits?: PollLimits;
  } = {},
) {
  const h = makeHost();
  const states: LiveState[] = [];
  let polls = 0;
  const poll =
    options.poll ??
    (() => {
      polls += 1;
      return Promise.resolve(true);
    });
  const live = createLivePoll({
    poll: () => {
      if (options.poll) polls += 1;
      return poll();
    },
    onState: (s) => states.push({ ...s }),
    limits: options.limits ?? LIMITS,
    host: h.host,
    now: h.now,
  });
  return { h, live, states, pollCount: () => polls };
}

describe("the timer only exists while something is moving", () => {
  it("runs no timer for a repository with nothing in flight", () => {
    // The normal state. A timer here is a subprocess every few seconds for
    // the lifetime of the window.
    const { h, live, states } = setup();
    live.sync(false);
    expect(h.liveTimers).toBe(0);
    // Idle is the state it starts in, so there is nothing to announce. An
    // emission here would invalidate derived state across the panel on every
    // render of a repository with no delivery activity at all.
    expect(states).toEqual([]);
    live.dispose();
  });

  it("starts a timer when work appears and stops it when the work ends", () => {
    const { h, live, states } = setup();
    live.sync(true);
    expect(h.liveTimers).toBe(1);
    expect(states.at(-1)?.kind).toBe("live");
    live.sync(false);
    expect(h.liveTimers).toBe(0);
    expect(states.at(-1)?.kind).toBe("idle");
    live.dispose();
  });

  it("does not stack timers when told the same thing repeatedly", () => {
    // `sync` is called from a reactive statement, so it fires on every
    // unrelated re-render. Each call must not add a timer.
    const { h, live } = setup();
    for (let i = 0; i < 25; i += 1) live.sync(true);
    expect(h.liveTimers).toBe(1);
    live.dispose();
  });

  it("leaves no timer or listener behind on dispose", () => {
    const { h, live } = setup();
    live.sync(true);
    expect(h.liveTimers).toBe(1);
    expect(h.liveListeners).toBe(1);
    live.dispose();
    expect(h.liveTimers).toBe(0);
    expect(h.liveListeners).toBe(0);
  });
});

describe("polls never overlap", () => {
  it("skips a tick while the previous poll is still running", async () => {
    // `gh run list` is capped at 45s against a 6s cadence; without this a slow
    // call stacks a new subprocess on every tick.
    // Boxed rather than a bare `let`: TypeScript cannot see the assignment
    // inside the promise executor and narrows a plain binding to `null`.
    const box: { release: ((ok: boolean) => void) | null } = { release: null };
    const { h, live, pollCount } = setup({
      poll: () => new Promise<boolean>((resolve) => (box.release = resolve)),
    });
    live.sync(true);
    h.advance(LIMITS.baseIntervalMs);
    h.fire();
    expect(pollCount()).toBe(1);
    for (let i = 0; i < 10; i += 1) {
      h.advance(LIMITS.baseIntervalMs);
      h.fire();
    }
    expect(pollCount(), "one in-flight poll, whatever the tick count").toBe(1);
    box.release?.(true);
    await Promise.resolve();
    expect(pollCount()).toBe(1);
    live.dispose();
  });

  it("respects the backoff interval between polls", async () => {
    const { h, live, pollCount } = setup({ poll: () => Promise.resolve(false) });
    live.sync(true);
    h.advance(LIMITS.baseIntervalMs);
    h.fire();
    await Promise.resolve();
    expect(pollCount()).toBe(1);
    // One failure doubles the delay to 2s, so a tick at +1s must not poll.
    h.advance(1_000);
    h.fire();
    await Promise.resolve();
    expect(pollCount(), "backoff not yet elapsed").toBe(1);
    h.advance(1_100);
    h.fire();
    await Promise.resolve();
    expect(pollCount()).toBe(2);
    live.dispose();
  });
});

describe("giving up is visible", () => {
  it("pauses with a reason after repeated failures, and stops the timer", async () => {
    const { h, live, states, pollCount } = setup({ poll: () => Promise.resolve(false) });
    live.sync(true);
    for (let i = 0; i < 40; i += 1) {
      h.advance(LIMITS.maxIntervalMs);
      h.fire();
      await Promise.resolve();
    }
    expect(pollCount()).toBe(LIMITS.maxFailures);
    const last = states.at(-1);
    expect(last?.kind).toBe("paused");
    expect(last?.reason).toContain("consecutive failures");
    expect(h.liveTimers, "an exhausted session must not keep a timer").toBe(0);
    live.dispose();
  });

  it("pauses after the poll budget even when every poll succeeds", async () => {
    // A run stuck queued for hours: nothing is failing, so only the budget
    // stops it.
    const { h, live, states, pollCount } = setup();
    live.sync(true);
    for (let i = 0; i < 40; i += 1) {
      h.advance(LIMITS.baseIntervalMs);
      h.fire();
      await Promise.resolve();
    }
    expect(pollCount()).toBe(LIMITS.maxPolls);
    expect(states.at(-1)?.kind).toBe("paused");
    expect(h.liveTimers).toBe(0);
    live.dispose();
  });

  it("never reports paused without saying why", async () => {
    const { h, live, states } = setup({ poll: () => Promise.resolve(false) });
    live.sync(true);
    for (let i = 0; i < 20; i += 1) {
      h.advance(LIMITS.maxIntervalMs);
      h.fire();
      await Promise.resolve();
    }
    for (const state of states.filter((s) => s.kind === "paused")) {
      expect(state.reason.trim()).not.toBe("");
    }
    live.dispose();
  });

  it("stays paused until something explicitly resets it", async () => {
    const { h, live, states, pollCount } = setup({ poll: () => Promise.resolve(false) });
    live.sync(true);
    for (let i = 0; i < 20; i += 1) {
      h.advance(LIMITS.maxIntervalMs);
      h.fire();
      await Promise.resolve();
    }
    const spent = pollCount();
    // More syncs while still in flight must not revive it: the sampling
    // showing work is not new information.
    for (let i = 0; i < 5; i += 1) live.sync(true);
    h.advance(LIMITS.maxIntervalMs);
    h.fire();
    await Promise.resolve();
    expect(pollCount()).toBe(spent);
    expect(states.at(-1)?.kind).toBe("paused");

    live.reset();
    expect(states.at(-1)?.kind).toBe("live");
    h.advance(LIMITS.baseIntervalMs);
    h.fire();
    await Promise.resolve();
    expect(pollCount()).toBe(spent + 1);
    live.dispose();
  });

  it("refunds the budget when the work actually finishes", async () => {
    const { h, live, pollCount } = setup();
    live.sync(true);
    for (let i = 0; i < 4; i += 1) {
      h.advance(LIMITS.baseIntervalMs);
      h.fire();
      await Promise.resolve();
    }
    const spent = pollCount();
    expect(spent).toBeGreaterThan(0);
    // Work ends, then new work starts: a fresh session with a full budget.
    live.sync(false);
    live.sync(true);
    for (let i = 0; i < 40; i += 1) {
      h.advance(LIMITS.baseIntervalMs);
      h.fire();
      await Promise.resolve();
    }
    expect(pollCount(), "a closed session earns a new budget").toBe(spent + LIMITS.maxPolls);
    live.dispose();
  });
});

describe("visibility", () => {
  it("spends no polls while the window is hidden", async () => {
    const { h, live, pollCount } = setup();
    live.sync(true);
    h.setHidden(true);
    for (let i = 0; i < 10; i += 1) {
      h.advance(LIMITS.baseIntervalMs);
      h.fire();
      await Promise.resolve();
    }
    expect(pollCount(), "a hidden window must not spend subprocesses").toBe(0);
    live.dispose();
  });

  it("catches up once the window comes back", async () => {
    const { h, live, pollCount } = setup();
    live.sync(true);
    h.setHidden(true);
    h.advance(60_000);
    h.setHidden(false);
    await Promise.resolve();
    // `createVisibleInterval` runs the tick immediately on rejoin, so what is
    // on screen is current at the moment someone looks at it.
    expect(pollCount()).toBe(1);
    live.dispose();
  });
});

describe("teardown races", () => {
  it("ignores a poll that settles after dispose", async () => {
    // The panel remounts per repository. A poll settling afterwards must not
    // reopen a session against a repository nobody is looking at.
    // Boxed rather than a bare `let`: TypeScript cannot see the assignment
    // inside the promise executor and narrows a plain binding to `null`.
    const box: { release: ((ok: boolean) => void) | null } = { release: null };
    const { h, live, states } = setup({
      poll: () => new Promise<boolean>((resolve) => (box.release = resolve)),
    });
    live.sync(true);
    h.advance(LIMITS.baseIntervalMs);
    h.fire();
    const before = states.length;
    live.dispose();
    box.release?.(true);
    await Promise.resolve();
    await Promise.resolve();
    expect(states.length, "no state change after dispose").toBe(before);
    expect(h.liveTimers).toBe(0);
  });

  it("ignores every call made after dispose", () => {
    const { h, live, states } = setup();
    live.dispose();
    const before = states.length;
    live.sync(true);
    live.reset();
    live.sync(false);
    expect(states.length).toBe(before);
    expect(h.liveTimers).toBe(0);
  });

  it("treats a rejected poll as a failure rather than letting it escape", async () => {
    // An unhandled rejection in a timer callback is invisible in production
    // and kills the loop; it must count as a failure and back off.
    const { h, live, states, pollCount } = setup({
      poll: () => Promise.reject(new Error("gh exploded")),
    });
    live.sync(true);
    h.advance(LIMITS.baseIntervalMs);
    h.fire();
    await Promise.resolve();
    await Promise.resolve();
    expect(pollCount()).toBe(1);
    // It kept going rather than dying silently...
    h.advance(LIMITS.maxIntervalMs);
    h.fire();
    await Promise.resolve();
    await Promise.resolve();
    expect(pollCount()).toBe(2);
    // ...and eventually gives up out loud.
    for (let i = 0; i < 20; i += 1) {
      h.advance(LIMITS.maxIntervalMs);
      h.fire();
      await Promise.resolve();
      await Promise.resolve();
    }
    expect(states.at(-1)?.kind).toBe("paused");
    live.dispose();
  });
});

describe("state reporting", () => {
  it("emits a state only when it actually changes", () => {
    // Every emission invalidates derived state across the panel.
    const { h, live, states } = setup();
    live.sync(true);
    const after = states.length;
    for (let i = 0; i < 20; i += 1) live.sync(true);
    expect(states.length, "no churn from repeated identical syncs").toBe(after);
    live.dispose();
    void h;
  });
});
