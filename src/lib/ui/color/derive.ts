/**
 * Linear-style theme derivation: three inputs (`base`, `accent`, `contrast`)
 * plus an appearance become the whole `--c-*` colour set, as `R G B` triplets
 * in the format app.css declares them in.
 *
 * Surfaces and ink are not hand-tuned. The lightness / chroma / hue offsets of
 * `surface`, `surface-hover`, `border`, `text` and `text-muted` from `bg` are
 * measured, at module load, from the triplets app.css ships (`SHIPPED_LADDER`
 * below). Applied to the shipped base at contrast 0.5 they give those triplets
 * back exactly; applied to another base they keep the same optical spacing.
 * `derive.test.ts` reads app.css and fails if `SHIPPED_LADDER` drifts from it.
 *
 * Everything that is read as text, an accent or a status colour then goes
 * through `ensureContrast` against the worst surface. A colour the guard
 * refuses is reported by key; it is never returned as if it had passed.
 */
import { TEXT_MIN, ensureContrast } from "./guard";
import {
  clampToGamut,
  contrastRatio,
  formatTriplet,
  normalizeHue,
  oklchToRgb,
  parseHex,
  rgbToOklch,
  type Oklch,
  type Rgb,
} from "./oklch";

export type Appearance = "dark" | "light";

/** The `--c-*` names this module produces. */
export const TOKEN_KEYS = [
  "--c-bg",
  "--c-surface",
  "--c-surface-hover",
  "--c-border",
  "--c-text",
  "--c-text-muted",
  "--c-text-faint",
  "--c-accent",
  "--c-on-accent",
  "--c-ok",
  "--c-on-ok",
  "--c-warn",
  "--c-on-warn",
  "--c-danger",
  "--c-on-danger",
  "--c-info",
  "--c-on-info",
  "--c-unknown",
  "--c-on-unknown",
] as const;

export type TokenKey = (typeof TOKEN_KEYS)[number];

/**
 * The code-colour roles (`--c-syntax-*`). They are not part of `TOKEN_KEYS`
 * because they are not `R G B` triplets: app.css declares them as `oklch()`
 * colours and a derived palette answers with `rgb()`, both used directly as a
 * `<color>`.
 */
export const SYNTAX_KEYS = [
  "--c-syntax-keyword",
  "--c-syntax-type",
  "--c-syntax-string",
  "--c-syntax-number",
  "--c-syntax-function",
  "--c-syntax-operator",
  "--c-syntax-attribute",
  "--c-syntax-property",
  "--c-syntax-regex",
  "--c-syntax-variable",
  "--c-syntax-punctuation",
] as const;

export type SyntaxKey = (typeof SYNTAX_KEYS)[number];

/**
 * The syntax colours app.css ships, per appearance, as the OKLCH it declares.
 * Transcribed; `derive.syntax.test.ts` compares every one with the stylesheet.
 * They are the starting point for a custom palette: the guard moves lightness
 * only when the derived code background needs it.
 */
export const SHIPPED_SYNTAX: Readonly<Record<Appearance, Readonly<Record<SyntaxKey, Oklch>>>> = {
  dark: {
    "--c-syntax-keyword": { l: 0.8, c: 0.125, h: 305 },
    "--c-syntax-type": { l: 0.8, c: 0.11, h: 205 },
    "--c-syntax-string": { l: 0.8, c: 0.12, h: 150 },
    "--c-syntax-number": { l: 0.82, c: 0.13, h: 70 },
    "--c-syntax-function": { l: 0.8, c: 0.1, h: 255 },
    "--c-syntax-operator": { l: 0.8, c: 0.115, h: 10 },
    "--c-syntax-attribute": { l: 0.82, c: 0.11, h: 100 },
    "--c-syntax-property": { l: 0.82, c: 0.105, h: 235 },
    "--c-syntax-regex": { l: 0.82, c: 0.125, h: 345 },
    "--c-syntax-variable": { l: 0.8, c: 0.1, h: 280 },
    "--c-syntax-punctuation": { l: 0.76, c: 0.03, h: 265 },
  },
  light: {
    "--c-syntax-keyword": { l: 0.48, c: 0.15, h: 305 },
    "--c-syntax-type": { l: 0.455, c: 0.075, h: 205 },
    "--c-syntax-string": { l: 0.45, c: 0.12, h: 150 },
    "--c-syntax-number": { l: 0.47, c: 0.1, h: 70 },
    "--c-syntax-function": { l: 0.465, c: 0.15, h: 255 },
    "--c-syntax-operator": { l: 0.48, c: 0.15, h: 10 },
    "--c-syntax-attribute": { l: 0.46, c: 0.095, h: 100 },
    "--c-syntax-property": { l: 0.46, c: 0.095, h: 235 },
    "--c-syntax-regex": { l: 0.48, c: 0.14, h: 345 },
    "--c-syntax-variable": { l: 0.475, c: 0.15, h: 280 },
    "--c-syntax-punctuation": { l: 0.45, c: 0.03, h: 265 },
  },
};

/** Alpha of the diff row tints and the selected-line tint that code is read over. */
export const CODE_TINT_ALPHA = 0.15;

/** The syntax floor: 4.5:1 plus headroom for the rounding to whole `rgb()` channels. */
export const SYNTAX_MIN = TEXT_MIN + 0.05;

export interface DeriveInputs {
  /** Surface hue and lightness. `#rgb` or `#rrggbb`. */
  readonly base: string;
  readonly accent: string;
  /** 0..1: how far apart the surface steps sit. 0.5 is the shipped ladder. */
  readonly contrast: number;
  /** Optional hex overrides for single roles; still guarded. */
  readonly ok?: string;
  readonly warn?: string;
  readonly danger?: string;
  readonly info?: string;
  readonly text?: string;
  readonly textMuted?: string;
}

/**
 * The ladder app.css declares under `html.dark` and `html.light`. Transcribed;
 * `derive.test.ts` compares every triplet with the stylesheet.
 */
export const SHIPPED_LADDER = {
  dark: {
    bg: [11, 14, 23],
    surface: [20, 26, 41],
    surfaceHover: [31, 39, 58],
    border: [44, 54, 78],
    text: [233, 237, 248],
    textMuted: [148, 161, 190],
  },
  light: {
    bg: [243, 244, 250],
    surface: [255, 255, 255],
    surfaceHover: [236, 238, 248],
    border: [96, 104, 136],
    text: [34, 40, 62],
    textMuted: [99, 108, 134],
  },
} as const satisfies Record<Appearance, Record<string, Rgb>>;

/** Contrast value at which the shipped ladder is reproduced. */
export const SHIPPED_CONTRAST = 0.5;

/** `#rrggbb` of the shipped `--c-bg` per appearance. */
export const SHIPPED_BASE_HEX: Readonly<Record<Appearance, string>> = {
  dark: "#0b0e17",
  light: "#f3f4fa",
};

/**
 * [brief] Chroma ceiling for the base, so a vivid base still yields a usable
 * grey ladder. It caps the base only: the shipped hover and border steps are
 * measured at C 0.037 and 0.045, so capping every step would not reproduce them.
 */
export const BASE_CHROMA_CAP = 0.03;

/**
 * [brief] Status hues in degrees (ok 150, warn 75, danger 20, info 240); the
 * design spec fixes them, they are not measured. `unknown` has no hue of its
 * own: it takes the base hue at chroma 0.
 */
export const STATUS_HUES = { ok: 150, warn: 75, danger: 20, info: 240 } as const;

/**
 * [brief] Spec text ladder, dark: text L 93, muted L 68, faint L 52. Faint sits
 * 0.16 of lightness beyond muted; that difference is the only part of the spec
 * ladder used, because text and muted themselves are measured from app.css
 * (L 94.6 and 70.9, not the spec's rounded 93 and 68).
 */
export const FAINT_LIGHTNESS_STEP = 0.16;

/**
 * Starting point for status colours, before the guard sets the final lightness.
 * Chosen to be in the right region for each appearance, not measured.
 */
const STATUS_START = {
  dark: { l: 0.78, c: 0.15 },
  light: { l: 0.48, c: 0.14 },
} as const;
const UNKNOWN_START = { dark: 0.7, light: 0.5 } as const;

interface Offset {
  readonly dl: number;
  readonly dc: number;
  readonly dh: number;
}

type LadderRole = "surface" | "surfaceHover" | "border" | "text" | "textMuted";

function signedHueDelta(from: number, to: number): number {
  const wrapped = normalizeHue(to - from);
  return wrapped > 180 ? wrapped - 360 : wrapped;
}

/** Offset of one shipped colour from the shipped `bg`, measured in OKLCH. */
function measureOffset(color: Rgb, bg: Rgb): Offset {
  const target = rgbToOklch(color);
  const origin = rgbToOklch(bg);
  const achromatic = target.c < 1e-3;
  return {
    dl: target.l - origin.l,
    dc: target.c - origin.c,
    // An achromatic step (the white card) has no hue to rotate toward.
    dh: achromatic ? 0 : signedHueDelta(origin.h, target.h),
  };
}

function measureLadder(appearance: Appearance): Record<LadderRole, Offset> {
  const ladder = SHIPPED_LADDER[appearance];
  return {
    surface: measureOffset(ladder.surface, ladder.bg),
    surfaceHover: measureOffset(ladder.surfaceHover, ladder.bg),
    border: measureOffset(ladder.border, ladder.bg),
    text: measureOffset(ladder.text, ladder.bg),
    textMuted: measureOffset(ladder.textMuted, ladder.bg),
  };
}

const OFFSETS: Readonly<Record<Appearance, Record<LadderRole, Offset>>> = {
  dark: measureLadder("dark"),
  light: measureLadder("light"),
};

/** Measured offsets, exposed so tests can show the numbers behind the ladder. */
export function ladderOffsets(appearance: Appearance): Readonly<Record<LadderRole, Offset>> {
  return OFFSETS[appearance];
}

export interface GuardReportEntry {
  readonly key: TokenKey | SyntaxKey;
  /** The colour before the guard, as `#rrggbb`. */
  readonly requested: Rgb;
  readonly minimum: number;
  readonly ratio: number;
  readonly adjusted: boolean;
}

export interface DeriveFailure {
  /** The input or token that could not be honoured. */
  readonly key: string;
  readonly message: string;
  /** The best ratio reached, when the guard refused a colour. */
  readonly ratio?: number;
  readonly minimum?: number;
}

export type DeriveResult =
  | {
      readonly ok: true;
      /** `R G B` triplets keyed by CSS custom property. */
      readonly tokens: Readonly<Record<TokenKey, string>>;
      /** `rgb()` colours for the code roles, each held to 4.5:1 on the derived code background. */
      readonly syntax: Readonly<Record<SyntaxKey, string>>;
      /** One entry per guarded token: what the guard measured and whether it moved. */
      readonly report: readonly GuardReportEntry[];
    }
  | { readonly ok: false; readonly failures: readonly DeriveFailure[] };

function placeAt(base: Oklch, offset: Offset, scale: number): Oklch {
  return {
    l: Math.min(1, Math.max(0, base.l + offset.dl * scale)),
    c: Math.max(0, base.c + offset.dc * scale),
    h: normalizeHue(base.h + offset.dh * scale),
  };
}

function worstSurface(fg: Rgb, surfaces: readonly Rgb[]): Rgb {
  let worst = surfaces[0];
  let worstRatio = contrastRatio(fg, worst);
  for (const surface of surfaces) {
    const ratio = contrastRatio(fg, surface);
    if (ratio < worstRatio) {
      worst = surface;
      worstRatio = ratio;
    }
  }
  return worst;
}

/**
 * Derives the palette. `contrast` scales every offset from `bg` linearly
 * (`contrast / 0.5`), so 0 collapses the ladder onto the base and 1 doubles it.
 */
export function deriveTokens(inputs: DeriveInputs, appearance: Appearance): DeriveResult {
  const failures: DeriveFailure[] = [];
  const parsed = (key: string, value: unknown): Rgb | null => {
    const rgb = parseHex(value);
    if (!rgb) failures.push({ key, message: "must be a #rgb or #rrggbb color" });
    return rgb;
  };

  const base = parsed("base", inputs.base);
  const accent = parsed("accent", inputs.accent);
  const optional = (
    key: "ok" | "warn" | "danger" | "info" | "text" | "textMuted",
  ): Rgb | undefined => {
    const value: unknown = inputs[key];
    if (value === undefined) return undefined;
    return parsed(key, value) ?? undefined;
  };
  const overrides = {
    ok: optional("ok"),
    warn: optional("warn"),
    danger: optional("danger"),
    info: optional("info"),
    text: optional("text"),
    textMuted: optional("textMuted"),
  };
  if (typeof inputs.contrast !== "number" || !Number.isFinite(inputs.contrast) || inputs.contrast < 0 || inputs.contrast > 1) {
    failures.push({ key: "contrast", message: "must be a number from 0 to 1" });
  }
  if (!base || !accent || failures.length > 0) return { ok: false, failures };

  const scale = inputs.contrast / SHIPPED_CONTRAST;
  const offsets = OFFSETS[appearance];
  const origin = rgbToOklch(base);
  const anchor: Oklch = { l: origin.l, c: Math.min(origin.c, BASE_CHROMA_CAP), h: origin.h };

  const bg = oklchToRgb(anchor);
  const surface = oklchToRgb(placeAt(anchor, offsets.surface, scale));
  const surfaceHover = oklchToRgb(placeAt(anchor, offsets.surfaceHover, scale));
  const border = oklchToRgb(placeAt(anchor, offsets.border, scale));
  const surfaces: readonly Rgb[] = [bg, surface, surfaceHover];

  const resolved = new Map<TokenKey | SyntaxKey, Rgb>([
    ["--c-bg", bg],
    ["--c-surface", surface],
    ["--c-surface-hover", surfaceHover],
    ["--c-border", border],
  ]);
  const report: GuardReportEntry[] = [];

  /** Runs `requested` through the guard against the worst of `against`. */
  const guard = (
    key: TokenKey | SyntaxKey,
    requested: Rgb,
    against: readonly Rgb[],
    minimum: number,
  ): Rgb => {
    const result = ensureContrast(requested, worstSurface(requested, against), minimum);
    if (!result.ok) {
      failures.push({
        key,
        message: `cannot reach ${minimum}:1 against its surface`,
        ratio: result.ratio,
        minimum,
      });
      return requested;
    }
    // Moving away from the worst surface must not strand it on another one.
    const weakest = Math.min(...against.map((surfaceRgb) => contrastRatio(result.color, surfaceRgb)));
    if (weakest < minimum) {
      failures.push({
        key,
        message: `reaches ${minimum}:1 against one surface but not all`,
        ratio: weakest,
        minimum,
      });
    }
    report.push({ key, requested, minimum, ratio: weakest, adjusted: result.adjusted });
    resolved.set(key, result.color);
    return result.color;
  };

  // Ink.
  const mutedPlaced = placeAt(anchor, offsets.textMuted, scale);
  const faintDirection = appearance === "dark" ? -1 : 1;
  const faintPlaced: Oklch = {
    ...mutedPlaced,
    l: Math.min(1, Math.max(0, mutedPlaced.l + faintDirection * FAINT_LIGHTNESS_STEP * scale)),
  };
  guard("--c-text", overrides.text ?? oklchToRgb(placeAt(anchor, offsets.text, scale)), surfaces, TEXT_MIN);
  guard("--c-text-muted", overrides.textMuted ?? oklchToRgb(mutedPlaced), surfaces, TEXT_MIN);
  guard("--c-text-faint", oklchToRgb(faintPlaced), surfaces, TEXT_MIN);

  // Accent, and the ink that sits on an accent fill.
  const accentColor = guard("--c-accent", accent, surfaces, TEXT_MIN);
  const darkInk = oklchToRgb({ l: 0.1, c: 0.01, h: anchor.h });
  const WHITE: Rgb = [255, 255, 255];
  /** White when it reads on the fill, otherwise a near-black. */
  const inkOn = (key: TokenKey, fill: Rgb): void => {
    const ink = contrastRatio(WHITE, fill) >= TEXT_MIN ? WHITE : darkInk;
    const ratio = contrastRatio(ink, fill);
    if (ratio < TEXT_MIN) {
      failures.push({ key, message: `no ink reaches ${TEXT_MIN}:1 on this fill`, ratio, minimum: TEXT_MIN });
    }
    report.push({ key, requested: ink, minimum: TEXT_MIN, ratio, adjusted: false });
    resolved.set(key, ink);
  };
  inkOn("--c-on-accent", accentColor);

  // Status. Hues are fixed; lightness and chroma adapt to the appearance.
  const start = STATUS_START[appearance];
  const statusRoles = [
    ["--c-ok", "--c-on-ok", "ok"],
    ["--c-warn", "--c-on-warn", "warn"],
    ["--c-danger", "--c-on-danger", "danger"],
    ["--c-info", "--c-on-info", "info"],
  ] as const;
  for (const [key, onKey, role] of statusRoles) {
    const requested = overrides[role] ?? oklchToRgb({ l: start.l, c: start.c, h: STATUS_HUES[role] });
    inkOn(onKey, guard(key, requested, surfaces, TEXT_MIN));
  }
  const unknown = guard(
    "--c-unknown",
    oklchToRgb({ l: UNKNOWN_START[appearance], c: 0, h: anchor.h }),
    surfaces,
    TEXT_MIN,
  );
  inkOn("--c-on-unknown", unknown);

  // Code. It is read over the three surfaces and over the paints the viewers
  // lay on them: the added and removed row tints and the selected-line tint.
  // The same guard, against the worst of those, so a palette that moves the
  // code background cannot strand the syntax colours that were tuned for the
  // shipped one.
  const tintFills = (["--c-ok", "--c-danger", "--c-accent"] as const).map((key) => resolved.get(key));
  // A refused status or accent colour has already failed the palette; there is
  // no tint to measure against, and the palette is not applied either way.
  if (failures.length === 0 && tintFills.every((fill) => fill !== undefined)) {
    const codeBackgrounds: Rgb[] = [...surfaces];
    for (const surfaceRgb of surfaces) {
      for (const fill of tintFills) {
        if (!fill) continue;
        const mix = (i: 0 | 1 | 2): number => fill[i] * CODE_TINT_ALPHA + surfaceRgb[i] * (1 - CODE_TINT_ALPHA);
        codeBackgrounds.push([mix(0), mix(1), mix(2)]);
      }
    }
    for (const key of SYNTAX_KEYS) {
      guard(key, oklchToRgb(clampToGamut(SHIPPED_SYNTAX[appearance][key])), codeBackgrounds, SYNTAX_MIN);
    }
  }

  if (failures.length > 0) return { ok: false, failures };

  // Every key is set above; a missing one is a bug in this function, not input.
  const pick = (key: TokenKey): string => {
    const rgb = resolved.get(key);
    if (!rgb) throw new Error(`deriveTokens did not produce ${key}`);
    return formatTriplet(rgb);
  };
  const tokens: Record<TokenKey, string> = {
    "--c-bg": pick("--c-bg"),
    "--c-surface": pick("--c-surface"),
    "--c-surface-hover": pick("--c-surface-hover"),
    "--c-border": pick("--c-border"),
    "--c-text": pick("--c-text"),
    "--c-text-muted": pick("--c-text-muted"),
    "--c-text-faint": pick("--c-text-faint"),
    "--c-accent": pick("--c-accent"),
    "--c-on-accent": pick("--c-on-accent"),
    "--c-ok": pick("--c-ok"),
    "--c-on-ok": pick("--c-on-ok"),
    "--c-warn": pick("--c-warn"),
    "--c-on-warn": pick("--c-on-warn"),
    "--c-danger": pick("--c-danger"),
    "--c-on-danger": pick("--c-on-danger"),
    "--c-info": pick("--c-info"),
    "--c-on-info": pick("--c-on-info"),
    "--c-unknown": pick("--c-unknown"),
    "--c-on-unknown": pick("--c-on-unknown"),
  };
  const pickSyntax = (key: SyntaxKey): string => {
    const rgb = resolved.get(key);
    if (!rgb) throw new Error(`deriveTokens did not produce ${key}`);
    return `rgb(${formatTriplet(rgb)})`;
  };
  const syntax: Record<SyntaxKey, string> = {
    "--c-syntax-keyword": pickSyntax("--c-syntax-keyword"),
    "--c-syntax-type": pickSyntax("--c-syntax-type"),
    "--c-syntax-string": pickSyntax("--c-syntax-string"),
    "--c-syntax-number": pickSyntax("--c-syntax-number"),
    "--c-syntax-function": pickSyntax("--c-syntax-function"),
    "--c-syntax-operator": pickSyntax("--c-syntax-operator"),
    "--c-syntax-attribute": pickSyntax("--c-syntax-attribute"),
    "--c-syntax-property": pickSyntax("--c-syntax-property"),
    "--c-syntax-regex": pickSyntax("--c-syntax-regex"),
    "--c-syntax-variable": pickSyntax("--c-syntax-variable"),
    "--c-syntax-punctuation": pickSyntax("--c-syntax-punctuation"),
  };
  return { ok: true, tokens, syntax, report };
}
