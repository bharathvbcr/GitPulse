import { afterEach, describe, expect, it, vi } from "vitest";
import { createAdaptiveTimer, type AdaptiveHost } from "./adaptiveTimer";
import { resetForegroundFocus } from "./foreground";
import { LAG_HARD_MS, noteEventLoopDelay, resetEventLoopDelay } from "./loadCadence";

function fakeHost(startHidden = false) {
  let hidden = startHidden;
  const listeners = new Set<() => void>();
  type Job = { handler: () => void; ms: number };
  const jobs: Job[] = [];

  const host: AdaptiveHost = {
    setTimeout(handler, ms) {
      const job = { handler, ms };
      jobs.push(job);
      return job;
    },
    clearTimeout(handle) {
      const index = jobs.indexOf(handle as Job);
      if (index >= 0) jobs.splice(index, 1);
    },
    addEventListener(_type, listener) {
      listeners.add(listener);
    },
    removeEventListener(_type, listener) {
      listeners.delete(listener);
    },
    isHidden: () => hidden,
  };

  return {
    host,
    get pending() {
      return jobs.length;
    },
    delays() {
      return jobs.map((job) => job.ms);
    },
    fireNext() {
      const job = jobs.shift();
      job?.handler();
    },
    fireAll() {
      const due = jobs.splice(0, jobs.length);
      for (const job of due) job.handler();
    },
    setHidden(value: boolean) {
      hidden = value;
      for (const listener of [...listeners]) listener();
    },
    listenerCount() {
      return listeners.size;
    },
  };
}

describe("createAdaptiveTimer", () => {
  afterEach(() => {
    resetEventLoopDelay();
    resetForegroundFocus();
  });

  it("waits one period before the first tick and keeps a single timeout", () => {
    const env = fakeHost();
    let ticks = 0;
    createAdaptiveTimer(() => (ticks += 1), 1_000, env.host);
    expect(ticks).toBe(0);
    expect(env.delays()).toEqual([1_000]);
    env.fireNext();
    expect(ticks).toBe(1);
    expect(env.pending).toBe(1);
  });

  it("does not arm while hidden, catches up once on return, and does not stack", () => {
    const env = fakeHost(true);
    let ticks = 0;
    createAdaptiveTimer(() => (ticks += 1), 1_000, env.host);
    expect(env.pending).toBe(0);
    env.setHidden(false);
    expect(ticks).toBe(1);
    expect(env.pending).toBe(1);
    env.setHidden(true);
    expect(env.pending).toBe(0);
    env.setHidden(false);
    expect(ticks).toBe(2);
    expect(env.pending).toBe(1);
  });

  it("stretches the next arm when the event loop is hard-late", () => {
    noteEventLoopDelay(LAG_HARD_MS);
    const env = fakeHost();
    createAdaptiveTimer(() => {}, 2_000, env.host);
    expect(env.delays()).toEqual([8_000]);
  });

  it("never arms a zero delay, including for a non-positive base", () => {
    const env = fakeHost();
    createAdaptiveTimer(() => {}, 0, env.host);
    createAdaptiveTimer(() => {}, -5, env.host);
    createAdaptiveTimer(() => {}, Number.NaN, env.host);
    expect(env.pending).toBe(0);
    expect(env.delays().every((ms) => ms > 0)).toBe(true);
  });

  it("does not re-arm when the tick disposes, or when a listener fires after dispose", () => {
    const env = fakeHost();
    let dispose = () => {};
    dispose = createAdaptiveTimer(() => dispose(), 1_000, env.host);
    env.fireNext();
    expect(env.pending).toBe(0);
    env.setHidden(false);
    expect(env.pending).toBe(0);
    expect(env.listenerCount()).toBe(0);
  });

  it("re-arms after a tick that throws, so one bad poll does not kill the loop", () => {
    const env = fakeHost();
    let boom = true;
    createAdaptiveTimer(() => {
      if (boom) {
        boom = false;
        throw new Error("poll failed");
      }
    }, 500, env.host);
    expect(() => env.fireNext()).toThrow(/poll failed/);
    expect(env.pending).toBe(1);
  });

  it("is a no-op without a host", () => {
    expect(() => createAdaptiveTimer(() => {}, 1_000, null)()).not.toThrow();
  });

  it("stays at one armed timeout across a hide/show/lag storm", () => {
    const env = fakeHost();
    let ticks = 0;
    const dispose = createAdaptiveTimer(() => (ticks += 1), 1_000, env.host);
    // Focus, blur, and visibility share one listener on a set-backed host.
    expect(env.listenerCount()).toBe(1);
    const lags = [0, 100, LAG_HARD_MS, 10_000, Number.NaN, 0];
    for (let i = 0; i < 500; i += 1) {
      noteEventLoopDelay(lags[i % lags.length] ?? 0);
      if (i % 7 === 0) env.setHidden(true);
      if (i % 11 === 0) env.setHidden(false);
      if (env.pending > 0 && i % 3 === 0) env.fireNext();
      expect(env.pending, `step ${i}`).toBeLessThanOrEqual(1);
      for (const delay of env.delays()) {
        expect(delay).toBeGreaterThan(0);
        expect(Number.isFinite(delay)).toBe(true);
      }
    }
    dispose();
    env.setHidden(false);
    env.fireAll();
    expect(env.pending).toBe(0);
    expect(ticks).toBeGreaterThan(0);
  });
});

describe("browser adaptive host", () => {
  afterEach(() => {
    resetEventLoopDelay();
    resetForegroundFocus();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  function installBrowser(focused = true) {
    vi.useFakeTimers();
    let hasFocus = focused;
    const doc = new EventTarget() as EventTarget & {
      hidden: boolean;
      visibilityState: string;
      hasFocus: () => boolean;
    };
    doc.hidden = false;
    doc.visibilityState = "visible";
    doc.hasFocus = () => hasFocus;
    const frame = Object.assign(new EventTarget(), {
      setTimeout: (handler: TimerHandler, ms?: number) => globalThis.setTimeout(handler, ms),
      clearTimeout: (handle: ReturnType<typeof setTimeout>) => globalThis.clearTimeout(handle),
    });
    vi.stubGlobal("document", doc);
    vi.stubGlobal("window", frame);
    return {
      frame,
      setFocused(value: boolean) { hasFocus = value; },
    };
  }

  it("drops the timer on window blur and catches up once on focus", () => {
    const env = installBrowser(true);
    let ticks = 0;
    const dispose = createAdaptiveTimer(() => { ticks += 1; }, 1_000);
    expect(vi.getTimerCount()).toBe(1);
    expect(ticks).toBe(0);
    env.frame.dispatchEvent(new Event("blur"));
    expect(vi.getTimerCount()).toBe(0);
    expect(ticks).toBe(0);
    env.frame.dispatchEvent(new Event("focus"));
    expect(ticks).toBe(1);
    expect(vi.getTimerCount()).toBe(1);
    env.frame.dispatchEvent(new Event("focus"));
    expect(ticks).toBe(1);
    expect(vi.getTimerCount()).toBe(1);
    dispose();
    expect(vi.getTimerCount()).toBe(0);
    env.frame.dispatchEvent(new Event("focus"));
    expect(ticks).toBe(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("does not arm while the webview reports no focus", () => {
    const env = installBrowser(false);
    const dispose = createAdaptiveTimer(() => {}, 1_000);
    expect(vi.getTimerCount()).toBe(0);
    env.setFocused(true);
    expect(vi.getTimerCount()).toBe(0);
    env.frame.dispatchEvent(new Event("focus"));
    expect(vi.getTimerCount()).toBe(1);
    dispose();
  });
});
