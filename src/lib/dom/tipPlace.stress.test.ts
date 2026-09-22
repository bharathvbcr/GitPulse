import { describe, expect, it } from "vitest";
import {
  placeTooltipBubble,
  preferredTipSide,
  tipCoversAnchor,
  type TipRect,
  type TipSide,
} from "./tipPlace";

const VIEW = { width: 1200, height: 800 };
const BUBBLE = { width: 180, height: 28 };

function anchor(top: number, left = 400, width = 28, height = 24): TipRect {
  return { left, top, width, height };
}

describe("preferredTipSide", () => {
  it("treats anything other than above as below", () => {
    for (const value of [null, undefined, "", "  ", "below", "ABOVE", "left", "above "]) {
      expect(preferredTipSide(value)).toBe(value?.trim() === "above" ? "above" : "below");
    }
    expect(preferredTipSide("above")).toBe("above");
    expect(preferredTipSide("  above  ")).toBe("above");
  });
});

describe("placeTooltipBubble", () => {
  it("opens below a mid-pane control and clears the anchor", () => {
    const box = anchor(300);
    const placed = placeTooltipBubble(box, BUBBLE, VIEW);
    expect(placed.side).toBe("below");
    expect(placed.top).toBe(box.top + box.height + 8);
    expect(tipCoversAnchor(placed, box, BUBBLE)).toBe(false);
  });

  it("opens above a toolbar control that asks for it, clear of the anchor", () => {
    // The whitespace toggle sits on the code toolbar. Below, the bubble
    // landed on the first source line.
    const box = anchor(48, 640);
    const placed = placeTooltipBubble(box, BUBBLE, VIEW, "above");
    expect(placed.side).toBe("above");
    expect(placed.top).toBe(box.top - BUBBLE.height - 8);
    expect(tipCoversAnchor(placed, box, BUBBLE)).toBe(false);
    expect(placed.top).toBeGreaterThanOrEqual(8);
  });

  it("stays in the toolbar band when a top-edge control cannot open above", () => {
    // The whitespace toggle in a window with no chrome above it. Opening
    // below lands on the first source line.
    const box = anchor(6, 200, 26, 27);
    const placed = placeTooltipBubble(box, BUBBLE, VIEW, "above");
    expect(placed.top + BUBBLE.height).toBeLessThanOrEqual(box.top + box.height + 8);
    expect(placed.left).toBeGreaterThanOrEqual(box.left + box.width);
    expect(tipCoversAnchor(placed, box, BUBBLE)).toBe(false);
  });

  it("flips above when below would leave the viewport", () => {
    const box = anchor(760, 200, 40, 24);
    const placed = placeTooltipBubble(box, BUBBLE, VIEW);
    expect(placed.side).toBe("above");
    expect(placed.top + BUBBLE.height).toBeLessThanOrEqual(VIEW.height - 8);
    expect(tipCoversAnchor(placed, box, BUBBLE)).toBe(false);
  });

  it("clamps horizontally inside the viewport", () => {
    const leftEdge = placeTooltipBubble(anchor(200, 0, 10, 20), BUBBLE, VIEW);
    const rightEdge = placeTooltipBubble(anchor(200, 1180, 20, 20), BUBBLE, VIEW);
    expect(leftEdge.left).toBe(8);
    expect(rightEdge.left + BUBBLE.width).toBeLessThanOrEqual(VIEW.width - 8);
  });

  it("stays finite and on-screen for degenerate geometry", () => {
    const bad = [Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY, -50, 0];
    const sides: TipSide[] = ["above", "below"];
    for (const n of bad) {
      for (const side of sides) {
        const placed = placeTooltipBubble(
          { left: n, top: n, width: n, height: n },
          { width: n, height: n },
          { width: n, height: n },
          side,
        );
        expect(Number.isFinite(placed.left)).toBe(true);
        expect(Number.isFinite(placed.top)).toBe(true);
        expect(placed.left).toBeGreaterThanOrEqual(0);
        expect(placed.top).toBeGreaterThanOrEqual(0);
      }
    }
  });

  it("clamps a bubble larger than the viewport instead of producing a negative origin", () => {
    const placed = placeTooltipBubble(
      anchor(10, 10, 20, 20),
      { width: 4000, height: 3000 },
      { width: 320, height: 240 },
    );
    expect(placed.left).toBe(8);
    expect(placed.top).toBe(8);
    expect(Number.isFinite(placed.left)).toBe(true);
    expect(Number.isFinite(placed.top)).toBe(true);
  });

  it("does not cover the anchor whenever either side fits", () => {
    const heights = [16, 28, 48, 96];
    const tops = [40, 80, 200, 400, 700];
    for (const bubbleHeight of heights) {
      for (const top of tops) {
        for (const side of ["above", "below"] as const) {
          const box = anchor(top, 100, 32, 22);
          const placed = placeTooltipBubble(box, { width: 160, height: bubbleHeight }, VIEW, side);
          const below = box.top + box.height + 8;
          const above = box.top - bubbleHeight - 8;
          const fitsBelow = below >= 8 && below + bubbleHeight <= VIEW.height - 8;
          const fitsAbove = above >= 8 && above + bubbleHeight <= VIEW.height - 8;
          if (fitsBelow || fitsAbove) {
            expect(tipCoversAnchor(placed, box, { width: 160, height: bubbleHeight })).toBe(false);
          }
        }
      }
    }
  });
});
