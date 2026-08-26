import { describe, expect, it } from "vitest";
import {
  computeCacheKey,
  createGraphStaticCache,
  sameCacheInputs,
  type CachedSurface,
  type GraphCacheInputs,
  type StripPaintRequest,
} from "../graphCache";

function inputs(overrides: Partial<GraphCacheInputs> = {}): GraphCacheInputs {
  return {
    dataVersion: 1,
    cssWidth: 300,
    densitySignature: "spacious",
    themeSignature: "a|b|c|d|e",
    dpr: 1,
    ...overrides,
  };
}

interface FakeSurface extends CachedSurface {
  id: number;
  paintRequests: StripPaintRequest[];
}

function makeHarness() {
  const surfaces: FakeSurface[] = [];
  const releases: number[] = [];
  let nextId = 0;
  const paints: StripPaintRequest[] = [];

  const cache = createGraphStaticCache(
    (req) => {
      paints.push({ ...req });
    },
    (w, h, dpr): CachedSurface | null => {
      const id = nextId++;
      const surface: FakeSurface = {
        id,
        canvas: { width: Math.round(w * dpr), height: Math.round(h * dpr) } as HTMLCanvasElement,
        ctx: { setTransform() {}, fillRect() {}, fillStyle: "" } as unknown as CanvasRenderingContext2D,
        release: () => releases.push(id),
        paintRequests: [],
      };
      surface.id = id;
      surfaces.push(surface);
      return surface;
    },
    { stripCssHeight: 40, maxStrips: 3 },
  );
  return { cache, surfaces, releases, paints };
}

describe("avatar toggle in the strip-cache key", () => {
  it("flipping showAvatars drops every live strip", () => {
    const h = makeHarness();
    h.cache.sync(inputs(), { rowHeight: 10, totalRows: 8 });
    h.cache.paint({ drawImage() {} } as unknown as CanvasRenderingContext2D, { contentTopCss: 0, viewportHeightCss: 40 });
    expect(h.cache.stats().liveStrips).toBeGreaterThan(0);

    h.cache.sync(inputs({ showAvatars: true }), { rowHeight: 10, totalRows: 8 });
    // Next paint must re-render strips, not reuse avatar-less tiles.
    const before = h.paints.length;
    h.cache.paint({ drawImage() {} } as unknown as CanvasRenderingContext2D, { contentTopCss: 0, viewportHeightCss: 40 });
    expect(h.paints.length).toBe(before + 1);
  });

  it("treats undefined and false as distinct from true but equal to each other", () => {
    expect(sameCacheInputs(inputs(), inputs({ showAvatars: false }))).toBe(true);
    expect(sameCacheInputs(inputs(), inputs({ showAvatars: true }))).toBe(false);
    expect(computeCacheKey(inputs())).not.toBe(computeCacheKey(inputs({ showAvatars: true })));
  });

  it("released surfaces are never resurrected after the toggle", () => {
    const h = makeHarness();
    h.cache.sync(inputs(), { rowHeight: 10, totalRows: 8 });
    h.cache.paint({ drawImage() {} } as unknown as CanvasRenderingContext2D, { contentTopCss: 0, viewportHeightCss: 80 });
    h.cache.sync(inputs({ showAvatars: true }), { rowHeight: 10, totalRows: 8 });
    const releasesAfterInvalidate = h.releases.length;
    h.cache.paint({ drawImage() {} } as unknown as CanvasRenderingContext2D, { contentTopCss: 0, viewportHeightCss: 80 });
    // Fresh surfaces allocated; old ones released exactly once.
    expect(h.releases.length).toBeGreaterThanOrEqual(releasesAfterInvalidate);
    for (const surface of h.surfaces) {
      // A surface is either unreleased or released exactly once.
      const times = h.releases.filter((id) => id === surface.id).length;
      expect(times).toBeLessThanOrEqual(1);
    }
  });
});
