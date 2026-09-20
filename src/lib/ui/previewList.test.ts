import { describe, expect, it } from "vitest";
import { TIMELINE_PREVIEW_COUNT } from "../delivery/timeline";
import { RUN_PREVIEW_COUNT, WORKFLOW_PREVIEW_COUNT } from "../github/runActions";
import {
  expandLabel,
  overflowsPreview,
  previewCap,
  previewSlice,
} from "./previewList";

const list = (n: number) => Array.from({ length: n }, (_, i) => `row-${i}`);

const COUNTS = [WORKFLOW_PREVIEW_COUNT, RUN_PREVIEW_COUNT, TIMELINE_PREVIEW_COUNT];

describe("previewCap", () => {
  it("floors a usable count and refuses to collapse a list to nothing", () => {
    expect(previewCap(3)).toBe(3);
    expect(previewCap(5.9)).toBe(5);
    expect(previewCap(0)).toBe(1);
    expect(previewCap(-4)).toBe(1);
    expect(previewCap(Number.NaN)).toBe(1);
    expect(previewCap(Number.POSITIVE_INFINITY)).toBe(1);
    expect(previewCap(Number.NEGATIVE_INFINITY)).toBe(1);
  });
});

describe("previewSlice", () => {
  it("caps the collapsed list at the preview count and expands to all", () => {
    for (const count of COUNTS) {
      expect(previewSlice(list(50), false, count)).toHaveLength(count);
      expect(previewSlice(list(50), true, count)).toHaveLength(50);
    }
  });

  it("keeps the first rows, so expanding only ever appends", () => {
    const all = list(12);
    const collapsed = previewSlice(all, false, TIMELINE_PREVIEW_COUNT);
    expect(collapsed).toEqual(all.slice(0, collapsed.length));
    expect(previewSlice(all, true, TIMELINE_PREVIEW_COUNT).slice(0, collapsed.length)).toEqual(
      collapsed,
    );
  });

  it("never renders an empty list for a repository that has rows", () => {
    for (const n of [1, 2, 3, 4, 5, 6, 20, 50]) {
      for (const count of [...COUNTS, 0, -1, Number.NaN, Number.POSITIVE_INFINITY]) {
        expect(previewSlice(list(n), false, count).length).toBeGreaterThan(0);
        expect(previewSlice(list(n), true, count).length).toBeGreaterThan(0);
      }
    }
  });

  it("shows every row, collapsed or not, when there are few", () => {
    const all = list(TIMELINE_PREVIEW_COUNT);
    expect(previewSlice(all, false, TIMELINE_PREVIEW_COUNT)).toEqual(all);
  });

  it("leaves an empty list empty rather than inventing a row", () => {
    expect(previewSlice([], false, TIMELINE_PREVIEW_COUNT)).toEqual([]);
    expect(previewSlice([], true, TIMELINE_PREVIEW_COUNT)).toEqual([]);
  });

  it("does not alias the caller's array", () => {
    const all = list(3);
    const expanded = previewSlice(all, true, TIMELINE_PREVIEW_COUNT);
    expanded.push("mutated");
    expect(all).toHaveLength(3);
    const collapsed = previewSlice(all, false, TIMELINE_PREVIEW_COUNT);
    collapsed.push("mutated");
    expect(all).toHaveLength(3);
  });
});

describe("overflowsPreview", () => {
  it("offers the expander only when rows are actually hidden", () => {
    expect(overflowsPreview(TIMELINE_PREVIEW_COUNT, TIMELINE_PREVIEW_COUNT)).toBe(false);
    expect(overflowsPreview(TIMELINE_PREVIEW_COUNT + 1, TIMELINE_PREVIEW_COUNT)).toBe(true);
    expect(overflowsPreview(0, TIMELINE_PREVIEW_COUNT)).toBe(false);
  });

  it("agrees with previewSlice about whether anything is hidden", () => {
    const counts = [...COUNTS, 0, -1, 1, 8, Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY];
    for (const count of counts) {
      for (let n = 0; n <= 25; n += 1) {
        const hidden = n - previewSlice(Array.from({ length: n }), false, count).length;
        expect(overflowsPreview(n, count), `n=${n} count=${String(count)}`).toBe(hidden > 0);
      }
    }
  });

  it("does not treat a garbage total as overflow", () => {
    expect(overflowsPreview(Number.NaN, 3)).toBe(false);
    expect(overflowsPreview(Number.POSITIVE_INFINITY, 3)).toBe(false);
    expect(overflowsPreview(-8, 3)).toBe(false);
  });
});

describe("expandLabel", () => {
  it("names the count to reveal, and offers the way back once expanded", () => {
    expect(expandLabel(23, false, "workflows")).toBe("Show all 23 workflows");
    expect(expandLabel(20, false, "runs")).toBe("Show all 20 runs");
    expect(expandLabel(23, true, "workflows")).toBe("Show fewer");
  });

  it("groups large counts the way the rest of the panel does", () => {
    expect(expandLabel(1234, false, "workflows")).toBe("Show all 1,234 workflows");
  });

  it("does not print NaN or a blank noun", () => {
    expect(expandLabel(Number.NaN, false, "runs")).toBe("Show all 0 runs");
    expect(expandLabel(20, false, "")).toBe("Show all 20 items");
    expect(expandLabel(20, false, "   ")).toBe("Show all 20 items");
  });
});
