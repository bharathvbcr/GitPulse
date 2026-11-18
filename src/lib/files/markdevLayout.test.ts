import { describe, expect, it } from "vitest";
import {
  MARKDEV_OUTLINE_FOOTPRINT,
  MARKDEV_OUTLINE_RAIL_MIN_WIDTH,
  MARKDEV_SPLIT_HORIZONTAL_MIN_WIDTH,
  MARKDEV_TOOLBAR_INLINE_MIN_WIDTH,
  resolveMarkDevLayout,
} from "./markdevLayout";

describe("resolveMarkDevLayout", () => {
  it("uses horizontal split and rail outline in wide viewports", () => {
    expect(
      resolveMarkDevLayout({
        width: 900,
        outlineRequested: false,
        hasOutline: true,
      }),
    ).toEqual({
      mode: "wide",
      outlineVisible: false,
      outlineLayout: "rail",
      contentWidth: 900,
      splitDirection: "horizontal",
      toolbar: "inline",
    });

    expect(
      resolveMarkDevLayout({
        width: 900,
        outlineRequested: true,
        hasOutline: true,
      }),
    ).toEqual({
      mode: "wide",
      outlineVisible: true,
      outlineLayout: "rail",
      contentWidth: 900 - MARKDEV_OUTLINE_FOOTPRINT,
      splitDirection: "horizontal",
      toolbar: "inline",
    });
  });

  it("switches split direction at the horizontal minimum width boundary", () => {
    expect(
      resolveMarkDevLayout({
        width: MARKDEV_SPLIT_HORIZONTAL_MIN_WIDTH - 1,
        outlineRequested: false,
        hasOutline: false,
      }).splitDirection,
    ).toBe("vertical");

    expect(
      resolveMarkDevLayout({
        width: MARKDEV_SPLIT_HORIZONTAL_MIN_WIDTH,
        outlineRequested: false,
        hasOutline: false,
      }).splitDirection,
    ).toBe("horizontal");
  });

  it("deducts the outline rail footprint before choosing split direction", () => {
    const withOutlineAt700 = resolveMarkDevLayout({
      width: 700,
      outlineRequested: true,
      hasOutline: true,
    });
    expect(withOutlineAt700.outlineLayout).toBe("rail");
    expect(withOutlineAt700.contentWidth).toBe(700 - MARKDEV_OUTLINE_FOOTPRINT);
    expect(withOutlineAt700.splitDirection).toBe("vertical");
    expect(withOutlineAt700.mode).toBe("compact");

    const withOutlineAt900 = resolveMarkDevLayout({
      width: 900,
      outlineRequested: true,
      hasOutline: true,
    });
    expect(withOutlineAt900.outlineLayout).toBe("rail");
    expect(withOutlineAt900.contentWidth).toBe(900 - MARKDEV_OUTLINE_FOOTPRINT);
    expect(withOutlineAt900.splitDirection).toBe("horizontal");
    expect(withOutlineAt900.mode).toBe("wide");
  });

  it("uses a top sheet outline and vertical split in narrow sidebar or terminal panes", () => {
    expect(
      resolveMarkDevLayout({
        width: 340,
        outlineRequested: true,
        hasOutline: true,
      }),
    ).toEqual({
      mode: "compact",
      outlineVisible: true,
      outlineLayout: "sheet",
      contentWidth: 340,
      splitDirection: "vertical",
      toolbar: "stacked",
    });

    expect(
      resolveMarkDevLayout({
        width: MARKDEV_OUTLINE_RAIL_MIN_WIDTH - 1,
        outlineRequested: true,
        hasOutline: true,
      }).outlineLayout,
    ).toBe("sheet");

    expect(
      resolveMarkDevLayout({
        width: MARKDEV_OUTLINE_RAIL_MIN_WIDTH,
        outlineRequested: true,
        hasOutline: true,
      }).outlineLayout,
    ).toBe("rail");
  });

  it("fails safely to unmeasured layout on invalid or zero widths", () => {
    for (const width of [0, -1, Number.NaN, Number.POSITIVE_INFINITY]) {
      expect(
        resolveMarkDevLayout({
          width,
          outlineRequested: true,
          hasOutline: true,
        }),
      ).toEqual({
        mode: "unmeasured",
        outlineVisible: true,
        outlineLayout: "rail",
        contentWidth: 0,
        splitDirection: "horizontal",
        toolbar: "inline",
      });
    }
  });

  it("gives the mode switcher its own row below the inline toolbar minimum", () => {
    const at = (width: number) =>
      resolveMarkDevLayout({ width, outlineRequested: true, hasOutline: true }).toolbar;
    expect(at(MARKDEV_TOOLBAR_INLINE_MIN_WIDTH - 1)).toBe("stacked");
    expect(at(MARKDEV_TOOLBAR_INLINE_MIN_WIDTH)).toBe("inline");
    // A terminal split with its explorer rail open leaves the viewer ~170px.
    expect(at(170)).toBe("stacked");
    // The decision follows the whole viewer, not the width left beside a side outline.
    expect(at(MARKDEV_OUTLINE_RAIL_MIN_WIDTH)).toBe("inline");
  });
});
