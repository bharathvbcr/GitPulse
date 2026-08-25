import { describe, expect, it } from "vitest";
import {
  forwardGraphWheel,
  normalizeWheelDelta,
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
    });

    expect(positionGraphTooltip(790, 490, 800, 500, 320, 160)).toEqual({
      left: 472,
      top: 318,
      placement: "above",
    });
  });

  it("handles viewports narrower than the preferred tooltip width", () => {
    expect(positionGraphTooltip(4, 100, 240, 300, 320, 160).left).toBe(8);
  });
});
