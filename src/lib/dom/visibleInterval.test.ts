import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { createVisibleInterval, type IntervalHost } from "./visibleInterval";

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
    setHidden(value: boolean) {
      hidden = value;
      for (const listener of [...listeners]) listener();
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

  it("is a no-op with no host rather than throwing", () => {
    // Components call this unconditionally; a DOM-free context must not crash.
    expect(() => createVisibleInterval(() => {}, 1000, null)()).not.toThrow();
  });
});

describe("every recurring UI timer is visibility-aware", () => {
  const read = (path: string) =>
    readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

  it.each([
    ["components/files/LivePulseDashboard.svelte"],
    ["components/ManviOpsPanel.svelte"],
    ["components/ManviHarnessPane.svelte"],
  ])("%s schedules through createVisibleInterval", (path) => {
    const source = read(path);
    expect(source).toContain("createVisibleInterval");
    // A bare window.setInterval here is the shape this replaced.
    expect(source).not.toMatch(/window\.setInterval\(/);
    expect(source).not.toMatch(/[^.\w]setInterval\(\(\) =>/);
  });
});
