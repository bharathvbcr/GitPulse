import { backingStoreSize, tuneGpu2dContext } from "./gpuContext";
import type { GraphTheme } from "./GraphRenderer";

/**
 * Offscreen static-layer cache for the commit graph.
 *
 * The graph's geometry is scroll-invariant apart from its vertical offset, so
 * connectors, nodes and decorations are rendered once into tile strips and
 * blitted per frame; only the live emphasis rings (hover/selection/HEAD) are
 * re-stroked by the caller on top. This turns a scroll frame from a full
 * rebuild into a couple of `drawImage` calls.
 *
 * The cache is deliberately dependency-injected (`StripPainter`,
 * `SurfaceFactory`) so the tiling, eviction and slicing math is testable in a
 * plain Node environment with recording fakes.
 */

export interface GraphCacheInputs {
  /** Bumped whenever the row data behind the graph changes identity. */
  dataVersion: number;
  /** CSS width of the graph gutter. */
  cssWidth: number;
  /** Density mode signature ("spacious" / "compact"). */
  densitySignature: string;
  /** Resolved theme colors joined into one signature. */
  themeSignature: string;
  /** Device pixel ratio the strips are rasterized at. */
  dpr: number;
}

/** Structural equality; a changed field means "drop every strip". */
export function sameCacheInputs(a: GraphCacheInputs | null, b: GraphCacheInputs): boolean {
  if (!a) return false;
  return (
    a.dataVersion === b.dataVersion &&
    a.cssWidth === b.cssWidth &&
    a.densitySignature === b.densitySignature &&
    a.themeSignature === b.themeSignature &&
    a.dpr === b.dpr
  );
}

/** Stable, human-readable key — mainly an aid for debugging and tests. */
export function computeCacheKey(inputs: GraphCacheInputs): string {
  const dpr = Number(inputs.dpr.toFixed(4));
  return `${inputs.dataVersion}:${inputs.cssWidth}:${inputs.densitySignature}:${inputs.themeSignature}:${dpr}`;
}

/** One stable signature string for a resolved theme. */
export function themeSignatureOf(theme: GraphTheme): string {
  return [theme.background, theme.nodeStroke, theme.selection, theme.head, theme.muted].join("|");
}

export interface StripPaintRequest {
  ctx: CanvasRenderingContext2D;
  /** First content row this strip covers. */
  firstRow: number;
  /** Rows covered (the final strip may be short). */
  rowCount: number;
  /** Content-space CSS y of the strip top (equals firstRow * rowHeight). */
  stripTopCss: number;
  /** CSS height of the strip surface, for culling. */
  viewportCssHeight: number;
}

/**
 * Renders one strip of static graph content. The context is pre-scaled so all
 * coordinates are CSS pixels in *content space*: row r's centre lands at
 * r * rowHeight + rowHeight/2 - stripTopCss.
 */
export type StripPainter = (req: StripPaintRequest) => void;

export interface CachedSurface {
  canvas: HTMLCanvasElement;
  ctx: CanvasRenderingContext2D;
  /**
   * Invoked exactly once when the cache surrenders the surface — LRU
   * eviction, invalidation, an input-change reset, or dispose. Optional:
   * dropping the reference alone lets GC reclaim a detached canvas, but a
   * hook here makes retention testable and gives hosts eager teardown.
   */
  release?: () => void;
}

export type SurfaceFactory = (
  cssWidth: number,
  cssHeight: number,
  dpr: number,
) => CachedSurface | null;

/**
 * Default strip surface: detached opaque canvas sized by the shared backing
 * store math. Runs only where `document` exists (browser).
 */
export function createOffscreenSurface(
  cssWidth: number,
  cssHeight: number,
  dpr: number,
): CachedSurface | null {
  if (typeof document === "undefined") return null;
  const canvas = document.createElement("canvas");
  const size = backingStoreSize(cssWidth, cssHeight, dpr);
  canvas.width = size.width;
  canvas.height = size.height;
  const ctx = canvas.getContext("2d", { alpha: false });
  if (!ctx) return null;
  tuneGpu2dContext(ctx);
  return { canvas, ctx };
}

export interface GraphCacheGeometry {
  rowHeight: number;
  totalRows: number;
}

export interface BlitRequest {
  /** Content-space CSS y of the viewport top (the scroller's scrollTop). */
  contentTopCss: number;
  viewportHeightCss: number;
}

export interface GraphStaticCacheOptions {
  /** Target CSS height of a strip; snapped down to whole rows. */
  stripCssHeight?: number;
  /** LRU cap on live strips. */
  maxStrips?: number;
}

export interface GraphStaticCacheStats {
  liveStrips: number;
  stripRenders: number;
}

export interface GraphStaticCache {
  /**
   * Updates key inputs and geometry. Any input change drops the strips;
   * growing `totalRows` keeps them (history appends older commits below).
   */
  sync(inputs: GraphCacheInputs, geometry: GraphCacheGeometry): void;
  /**
   * Blits the visible strips at their content offsets. Returns true when the
   * static layer covers the request; false means the caller must fall back to
   * a direct full render (cache bypassed or empty).
   */
  paint(target: CanvasRenderingContext2D, req: BlitRequest): boolean;
  /** Drops every strip. */
  invalidate(): void;
  /** Releases all surfaces; the instance must not be used afterwards. */
  dispose(): void;
  stats(): GraphStaticCacheStats;
}

const DEFAULT_STRIP_CSS_HEIGHT = 512;
const DEFAULT_MAX_STRIPS = 4;

export function createGraphStaticCache(
  painter: StripPainter,
  createSurface: SurfaceFactory,
  options: GraphStaticCacheOptions = {},
): GraphStaticCache {
  const stripCssHeight = options.stripCssHeight ?? DEFAULT_STRIP_CSS_HEIGHT;
  const maxStrips = Math.max(1, options.maxStrips ?? DEFAULT_MAX_STRIPS);

  let inputs: GraphCacheInputs | null = null;
  let rowHeight = 0;
  let totalRows = 0;
  let rowsPerStrip = 0;
  let stripRenders = 0;
  /** Insertion-ordered map doubles as the LRU list (oldest first). */
  const strips = new Map<number, CachedSurface>();

  function computeRowsPerStrip(): number {
    if (rowHeight <= 0) return 0;
    return Math.max(1, Math.floor(stripCssHeight / rowHeight));
  }

  function usable(): boolean {
    // Graceful bypass: degenerate graphs or zero-width gutters render direct.
    return inputs !== null && inputs.dpr > 0 && inputs.cssWidth > 0 && totalRows >= 2 && rowsPerStrip > 0;
  }

  function touch(index: number, surface: CachedSurface): CachedSurface {
    strips.delete(index);
    strips.set(index, surface);
    return surface;
  }

  function evictOverflow(): void {
    while (strips.size > maxStrips) {
      const oldest = strips.entries().next();
      if (oldest.done) break;
      strips.delete(oldest.value[0]);
      oldest.value[1].release?.();
    }
  }

  /** Drops every strip, releasing each surface exactly once. */
  function clearStrips(): void {
    if (strips.size === 0) return;
    for (const surface of strips.values()) surface.release?.();
    strips.clear();
  }

  function ensureStrip(index: number): CachedSurface | null {
    const cached = strips.get(index);
    if (cached) return touch(index, cached);

    if (!inputs) return null;
    const firstRow = index * rowsPerStrip;
    const rowCount = Math.min(rowsPerStrip, totalRows - firstRow);
    if (rowCount <= 0) return null;

    // Make room before allocating: the superseded surface is released before
    // its replacement exists, so live memory never peaks above the LRU cap.
    if (strips.size >= maxStrips) {
      const oldest = strips.entries().next();
      if (!oldest.done) {
        strips.delete(oldest.value[0]);
        oldest.value[1].release?.();
      }
    }

    const cssHeight = rowCount * rowHeight;
    const surface = createSurface(inputs.cssWidth, cssHeight, inputs.dpr);
    if (!surface) return null;

    const scale = inputs.dpr > 0 ? inputs.dpr : 1;
    surface.ctx.setTransform(scale, 0, 0, scale, 0, 0);
    painter({
      ctx: surface.ctx,
      firstRow,
      rowCount,
      stripTopCss: firstRow * rowHeight,
      viewportCssHeight: cssHeight,
    });
    stripRenders += 1;

    strips.set(index, surface);
    evictOverflow();
    return surface;
  }

  function sync(next: GraphCacheInputs, geometry: GraphCacheGeometry): void {
    const changed =
      !sameCacheInputs(inputs, next) ||
      geometry.rowHeight !== rowHeight ||
      geometry.totalRows < totalRows;
    rowHeight = geometry.rowHeight;
    totalRows = geometry.totalRows;
    rowsPerStrip = computeRowsPerStrip();
    if (changed) clearStrips();
    inputs = { ...next };
  }

  function paint(target: CanvasRenderingContext2D, req: BlitRequest): boolean {
    if (!usable() || !inputs) return false;
    const dpr = inputs.dpr;

    const viewTop = req.contentTopCss;
    const viewBottom = viewTop + req.viewportHeightCss;
    const contentBottom = totalRows * rowHeight;
    const clippedBottom = Math.min(viewBottom, contentBottom);
    if (clippedBottom <= viewTop) return false;

    const stripSpan = rowsPerStrip * rowHeight;
    const firstStrip = Math.floor(viewTop / stripSpan);
    const lastStrip = Math.floor((clippedBottom - 1) / stripSpan);

    for (let s = firstStrip; s <= lastStrip; s++) {
      const surface = ensureStrip(s);
      if (!surface) return false;

      const stripTopCss = s * stripSpan;
      const rowsHere = Math.min(rowsPerStrip, totalRows - s * rowsPerStrip);
      const stripBottomCss = Math.min(stripTopCss + rowsHere * rowHeight, contentBottom);

      const srcTopCss = Math.max(viewTop, stripTopCss);
      const srcBottomCss = Math.min(clippedBottom, stripBottomCss);
      if (srcBottomCss <= srcTopCss) continue;

      // Source rect in device pixels, clamped to the actual texture; the
      // destination rect derives from it so texel mapping stays 1:1 even when
      // round(css*dpr) nudges the surface height (non-integer dpr).
      const sy = (srcTopCss - stripTopCss) * dpr;
      const sh = Math.min((srcBottomCss - srcTopCss) * dpr, surface.canvas.height - sy);
      if (sh <= 0) continue;
      // Destination is viewport space: the visible canvas is not translated
      // by scrollTop, so a content-space dest Y would paint the graph 180px
      // (or however far the user scrolled) below the overlay rings and hits.
      const destY = srcTopCss - viewTop;
      target.drawImage(
        surface.canvas,
        0,
        sy,
        surface.canvas.width,
        sh,
        0,
        destY,
        surface.canvas.width / dpr,
        sh / dpr,
      );
    }
    return true;
  }

  function invalidate(): void {
    clearStrips();
  }

  function dispose(): void {
    clearStrips();
    inputs = null;
    totalRows = 0;
    rowsPerStrip = 0;
  }

  function stats(): GraphStaticCacheStats {
    return { liveStrips: strips.size, stripRenders };
  }

  return { sync, paint, invalidate, dispose, stats };
}
