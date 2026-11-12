import { describe, expect, it, vi } from "vitest";
import {
  classifyDropRegion,
  createDropZoneRegistry,
  physicalToCssPoint,
  type DropRegion,
} from "./dropZoneRegistry";

describe("dropZoneRegistry", () => {
  it("converts physical pixels to CSS pixels using Retina scaleFactor and pageZoom", () => {
    expect(physicalToCssPoint({ x: 400, y: 300 }, 2, 1)).toEqual({ x: 200, y: 150 });
    expect(physicalToCssPoint({ x: 600, y: 300 }, 2, 1.5)).toEqual({ x: 200, y: 100 });
    expect(physicalToCssPoint({ x: 100, y: 50 }, Number.NaN, 0)).toEqual({ x: 100, y: 50 });
  });

  it("classifies center vs left/right/top/bottom split drop bands", () => {
    const rect = { left: 100, top: 200, width: 400, height: 200 };
    expect(classifyDropRegion({ x: 50, y: 250 }, rect)).toBeNull();
    expect(classifyDropRegion({ x: 300, y: 300 }, rect)).toBe("center");
    expect(classifyDropRegion({ x: 115, y: 300 }, rect)).toBe("left");
    expect(classifyDropRegion({ x: 490, y: 300 }, rect)).toBe("right");
    expect(classifyDropRegion({ x: 300, y: 210 }, rect)).toBe("top");
    expect(classifyDropRegion({ x: 300, y: 392 }, rect)).toBe("bottom");
    expect(classifyDropRegion({ x: 115, y: 300 }, rect, false)).toBe("center");
  });

  it("notifies hover transitions, skips hidden/occluded zones, and dispatches drops", () => {
    let occluded = false;
    const registry = createDropZoneRegistry({ isPointOccluded: () => occluded });
    const hoversA: Array<DropRegion | null> = [];
    const dropsA = vi.fn(() => true);

    const unregisterA = registry.register({
      id: "pane-a",
      getRect: () => ({ left: 0, top: 0, width: 200, height: 200 }),
      isVisible: () => true,
      onHover: (region) => hoversA.push(region),
      onDrop: dropsA,
    });

    expect(registry.hoverCssPoint({ x: 100, y: 100 }, ["/repo/a.ts"])).toEqual({
      id: "pane-a",
      region: "center",
    });
    expect(registry.hoverCssPoint({ x: 195, y: 100 }, ["/repo/a.ts"])).toEqual({
      id: "pane-a",
      region: "right",
    });
    expect(registry.hoverCssPoint({ x: 500, y: 500 }, ["/repo/a.ts"])).toBeNull();
    expect(hoversA).toEqual(["center", "right", null]);

    occluded = true;
    expect(registry.hoverCssPoint({ x: 100, y: 100 }, ["/repo/a.ts"])).toBeNull();
    occluded = false;

    expect(registry.dropAtCssPoint({ x: 195, y: 100 }, ["/repo/a.ts"])).toBe(true);
    expect(dropsA).toHaveBeenCalledWith(["/repo/a.ts"], "right");

    unregisterA();
    expect(registry.dropAtCssPoint({ x: 100, y: 100 }, ["/repo/a.ts"])).toBe(false);
  });

  it("tracks in-app explorer file drag payloads and clears them on dragend", () => {
    const registry = createDropZoneRegistry();
    expect(registry.currentInAppDrag()).toBeNull();
    registry.beginInAppDrag(["/repo/src/lib.rs"], true);
    expect(registry.currentInAppDrag()).toEqual({ paths: ["/repo/src/lib.rs"], filesOnly: true });
    registry.endInAppDrag();
    expect(registry.currentInAppDrag()).toBeNull();
  });
});
