import { describe, expect, it } from "vitest";
import { createFitScheduler, FIT_MIN_INTERVAL_MS } from "./fitSchedule";

/** A clock, timers and animation frames that only move when the test says so. */
function rig(minIntervalMs?: number) {
  let time = 1000;
  let nextHandle = 1;
  const timers = new Map<number, { at: number; run: () => void }>();
  const frames = new Map<number, () => void>();
  let runs = 0;
  const scheduler = createFitScheduler(() => { runs++; }, {
    minIntervalMs,
    now: () => time,
    setTimer: (run, ms) => { const handle = nextHandle++; timers.set(handle, { at: time + ms, run }); return handle; },
    clearTimer: (handle) => { timers.delete(handle as number); },
    raf: (callback) => { const handle = nextHandle++; frames.set(handle, () => callback(time)); return handle; },
    caf: (handle) => { frames.delete(handle); },
  });
  const advance = (ms: number) => {
    time += ms;
    for (const [handle, timer] of [...timers]) {
      if (timer.at <= time) { timers.delete(handle); timer.run(); }
    }
  };
  const paint = () => {
    for (const [handle, run] of [...frames]) { frames.delete(handle); run(); }
  };
  return { scheduler, advance, paint, runs: () => runs, pendingTimers: () => timers.size, pendingFrames: () => frames.size };
}

describe("fit scheduler: layout requests", () => {
  it("fits at once when nothing has fitted lately", () => {
    const r = rig();
    r.scheduler.request();
    expect(r.runs()).toBe(1);
  });

  it("coalesces a burst inside the interval into one trailing fit", () => {
    const r = rig();
    r.scheduler.request();
    for (let i = 0; i < 5; i++) { r.advance(4); r.scheduler.request(); }
    expect(r.runs()).toBe(1);
    expect(r.pendingTimers()).toBe(1);
    r.advance(FIT_MIN_INTERVAL_MS);
    expect(r.runs()).toBe(2);
    expect(r.pendingTimers()).toBe(0);
  });

  it("never fits more often than the interval during a sustained drag", () => {
    const r = rig();
    // 60 frames of 16 ms, a request on each.
    for (let frame = 0; frame < 60; frame++) { r.scheduler.request(); r.advance(16); }
    r.advance(FIT_MIN_INTERVAL_MS);
    const elapsed = 60 * 16;
    expect(r.runs()).toBeLessThanOrEqual(Math.ceil(elapsed / FIT_MIN_INTERVAL_MS) + 1);
    expect(r.runs()).toBeGreaterThanOrEqual(Math.floor(elapsed / (2 * FIT_MIN_INTERVAL_MS)));
  });

  it("always fits once more after the last request of a burst", () => {
    const r = rig();
    r.scheduler.request();
    r.advance(10);
    r.scheduler.request();
    expect(r.runs()).toBe(1);
    r.advance(FIT_MIN_INTERVAL_MS);
    expect(r.runs()).toBe(2);
    r.advance(1000);
    expect(r.runs()).toBe(2);
  });

  it("fits at once again after a quiet gap", () => {
    const r = rig();
    r.scheduler.request();
    r.advance(500);
    r.scheduler.request();
    expect(r.runs()).toBe(2);
  });
});

describe("fit scheduler: settling on show", () => {
  it("fits on the next animation frame, not in the caller's tick", () => {
    const r = rig();
    r.scheduler.settle();
    expect(r.runs()).toBe(0);
    r.paint();
    expect(r.runs()).toBe(1);
  });

  it("is one fit however many things settle in the same frame", () => {
    const r = rig();
    r.scheduler.settle();
    r.scheduler.settle();
    r.scheduler.settle();
    r.paint();
    expect(r.runs()).toBe(1);
  });

  it("absorbs a throttled run that was waiting", () => {
    const r = rig();
    r.scheduler.request();
    r.advance(5);
    r.scheduler.request();
    expect(r.pendingTimers()).toBe(1);
    r.scheduler.settle();
    expect(r.pendingTimers()).toBe(0);
    r.paint();
    expect(r.runs()).toBe(2);
    r.advance(1000);
    expect(r.runs()).toBe(2);
  });

  it("runs `after` once, after the fit", () => {
    const order: string[] = [];
    let time = 0;
    const frames: Array<() => void> = [];
    const scheduler = createFitScheduler(() => order.push("fit"), {
      now: () => time,
      raf: (callback) => { frames.push(() => callback(time)); return frames.length; },
      caf: () => {},
    });
    const after = () => order.push("after");
    scheduler.settle(after);
    scheduler.settle(after);
    time += 1;
    for (const frame of frames.splice(0)) frame();
    expect(order).toEqual(["fit", "after"]);
  });
});

describe("fit scheduler: teardown", () => {
  it("cancels a pending trailing fit and a pending frame", () => {
    const r = rig();
    r.scheduler.request();
    r.advance(5);
    r.scheduler.request();
    r.scheduler.settle(() => { throw new Error("after must not run"); });
    r.scheduler.cancel();
    expect(r.pendingTimers()).toBe(0);
    expect(r.pendingFrames()).toBe(0);
    r.advance(1000);
    r.paint();
    expect(r.runs()).toBe(1);
  });
});
