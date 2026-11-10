/**
 * Settings -> Appearance, turned into CSS (design language §11).
 *
 * `planAppearance` is pure: preferences and the live theme and host capability
 * in, a plan out (inline custom properties to set, ones to remove, attributes to
 * stamp, the accent override, and the notices the reader is owed). The DOM write
 * is a thin wrapper, and `installAppearance` is the one subscription that keeps
 * the document in step with the stores. Vitest never runs `$effect`, so this is
 * where the mapping is pinned.
 *
 * Two rules shape everything here:
 * - Only INPUT hooks are written inline (`--gp-material`, `--gp-float-blur`,
 *   `--gp-font-ui`, `--gp-radius-scale` ...). The OUTPUT variables the system's
 *   Reduce Transparency blocks redeclare (`--mac-fill-*`, the float filter) stay
 *   in app.css, computed from those inputs. An inline value for an output would
 *   beat the accessibility override on the same element; an input cannot.
 * - Colours go through the contrast guard (`color/derive.ts` over
 *   `color/guard.ts`) and never around it. A colour the guard moved is reported;
 *   one it refuses is replaced by the nearest preset and reported.
 */
import { derived, writable, type Readable } from "svelte/store";
import { isDensity, type Density } from "./density";
import {
  ACCENTS,
  DEFAULT_ACCENT,
  accentFor,
  isAccentId,
  applyAccent,
  setAccentOverride,
  type AccentId,
} from "../accents";
import {
  SHIPPED_BASE_HEX,
  SHIPPED_CONTRAST,
  TOKEN_KEYS,
  deriveTokens,
  type Appearance,
  type DeriveInputs,
  type GuardReportEntry,
} from "../color/derive";
import {
  clampToGamut,
  contrastRatio,
  formatHex,
  formatTriplet,
  normalizeHue,
  oklchToRgb,
  parseHex,
  rgbToOklch,
  type Rgb,
} from "../color/oklch";
import { setUserTransparencyReduced } from "../transparencyPreference";
import { DEFAULT_MONO_STACK, DEFAULT_UI_STACK, fontStack, normalizeFamily } from "./fonts";
import { measureFloor } from "./materialModel";
import { RADIUS_SCALE_RANGE, validateTheme, type GitPulseTheme } from "./themeSchema";

export type FieldLevel = "off" | "subtle" | "vivid";
export const FIELD_LEVELS: readonly FieldLevel[] = ["off", "subtle", "vivid"];
export function isFieldLevel(value: unknown): value is FieldLevel {
  return value === "off" || value === "subtle" || value === "vivid";
}

/** The part of the interface preferences this module reads. */
export interface AppearancePrefs {
  /** The preset accent. Ignored while `customAccent` or an imported theme says otherwise. */
  readonly accent: AccentId;
  /** `#rrggbb`, or `""` for "use the preset". */
  readonly customAccent: string;
  /** An imported theme; it draws only in its own appearance. */
  readonly customTheme: GitPulseTheme | null;
  /** A local family name, or `""` for the system font. */
  readonly fontUi: string;
  readonly fontMono: string;
  readonly density: Density;
  /** Multiplies the radius ladder; 1 is the shipped shape. */
  readonly radiusScale: number;
  /**
   * 0 = the shipped alphas, 1 = opaque, stored per appearance so dragging light
   * up to its floor leaves dark alone. Each is held to its own measured floor while it draws.
   */
  readonly materialIntensity: Readonly<Record<Appearance, number>>;
  /** Float blur radius in px. 0 means `backdrop-filter: none`. */
  readonly floatBlur: number;
  readonly fieldLevel: FieldLevel;
  /** Degrees the four background-field hues rotate together. */
  readonly fieldHue: number;
  /** The reader's one-way "Transparency: reduced" choice. */
  readonly reduceTransparency: boolean;
}

export const FLOAT_BLUR_SHIPPED = 34;
export const FLOAT_BLUR_MAX = 40;
export const FIELD_HUE_RANGE = { min: 0, max: 360 } as const;
export const SHIPPED_RADIUS_SCALE = 1;

export const APPEARANCE_DEFAULTS: AppearancePrefs = {
  accent: DEFAULT_ACCENT,
  customAccent: "",
  customTheme: null,
  fontUi: "",
  fontMono: "",
  density: "comfortable",
  radiusScale: SHIPPED_RADIUS_SCALE,
  materialIntensity: { light: 0, dark: 0 },
  floatBlur: FLOAT_BLUR_SHIPPED,
  fieldLevel: "vivid",
  fieldHue: 0,
  reduceTransparency: false,
};

/** Level -> fraction of the theme's shipped field gain. Never above shipped (§11). */
export const FIELD_GAIN: Readonly<Record<FieldLevel, number>> = { off: 0, subtle: 0.5, vivid: 1 };

/** The hue field app.css ships per appearance. `apply.test.ts` pins it to the stylesheet. */
export const SHIPPED_FIELD: Readonly<
  Record<Appearance, { readonly hues: readonly [Rgb, Rgb, Rgb, Rgb]; readonly gain: number }>
> = {
  dark: {
    hues: [
      [52, 78, 200],
      [12, 150, 170],
      [110, 66, 210],
      [180, 50, 130],
    ],
    gain: 1,
  },
  light: {
    hues: [
      [120, 152, 255],
      [46, 196, 214],
      [168, 136, 252],
      [240, 122, 186],
    ],
    gain: 0.5,
  },
};

/** Lowest material intensity each appearance may draw at, measured by `materialModel.ts`. */
export const MATERIAL_FLOOR: Readonly<Record<Appearance, number>> = {
  dark: measureFloor("dark").floor,
  light: measureFloor("light").floor,
};

export const CORNER_STYLES = [
  { id: "sharp", label: "Sharp", scale: 0.5 },
  { id: "soft", label: "Soft", scale: 1 },
  { id: "round", label: "Round", scale: 1.25 },
] as const;

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

function finite(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

/** A stored theme is untrusted input like an imported one: it goes through the validator again. */
function readTheme(value: unknown): GitPulseTheme | null {
  if (value === null || value === undefined) return null;
  const checked = validateTheme(value);
  return checked.ok ? checked.theme : null;
}

/** A stored intensity: the per-appearance pair, or the single number older builds wrote (it seeds both). */
function readIntensity(value: unknown): Readonly<Record<Appearance, number>> {
  if (typeof value === "object" && value !== null) {
    const pair = value as Readonly<Record<string, unknown>>;
    return { light: clamp(finite(pair.light, 0), 0, 1), dark: clamp(finite(pair.dark, 0), 0, 1) };
  }
  const single = clamp(finite(value, 0), 0, 1);
  return { light: single, dark: single };
}

/** Reads stored appearance preferences; anything unrecognised becomes its default. */
export function sanitizeAppearance(raw: Readonly<Record<string, unknown>>): AppearancePrefs {
  const custom = typeof raw.customAccent === "string" ? parseHex(raw.customAccent) : null;
  const fontUi = normalizeFamily(raw.fontUi);
  const fontMono = normalizeFamily(raw.fontMono);
  return {
    accent: isAccentId(raw.accent) ? raw.accent : APPEARANCE_DEFAULTS.accent,
    customAccent: custom ? formatHex(custom) : "",
    customTheme: readTheme(raw.customTheme),
    fontUi: fontUi ?? "",
    fontMono: fontMono ?? "",
    density: isDensity(raw.density) ? raw.density : APPEARANCE_DEFAULTS.density,
    radiusScale: clamp(
      finite(raw.radiusScale, SHIPPED_RADIUS_SCALE),
      RADIUS_SCALE_RANGE.min,
      RADIUS_SCALE_RANGE.max,
    ),
    materialIntensity: readIntensity(raw.materialIntensity),
    floatBlur: Math.round(clamp(finite(raw.floatBlur, FLOAT_BLUR_SHIPPED), 0, FLOAT_BLUR_MAX)),
    fieldLevel: isFieldLevel(raw.fieldLevel) ? raw.fieldLevel : APPEARANCE_DEFAULTS.fieldLevel,
    fieldHue: normalizeHue(finite(raw.fieldHue, 0)),
    reduceTransparency: raw.reduceTransparency === true,
  };
}

// --- accent -----------------------------------------------------------------

export type AccentOutcome =
  /** A preset in force; the stylesheet or `applyAccent` owns it. */
  | { readonly status: "preset"; readonly resolved: string }
  /** A custom colour that already met the guard. */
  | { readonly status: "passes"; readonly requested: string; readonly resolved: string; readonly ratio: number }
  /** A custom colour the guard moved to reach the minimum. */
  | { readonly status: "adjusted"; readonly requested: string; readonly resolved: string; readonly ratio: number }
  /** A colour the guard refused; the nearest preset is used instead. */
  | { readonly status: "snapped"; readonly requested: string; readonly resolved: string; readonly preset: AccentId };

export function presetHex(id: AccentId, appearance: Appearance): string {
  return formatHex(accentFor(id).rgb[appearance]);
}

/** The preset whose OKLCH position is nearest `hex`. */
export function nearestPreset(hex: string, appearance: Appearance): AccentId {
  const target = parseHex(hex);
  if (!target) return DEFAULT_ACCENT;
  const want = rgbToOklch(target);
  let best: AccentId = DEFAULT_ACCENT;
  let bestDistance = Number.POSITIVE_INFINITY;
  for (const accent of ACCENTS) {
    const have = rgbToOklch(accent.rgb[appearance]);
    const hueDelta = Math.min(Math.abs(have.h - want.h), 360 - Math.abs(have.h - want.h)) / 360;
    const distance = (have.l - want.l) ** 2 + (have.c - want.c) ** 2 * 4 + hueDelta ** 2;
    if (distance < bestDistance) {
      best = accent.id;
      bestDistance = distance;
    }
  }
  return best;
}

function accentEntry(report: readonly GuardReportEntry[]): GuardReportEntry | undefined {
  return report.find((entry) => entry.key === "--c-accent");
}

/**
 * What the accent will be, and how it got there. Pure; the settings panel
 * renders this as its live "passes / adjusted / snapped" line.
 */
export function resolveAccent(
  prefs: Pick<AppearancePrefs, "accent" | "customAccent">,
  appearance: Appearance,
  base: string = SHIPPED_BASE_HEX[appearance],
  contrast: number = SHIPPED_CONTRAST,
): AccentOutcome {
  const requested = parseHex(prefs.customAccent);
  if (!requested) return { status: "preset", resolved: presetHex(prefs.accent, appearance) };
  const hex = formatHex(requested);
  const derivedPalette = deriveTokens({ base, accent: hex, contrast }, appearance);
  if (derivedPalette.ok) {
    const entry = accentEntry(derivedPalette.report);
    const resolvedTriplet = derivedPalette.tokens["--c-accent"];
    const resolved = formatHex(tripletChannels(resolvedTriplet));
    const ratio = entry?.ratio ?? 0;
    return entry?.adjusted
      ? { status: "adjusted", requested: hex, resolved, ratio }
      : { status: "passes", requested: hex, resolved, ratio };
  }
  const preset = nearestPreset(hex, appearance);
  return { status: "snapped", requested: hex, resolved: presetHex(preset, appearance), preset };
}

// --- field ------------------------------------------------------------------

function rotateHue(color: Rgb, degrees: number): Rgb {
  const lch = rgbToOklch(color);
  return oklchToRgb(clampToGamut({ l: lch.l, c: lch.c, h: normalizeHue(lch.h + degrees) }));
}

/** `hex` with its OKLCH hue turned by `degrees`; the input comes back untouched at 0. */
export function rotateHueHex(hex: string, degrees: number): string {
  const rgb = parseHex(hex);
  if (!rgb) return hex;
  return degrees === 0 ? formatHex(rgb) : formatHex(rotateHue(rgb, degrees));
}

// --- plan -------------------------------------------------------------------

export interface AppearanceNote {
  readonly id: string;
  readonly tone: "info" | "warn";
  readonly text: string;
}

export interface AppearancePlan {
  /** Inline custom properties to set. */
  readonly vars: Readonly<Record<string, string>>;
  /** Attributes to stamp (a string) or remove (null). */
  readonly attributes: Readonly<Record<string, string | null>>;
  /** `R G B` for `--c-accent`, or null to leave the accent to the preset. */
  readonly accentOverride: string | null;
  /** The reader's one-way transparency choice, forwarded to the bridge. */
  readonly transparencyReduced: boolean;
  readonly notes: readonly AppearanceNote[];
}

export interface AppearanceContext {
  readonly theme: Appearance;
  /** `HostProfile.native_material`: whether the glass tokens mean anything here. */
  readonly nativeMaterial: boolean;
}

/** Every inline property this module may set, so a plan can clear what it no longer sets. */
export const MANAGED_VARS: readonly string[] = [
  ...TOKEN_KEYS.filter((key) => key !== "--c-accent"),
  "--gp-font-ui",
  "--gp-font-mono",
  "--gp-radius-scale",
  "--gp-material",
  "--gp-float-blur",
  "--mac-hue-a",
  "--mac-hue-b",
  "--mac-hue-c",
  "--mac-hue-d",
  "--mac-hue-gain",
];

export const MANAGED_ATTRIBUTES: readonly string[] = ["data-density", "data-radius-scale", "data-float-blur"];

function themeInputs(theme: GitPulseTheme, accent: string): DeriveInputs {
  const { colors } = theme;
  return {
    base: colors.base,
    accent,
    contrast: colors.contrast ?? SHIPPED_CONTRAST,
    ...(colors.ok === undefined ? {} : { ok: colors.ok }),
    ...(colors.warn === undefined ? {} : { warn: colors.warn }),
    ...(colors.danger === undefined ? {} : { danger: colors.danger }),
    ...(colors.info === undefined ? {} : { info: colors.info }),
    ...(colors.text === undefined ? {} : { text: colors.text }),
    ...(colors.textMuted === undefined ? {} : { textMuted: colors.textMuted }),
  };
}

function tripletChannels(triplet: string): Rgb {
  const [r, g, b] = triplet.split(" ").map(Number);
  return [r, g, b];
}

export function planAppearance(prefs: AppearancePrefs, context: AppearanceContext): AppearancePlan {
  const vars: Record<string, string> = {};
  const attributes: Record<string, string | null> = {};
  const notes: AppearanceNote[] = [];
  const { theme } = context;
  let accentOverride: string | null = null;

  // Palette and accent.
  const customTheme = prefs.customTheme;
  const themeActive = customTheme !== null && customTheme.appearance === theme;
  if (customTheme !== null && !themeActive) {
    notes.push({
      id: "theme-other-mode",
      tone: "info",
      text: `“${customTheme.name}” draws in ${customTheme.appearance} mode. This window is ${theme}, so it uses the built-in palette.`,
    });
  }
  const requestedAccent =
    prefs.customAccent !== ""
      ? prefs.customAccent
      : prefs.accent !== DEFAULT_ACCENT
        ? presetHex(prefs.accent, theme)
        : null;
  let themeApplied = false;
  if (themeActive && customTheme !== null) {
    const result = deriveTokens(themeInputs(customTheme, requestedAccent ?? customTheme.colors.accent), theme);
    if (result.ok) {
      themeApplied = true;
      for (const key of TOKEN_KEYS) {
        if (key === "--c-accent") accentOverride = result.tokens[key];
        else vars[key] = result.tokens[key];
      }
      const entry = accentEntry(result.report);
      if (entry?.adjusted) {
        notes.push({
          id: "accent-adjusted",
          tone: "info",
          text: `The accent was adjusted to ${entry.ratio.toFixed(1)}:1 so it stays readable on this theme.`,
        });
      }
    } else {
      const detail = result.failures
        .map((failure) =>
          failure.ratio === undefined
            ? `${failure.key} ${failure.message}`
            : `${failure.key} reaches ${failure.ratio.toFixed(1)}:1 of ${failure.minimum}:1`,
        )
        .join("; ");
      notes.push({
        id: "theme-refused",
        tone: "warn",
        text: `“${customTheme.name}” isn't applied. ${detail}.`,
      });
    }
  }
  if (!themeApplied && prefs.customAccent !== "") {
    const outcome = resolveAccent(prefs, theme);
    if (outcome.status === "passes" || outcome.status === "adjusted") {
      const rgb = parseHex(outcome.resolved);
      accentOverride = rgb ? formatTriplet(rgb) : null;
      if (outcome.status === "adjusted") {
        notes.push({
          id: "accent-adjusted",
          tone: "info",
          text: `Adjusted from ${outcome.requested} to ${outcome.resolved} (${outcome.ratio.toFixed(1)}:1) so accent text stays readable.`,
        });
      }
    } else if (outcome.status === "snapped") {
      const rgb = parseHex(outcome.resolved);
      accentOverride = rgb ? formatTriplet(rgb) : null;
      notes.push({
        id: "accent-snapped",
        tone: "warn",
        text: `${outcome.requested} can't reach readable contrast in this theme. Using ${accentFor(outcome.preset).label} instead.`,
      });
    }
  }

  // Type.
  const ui = normalizeFamily(prefs.fontUi);
  if (ui) vars["--gp-font-ui"] = fontStack(ui, DEFAULT_UI_STACK);
  const mono = normalizeFamily(prefs.fontMono);
  if (mono) vars["--gp-font-mono"] = fontStack(mono, DEFAULT_MONO_STACK);

  // Shape and density: plain CSS, no host capability needed.
  attributes["data-density"] = prefs.density;
  const scale = clamp(prefs.radiusScale, RADIUS_SCALE_RANGE.min, RADIUS_SCALE_RANGE.max);
  if (Math.abs(scale - SHIPPED_RADIUS_SCALE) > 1e-6) {
    vars["--gp-radius-scale"] = String(Number(scale.toFixed(3)));
    attributes["data-radius-scale"] = String(Number(scale.toFixed(3)));
  } else {
    attributes["data-radius-scale"] = null;
  }

  // Material, float blur, background field: the `html.macos` tokens.
  attributes["data-float-blur"] = null;
  if (context.nativeMaterial) {
    const floor = MATERIAL_FLOOR[theme];
    const wanted = prefs.materialIntensity[theme];
    const intensity = clamp(wanted, floor, 1);
    if (intensity > 0) vars["--gp-material"] = String(Number(intensity.toFixed(3)));
    if (wanted < floor) {
      notes.push({
        id: "material-floor",
        tone: "info",
        text: `Held at ${Math.round(floor * 100)}% in ${theme} mode: thinner glass would put text under 4.5:1 over some desktops.`,
      });
    }

    const blur = Math.round(clamp(prefs.floatBlur, 0, FLOAT_BLUR_MAX));
    if (blur === 0) attributes["data-float-blur"] = "off";
    else if (blur !== FLOAT_BLUR_SHIPPED) vars["--gp-float-blur"] = `${blur}px`;

    const shipped = SHIPPED_FIELD[theme];
    const fieldTheme = themeApplied && customTheme?.field ? customTheme.field : null;
    const hues = fieldTheme ? fieldTheme.hues.map((hex) => parseHex(hex) ?? shipped.hues[0]) : shipped.hues;
    const share = fieldTheme ? fieldTheme.gain : FIELD_GAIN[prefs.fieldLevel];
    if (share !== 1) vars["--mac-hue-gain"] = String(Number((shipped.gain * share).toFixed(3)));
    if (fieldTheme || prefs.fieldHue !== 0) {
      const names = ["--mac-hue-a", "--mac-hue-b", "--mac-hue-c", "--mac-hue-d"];
      hues.forEach((hue, index) => {
        vars[names[index]] = formatTriplet(rotateHue(hue as Rgb, prefs.fieldHue));
      });
    }
  }

  return {
    vars,
    attributes,
    accentOverride,
    transparencyReduced: context.nativeMaterial && prefs.reduceTransparency,
    notes,
  };
}

// --- DOM --------------------------------------------------------------------

export interface AppearanceTarget {
  readonly style: {
    setProperty(name: string, value: string): void;
    removeProperty(name: string): void;
  };
  setAttribute(name: string, value: string): void;
  removeAttribute(name: string): void;
}

/** Writes `plan` and clears every managed property it no longer sets. */
export function writeAppearance(plan: AppearancePlan, target: AppearanceTarget): void {
  for (const name of MANAGED_VARS) {
    const value = plan.vars[name];
    if (value === undefined) target.style.removeProperty(name);
    else target.style.setProperty(name, value);
  }
  for (const name of MANAGED_ATTRIBUTES) {
    const value = plan.attributes[name];
    if (value === undefined || value === null) target.removeAttribute(name);
    else target.setAttribute(name, value);
  }
}

const noteStore = writable<readonly AppearanceNote[]>([]);
/** The notices the last plan owed the reader (a guard adjustment, a refused theme). */
export const appearanceNotes: Readable<readonly AppearanceNote[]> = { subscribe: noteStore.subscribe };

export interface AppearanceSources {
  readonly prefs: Readable<AppearancePrefs>;
  readonly theme: Readable<Appearance>;
  readonly platform: Readable<{ readonly native_material: boolean }>;
  readonly target?: AppearanceTarget | null;
}

/**
 * Keeps the document in step with the stores. Returns the unsubscribe. Outside a
 * document (SSR, a unit test with no DOM) it does nothing and says so by
 * returning a no-op.
 */
export function installAppearance(sources: AppearanceSources): () => void {
  const target = sources.target ?? (typeof document === "undefined" ? null : document.documentElement);
  if (!target) return () => {};
  const combined = derived(
    [sources.prefs, sources.theme, sources.platform],
    ([prefs, theme, platform]) => ({ prefs, theme, nativeMaterial: platform.native_material }),
  );
  return combined.subscribe(({ prefs, theme, nativeMaterial }) => {
    const plan = planAppearance(prefs, { theme, nativeMaterial });
    // Order matters: the override first, so the `applyAccent` below (and the one
    // App.svelte runs on every accent change) writes the same value.
    setAccentOverride(plan.accentOverride);
    writeAppearance(plan, target);
    applyAccent(prefs.accent, theme, target);
    setUserTransparencyReduced(plan.transparencyReduced);
    noteStore.set(plan.notes);
  });
}

/** Contrast of two hex colours, for the settings panel's readout. */
export function hexContrast(a: string, b: string): number | null {
  const first = parseHex(a);
  const second = parseHex(b);
  return first && second ? contrastRatio(first, second) : null;
}

