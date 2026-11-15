import { describe, expect, it } from "vitest";
import { MAX_WALLPAPER_EDGE, MAX_WALLPAPER_SOURCE_BYTES, checkWallpaperFile, fitWithin } from "./wallpaperStore";

const file = (type: string, size = 1024, name = "photo.png") => ({ type, size, name });

describe("checkWallpaperFile", () => {
  it("accepts the raster types", () => {
    for (const type of ["image/png", "image/jpeg", "image/webp", "image/gif", "image/avif"]) {
      expect(checkWallpaperFile(file(type)).ok, type).toBe(true);
    }
  });

  it("refuses SVG by type and by name, since an SVG is a document rather than a picture", () => {
    expect(checkWallpaperFile(file("image/svg+xml", 10, "a.svg")).ok).toBe(false);
    expect(checkWallpaperFile(file("image/png", 10, "sneaky.svg")).ok).toBe(false);
    expect(checkWallpaperFile(file("image/png", 10, "sneaky.SVGZ")).ok).toBe(false);
  });

  it("refuses non-images, empty files and files past the cap", () => {
    expect(checkWallpaperFile(file("text/html")).ok).toBe(false);
    expect(checkWallpaperFile(file("")).ok).toBe(false);
    expect(checkWallpaperFile(file("image/png", 0)).ok).toBe(false);
    expect(checkWallpaperFile(file("image/png", MAX_WALLPAPER_SOURCE_BYTES + 1)).ok).toBe(false);
    expect(checkWallpaperFile(file("image/png", MAX_WALLPAPER_SOURCE_BYTES)).ok).toBe(true);
  });
});

describe("fitWithin", () => {
  it("never upscales", () => {
    expect(fitWithin(800, 600)).toEqual({ width: 800, height: 600 });
  });

  it("holds the longest edge to the cap and keeps the aspect", () => {
    expect(fitWithin(6000, 4000)).toEqual({ width: MAX_WALLPAPER_EDGE, height: 1707 });
    expect(fitWithin(1000, 5120)).toEqual({ width: 500, height: MAX_WALLPAPER_EDGE });
  });

  it("never returns a zero dimension", () => {
    expect(fitWithin(100_000, 1)).toEqual({ width: MAX_WALLPAPER_EDGE, height: 1 });
  });
});
