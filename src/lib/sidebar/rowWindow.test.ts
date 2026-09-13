import { describe, expect, it } from "vitest";
import {
  buildRowOffsets,
  clampScrollTopToOffsets,
  rowBottom,
  rowCount,
  rowTop,
  scrollOffsetToCenter,
  scrollOffsetToReveal,
  totalRowHeight,
  windowFromOffsets,
} from "./rowWindow";
import {
  BRANCH_OVERSCAN,
  BRANCH_ROW_HEIGHT,
  BRANCH_ROW_HEIGHT_TWO_LINE,
  BRANCH_ROW_LAYOUTS,
  isBranchRowLayout,
  sidebarRowHeight,
  type SidebarRowKind,
} from "./metrics";

/** The sidebar's real shape: a section header, then branches under it. */
const SIDEBAR_KINDS: SidebarRowKind[] = [
  "section-header",
  "branch",
  "branch",
  "folder-header",
  "branch",
  "section-header",
  "tag",
];

function heightsFor(kinds: SidebarRowKind[]): number[] {
  return kinds.map((kind) => sidebarRowHeight(kind, "spacious", "two-line"));
}

describe("buildRowOffsets", () => {
  it("returns prefix sums with a leading zero and a trailing total", () => {
    const offsets = buildRowOffsets([10, 20, 5]);
    expect(offsets).toEqual([0, 10, 30, 35]);
    expect(rowCount(offsets)).toBe(3);
    expect(totalRowHeight(offsets)).toBe(35);
  });

  it("describes an empty list as a single zero, not an empty array", () => {
    // rowCount must come out 0, and totalRowHeight must not read past the end.
    const offsets = buildRowOffsets([]);
    expect(offsets).toEqual([0]);
    expect(rowCount(offsets)).toBe(0);
    expect(totalRowHeight(offsets)).toBe(0);
  });

  it("keeps the array non-decreasing when a height is degenerate", () => {
    // Binary search is only meaningful over a sorted array, and one NaN in a
    // prefix sum makes every later comparison false — which would silently
    // paint an empty list rather than a wrong one.
    const offsets = buildRowOffsets([10, Number.NaN, 20, -5, Number.POSITIVE_INFINITY, 8]);
    for (let i = 1; i < offsets.length; i++) {
      expect(offsets[i]).toBeGreaterThanOrEqual(offsets[i - 1]);
      expect(Number.isFinite(offsets[i])).toBe(true);
    }
    // The finite rows keep their real heights; the degenerate ones take none.
    expect(totalRowHeight(offsets)).toBe(38);
  });
});

describe("rowTop / rowBottom", () => {
  it("reports the real edges of a mixed-height list", () => {
    const offsets = buildRowOffsets(heightsFor(SIDEBAR_KINDS));
    const header = BRANCH_ROW_HEIGHT.spacious;
    const branch = BRANCH_ROW_HEIGHT_TWO_LINE.spacious;

    expect(rowTop(offsets, 0)).toBe(0);
    expect(rowBottom(offsets, 0)).toBe(header);
    // Row 1 is the first branch: it starts below the header, not at 1 * branch.
    expect(rowTop(offsets, 1)).toBe(header);
    expect(rowBottom(offsets, 1)).toBe(header + branch);
    expect(rowTop(offsets, 2)).toBe(header + branch);
  });

  it("is not index * height once heights differ — the whole point of the module", () => {
    const offsets = buildRowOffsets(heightsFor(SIDEBAR_KINDS));
    const uniform = BRANCH_ROW_HEIGHT_TWO_LINE.spacious;
    // The old arithmetic would have put row 3 here. If these ever agree the
    // fixture has stopped mixing heights and this suite proves nothing.
    expect(rowTop(offsets, 3)).not.toBe(3 * uniform);
  });

  it("fails closed to the top for an index outside the list", () => {
    const offsets = buildRowOffsets([10, 20]);
    for (const bad of [-1, 2, 99, 1.5, Number.NaN]) {
      expect(rowTop(offsets, bad)).toBe(0);
      expect(rowBottom(offsets, bad)).toBe(0);
    }
  });
});

describe("clampScrollTopToOffsets", () => {
  it("caps at the last offset that still shows content", () => {
    const offsets = buildRowOffsets([100, 100, 100]);
    expect(clampScrollTopToOffsets(1000, offsets, 120)).toBe(180);
    expect(clampScrollTopToOffsets(50, offsets, 120)).toBe(50);
  });

  it("returns 0 when the content is shorter than the viewport", () => {
    const offsets = buildRowOffsets([30]);
    expect(clampScrollTopToOffsets(500, offsets, 900)).toBe(0);
  });

  it("fails closed to 0 on every degenerate input", () => {
    const offsets = buildRowOffsets([30, 30]);
    expect(clampScrollTopToOffsets(Number.NaN, offsets, 100)).toBe(0);
    expect(clampScrollTopToOffsets(Number.POSITIVE_INFINITY, offsets, 100)).toBe(0);
    expect(clampScrollTopToOffsets(-40, offsets, 100)).toBe(0);
    expect(clampScrollTopToOffsets(10, buildRowOffsets([]), 100)).toBe(0);
    expect(clampScrollTopToOffsets(10, offsets, Number.NaN)).toBe(0);
    expect(clampScrollTopToOffsets(10, offsets, -1)).toBe(0);
  });
});

describe("windowFromOffsets", () => {
  const heights = [30, 44, 44, 30, 44, 30, 30];
  const offsets = buildRowOffsets(heights); // tops: 0 30 74 118 148 192 222, total 252

  it("covers every row intersecting the viewport with no overscan", () => {
    // Viewport [30, 130) touches rows 1 (30–74), 2 (74–118) and 3 (118–148).
    expect(windowFromOffsets(30, 100, offsets, 0)).toEqual({ start: 1, end: 4 });
  });

  it("includes a row whose top edge is above the viewport but whose body is inside", () => {
    // Scrolled into the middle of row 2: row 2 must still be painted.
    expect(windowFromOffsets(80, 20, offsets, 0)).toEqual({ start: 2, end: 3 });
  });

  it("pads by overscan in rows on both sides and clamps to the list", () => {
    // Viewport [118, 148) is row 3 alone; ±2 rows of pad reach 1 and 6.
    expect(windowFromOffsets(118, 30, offsets, 2)).toEqual({ start: 1, end: 6 });
    // At the top, the leading pad clamps at 0 without stealing from the tail:
    // row 0 is the only visible row, so the band is it plus two rows below.
    expect(windowFromOffsets(0, 30, offsets, 2)).toEqual({ start: 0, end: 3 });
    expect(windowFromOffsets(0, 30, offsets, Number.POSITIVE_INFINITY)).toEqual({
      start: 0,
      end: heights.length,
    });
  });

  it("paints the tail row rather than a blank pane at the very bottom", () => {
    // An anchor past the content returns an empty band from the arithmetic;
    // the guarantee folded into the function turns it into one real row.
    const win = windowFromOffsets(totalRowHeight(offsets) + 500, 100, offsets, 0);
    expect(win.end).toBeGreaterThan(win.start);
    expect(win.end).toBe(heights.length);
  });

  it("paints nothing for an empty list or an unknown anchor", () => {
    expect(windowFromOffsets(0, 100, buildRowOffsets([]), 4)).toEqual({ start: 0, end: 0 });
    expect(windowFromOffsets(Number.NaN, 100, offsets, 4)).toEqual({ start: 0, end: 0 });
  });

  it("paints nothing before the viewport has been measured", () => {
    // clientHeight is 0 on the first frame; guessing a height there would
    // mount rows that are immediately thrown away.
    expect(windowFromOffsets(0, 0, offsets, 0)).toEqual({ start: 0, end: 0 });
  });

  it("never returns a band outside the list for any anchor", () => {
    for (let top = -200; top <= totalRowHeight(offsets) + 200; top += 7) {
      const win = windowFromOffsets(top, 90, offsets, BRANCH_OVERSCAN);
      expect(win.start).toBeGreaterThanOrEqual(0);
      expect(win.end).toBeLessThanOrEqual(heights.length);
      expect(win.end).toBeGreaterThanOrEqual(win.start);
    }
  });

  it("leaves no visible gap: the band always spans the whole viewport", () => {
    // The property that matters on screen. Walk every scroll position the
    // clamp permits and assert the painted band starts at or above the
    // viewport top and ends at or below its bottom.
    const viewport = 90;
    for (let top = 0; top <= totalRowHeight(offsets); top += 3) {
      const clamped = clampScrollTopToOffsets(top, offsets, viewport);
      const win = windowFromOffsets(clamped, viewport, offsets, 0);
      expect(rowTop(offsets, win.start)).toBeLessThanOrEqual(clamped);
      expect(rowBottom(offsets, win.end - 1)).toBeGreaterThanOrEqual(
        Math.min(clamped + viewport, totalRowHeight(offsets)),
      );
    }
  });
});

describe("scrollOffsetToReveal", () => {
  const offsets = buildRowOffsets([30, 44, 44, 30, 44, 30, 30]);

  it("returns null for a row already fully on screen", () => {
    expect(scrollOffsetToReveal(offsets, 1, 30, 100)).toBeNull();
  });

  it("scrolls up to the row's own top edge", () => {
    expect(scrollOffsetToReveal(offsets, 1, 120, 100)).toBe(30);
  });

  it("scrolls down by exactly enough to show the row's bottom edge", () => {
    // Row 4 spans 148–192. With a 100px viewport at 0, reveal needs top 92.
    expect(scrollOffsetToReveal(offsets, 4, 0, 100)).toBe(92);
  });

  it("lands the row fully inside the viewport for every row and anchor", () => {
    // The behavioural contract, checked rather than asserted per-case: after
    // applying the returned offset, the row is inside the viewport.
    const viewport = 100;
    for (let index = 0; index < rowCount(offsets); index++) {
      for (const anchor of [0, 17, 60, 140, 222, 400]) {
        const next = scrollOffsetToReveal(offsets, index, anchor, viewport) ?? anchor;
        expect(rowTop(offsets, index)).toBeGreaterThanOrEqual(next);
        expect(rowBottom(offsets, index)).toBeLessThanOrEqual(next + viewport);
      }
    }
  });

  it("returns null rather than a number for an out-of-range row or geometry", () => {
    expect(scrollOffsetToReveal(offsets, -1, 0, 100)).toBeNull();
    expect(scrollOffsetToReveal(offsets, 99, 0, 100)).toBeNull();
    expect(scrollOffsetToReveal(offsets, 1, 0, 0)).toBeNull();
    expect(scrollOffsetToReveal(offsets, 1, Number.NaN, 100)).toBeNull();
  });
});

describe("scrollOffsetToCenter", () => {
  const offsets = buildRowOffsets([30, 44, 44, 30, 44, 30, 30]);

  it("parks the row about a third of the way down the viewport", () => {
    // Row 4 starts at 148; a 90px viewport biases by 30.
    expect(scrollOffsetToCenter(offsets, 4, 90)).toBe(118);
  });

  it("never asks for an offset outside the scrollable range", () => {
    const total = totalRowHeight(offsets);
    for (let index = 0; index < rowCount(offsets); index++) {
      const target = scrollOffsetToCenter(offsets, index, 90);
      expect(target).toBeGreaterThanOrEqual(0);
      expect(target).toBeLessThanOrEqual(Math.max(0, total - 90));
    }
  });

  it("shows the first rows from the top instead of scrolling past them", () => {
    expect(scrollOffsetToCenter(offsets, 0, 90)).toBe(0);
  });
});

describe("sidebarRowHeight", () => {
  it("only grows branch rows, and only under the two-line layout", () => {
    const header = BRANCH_ROW_HEIGHT.spacious;
    expect(sidebarRowHeight("branch", "spacious", "two-line")).toBe(
      BRANCH_ROW_HEIGHT_TWO_LINE.spacious,
    );
    expect(sidebarRowHeight("branch", "spacious", "one-line")).toBe(header);
    for (const kind of ["section-header", "folder-header", "tag"] as const) {
      expect(sidebarRowHeight(kind, "spacious", "two-line")).toBe(header);
      expect(sidebarRowHeight(kind, "spacious", "one-line")).toBe(header);
    }
  });

  it("keeps two lines taller than one in both densities", () => {
    for (const density of ["spacious", "compact"] as const) {
      expect(sidebarRowHeight("branch", density, "two-line")).toBeGreaterThan(
        sidebarRowHeight("branch", density, "one-line"),
      );
    }
    // Compact must stay genuinely tighter than spacious in the new layout too.
    expect(BRANCH_ROW_HEIGHT_TWO_LINE.compact).toBeLessThan(BRANCH_ROW_HEIGHT_TWO_LINE.spacious);
  });

  it("leaves room for two lines of real text", () => {
    // Line one is 13px-icon-beside-12px-text, line two is 10px text. A height
    // that cannot fit both clips the row, and clipping is invisible in a
    // screenshot test that only checks the row exists.
    for (const density of ["spacious", "compact"] as const) {
      expect(BRANCH_ROW_HEIGHT_TWO_LINE[density]).toBeGreaterThanOrEqual(13 + 10 + 8);
    }
  });

  it("fails closed to the roomier height on an unknown density or layout", () => {
    expect(sidebarRowHeight("branch", "nonsense" as "spacious", "two-line")).toBe(
      BRANCH_ROW_HEIGHT_TWO_LINE.spacious,
    );
    expect(
      sidebarRowHeight("branch", undefined as unknown as "spacious", "two-line"),
    ).toBe(BRANCH_ROW_HEIGHT_TWO_LINE.spacious);
    // An unknown layout is not "one-line": it must not silently shrink rows
    // whose markup is still two lines tall.
    expect(sidebarRowHeight("branch", "spacious", "nonsense" as "two-line")).toBe(
      BRANCH_ROW_HEIGHT_TWO_LINE.spacious,
    );
  });
});

describe("isBranchRowLayout", () => {
  it("accepts exactly the advertised layouts", () => {
    for (const layout of BRANCH_ROW_LAYOUTS) {
      expect(isBranchRowLayout(layout)).toBe(true);
    }
    for (const bad of ["", "two", "TWO-LINE", null, undefined, 0, {}, ["two-line"]]) {
      expect(isBranchRowLayout(bad)).toBe(false);
    }
  });

  it("defaults to the layout that keeps names readable", () => {
    expect(BRANCH_ROW_LAYOUTS[0]).toBe("two-line");
  });
});
