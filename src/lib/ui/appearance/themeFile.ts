/**
 * Theme import and export for Settings -> Appearance (design language §11).
 *
 * Import is a file pick, and the text it reads is untrusted: it is size-capped,
 * parsed as JSON, and handed to `validateTheme`, whose allow-list rejects CSS,
 * `url()`, filters, z-index, durations and font URLs. A file that fails is a
 * refusal with the offending keys named; nothing is applied from a failed
 * import, and a half-valid file is not a thing: `validateTheme` returns a theme
 * or errors, never both.
 *
 * Export builds the file from the preferences in force and runs it back through
 * the same validator, so what is written is exactly what an import accepts.
 */
import { SHIPPED_BASE_HEX, SHIPPED_CONTRAST, type Appearance } from "../color/derive";
import { formatHex } from "../color/oklch";
import {
  FIELD_GAIN,
  SHIPPED_FIELD,
  SHIPPED_RADIUS_SCALE,
  resolveAccent,
  rotateHueHex,
  type AppearancePrefs,
} from "./apply";
import { DEFAULT_ACCENT } from "../accents";
import {
  exportTheme,
  validateTheme,
  type GitPulseTheme,
  type ThemeError,
} from "./themeSchema";

/** A theme file is a few hundred bytes; 64 KiB is generous and bounds the parse. */
export const MAX_THEME_FILE_BYTES = 64 * 1024;

/** The generic family names a theme file uses for "the system font". */
const SYSTEM_UI = "system-ui";
const SYSTEM_MONO = "ui-monospace";

export type ThemeImport =
  | { readonly ok: true; readonly theme: GitPulseTheme; readonly ignoredKeys: readonly string[] }
  | { readonly ok: false; readonly reason: string; readonly errors: readonly ThemeError[] };

/** Parses and validates theme file text. Never throws. */
export function importThemeText(text: string): ThemeImport {
  if (new TextEncoder().encode(text).length > MAX_THEME_FILE_BYTES) {
    return {
      ok: false,
      reason: `The file is larger than ${MAX_THEME_FILE_BYTES / 1024} KB, which is more than a theme needs.`,
      errors: [],
    };
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return { ok: false, reason: "The file isn't valid JSON.", errors: [] };
  }
  const checked = validateTheme(parsed);
  if (checked.ok) return { ok: true, theme: checked.theme, ignoredKeys: checked.ignoredKeys };
  const first = checked.errors.slice(0, 3).map((error) => `${error.path} ${error.message}`);
  const more = checked.errors.length - first.length;
  return {
    ok: false,
    reason: `${first.join("; ")}${more > 0 ? `; and ${more} more` : ""}.`,
    errors: checked.errors,
  };
}

/** The preference changes importing `theme` makes. The palette stays in `customTheme`. */
export function themeToPatch(theme: GitPulseTheme): Partial<AppearancePrefs> {
  return {
    customTheme: theme,
    // The theme brings its own accent; a leftover custom accent would hide it.
    customAccent: "",
    ...(theme.shape === undefined ? {} : { radiusScale: theme.shape.radiusScale }),
    ...(theme.fonts === undefined
      ? {}
      : {
          fontUi: theme.fonts.ui === SYSTEM_UI ? "" : theme.fonts.ui,
          fontMono: theme.fonts.mono === SYSTEM_MONO ? "" : theme.fonts.mono,
        }),
  };
}

/** Suggested file name for an exported theme: the name, lower-cased, hyphenated. */
export function themeFileName(theme: GitPulseTheme): string {
  const slug = theme.name
    .toLowerCase()
    .replace(/[^\p{L}\p{N}]+/gu, "-")
    .replace(/^-+|-+$/g, "");
  return `${slug === "" ? "gitpulse-theme" : slug}.gitpulse-theme.json`;
}

export type ThemeExportResult =
  | { readonly ok: true; readonly theme: GitPulseTheme; readonly json: string; readonly fileName: string }
  | { readonly ok: false; readonly reason: string; readonly errors: readonly ThemeError[] };

/**
 * The theme file for the window as it is now, in `appearance` mode. Colours are
 * the resolved ones (after the guard), so a theme round-trips.
 */
export function exportCurrentTheme(
  prefs: AppearancePrefs,
  appearance: Appearance,
  name: string,
): ThemeExportResult {
  const active =
    prefs.customTheme !== null && prefs.customTheme.appearance === appearance ? prefs.customTheme : null;
  const base = active?.colors.base ?? SHIPPED_BASE_HEX[appearance];
  const contrast = active?.colors.contrast ?? SHIPPED_CONTRAST;
  const accentChosen = prefs.customAccent !== "" || prefs.accent !== DEFAULT_ACCENT;
  const accent =
    active !== null && !accentChosen
      ? active.colors.accent
      : resolveAccent(prefs, appearance, base, contrast).resolved;
  const colors = { ...(active?.colors ?? {}), base, accent, contrast };
  const shipped = SHIPPED_FIELD[appearance];
  const sourceHues = active?.field?.hues ?? shipped.hues.map((hue) => formatHex(hue));
  const hues = sourceHues.map((hex) => rotateHueHex(hex, active?.field ? 0 : prefs.fieldHue));
  const field = {
    hues: [hues[0], hues[1], hues[2], hues[3]] as const,
    gain: active?.field ? active.field.gain : FIELD_GAIN[prefs.fieldLevel],
  };
  const result = exportTheme({
    name,
    appearance,
    colors,
    field,
    ...(Math.abs(prefs.radiusScale - SHIPPED_RADIUS_SCALE) > 1e-6
      ? { shape: { radiusScale: prefs.radiusScale } }
      : {}),
    ...(prefs.fontUi !== "" || prefs.fontMono !== ""
      ? { fonts: { ui: prefs.fontUi || SYSTEM_UI, mono: prefs.fontMono || SYSTEM_MONO } }
      : {}),
  });
  if (!result.ok) {
    const first = result.errors.slice(0, 3).map((error) => `${error.path} ${error.message}`);
    return { ok: false, reason: `${first.join("; ")}.`, errors: result.errors };
  }
  return { ok: true, theme: result.theme, json: result.json, fileName: themeFileName(result.theme) };
}
