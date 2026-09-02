import { describe, expect, it } from "vitest";
import {
  clampScrollTop,
  computeWindow,
  ensureNonEmptyWindow,
} from "./virtualWindow";

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

/**
 * The exact pipeline VirtualList.svelte runs since the blank-frame fix:
 * clamp the anchor, window over that, then guarantee a non-empty band.
 */
function effectiveWindow(
  rawScrollTop: number,
  viewportHeight: number,
  totalRows: number,
  rowHeight: number,
  overscan: number
): { clamped: number; win: { start: number; end: number } } {
  const clamped = clampScrollTop(rawScrollTop, totalRows, rowHeight, viewportHeight);
  const win = ensureNonEmptyWindow(
    computeWindow(clamped, viewportHeight, totalRows, rowHeight, overscan),
    totalRows,
    rowHeight,
    viewportHeight
  );
  return { clamped, win };
}

/**
 * Post-clamp invariants. Ordering and bounds hold for every input; the
 * "never blank" guarantee additionally requires renderable geometry —
 * computeWindow fails closed on non-positive row heights by documented
 * contract (and "one row" is undefined without one) — and totals within
 * array scale, past which "row N" has no identity so no band is expressible.
 */
function expectPostClampInvariants(
  win: { start: number; end: number },
  totalRows: number,
  rowHeight: number,
  viewportHeight: number
): void {
  expect(Number.isFinite(win.start)).toBe(true);
  expect(Number.isFinite(win.end)).toBe(true);
  expect(win.start).toBeGreaterThanOrEqual(0);
  expect(win.end).toBeGreaterThanOrEqual(win.start);
  expect(win.start).toBeLessThanOrEqual(totalRows);
  expect(win.end).toBeLessThanOrEqual(totalRows);
  if (
    Number.isFinite(totalRows) &&
    totalRows > 0 &&
    totalRows <= Number.MAX_SAFE_INTEGER &&
    Number.isFinite(rowHeight) &&
    rowHeight > 0 &&
    Number.isFinite(viewportHeight) &&
    viewportHeight > 0
  ) {
    // THE invariant: content exists and is visible, so at least one row
    // paints — never a blank pane while the browser's async clamp is in
    // flight.
    expect(win.start).toBeLessThan(win.end);
  }
}

describe("clampScrollTop stress: shrink/grow cycles under deep scrolls", () => {
  /** Deterministic mulberry32 — a failure must be exactly reproducible. */
  function mulberry32(seed: number): () => number {
    let state = seed | 0;
    return () => {
      state = (state + 0x6d2b79f5) | 0;
      let t = Math.imul(state ^ (state >>> 15), 1 | state);
      t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
  }

  it("renders at least one row across 2,000 randomized shrink/grow frames", () => {
    const rand = mulberry32(0x1f2e3d4c);
    const rowHeights = [20, 24.4, 0.5];
    const viewports = [600, 900];
    for (let i = 0; i < 2_000; i++) {
      // Whitespace-collapse style churn: half the frames collapse a huge diff
      // to a handful of rows, the rest regrow it.
      const total =
        rand() < 0.5 ? Math.floor(rand() * 300_000) : Math.floor(rand() * 40) + 1;
      const rowHeight = rowHeights[i % rowHeights.length];
      const viewportHeight = viewports[i % viewports.length];
      // Mostly deep anchors past the shrunk content, sometimes beyond 32 bits,
      // occasionally garbage.
      const roll = rand();
      const rawScroll =
        roll < 0.6
          ? rand() * 6_000_000
          : roll < 0.9
            ? rand() * Number.MAX_SAFE_INTEGER
            : rand() < 0.5
              ? Number.NaN
              : -rand() * 1e6;
      const { win } = effectiveWindow(rawScroll, viewportHeight, total, rowHeight, 8);
      expectPostClampInvariants(win, total, rowHeight, viewportHeight);
    }
  });

  it("holds post-clamp invariants across the adversarial grid", () => {
    const totals = [0, 1, 2.5, 99, 300_000, Number.MAX_SAFE_INTEGER, 1e300];
    const rowHeights = [Number.MIN_VALUE, 1e-300, 0.001, 0.5, 13.37, 20, 1e6, 1e300];
    const viewports = [1e-300, 0.5, 600, 1_000_000, 1e300];
    const scrolls = [0, 7.25, 1_234.56, 1e9, Number.MAX_SAFE_INTEGER, Number.MAX_VALUE];
    const overscans = [0, 3, Number.NaN];

    for (const total of totals) {
      for (const rowHeight of rowHeights) {
        for (const viewport of viewports) {
          for (const scroll of scrolls) {
            for (const overscan of overscans) {
              const { clamped, win } = effectiveWindow(
                scroll,
                viewport,
                total,
                rowHeight,
                overscan
              );
              // The clamp itself must be a sane projection first.
              expect(Number.isFinite(clamped)).toBe(true);
              expect(clamped).toBeGreaterThanOrEqual(0);
              if (Number.isFinite(scroll)) {
                expect(clamped).toBeLessThanOrEqual(Math.max(0, scroll));
              }
              expectPostClampInvariants(win, total, rowHeight, viewport);
            }
          }
        }
      }
    }
  });

  it("never blanks at the bottom edge with physical-range geometry", () => {
    // Realistic DiffViewer scale: integer row counts up to ~1M, fractional
    // row heights, ordinary viewports — including overscan 0, where
    // computeWindow has no cushion to absorb float round-trip at the cap.
    let probed = 0;
    const totalsToProbe: number[] = [];
    for (let t = 1; t <= 50; t++) totalsToProbe.push(t);
    for (let t = 51; t <= 4_000_000; t = Math.floor(t * 7 + 1)) totalsToProbe.push(t);
    for (const total of totalsToProbe) {
      for (const rowHeight of [0.1, 0.25, 0.5, 7, 13.37, 16.8, 20, 24.4, 33.3]) {
        for (const viewport of [0.5, 1, 600, 601.5, 1080.75]) {
          for (const overscan of [0, 2, 8]) {
            // Anchored exactly at (and past) the pixel cap.
            for (const scroll of [
              clampScrollTop(Number.MAX_SAFE_INTEGER, total, rowHeight, viewport),
              Math.max(0, total * rowHeight - viewport),
              Math.max(0, total * rowHeight),
            ]) {
              const { win } = effectiveWindow(scroll, viewport, total, rowHeight, overscan);
              expectPostClampInvariants(win, total, rowHeight, viewport);
              probed++;
            }
          }
        }
      }
    }
    // Guard against silently probing nothing if the loops above rot.
    expect(probed).toBeGreaterThan(20_000);
  }, 20000);
  it("keeps the clamped anchor's row painted even at extreme geometry", () => {
    // Whatever the inputs, once clamped, the row under the anchor must be on
    // screen (this is the user-visible half of "no blank frame").
    const cases: Array<[number, number, number, number]> = [
      [999_999, 600, 50_000, 20],
      [1e9, 900, 37, 24.4],
      [Number.MAX_SAFE_INTEGER, 600, 123_456, 13.37],
      [2_000 - 0.25, 600, 100, 20],
    ];
    for (const [scroll, viewport, total, rowHeight] of cases) {
      const { clamped, win } = effectiveWindow(scroll, viewport, total, rowHeight, 8);
      const anchorRow = Math.floor(clamped / rowHeight);
      expect(win.start).toBeLessThanOrEqual(anchorRow);
      expect(win.end).toBeGreaterThan(anchorRow);
    }
  });
});
