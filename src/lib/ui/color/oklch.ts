/**
 * Color math for the theme engine: sRGB, linear light, OKLab, OKLCH, hex and
 * the WCAG 2 contrast ratio. Pure functions, no dependencies.
 *
 * OKLab matrices are Bjorn Ottosson's published ones (the CSS Color 4
 * reference). `relativeLuminance` deliberately uses the 0.03928 breakpoint
 * that `accents.contract.test.ts` and `themeContrast.test.ts` use, rather than
 * the 0.04045 of the sRGB standard that the colour-space conversion uses: the
 * runtime guard and the contract tests must report the same ratio for the same
 * pair, so this module follows the contract tests' formula.
 */

/** sRGB channels in 0..255. Integers when they came from a hex string. */
export type Rgb = readonly [number, number, number];

export interface Oklab {
  readonly l: number;
  readonly a: number;
  readonly b: number;
}

/** `l` in 0..1, `c` >= 0, `h` in degrees [0, 360). */
export interface Oklch {
  readonly l: number;
  readonly c: number;
  readonly h: number;
}

/** Chroma below this has no meaningful hue. */
export const ACHROMATIC_CHROMA = 1e-4;

/** Slack, in linear light, when deciding that a colour is inside sRGB. */
const GAMUT_EPSILON = 1e-4;

function srgbToLinear(channel01: number): number {
  return channel01 <= 0.04045 ? channel01 / 12.92 : ((channel01 + 0.055) / 1.055) ** 2.4;
}

function linearToSrgb(linear: number): number {
  return linear <= 0.0031308 ? linear * 12.92 : 1.055 * linear ** (1 / 2.4) - 0.055;
}

/** Linear-light RGB (0..1, unclamped) from an OKLab colour. */
function oklabToLinear(lab: Oklab): [number, number, number] {
  const l_ = lab.l + 0.3963377774 * lab.a + 0.2158037573 * lab.b;
  const m_ = lab.l - 0.1055613458 * lab.a - 0.0638541728 * lab.b;
  const s_ = lab.l - 0.0894841775 * lab.a - 1.291485548 * lab.b;
  const l = l_ ** 3;
  const m = m_ ** 3;
  const s = s_ ** 3;
  return [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ];
}

export function rgbToOklab(rgb: Rgb): Oklab {
  const r = srgbToLinear(rgb[0] / 255);
  const g = srgbToLinear(rgb[1] / 255);
  const b = srgbToLinear(rgb[2] / 255);
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  return {
    l: 0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    a: 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    b: 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  };
}

export function oklabToOklch(lab: Oklab): Oklch {
  const c = Math.hypot(lab.a, lab.b);
  if (c < ACHROMATIC_CHROMA) return { l: lab.l, c, h: 0 };
  const degrees = (Math.atan2(lab.b, lab.a) * 180) / Math.PI;
  return { l: lab.l, c, h: degrees < 0 ? degrees + 360 : degrees };
}

export function oklchToOklab(lch: Oklch): Oklab {
  const radians = (lch.h * Math.PI) / 180;
  return { l: lch.l, a: lch.c * Math.cos(radians), b: lch.c * Math.sin(radians) };
}

export function rgbToOklch(rgb: Rgb): Oklch {
  return oklabToOklch(rgbToOklab(rgb));
}

/** Wraps degrees into [0, 360). */
export function normalizeHue(degrees: number): number {
  const wrapped = degrees % 360;
  return wrapped < 0 ? wrapped + 360 : wrapped;
}

/** Whether the colour is displayable in sRGB (within `GAMUT_EPSILON`). */
export function inSrgbGamut(lch: Oklch): boolean {
  return oklabToLinear(oklchToOklab(lch)).every(
    (channel) => channel >= -GAMUT_EPSILON && channel <= 1 + GAMUT_EPSILON,
  );
}

/**
 * Brings an OKLCH colour into sRGB by reducing chroma at fixed lightness and
 * hue (binary search for the largest in-gamut chroma). Lightness is clamped to
 * 0..1 first; negative or non-finite chroma becomes 0.
 */
export function clampToGamut(lch: Oklch): Oklch {
  const l = Math.min(1, Math.max(0, Number.isFinite(lch.l) ? lch.l : 0));
  const h = Number.isFinite(lch.h) ? normalizeHue(lch.h) : 0;
  const c = Number.isFinite(lch.c) ? Math.max(0, lch.c) : 0;
  const wanted: Oklch = { l, c, h };
  if (inSrgbGamut(wanted)) return wanted;
  let lo = 0;
  let hi = c;
  for (let i = 0; i < 24; i += 1) {
    const mid = (lo + hi) / 2;
    if (inSrgbGamut({ l, c: mid, h })) lo = mid;
    else hi = mid;
  }
  return { l, c: lo, h };
}

/** OKLCH to 8-bit sRGB: gamut-clamped (chroma reduced), then rounded. */
export function oklchToRgb(lch: Oklch): [number, number, number] {
  const linear = oklabToLinear(oklchToOklab(clampToGamut(lch)));
  const encode = (channel: number): number =>
    Math.round(Math.min(1, Math.max(0, linearToSrgb(Math.min(1, Math.max(0, channel))))) * 255);
  return [encode(linear[0]), encode(linear[1]), encode(linear[2])];
}

const HEX_PATTERN = /^#(?:[0-9a-fA-F]{3}|[0-9a-fA-F]{6})$/;

/** `#rgb` or `#rrggbb` only. Anything else (alpha, names, functions) is null. */
export function parseHex(text: unknown): [number, number, number] | null {
  if (typeof text !== "string" || !HEX_PATTERN.test(text)) return null;
  const digits =
    text.length === 4
      ? [text[1], text[2], text[3]].map((digit) => digit + digit)
      : [text.slice(1, 3), text.slice(3, 5), text.slice(5, 7)];
  return [
    Number.parseInt(digits[0], 16),
    Number.parseInt(digits[1], 16),
    Number.parseInt(digits[2], 16),
  ];
}

/** Lowercase `#rrggbb`. Channels are rounded and clamped to 0..255. */
export function formatHex(rgb: Rgb): string {
  return `#${rgb
    .map((channel) =>
      Math.min(255, Math.max(0, Math.round(Number.isFinite(channel) ? channel : 0)))
        .toString(16)
        .padStart(2, "0"),
    )
    .join("")}`;
}

/** `R G B` triplet, the format of the `--c-*` custom properties. */
export function formatTriplet(rgb: Rgb): string {
  return rgb
    .map((channel) =>
      Math.min(255, Math.max(0, Math.round(Number.isFinite(channel) ? channel : 0))),
    )
    .join(" ");
}

/** WCAG 2 relative luminance. Same formula as the contract tests. */
export function relativeLuminance(rgb: Rgb): number {
  const [r, g, b] = rgb.map((channel) => {
    const value = channel / 255;
    return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

/** WCAG 2 contrast ratio, 1..21, symmetric in its arguments. */
export function contrastRatio(foreground: Rgb, background: Rgb): number {
  const a = relativeLuminance(foreground);
  const b = relativeLuminance(background);
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}
