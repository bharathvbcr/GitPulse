import { motionDuration, type MediaMatch } from "../motion/easing";
import { BAKED_SPRINGS, type SpringName } from "../motion/springs.generated";
import { DURATION_MS, EXIT_EASE, EXIT_MS } from "../motion/tokens";
import { isMacOS } from "../platform";
import type { CrossfadeParams, ScaleParams } from "svelte/transition";

/**
 * Shared transition params for every modal backdrop and card — one seam so
 * overlays stay consistent and reduced-motion handling stays in easing.ts.
 *
 * Every duration and curve below is a motion token (src/lib/motion/tokens.ts,
 * mirrored in app.css as `--gp-dur-*` / `--gp-ease-*`); none is a literal of
 * this file. The one exception is `LIQUID_SELECTION_MS`, which the design
 * language fixes at 280 between the `base` and `slow` tokens.
 *
 * Invariant: each OUT duration is strictly shorter than its IN twin. Long
 * outros made a close→reopen toggle stack two blurred backdrops mid-fade and
 * pulse the screen darker; exits must yield almost immediately instead. The
 * exit tokens (`EXIT_MS`) are capped at 70 ms, so the invariant holds by
 * construction and `transitions.test.ts` pins it.
 */
export const BACKDROP_IN_MS = DURATION_MS.fast;
export const BACKDROP_OUT_MS = EXIT_MS.fast;
export const CARD_IN_MS = DURATION_MS.slow;
export const CARD_OUT_MS = EXIT_MS.slow;
/** The shared-element pill that glides between tabs (spec 8.4 keeps it at 280). */
export const LIQUID_SELECTION_MS = 280;
/** Preserved from motion/easing scaleParams so entrances look unchanged. */
export const CARD_START = 0.97;
/** macOS opens a card from slightly closer to full size. */
export const CARD_START_MAC = 0.985;

type EasingFn = (t: number) => number;

/**
 * A baked spring as a JS easing, for Svelte transitions (which take a function,
 * not the CSS `linear()` string the stylesheet uses). It reads the same
 * `[progress, value]` stops, so the two engines play one curve. Values may
 * overshoot 1 (that is the spring); progress outside 0..1 is clamped.
 */
export function springEasing(name: Exclude<SpringName, "standard">): EasingFn {
  const stops = BAKED_SPRINGS[name].points;
  return (t) => {
    if (t <= 0) return 0;
    if (t >= 1) return 1;
    for (let index = 1; index < stops.length; index += 1) {
      const [x1, y1] = stops[index];
      if (t <= x1) {
        const [x0, y0] = stops[index - 1];
        return y0 + ((y1 - y0) * (t - x0)) / (x1 - x0);
      }
    }
    return 1;
  };
}

/** Parses `cubic-bezier(x1, y1, x2, y2)`; throws on anything else so a token edit cannot silently fall back. */
function parseCubicBezier(value: string): readonly [number, number, number, number] {
  const match = /^cubic-bezier\(\s*([\d.]+)\s*,\s*(-?[\d.]+)\s*,\s*([\d.]+)\s*,\s*(-?[\d.]+)\s*\)$/.exec(value);
  if (!match) throw new Error(`not a cubic-bezier easing: ${value}`);
  return [Number(match[1]), Number(match[2]), Number(match[3]), Number(match[4])];
}

/** A CSS `cubic-bezier` as a JS easing (Newton's method on x, bisection as the guard). */
export function cubicBezierEasing(x1: number, y1: number, x2: number, y2: number): EasingFn {
  const sample = (a: number, b: number, s: number) => 3 * a * (1 - s) * (1 - s) * s + 3 * b * (1 - s) * s * s + s * s * s;
  const slope = (a: number, b: number, s: number) => 3 * a * (1 - s) * (1 - s) + 6 * (b - a) * (1 - s) * s + 3 * (1 - b) * s * s;
  return (x) => {
    if (x <= 0) return 0;
    if (x >= 1) return 1;
    let s = x;
    for (let step = 0; step < 8; step += 1) {
      const error = sample(x1, x2, s) - x;
      if (Math.abs(error) < 1e-6) return sample(y1, y2, s);
      const derivative = slope(x1, x2, s);
      if (Math.abs(derivative) < 1e-6) break;
      s -= error / derivative;
    }
    let low = 0;
    let high = 1;
    s = x;
    for (let step = 0; step < 32; step += 1) {
      const error = sample(x1, x2, s) - x;
      if (Math.abs(error) < 1e-6) break;
      if (error > 0) high = s;
      else low = s;
      s = (low + high) / 2;
    }
    return sample(y1, y2, s);
  };
}

const spatial = springEasing("spatial");
const exit = cubicBezierEasing(...parseCubicBezier(EXIT_EASE));

export function backdropFade(media?: MediaMatch | null): { duration: number } {
  return { duration: motionDuration(BACKDROP_IN_MS, media) };
}

export function backdropFadeOut(media?: MediaMatch | null): { duration: number } {
  return { duration: motionDuration(BACKDROP_OUT_MS, media) };
}

export function cardScale(media?: MediaMatch | null): ScaleParams & { duration: number; start: number } {
  return {
    duration: motionDuration(CARD_IN_MS, media),
    start: isMacOS() ? CARD_START_MAC : CARD_START,
    easing: spatial,
  };
}

export function cardScaleOut(media?: MediaMatch | null): ScaleParams & { duration: number; start: number } {
  return {
    duration: motionDuration(CARD_OUT_MS, media),
    start: isMacOS() ? CARD_START_MAC : CARD_START,
    easing: exit,
  };
}

export function liquidSelection(media?: MediaMatch | null): CrossfadeParams {
  return {
    easing: spatial,
    // An outgoing pill may have mounted before the system preference changed.
    // Svelte evaluates this callback when each transition actually starts.
    duration: () => motionDuration(LIQUID_SELECTION_MS, media),
  };
}
