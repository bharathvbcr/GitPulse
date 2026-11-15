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

describe("the GPU slot pool's cap can change", () => {
  it("reports its cap", () => {
    const slots = createGpuSlots(3);
    expect(slots.cap()).toBe(3);
    slots.setCap(5);
    expect(slots.cap()).toBe(5);
  });

  it("releases hidden holders beyond a smaller cap, longest-hidden first", () => {
    const slots = createGpuSlots(4);
    const log: string[] = [];
    const [a, b, c, d] = ["a", "b", "c", "d"].map((n) => holder(slots, n, log));
    // Hidden in this order: c, a, d; b stays on screen.
    c.hide();
    a.hide();
    d.hide();
    slots.setCap(2);
    expect(log).toEqual(["c", "a"]);
    expect(slots.held()).toBe(2);
    b.show();
  });

  it("never releases an on-screen session, even when that leaves the pool over its cap", () => {
    const slots = createGpuSlots(3);
    const log: string[] = [];
    holder(slots, "a", log);
    holder(slots, "b", log);
    const c = holder(slots, "c", log);
    c.hide();
    slots.setCap(1);
    expect(log).toEqual(["c"]);
    expect(slots.held()).toBe(2);
    // Over the cap: nobody new gets a context until the holders leave.
    expect(slots.take()).toBe(false);
    slots.give();
    expect(slots.take()).toBe(false);
    slots.give();
    expect(slots.take()).toBe(true);
  });

  it("a larger cap takes effect at once and wakes a refused session", () => {
    const slots = createGpuSlots(1);
    const log: string[] = [];
    holder(slots, "a", log);
    expect(slots.take()).toBe(false);
    let retried = 0;
    slots.wait(() => {
      retried += 1;
      expect(slots.take()).toBe(true);
    });
    slots.setCap(2);
    expect(retried).toBe(1);
    expect(slots.held()).toBe(2);
  });

  it("is idempotent when the same cap arrives twice", () => {
    const slots = createGpuSlots(3);
    const log: string[] = [];
    holder(slots, "a", log).hide();
    holder(slots, "b", log).hide();
    slots.setCap(3);
    slots.setCap(3);
    expect(log).toEqual([]);
    expect(slots.held()).toBe(2);
  });

  it("a cap that shrinks while a session is attaching never frees the context it just claimed", () => {
    const slots = createGpuSlots(2);
    const log: string[] = [];
    const hidden = holder(slots, "hidden", log);
    hidden.hide();
    // The attaching session has taken its slot but is not idle yet.
    expect(slots.take()).toBe(true);
    slots.setCap(1);
    expect(log).toEqual(["hidden"]);
    expect(slots.held()).toBe(1);
    // When it attaches and is later hidden, a further shrink does release it.
    const release: string[] = [];
    slots.idle(() => {
      release.push("attached");
      slots.give();
    });
    slots.setCap(1);
    expect(release).toEqual([]);
    slots.setCap(1);
    expect(slots.held()).toBe(1);
  });

  it("ignores a cap that is not a number and never goes below one", () => {
    const slots = createGpuSlots(3);
    slots.setCap(Number.NaN);
    expect(slots.cap()).toBe(3);
    slots.setCap(Number.POSITIVE_INFINITY);
    expect(slots.cap()).toBe(3);
    slots.setCap(0);
    expect(slots.cap()).toBe(1);
    slots.setCap(-4);
    expect(slots.cap()).toBe(1);
    slots.setCap(2.9);
    expect(slots.cap()).toBe(2);
  });
});
