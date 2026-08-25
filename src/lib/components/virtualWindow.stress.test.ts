import { describe, expect, it } from "vitest";
import { computeWindow } from "../dom/virtualWindow";

/**
 * Attacks computeWindow through the exact shape VirtualList.svelte calls it
 * with: `overscan` defaults to 8 there, `viewportHeight` starts at 0 until a
 * ResizeObserver reports, and `scrollTop` is a two-way BINDABLE that any
 * split-pane writer can set to garbage before the DOM clamps it.
 *
 * Invariants under attack:
 * - totality: never throws, outputs always finite numbers;
 * - ordering: 0 <= start <= end;
 * - clamping: end <= totalRows whenever totalRows is finite and >= 0;
 * - fail-closed: degenerate geometry paints nothing.
 */

function expectTotal(win: { start: number; end: number }, totalRows: number): void {
  expect(Number.isFinite(win.start)).toBe(true);
  expect(Number.isFinite(win.end)).toBe(true);
  expect(win.start).toBeGreaterThanOrEqual(0);
  expect(win.end).toBeGreaterThanOrEqual(win.start);
  if (Number.isFinite(totalRows) && totalRows >= 0) {
    expect(win.end).toBeLessThanOrEqual(totalRows);
    expect(win.start).toBeLessThanOrEqual(totalRows);
  }
}

const GARBAGE = [
  Number.NaN,
  Number.POSITIVE_INFINITY,
  Number.NEGATIVE_INFINITY,
  undefined,
  null,
  "",
  "12",
  "12px",
  {},
];

describe("computeWindow stress: scrollTop beyond content", () => {
  const VIEWS = [600, 1_000_000];

  it("clamps elastic overscroll to an empty tail window instead of inventing rows", () => {
    for (const viewportHeight of VIEWS) {
      // scrollTop 1e9 past a 100-row list: start pins to totalRows so zero
      // rows paint — no negative-length band, no out-of-range row indices.
      expect(computeWindow(1e9, viewportHeight, 100, 20, 8)).toEqual({
        start: 100,
        end: 100,
      });
    }
  });

  it("absorbs astronomically large but finite scroll offsets", () => {
    for (const scroll of [Number.MAX_SAFE_INTEGER + 1, 1e300, Number.MAX_VALUE]) {
      expectTotal(computeWindow(scroll, 600, 100, 20, 8), 100);
      expect(computeWindow(scroll, 600, 100, 20, 8)).toEqual({ start: 100, end: 100 });
    }
  });

  it("treats non-finite scroll as 'no anchor' and paints nothing", () => {
    for (const garbage of [Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY]) {
      expect(computeWindow(garbage, 600, 100, 20, 8)).toEqual({ start: 0, end: 0 });
    }
  });

  it("coerces caller-side garbage deterministically via Number()", () => {
    // VirtualList binds scrollTop straight from state; a hostile binding may
    // hold anything coercible. The fn itself demands a number, so callers
    /// that pass raw values go through Number() first — assert every lane.
    for (const raw of GARBAGE) {
      const coerced = typeof raw === "number" ? raw : Number(raw);
      expectTotal(computeWindow(coerced, 600, 100, 20, 8), 100);
    }
    expect(computeWindow(Number(null), 600, 100, 20, 8)).toEqual({ start: 0, end: 38 }); // null -> 0
    expect(computeWindow(Number(""), 600, 100, 20, 8)).toEqual({ start: 0, end: 38 });   // "" -> 0
    // 12px < one row: floor(12/20) is still row 0.
    expect(computeWindow(Number("12"), 600, 100, 20, 8)).toEqual({ start: 0, end: 38 });
    expect(computeWindow(Number("12px"), 600, 100, 20, 8)).toEqual({ start: 0, end: 0 }); // NaN fail-closed
  });

  it("keeps negative scrollTop pinned at the top band", () => {
    expect(computeWindow(-0.0001, 600, 100, 20, 8)).toEqual({ start: 0, end: 38 });
    expect(computeWindow(-1e9, 600, 100, 20, 8)).toEqual({ start: 0, end: 38 });
  });
});

describe("computeWindow stress: pre-mount / degenerate viewport", () => {
  it("renders overscan-only bands while viewportHeight is still 0", () => {
    // VirtualList's ResizeObserver has not fired yet: viewport 0 means zero
    // visible rows, so exactly the default overscan=8 band below the anchor.
    expect(computeWindow(0, 0, 100, 20, 8)).toEqual({ start: 0, end: 8 });
    expect(computeWindow(40 * 20, 0, 100, 20, 8)).toEqual({ start: 32, end: 48 });
  });

  it("fails sane for NaN/negative/infinite viewports across totals", () => {
    for (const totalRows of [0, 1, 100]) {
      for (const viewportHeight of [Number.NaN, -50, Number.POSITIVE_INFINITY]) {
        expectTotal(computeWindow(120, viewportHeight, totalRows, 20, 3), totalRows);
        // No visible rows: the band is pure overscan around the anchor.
        const win = computeWindow(120, viewportHeight, totalRows, 20, 3);
        if (totalRows >= 6) {
          expect(win.start).toBe(3); // floor(120/20) - 3
          expect(win.end).toBe(totalRows === 1 ? 1 : 9);
        }
      }
    }
  });
});

describe("computeWindow stress: degenerate row heights", () => {
  it("paints nothing for zero, negative or NaN rowHeight regardless of scroll", () => {
    for (const rowHeight of [0, -1, Number.NaN, Number.NEGATIVE_INFINITY]) {
      expect(computeWindow(500, 600, 100, rowHeight, 8)).toEqual({ start: 0, end: 0 });
    }
  });

  it("stays finite when rowHeight underflows division into Infinity", () => {
    // 1 / Number.MIN_VALUE = Infinity; the clamps must absorb it, never leak.
    const win = computeWindow(1, 600, 100, Number.MIN_VALUE, 8);
    expectTotal(win, 100);
    expect(win).toEqual({ start: 100, end: 100 });
  });

  it("covers the scrolled row with DPR-fractional row heights", () => {
    for (const rowHeight of [0.5, 13.37, 1 / 3, 10.5]) {
      const scrollTop = 1234.56;
      const totalRows = 5_000;
      const win = computeWindow(scrollTop, 600, totalRows, rowHeight, 8);
      expectTotal(win, totalRows);
      const firstVisible = Math.floor(scrollTop / rowHeight);
      expect(win.start).toBeLessThanOrEqual(firstVisible);
      expect(win.end).toBeGreaterThan(firstVisible);
      // Integer totals must yield integer bounds (VirtualList indexes rows).
      expect(Number.isInteger(win.start)).toBe(true);
      expect(Number.isInteger(win.end)).toBe(true);
    }
  });
});

describe("computeWindow stress: degenerate totals", () => {
  it("paints nothing for empty, NaN, infinite or negative totals", () => {
    for (const totalRows of [0, Number.NaN, Number.POSITIVE_INFINITY, -1, -1e9]) {
      expect(computeWindow(100, 600, totalRows, 20, 8)).toEqual({ start: 0, end: 0 });
    }
  });

  it("characterizes fractional totals as clamped-but-fractional bounds", () => {
    // VirtualList can never produce this (itemCount ?? items?.length ?? 0 is
    // always an integer), but the math stays TOTAL: finite, ordered, clamped
    // — merely non-integral. Pinned so anyone tightening integrality does it
    // deliberately.
    expect(computeWindow(1e9, 600, 2.5, 20, 8)).toEqual({ start: 2.5, end: 2.5 });
    expect(computeWindow(0, 600, 2.5, 20, 8)).toEqual({ start: 0, end: 2.5 });
  });
});

describe("computeWindow stress: start>end inversion guard", () => {
  it("never inverts across the full adversarial grid", () => {
    const scrolls = [-1e9, -0.5, 0, 7.25, 1e9];
    const viewports = [0, 1, 600, Number.NaN, Number.POSITIVE_INFINITY];
    const totals = [0, 1, 99, 100];
    const rowHeights = [Number.MIN_VALUE, 0.001, 0.5, 1, 20, 1e6, Number.NaN];
    const overscans = [-8, 0, 8, Number.NaN, Number.POSITIVE_INFINITY];

    for (const scrollTop of scrolls) {
      for (const viewportHeight of viewports) {
        for (const totalRows of totals) {
          for (const rowHeight of rowHeights) {
            for (const overscan of overscans) {
              const win = computeWindow(scrollTop, viewportHeight, totalRows, rowHeight, overscan);
              expectTotal(win, totalRows);
            }
          }
        }
      }
    }
  });

  it("keeps the exact VirtualList defaults total on the happy path", () => {
    // Sanity anchor: ordinary inputs still produce the ordinary window.
    expect(computeWindow(0, 600, 100, 20, 8)).toEqual({ start: 0, end: 38 });
    expect(computeWindow(50 * 20, 600, 100, 20, 8)).toEqual({ start: 42, end: 88 });
    expect(computeWindow(95 * 20, 600, 100, 20, 8)).toEqual({ start: 87, end: 100 });
  });
});
