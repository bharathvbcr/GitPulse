import { describe, expect, it } from "vitest";
import { createGpuSlots, type GpuSlots } from "./gpuRenderer";

/**
 * A session that holds a slot. Hidden, it offers the slot through `idle`; the
 * pool's release gives it back, as the renderer's own release does.
 */
function holder(slots: GpuSlots, name: string, log: string[]) {
  expect(slots.take(), `${name} takes a slot`).toBe(true);
  let withdraw: (() => void) | null = null;
  return {
    hide() {
      withdraw = slots.idle(() => {
        log.push(name);
        slots.give();
      });
    },
    show() {
      withdraw?.();
      withdraw = null;
    },
  };
}

/** A pool the way the app builds it: a small budget for hidden contexts, the window's maximum on screen. */
const compact = () => createGpuSlots(2, 8);

describe("the cap budgets hidden contexts, not the terminals on screen", () => {
  it("reports its cap, floors it, keeps at least one, and ignores a value that is not a number", () => {
    const slots = createGpuSlots(3, 8);
    expect(slots.cap()).toBe(3);
    slots.setCap(5);
    expect(slots.cap()).toBe(5);
    slots.setCap(Number.NaN);
    slots.setCap(Number.POSITIVE_INFINITY);
    expect(slots.cap()).toBe(5);
    slots.setCap(0);
    expect(slots.cap()).toBe(1);
    slots.setCap(-4);
    expect(slots.cap()).toBe(1);
    slots.setCap(2.9);
    expect(slots.cap()).toBe(2);
  });

  it("gives every on-screen terminal a context on the compact budget, and holds no hidden one", () => {
    const slots = compact();
    const log: string[] = [];
    const a = holder(slots, "a", log);
    holder(slots, "b", log);
    holder(slots, "c", log);
    expect(slots.held()).toBe(3);
    // The third pane leaves the screen: the pool is over its budget of two, so it goes at once.
    a.hide();
    expect(log).toEqual(["a"]);
    expect(slots.held()).toBe(2);
  });

  it("holds one hidden context beside one visible pane for the grace, and a new on-screen terminal reclaims the longest-hidden", () => {
    const slots = compact();
    const log: string[] = [];
    const a = holder(slots, "a", log);
    const b = holder(slots, "b", log);
    b.hide();
    expect(log, "one visible plus one hidden is inside the budget of two").toEqual([]);
    expect(slots.held()).toBe(2);
    // a is hidden too; both hidden is still two.
    a.hide();
    expect(log).toEqual([]);
    // A third terminal comes on screen: two hidden plus one visible is over budget, so the longest-hidden goes.
    holder(slots, "c", log);
    expect(log).toEqual(["b"]);
    expect(slots.held()).toBe(2);
  });

  it("a terminal hidden while the pool is over budget releases at once instead of waiting out its grace", () => {
    const slots = createGpuSlots(1, 8);
    const log: string[] = [];
    const a = holder(slots, "a", log);
    const b = holder(slots, "b", log);
    holder(slots, "c", log);
    expect(slots.held()).toBe(3);
    a.hide();
    expect(log).toEqual(["a"]);
    b.hide();
    // One pane is still on screen and the budget is one: no room for a hidden holder beside it.
    expect(log).toEqual(["a", "b"]);
    expect(slots.held()).toBe(1);
  });

  it("refuses an on-screen terminal only past the window's maximum, and wakes it when one leaves", () => {
    const slots = compact();
    const log: string[] = [];
    const panes = Array.from({ length: 8 }, (_, n) => holder(slots, `p${n}`, log));
    expect(slots.take()).toBe(false);
    let retried = 0;
    slots.wait(() => {
      retried += 1;
      expect(slots.take()).toBe(true);
    });
    // A pane hiding is not a pane gone: its context is still held, but it is no longer on screen.
    panes[0].hide();
    expect(retried).toBe(1);
    // The waiting pane took its slot by taking over the hidden one.
    expect(log).toEqual(["p0"]);
    expect(slots.held()).toBe(8);
  });

  it("taking a slot for an on-screen terminal reclaims the longest-hidden holder when the budget is spent", () => {
    const slots = compact();
    const log: string[] = [];
    const a = holder(slots, "a", log);
    const b = holder(slots, "b", log);
    a.hide();
    b.hide();
    holder(slots, "c", log);
    expect(log).toEqual(["a"]);
    expect(slots.held()).toBe(2);
  });
});

describe("lowering the cap while the app runs", () => {
  it("releases hidden holders beyond the budget, longest-hidden first, and never an on-screen one", () => {
    const slots = createGpuSlots(4, 8);
    const log: string[] = [];
    const [a, b, c] = ["a", "b", "c"].map((n) => holder(slots, n, log));
    holder(slots, "d", log);
    a.hide();
    b.hide();
    c.hide();
    expect(slots.held()).toBe(4);
    slots.setCap(2);
    // d is on screen; the budget of two holds d and one hidden holder, the most recently hidden.
    expect(log).toEqual(["a", "b"]);
    expect(slots.held()).toBe(2);
  });

  it("leaves on-screen terminals alone even when they alone exceed the new cap", () => {
    const slots = createGpuSlots(4, 8);
    const log: string[] = [];
    holder(slots, "a", log);
    holder(slots, "b", log);
    holder(slots, "c", log);
    const d = holder(slots, "d", log);
    d.hide();
    slots.setCap(1);
    expect(log).toEqual(["d"]);
    expect(slots.held()).toBe(3);
    // Three on screen: a fourth is still admitted, up to the window's maximum.
    expect(slots.take()).toBe(true);
  });

  it("a cap that shrinks while a session is attaching never frees the context it just claimed", () => {
    const slots = createGpuSlots(2, 8);
    const log: string[] = [];
    holder(slots, "hidden", log).hide();
    // The attaching session has taken its slot but has not offered it yet.
    expect(slots.take()).toBe(true);
    slots.setCap(1);
    expect(log).toEqual(["hidden"]);
    expect(slots.held()).toBe(1);
  });

  it("a larger cap keeps more hidden holders from then on", () => {
    const slots = createGpuSlots(1, 8);
    const log: string[] = [];
    const a = holder(slots, "a", log);
    a.hide();
    slots.setCap(3);
    holder(slots, "b", log).hide();
    holder(slots, "c", log).hide();
    expect(log).toEqual([]);
    expect(slots.held()).toBe(3);
  });

  it("is idempotent when the same cap arrives twice", () => {
    const slots = createGpuSlots(3, 8);
    const log: string[] = [];
    holder(slots, "a", log).hide();
    holder(slots, "b", log).hide();
    slots.setCap(3);
    slots.setCap(3);
    expect(log).toEqual([]);
    expect(slots.held()).toBe(2);
  });
});

describe("a pool built with one number keeps that number as both budget and maximum", () => {
  it("refuses beyond it, as before", () => {
    const slots = createGpuSlots(2);
    expect(slots.take()).toBe(true);
    expect(slots.take()).toBe(true);
    expect(slots.take()).toBe(false);
  });
});
