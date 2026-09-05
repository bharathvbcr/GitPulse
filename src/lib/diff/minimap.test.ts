import { describe, expect, it } from "vitest";
import {
  buildTicks,
  fileMarks,
  maxScroll,
  ratioFromPointer,
  scrollForRatio,
  scrollForRow,
  viewportBand,
} from "./minimap";
import { TONE_ADD, TONE_CTX, TONE_DEL, TONE_FILE, TONE_HUNK, TONE_MOD } from "./rowModel";

const tones = (...values: number[]) => new Uint8Array(values);

describe("buildTicks", () => {
  it("draws nothing for a run of pure context", () => {
    expect(buildTicks(tones(TONE_CTX, TONE_CTX, TONE_CTX))).toEqual([]);
  });

  it("calls a bucket holding both sides a modification, not growth", () => {
    // The map claiming a rewrite was pure growth is the failure this guards.
    const ticks = buildTicks(tones(TONE_ADD, TONE_DEL), 1);
    expect(ticks).toHaveLength(1);
    expect(ticks[0].tone).toBe(TONE_MOD);
  });

  it("keeps a one-sided bucket on its own side", () => {
    expect(buildTicks(tones(TONE_ADD, TONE_ADD), 1)[0].tone).toBe(TONE_ADD);
    expect(buildTicks(tones(TONE_DEL, TONE_DEL), 1)[0].tone).toBe(TONE_DEL);
  });

  it("prefers a change over the file and hunk markers sharing its bucket", () => {
    expect(buildTicks(tones(TONE_FILE, TONE_HUNK, TONE_ADD), 1)[0].tone).toBe(TONE_ADD);
  });

  it("still marks a bucket that holds only structure", () => {
    expect(buildTicks(tones(TONE_FILE, TONE_CTX), 1)[0].tone).toBe(TONE_FILE);
    expect(buildTicks(tones(TONE_HUNK, TONE_CTX), 1)[0].tone).toBe(TONE_HUNK);
  });

  it("positions ticks as a fraction of the list it was given", () => {
    const ticks = buildTicks(tones(TONE_ADD, TONE_CTX, TONE_CTX, TONE_DEL), 4);
    expect(ticks.map((tick) => Math.round(tick.topPct))).toEqual([0, 75]);
  });

  it("never exceeds the tick budget", () => {
    const many = new Uint8Array(100_000).fill(TONE_ADD);
    expect(buildTicks(many, 80).length).toBeLessThanOrEqual(80);
  });

  it("gives a single-line diff a tick tall enough to see", () => {
    const [tick] = buildTicks(tones(TONE_ADD), 160);
    expect(tick.heightPct).toBeGreaterThan(0);
  });

  it("keeps a lone change visible inside a huge diff", () => {
    const many = new Uint8Array(50_000).fill(TONE_CTX);
    many[25_000] = TONE_ADD;
    const ticks = buildTicks(many);
    expect(ticks).toHaveLength(1);
    expect(ticks[0].heightPct).toBeGreaterThanOrEqual(0.9);
  });

  it("returns nothing for an empty list or a zero budget", () => {
    expect(buildTicks(new Uint8Array(0))).toEqual([]);
    expect(buildTicks(tones(TONE_ADD), 0)).toEqual([]);
  });

  it("gives every tick a distinct key", () => {
    const ticks = buildTicks(new Uint8Array(500).fill(TONE_ADD), 50);
    expect(new Set(ticks.map((tick) => tick.key)).size).toBe(ticks.length);
  });
});

describe("scroll mapping", () => {
  // 1,000 rows of 20px in a 400px viewport: 20,000px of content, 19,600 of scroll.
  const ROWS = 1_000;
  const H = 20;
  const VIEW = 400;

  it("centres the row you aimed at instead of putting it at the top", () => {
    // Top-aligning is why clicking the last tick used to scroll past the end
    // and clamp, leaving the thing you aimed at off screen.
    expect(scrollForRatio(0.5, ROWS, H, VIEW)).toBe(0.5 * ROWS * H - VIEW / 2);
  });

  it("shows the end of the diff when you click the bottom of the strip", () => {
    const bottom = scrollForRatio(1, ROWS, H, VIEW);
    expect(bottom).toBe(maxScroll(ROWS, H, VIEW));
    // And the last row really is inside the viewport at that offset.
    expect(bottom + VIEW).toBeGreaterThanOrEqual(ROWS * H);
  });

  it("never scrolls above the start", () => {
    expect(scrollForRatio(0, ROWS, H, VIEW)).toBe(0);
    expect(scrollForRatio(0.005, ROWS, H, VIEW)).toBe(0);
  });

  it("is calibrated to the row count it is given, not to some other list", () => {
    // The regression: split view was scrolled with the UNIFIED line count, so
    // every position was off by the ratio between the two lists.
    const unified = scrollForRatio(0.5, 4_000, H, VIEW);
    const split = scrollForRatio(0.5, 2_000, H, VIEW);
    expect(unified).not.toBe(split);
    expect(split).toBe(0.5 * 2_000 * H - VIEW / 2);
  });

  it("clamps a ratio outside 0..1 rather than scrolling into nothing", () => {
    expect(scrollForRatio(-3, ROWS, H, VIEW)).toBe(0);
    expect(scrollForRatio(9, ROWS, H, VIEW)).toBe(maxScroll(ROWS, H, VIEW));
  });

  it("survives NaN and Infinity from a degenerate measurement", () => {
    expect(scrollForRatio(Number.NaN, ROWS, H, VIEW)).toBe(0);
    expect(scrollForRatio(0.5, ROWS, H, Number.NaN)).toBe(0.5 * ROWS * H);
    expect(Number.isFinite(scrollForRatio(0.5, ROWS, H, Number.POSITIVE_INFINITY))).toBe(true);
    expect(maxScroll(Number.NaN, H, VIEW)).toBe(0);
  });

  it("is zero when the whole diff already fits", () => {
    expect(maxScroll(5, H, VIEW)).toBe(0);
    expect(scrollForRatio(1, 5, H, VIEW)).toBe(0);
  });
});

describe("scrollForRow", () => {
  it("centres one row", () => {
    expect(scrollForRow(500, 1_000, 20, 400)).toBe(500 * 20 - 400 / 2 + 10);
  });

  it("clamps to the ends rather than overscrolling", () => {
    expect(scrollForRow(0, 1_000, 20, 400)).toBe(0);
    expect(scrollForRow(999, 1_000, 20, 400)).toBe(maxScroll(1_000, 20, 400));
    expect(scrollForRow(-5, 1_000, 20, 400)).toBe(0);
    expect(scrollForRow(9_999, 1_000, 20, 400)).toBe(maxScroll(1_000, 20, 400));
  });

  it("is zero for an empty list", () => {
    expect(scrollForRow(3, 0, 20, 400)).toBe(0);
  });
});

describe("viewportBand", () => {
  it("marks where the reader is, as a share of the whole diff", () => {
    const band = viewportBand(4_000, 400, 1_000, 20);
    expect(band).not.toBeNull();
    expect(band?.topPct).toBeCloseTo(20);
    expect(band?.heightPct).toBeCloseTo(2);
  });

  it("draws nothing when the whole diff is already on screen", () => {
    expect(viewportBand(0, 400, 5, 20)).toBeNull();
    expect(viewportBand(0, 0, 1_000, 20)).toBeNull();
    expect(viewportBand(0, 400, 0, 20)).toBeNull();
  });

  it("clamps a scroll position past the end", () => {
    const band = viewportBand(999_999, 400, 1_000, 20);
    expect(band?.topPct).toBeCloseTo(98);
  });

  it("treats a bogus scroll position as the top", () => {
    expect(viewportBand(Number.NaN, 400, 1_000, 20)?.topPct).toBe(0);
  });
});

describe("ratioFromPointer", () => {
  it("maps a click to its share of the strip", () => {
    expect(ratioFromPointer(150, 100, 200)).toBeCloseTo(0.25);
  });

  it("clamps outside the strip instead of returning a wild ratio", () => {
    expect(ratioFromPointer(0, 100, 200)).toBe(0);
    expect(ratioFromPointer(999, 100, 200)).toBe(1);
  });

  it("is zero for a strip with no height", () => {
    expect(ratioFromPointer(50, 0, 0)).toBe(0);
    expect(ratioFromPointer(50, 0, Number.NaN)).toBe(0);
  });
});

describe("fileMarks", () => {
  it("marks each file boundary as a share of the strip", () => {
    expect(fileMarks([0, 250, 500], 1_000)).toEqual([25, 50]);
  });

  it("drops the mark at the very top, which is the strip's own edge", () => {
    expect(fileMarks([0], 100)).toEqual([]);
  });

  it("projects through a row mapping for the split list", () => {
    // In split view a source line index is not a row index.
    expect(fileMarks([100], 100, (line) => line / 2)).toEqual([50]);
  });

  it("ignores a mapping that answers with nothing", () => {
    expect(fileMarks([100], 100, () => -1)).toEqual([]);
    expect(fileMarks([100], 100, () => Number.NaN)).toEqual([]);
  });

  it("is empty for an empty list", () => {
    expect(fileMarks([5], 0)).toEqual([]);
  });
});
