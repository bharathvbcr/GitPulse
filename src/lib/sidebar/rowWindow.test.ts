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
  BRANCH_ROW_HEIGHT_THREE_LINE,
  BRANCH_ROW_HEIGHT_TWO_LINE,
  BRANCH_ROW_LAYOUTS,
  DEFAULT_BRANCH_ROW_LAYOUT,
  branchRowLines,
  isBranchRowLayout,
  sidebarRowHeight,
  type BranchRowLayout,
  type SidebarDensity,
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

  it("keeps the row an inset inside each edge, for a scroller whose edges are covered", () => {
    const viewport = 160;
    for (let index = 0; index < rowCount(offsets); index++) {
      for (const anchor of [0, 17, 60, 140, 222, 400]) {
        const next = scrollOffsetToReveal(offsets, index, anchor, viewport, 30) ?? anchor;
        // A row at the very top has nothing above to clear, so its inset is 0.
        expect(rowTop(offsets, index) - next).toBeGreaterThanOrEqual(Math.min(30, rowTop(offsets, index)));
        expect(next + viewport - rowBottom(offsets, index)).toBeGreaterThanOrEqual(30);
      }
    }
  });

  it("never asks for a scroll to row 0 when the list is already at the top", () => {
    expect(scrollOffsetToReveal(offsets, 0, 0, 160, 30)).toBeNull();
    expect(scrollOffsetToReveal(offsets, 1, 0, 160, 30)).toBeNull();
  });

  it("caps the inset at a quarter of the viewport, so a short list is still navigable", () => {
    // 60 px viewport: the inset is 15, not 40. Row 1 spans 30–74.
    expect(scrollOffsetToReveal(offsets, 1, 0, 60, 40)).toBe(74 - 60 + 15);
  });

  it("treats a negative or non-finite inset as flush", () => {
    expect(scrollOffsetToReveal(offsets, 4, 0, 100, -5)).toBe(92);
    expect(scrollOffsetToReveal(offsets, 4, 0, 100, Number.NaN)).toBe(92);
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
  it("only grows branch rows, and only under the multi-line layouts", () => {
    const header = BRANCH_ROW_HEIGHT.spacious;
    expect(sidebarRowHeight("branch", "spacious", "two-line")).toBe(
      BRANCH_ROW_HEIGHT_TWO_LINE.spacious,
    );
    expect(sidebarRowHeight("branch", "spacious", "three-line")).toBe(
      BRANCH_ROW_HEIGHT_THREE_LINE.spacious,
    );
    expect(sidebarRowHeight("branch", "spacious", "one-line")).toBe(header);
    for (const kind of ["section-header", "folder-header", "tag"] as const) {
      for (const layout of BRANCH_ROW_LAYOUTS) {
        expect(sidebarRowHeight(kind, "spacious", layout)).toBe(header);
        expect(sidebarRowHeight(kind, "compact", layout)).toBe(BRANCH_ROW_HEIGHT.compact);
      }
    }
  });

  it("answers the measured three-line heights per density", () => {
    // 47px of content (three 13px lines and two 4px gaps) plus 15px spacious
    // and 9px compact. Pinned as numbers: the harness measures the content,
    // and this is the half that fails if the constants move without it.
    expect(sidebarRowHeight("branch", "spacious", "three-line")).toBe(62);
    expect(sidebarRowHeight("branch", "compact", "three-line")).toBe(56);
    expect(sidebarRowHeight("branch", "spacious", "two-line")).toBe(40);
    expect(sidebarRowHeight("branch", "compact", "two-line")).toBe(34);
  });

  it("keeps each extra line taller than the last in both densities", () => {
    for (const density of ["spacious", "compact"] as const) {
      const one = sidebarRowHeight("branch", density, "one-line");
      const two = sidebarRowHeight("branch", density, "two-line");
      const three = sidebarRowHeight("branch", density, "three-line");
      expect(two).toBeGreaterThan(one);
      expect(three).toBeGreaterThan(two);
    }
    // Compact must stay genuinely tighter than spacious in every layout.
    expect(BRANCH_ROW_HEIGHT_TWO_LINE.compact).toBeLessThan(BRANCH_ROW_HEIGHT_TWO_LINE.spacious);
    expect(BRANCH_ROW_HEIGHT_THREE_LINE.compact).toBeLessThan(BRANCH_ROW_HEIGHT_THREE_LINE.spacious);
  });

  it("gives three lines at least the per-line room of two", () => {
    for (const density of ["spacious", "compact"] as const) {
      expect(BRANCH_ROW_HEIGHT_THREE_LINE[density] / 3).toBeGreaterThanOrEqual(
        BRANCH_ROW_HEIGHT_TWO_LINE[density] / 2,
      );
    }
  });

  it("leaves room for three lines of real text", () => {
    // Three 13px lines and two 4px gaps is the 47px the harness measures. A
    // height that cannot hold it clips the row.
    for (const density of ["spacious", "compact"] as const) {
      expect(BRANCH_ROW_HEIGHT_THREE_LINE[density]).toBeGreaterThanOrEqual(47);
    }
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
    // whose markup is still several lines tall. It is the default's three.
    expect(sidebarRowHeight("branch", "spacious", "nonsense" as BranchRowLayout)).toBe(
      BRANCH_ROW_HEIGHT_THREE_LINE.spacious,
    );
    expect(sidebarRowHeight("branch", "compact", "nonsense" as BranchRowLayout)).toBe(
      BRANCH_ROW_HEIGHT_THREE_LINE.compact,
    );
    expect(
      sidebarRowHeight("branch", "nonsense" as SidebarDensity, "three-line"),
    ).toBe(BRANCH_ROW_HEIGHT_THREE_LINE.spacious);
  });
});

describe("branchRowLines", () => {
  it("counts the lines each layout draws", () => {
    expect(branchRowLines("one-line")).toBe(1);
    expect(branchRowLines("two-line")).toBe(2);
    expect(branchRowLines("three-line")).toBe(3);
  });

  it("reads an unknown layout as the default's three lines", () => {
    expect(branchRowLines("nonsense" as BranchRowLayout)).toBe(3);
    expect(branchRowLines(undefined as unknown as BranchRowLayout)).toBe(3);
  });

  it("agrees with sidebarRowHeight about which layouts are taller than a header", () => {
    for (const layout of BRANCH_ROW_LAYOUTS) {
      const grows = sidebarRowHeight("branch", "spacious", layout) > BRANCH_ROW_HEIGHT.spacious;
      expect(grows).toBe(branchRowLines(layout) > 1);
    }
  });
});

describe("isBranchRowLayout", () => {
  it("accepts exactly the advertised layouts", () => {
    for (const layout of BRANCH_ROW_LAYOUTS) {
      expect(isBranchRowLayout(layout)).toBe(true);
    }
    expect(isBranchRowLayout("three-line")).toBe(true);
    for (const bad of [
      "",
      "two",
      "three",
      "TWO-LINE",
      "THREE-LINE",
      "four-line",
      null,
      undefined,
      0,
      3,
      {},
      ["two-line"],
      ["three-line"],
    ]) {
      expect(isBranchRowLayout(bad)).toBe(false);
    }
  });

  it("lists the layouts roomiest first, with three lines the default", () => {
    expect([...BRANCH_ROW_LAYOUTS]).toEqual(["three-line", "two-line", "one-line"]);
    expect(DEFAULT_BRANCH_ROW_LAYOUT).toBe("three-line");
    expect(BRANCH_ROW_LAYOUTS[0]).toBe(DEFAULT_BRANCH_ROW_LAYOUT);
  });
});

describe("window math over mixed three-line rows", () => {
  // The sidebar's real shape under the default layout: a 30px header, then
  // branches at 62px, with a folder header and a tag in between.
  const kinds: SidebarRowKind[] = [
    "section-header",
    "branch",
    "branch",
    "folder-header",
    "branch",
    "branch",
    "branch",
    "section-header",
    "tag",
  ];
  const heights = kinds.map((kind) => sidebarRowHeight(kind, "spacious", "three-line"));
  const offsets = buildRowOffsets(heights);
  // tops: 0 30 92 154 184 246 308 370 400, total 430

  it("places each row by the sum of the real heights above it", () => {
    expect(heights).toEqual([30, 62, 62, 30, 62, 62, 62, 30, 30]);
    expect([...offsets]).toEqual([0, 30, 92, 154, 184, 246, 308, 370, 400, 430]);
    expect(totalRowHeight(offsets)).toBe(430);
  });

  it("is not what two-line heights would say about the same list", () => {
    const twoLine = buildRowOffsets(
      kinds.map((kind) => sidebarRowHeight(kind, "spacious", "two-line")),
    );
    expect(totalRowHeight(offsets)).toBeGreaterThan(totalRowHeight(twoLine));
    expect(rowTop(offsets, 5)).not.toBe(rowTop(twoLine, 5));
  });

  it("windows the rows that touch the viewport", () => {
    // Viewport [100, 260) touches row 2 (92–154), 3 (154–184), 4 (184–246) and 5 (246–308).
    expect(windowFromOffsets(100, 160, offsets, 0)).toEqual({ start: 2, end: 6 });
  });

  it("reveals a three-line row by its real edges", () => {
    // Row 5 spans 246–308; a 120px viewport at 0 needs top 188 to show it.
    expect(scrollOffsetToReveal(offsets, 5, 0, 120)).toBe(188);
    // Row 1 spans 30–92 and is already inside [0, 120).
    expect(scrollOffsetToReveal(offsets, 1, 0, 120)).toBeNull();
  });

  it("lands every row fully in view for every anchor", () => {
    const viewport = 140;
    for (let index = 0; index < rowCount(offsets); index++) {
      for (const anchor of [0, 33, 90, 200, 290, 400]) {
        const next = scrollOffsetToReveal(offsets, index, anchor, viewport) ?? anchor;
        expect(rowTop(offsets, index)).toBeGreaterThanOrEqual(next);
        expect(rowBottom(offsets, index)).toBeLessThanOrEqual(next + viewport);
      }
    }
  });

  it("parks a located row a third of the way down, from its own top", () => {
    // Row 4 starts at 184; a 90px viewport puts it 30px down, so scrollTop 154.
    expect(scrollOffsetToCenter(offsets, 4, 90)).toBe(154);
  });
});
