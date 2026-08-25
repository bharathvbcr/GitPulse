import { describe, expect, it } from "vitest";
import { computeWindow } from "./virtualWindow";

describe("computeWindow", () => {
  it("returns an empty window for an empty list", () => {
    expect(computeWindow(0, 600, 0, 20, 20)).toEqual({ start: 0, end: 0 });
    // A scrolled position over nothing must stay empty, not negative.
    expect(computeWindow(9_999, 600, 0, 20, 20)).toEqual({ start: 0, end: 0 });
  });

  it("guards against degenerate row heights and NaN inputs", () => {
    expect(computeWindow(0, 600, 100, 0, 20)).toEqual({ start: 0, end: 0 });
    // A NaN scroll anchor fails closed: paint nothing rather than guess the
    // top band.
    expect(computeWindow(Number.NaN, 600, 100, 20, 20)).toEqual({ start: 0, end: 0 });
    // NaN viewport still renders the overscan band around the top.
    expect(computeWindow(0, Number.NaN, 100, 20, 20).end).toBeGreaterThan(0);
  });

  it("renders a tiny list whole regardless of overscan or scroll", () => {
    expect(computeWindow(0, 600, 5, 20, 20)).toEqual({ start: 0, end: 5 });
    expect(computeWindow(80, 600, 5, 20, 20)).toEqual({ start: 0, end: 5 });
    // Fewer rows than one viewport.
    expect(computeWindow(0, 1_000, 3, 20, 0)).toEqual({ start: 0, end: 3 });
  });

  it("clamps a scroll position beyond the end of the list", () => {
    // 100 rows × 20px = 2000px of content; scrollTop 5000 is past the end.
    const win = computeWindow(5_000, 600, 100, 20, 20);
    expect(win.start).toBeLessThanOrEqual(100);
    expect(win.end).toBe(100);
  });

  it("handles exact multiples of the viewport height", () => {
    // Viewport is exactly 30 rows; scrolling to row 30 (the second page)
    // starts exactly there with no fractional carry.
    const win = computeWindow(30 * 20, 30 * 20, 120, 20, 0);
    expect(win).toEqual({ start: 30, end: 60 });
  });

  it("applies overscan symmetrically around the visible band", () => {
    // Visible rows are [50, 60); with overscan 7 the window is [43, 67).
    const win = computeWindow(50 * 20, 10 * 20, 100, 20, 7);
    expect(win.start).toBe(43);
    // 50 + 10 + 7; a naive 50 + 10 + 14 expectation double-counts the
    // bottom overscan that the band already includes once.
    expect(win.end).toBe(67);
    const topEdge = computeWindow(0, 10 * 20, 100, 20, 7);
    expect(topEdge.start).toBe(0);
    expect(topEdge.end).toBe(17);
    const bottomEdge = computeWindow(95 * 20, 10 * 20, 100, 20, 7);
    expect(bottomEdge.start).toBe(88);
    expect(bottomEdge.end).toBe(100);
  });

  it("clamps the top edge to a single overscan band", () => {
    // scrollTop 0 clamps rawStart -7 up to 0; the band stays [0, 10 + 7) —
    // the clamp must not re-add the top overscan (which would yield 24).
    expect(computeWindow(0, 10 * 20, 100, 20, 7)).toEqual({ start: 0, end: 17 });
    // Negative scroll positions behave like scrollTop 0.
    expect(computeWindow(-37, 10 * 20, 100, 20, 7)).toEqual({ start: 0, end: 17 });
  });

  it("rounds fractional viewports up to whole rows", () => {
    expect(computeWindow(0, 205, 100, 20, 0)).toEqual({ start: 0, end: 11 });
    expect(computeWindow(0, 201, 100, 20, 0)).toEqual({ start: 0, end: 11 });
    expect(computeWindow(0, 200, 100, 20, 0)).toEqual({ start: 0, end: 10 });
  });

  it("fails closed for non-finite totals and row heights", () => {
    expect(computeWindow(Number.NaN, 600, Number.NaN, 20, 5)).toEqual({ start: 0, end: 0 });
    expect(computeWindow(120, 600, 100, Number.NaN, 5)).toEqual({ start: 0, end: 0 });
    expect(computeWindow(120, 600, 100, Number.POSITIVE_INFINITY, 5)).toEqual({ start: 0, end: 0 });
    expect(computeWindow(Number.POSITIVE_INFINITY, 600, 100, 20, 5)).toEqual({ start: 0, end: 0 });
  });

  it("never returns an inverted or out-of-range window", () => {
    for (let scrollTop = -40; scrollTop <= 2_400; scrollTop += 37) {
      for (const totalRows of [0, 1, 19, 20, 101]) {
        const win = computeWindow(scrollTop, 413, totalRows, 20, 6);
        expect(win.start).toBeGreaterThanOrEqual(0);
        expect(win.start).toBeLessThanOrEqual(totalRows);
        expect(win.end).toBeGreaterThanOrEqual(win.start);
        expect(win.end).toBeLessThanOrEqual(totalRows);
      }
    }
  });
});
