/**
 * A model of the glass stack, used to measure the material-intensity slider's
 * floor (design language §14: "light theme over a dark desktop is unmeasured").
 *
 * The window is translucent, so the ground text sits on is not a token: it is
 * the desktop behind the window composited through every layer app.css paints.
 * This composites those layers source-over, from the desktop up, over a pure
 * white and a pure black desktop. Those are the extremes of what a desktop can
 * be, so a pass over both holds for any wallpaper (the native blur can only pull
 * a desktop toward its own mean, never outside that range).
 *
 * It is a model, not a pixel sample, and says so: the layer alphas below are
 * transcribed from app.css, and `materialModel.test.ts` compares every one with
 * the stylesheet so a drifted constant fails instead of quietly measuring a
 * material the app no longer ships.
 */
import { SHIPPED_LADDER, type Appearance } from "../color/derive";
import { contrastRatio, type Rgb } from "../color/oklch";
import { TEXT_MIN } from "../color/guard";

/** The shipped alpha of one layer, and the alpha it reaches at intensity 1. */
export interface Layer {
  readonly color: Rgb;
  readonly alpha: number;
}

export interface MaterialStack {
  /** `--mac-shell-veil`: `--c-bg` over the desktop. Not tuned by intensity. */
  readonly veil: number;
  /** `--mac-plate-fill`: the workspace plate. */
  readonly plate: Layer;
  /** `--mac-fill-surface`: panels, cards, rows. */
  readonly surface: Layer;
  /** `--mac-fill-surface-hover`: hover and selection, on top of a surface. */
  readonly hover: Layer;
  /** `--mac-fill-bg`: fields and inset areas, directly on the plate. */
  readonly fillBg: Layer;
  /** Largest alpha any hue-field blob reaches (its centre), at full gain. */
  readonly blobAlpha: number;
  /** `--mac-hue-gain`. */
  readonly gain: number;
  /** The four hue-field colours. */
  readonly blobs: readonly Rgb[];
}

const L = SHIPPED_LADDER;

/** Transcribed from app.css `html.macos` and `html.macos.light`. */
export const SHIPPED_STACK: Readonly<Record<Appearance, MaterialStack>> = {
  dark: {
    veil: 0.55,
    plate: { color: L.dark.bg, alpha: 0.46 },
    surface: { color: L.dark.surface, alpha: 0.5 },
    hover: { color: L.dark.surfaceHover, alpha: 0.78 },
    fillBg: { color: L.dark.bg, alpha: 0.42 },
    blobAlpha: 0.5,
    gain: 1,
    blobs: [
      [52, 78, 200],
      [12, 150, 170],
      [110, 66, 210],
      [180, 50, 130],
    ],
  },
  light: {
    veil: 0.62,
    plate: { color: [255, 255, 255], alpha: 0.6 },
    surface: { color: L.light.surface, alpha: 0.6 },
    hover: { color: L.light.surfaceHover, alpha: 0.82 },
    fillBg: { color: L.light.bg, alpha: 0.52 },
    blobAlpha: 0.5,
    gain: 0.5,
    blobs: [
      [120, 152, 255],
      [46, 196, 214],
      [168, 136, 252],
      [240, 122, 186],
    ],
  },
};

/**
 * Everything the measurement reads that a palette decides: the layer stack, the
 * colour the shell veil is painted in (`--c-bg`), and the two inks. The shipped
 * palette is the stylesheet's; an applied theme (an import or a built-in
 * preset) brings its own derived ladder and field, and its floor is measured
 * over that rather than assumed from the shipped one.
 */
export interface Palette {
  readonly stack: MaterialStack;
  readonly shell: Rgb;
  readonly text: Rgb;
  readonly textMuted: Rgb;
}

export function shippedPalette(appearance: Appearance): Palette {
  return {
    stack: SHIPPED_STACK[appearance],
    shell: L[appearance].bg,
    text: L[appearance].text,
    textMuted: L[appearance].textMuted,
  };
}

/** The `--c-*` triplets a derived palette carries that the stack reads. */
export interface PaletteTokens {
  readonly "--c-bg": string;
  readonly "--c-surface": string;
  readonly "--c-surface-hover": string;
  readonly "--c-text": string;
  readonly "--c-text-muted": string;
}

function triplet(value: string): Rgb {
  const [r, g, b] = value.trim().split(/\s+/).map(Number);
  return [r, g, b];
}

/**
 * The stack a derived palette draws, with the shipped alphas. Light mode's plate
 * stays white because app.css spells `--mac-plate-fill` as white there, not as
 * `--c-bg`; every other layer follows the palette.
 */
export function derivedPalette(
  appearance: Appearance,
  tokens: PaletteTokens,
  field: { readonly blobs: readonly Rgb[]; readonly gain: number },
): Palette {
  const shipped = SHIPPED_STACK[appearance];
  const bg = triplet(tokens["--c-bg"]);
  return {
    stack: {
      ...shipped,
      plate: { color: appearance === "dark" ? bg : shipped.plate.color, alpha: shipped.plate.alpha },
      surface: { color: triplet(tokens["--c-surface"]), alpha: shipped.surface.alpha },
      hover: { color: triplet(tokens["--c-surface-hover"]), alpha: shipped.hover.alpha },
      fillBg: { color: bg, alpha: shipped.fillBg.alpha },
      blobs: field.blobs,
      gain: field.gain,
    },
    shell: bg,
    text: triplet(tokens["--c-text"]),
    textMuted: triplet(tokens["--c-text-muted"]),
  };
}

export type Desktop = "white" | "black";
const DESKTOPS: Readonly<Record<Desktop, Rgb>> = {
  white: [255, 255, 255],
  black: [0, 0, 0],
};

/** Source-over of `color` at `alpha` onto `under`. */
export function over(color: Rgb, alpha: number, under: Rgb): Rgb {
  const a = Math.min(1, Math.max(0, alpha));
  return [
    color[0] * a + under[0] * (1 - a),
    color[1] * a + under[1] * (1 - a),
    color[2] * a + under[2] * (1 - a),
  ];
}

/** What `--gp-material` = `t` does to an alpha: `a + (1 - a) * t`. */
export function thicken(alpha: number, t: number): number {
  return alpha + (1 - alpha) * Math.min(1, Math.max(0, t));
}

export type GroundKind = "panel" | "field" | "hover";

export interface Ground {
  readonly kind: GroundKind;
  readonly desktop: Desktop;
  /** Which hue blob (or `none`) lies under this ground. */
  readonly blob: number | "none";
  readonly color: Rgb;
}

/**
 * Every ground text can sit on at intensity `t`: three surface kinds, over each
 * desktop, under each blob and under none. Nested panels only add coverage, so
 * one panel on the plate is the thinnest stack that exists.
 */
export function grounds(appearance: Appearance, t: number, palette: Palette = shippedPalette(appearance)): Ground[] {
  const { stack } = palette;
  const out: Ground[] = [];
  for (const desktop of Object.keys(DESKTOPS) as Desktop[]) {
    const shell = over(palette.shell, stack.veil, DESKTOPS[desktop]);
    const choices: (number | "none")[] = ["none", ...stack.blobs.map((_, index) => index)];
    for (const blob of choices) {
      const field =
        blob === "none" ? shell : over(stack.blobs[blob], stack.blobAlpha * stack.gain, shell);
      const plate = over(stack.plate.color, thicken(stack.plate.alpha, t), field);
      const panel = over(stack.surface.color, thicken(stack.surface.alpha, t), plate);
      out.push({ kind: "panel", desktop, blob, color: panel });
      out.push({
        kind: "field",
        desktop,
        blob,
        color: over(stack.fillBg.color, thicken(stack.fillBg.alpha, t), plate),
      });
      out.push({
        kind: "hover",
        desktop,
        blob,
        color: over(stack.hover.color, thicken(stack.hover.alpha, t), panel),
      });
    }
  }
  return out;
}

export interface Worst {
  readonly ratio: number;
  readonly ground: Ground;
}

/** The lowest contrast of `ink` over every ground at intensity `t`. */
export function worstContrast(
  appearance: Appearance,
  t: number,
  ink: Rgb,
  palette: Palette = shippedPalette(appearance),
): Worst {
  let worst: Worst | null = null;
  for (const ground of grounds(appearance, t, palette)) {
    const ratio = contrastRatio(ink, ground.color);
    if (worst === null || ratio < worst.ratio) worst = { ratio, ground };
  }
  if (worst === null) throw new Error("no grounds were produced");
  return worst;
}

/** The two inks the floor is held to: body text and the muted text under it. */
export function inks(appearance: Appearance): { text: Rgb; textMuted: Rgb } {
  return { text: L[appearance].text, textMuted: L[appearance].textMuted };
}

/** Intensity steps searched, coarse enough to read and fine enough to clamp to. */
export const INTENSITY_STEP = 0.05;

export interface FloorMeasurement {
  readonly appearance: Appearance;
  /** Lowest intensity at which text and muted text both reach 4.5:1 everywhere. */
  readonly floor: number;
  /** Worst ratios at the shipped alphas (intensity 0). */
  readonly atShipped: { readonly text: Worst; readonly textMuted: Worst };
  /** Worst ratios at the floor. */
  readonly atFloor: { readonly text: Worst; readonly textMuted: Worst };
}

export function measureFloor(
  appearance: Appearance,
  palette: Palette = shippedPalette(appearance),
): FloorMeasurement {
  const ink = { text: palette.text, textMuted: palette.textMuted };
  const at = (t: number) => ({
    text: worstContrast(appearance, t, ink.text, palette),
    textMuted: worstContrast(appearance, t, ink.textMuted, palette),
  });
  const passes = (t: number) => {
    const worst = at(t);
    return worst.text.ratio >= TEXT_MIN && worst.textMuted.ratio >= TEXT_MIN;
  };
  let floor = 0;
  while (floor < 1 && !passes(floor)) floor = Math.round((floor + INTENSITY_STEP) * 100) / 100;
  return { appearance, floor, atShipped: at(0), atFloor: at(floor) };
}

/** The lowest intensity the slider may reach: the larger of the two themes' floors. */
export function materialIntensityFloor(): number {
  return Math.max(measureFloor("dark").floor, measureFloor("light").floor);
}
