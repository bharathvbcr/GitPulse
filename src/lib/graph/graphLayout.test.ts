import { describe, expect, it } from "vitest";
import {
  MAX_GRAPH_CONTENT_WIDTH,
  clampGraphScrollLeft,
  graphContentBoxStyle,
  graphViewportBoxStyle,
  resolveGraphLayout,
  resolveGraphOverflow,
} from "./graphLayout";

describe("resolveGraphLayout", () => {
  it("caps a repository-wide graph before it can starve commit rows", () => {
    const layout = resolveGraphLayout({
      measuredLaneWidth: 1_900,
      avatarSlotWidth: 28,
      availableWidth: 1_840,
      widthMode: "balanced",
    });

    expect(layout.contentWidth).toBe(1_928);
    expect(layout.viewportWidth).toBe(440);
    expect(layout.remainingWidth).toBe(1_400);
    expect(layout.isHorizontallyScrollable).toBe(true);
  });

  it("keeps a hard commit-row reserve even in the Full graph-width mode", () => {
    const layout = resolveGraphLayout({
      measuredLaneWidth: 2_400,
      avatarSlotWidth: 0,
      availableWidth: 1_840,
      widthMode: "full",
    });

    expect(layout.viewportWidth).toBe(1_360);
    expect(layout.remainingWidth).toBe(480);
  });

  it("uses the packed lane width for ordinary repositories instead of a 220px pad", () => {
    const tight = resolveGraphLayout({
      measuredLaneWidth: 72,
      avatarSlotWidth: 24,
      availableWidth: 1_200,
      widthMode: "wide",
    });
    expect(tight).toMatchObject({
      contentWidth: 96,
      viewportWidth: 96,
      remainingWidth: 1_104,
      isHorizontallyScrollable: false,
    });

    const wider = resolveGraphLayout({
      measuredLaneWidth: 260,
      avatarSlotWidth: 24,
      availableWidth: 1_200,
      widthMode: "wide",
    });
    expect(wider).toMatchObject({
      contentWidth: 284,
      viewportWidth: 284,
      remainingWidth: 916,
      isHorizontallyScrollable: false,
    });
  });

  it("stays finite and bounded for corrupt or extreme measurements", () => {
    const nonFinite = resolveGraphLayout({
      measuredLaneWidth: Number.POSITIVE_INFINITY,
      avatarSlotWidth: Number.NaN,
      availableWidth: 1_000,
      widthMode: "balanced",
    });
    const extreme = resolveGraphLayout({
      measuredLaneWidth: Number.MAX_VALUE,
      avatarSlotWidth: 50,
      availableWidth: 1_000,
      widthMode: "full",
    });

    expect(nonFinite.contentWidth).toBe(220);
    expect(Number.isFinite(nonFinite.viewportWidth)).toBe(true);
    expect(extreme.contentWidth).toBe(MAX_GRAPH_CONTENT_WIDTH);
    expect(extreme.viewportWidth).toBeLessThan(extreme.availableWidth);

    const invalidMode = resolveGraphLayout({
      measuredLaneWidth: 900,
      avatarSlotWidth: 0,
      availableWidth: 1_000,
      widthMode: "unexpected" as never,
    });
    expect(invalidMode.viewportWidth).toBe(440);
  });

  it("degrades proportionally in a narrow pane without overflowing it", () => {
    const layout = resolveGraphLayout({
      measuredLaneWidth: 900,
      avatarSlotWidth: 0,
      availableWidth: 320,
      widthMode: "full",
    });

    expect(layout.viewportWidth).toBe(144);
    expect(layout.remainingWidth).toBe(176);
    expect(layout.viewportWidth + layout.remainingWidth).toBe(320);
  });
});

describe("graph viewport overflow geometry", () => {
  it("locks the flex item so content width cannot inflate the gutter", () => {
    expect(graphViewportBoxStyle(440)).toBe(
      "width:440px;max-width:440px;min-width:0px;flex-basis:440px;",
    );
    expect(graphContentBoxStyle(1_928)).toBe(
      "width:1928px;min-width:1928px;height:100%;",
    );
  });

  it("stays finite for corrupt measurements", () => {
    expect(graphViewportBoxStyle(Number.NaN)).toContain("width:0px");
    expect(graphContentBoxStyle(Number.POSITIVE_INFINITY)).toContain("width:0px");
  });

  it("fades only the side that still has hidden lanes", () => {
    expect(resolveGraphOverflow(0, 440, 1_200)).toEqual({
      canScroll: true,
      showStartFade: false,
      showEndFade: true,
    });
    expect(resolveGraphOverflow(400, 440, 1_200)).toEqual({
      canScroll: true,
      showStartFade: true,
      showEndFade: true,
    });
    expect(resolveGraphOverflow(760, 440, 1_200)).toEqual({
      canScroll: true,
      showStartFade: true,
      showEndFade: false,
    });
    expect(resolveGraphOverflow(0, 440, 440)).toEqual({
      canScroll: false,
      showStartFade: false,
      showEndFade: false,
    });
  });

  it("clamps a stale pan when the graph shrinks", () => {
    expect(clampGraphScrollLeft(900, 440, 1_200)).toBe(760);
    expect(clampGraphScrollLeft(-12, 440, 1_200)).toBe(0);
    expect(clampGraphScrollLeft(Number.NaN, 440, 200)).toBe(0);
  });
});
