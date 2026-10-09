import { afterEach, describe, expect, it, vi } from "vitest";
import { createDeferredWrite, flushOnPageHide } from "./deferredWrite";

afterEach(() => {
  vi.useRealTimers();
});

function listenerTarget<T extends object>(extra: T) {
  const listeners = new Map<string, Set<() => void>>();
  return {
    ...extra,
    addEventListener: (type: string, listener: () => void) => {
      if (!listeners.has(type)) listeners.set(type, new Set());
      listeners.get(type)!.add(listener);
    },
    removeEventListener: (type: string, listener: () => void) => {
      listeners.get(type)?.delete(listener);
    },
    emit: (type: string) => {
      for (const listener of listeners.get(type) ?? []) listener();
    },
    count: (type: string) => listeners.get(type)?.size ?? 0,
  };
}

describe("createDeferredWrite", () => {
  it("shares one write between every request in a window", () => {
    vi.useFakeTimers();
    const write = vi.fn();
    const pending = createDeferredWrite(write, 100);
    for (let i = 0; i < 1_000; i++) pending.schedule();
    expect(write).not.toHaveBeenCalled();
    expect(pending.pending).toBe(true);
    vi.advanceTimersByTime(100);
    expect(write).toHaveBeenCalledTimes(1);
    expect(pending.pending).toBe(false);
  });

  it("does not let a steady stream of requests postpone the write", () => {
    // A trailing debounce would wait for the stream to stop; this writes
    // within one window of the first request however busy the caller is.
    vi.useFakeTimers();
    const write = vi.fn();
    const pending = createDeferredWrite(write, 100);
    for (let t = 0; t < 1_000; t += 10) {
      pending.schedule();
      vi.advanceTimersByTime(10);
    }
    expect(write).toHaveBeenCalledTimes(10);
  });

  it("flushes a pending write at once and only once", () => {
    vi.useFakeTimers();
    const write = vi.fn();
    const pending = createDeferredWrite(write, 100);
    pending.flush();
    expect(write).not.toHaveBeenCalled();
    pending.schedule();
    pending.flush();
    expect(write).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(1_000);
    expect(write).toHaveBeenCalledTimes(1);
  });

  it("drops a cancelled write", () => {
    vi.useFakeTimers();
    const write = vi.fn();
    const pending = createDeferredWrite(write, 100);
    pending.schedule();
    pending.cancel();
    vi.advanceTimersByTime(1_000);
    pending.flush();
    expect(write).not.toHaveBeenCalled();
  });

  it("refuses a delay that would spin or never fire", () => {
    for (const bad of [0, -1, Number.NaN, Number.POSITIVE_INFINITY]) {
      expect(() => createDeferredWrite(() => {}, bad)).toThrow(RangeError);
    }
  });
});

describe("flushOnPageHide", () => {
  it("flushes on pagehide and when the document becomes hidden, not when it shows", () => {
    const win = listenerTarget({});
    const doc = listenerTarget({ visibilityState: "visible" as DocumentVisibilityState });
    const flush = vi.fn();
    const unbind = flushOnPageHide(flush, win, doc);
    doc.emit("visibilitychange");
    expect(flush).not.toHaveBeenCalled();
    doc.visibilityState = "hidden";
    doc.emit("visibilitychange");
    win.emit("pagehide");
    expect(flush).toHaveBeenCalledTimes(2);
    unbind();
    expect(win.count("pagehide")).toBe(0);
    expect(doc.count("visibilitychange")).toBe(0);
  });

  it("wires nothing on a window stub that has no event methods", () => {
    // Store tests stub `window` as `{ localStorage }`.
    expect(() => flushOnPageHide(() => {}, { localStorage: {} }, null)()).not.toThrow();
  });
});
