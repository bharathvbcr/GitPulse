import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import {
  BACKDROP_IN_MS,
  BACKDROP_OUT_MS,
  CARD_IN_MS,
  CARD_OUT_MS,
  CARD_START,
  backdropFade,
  backdropFadeOut,
  cardScale,
  cardScaleOut,
} from "./transitions";

function media(matches: boolean) {
  return { matchMedia: () => ({ matches }) as MediaQueryList };
}

describe("modal transition params", () => {
  // Pin the standard profile: Node now exposes the host platform through
  // navigator. Mac behavior has its own coverage in macAppearance.test.ts.
  beforeEach(() => vi.stubGlobal("navigator", { platform: "Win32" }));
  afterEach(() => vi.unstubAllGlobals());

  it("pins exact durations; every OUT is strictly shorter than its IN twin", () => {
    expect(BACKDROP_IN_MS).toBe(140);
    expect(BACKDROP_OUT_MS).toBe(60);
    expect(CARD_IN_MS).toBe(180);
    expect(CARD_OUT_MS).toBe(60);
    expect(BACKDROP_OUT_MS).toBeLessThan(BACKDROP_IN_MS);
    expect(CARD_OUT_MS).toBeLessThan(CARD_IN_MS);
  });

  it("produces deterministic params that preserve the entrance feel", () => {
    expect(backdropFade(media(false))).toEqual({ duration: 140 });
    expect(backdropFadeOut(media(false))).toEqual({ duration: 60 });
    expect(cardScale(media(false))).toEqual({ duration: 180, start: 0.97 });
    expect(cardScaleOut(media(false))).toEqual({ duration: 60, start: 0.97 });
    expect(CARD_START).toBe(0.97);
  });

  it("collapses to zero under prefers-reduced-motion, including OUTs", () => {
    expect(backdropFade(media(true))).toEqual({ duration: 0 });
    expect(backdropFadeOut(media(true))).toEqual({ duration: 0 });
    expect(cardScale(media(true))).toEqual({ duration: 0, start: 0.97 });
    expect(cardScaleOut(media(true))).toEqual({ duration: 0, start: 0.97 });
  });

  it("treats a missing media source as motion-enabled", () => {
    expect(backdropFade(null)).toEqual({ duration: BACKDROP_IN_MS });
    expect(backdropFadeOut(null)).toEqual({ duration: BACKDROP_OUT_MS });
    expect(cardScale(null)).toEqual({ duration: CARD_IN_MS, start: CARD_START });
    expect(cardScaleOut(null)).toEqual({ duration: CARD_OUT_MS, start: CARD_START });
  });
});
