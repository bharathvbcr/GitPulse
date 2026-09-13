import { describe, expect, it } from "vitest";
import { tourPosition } from "./productTourTarget";

describe("live walkthrough placement", () => {
  const card = { width: 380, height: 420 };
  it("sits below a title-bar control without covering it", () => {
    expect(tourPosition({ left: 140, top: 8, width: 120, height: 32 }, card, 1200, 900))
      .toEqual({ left: 140, top: 56 });
  });
  it("moves above a low target and away from the right edge", () => {
    expect(tourPosition({ left: 1000, top: 750, width: 100, height: 32 }, card, 1200, 900))
      .toEqual({ left: 804, top: 314 });
  });
  it("docks at the lower right when a target is absent", () => {
    expect(tourPosition(null, card, 1200, 900)).toEqual({ left: 804, top: 464 });
  });
  it.each([[320, 400], [200, 160], [0, 0]])("never positions outside a %s by %s viewport", (width, height) => {
    const result = tourPosition({ left: -40, top: 500, width: 800, height: 200 }, card, width, height);
    expect(result.left).toBeGreaterThanOrEqual(0);
    expect(result.top).toBeGreaterThanOrEqual(0);
    expect(result.left).toBeLessThanOrEqual(width);
    expect(result.top).toBeLessThanOrEqual(height);
  });
});
