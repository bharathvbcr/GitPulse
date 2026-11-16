import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  MAX_AVATAR_IMAGES,
  avatarImage,
  onAvatarImagesLoaded,
  resetAvatarImagesForTests,
} from "./avatarImages";

class FakeImage {
  static created: FakeImage[] = [];
  crossOrigin = "";
  referrerPolicy = "";
  decoding = "";
  naturalWidth = 0;
  complete = false;
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  private _src = "";
  constructor() {
    FakeImage.created.push(this);
  }
  set src(value: string) {
    this._src = value;
  }
  get src() {
    return this._src;
  }
  finish() {
    this.naturalWidth = 64;
    this.complete = true;
    this.onload?.();
  }
  fail() {
    this.onerror?.();
  }
}

beforeEach(() => {
  FakeImage.created = [];
  resetAvatarImagesForTests();
  vi.stubGlobal("Image", FakeImage);
  vi.useFakeTimers();
  vi.stubGlobal("requestAnimationFrame", (cb: () => void) => setTimeout(cb, 16));
});
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("avatarImage", () => {
  it("is null until loaded, with anonymous CORS and no referrer, and loads once", () => {
    expect(avatarImage("https://a/1")).toBeNull();
    expect(avatarImage("https://a/1")).toBeNull();
    expect(FakeImage.created).toHaveLength(1);
    const img = FakeImage.created[0];
    expect(img.crossOrigin).toBe("anonymous");
    expect(img.referrerPolicy).toBe("no-referrer");
    expect(img.decoding).toBe("async");
    expect(img.src).toBe("https://a/1");
    img.finish();
    expect(avatarImage("https://a/1")).toBe(img);
  });

  it("does not hand back an image that decoded to nothing", () => {
    avatarImage("https://a/empty");
    const img = FakeImage.created[0];
    img.complete = true;
    img.onload?.();
    expect(avatarImage("https://a/empty")).toBeNull();
  });

  it("remembers failures and never retries them", () => {
    avatarImage("https://a/bad");
    FakeImage.created[0].fail();
    expect(avatarImage("https://a/bad")).toBeNull();
    expect(avatarImage("https://a/bad")).toBeNull();
    expect(FakeImage.created).toHaveLength(1);
  });

  it("is bounded: the oldest URL is evicted and would load again", () => {
    for (let i = 0; i <= MAX_AVATAR_IMAGES; i++) avatarImage(`https://a/${i}`);
    expect(FakeImage.created).toHaveLength(MAX_AVATAR_IMAGES + 1);
    avatarImage("https://a/0");
    expect(FakeImage.created).toHaveLength(MAX_AVATAR_IMAGES + 2);
    avatarImage(`https://a/${MAX_AVATAR_IMAGES}`);
    expect(FakeImage.created).toHaveLength(MAX_AVATAR_IMAGES + 2);
  });
});

describe("onAvatarImagesLoaded", () => {
  it("coalesces many loads into one callback per frame", () => {
    const cb = vi.fn();
    onAvatarImagesLoaded(cb);
    for (let i = 0; i < 5; i++) avatarImage(`https://a/${i}`);
    for (const img of FakeImage.created) img.finish();
    expect(cb).not.toHaveBeenCalled();
    vi.advanceTimersByTime(20);
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it("stops calling after unsubscribe", () => {
    const cb = vi.fn();
    const stop = onAvatarImagesLoaded(cb);
    stop();
    avatarImage("https://a/x");
    FakeImage.created[0].finish();
    vi.advanceTimersByTime(20);
    expect(cb).not.toHaveBeenCalled();
  });
});
