import { describe, expect, it, vi } from "vitest";
import { createListenerTracker } from "./listenerTracker";

describe("createListenerTracker", () => {
  it("starts empty and undisposed", () => {
    const tracker = createListenerTracker();
    expect(tracker.size).toBe(0);
    expect(tracker.disposed).toBe(false);
  });

  it("accounts size as fns are tracked and unwound", () => {
    const tracker = createListenerTracker();
    tracker.track(vi.fn());
    tracker.track(vi.fn());
    expect(tracker.size).toBe(2);
    tracker.dispose();
    expect(tracker.size).toBe(0);
  });

  it("dispose unwinds tracked fns LIFO", () => {
    // Reverse-registration order matters for layered listeners: the newest
    // subscription may sit on top of state an older one owns.
    const tracker = createListenerTracker();
    const order: string[] = [];
    tracker.track(() => order.push("first"));
    tracker.track(() => order.push("second"));
    tracker.track(() => order.push("third"));
    tracker.dispose();
    expect(order).toEqual(["third", "second", "first"]);
    expect(tracker.disposed).toBe(true);
  });

  it("THE REGRESSION: a registration landing after dispose self-unregisters", () => {
    // Teardown ran while listen() promises were still pending; when they
    // resolve their unlisten fns must fire immediately instead of landing
    // in a dead array and leaking for the webview lifetime.
    const tracker = createListenerTracker();
    tracker.dispose();
    const late = vi.fn();
    tracker.track(late);
    expect(late).toHaveBeenCalledTimes(1);
    expect(tracker.size).toBe(0);
  });

  it("a late throwing unlisten is swallowed instead of escaping track()", () => {
    const tracker = createListenerTracker();
    tracker.dispose();
    expect(() => tracker.track(() => { throw new Error("dead"); })).not.toThrow();
  });

  it("double dispose is safe and does not re-run fns", () => {
    const tracker = createListenerTracker();
    const fn = vi.fn();
    tracker.track(fn);
    tracker.dispose();
    expect(fn).toHaveBeenCalledTimes(1);
    expect(() => tracker.dispose()).not.toThrow();
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it("one throwing unlisten does not strand the rest of the unwind", () => {
    const tracker = createListenerTracker();
    const order: string[] = [];
    tracker.track(() => order.push("oldest"));
    tracker.track(() => { throw new Error("already gone"); });
    tracker.track(() => order.push("newest"));
    expect(() => tracker.dispose()).not.toThrow();
    expect(order).toEqual(["newest", "oldest"]);
  });
});
