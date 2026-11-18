import { describe, expect, it } from "vitest";
import {
  MAX_WALLPAPER_EDGE,
  MAX_WALLPAPER_SOURCE_BYTES,
  MAX_WALLPAPER_VIDEO_BYTES,
  WALLPAPER_ACCEPT,
  WALLPAPER_TYPES,
  WALLPAPER_VIDEO_TYPES,
  checkWallpaperFile,
  fitWithin,
  isStored,
} from "./wallpaperStore";

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

describe("checkWallpaperFile for videos", () => {
  it("offers MP4, WebM and MOV to the decoder, as videos", () => {
    for (const type of WALLPAPER_VIDEO_TYPES) {
      const result = checkWallpaperFile(file(type, 1024, "loop.mp4"));
      expect(result.ok && result.kind, type).toBe("video");
    }
    const image = checkWallpaperFile(file("image/png"));
    expect(image.ok && image.kind).toBe("image");
  });

  it("holds videos to their own cap and refuses empty ones", () => {
    expect(checkWallpaperFile(file("video/mp4", MAX_WALLPAPER_VIDEO_BYTES, "a.mp4")).ok).toBe(true);
    expect(checkWallpaperFile(file("video/mp4", MAX_WALLPAPER_VIDEO_BYTES + 1, "a.mp4")).ok).toBe(false);
    expect(checkWallpaperFile(file("video/mp4", 0, "a.mp4")).ok).toBe(false);
  });

  it("names both kinds when it refuses a type", () => {
    const refused = checkWallpaperFile(file("video/x-msvideo", 10, "clip.avi"));
    expect(refused.ok).toBe(false);
    if (!refused.ok) expect(refused.reason).toContain("MP4, WebM or MOV");
  });

  it("offers every image and video type in the picker", () => {
    for (const type of [...WALLPAPER_TYPES, ...WALLPAPER_VIDEO_TYPES]) expect(WALLPAPER_ACCEPT.split(",")).toContain(type);
  });
});

describe("stored wallpaper records", () => {
  const still = new Blob(["jpeg"], { type: "image/jpeg" });

  it("reads a record saved before videos existed as an image", () => {
    expect(isStored({ blob: still, name: "old.jpg", width: 10, height: 10, savedAt: 1 })).toBe(true);
  });

  it("reads a still saved as bytes only with an image type", () => {
    const bytes = new ArrayBuffer(8);
    expect(isStored({ blob: bytes, blobType: "image/jpeg", name: "new.jpg", width: 10, height: 10, savedAt: 1 })).toBe(true);
    expect(isStored({ blob: bytes, name: "new.jpg", width: 10, height: 10, savedAt: 1 })).toBe(false);
    expect(isStored({ blob: bytes, blobType: "text/html", name: "new.jpg", width: 10, height: 10, savedAt: 1 })).toBe(false);
  });

  it("reads a video record, and refuses one whose video is not bytes of a video type", () => {
    const video = new ArrayBuffer(8);
    expect(isStored({ blob: still, name: "loop.mp4", width: 10, height: 10, savedAt: 1, video, videoType: "video/mp4" })).toBe(true);
    expect(isStored({ blob: still, name: "loop.mp4", width: 10, height: 10, savedAt: 1, video, videoType: "text/html" })).toBe(false);
    expect(isStored({ blob: still, name: "loop.mp4", width: 10, height: 10, savedAt: 1, video })).toBe(false);
    expect(isStored({ blob: still, name: "loop.mp4", width: 10, height: 10, savedAt: 1, video: "blob:x", videoType: "video/mp4" })).toBe(false);
    expect(isStored({ name: "no still", width: 10, height: 10 })).toBe(false);
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
