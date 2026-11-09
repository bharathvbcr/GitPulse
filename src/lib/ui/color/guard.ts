/**
 * The contrast guard: one function that both the runtime (derived themes,
 * custom accents, imported themes) and the contract tests use to decide
 * whether a foreground is legible on its worst surface.
 *
 * `ensureContrast` moves a colour's OKLCH lightness away from the background in
 * 1% steps, keeping hue (and chroma, reduced only where sRGB cannot hold it),
 * until the WCAG ratio reaches the minimum. It refuses, rather than returning a
 * colour that failed, when it runs out of range at L 5% or L 95%.
 */
import {
  clampToGamut,
  contrastRatio,
  oklchToRgb,
  relativeLuminance,
  rgbToOklch,
  type Rgb,
} from "./oklch";

/** WCAG 2 AA for text, including accent-as-text and status-as-text. */
export const TEXT_MIN = 4.5;
/** WCAG 2.1 non-text contrast: edges, rings, chip fills. */
export const NON_TEXT_MIN = 3.0;

/** Lightness the search stops at, as a fraction. */
export const LIGHTNESS_FLOOR = 0.05;
export const LIGHTNESS_CEILING = 0.95;
/** Lightness moved per step. */
export const LIGHTNESS_STEP = 0.01;

export type GuardResult =
  | {
      readonly ok: true;
      readonly color: readonly [number, number, number];
      /** False when the input already passed and is returned untouched. */
      readonly adjusted: boolean;
      readonly ratio: number;
    }
  | {
      /** The colour could not reach `min` before L 5% / 95%. */
      readonly ok: false;
      /** Ratio at the last lightness tried. */
      readonly ratio: number;
    };

function assertRgb(name: string, rgb: Rgb): void {
  if (rgb.length !== 3 || !rgb.every((channel) => Number.isFinite(channel))) {
    throw new RangeError(`${name} must be three finite RGB channels`);
  }
}

/**
 * Nudges `fg` away from `bgWorst` until `contrastRatio(fg, bgWorst) >= min`.
 *
 * "Away" is decided by relative luminance: a foreground lighter than the
 * background gets lighter, otherwise darker; on a tie, a dark background gets a
 * lighter foreground. The search never reverses direction, so a refusal means
 * "no colour of this hue and chroma, further from the background, passes".
 *
 * Throws `RangeError` for a non-finite channel or `min` outside 1..21: those
 * are programmer errors, and a quiet fallback would read as a pass.
 */
export function ensureContrast(fg: Rgb, bgWorst: Rgb, min: number): GuardResult {
  assertRgb("fg", fg);
  assertRgb("bgWorst", bgWorst);
  if (!Number.isFinite(min) || min < 1 || min > 21) {
    throw new RangeError("min must be a finite contrast ratio between 1 and 21");
  }

  const start = contrastRatio(fg, bgWorst);
  if (start >= min) {
    return { ok: true, color: [fg[0], fg[1], fg[2]], adjusted: false, ratio: start };
  }

  const fgLuminance = relativeLuminance(fg);
  const bgLuminance = relativeLuminance(bgWorst);
  const lighter = fgLuminance === bgLuminance ? bgLuminance < 0.18 : fgLuminance > bgLuminance;
  const origin = rgbToOklch(fg);
  const limit = lighter ? LIGHTNESS_CEILING : LIGHTNESS_FLOOR;
  const room = lighter ? limit - origin.l : origin.l - limit;

  let last = start;
  if (room > 0) {
    const steps = Math.ceil(room / LIGHTNESS_STEP - 1e-9);
    for (let step = 1; step <= steps; step += 1) {
      const travelled = Math.min(step * LIGHTNESS_STEP, room);
      const l = lighter ? origin.l + travelled : origin.l - travelled;
      const candidate = oklchToRgb(clampToGamut({ l, c: origin.c, h: origin.h }));
      last = contrastRatio(candidate, bgWorst);
      if (last >= min) return { ok: true, color: candidate, adjusted: true, ratio: last };
    }
  }
  return { ok: false, ratio: last };
}
