import { describe, it, expect, vi } from "vitest";
import {
  computeCacheKey,
  createGraphStaticCache,
  sameCacheInputs,
  themeSignatureOf,
  type BlitRequest,
  type CachedSurface,
  type GraphCacheInputs,
  type StripPaintRequest,
  type SurfaceFactory,
} from "../graphCache";
import {
  DEFAULT_THEME,
  GraphRenderer,
  LOOKBACK_ROWS,
  primedRowRange,
  type VisualCommitRow,
} from "../GraphRenderer";

const BASE_INPUTS: GraphCacheInputs = {
  dataVersion: 1,
  cssWidth: 220,
  densitySignature: "spacious",
  themeSignature: "dark",
  dpr: 2,
};

function inputs(overrides: Partial<GraphCacheInputs> = {}): GraphCacheInputs {
  return { ...BASE_INPUTS, ...overrides };
}

/** Target context that records drawImage source/dest rects. */
function recordingTarget() {
  const blits: Array<{
    src: [number, number, number, number];
    dst: [number, number, number, number];
  }> = [];
  const ctx = {
    drawImage: vi.fn(
      (
        _canvas: HTMLCanvasElement,
        sx: number,
        sy: number,
        sw: number,
        sh: number,
        dx: number,
        dy: number,
        dw: number,
        dh: number,
      ) => {
        blits.push({ src: [sx, sy, sw, sh], dst: [dx, dy, dw, dh] });
      },
    ),
  };
  return { ctx: ctx as unknown as CanvasRenderingContext2D, blits };
}

/** Injectable surface factory whose "canvases" are plain objects, with a release counter per surface. */
function fakeSurfaceFactory() {
  const surfaces: CachedSurface[] = [];
  const released: CachedSurface[] = [];
  const factory: SurfaceFactory = (cssWidth, cssHeight, dpr) => {
    const canvas = {
      width: Math.max(1, Math.round(cssWidth * dpr)),
      height: Math.max(1, Math.round(cssHeight * dpr)),
    } as HTMLCanvasElement;
    const ctx = {
      setTransform: vi.fn(),
      // Full op surface: some painters run real GraphRenderer.render against it.
      save: vi.fn(),
      restore: vi.fn(),
      beginPath: vi.fn(),
      closePath: vi.fn(),
      moveTo: vi.fn(),
      lineTo: vi.fn(),
      bezierCurveTo: vi.fn(),
      arc: vi.fn(),
      fill: vi.fn(),
      stroke: vi.fn(),
      lineWidth: 2,
      lineCap: "round",
      lineJoin: "round",
      imageSmoothingEnabled: true,
      globalAlpha: 1,
      strokeStyle: "",
      fillStyle: "",
    } as unknown as CanvasRenderingContext2D;
    const surface: CachedSurface = { canvas, ctx };
    surface.release = () => released.push(surface);
    surfaces.push(surface);
    return surface;
  };
  return { factory, surfaces, released };
}

/**
 * Painter-side context that records the alpha every stroke ran at — the only
 * way dangling stubs are distinguishable from ordinary opaque geometry.
 */
function strokeAlphaRecorder() {
  const alphasAtStroke: number[] = [];
  const ctx: Record<string, unknown> = {
    save: vi.fn(),
    restore: vi.fn(),
    beginPath: vi.fn(),
    moveTo: vi.fn(),
    lineTo: vi.fn(),
    bezierCurveTo: vi.fn(),
    arc: vi.fn(),
    fill: vi.fn(),
    drawImage: vi.fn(),
    setTransform: vi.fn(),
    lineWidth: 2,
    lineCap: "round",
    lineJoin: "round",
    imageSmoothingEnabled: true,
    globalAlpha: 1,
    strokeStyle: "",
    fillStyle: "",
    canvas: { width: 220, height: 40 },
    stroke: () => alphasAtStroke.push(ctx.globalAlpha as number),
  };
  return {
    alphasAtStroke,
    ctx: ctx as unknown as CanvasRenderingContext2D,
  };
}

/** Row carrying one connection; enough for renderer-driven strip painting. */
function graphRow(id: string, overrides: Partial<VisualCommitRow> = {}): VisualCommitRow {
  return {
    id,
    parent_ids: [],
    summary: "s",
    author_name: "Dev",
    author_email: "dev@example.com",
    timestamp: 1,
    lane: 0,
    color_index: 0,
    active_lanes: [0],
    active_lane_colors: [0],
    connections: [],
    is_merge: false,
    is_root: false,
    ...overrides,
  };
}

/** Painter that records every strip request it receives. */
function recordingPainter() {
  const requests: StripPaintRequest[] = [];
  const painter = (req: StripPaintRequest) => {
    requests.push(req);
  };
  return { painter, requests };
}

function paintReq(contentTopCss: number, viewportHeightCss: number): BlitRequest {
  return { contentTopCss, viewportHeightCss };
}

describe("cache key computation", () => {
  it("distinguishes every input field", () => {
    const base = computeCacheKey(inputs());
    expect(computeCacheKey(inputs())).toBe(base);
    expect(computeCacheKey(inputs({ dataVersion: 2 }))).not.toBe(base);
    expect(computeCacheKey(inputs({ cssWidth: 300 }))).not.toBe(base);
    expect(computeCacheKey(inputs({ densitySignature: "compact" }))).not.toBe(base);
    expect(computeCacheKey(inputs({ themeSignature: "light" }))).not.toBe(base);
    expect(computeCacheKey(inputs({ dpr: 1.5 }))).not.toBe(base);
  });

  it("normalizes dpr formatting without changing semantics", () => {
    expect(computeCacheKey(inputs({ dpr: 2.0 }))).toBe(computeCacheKey(inputs({ dpr: 2 })));
    expect(sameCacheInputs(null, inputs())).toBe(false);
    expect(sameCacheInputs(inputs(), inputs())).toBe(true);
    expect(sameCacheInputs(inputs(), inputs({ dataVersion: 9 }))).toBe(false);
  });

  it("builds a theme signature from all five resolved colors", () => {
    const sig = themeSignatureOf(DEFAULT_THEME);
    for (const value of Object.values(DEFAULT_THEME)) {
      expect(sig).toContain(value);
    }
  });
});

describe("strip tiling and slicing", () => {
  // rowHeight 10, stripCssHeight 20 -> rowsPerStrip 2; totalRows 5 -> strips
  // cover content [0,20), [20,40), [40,50).
  function makeCache() {
    const { factory, surfaces } = fakeSurfaceFactory();
    const { painter, requests } = recordingPainter();
    const cache = createGraphStaticCache(painter, factory, { stripCssHeight: 20 });
    return { cache, surfaces, requests };
  }

  function syncFiveRows(cache: ReturnType<typeof makeCache>["cache"]) {
    cache.sync(inputs(), { rowHeight: 10, totalRows: 5 });
  }

  it("materializes only visible strips and blits partial strips at offsets", () => {
    const { cache, surfaces, requests } = makeCache();
    cache.sync(inputs({ dpr: 1 }), { rowHeight: 10, totalRows: 5 });
    const target = recordingTarget();

    // Viewport covers content y 12..27: bottom of strip 0, most of strip 1.
    const blitted = cache.paint(target.ctx, paintReq(12, 15));

    expect(blitted).toBe(true);
    expect(surfaces).toHaveLength(2); // strip 2 untouched
    expect(requests.map((r) => r.firstRow)).toEqual([0, 2]);
    expect(requests[0]).toMatchObject({
      rowCount: 2,
      stripTopCss: 0,
      viewportCssHeight: 20,
    });
    expect(requests[1]).toMatchObject({
      firstRow: 2,
      rowCount: 2,
      stripTopCss: 20,
      viewportCssHeight: 20,
    });

    expect(target.blits).toEqual([
      // strip 0 clipped to its lower part: source y 12..20 -> dest y 12
      { src: [0, 12, 220, 8], dst: [0, 12, 220, 8] },
      // strip 1 clipped to its upper part: source y 0..7 -> dest y 20
      { src: [0, 0, 220, 7], dst: [0, 20, 220, 7] },
    ]);
  });

  it("scales source rects by dpr and keeps texel mapping 1:1", () => {
    const { cache, surfaces } = makeCache();
    cache.sync(inputs({ dpr: 2 }), { rowHeight: 10, totalRows: 5 });
    const target = recordingTarget();

    cache.paint(target.ctx, paintReq(12, 15));

    expect(target.blits).toEqual([
      { src: [0, 24, 440, 16], dst: [0, 12, 220, 8] },
      { src: [0, 0, 440, 14], dst: [0, 20, 220, 7] },
    ]);
    expect(surfaces[0].canvas.height).toBe(40); // round(20 * 2)
  });

  it("handles non-integer dpr without producing zero-height sources", () => {
    const { cache, surfaces } = makeCache();
    cache.sync(inputs({ dpr: 1.5 }), { rowHeight: 10, totalRows: 5 });
    const target = recordingTarget();

    const blitted = cache.paint(target.ctx, paintReq(0, 50));
    expect(blitted).toBe(true);
    for (const blit of target.blits) {
      expect(blit.src[3]).toBeGreaterThan(0);
      expect(blit.dst[3]).toBeCloseTo(blit.src[3] / 1.5, 5);
    }
    expect(surfaces.every((s) => s.canvas.height > 0)).toBe(true);
  });

  it("clips the final short strip to real content", () => {
    const { cache } = makeCache();
    syncFiveRows(cache);
    const target = recordingTarget();

    cache.paint(target.ctx, paintReq(40, 30)); // view extends past content end

    expect(target.blits).toEqual([
      // only rows 4 live here: content 40..50, not the requested 70
      { src: [0, 0, 440, 20], dst: [0, 40, 220, 10] },
    ]);
  });

  it("reports the final strip's short geometry to the painter", () => {
    const { cache, requests } = makeCache();
    syncFiveRows(cache);
    const target = recordingTarget();

    cache.paint(target.ctx, paintReq(45, 5));

    expect(requests).toHaveLength(1);
    expect(requests[0]).toMatchObject({
      firstRow: 4,
      rowCount: 1,
      stripTopCss: 40,
      viewportCssHeight: 10,
    });
  });
});

describe("invalidation matrix", () => {
  function makeCache() {
    const { factory, surfaces } = fakeSurfaceFactory();
    const { painter } = recordingPainter();
    const cache = createGraphStaticCache(painter, factory, { stripCssHeight: 20 });
    return { cache, surfaces };
  }

  it("reuses strips when nothing changed and re-renders on any input change", () => {
    const { cache, surfaces } = makeCache();
    const target = recordingTarget();
    const current = inputs();

    const renderDelta = (
      mutate?: (i: GraphCacheInputs) => void,
      geometry = { rowHeight: 10, totalRows: 5 },
    ) => {
      if (mutate) mutate(current);
      const before = surfaces.length;
      cache.sync(current, geometry);
      cache.paint(target.ctx, paintReq(0, 20));
      return surfaces.length - before;
    };

    expect(renderDelta()).toBe(1); // fresh cache renders once
    expect(renderDelta()).toBe(0); // unchanged: reused

    expect(renderDelta((i) => { i.dataVersion += 1; })).toBe(1);
    expect(renderDelta((i) => { i.cssWidth += 10; })).toBe(1);
    expect(renderDelta((i) => { i.densitySignature = "compact"; })).toBe(1);
    expect(renderDelta((i) => { i.themeSignature = "light"; })).toBe(1);
    expect(renderDelta((i) => { i.dpr = 1.5; })).toBe(1);

    // Growing history appends rows below; existing strips stay valid.
    expect(renderDelta(undefined, { rowHeight: 10, totalRows: 8 })).toBe(0);
    const rendersBeforeGrowth = cache.stats().stripRenders;
    cache.paint(recordingTarget().ctx, paintReq(60, 20));
    // Only the new deep strip renders; the cached ones above survive.
    expect(cache.stats().stripRenders).toBe(rendersBeforeGrowth + 1);
  });

  it("drops strips when row height changes", () => {
    const { cache } = makeCache();
    cache.sync(inputs(), { rowHeight: 10, totalRows: 5 });
    cache.paint(recordingTarget().ctx, paintReq(0, 20));

    cache.sync(inputs(), { rowHeight: 26, totalRows: 5 });
    expect(cache.stats().liveStrips).toBe(0);
    const rendersBefore = cache.stats().stripRenders;
    cache.paint(recordingTarget().ctx, paintReq(0, 20));
    expect(cache.stats().stripRenders).toBe(rendersBefore + 1);
  });

  it("explicit invalidate forces re-render on next paint", () => {
    const { cache } = makeCache();
    cache.sync(inputs(), { rowHeight: 10, totalRows: 5 });
    const target = recordingTarget();

    cache.paint(target.ctx, paintReq(0, 20));
    const afterFirst = cache.stats().stripRenders;
    cache.invalidate();
    cache.paint(target.ctx, paintReq(0, 20));
    expect(cache.stats().stripRenders).toBe(afterFirst + 1);
  });
});

describe("LRU eviction", () => {
  function tinyCache(maxStrips: number) {
    const { factory, surfaces } = fakeSurfaceFactory();
    const { painter } = recordingPainter();
    const cache = createGraphStaticCache(painter, factory, {
      stripCssHeight: 10, // one row per strip
      maxStrips,
    });
    cache.sync(inputs({ cssWidth: 100 }), { rowHeight: 10, totalRows: 6 });
    return { cache, surfaces };
  }

  it("caps live strips and re-renders an evicted strip on demand", () => {
    const { cache, surfaces } = tinyCache(2);
    const target = recordingTarget();

    // Walk the whole 6-row list: strips 0..5 pass through the cap of 2.
    cache.paint(target.ctx, paintReq(0, 60));

    expect(cache.stats().liveStrips).toBe(2);
    expect(cache.stats().stripRenders).toBe(6);
    expect(surfaces).toHaveLength(6);

    // Strip 0 was evicted; touching it again must re-render.
    cache.paint(recordingTarget().ctx, paintReq(0, 10));
    expect(cache.stats().stripRenders).toBe(7);
  });

  it("keeps recently used strips alive across paints", () => {
    const { cache } = tinyCache(2);
    const target = recordingTarget();

    cache.paint(target.ctx, paintReq(0, 20)); // strips 0,1
    cache.paint(target.ctx, paintReq(0, 10)); // strip 0 re-touched, not re-rendered
    cache.paint(target.ctx, paintReq(10, 10)); // strip 1 re-touched
    expect(cache.stats().stripRenders).toBe(2);

    cache.paint(target.ctx, paintReq(20, 10)); // evicts LRU (strip 0), renders 2
    expect(cache.stats().stripRenders).toBe(3);
    expect(cache.stats().liveStrips).toBe(2);
  });
});

describe("graceful bypass", () => {
  it("refuses to engage for tiny graphs, zero width, or bad dpr", () => {
    const { factory, surfaces } = fakeSurfaceFactory();
    const { painter } = recordingPainter();
    const cache = createGraphStaticCache(painter, factory);
    const target = recordingTarget();

    cache.sync(inputs(), { rowHeight: 36, totalRows: 1 });
    expect(cache.paint(target.ctx, paintReq(0, 600))).toBe(false);

    cache.sync(inputs({ cssWidth: 0 }), { rowHeight: 36, totalRows: 10 });
    expect(cache.paint(target.ctx, paintReq(0, 600))).toBe(false);

    cache.sync(inputs({ dpr: 0 }), { rowHeight: 36, totalRows: 10 });
    expect(cache.paint(target.ctx, paintReq(0, 600))).toBe(false);

    expect(surfaces).toHaveLength(0);
    expect(target.blits).toHaveLength(0);
  });

  it("returns false when the viewport sits entirely past the content", () => {
    const { factory } = fakeSurfaceFactory();
    const { painter } = recordingPainter();
    const cache = createGraphStaticCache(painter, factory, { stripCssHeight: 20 });
    cache.sync(inputs(), { rowHeight: 10, totalRows: 5 });
    const target = recordingTarget();

    expect(cache.paint(target.ctx, paintReq(100, 50))).toBe(false);
  });

  it("dispose releases everything and rejects further use", () => {
    const { factory } = fakeSurfaceFactory();
    const { painter } = recordingPainter();
    const cache = createGraphStaticCache(painter, factory, { stripCssHeight: 20 });
    cache.sync(inputs(), { rowHeight: 10, totalRows: 5 });
    const target = recordingTarget();

    cache.paint(target.ctx, paintReq(0, 20));
    expect(cache.stats().liveStrips).toBe(1);

    cache.dispose();
    expect(cache.stats().liveStrips).toBe(0);
    expect(cache.paint(target.ctx, paintReq(0, 20))).toBe(false);
  });
});

describe("surface release hygiene", () => {
  function makeCache(maxStrips?: number) {
    const { factory, surfaces, released } = fakeSurfaceFactory();
    const { painter } = recordingPainter();
    const cache = createGraphStaticCache(painter, factory, {
      stripCssHeight: 20,
      ...(maxStrips !== undefined ? { maxStrips } : {}),
    });
    return { cache, surfaces, released };
  }

  it("dispose drops every factory-created surface (none retained)", () => {
    const { cache, surfaces, released } = makeCache();
    cache.sync(inputs(), { rowHeight: 10, totalRows: 6 });
    cache.paint(recordingTarget().ctx, paintReq(0, 60)); // 3 strips

    expect(surfaces).toHaveLength(3);
    expect(released).toHaveLength(0);
    cache.dispose();
    // Every surface the factory handed out was surrendered exactly once.
    expect(released).toHaveLength(3);
    expect(new Set(released).size).toBe(3);
    expect(cache.stats().liveStrips).toBe(0);
  });

  it("an input change releases superseded strips in the same call, before any repaint", () => {
    const { cache, surfaces, released } = makeCache();
    cache.sync(inputs(), { rowHeight: 10, totalRows: 5 });
    cache.paint(recordingTarget().ctx, paintReq(0, 40)); // 2 strips live
    expect(cache.stats().liveStrips).toBe(2);

    cache.sync(inputs({ cssWidth: 300 }), { rowHeight: 10, totalRows: 5 });

    expect(released).toHaveLength(2);
    expect(cache.stats().liveStrips).toBe(0);
    expect(surfaces).toHaveLength(2); // no transient extra allocation yet
  });

  it("explicit invalidate releases all strips", () => {
    const { cache, released } = makeCache();
    cache.sync(inputs(), { rowHeight: 10, totalRows: 4 });
    cache.paint(recordingTarget().ctx, paintReq(0, 40));

    cache.invalidate();
    expect(released).toHaveLength(2);
    expect(cache.stats().liveStrips).toBe(0);
  });

  it("LRU eviction releases evictees while the cap bounds live memory", () => {
    const { factory, surfaces, released } = fakeSurfaceFactory();
    // Peak concurrency: how many factory surfaces were alive at once. Room is
    // made *before* each new allocation, so it must never exceed the cap.
    let peakLive = 0;
    const trackedFactory: SurfaceFactory = (w, h, dpr) => {
      const surface = factory(w, h, dpr);
      peakLive = Math.max(peakLive, surfaces.length - released.length);
      return surface;
    };
    const cache = createGraphStaticCache(recordingPainter().painter, trackedFactory, {
      stripCssHeight: 20,
      maxStrips: 2,
    });
    cache.sync(inputs({ cssWidth: 100 }), { rowHeight: 20, totalRows: 6 });

    cache.paint(recordingTarget().ctx, paintReq(0, 120)); // walks strips 0..2

    expect(peakLive).toBeLessThanOrEqual(2);
    // Cap holds and every surface pushed out of the map was released.
    expect(cache.stats().liveStrips).toBe(2);
    expect(released.length + cache.stats().liveStrips).toBe(surfaces.length);
    expect(released.length).toBeGreaterThan(0);
  });
});

describe("static layer carries no dangling-stub ops", () => {
  /**
   * The stub geometry is translucent by design; rasterized per strip it would
   * clip mid-fade at strip boundaries (the seam artifact). Strips must render
   * with zero translucent strokes — the overlay owns stubs now.
   */
  function stubbyRows(): VisualCommitRow[] {
    return [
      graphRow("a", {
        connections: [{ from_lane: 0, to_lane: 0, to_row_offset: 1, is_merge: false, color_index: 0, is_dangling: true }],
      }),
      graphRow("b", {
        lane: 1,
        color_index: 1,
        active_lanes: [0, 1],
        active_lane_colors: [0, 1],
        connections: [{ from_lane: 1, to_lane: 0, to_row_offset: 1, is_merge: false, color_index: 1, is_dangling: true }],
      }),
      graphRow("c"),
      graphRow("d"),
      graphRow("e"),
    ];
  }

  function rendererPaintedCache() {
    const renderer = new GraphRenderer({ rowHeight: 10 });
    const rows = stubbyRows();
    // Same painter shape CommitTable installs: primed lookback above the strip.
    const painter = (req: StripPaintRequest) => {
      const range = primedRowRange(req.firstRow, req.rowCount, rows.length);
      renderer.render(
        req.ctx,
        rows,
        range.from,
        range.to,
        req.stripTopCss,
        undefined,
        { viewportHeight: req.viewportCssHeight },
        null,
      );
    };
    return { renderer, rows, painter };
  }

  it("strip painting records only opaque strokes even for rows full of dangling connections", () => {
    const { renderer, rows, painter } = rendererPaintedCache();

    // Surfaces hand out recording contexts: every stroke any strip ever runs
    // is captured with its alpha — the fingerprint of a stub is translucency.
    const stripStrokes: number[] = [];
    const factory: SurfaceFactory = (cssWidth, cssHeight, dpr) => {
      const ctx: Record<string, unknown> = {
        save: vi.fn(),
        restore: vi.fn(),
        beginPath: vi.fn(),
        moveTo: vi.fn(),
        lineTo: vi.fn(),
        bezierCurveTo: vi.fn(),
        arc: vi.fn(),
        fill: vi.fn(),
        drawImage: vi.fn(),
        setTransform: vi.fn(),
        stroke: () => stripStrokes.push(ctx.globalAlpha as number),
        globalAlpha: 1,
        lineWidth: 2,
        lineCap: "round",
        lineJoin: "round",
        imageSmoothingEnabled: true,
        strokeStyle: "",
        fillStyle: "",
      };
      return {
        canvas: {
          width: Math.max(1, Math.round(cssWidth * dpr)),
          height: Math.max(1, Math.round(cssHeight * dpr)),
        } as HTMLCanvasElement,
        ctx: ctx as unknown as CanvasRenderingContext2D,
      };
    };

    const cache = createGraphStaticCache(painter, factory, { stripCssHeight: 20 });
    cache.sync(inputs({ dpr: 1 }), { rowHeight: 10, totalRows: rows.length });

    expect(cache.paint(recordingTarget().ctx, paintReq(0, 50))).toBe(true);

    // The strips did draw real content…
    expect(stripStrokes.length).toBeGreaterThan(0);
    // …and not one stroke ran translucent.
    expect(stripStrokes.every((a) => a === 1)).toBe(true);

    // Control: the overlay API does record the fades, so the assertion above
    // cannot pass vacuously.
    const control = strokeAlphaRecorder();
    renderer.drawDanglingStubs(control.ctx, rows, 0, rows.length, 0, 50);
    expect(control.alphasAtStroke.some((a) => a < 1)).toBe(true);
  });

  it("gives painters the tiling info needed to prime cross-seam connectors", () => {
    const requests: StripPaintRequest[] = [];
    const { factory } = fakeSurfaceFactory();
    const probe = createGraphStaticCache((req) => void requests.push(req), factory, {
      stripCssHeight: 20,
    });
    probe.sync(inputs({ dpr: 1 }), { rowHeight: 10, totalRows: 9 });
    probe.paint(recordingTarget().ctx, paintReq(0, 90)); // strips 0..4

    expect(requests.length).toBeGreaterThan(1);
    for (const req of requests) {
      const range = primedRowRange(req.firstRow, req.rowCount, 9);
      // The primed window reaches above the strip's own first row…
      expect(range.from).toBe(Math.max(0, req.firstRow - LOOKBACK_ROWS));
      expect(range.from).toBeLessThanOrEqual(req.firstRow);
      // …and covers the strip's last row, so a connector entering the strip
      // from the seam always finds its origin row inside the painted range.
      expect(range.to).toBe(Math.min(9, req.firstRow + req.rowCount - 1));
    }
  });
});
