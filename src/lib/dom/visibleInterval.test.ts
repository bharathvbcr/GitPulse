import { readFileSync } from "node:fs";
import { afterEach, describe, expect, it, vi } from "vitest";
import { createVisibleInterval, type IntervalHost } from "./visibleInterval";
import { resetForegroundFocus } from "../runtime/foreground";
import { LAG_HARD_MS, noteEventLoopDelay, resetEventLoopDelay } from "../runtime/loadCadence";

function fakeHost(startHidden = false) {
  let hidden = startHidden;
  const listeners = new Set<() => void>();
  const timers = new Map<number, { handler: () => void; ms: number }>();
  let nextId = 1;

  const host: IntervalHost = {
    setInterval(handler, ms) {
      const id = nextId++;
      timers.set(id, { handler, ms });
      return id;
    },
    clearInterval(handle) {
      timers.delete(handle as number);
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
    get live() {
      return timers.size;
    },
    get listenerCount() {
      return listeners.size;
    },
    fireAll() {
      for (const timer of [...timers.values()]) timer.handler();
    },
    periods() {
      return [...timers.values()].map((timer) => timer.ms);
    },
    setHidden(value: boolean) {
      hidden = value;
      for (const listener of [...listeners]) listener();
    },
    /** Snapshot of the registered listeners, for post-disposal replay. */
    capturedListeners() {
      return [...listeners];
    },
  };
}

describe("createVisibleInterval", () => {
  it("runs while the window is visible", () => {
    const env = fakeHost();
    let ticks = 0;
    createVisibleInterval(() => (ticks += 1), 1000, env.host);
    expect(env.live).toBe(1);
    env.fireAll();
    expect(ticks).toBe(1);
  });

  it("stops the timer entirely when the window is hidden", () => {
    // Merely declining the WORK inside a tick still wakes the renderer every
    // period; the timer itself has to go.
    const env = fakeHost();
    createVisibleInterval(() => {}, 1000, env.host);
    env.setHidden(true);
    expect(env.live).toBe(0);
  });

  it("never starts while the window is already hidden", () => {
    const env = fakeHost(true);
    createVisibleInterval(() => {}, 1000, env.host);
    expect(env.live).toBe(0);
  });

  it("catches up once on the way back rather than waiting a full period", () => {
    // Whatever the tick renders is stale by however long the window was
    // hidden, and the user is looking at it the moment it returns.
    const env = fakeHost();
    let ticks = 0;
    createVisibleInterval(() => (ticks += 1), 1000, env.host);
    env.setHidden(true);
    expect(ticks).toBe(0);
    env.setHidden(false);
    expect(ticks).toBe(1);
    expect(env.live).toBe(1);
  });

  it("does not stack timers across repeated visibility changes", () => {
    const env = fakeHost();
    createVisibleInterval(() => {}, 1000, env.host);
    for (let i = 0; i < 5; i += 1) {
      env.setHidden(true);
      env.setHidden(false);
    }
    expect(env.live).toBe(1);
  });

  it("removes both the timer and the listener on dispose", () => {
    const env = fakeHost();
    const dispose = createVisibleInterval(() => {}, 1000, env.host);
    expect(env.listenerCount).toBe(1);
    dispose();
    expect(env.live).toBe(0);
    expect(env.listenerCount).toBe(0);
  });

  it("creates no timer when a tick disposes the interval from inside itself", () => {
    // The rejoin path runs `tick()` and then `start()`. A tick that disposes
    // its own interval — a poll loop deciding it is finished does exactly
    // that — used to get a fresh timer created immediately afterwards,
    // holding a handle the disposer had already forgotten. Nothing could stop
    // it after that, so it fired until the page went away.
    const env = fakeHost();
    const box: { dispose: (() => void) | null } = { dispose: null };
    let ticks = 0;
    box.dispose = createVisibleInterval(
      () => {
        ticks += 1;
        box.dispose?.();
      },
      1000,
      env.host,
    );
    expect(env.live).toBe(1);

    // Hide and rejoin: the rejoin ticks, the tick disposes.
    env.setHidden(true);
    env.setHidden(false);
    expect(ticks).toBe(1);
    expect(env.live, "a disposed interval must not be resurrected by its own tick").toBe(0);
    expect(env.listenerCount).toBe(0);

    // And nothing may bring it back afterwards.
    env.setHidden(false);
    expect(env.live).toBe(0);
  });

  it("ignores a visibility event that arrives after disposal", () => {
    // A listener removal that races a dispatched event is ordinary in a real
    // DOM; the callback must not restart a timer the owner has released.
    const env = fakeHost();
    const dispose = createVisibleInterval(() => {}, 1000, env.host);
    const listeners = env.capturedListeners();
    dispose();
    expect(env.live).toBe(0);
    for (const listener of listeners) listener();
    expect(env.live, "a post-disposal event must not start a timer").toBe(0);
  });

  it("is a no-op with no host rather than throwing", () => {
    // Components call this unconditionally; a DOM-free context must not crash.
    expect(() => createVisibleInterval(() => {}, 1000, null)()).not.toThrow();
  });

  it("refuses a non-positive period instead of spinning", () => {
    const env = fakeHost();
    createVisibleInterval(() => {}, 0, env.host);
    expect(env.live).toBe(0);
  });

  it("lengthens the interval while the event loop is late and shortens it again when the loop recovers", () => {
    noteEventLoopDelay(LAG_HARD_MS);
    const env = fakeHost();
    createVisibleInterval(() => {}, 1000, env.host);
    expect(env.periods()).toEqual([4000]);
    resetEventLoopDelay();
    env.fireAll();
    expect(env.periods()).toEqual([1000]);
    resetEventLoopDelay();
  });
});

describe("every recurring UI timer is visibility-aware", () => {
  const read = (path: string) =>
    readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

  it.each([
    ["components/files/LivePulseDashboard.svelte"],
    ["components/ManviOpsPanel.svelte"],
    ["components/ManviHarnessPane.svelte"],
    // The delivery poll's timeline ticker fires every second while a run is in
    // flight, and its `live` state outlives the window being hidden.
    ["components/GitHubPanel.svelte"],
  ])("%s schedules through createVisibleInterval", (path) => {
    const source = read(path);
    expect(source).toContain("createVisibleInterval");
    // A bare window.setInterval here is the shape this replaced.
    expect(source).not.toMatch(/window\.setInterval\(/);
    expect(source).not.toMatch(/[^.\w]setInterval\(\(\) =>/);
  });
});

describe("browser interval host", () => {
  afterEach(() => {
    resetEventLoopDelay();
    resetForegroundFocus();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it("drops the interval on window blur and catches up once on focus", () => {
    vi.useFakeTimers();
    const doc = new EventTarget() as EventTarget & {
      hidden: boolean;
      visibilityState: string;
      hasFocus: () => boolean;
    };
    doc.hidden = false;
    doc.visibilityState = "visible";
    doc.hasFocus = () => true;
    const frame = Object.assign(new EventTarget(), {
      setInterval: (handler: TimerHandler, ms?: number) => globalThis.setInterval(handler, ms),
      clearInterval: (handle: ReturnType<typeof setInterval>) => globalThis.clearInterval(handle),
    });
    vi.stubGlobal("document", doc);
    vi.stubGlobal("window", frame);
    let ticks = 0;
    const dispose = createVisibleInterval(() => { ticks += 1; }, 1_000);
    expect(vi.getTimerCount()).toBe(1);
    frame.dispatchEvent(new Event("blur"));
    expect(vi.getTimerCount()).toBe(0);
    expect(ticks).toBe(0);
    frame.dispatchEvent(new Event("focus"));
    expect(ticks).toBe(1);
    expect(vi.getTimerCount()).toBe(1);
    frame.dispatchEvent(new Event("focus"));
    expect(ticks).toBe(1);
    dispose();
    expect(vi.getTimerCount()).toBe(0);
  });
});
