/**
 * Validator and exporter for the `gitpulse-theme/1` file format
 * (DESIGN_LANGUAGE.md section 11).
 *
 * A theme file is untrusted input. The validator reads it with an allow-list:
 * every accepted value is a hex colour, a number inside a stated range, an
 * enum member or a plain name, and the result is a freshly built object, never
 * the input. Nothing that could reach CSS as anything but a colour or a number
 * is accepted: no `url()`, `var()`, functions, declarations, z-index, durations,
 * filters or font URLs. Unknown keys are reported and ignored; a few keys that
 * name a forbidden capability are errors rather than ignored.
 */
import { formatHex, parseHex } from "../color/oklch";

export const THEME_SCHEMA_ID = "gitpulse-theme/1";

/** Longest accepted `name` or font family. */
export const MAX_NAME_LENGTH = 64;
/** Most own keys one object may carry before the file is refused outright. */
export const MAX_KEYS_PER_OBJECT = 1024;
/** Longest key path echoed back in an error or an ignored-key entry. */
const MAX_PATH_LENGTH = 80;

export const CONTRAST_RANGE = { min: 0, max: 1 } as const;
export const GAIN_RANGE = { min: 0, max: 1 } as const;
export const RADIUS_SCALE_RANGE = { min: 0.5, max: 1.25 } as const;

export type ThemeAppearance = "dark" | "light";

export interface ThemeColors {
  readonly base: string;
  readonly accent: string;
  readonly contrast?: number;
  readonly ok?: string;
  readonly warn?: string;
  readonly danger?: string;
  readonly info?: string;
  readonly text?: string;
  readonly textMuted?: string;
}

export interface GitPulseTheme {
  readonly schema: typeof THEME_SCHEMA_ID;
  readonly name: string;
  readonly appearance: ThemeAppearance;
  readonly colors: ThemeColors;
  readonly field?: { readonly hues: readonly [string, string, string, string]; readonly gain: number };
  readonly shape?: { readonly radiusScale: number };
  readonly fonts?: { readonly ui: string; readonly mono: string };
}

export interface ThemeError {
  readonly path: string;
  readonly message: string;
}

export type ThemeValidation =
  | {
      readonly ok: true;
      readonly theme: GitPulseTheme;
      /** Unknown keys that were dropped, as dotted paths. */
      readonly ignoredKeys: readonly string[];
    }
  | { readonly ok: false; readonly errors: readonly ThemeError[] };

/**
 * Keys that name something a theme must never carry. Matched after lowercasing
 * and removing `-` and `_`. Seeing one is an error, not a quiet ignore: the
 * file is asking for a capability the format refuses by rule.
 */
const FORBIDDEN_KEYS: ReadonlySet<string> = new Set([
  "zindex",
  "duration",
  "durations",
  "transition",
  "animation",
  "easing",
  "filter",
  "backdropfilter",
  "blur",
  "url",
  "fonturl",
  "fontface",
  "css",
  "style",
  "import",
]);

/** Value shapes that can reach a stylesheet as something other than text. */
const DANGEROUS_VALUE = [
  { pattern: /url\s*\(/i, label: "url()" },
  { pattern: /var\s*\(/i, label: "var()" },
  { pattern: /[A-Za-z0-9_-]\(/, label: "a CSS function" },
  { pattern: /[;{}]/, label: "a CSS declaration" },
  { pattern: /:\/\/|@import|\/\*|\\/, label: "a URL or CSS escape" },
  { pattern: /z-?index/i, label: "z-index" },
  { pattern: /[<>]/, label: "markup" },
] as const;

// Unicode letters and digits, space, and a few name punctuation marks.
const NAME_PATTERN = /^[\p{L}\p{N} _.'&+()-]+$/u;
const FONT_FAMILY_PATTERN = /^[\p{L}\p{N} -]+$/u;

function clip(text: string): string {
  return text.length > MAX_PATH_LENGTH ? `${text.slice(0, MAX_PATH_LENGTH)}…` : text;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function normalizeKey(key: string): string {
  return key.toLowerCase().replaceAll("-", "").replaceAll("_", "");
}

class Reader {
  readonly errors: ThemeError[] = [];
  readonly ignored: string[] = [];

  error(path: string, message: string): void {
    this.errors.push({ path: clip(path), message });
  }

  /**
   * Own enumerable keys of `value`, with the allow-list applied: keys not in
   * `known` are ignored (or an error, for forbidden names). Returns null when
   * the object is not an object or has too many keys.
   */
  keysOf(value: unknown, path: string, known: readonly string[]): Record<string, unknown> | null {
    if (!isRecord(value)) {
      this.error(path || "(theme)", "must be an object");
      return null;
    }
    const keys = Object.keys(value);
    if (keys.length > MAX_KEYS_PER_OBJECT) {
      this.error(path || "(theme)", `has ${keys.length} keys; the limit is ${MAX_KEYS_PER_OBJECT}`);
      return null;
    }
    for (const key of keys) {
      if (known.includes(key)) continue;
      const where = path ? `${path}.${key}` : key;
      if (FORBIDDEN_KEYS.has(normalizeKey(key))) {
        this.error(where, "is not supported in a theme");
      } else {
        this.ignored.push(clip(where));
      }
    }
    return value;
  }

  /** The value at an own property, or undefined. Never walks the prototype. */
  own(object: Record<string, unknown>, key: string): unknown {
    return Object.hasOwn(object, key) ? object[key] : undefined;
  }

  /** Like `own`, but a missing key is an error. */
  required(object: Record<string, unknown>, key: string, path: string): unknown {
    const value = this.own(object, key);
    if (value === undefined) this.error(path, "is required");
    return value;
  }

  hex(value: unknown, path: string): string | undefined {
    const rgb = parseHex(value);
    if (!rgb) {
      this.error(path, "must be a #rgb or #rrggbb hex color");
      return undefined;
    }
    return formatHex(rgb);
  }

  number(value: unknown, path: string, range: { min: number; max: number }): number | undefined {
    if (typeof value !== "number" || !Number.isFinite(value)) {
      this.error(path, "must be a finite number");
      return undefined;
    }
    if (value < range.min || value > range.max) {
      this.error(path, `must be between ${range.min} and ${range.max}`);
      return undefined;
    }
    return value;
  }

  /** Rejects strings that could carry CSS or markup; returns the text if clean. */
  safeText(value: unknown, path: string): string | undefined {
    if (typeof value !== "string") {
      this.error(path, "must be a string");
      return undefined;
    }
    if (value.length > MAX_NAME_LENGTH) {
      this.error(path, `must be at most ${MAX_NAME_LENGTH} characters`);
      return undefined;
    }
    for (const { pattern, label } of DANGEROUS_VALUE) {
      if (pattern.test(value)) {
        this.error(path, `must not contain ${label}`);
        return undefined;
      }
    }
    return value;
  }

  name(value: unknown, path: string): string | undefined {
    const text = this.safeText(value, path);
    if (text === undefined) return undefined;
    const trimmed = text.trim();
    if (trimmed === "" || !NAME_PATTERN.test(trimmed)) {
      this.error(path, "must be a non-empty name of letters, digits, spaces and . _ ' & + ( ) -");
      return undefined;
    }
    return trimmed;
  }

  /** One local family name: quotes are stripped, nothing else is allowed. */
  fontFamily(value: unknown, path: string): string | undefined {
    const text = this.safeText(value, path);
    if (text === undefined) return undefined;
    const family = text.replaceAll('"', "").replaceAll("'", "").trim();
    if (family === "" || !FONT_FAMILY_PATTERN.test(family)) {
      this.error(path, "must be one font family name of letters, digits, spaces and hyphens");
      return undefined;
    }
    return family;
  }
}

const COLOR_KEYS = ["base", "accent", "contrast", "ok", "warn", "danger", "info", "text", "textMuted"] as const;
const HEX_COLOR_KEYS = ["ok", "warn", "danger", "info", "text", "textMuted"] as const;

function readColors(reader: Reader, input: unknown): ThemeColors | undefined {
  const object = reader.keysOf(input, "colors", COLOR_KEYS);
  if (!object) return undefined;
  const baseInput = reader.required(object, "base", "colors.base");
  const base = baseInput === undefined ? undefined : reader.hex(baseInput, "colors.base");
  const accentInput = reader.required(object, "accent", "colors.accent");
  const accent = accentInput === undefined ? undefined : reader.hex(accentInput, "colors.accent");
  const contrastInput = reader.own(object, "contrast");
  const contrast =
    contrastInput === undefined ? undefined : reader.number(contrastInput, "colors.contrast", CONTRAST_RANGE);

  const optional: { -readonly [K in (typeof HEX_COLOR_KEYS)[number]]?: string } = {};
  for (const key of HEX_COLOR_KEYS) {
    const value = reader.own(object, key);
    if (value === undefined) continue;
    const hex = reader.hex(value, `colors.${key}`);
    if (hex !== undefined) optional[key] = hex;
  }
  if (base === undefined || accent === undefined) return undefined;
  return {
    base,
    accent,
    ...(contrast === undefined ? {} : { contrast }),
    ...optional,
  };
}

function readField(reader: Reader, input: unknown): GitPulseTheme["field"] {
  const object = reader.keysOf(input, "field", ["hues", "gain"]);
  if (!object) return undefined;
  const huesInput = reader.own(object, "hues");
  let hues: [string, string, string, string] | undefined;
  if (!Array.isArray(huesInput) || huesInput.length !== 4) {
    reader.error("field.hues", "must be an array of exactly four hex colors");
  } else {
    const read = huesInput.map((item: unknown, index: number) => reader.hex(item, `field.hues[${index}]`));
    const [a, b, c, d] = read;
    if (a !== undefined && b !== undefined && c !== undefined && d !== undefined) hues = [a, b, c, d];
  }
  const gainInput = reader.required(object, "gain", "field.gain");
  const gain = gainInput === undefined ? undefined : reader.number(gainInput, "field.gain", GAIN_RANGE);
  return hues !== undefined && gain !== undefined ? { hues, gain } : undefined;
}

function readShape(reader: Reader, input: unknown): GitPulseTheme["shape"] {
  const object = reader.keysOf(input, "shape", ["radiusScale"]);
  if (!object) return undefined;
  const scaleInput = reader.required(object, "radiusScale", "shape.radiusScale");
  if (scaleInput === undefined) return undefined;
  const radiusScale = reader.number(scaleInput, "shape.radiusScale", RADIUS_SCALE_RANGE);
  return radiusScale === undefined ? undefined : { radiusScale };
}

function readFonts(reader: Reader, input: unknown): GitPulseTheme["fonts"] {
  const object = reader.keysOf(input, "fonts", ["ui", "mono"]);
  if (!object) return undefined;
  const uiInput = reader.required(object, "ui", "fonts.ui");
  const monoInput = reader.required(object, "mono", "fonts.mono");
  const ui = uiInput === undefined ? undefined : reader.fontFamily(uiInput, "fonts.ui");
  const mono = monoInput === undefined ? undefined : reader.fontFamily(monoInput, "fonts.mono");
  return ui !== undefined && mono !== undefined ? { ui, mono } : undefined;
}

const TOP_LEVEL_KEYS = ["schema", "name", "appearance", "colors", "field", "shape", "fonts"] as const;

/** Validates untrusted theme data. The returned theme is a fresh object. */
export function validateTheme(input: unknown): ThemeValidation {
  const reader = new Reader();
  const root = reader.keysOf(input, "", TOP_LEVEL_KEYS);
  if (!root) return { ok: false, errors: reader.errors };

  if (reader.own(root, "schema") !== THEME_SCHEMA_ID) {
    reader.error("schema", `must be "${THEME_SCHEMA_ID}"`);
  }
  const nameInput = reader.required(root, "name", "name");
  const name = nameInput === undefined ? undefined : reader.name(nameInput, "name");
  const appearanceInput = reader.own(root, "appearance");
  let appearance: ThemeAppearance | undefined;
  if (appearanceInput === "dark" || appearanceInput === "light") appearance = appearanceInput;
  else reader.error("appearance", 'must be "dark" or "light"');

  const colorsInput = reader.required(root, "colors", "colors");
  const colors = colorsInput === undefined ? undefined : readColors(reader, colorsInput);

  const fieldInput = reader.own(root, "field");
  const field = fieldInput === undefined ? undefined : readField(reader, fieldInput);
  const shapeInput = reader.own(root, "shape");
  const shape = shapeInput === undefined ? undefined : readShape(reader, shapeInput);
  const fontsInput = reader.own(root, "fonts");
  const fonts = fontsInput === undefined ? undefined : readFonts(reader, fontsInput);

  if (reader.errors.length > 0 || name === undefined || appearance === undefined || colors === undefined) {
    return { ok: false, errors: reader.errors };
  }
  const theme: GitPulseTheme = {
    schema: THEME_SCHEMA_ID,
    name,
    appearance,
    colors,
    ...(field === undefined ? {} : { field }),
    ...(shape === undefined ? {} : { shape }),
    ...(fonts === undefined ? {} : { fonts }),
  };
  return { ok: true, theme, ignoredKeys: reader.ignored };
}

export type ThemeExport =
  | { readonly ok: true; readonly theme: GitPulseTheme; readonly json: string }
  | { readonly ok: false; readonly errors: readonly ThemeError[] };

/**
 * Builds a theme file from inputs. The candidate goes through
 * `validateTheme` before it is returned, so what is written is exactly what an
 * import would accept, and the returned theme is the validator's output.
 */
export function exportTheme(inputs: Omit<GitPulseTheme, "schema">): ThemeExport {
  const candidate: Record<string, unknown> = { ...inputs, schema: THEME_SCHEMA_ID };
  const checked = validateTheme(candidate);
  if (!checked.ok) return checked;
  return { ok: true, theme: checked.theme, json: JSON.stringify(checked.theme, null, 2) };
}
