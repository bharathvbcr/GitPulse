import { motionDuration, type MediaMatch } from "../motion/easing";

/**
 * Shared transition params for every modal backdrop and card — one seam so
 * overlays stay consistent and reduced-motion handling stays in easing.ts.
 *
 * Invariant: each OUT duration is strictly shorter than its IN twin. Long
 * outros made a close→reopen toggle stack two blurred backdrops mid-fade and
 * pulse the screen darker; exits must yield almost immediately instead.
 */
export const BACKDROP_IN_MS = 140;
export const BACKDROP_OUT_MS = 60;
export const CARD_IN_MS = 180;
export const CARD_OUT_MS = 60;
/** Preserved from motion/easing scaleParams so entrances look unchanged. */
export const CARD_START = 0.97;

export function backdropFade(media?: MediaMatch | null): { duration: number } {
  return { duration: motionDuration(BACKDROP_IN_MS, media) };
}

export function backdropFadeOut(media?: MediaMatch | null): { duration: number } {
  return { duration: motionDuration(BACKDROP_OUT_MS, media) };
}

export function cardScale(media?: MediaMatch | null): { duration: number; start: number } {
  return { duration: motionDuration(CARD_IN_MS, media), start: CARD_START };
}

export function cardScaleOut(media?: MediaMatch | null): { duration: number; start: number } {
  return { duration: motionDuration(CARD_OUT_MS, media), start: CARD_START };
}
