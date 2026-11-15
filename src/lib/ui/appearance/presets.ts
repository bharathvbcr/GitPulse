/**
 * Built-in theme presets: the gallery at the top of Settings -> Appearance.
 *
 * A preset is a `gitpulse-theme/1` theme (so it goes through the same
 * validator, the same contrast guard and the same derivation as an imported
 * file; a preset is not a second palette path) plus the window-level choices a
 * theme file cannot carry: a wallpaper and a material intensity. Applying one is
 * a single preference patch built by `presetPatch`.
 *
 * `presets.contract.test.ts` derives its cases from this list: every preset
 * must validate, derive without a refusal, and have a material floor the
 * slider can reach. Adding a preset adds its checks.
 */
import { DEFAULT_ACCENT } from "../accents";
import { SHIPPED_CONTRAST, deriveTokens, type Appearance } from "../color/derive";
import type { BackdropId } from "./backdrops";
import { THEME_SCHEMA_ID, validateTheme, type GitPulseTheme } from "./themeSchema";
import type { AppearancePrefs } from "./apply";

export interface ThemePreset {
  readonly id: string;
  readonly label: string;
  readonly description: string;
  /** Validated at module load; the validator's output, never the literal. */
  readonly theme: GitPulseTheme;
  readonly backdrop: BackdropId;
  /**
   * Material intensity for the preset's appearance, 0..1. A wallpaper preset
   * uses 0 (the measured floor still applies) so the picture shows through;
   * Pure Black uses 1 so nothing does.
   */
  readonly material: number;
}

type PresetSource = Omit<ThemePreset, "theme"> & { readonly theme: Omit<GitPulseTheme, "schema"> };

const SOURCES: readonly PresetSource[] = [
  {
    id: "neural-hud",
    label: "Neural HUD",
    description: "Deep navy with a cyan heads-up display and a node-graph wallpaper.",
    backdrop: "neural",
    material: 0,
    theme: {
      name: "Neural HUD",
      appearance: "dark",
      colors: { base: "#06101c", accent: "#4cc9ff", contrast: 0.55 },
      field: { hues: ["#0b5f8a", "#0e8fa3", "#1d3f9e", "#0a6d7a"], gain: 0.8 },
    },
  },
  {
    id: "reactor",
    label: "Reactor",
    description: "A glowing ringed core on dark red and gold, with a cyan accent.",
    backdrop: "reactor",
    material: 0,
    theme: {
      name: "Reactor",
      appearance: "dark",
      colors: { base: "#0d0809", accent: "#5fe1ff", contrast: 0.55 },
      field: { hues: ["#8a1a1a", "#a8741c", "#127a99", "#6b1020"], gain: 0.7 },
    },
  },
  {
    id: "pure-black",
    label: "Pure Black",
    description: "True black surfaces and a neutral accent. No glow, no wallpaper.",
    backdrop: "none",
    material: 1,
    theme: {
      name: "Pure Black",
      appearance: "dark",
      colors: { base: "#000000", accent: "#d4d4d8", contrast: 0.5 },
      field: { hues: ["#000000", "#000000", "#000000", "#000000"], gain: 0 },
    },
  },
  {
    id: "crimson",
    label: "Crimson",
    description: "Black with a red accent and red hex plating.",
    backdrop: "hex",
    material: 0,
    theme: {
      name: "Crimson",
      appearance: "dark",
      colors: { base: "#080505", accent: "#ff4d4d", contrast: 0.5 },
      field: { hues: ["#7a0d0d", "#4a0808", "#9a1515", "#3a0606"], gain: 0.6 },
    },
  },
  {
    id: "phosphor",
    label: "Phosphor",
    description: "A green-on-black terminal look with circuit traces.",
    backdrop: "circuit",
    material: 0,
    theme: {
      name: "Phosphor",
      appearance: "dark",
      colors: { base: "#030b06", accent: "#3dff8a", contrast: 0.5 },
      field: { hues: ["#0b5a2a", "#0a3d1f", "#127a3a", "#06301a"], gain: 0.6 },
    },
  },
  {
    id: "synthwave",
    label: "Synthwave",
    description: "Purple dusk with a magenta accent and a retro horizon.",
    backdrop: "horizon",
    material: 0,
    theme: {
      name: "Synthwave",
      appearance: "dark",
      colors: { base: "#120a1f", accent: "#ff5fd2", contrast: 0.5 },
      field: { hues: ["#6a1b9a", "#b0207a", "#3a1e8a", "#c2410c"], gain: 0.8 },
    },
  },
  {
    id: "paper",
    label: "Paper",
    description: "A warm light theme with a rust accent.",
    backdrop: "none",
    material: 0,
    theme: {
      name: "Paper",
      appearance: "light",
      colors: { base: "#f6f1e7", accent: "#b4410f", contrast: 0.5 },
      field: { hues: ["#f2c48d", "#e8a87c", "#d9b38c", "#f0d5a8"], gain: 0.5 },
    },
  },
];

function build(source: PresetSource): ThemePreset {
  const checked = validateTheme({ ...source.theme, schema: THEME_SCHEMA_ID });
  if (!checked.ok) {
    // A shipped preset that fails the shipped validator is a build defect; say
    // which one loudly instead of shipping a gallery card that does nothing.
    const detail = checked.errors.map((error) => `${error.path} ${error.message}`).join("; ");
    throw new Error(`theme preset "${source.id}" is invalid: ${detail}`);
  }
  return { ...source, theme: checked.theme };
}

export const THEME_PRESETS: readonly ThemePreset[] = SOURCES.map(build);

export function presetById(id: string): ThemePreset | undefined {
  return THEME_PRESETS.find((preset) => preset.id === id);
}

/**
 * The preference patch that puts `theme` in force from the gallery: the theme,
 * with the accent reset so the theme's own accent draws (a leftover preset or
 * custom accent would override it in `planAppearance`).
 */
export function themeSelectionPatch(theme: GitPulseTheme): Partial<AppearancePrefs> {
  return {
    customTheme: theme,
    customAccent: "",
    accent: DEFAULT_ACCENT,
    ...(theme.shape === undefined ? {} : { radiusScale: theme.shape.radiusScale }),
  };
}

/** Everything applying `preset` changes. The caller also switches the window to `preset.theme.appearance`. */
export function presetPatch(
  preset: ThemePreset,
  current: Pick<AppearancePrefs, "materialIntensity">,
): Partial<AppearancePrefs> {
  const appearance: Appearance = preset.theme.appearance;
  return {
    ...themeSelectionPatch(preset.theme),
    backdrop: preset.backdrop,
    materialIntensity: { ...current.materialIntensity, [appearance]: preset.material },
  };
}

export interface ThemePreview {
  readonly bg: string;
  readonly surface: string;
  readonly border: string;
  readonly text: string;
  readonly textMuted: string;
  readonly accent: string;
}

const previews = new WeakMap<GitPulseTheme, ThemePreview>();

/**
 * The colours a gallery tile draws, as CSS `rgb()` values: the theme's derived
 * palette after the contrast guard, i.e. what the window would actually paint,
 * not the raw hex the file asked for. Falls back to the requested base and
 * accent when the derivation refuses (the tile still says what the file is).
 */
export function themePreview(theme: GitPulseTheme): ThemePreview {
  const known = previews.get(theme);
  if (known) return known;
  const result = deriveTokens(
    { base: theme.colors.base, accent: theme.colors.accent, contrast: theme.colors.contrast ?? SHIPPED_CONTRAST },
    theme.appearance,
  );
  const rgb = (triplet: string) => `rgb(${triplet})`;
  const preview: ThemePreview = result.ok
    ? {
        bg: rgb(result.tokens["--c-bg"]),
        surface: rgb(result.tokens["--c-surface"]),
        border: rgb(result.tokens["--c-border"]),
        text: rgb(result.tokens["--c-text"]),
        textMuted: rgb(result.tokens["--c-text-muted"]),
        accent: rgb(result.tokens["--c-accent"]),
      }
    : {
        bg: theme.colors.base,
        surface: theme.colors.base,
        border: theme.colors.accent,
        text: theme.colors.accent,
        textMuted: theme.colors.accent,
        accent: theme.colors.accent,
      };
  previews.set(theme, preview);
  return preview;
}

/** Same theme, by value: the validator's output is key-ordered, so its JSON is canonical. */
export function sameTheme(a: GitPulseTheme | null, b: GitPulseTheme | null): boolean {
  if (a === null || b === null) return a === b;
  return JSON.stringify(a) === JSON.stringify(b);
}
