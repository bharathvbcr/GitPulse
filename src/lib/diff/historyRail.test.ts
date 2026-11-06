import { describe, expect, it } from "vitest";
import {
  HISTORY_ROW_HEIGHT,
  LOAD_MORE_WITHIN_ROWS,
  MAX_GUTTER_SHARE,
  MIN_GUTTER_WIDTH,
  RAIL_GRAPH_CONFIG,
  commitLabel,
  compareLabel,
  gutterLayout,
  indexOfCommit,
  resolveRowAction,
  sameCommit,
  shouldLoadMore,
  stepRow,
} from "./historyRail";

describe("rail graph geometry", () => {
  it("paints on the same pitch the rows are laid out at", () => {
    // A canvas row height that differs from the DOM row's drifts every node
    // off its row by the difference times the row index.
    expect(RAIL_GRAPH_CONFIG.rowHeight).toBe(HISTORY_ROW_HEIGHT);
  });
});

describe("gutterLayout", () => {
  it("takes what the graph needs when the rail has room", () => {
    expect(gutterLayout(60, 248)).toEqual({ width: 60, clipped: false });
  });

  it("caps a wide graph at its share of the rail and says it was cut", () => {
    const layout = gutterLayout(400, 248);
    expect(layout.width).toBe(Math.floor(248 * MAX_GUTTER_SHARE));
    expect(layout.clipped).toBe(true);
  });

  it("grows with the rail, so widening it is the way to see more lanes", () => {
    expect(gutterLayout(400, 520).width).toBeGreaterThan(gutterLayout(400, 248).width);
  });

  it("never collapses below one lane, whatever it is handed", () => {
    for (const [measured, rail] of [
      [0, 248],
      [Number.NaN, 248],
      [-5, 248],
      [60, 0],
      [60, Number.NaN],
    ]) {
      expect(gutterLayout(measured, rail).width, `${measured}/${rail}`).toBeGreaterThanOrEqual(MIN_GUTTER_WIDTH);
    }
  });
});

describe("resolveRowAction", () => {
  // Rows newest first: index 0 is the newest.
  const order = ["new", "mid", "old"];
  const orderOf = (id: string) => order.indexOf(id);

  it("opens a commit on a plain click", () => {
    expect(resolveRowAction("mid", null, false, orderOf)).toEqual({ kind: "open", commitId: "mid" });
  });

  it("arms a base on the compare gesture", () => {
    expect(resolveRowAction("mid", null, true, orderOf)).toEqual({ kind: "arm", commitId: "mid" });
  });

  it("disarms on the compare gesture over the armed commit itself", () => {
    expect(resolveRowAction("mid", "mid", true, orderOf)).toEqual({ kind: "disarm" });
  });

  it("opens the armed commit on a plain click of it, rather than comparing it with itself", () => {
    expect(resolveRowAction("mid", "mid", false, orderOf)).toEqual({ kind: "open", commitId: "mid" });
  });

  it("compares older..newer whichever was picked first", () => {
    expect(resolveRowAction("new", "old", false, orderOf)).toEqual({ kind: "compare", base: "old", target: "new" });
    expect(resolveRowAction("old", "new", false, orderOf)).toEqual({ kind: "compare", base: "old", target: "new" });
    // The compare gesture on the second pick compares too.
    expect(resolveRowAction("old", "new", true, orderOf)).toEqual({ kind: "compare", base: "old", target: "new" });
  });

  it("keeps the pick order when either commit is not in the graph", () => {
    expect(resolveRowAction("gone", "new", false, orderOf)).toEqual({ kind: "compare", base: "new", target: "gone" });
  });
});

describe("compareLabel", () => {
  it("spells the comparison the way git does", () => {
    expect(compareLabel("a1b2c3d4e5f6", "f6e5d4c3b2a1")).toBe("a1b2c3d..f6e5d4c");
  });
});

describe("stepRow", () => {
  it("moves one row with the arrows and clamps at both ends", () => {
    expect(stepRow(3, "ArrowDown", 10, 5)).toBe(4);
    expect(stepRow(3, "ArrowUp", 10, 5)).toBe(2);
    expect(stepRow(9, "ArrowDown", 10, 5)).toBe(9);
    expect(stepRow(0, "ArrowUp", 10, 5)).toBe(0);
  });

  it("starts from the top when nothing is under the cursor yet", () => {
    expect(stepRow(-1, "ArrowDown", 10, 5)).toBe(0);
    expect(stepRow(-1, "ArrowUp", 10, 5)).toBe(0);
  });

  it("pages a screenful and jumps to the ends", () => {
    expect(stepRow(2, "PageDown", 100, 8)).toBe(10);
    expect(stepRow(2, "PageUp", 100, 8)).toBe(0);
    expect(stepRow(40, "Home", 100, 8)).toBe(0);
    expect(stepRow(40, "End", 100, 8)).toBe(99);
  });

  it("ignores keys that are not steps, and an empty list", () => {
    expect(stepRow(3, "Enter", 10, 5)).toBeNull();
    expect(stepRow(3, "ArrowDown", 0, 5)).toBeNull();
  });

  it("clamps a cursor left past a list that shrank", () => {
    expect(stepRow(50, "ArrowUp", 10, 5)).toBe(8);
  });
});

describe("shouldLoadMore", () => {
  const base = { viewportHeight: 10 * 26, total: 100, rowHeight: 26, hasMore: true, loading: false };

  it("asks for older history before the reader reaches the end", () => {
    const nearEnd = (100 - LOAD_MORE_WITHIN_ROWS) * 26 - base.viewportHeight;
    expect(shouldLoadMore({ ...base, scrollTop: nearEnd })).toBe(true);
    expect(shouldLoadMore({ ...base, scrollTop: 0 })).toBe(false);
  });

  it("does not ask while a load is out, or when nothing older exists", () => {
    const atEnd = 100 * 26;
    expect(shouldLoadMore({ ...base, scrollTop: atEnd, loading: true })).toBe(false);
    expect(shouldLoadMore({ ...base, scrollTop: atEnd, hasMore: false })).toBe(false);
    expect(shouldLoadMore({ ...base, scrollTop: atEnd, total: 0 })).toBe(false);
  });

  it("asks at once when the whole loaded history fits on screen", () => {
    expect(shouldLoadMore({ ...base, total: 5, scrollTop: 0 })).toBe(true);
  });
});

describe("indexOfCommit / sameCommit", () => {
  const ids = ["a1b2c3d4e5f6", "b2c3d4e5f6a1"];

  it("matches an abbreviated id against a full one, in either direction", () => {
    expect(indexOfCommit(ids, "b2c3d4e")).toBe(1);
    expect(sameCommit("a1b2c3d", "a1b2c3d4e5f6")).toBe(true);
  });

  it("refuses a prefix too short to mean anything", () => {
    expect(indexOfCommit(ids, "a1b")).toBe(-1);
    expect(sameCommit("a1b2c3d4e5f6", "a")).toBe(false);
  });

  it("finds nothing for no selection or a different commit", () => {
    expect(indexOfCommit(ids, null)).toBe(-1);
    expect(indexOfCommit(ids, "ffffffffffff")).toBe(-1);
    expect(sameCommit("a1b2c3d4e5f6", "")).toBe(false);
  });
});

describe("commitLabel", () => {
  it("uses the summary, and names an empty message rather than rendering a blank row", () => {
    expect(commitLabel("Add parser")).toBe("Add parser");
    for (const summary of ["", "   ", "\n"]) expect(commitLabel(summary)).toBe("(no commit message)");
  });
});
