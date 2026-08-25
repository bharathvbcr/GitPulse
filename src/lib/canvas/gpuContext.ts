export interface Gpu2dAttributes {
  alpha: boolean;
  desynchronized: boolean;
  colorSpace: "srgb";
  willReadFrequently: false;
}

/**
 * Context flags that keep the backing store on the GPU compositor:
 * opaque (no per-pixel blend with the page), desynchronized (low-latency
 * present), and never marked for readback.
 *
 * Platform note: `desynchronized` is a Chromium-only hint — WKWebView parses
 * and ignores it — so on macOS the operative flag is `alpha: false`; an
 * opaque backing store is what skips per-pixel blending in the WebKit
 * compositor. Don't go hunting for a WebKit equivalent of the other knob.
 */
export function gpu2dAttributes(opaque = true): Gpu2dAttributes {
  return {
    alpha: !opaque,
    desynchronized: true,
    colorSpace: "srgb",
    willReadFrequently: false,
  };
}

export function tuneGpu2dContext(ctx: CanvasRenderingContext2D): void {
  ctx.imageSmoothingEnabled = true;
  if ("imageSmoothingQuality" in ctx) {
    ctx.imageSmoothingQuality = "high";
  }
}

export function acquireGpu2dContext(
  canvas: HTMLCanvasElement,
  opaque = true,
): CanvasRenderingContext2D | null {
  const preferred = gpu2dAttributes(opaque);
  const ctx =
    canvas.getContext("2d", preferred) ??
    canvas.getContext("2d", { ...preferred, desynchronized: false });
  if (!ctx) return null;
  tuneGpu2dContext(ctx);
  return ctx;
}

export function backingStoreSize(
  cssWidth: number,
  cssHeight: number,
  dpr: number,
): { width: number; height: number } {
  const scale = dpr > 0 ? dpr : 1;
  return {
    width: Math.max(1, Math.round(cssWidth * scale)),
    height: Math.max(1, Math.round(cssHeight * scale)),
  };
}

/**
 * Resize the backing store only when CSS size or DPR actually changed.
 * Assigning `canvas.width` resets the GPU texture and the 2D state — doing
 * that on every scroll frame is the usual source of graph jank.
 */
export function syncCanvasBackingStore(
  canvas: HTMLCanvasElement,
  ctx: CanvasRenderingContext2D,
  cssWidth: number,
  cssHeight: number,
  dpr: number,
): boolean {
  const { width, height } = backingStoreSize(cssWidth, cssHeight, dpr);
  const resized = canvas.width !== width || canvas.height !== height;
  if (resized) {
    canvas.width = width;
    canvas.height = height;
    tuneGpu2dContext(ctx);
  }
  ctx.setTransform(dpr > 0 ? dpr : 1, 0, 0, dpr > 0 ? dpr : 1, 0, 0);
  return resized;
}

export function fillOpaqueBackground(
  ctx: CanvasRenderingContext2D,
  cssWidth: number,
  cssHeight: number,
  color: string,
): void {
  ctx.fillStyle = color;
  ctx.fillRect(0, 0, cssWidth, cssHeight);
}
