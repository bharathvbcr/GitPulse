import { describe, expect, it } from "vitest";
import { computeWindow } from "./virtualWindow";

/**
 * Invariants for every cell of the adversarial grid:
 * - the window is finite;
 * - start ≥ 0 and end ≥ start always;
 * - for a real (finite, non-negative) totalRows: end ≤ totalRows;
 * - degenerate totals fail closed with an empty window.
 */
function expectInvariants(
  win: { start: number; end: number },
  totalRows: number
): void {
  expect(Number.isFinite(win.start)).toBe(true);
  expect(Number.isFinite(win.end)).toBe(true);
  expect(win.start).toBeGreaterThanOrEqual(0);
  expect(win.end).toBeGreaterThanOrEqual(win.start);
  if (Number.isFinite(totalRows) && totalRows >= 0) {
    expect(win.start).toBeLessThanOrEqual(totalRows);
    expect(win.end).toBeLessThanOrEqual(totalRows);
  } else {
    // NaN / Infinity / negative totals must paint nothing at all.
    expect(win).toEqual({ start: 0, end: 0 });
  }
}

/** The band must cover the row under scrollTop whenever the list reaches it. */
function expectCoverage(
  win: { start: number; end: number },
  scrollTop: number,
  viewportHeight: number,
  totalRows: number,
  rowHeight: number
): void {
  if (!(viewportHeight > 0 && Number.isFinite(viewportHeight))) return;
  if (!Number.isFinite(scrollTop)) return;
  if (!(Number.isFinite(rowHeight) && rowHeight > 0)) return;
  if (!(Number.isFinite(totalRows) && totalRows > 0)) return;
  const firstVisible = Math.floor(Math.max(0, scrollTop) / rowHeight);
  if (firstVisible >= totalRows) return; // overscroll past the end: nothing to cover
  expect(win.start).toBeLessThanOrEqual(firstVisible);
  expect(win.end).toBeGreaterThan(firstVisible);
}

describe("computeWindow stress: exhaustive adversarial grid", () => {
  const totals = [0, 1, Number.NaN, Number.POSITIVE_INFINITY, -5];
  const heights = [0, -1, Number.NaN, Number.POSITIVE_INFINITY, 0.5];
  const scrolls = [Number.NaN, -1000, 0, 123.75, 1e9];
  const overscans = [-5, 0, 3, Number.NaN, Number.POSITIVE_INFINITY];
  const viewports = [600, 0, Number.NaN, Number.POSITIVE_INFINITY];

  it("holds every invariant across all 2,500 parameter combinations", () => {
    for (const totalRows of totals) {
      for (const rowHeight of heights) {
        for (const scrollTop of scrolls) {
          for (const overscan of overscans) {
            for (const viewportHeight of viewports) {
              const win = computeWindow(
                scrollTop,
                viewportHeight,
                totalRows,
                rowHeight,
                overscan
              );
              expectInvariants(win, totalRows);
              expectCoverage(win, scrollTop, viewportHeight, totalRows, rowHeight);
            }
          }
        }
      }
    }
  });

  it("covers the scrolled row with fractional row heights", () => {
    // scrollTop 10px at 0.5px/row = row 20; viewport 5px = 10 rows.
    expect(computeWindow(10, 5, 100, 0.5, 0)).toEqual({ start: 20, end: 30 });
  });

  // DEFECT FIX (virtualWindow.ts): Math.max(0, Math.floor(NaN)) is NaN, so a
  // non-finite overscan used to poison start/end into NaN and violate the
  // documented "[0, totalRows]" clamp. NaN now clamps to 0 (+Infinity keeps
  // its render-the-whole-list meaning via the clamps).
  it("treats non-finite overscan as zero instead of poisoning the band", () => {
    expect(computeWindow(0, 600, 100, 20, Number.NaN)).toEqual({ start: 0, end: 30 });
    expect(computeWindow(120, 600, 100, 20, Number.NaN)).toEqual({ start: 6, end: 36 });
  });

  it("pins Infinity overscan to render the whole list", () => {
    // Defensible-but-surprising: an infinite overscan band degrades to
    // "render everything", still clamped. Pinned as contract.
    expect(computeWindow(120, 600, 100, 20, Number.POSITIVE_INFINITY)).toEqual({
      start: 0,
      end: 100,
    });
  });

  it("clamps negative overscan to zero rather than widening", () => {
    expect(computeWindow(50 * 20, 10 * 20, 100, 20, -5)).toEqual({ start: 50, end: 60 });
  });

  it("fails closed on NaN scroll even with generous geometry", () => {
    expect(computeWindow(Number.NaN, 600, 100, 20, 7)).toEqual({ start: 0, end: 0 });
  });

  it("treats an infinite viewport as zero visible rows plus overscan only", () => {
    // Defensible-but-surprising: non-finite viewport contributes no visible
    // rows; only the overscan bands paint. end = 50 + 0 + 3. Pinned as
    // contract.
    expect(computeWindow(50 * 20, Number.POSITIVE_INFINITY, 100, 20, 3)).toEqual({
      start: 47,
      end: 53,
    });
  });
});
