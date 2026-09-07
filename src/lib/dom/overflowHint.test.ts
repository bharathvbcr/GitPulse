import { describe, expect, it, vi } from "vitest";
import {
  EMPTY_OVERFLOW_HINT,
  overflowNudge,
  readOverflowHint,
  resolveOverflowHint,
  scrollOverflowBy,
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
