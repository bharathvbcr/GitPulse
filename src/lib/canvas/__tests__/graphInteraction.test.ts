import { describe, expect, it } from "vitest";
import {
  applyGraphGutterWheel,
  canvasPointFromClient,
  forwardGraphWheel,
  graphDragScrollLeft,
  isGraphPanGesture,
  normalizeWheelDelta,
  panGraphHorizontally,
  positionGraphTooltip,
} from "../graphInteraction";

describe("graph interactions", () => {
  it("normalizes pixel, line, and page wheel deltas for the shared scroller", () => {
    expect(normalizeWheelDelta(24, 0, 36, 600)).toBe(24);
    expect(normalizeWheelDelta(3, 1, 36, 600)).toBe(108);
    expect(normalizeWheelDelta(-1, 2, 36, 600)).toBe(-600);
    expect(normalizeWheelDelta(Number.NaN, 0, 36, 600)).toBe(0);
  });

  it("forwards branch-canvas wheel input to the commit-list scroller", () => {
    const scroller = { scrollTop: 100, scrollHeight: 1_000, clientHeight: 400 };

    expect(forwardGraphWheel(scroller, 2, 1, 36)).toBe(true);
    expect(scroller.scrollTop).toBe(172);

    expect(forwardGraphWheel(scroller, 10, 2, 36)).toBe(true);
    expect(scroller.scrollTop).toBe(600);
    expect(forwardGraphWheel(scroller, 1, 0, 36)).toBe(false);
    expect(scroller.scrollTop).toBe(600);
  });

  it("keeps a node tooltip inside the graph viewport", () => {
    expect(positionGraphTooltip(20, 20, 800, 500, 320, 160)).toEqual({
      left: 32,
      top: 32,
      placement: "below",
      anchorX: 16,
    });

    expect(positionGraphTooltip(790, 490, 800, 500, 320, 160)).toEqual({
      left: 472,
      top: 318,
      placement: "above",
      // Horizontal clamping shoved the box left of the pointer; the caret
      // rides toward the pointer but stays INSIDE the box: raw offset
      // 790-472=318 clamps to width-16=304.
      anchorX: 304,
    });
  });

  it("handles viewports narrower than the preferred tooltip width", () => {
    expect(positionGraphTooltip(4, 100, 240, 300, 320, 160).left).toBe(8);
  });
});

describe("canvasPointFromClient", () => {
  it("maps a pointer onto lane 0 when the gutter is not scrolled", () => {
    expect(canvasPointFromClient(120, 68, { left: 100, top: 50, scrollLeft: 0 })).toEqual({
      x: 20,
      y: 18,
    });
  });

  it("adds gutter scrollLeft so a horizontally panned branch node stays hittable", () => {
    // Viewport at x=100, panned 200px. A later-lane node painted at content
    // x=228 sits at client x=128; forgetting scrollLeft would look at x=28
    // and miss (the cached-canvas-rect bug).
    expect(canvasPointFromClient(128, 68, { left: 100, top: 50, scrollLeft: 200 })).toEqual({
      x: 228,
      y: 18,
    });
  });

  it("stays finite for NaN/Infinity viewport metrics", () => {
    const p = canvasPointFromClient(Number.NaN, Number.POSITIVE_INFINITY, {
      left: Number.NaN,
      top: Number.NEGATIVE_INFINITY,
      scrollLeft: Number.NaN,
    });
    expect(Number.isFinite(p.x)).toBe(true);
    expect(Number.isFinite(p.y)).toBe(true);
  });
});

describe("graph gutter horizontal pan", () => {
  it("pans extra branch lanes without wrapping past the content edge", () => {
    const gutter = { scrollLeft: 40, scrollWidth: 1_200, clientWidth: 440 };
    expect(panGraphHorizontally(gutter, 80)).toBe(true);
    expect(gutter.scrollLeft).toBe(120);
    expect(panGraphHorizontally(gutter, 10_000)).toBe(true);
    expect(gutter.scrollLeft).toBe(760);
    expect(panGraphHorizontally(gutter, 1)).toBe(false);
    expect(gutter.scrollLeft).toBe(760);
  });

  it("does not pan a gutter that already fits its lanes", () => {
    const gutter = { scrollLeft: 0, scrollWidth: 220, clientWidth: 220 };
    expect(panGraphHorizontally(gutter, 40)).toBe(false);
    expect(gutter.scrollLeft).toBe(0);
  });

  it("maps a pointer drag to scrollLeft in the opposite direction", () => {
    expect(graphDragScrollLeft(200, 100, 40)).toBe(260);
    expect(isGraphPanGesture(100, 50, 103, 51)).toBe(false);
    expect(isGraphPanGesture(100, 50, 108, 50)).toBe(true);
  });

  it("uses shift+wheel and dominant deltaX to pan, otherwise forwards vertically", () => {
    const gutter = { scrollLeft: 0, scrollWidth: 1_200, clientWidth: 440 };
    const list = { scrollTop: 80, scrollHeight: 2_000, clientHeight: 400 };

    expect(
      applyGraphGutterWheel(
        { ctrlKey: true, shiftKey: false, deltaX: 0, deltaY: 40, deltaMode: 0 },
        gutter,
        list,
        36,
      ),
    ).toBe(true);
    expect(gutter.scrollLeft).toBe(0);
    expect(list.scrollTop).toBe(80);

    expect(
      applyGraphGutterWheel(
        { ctrlKey: false, shiftKey: true, deltaX: 0, deltaY: 50, deltaMode: 0 },
        gutter,
        list,
        36,
      ),
    ).toBe(true);
    expect(gutter.scrollLeft).toBe(50);
    expect(list.scrollTop).toBe(80);

    expect(
      applyGraphGutterWheel(
        { ctrlKey: false, shiftKey: false, deltaX: 30, deltaY: 4, deltaMode: 0 },
        gutter,
        list,
        36,
      ),
    ).toBe(true);
    expect(gutter.scrollLeft).toBe(80);

    expect(
      applyGraphGutterWheel(
        { ctrlKey: false, shiftKey: false, deltaX: 0, deltaY: 20, deltaMode: 0 },
        gutter,
        list,
        36,
      ),
    ).toBe(true);
    expect(list.scrollTop).toBe(100);
    expect(gutter.scrollLeft).toBe(80);
  });
});
