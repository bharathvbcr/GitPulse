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
import { APPEARANCE_DEFAULTS, type AppearancePrefs } from "./apply";
import { themeToPatch } from "./themeFile";

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
    id: "cobalt",
    label: "Waylode Cobalt",
    description: "Deep midnight slate with a sky-cobalt accent.",
    backdrop: "none",
    material: 0,
    theme: {
      name: "Waylode Cobalt",
      appearance: "dark",
      colors: { base: "#060a14", accent: "#38bdf8", contrast: 0.55 },
      field: { hues: ["#0c4a6e", "#1d4ed8", "#1e3a8a", "#0369a1"], gain: 0.65 },
    },
  },
  {
    id: "lodestar",
    label: "Waylode Lodestar",
    description: "Midnight navy with a lodestar teal accent and warm amber field.",
    backdrop: "none",
    material: 0,
    theme: {
      name: "Waylode Lodestar",
      appearance: "dark",
      colors: { base: "#0a0d14", accent: "#2dd4bf", contrast: 0.55 },
      field: { hues: ["#0f766e", "#b45309", "#0369a1", "#115e59"], gain: 0.65 },
    },
  },
  {
    id: "oxblood",
    label: "VBCR Oxblood",
    description: "Deep oxblood obsidian with a crimson accent.",
    backdrop: "none",
    material: 0,
    theme: {
      name: "VBCR Oxblood",
      appearance: "dark",
      colors: { base: "#0c0608", accent: "#ef4444", contrast: 0.55 },
      field: { hues: ["#7f1d1d", "#991b1b", "#450a0a", "#630e0e"], gain: 0.6 },
    },
  },
  {
    id: "gilded",
    label: "Waylode Gilded",
    description: "Dark bronze surfaces with a gilded gold accent.",
    backdrop: "none",
    material: 0,
    theme: {
      name: "Waylode Gilded",
      appearance: "dark",
      colors: { base: "#0c0905", accent: "#fbbf24", contrast: 0.55 },
      field: { hues: ["#78350f", "#92400e", "#451a03", "#b45309"], gain: 0.6 },
    },
  },
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
  // Developer looks.
  {
    id: "amber-crt",
    label: "Amber CRT",
    description: "An amber phosphor terminal with scanlines and a git log on the glass.",
    backdrop: "scanlines",
    material: 0,
    theme: {
      name: "Amber CRT",
      appearance: "dark",
      colors: { base: "#0c0700", accent: "#ffb000", contrast: 0.5 },
      field: { hues: ["#6b3f00", "#8a5200", "#4a2a00", "#a86400"], gain: 0.6 },
    },
  },
  {
    id: "blueprint",
    label: "Blueprint",
    description: "Drafting blue with a pale cyan accent over a technical drawing.",
    backdrop: "blueprint",
    material: 0,
    theme: {
      name: "Blueprint",
      appearance: "dark",
      colors: { base: "#071a33", accent: "#8fd0ff", contrast: 0.55 },
      field: { hues: ["#0b3d7a", "#12528f", "#0a2f5e", "#1a6aa8"], gain: 0.6 },
    },
  },
  {
    id: "midnight-ide",
    label: "Midnight IDE",
    description: "A soft charcoal editor with a lavender accent and pink highlights.",
    backdrop: "none",
    material: 0,
    theme: {
      name: "Midnight IDE",
      appearance: "dark",
      colors: { base: "#1c1d26", accent: "#b38cff", contrast: 0.5 },
      field: { hues: ["#5b3e9e", "#9c3f80", "#2f4a8a", "#3e8a6b"], gain: 0.6 },
    },
  },
  {
    id: "arctic",
    label: "Arctic",
    description: "Cool slate greys with a frost-blue accent.",
    backdrop: "none",
    material: 0,
    theme: {
      name: "Arctic",
      appearance: "dark",
      colors: { base: "#232a36", accent: "#7fc8d8", contrast: 0.5 },
      field: { hues: ["#3b5a7a", "#4f7a8f", "#2e4560", "#5f6f8f"], gain: 0.5 },
    },
  },
  {
    id: "solar-dark",
    label: "Solar Dark",
    description: "Deep teal surfaces with a warm yellow accent, easy on long sessions.",
    backdrop: "none",
    material: 0,
    theme: {
      name: "Solar Dark",
      appearance: "dark",
      colors: { base: "#04262e", accent: "#e0a500", contrast: 0.5 },
      field: { hues: ["#0d5560", "#7a5a0a", "#124a6a", "#5a6a10"], gain: 0.5 },
    },
  },
  {
    id: "daylight",
    label: "Daylight",
    description: "A crisp white editor look with a clear blue accent.",
    backdrop: "none",
    material: 0,
    theme: {
      name: "Daylight",
      appearance: "light",
      colors: { base: "#fafbfc", accent: "#0a63c9", contrast: 0.5 },
      field: { hues: ["#c9dcf5", "#d8e6f7", "#c4d4ec", "#e0ebf8"], gain: 0.5 },
    },
  },
  // Science-fiction looks.
  {
    id: "nebula",
    label: "Nebula",
    description: "Violet and teal gas clouds in a starfield, with a lilac accent.",
    backdrop: "nebula",
    material: 0,
    theme: {
      name: "Nebula",
      appearance: "dark",
      colors: { base: "#07050f", accent: "#c09cff", contrast: 0.55 },
      field: { hues: ["#5a1fa8", "#137f86", "#8a2a6a", "#2a2f9e"], gain: 0.8 },
    },
  },
  {
    id: "orbital",
    label: "Orbital",
    description: "A ringed planet and orbit tracks on black, with an apricot accent.",
    backdrop: "orbit",
    material: 0,
    theme: {
      name: "Orbital",
      appearance: "dark",
      colors: { base: "#05070d", accent: "#ffb36b", contrast: 0.55 },
      field: { hues: ["#8a4a1e", "#1e3a6a", "#a8642a", "#2a2f4a"], gain: 0.6 },
    },
  },
  {
    id: "neon-city",
    label: "Neon City",
    description: "A night skyline in magenta and cyan, with a cyan accent.",
    backdrop: "skyline",
    material: 0,
    theme: {
      name: "Neon City",
      appearance: "dark",
      colors: { base: "#0d0518", accent: "#36e3ff", contrast: 0.55 },
      field: { hues: ["#8a1a7a", "#13708a", "#4a1a8a", "#a8205a"], gain: 0.8 },
    },
  },
  {
    id: "hyperspace",
    label: "Hyperspace",
    description: "Star streaks rushing toward a bright core, with an ice-blue accent.",
    backdrop: "warp",
    material: 0,
    theme: {
      name: "Hyperspace",
      appearance: "dark",
      colors: { base: "#02030a", accent: "#8fb8ff", contrast: 0.55 },
      field: { hues: ["#1a2f8a", "#2a5aa8", "#101a5a", "#3a4ab0"], gain: 0.7 },
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

type ClearableFields = "customTheme" | "backdrop" | "materialIntensity" | "radiusScale" | "fontUi" | "fontMono";

/**
 * The inverse of putting a theme in force: clears the theme and puts back each
 * value it stamped, but only where that value is still the stamped one, so a
 * wallpaper, material, corner scale or font chosen afterwards is kept.
 *
 * Any theme may have stamped a corner scale and fonts (`themeToPatch` is the
 * superset of what applying one writes). A built-in preset also stamped its
 * wallpaper and its material; a saved or imported theme never touched the
 * wallpaper, so removing one leaves it alone. With no theme in force this is
 * an empty patch.
 */
export function clearThemePatch(current: Pick<AppearancePrefs, ClearableFields>): Partial<AppearancePrefs> {
  const theme = current.customTheme;
  if (theme === null) return {};
  const stamped = themeToPatch(theme);
  const keptUnless = <K extends "radiusScale" | "fontUi" | "fontMono">(key: K): Partial<AppearancePrefs> =>
    stamped[key] !== undefined && current[key] === stamped[key] ? { [key]: APPEARANCE_DEFAULTS[key] } : {};
  const preset = THEME_PRESETS.find((candidate) => sameTheme(candidate.theme, theme));
  const appearance: Appearance | null = preset ? preset.theme.appearance : null;
  return {
    customTheme: null,
    ...keptUnless("radiusScale"),
    ...keptUnless("fontUi"),
    ...keptUnless("fontMono"),
    ...(preset && current.backdrop === preset.backdrop ? { backdrop: APPEARANCE_DEFAULTS.backdrop } : {}),
    ...(preset && appearance && current.materialIntensity[appearance] === preset.material
      ? {
          materialIntensity: {
            ...current.materialIntensity,
            [appearance]: APPEARANCE_DEFAULTS.materialIntensity[appearance],
          },
        }
      : {}),
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
