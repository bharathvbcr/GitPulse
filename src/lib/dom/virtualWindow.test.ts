import { describe, expect, it } from "vitest";
import { clampScrollTop, computeWindow, ensureNonEmptyWindow } from "./virtualWindow";

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

describe("clampScrollTop", () => {
  // 100 rows x 20px = 2000px of content, 600px viewport: the deepest offset
  // that still pins row 99 to (the top of) the viewport.
  const MAX = 100 * 20 - 600;

  it("clamps a deep scroll to the last scrollable pixel", () => {
    expect(clampScrollTop(5_000, 100, 20, 600)).toBe(MAX);
    expect(clampScrollTop(0, 100, 20, 600)).toBe(0);
    expect(clampScrollTop(MAX - 0.5, 100, 20, 600)).toBe(MAX - 0.5);
    // An in-range anchor passes through untouched — clamping must not
    // disturb ordinary scrolling.
    expect(clampScrollTop(700, 100, 20, 600)).toBe(700);
  });

  it("keeps an exact-fit boundary scroll unchanged", () => {
    // scrollTop == max is the whole point: this is where the blank-frame bug
    // used to live (computeWindow saw a past-the-end offset and painted
    // {total, total}).
    expect(clampScrollTop(MAX, 100, 20, 600)).toBe(MAX);
  });

  it("fails closed to 0 for non-finite or negative scroll positions", () => {
    expect(clampScrollTop(Number.NaN, 100, 20, 600)).toBe(0);
    expect(clampScrollTop(Number.POSITIVE_INFINITY, 100, 20, 600)).toBe(0);
    expect(clampScrollTop(Number.NEGATIVE_INFINITY, 100, 20, 600)).toBe(0);
    expect(clampScrollTop(-0.0001, 100, 20, 600)).toBe(0);
    expect(clampScrollTop(-1e9, 100, 20, 600)).toBe(0);
  });

  it("fails closed to 0 for degenerate totals and row heights", () => {
    for (const total of [0, -3, Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY]) {
      expect(clampScrollTop(120, total, 20, 600)).toBe(0);
    }
    for (const rowHeight of [0, -1, Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY]) {
      expect(clampScrollTop(120, 100, rowHeight, 600)).toBe(0);
    }
  });

  it("fails closed to 0 for non-measurable viewports", () => {
    // A viewport that cannot bound scrolling yields no cap at all; zero and
    // positive values stay legitimate.
    for (const viewport of [
      Number.NaN,
      Number.POSITIVE_INFINITY,
      Number.NEGATIVE_INFINITY,
      -50,
    ]) {
      expect(clampScrollTop(120, 100, 20, viewport)).toBe(0);
    }
  });

  it("caps at the full spacer height while the viewport is unmeasured", () => {
    // Pre-mount state: nothing visible yet, so the only cap is the content
    // height itself (VirtualList starts viewportHeight at 0).
    expect(clampScrollTop(9_999, 100, 20, 0)).toBe(2_000);
    // In-range anchors pass through untouched.
    expect(clampScrollTop(999, 100, 20, 0)).toBe(999);
    expect(clampScrollTop(-5, 100, 20, 0)).toBe(0);
  });

  it("returns 0 when the content exactly fits or underfills the viewport", () => {
    expect(clampScrollTop(2_000, 100, 20, 2_000)).toBe(0);
    expect(clampScrollTop(499, 25, 20, 500)).toBe(0);
    expect(clampScrollTop(500, 25, 20, 500)).toBe(0);
  });

  it("survives fractional DPR-ish geometry without drift", () => {
    expect(clampScrollTop(1_234.56, 5_000, 13.37, 601.5)).toBe(1_234.56);
    // Past-the-end fractional scroll lands exactly on the fractional cap.
    expect(clampScrollTop(70_000, 5_000, 13.37, 601.5)).toBe(5_000 * 13.37 - 601.5);
    expect(clampScrollTop(43 + 1, 100, 0.5, 7)).toBe(43);
  });

  it("is idempotent, so one clamp application converges the bindable", () => {
    // The projection property backing the component fix: feeding the
    // browser-clamped value back through the pipeline changes nothing.
    for (const raw of [-10, 0, 700, MAX, MAX + 0.25, 9_999]) {
      const once = clampScrollTop(raw, 100, 20, 600);
      expect(clampScrollTop(once, 100, 20, 600)).toBe(once);
    }
  });
});

describe("ensureNonEmptyWindow", () => {
  it("leaves every non-empty window untouched", () => {
    expect(ensureNonEmptyWindow({ start: 0, end: 38 }, 100, 20, 600)).toEqual({
      start: 0,
      end: 38,
    });
    expect(ensureNonEmptyWindow({ start: 99, end: 100 }, 100, 20, 600)).toEqual({
      start: 99,
      end: 100,
    });
    expect(ensureNonEmptyWindow({ start: 0, end: 0 }, 0, 20, 600)).toEqual({ start: 0, end: 0 });
  });

  it("paints the last row when renderable content collapsed to empty", () => {
    // The blank-frame shape: a bottom anchor whose window came back {T, T}.
    expect(ensureNonEmptyWindow({ start: 100, end: 100 }, 100, 20, 600)).toEqual({
      start: 99,
      end: 100,
    });
    // An interior collapse (not producible today, but total) pins to that row.
    expect(ensureNonEmptyWindow({ start: 5, end: 5 }, 100, 20, 600)).toEqual({ start: 5, end: 6 });
    // Single-row list.
    expect(ensureNonEmptyWindow({ start: 1, end: 1 }, 1, 20, 600)).toEqual({ start: 0, end: 1 });
  });

  it("fails open only where a row has no identity", () => {
    // Degenerate geometry or sub-array-scale totals: pass through unchanged.
    for (const total of [0, -1, Number.NaN, Number.POSITIVE_INFINITY]) {
      expect(ensureNonEmptyWindow({ start: 3, end: 3 }, total, 20, 600)).toEqual({
        start: 3,
        end: 3,
      });
    }
    for (const rowHeight of [0, -1, Number.NaN]) {
      expect(ensureNonEmptyWindow({ start: 3, end: 3 }, 100, rowHeight, 600)).toEqual({
        start: 3,
        end: 3,
      });
    }
    for (const viewport of [0, -1, Number.NaN]) {
      expect(ensureNonEmptyWindow({ start: 3, end: 3 }, 100, 20, viewport)).toEqual({
        start: 3,
        end: 3,
      });
    }
    // Past MAX_SAFE_INTEGER rows there is no representable "last row"; the
    // input window is returned rather than an invented one.
    const beyond = { start: Number.MAX_SAFE_INTEGER, end: Number.MAX_SAFE_INTEGER };
    expect(
      ensureNonEmptyWindow(beyond, Number.MAX_SAFE_INTEGER + 1, 20, 600)
    ).toEqual(beyond);
    // Fractional totals still yield a non-empty band inside [0, total].
    expect(ensureNonEmptyWindow({ start: 2.5, end: 2.5 }, 2.5, 20, 600)).toEqual({
      start: 1.5,
      end: 2.5,
    });
  });
});
