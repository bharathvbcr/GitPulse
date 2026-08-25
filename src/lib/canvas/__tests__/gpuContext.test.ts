import { describe, it, expect, vi } from "vitest";
import {
  gpu2dAttributes,
  acquireGpu2dContext,
  backingStoreSize,
  syncCanvasBackingStore,
  fillOpaqueBackground,
} from "../gpuContext";

function mockContext(overrides: Partial<CanvasRenderingContext2D> = {}) {
  return {
    imageSmoothingEnabled: false,
    imageSmoothingQuality: "low",
    setTransform: vi.fn(),
    fillRect: vi.fn(),
    fillStyle: "",
    ...overrides,
  } as unknown as CanvasRenderingContext2D;
}

describe("gpu canvas context", () => {
  it("requests an opaque, desynchronized, no-readback 2d context", () => {
    expect(gpu2dAttributes(true)).toEqual({
      alpha: false,
      desynchronized: true,
      colorSpace: "srgb",
      willReadFrequently: false,
    });
    expect(gpu2dAttributes(false).alpha).toBe(true);
  });

  it("falls back when desynchronized contexts are refused", () => {
    const ctx = mockContext();
    const getContext = vi
      .fn()
      .mockReturnValueOnce(null)
      .mockReturnValueOnce(ctx);
    const canvas = { getContext } as unknown as HTMLCanvasElement;

    expect(acquireGpu2dContext(canvas, true)).toBe(ctx);
    expect(getContext).toHaveBeenNthCalledWith(
      1,
      "2d",
      expect.objectContaining({ desynchronized: true, alpha: false }),
    );
    expect(getContext).toHaveBeenNthCalledWith(
      2,
      "2d",
      expect.objectContaining({ desynchronized: false, alpha: false }),
    );
    expect(ctx.imageSmoothingEnabled).toBe(true);
  });

  it("does not realloc the backing store when CSS size and DPR are unchanged", () => {
    const ctx = mockContext();
    const canvas = { width: 200, height: 100 } as HTMLCanvasElement;

    const first = syncCanvasBackingStore(canvas, ctx, 100, 50, 2);
    expect(first).toBe(false);
    expect(canvas.width).toBe(200);
    expect(ctx.setTransform).toHaveBeenCalledWith(2, 0, 0, 2, 0, 0);

    const resized = syncCanvasBackingStore(canvas, ctx, 110, 50, 2);
    expect(resized).toBe(true);
    expect(canvas.width).toBe(220);
    expect(canvas.height).toBe(100);
  });

  it("rounds backing-store pixels and never produces a zero texture", () => {
    expect(backingStoreSize(10.4, 0, 2)).toEqual({ width: 21, height: 1 });
    expect(backingStoreSize(0, 0, 0)).toEqual({ width: 1, height: 1 });
  });

  it("fills the CSS rectangle so an opaque context has no uncleared pixels", () => {
    const ctx = mockContext();
    fillOpaqueBackground(ctx, 120, 40, "#0d1117");
    expect(ctx.fillStyle).toBe("#0d1117");
    expect(ctx.fillRect).toHaveBeenCalledWith(0, 0, 120, 40);
  });
});
