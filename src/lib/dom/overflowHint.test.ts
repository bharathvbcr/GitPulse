import { describe, expect, it, vi } from "vitest";
import {
  EMPTY_OVERFLOW_HINT,
  applyHorizontalScrollDelta,
  overflowNudge,
  readOverflowHint,
  resolveOverflowHint,
  scrollChildIntoHorizontalView,
  scrollOverflowBy,
  verticalWheelToHorizontalDelta,
  type OverflowBox,
  type OverflowScroller,
} from "./overflowHint";

describe("resolveOverflowHint", () => {
  it("advertises only the side that still has hidden content", () => {
    expect(resolveOverflowHint(0, 440, 1_200)).toEqual({
      canScroll: true,
      showStart: false,
      showEnd: true,
    });
    expect(resolveOverflowHint(400, 440, 1_200)).toEqual({
      canScroll: true,
      showStart: true,
      showEnd: true,
    });
    expect(resolveOverflowHint(760, 440, 1_200)).toEqual({
      canScroll: true,
      showStart: true,
      showEnd: false,
    });
    expect(resolveOverflowHint(0, 440, 440)).toEqual(EMPTY_OVERFLOW_HINT);
  });

  it("still shows an end cue for a sub-2px overflow that canScroll already admits", () => {
    // max = 0.6 is above the 0.5 eps, but a 1px edge threshold would suppress
    // both cues (0 < 0.6-1 is false). The threshold shrinks so the overflow
    // is not silent.
    expect(resolveOverflowHint(0, 100, 100.6)).toEqual({
      canScroll: true,
      showStart: false,
      showEnd: true,
    });
  });

  it("fails closed on non-finite measurements", () => {
    expect(resolveOverflowHint(Number.NaN, 440, 1_200)).toMatchObject({
      canScroll: true,
      showStart: false,
    });
    // A NaN viewport degrades to 0, so content still overflows rather than
    // leaking NaN into the cue. Infinite content is not a number we can pan.
    expect(resolveOverflowHint(0, Number.NaN, 1_200)).toMatchObject({
      canScroll: true,
      showStart: false,
      showEnd: true,
    });
    expect(resolveOverflowHint(0, 440, Number.POSITIVE_INFINITY)).toEqual(
      EMPTY_OVERFLOW_HINT,
    );
    expect(resolveOverflowHint(-12, 440, 1_200).showStart).toBe(false);
  });
});

describe("readOverflowHint", () => {
  const box = (partial: Partial<OverflowBox>): OverflowBox => ({
    scrollLeft: 0,
    scrollTop: 0,
    clientWidth: 100,
    clientHeight: 80,
    scrollWidth: 100,
    scrollHeight: 80,
    ...partial,
  });

  it("reads the requested axis", () => {
    const el = box({
      scrollLeft: 20,
      clientWidth: 100,
      scrollWidth: 300,
      scrollTop: 0,
      clientHeight: 80,
      scrollHeight: 400,
    });
    expect(readOverflowHint(el, "x")).toEqual({
      canScroll: true,
      showStart: true,
      showEnd: true,
    });
    expect(readOverflowHint(el, "y")).toEqual({
      canScroll: true,
      showStart: false,
      showEnd: true,
    });
  });
});

describe("overflowNudge / scrollOverflowBy", () => {
  it("pans by 80% of the viewport, never less than 48px", () => {
    expect(overflowNudge(200, "end")).toBe(160);
    expect(overflowNudge(200, "start")).toBe(-160);
    expect(overflowNudge(40, "end")).toBe(48);
    expect(overflowNudge(Number.NaN, "end")).toBe(48);
  });

  it("scrolls only the requested axis", () => {
    const scrollBy = vi.fn();
    const el: OverflowScroller = {
      scrollLeft: 0,
      scrollTop: 0,
      clientWidth: 100,
      clientHeight: 80,
      scrollWidth: 400,
      scrollHeight: 400,
      scrollBy,
    };
    scrollOverflowBy(el, "x", "end", { behavior: "smooth" });
    expect(scrollBy).toHaveBeenCalledWith({
      left: 80,
      top: 0,
      behavior: "smooth",
    });
    scrollBy.mockClear();
    scrollOverflowBy(el, "y", "start");
    expect(scrollBy).toHaveBeenCalledWith({
      left: 0,
      top: -64,
      behavior: "auto",
    });
  });
});

describe("verticalWheelToHorizontalDelta", () => {
  it("maps a dominant vertical wheel onto X and leaves a horizontal one alone", () => {
    expect(verticalWheelToHorizontalDelta({ deltaX: 0, deltaY: 40 })).toBe(40);
    expect(verticalWheelToHorizontalDelta({ deltaX: 0, deltaY: -12 })).toBe(-12);
    expect(verticalWheelToHorizontalDelta({ deltaX: 30, deltaY: 10 })).toBeNull();
    expect(verticalWheelToHorizontalDelta({ deltaX: 8, deltaY: 8 })).toBeNull();
    expect(verticalWheelToHorizontalDelta({ deltaX: 0, deltaY: 0 })).toBeNull();
  });

  it("fails closed on non-finite deltas", () => {
    expect(verticalWheelToHorizontalDelta({ deltaX: Number.NaN, deltaY: 10 })).toBeNull();
    expect(verticalWheelToHorizontalDelta({ deltaX: 0, deltaY: Number.POSITIVE_INFINITY })).toBeNull();
  });
});

describe("applyHorizontalScrollDelta", () => {
  const scroller = (partial: Partial<{ scrollLeft: number; scrollWidth: number; clientWidth: number }>) => ({
    scrollLeft: 0,
    scrollWidth: 800,
    clientWidth: 200,
    ...partial,
  });

  it("clamps to the scrollable range", () => {
    expect(applyHorizontalScrollDelta(scroller({}), 50)).toBe(50);
    expect(applyHorizontalScrollDelta(scroller({ scrollLeft: 580 }), 50)).toBe(600);
    expect(applyHorizontalScrollDelta(scroller({ scrollLeft: 10 }), -50)).toBe(0);
  });

  it("does not move a strip that does not overflow", () => {
    expect(applyHorizontalScrollDelta(scroller({ scrollWidth: 200, clientWidth: 200 }), 80)).toBe(0);
  });

  it("fails closed on hostile measurements", () => {
    expect(applyHorizontalScrollDelta(scroller({ scrollLeft: Number.NaN }), 40)).toBe(40);
    expect(applyHorizontalScrollDelta(scroller({}), Number.NaN)).toBe(0);
    expect(applyHorizontalScrollDelta(scroller({ scrollWidth: Number.POSITIVE_INFINITY }), 40)).toBe(0);
  });
});

describe("scrollChildIntoHorizontalView", () => {
  it("pans just enough to reveal a child past either edge", () => {
    const strip = { scrollLeft: 0, clientWidth: 200, scrollWidth: 800 };
    expect(scrollChildIntoHorizontalView(strip, { offsetLeft: 0, offsetWidth: 80 })).toBe(0);
    expect(scrollChildIntoHorizontalView(strip, { offsetLeft: 250, offsetWidth: 80 })).toBe(130);
    expect(scrollChildIntoHorizontalView({ ...strip, scrollLeft: 400 }, { offsetLeft: 0, offsetWidth: 80 })).toBe(0);
  });

  it("does not overscroll a strip that is already showing the child", () => {
    const strip = { scrollLeft: 100, clientWidth: 200, scrollWidth: 800 };
    expect(scrollChildIntoHorizontalView(strip, { offsetLeft: 120, offsetWidth: 40 })).toBe(100);
  });

  it("fails closed on hostile geometry", () => {
    expect(
      scrollChildIntoHorizontalView(
        { scrollLeft: Number.NaN, clientWidth: 0, scrollWidth: 800 },
        { offsetLeft: 400, offsetWidth: 80 },
      ),
    ).toBe(0);
  });
});
