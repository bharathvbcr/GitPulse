import { describe, expect, it } from "bun:test";
import { buildAreaTree } from "../src/areaTree";
import {
  ancestorKeys,
  neighborRowKey,
  NO_KEYS,
  subsystemNodeId,
  treeKeyOutcome,
  visibleAreaRows,
} from "../src/areaTreeNav";
import type { AreaSource } from "../src/types";

function sub(area: string): AreaSource {
  return {
    area, summary: `${area}: 1 file`,
  };
}

// Nested areas, an exact duplicate (src/api twice), and two areas that share a
// basename (app/utils and lib/utils). `crates/a` and `crates/b` sit under a
// folder that is not itself an area, so it is a group row.
const SUBS = [
  sub("src"),
  sub("src/api"),
  sub("src/api"),
  sub("app/utils"),
  sub("lib/utils"),
  sub("crates/a"),
  sub("crates/b"),
];
const ROWS = buildAreaTree(SUBS);
const keys = (rows: { key: string }[]) => rows.map((r) => r.key);

describe("visibleAreaRows", () => {
  it("shows every row when nothing is collapsed and there is no filter", () => {
    expect(keys(visibleAreaRows(ROWS, NO_KEYS, ""))).toEqual(keys(ROWS));
    expect(keys(ROWS)).toEqual([
      "area:app/utils",
      "group:crates",
      "area:crates/a",
      "area:crates/b",
      "area:lib/utils",
      "area:src",
      "area:src/api",
    ]);
  });

  it("hides the descendants of a collapsed row, at any depth", () => {
    expect(keys(visibleAreaRows(ROWS, new Set(["area:src"]), ""))).not.toContain("area:src/api");
    expect(keys(visibleAreaRows(ROWS, new Set(["group:crates"]), ""))).toEqual([
      "area:app/utils",
      "group:crates",
      "area:lib/utils",
      "area:src",
      "area:src/api",
    ]);
  });

  it("filters by area substring and keeps the ancestors of a match", () => {
    expect(keys(visibleAreaRows(ROWS, NO_KEYS, "api"))).toEqual(["area:src", "area:src/api"]);
    expect(keys(visibleAreaRows(ROWS, NO_KEYS, "CRATES/B"))).toEqual(["group:crates", "area:crates/b"]);
    expect(visibleAreaRows(ROWS, NO_KEYS, "nothing-like-this")).toEqual([]);
  });

  it("ignores collapsed rows while filtering so a match is never folded away", () => {
    expect(keys(visibleAreaRows(ROWS, new Set(["area:src"]), "api"))).toEqual(["area:src", "area:src/api"]);
  });

  it("treats a whitespace-only filter as no filter", () => {
    expect(keys(visibleAreaRows(ROWS, NO_KEYS, "  "))).toEqual(keys(ROWS));
  });
});

describe("ancestorKeys", () => {
  it("lists parents nearest first", () => {
    expect(ancestorKeys(ROWS, "area:src/api")).toEqual(["area:src"]);
    expect(ancestorKeys(ROWS, "area:src")).toEqual([]);
    expect(ancestorKeys(ROWS, "no-such-row")).toEqual([]);
  });
});

describe("treeKeyOutcome", () => {
  const all = visibleAreaRows(ROWS, NO_KEYS, "");
  const press = (key: string, focus: string, collapsed: ReadonlySet<string> = NO_KEYS) =>
    treeKeyOutcome(key, visibleAreaRows(ROWS, collapsed, ""), focus, collapsed);

  it("moves focus with Up, Down, Home and End and clamps at both ends", () => {
    expect(press("ArrowDown", "group:crates")).toEqual({ focusKey: "area:crates/a" });
    expect(press("ArrowUp", "area:crates/a")).toEqual({ focusKey: "group:crates" });
    expect(press("ArrowUp", all[0].key)).toEqual({ focusKey: all[0].key });
    expect(press("ArrowDown", "area:src/api")).toEqual({ focusKey: "area:src/api" });
    expect(press("Home", "area:src")).toEqual({ focusKey: all[0].key });
    expect(press("End", "area:src")).toEqual({ focusKey: "area:src/api" });
  });

  it("Left collapses an open row, then moves to the parent", () => {
    expect(press("ArrowLeft", "area:src")).toEqual({ focusKey: "area:src", collapsed: new Set(["area:src"]) });
    expect(press("ArrowLeft", "area:src/api")).toEqual({ focusKey: "area:src" });
    // A top-level leaf has no parent: nothing moves.
    expect(press("ArrowLeft", "area:app/utils")).toEqual({ focusKey: "area:app/utils" });
  });

  it("Right expands a collapsed row, then moves to the first child", () => {
    const folded = new Set(["area:src"]);
    expect(press("ArrowRight", "area:src", folded)).toEqual({ focusKey: "area:src", collapsed: new Set() });
    expect(press("ArrowRight", "area:src")).toEqual({ focusKey: "area:src/api" });
    expect(press("ArrowRight", "group:crates")).toEqual({ focusKey: "area:crates/a" });
    // A leaf has no child to go to.
    expect(press("ArrowRight", "area:src/api")).toEqual({ focusKey: "area:src/api" });
  });

  it("Enter and Space select an area row and toggle a group row", () => {
    expect(press("Enter", "area:src/api")).toEqual({ focusKey: "area:src/api", select: "area:src/api" });
    expect(press(" ", "area:src")).toEqual({ focusKey: "area:src", select: "area:src" });
    expect(press("Enter", "group:crates")).toEqual({ focusKey: "group:crates", collapsed: new Set(["group:crates"]) });
    expect(press("Enter", "area:app/utils")).toEqual({ focusKey: "area:app/utils", select: "area:app/utils" });
  });

  it("leaves other keys alone, and a focus that is not visible", () => {
    expect(press("a", "area:src")).toBeNull();
    expect(press("Tab", "area:src")).toBeNull();
    expect(press("ArrowDown", "group:gone")).toBeNull();
  });
});

describe("neighborRowKey", () => {
  it("matches a neighbor to a selectable row by normalized area", () => {
    expect(neighborRowKey(ROWS, "src/api")).toBe("area:src/api");
    expect(neighborRowKey(ROWS, "./src/api/")).toBe("area:src/api");
    expect(neighborRowKey(ROWS, "src\\api")).toBe("area:src/api");
  });

  it("does not resolve a group row, an unknown area or an empty name", () => {
    expect(neighborRowKey(ROWS, "crates")).toBeNull();
    expect(neighborRowKey(ROWS, "elsewhere")).toBeNull();
    expect(neighborRowKey(ROWS, "")).toBeNull();
  });
});

describe("subsystemNodeId", () => {
  it("is the producer's area with backslashes as slashes, as build_preview_payload ids it", () => {
    expect(subsystemNodeId("src/api")).toBe("src/api");
    expect(subsystemNodeId("src\\api")).toBe("src/api");
    // Not normalizeArea: the payload keeps "./" and trailing slashes.
    expect(subsystemNodeId("./src/")).toBe("./src/");
  });
});
