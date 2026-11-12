import { describe, expect, it } from "vitest";
import {
  createRafWriteQueue,
  FRAME_WRITE_BACKSTOP_MS,
  FRAME_WRITE_BUDGET_BYTES,
} from "./rafWriteQueue";

function rig(budgetBytes = FRAME_WRITE_BUDGET_BYTES) {
  let nextId = 1;
  const frames = new Map<number, () => void>();
  const timers = new Map<number, { ms: number; run: () => void }>();
  const writes: Array<{ bytes: Uint8Array; written: () => void }> = [];
  const errors: unknown[] = [];
  let shouldThrow: Error | null = null;

  const queue = createRafWriteQueue({
    budgetBytes,
    write(bytes, written) {
      if (shouldThrow) throw shouldThrow;
      writes.push({ bytes: bytes.slice(), written });
    },
    onError(error) {
      errors.push(error);
    },
    raf(cb) {
      const id = nextId++;
      frames.set(id, () => cb(0));
      return id;
    },
    caf(id) {
      frames.delete(id);
    },
    setTimer(run, ms) {
      const id = nextId++;
      timers.set(id, { ms, run });
      return id;
    },
    clearTimer(handle) {
      timers.delete(handle as number);
    },
  });

  return {
    queue,
    writes,
    errors,
    pendingFrames: () => frames.size,
    pendingTimers: () => timers.size,
    fireFrame() {
      for (const [id, run] of [...frames]) {
        frames.delete(id);
        run();
      }
    },
    fireTimers() {
      for (const [id, timer] of [...timers]) {
        timers.delete(id);
        timer.run();
      }
    },
    failNextWith(err: Error | null) {
      shouldThrow = err;
    },
  };
}

describe("rafWriteQueue", () => {
  it("coalesces multiple chunks inside one animation frame into a single write and settles every callback in order", () => {
    const r = rig();
    const order: string[] = [];
    r.queue.enqueue(Uint8Array.of(1, 2), () => order.push("a"));
    r.queue.enqueue(Uint8Array.of(3), () => order.push("b"));
    r.queue.enqueue(Uint8Array.of(4, 5, 6), () => order.push("c"));

    expect(r.writes.length).toBe(0);
    expect(r.pendingFrames()).toBe(1);
    expect(r.pendingTimers()).toBe(1);

    r.fireFrame();
    expect(r.writes.length).toBe(1);
    expect([...r.writes[0].bytes]).toEqual([1, 2, 3, 4, 5, 6]);
    expect(r.pendingTimers()).toBe(0);
    expect(order).toEqual([]);

    r.writes[0].written();
    expect(order).toEqual(["a", "b", "c"]);
  });

  it("passes a single chunk without copying its backing buffer", () => {
    let seen: Uint8Array | null = null;
    const frames: Array<() => void> = [];
    const queue = createRafWriteQueue({
      write(bytes, written) {
        seen = bytes;
        written();
      },
      raf(cb) {
        frames.push(() => cb(0));
        return frames.length;
      },
      caf() {},
      setTimer: () => 1,
      clearTimer() {},
    });
    const original = Uint8Array.of(9, 8, 7);
    let done = false;
    queue.enqueue(original, () => {
      done = true;
    });
    frames.shift()?.();
    expect(seen).toBe(original);
    expect(done).toBe(true);
  });

  it("caps one frame's merged write at budgetBytes on whole-chunk boundaries and schedules the next frame for the rest", () => {
    const r = rig(6);
    const settled: number[] = [];
    r.queue.enqueue(Uint8Array.of(1, 2, 3, 4), () => settled.push(1));
    r.queue.enqueue(Uint8Array.of(5, 6, 7, 8), () => settled.push(2));
    r.queue.enqueue(Uint8Array.of(9, 10), () => settled.push(3));
    r.queue.enqueue(Uint8Array.of(11), () => settled.push(4));

    // Frame 1: 4 bytes fits; adding chunk 2 (4 bytes -> 8 > 6) stops the batch.
    r.fireFrame();
    expect(r.writes.length).toBe(1);
    expect([...r.writes[0].bytes]).toEqual([1, 2, 3, 4]);
    r.writes[0].written();
    expect(settled).toEqual([1]);

    // Frame 2: chunk 2 (4 bytes) + chunk 3 (2 bytes) = 6 <= 6 fits; chunk 4 (7 > 6) waits.
    expect(r.pendingFrames()).toBe(1);
    r.fireFrame();
    expect(r.writes.length).toBe(2);
    expect([...r.writes[1].bytes]).toEqual([5, 6, 7, 8, 9, 10]);
    r.writes[1].written();
    expect(settled).toEqual([1, 2, 3]);

    // Frame 3: remaining chunk 4 drains cleanly.
    expect(r.pendingFrames()).toBe(1);
    r.fireFrame();
    expect(r.writes.length).toBe(3);
    expect([...r.writes[2].bytes]).toEqual([11]);
    r.writes[2].written();
    expect(settled).toEqual([1, 2, 3, 4]);
    expect(r.pendingFrames()).toBe(0);
  });

  it("flushes via the backstop timer when requestAnimationFrame does not fire in a hidden window", () => {
    const r = rig();
    let settled = false;
    r.queue.enqueue(Uint8Array.of(42), () => {
      settled = true;
    });
    expect(FRAME_WRITE_BACKSTOP_MS).toBeLessThanOrEqual(20);
    r.fireTimers();
    expect(r.pendingFrames()).toBe(0);
    expect(r.writes.length).toBe(1);
    r.writes[0].written();
    expect(settled).toBe(true);
  });

  it("flushSync drains every queued chunk immediately in order so reset/exit messages land behind prior output", () => {
    const r = rig(4);
    const settled: string[] = [];
    r.queue.enqueue(Uint8Array.of(1, 2, 3), () => settled.push("first"));
    r.queue.enqueue(Uint8Array.of(4, 5, 6), () => settled.push("second"));

    r.queue.flushSync();
    expect(r.pendingFrames()).toBe(0);
    expect(r.pendingTimers()).toBe(0);
    expect(r.writes.length).toBe(1);
    expect([...r.writes[0].bytes]).toEqual([1, 2, 3, 4, 5, 6]);
    r.writes[0].written();
    expect(settled).toEqual(["first", "second"]);
  });

  it("discard cancels timers and settles queued callbacks without writing", () => {
    const r = rig();
    const settled: string[] = [];
    r.queue.enqueue(Uint8Array.of(1, 2), () => settled.push("queued"));
    r.queue.discard();
    expect(r.writes.length).toBe(0);
    expect(r.pendingFrames()).toBe(0);
    expect(r.pendingTimers()).toBe(0);
    expect(settled).toEqual(["queued"]);

    r.queue.enqueue(Uint8Array.of(3), () => settled.push("after-discard"));
    expect(r.writes.length).toBe(0);
    expect(settled).toEqual(["queued", "after-discard"]);
  });

  it("settles callbacks and reports onError when the underlying write throws so output credit never leaks", () => {
    const r = rig();
    let released = false;
    r.failNextWith(new Error("xterm exploded"));
    r.queue.enqueue(Uint8Array.of(7, 8), () => {
      released = true;
    });
    r.fireFrame();
    expect(released).toBe(true);
    expect(r.errors.length).toBe(1);
  });
});
