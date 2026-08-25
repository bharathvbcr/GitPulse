import { describe, it, expect, vi } from "vitest";
import { createFrameScheduler } from "../frameScheduler";

describe("createFrameScheduler", () => {
  it("coalesces multiple schedule calls into one frame and runs the latest job", () => {
    const queued: FrameRequestCallback[] = [];
    const raf = vi.fn((cb: FrameRequestCallback) => {
      queued.push(cb);
      return queued.length;
    });
    const caf = vi.fn();
    const scheduler = createFrameScheduler(raf, caf);
    const first = vi.fn();
    const second = vi.fn();

    scheduler.schedule(first);
    scheduler.schedule(second);
    expect(raf).toHaveBeenCalledTimes(1);
    expect(scheduler.isScheduled()).toBe(true);

    queued[0](16);
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);
    expect(scheduler.isScheduled()).toBe(false);
  });

  it("cancel drops the pending job even if the fake raf still fires", () => {
    const queued: FrameRequestCallback[] = [];
    const raf = (cb: FrameRequestCallback) => {
      queued.push(cb);
      return 7;
    };
    const caf = vi.fn();
    const scheduler = createFrameScheduler(raf, caf);
    const job = vi.fn();
    scheduler.schedule(job);
    scheduler.cancel();

    expect(caf).toHaveBeenCalledWith(7);
    queued[0](32);
    expect(job).not.toHaveBeenCalled();
    expect(scheduler.isScheduled()).toBe(false);
  });

  /**
   * Pins CommitTable's animation-loop contract: the only RAF driver in the
   * app must rearm strictly while stepGraphPaint reports `animating`, so the
   * loop goes fully idle (no armed callback, no rAF calls) once settled — and
   * still wakes on the next event-driven schedule.
   */
  it("goes idle when the frame job stops rescheduling, and wakes on demand", () => {
    let armed = 0;
    const queued: FrameRequestCallback[] = [];
    const raf = vi.fn((cb: FrameRequestCallback) => {
      queued.push(cb);
      return ++armed;
    });
    const caf = vi.fn();
    const scheduler = createFrameScheduler(raf, caf);

    let animating = true;
    const onFrame = () => {
      if (animating) scheduler.schedule(onFrame);
    };

    scheduler.schedule(onFrame);
    expect(raf).toHaveBeenCalledTimes(1);

    queued.shift()!(16); // frame 1: animating → rearms
    expect(scheduler.isScheduled()).toBe(true);
    expect(raf).toHaveBeenCalledTimes(2);

    animating = false;
    queued.shift()!(32); // frame 2: settled → no rearm
    expect(scheduler.isScheduled()).toBe(false);
    const framesAfterSettle = armed;
    expect(queued).toHaveLength(0);

    // Time passes with nothing scheduled; no further frames are requested.
    expect(armed).toBe(framesAfterSettle);
    expect(raf).toHaveBeenCalledTimes(framesAfterSettle);

    // The next event-driven wake arms exactly one more frame.
    scheduler.schedule(onFrame);
    expect(scheduler.isScheduled()).toBe(true);
    expect(raf).toHaveBeenCalledTimes(framesAfterSettle + 1);
  });
});
