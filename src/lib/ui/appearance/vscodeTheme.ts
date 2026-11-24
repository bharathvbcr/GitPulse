/**
 * VS Code color theme (`.json` / JSONC) and `.vsix` extension archive importer.
 *
 * Converts untrusted VS Code theme files into validated `gitpulse-theme/1`
 * palettes through `validateTheme` and `deriveTokens`, enforcing >= 30 deg
 * OKLCH hue separation between vibrant accents and semantic status colors so
 * imported accents never masquerade as success, warning, danger, or info.
 */
import { deriveTokens, SHIPPED_BASE_HEX, SHIPPED_CONTRAST, STATUS_HUES, type Appearance } from "../color/derive";
import { ensureContrast, TEXT_MIN } from "../color/guard";
import {
  clampToGamut,
  formatHex,
  normalizeHue,
  oklchToRgb,
  relativeLuminance,
  rgbToOklch,
  type Oklch,
  type Rgb,
} from "../color/oklch";
import { MAX_NAME_LENGTH, validateTheme } from "./themeSchema";
import type { ThemeImport } from "./themeFile";

export const MAX_VSCODE_JSON_BYTES = 256 * 1024;
export const MAX_VSIX_BYTES = 2 * 1024 * 1024;
export const MAX_VSIX_ENTRIES = 256;
export const MAX_VSIX_ENTRY_BYTES = 512 * 1024;
export const MAX_VSCODE_COLOR_KEYS = 3000;
export const MIN_STATUS_ACCENT_SEPARATION = 30;

const ZIP_EOCD_SIG = 0x06054b50;
const ZIP_CD_SIG = 0x02014b50;
const ZIP_LFH_SIG = 0x04034b50;
const ZIP_METHOD_STORED = 0;
const ZIP_METHOD_DEFLATE = 8;

const DEFAULT_DARK_BASE: Rgb = [30, 30, 30];
const DEFAULT_LIGHT_BASE: Rgb = [255, 255, 255];
const DEFAULT_ACCENT: Readonly<Record<Appearance, string>> = {
  dark: "#4f8cff",
  light: "#2563eb",
};
const ACHROMATIC_ACCENT_CHROMA = 0.04;

export interface VsCodeThemeHints {
  readonly fallbackName?: string;
  readonly appearanceHint?: Appearance;
}

/**
 * Strips `//` line comments, `/* ... * /` block comments, and trailing commas
 * outside string literals without touching URLs or escaped quotes in strings.
 */
export function stripJsonc(text: string): string {
  const withoutComments = stripJsoncComments(text);
  return stripTrailingCommas(withoutComments);
}

function stripJsoncComments(text: string): string {
  let out = "";
  let i = 0;
  let inString = false;
  let escaped = false;
  while (i < text.length) {
    const ch = text[i];
    if (inString) {
      out += ch;
      if (escaped) {
        escaped = false;
      } else if (ch === "\\") {
        escaped = true;
      } else if (ch === '"') {
        inString = false;
      }
      i += 1;
      continue;
    }
    if (ch === '"') {
      inString = true;
      out += ch;
      i += 1;
      continue;
    }
    if (ch === "/" && text[i + 1] === "/") {
      i += 2;
      while (i < text.length && text[i] !== "\n" && text[i] !== "\r") i += 1;
      continue;
    }
    if (ch === "/" && text[i + 1] === "*") {
      i += 2;
      while (i < text.length && !(text[i] === "*" && text[i + 1] === "/")) i += 1;
      if (i < text.length) i += 2;
      continue;
    }
    out += ch;
    i += 1;
  }
  return out;
}

function stripTrailingCommas(text: string): string {
  let out = "";
  let i = 0;
  let inString = false;
  let escaped = false;
  while (i < text.length) {
    const ch = text[i];
    if (inString) {
      out += ch;
      if (escaped) {
        escaped = false;
      } else if (ch === "\\") {
        escaped = true;
      } else if (ch === '"') {
        inString = false;
      }
      i += 1;
      continue;
    }
    if (ch === '"') {
      inString = true;
      out += ch;
      i += 1;
      continue;
    }
    if (ch === ",") {
      let lookahead = i + 1;
      while (lookahead < text.length && /\s/.test(text[lookahead])) lookahead += 1;
      if (text[lookahead] === "}" || text[lookahead] === "]") {
        i += 1;
        continue;
      }
    }
    out += ch;
    i += 1;
  }
  return out;
}

const HEX_3_OR_4 = /^#[0-9a-fA-F]{3,4}$/;
const HEX_6_OR_8 = /^#[0-9a-fA-F]{6}(?:[0-9a-fA-F]{2})?$/;

/** Parses `#rgb`, `#rgba`, `#rrggbb`, or `#rrggbbaa`, compositing alpha over `backdrop`. */
export function parseHexWithAlpha(raw: unknown, backdrop: Rgb): Rgb | null {
  if (typeof raw !== "string") return null;
  const trimmed = raw.trim();
  let r: number;
  let g: number;
  let b: number;
  let a = 255;
  if (HEX_3_OR_4.test(trimmed)) {
    r = Number.parseInt(trimmed[1] + trimmed[1], 16);
    g = Number.parseInt(trimmed[2] + trimmed[2], 16);
    b = Number.parseInt(trimmed[3] + trimmed[3], 16);
    if (trimmed.length === 5) {
      a = Number.parseInt(trimmed[4] + trimmed[4], 16);
    }
  } else if (HEX_6_OR_8.test(trimmed)) {
    r = Number.parseInt(trimmed.slice(1, 3), 16);
    g = Number.parseInt(trimmed.slice(3, 5), 16);
    b = Number.parseInt(trimmed.slice(5, 7), 16);
    if (trimmed.length === 9) {
      a = Number.parseInt(trimmed.slice(7, 9), 16);
    }
  } else {
    return null;
  }
  if (a >= 255) return [r, g, b];
  const alpha = a / 255;
  return [
    Math.round(r * alpha + backdrop[0] * (1 - alpha)),
    Math.round(g * alpha + backdrop[1] * (1 - alpha)),
    Math.round(b * alpha + backdrop[2] * (1 - alpha)),
  ];
}

/**
 * Shortest angular distance in degrees between two hues in `[-180, 180)`.
 */
export function signedHueDistance(fromHue: number, toHue: number): number {
  const delta = normalizeHue(toHue - fromHue);
  return delta >= 180 ? delta - 360 : delta;
}

/**
 * Rotates `statusHue` in OKLCH so it sits at least `minSeparation` degrees
 * away from vibrant accents (`accent.c >= 0.04`).
 */
export function clearHueFromAccent(
  statusHue: number,
  accent: Oklch,
  minSeparation = MIN_STATUS_ACCENT_SEPARATION,
): number {
  const normalized = normalizeHue(statusHue);
  if (!Number.isFinite(accent.c) || accent.c < ACHROMATIC_ACCENT_CHROMA) return normalized;
  const delta = signedHueDistance(accent.h, normalized);
  if (Math.abs(delta) >= minSeparation) return normalized;
  return delta >= 0
    ? normalizeHue(accent.h + minSeparation)
    : normalizeHue(accent.h - minSeparation);
}

function isPlainRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function pickColor(
  colors: Record<string, unknown>,
  keys: readonly string[],
  backdrop: Rgb,
): Rgb | undefined {
  for (const key of keys) {
    if (!Object.hasOwn(colors, key)) continue;
    const parsed = parseHexWithAlpha(colors[key], backdrop);
    if (parsed) return parsed;
  }
  return undefined;
}

function sanitizeThemeName(raw: unknown, fallback = "Imported VS Code Theme"): string {
  const source = typeof raw === "string" ? raw : fallback;
  const cleaned = source
    .replace(/\b(?:url|var)\s*\(/gi, "(")
    .replace(/z-?index/gi, "")
    .replace(/([A-Za-z0-9_-])\(/g, "$1 (")
    .replace(/[^\p{L}\p{N} _.'&+()-]+/gu, " ")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, MAX_NAME_LENGTH)
    .trim();
  return cleaned || fallback.slice(0, MAX_NAME_LENGTH);
}

function inferAppearance(rawType: unknown, base: Rgb | undefined, hint?: Appearance): Appearance {
  if (rawType === "light" || rawType === "hcLight") return "light";
  if (rawType === "dark" || rawType === "hcDark") return "dark";
  if (hint) return hint;
  if (base) return relativeLuminance(base) >= 0.25 ? "light" : "dark";
  return "dark";
}

function fitReadableColor(candidate: Rgb, base: Rgb, appearance: Appearance): Rgb {
  const direct = ensureContrast(candidate, base, TEXT_MIN);
  if (direct.ok) return direct.color;
  const lch = rgbToOklch(candidate);
  const targetL = appearance === "dark" ? 0.78 : 0.45;
  const shifted = oklchToRgb(clampToGamut({ l: targetL, c: lch.c, h: lch.h }));
  const retried = ensureContrast(shifted, base, TEXT_MIN);
  if (retried.ok) return retried.color;
  const fallback = appearance === "dark" ? [233, 237, 248] : [34, 40, 62];
  return [fallback[0], fallback[1], fallback[2]];
}

function clearAndFitStatusColor(
  candidate: Rgb | undefined,
  defaultHue: number,
  accent: Oklch,
  base: Rgb,
  appearance: Appearance,
): string {
  const sourceLch: Oklch = candidate
    ? rgbToOklch(candidate)
    : {
        l: appearance === "dark" ? 0.78 : 0.48,
        c: appearance === "dark" ? 0.15 : 0.14,
        h: defaultHue,
      };
  const safeHue = clearHueFromAccent(
    sourceLch.c >= ACHROMATIC_ACCENT_CHROMA ? sourceLch.h : defaultHue,
    accent,
  );
  const clearedRgb = oklchToRgb(
    clampToGamut({
      l: sourceLch.l,
      c: Math.max(sourceLch.c, 0.11),
      h: safeHue,
    }),
  );
  return formatHex(fitReadableColor(clearedRgb, base, appearance));
}

function extractThemeNameFromFileName(fileName?: string): string | undefined {
  if (!fileName) return undefined;
  const leaf = fileName.split(/[\\/]/).pop() ?? "";
  return leaf.replace(/\.(?:vsix|jsonc|json)$/i, "").replace(/[-_]+/g, " ").trim() || undefined;
}

/** Returns true when parsed JSON looks like a VS Code color theme (not `gitpulse-theme/1`). */
export function isVsCodeThemeObject(parsed: unknown): parsed is Record<string, unknown> {
  return (
    isPlainRecord(parsed) &&
    !Object.hasOwn(parsed, "schema") &&
    isPlainRecord(parsed.colors)
  );
}

/**
 * Parses and converts a VS Code `.json` / JSONC theme string into a validated
 * `gitpulse-theme/1` palette with OKLCH accent-status hue clearance (`>= 30 deg`).
 */
export function importVsCodeThemeText(text: string, hints: VsCodeThemeHints = {}): ThemeImport {
  if (new TextEncoder().encode(text).length > MAX_VSCODE_JSON_BYTES) {
    return {
      ok: false,
      reason: `The VS Code theme is larger than ${MAX_VSCODE_JSON_BYTES / 1024} KB.`,
      errors: [],
    };
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(stripJsonc(text));
  } catch {
    return { ok: false, reason: "The VS Code theme isn't valid JSON/JSONC.", errors: [] };
  }
  if (!isPlainRecord(parsed) || !isPlainRecord(parsed.colors)) {
    return { ok: false, reason: "The theme file has no VS Code `colors` object.", errors: [] };
  }
  const colorEntries = Object.keys(parsed.colors);
  if (colorEntries.length > MAX_VSCODE_COLOR_KEYS) {
    return {
      ok: false,
      reason: `The VS Code theme defines ${colorEntries.length} color keys; the limit is ${MAX_VSCODE_COLOR_KEYS}.`,
      errors: [],
    };
  }

  const initialBackdrop = hints.appearanceHint === "light" ? DEFAULT_LIGHT_BASE : DEFAULT_DARK_BASE;
  const baseRaw = pickColor(
    parsed.colors,
    ["editor.background", "sideBar.background", "panel.background", "workbench.background"],
    initialBackdrop,
  );
  const appearance = inferAppearance(parsed.type, baseRaw, hints.appearanceHint);
  const fallbackBaseHex = SHIPPED_BASE_HEX[appearance];
  const baseRgb =
    baseRaw ??
    parseHexWithAlpha(fallbackBaseHex, appearance === "light" ? DEFAULT_LIGHT_BASE : DEFAULT_DARK_BASE) ??
    initialBackdrop;
  const baseHex = formatHex(baseRgb);

  const accentRaw =
    pickColor(
      parsed.colors,
      [
        "focusBorder",
        "button.background",
        "textLink.foreground",
        "activityBarBadge.background",
        "statusBar.background",
      ],
      baseRgb,
    ) ?? (parseHexWithAlpha(DEFAULT_ACCENT[appearance], baseRgb) as Rgb);
  const accentFit = fitReadableColor(accentRaw, baseRgb, appearance);
  const accentOklch = rgbToOklch(accentFit);

  const textRaw = pickColor(parsed.colors, ["editor.foreground", "foreground"], baseRgb);
  const mutedRaw = pickColor(
    parsed.colors,
    ["descriptionForeground", "editorLineNumber.foreground", "sideBarTitle.foreground"],
    baseRgb,
  );

  const okRaw = pickColor(
    parsed.colors,
    ["gitDecoration.addedResourceForeground", "terminal.ansiGreen", "testing.iconPassed"],
    baseRgb,
  );
  const warnRaw = pickColor(
    parsed.colors,
    ["editorWarning.foreground", "list.warningForeground", "terminal.ansiYellow"],
    baseRgb,
  );
  const dangerRaw = pickColor(
    parsed.colors,
    ["editorError.foreground", "errorForeground", "list.errorForeground", "terminal.ansiRed"],
    baseRgb,
  );
  const infoRaw = pickColor(
    parsed.colors,
    ["editorInfo.foreground", "textLink.foreground", "terminal.ansiBlue"],
    baseRgb,
  );

  const candidate = {
    schema: "gitpulse-theme/1",
    name: sanitizeThemeName(parsed.name, hints.fallbackName ?? "Imported VS Code Theme"),
    appearance,
    colors: {
      base: baseHex,
      accent: formatHex(accentFit),
      contrast: SHIPPED_CONTRAST,
      ok: clearAndFitStatusColor(okRaw, STATUS_HUES.ok, accentOklch, baseRgb, appearance),
      warn: clearAndFitStatusColor(warnRaw, STATUS_HUES.warn, accentOklch, baseRgb, appearance),
      danger: clearAndFitStatusColor(dangerRaw, STATUS_HUES.danger, accentOklch, baseRgb, appearance),
      info: clearAndFitStatusColor(infoRaw, STATUS_HUES.info, accentOklch, baseRgb, appearance),
      ...(textRaw ? { text: formatHex(fitReadableColor(textRaw, baseRgb, appearance)) } : {}),
      ...(mutedRaw ? { textMuted: formatHex(fitReadableColor(mutedRaw, baseRgb, appearance)) } : {}),
    },
  };

  const validated = validateTheme(candidate);
  if (!validated.ok) {
    const first = validated.errors.slice(0, 3).map((e) => `${e.path} ${e.message}`);
    return { ok: false, reason: `${first.join("; ")}.`, errors: validated.errors };
  }

  const derived = deriveTokens(
    { ...validated.theme.colors, contrast: validated.theme.colors.contrast ?? SHIPPED_CONTRAST },
    validated.theme.appearance,
  );
  if (!derived.ok) {
    // Fall back to canonical contrast-safe base + accent if the imported base made surfaces uncontrastable.
    const safeCandidate = {
      ...validated.theme,
      colors: {
        base: SHIPPED_BASE_HEX[appearance],
        accent: formatHex(accentFit),
        contrast: SHIPPED_CONTRAST,
        ok: validated.theme.colors.ok,
        warn: validated.theme.colors.warn,
        danger: validated.theme.colors.danger,
        info: validated.theme.colors.info,
      },
    };
    const rechecked = validateTheme(safeCandidate);
    if (!rechecked.ok) {
      return { ok: false, reason: "The VS Code theme colors couldn't pass contrast checks.", errors: rechecked.errors };
    }
    return { ok: true, theme: rechecked.theme, ignoredKeys: [] };
  }

  return { ok: true, theme: validated.theme, ignoredKeys: [] };
}

interface ZipEntry {
  readonly name: string;
  readonly method: number;
  readonly compressedSize: number;
  readonly uncompressedSize: number;
  readonly localHeaderOffset: number;
}

function isSafeArchivePath(name: string): boolean {
  if (!name || name.startsWith("/") || name.includes("\\") || name.includes("\0")) return false;
  return !name.split("/").some((segment) => segment === "..");
}

function findEocdOffset(bytes: Uint8Array): number {
  const minOffset = Math.max(0, bytes.length - 22 - 65_535);
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  for (let offset = bytes.length - 22; offset >= minOffset; offset -= 1) {
    if (view.getUint32(offset, true) === ZIP_EOCD_SIG) return offset;
  }
  return -1;
}

function readZipCentralDirectory(bytes: Uint8Array): ZipEntry[] {
  const eocdOffset = findEocdOffset(bytes);
  if (eocdOffset < 0) {
    throw new Error("The .vsix file is not a valid ZIP archive.");
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const entryCount = view.getUint16(eocdOffset + 10, true);
  if (entryCount === 0xffff) {
    throw new Error("ZIP64 .vsix archives are not supported.");
  }
  if (entryCount > MAX_VSIX_ENTRIES) {
    throw new Error(`The .vsix archive has ${entryCount} entries; the limit is ${MAX_VSIX_ENTRIES}.`);
  }
  const cdOffset = view.getUint32(eocdOffset + 16, true);
  const cdSize = view.getUint32(eocdOffset + 12, true);
  if (cdOffset + cdSize > bytes.length) {
    throw new Error("The .vsix archive directory is truncated.");
  }

  const decoder = new TextDecoder("utf-8", { fatal: true });
  const entries: ZipEntry[] = [];
  let cursor = cdOffset;
  for (let idx = 0; idx < entryCount; idx += 1) {
    if (cursor + 46 > bytes.length || view.getUint32(cursor, true) !== ZIP_CD_SIG) {
      throw new Error("The .vsix archive directory is malformed.");
    }
    const method = view.getUint16(cursor + 10, true);
    const compressedSize = view.getUint32(cursor + 20, true);
    const uncompressedSize = view.getUint32(cursor + 24, true);
    const nameLen = view.getUint16(cursor + 28, true);
    const extraLen = view.getUint16(cursor + 30, true);
    const commentLen = view.getUint16(cursor + 32, true);
    const localHeaderOffset = view.getUint32(cursor + 42, true);
    const nameEnd = cursor + 46 + nameLen;
    if (nameEnd > bytes.length) {
      throw new Error("The .vsix entry name extends past EOF.");
    }
    const name = decoder.decode(bytes.subarray(cursor + 46, nameEnd));
    if (!isSafeArchivePath(name)) {
      throw new Error(`Refusing unsafe archive path "${name}".`);
    }
    entries.push({ name, method, compressedSize, uncompressedSize, localHeaderOffset });
    cursor = nameEnd + extraLen + commentLen;
  }
  return entries;
}

async function inflateBounded(compressed: Uint8Array, maxBytes: number): Promise<Uint8Array> {
  const Decompress = globalThis.DecompressionStream;
  if (typeof Decompress !== "function") {
    throw new Error("Deflate decompression is unavailable in this runtime.");
  }
  const stream = new Blob([Uint8Array.from(compressed)])
    .stream()
    .pipeThrough(new Decompress("deflate-raw"));
  const reader = stream.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > maxBytes) {
      await reader.cancel();
      throw new Error(`Archive entry exceeds ${maxBytes / 1024} KB uncompressed limit.`);
    }
    chunks.push(value);
  }
  const out = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return out;
}

async function readZipEntryBytes(bytes: Uint8Array, entry: ZipEntry): Promise<Uint8Array> {
  if (entry.uncompressedSize > MAX_VSIX_ENTRY_BYTES) {
    throw new Error(`Archive entry "${entry.name}" exceeds ${MAX_VSIX_ENTRY_BYTES / 1024} KB limit.`);
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const lfh = entry.localHeaderOffset;
  if (lfh + 30 > bytes.length || view.getUint32(lfh, true) !== ZIP_LFH_SIG) {
    throw new Error(`Archive entry "${entry.name}" has an invalid local header.`);
  }
  const nameLen = view.getUint16(lfh + 26, true);
  const extraLen = view.getUint16(lfh + 28, true);
  const dataStart = lfh + 30 + nameLen + extraLen;
  const dataEnd = dataStart + entry.compressedSize;
  if (dataEnd > bytes.length) {
    throw new Error(`Archive entry "${entry.name}" is truncated.`);
  }
  const payload = bytes.subarray(dataStart, dataEnd);
  if (entry.method === ZIP_METHOD_STORED) {
    if (payload.byteLength > MAX_VSIX_ENTRY_BYTES) {
      throw new Error(`Archive entry "${entry.name}" exceeds ${MAX_VSIX_ENTRY_BYTES / 1024} KB limit.`);
    }
    return payload;
  }
  if (entry.method === ZIP_METHOD_DEFLATE) {
    return inflateBounded(payload, MAX_VSIX_ENTRY_BYTES);
  }
  throw new Error(`Unsupported ZIP compression method (${entry.method}) in .vsix archive.`);
}

function resolveArchiveRelativePath(manifestPath: string, relPath: string): string {
  const baseParts = manifestPath.split("/").slice(0, -1);
  for (const part of relPath.replaceAll("\\", "/").split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") {
      throw new Error("Refusing path traversal in .vsix theme path.");
    }
    baseParts.push(part);
  }
  return baseParts.join("/");
}

function uiThemeToAppearance(uiTheme: unknown): Appearance | undefined {
  if (uiTheme === "vs" || uiTheme === "hc-light") return "light";
  if (uiTheme === "vs-dark" || uiTheme === "hc-black") return "dark";
  return undefined;
}

/**
 * Extracts and converts the primary theme from a VS Code `.vsix` extension archive.
 */
export async function importVsixBytes(bytes: Uint8Array, fileName?: string): Promise<ThemeImport> {
  if (bytes.byteLength > MAX_VSIX_BYTES) {
    return {
      ok: false,
      reason: `The .vsix file is larger than ${MAX_VSIX_BYTES / (1024 * 1024)} MB.`,
      errors: [],
    };
  }
  try {
    const entries = readZipCentralDirectory(bytes);
    const byName = new Map(entries.map((entry) => [entry.name, entry]));
    const manifestEntry =
      byName.get("extension/package.json") ??
      entries.find((entry) => /^[^/]+\/package\.json$/.test(entry.name));
    if (!manifestEntry) {
      return { ok: false, reason: "The .vsix archive has no extension/package.json manifest.", errors: [] };
    }
    const decoder = new TextDecoder("utf-8", { fatal: true });
    const manifest = JSON.parse(decoder.decode(await readZipEntryBytes(bytes, manifestEntry))) as unknown;
    const contributes = isPlainRecord(manifest) && isPlainRecord(manifest.contributes) ? manifest.contributes : null;
    const themes = contributes && Array.isArray(contributes.themes) ? contributes.themes : null;
    if (!themes || themes.length === 0) {
      return { ok: false, reason: "The .vsix extension declares no contributes.themes entries.", errors: [] };
    }

    const manifestDisplayName =
      isPlainRecord(manifest) && typeof manifest.displayName === "string"
        ? manifest.displayName
        : extractThemeNameFromFileName(fileName);

    for (const item of themes) {
      if (!isPlainRecord(item) || typeof item.path !== "string") continue;
      const targetPath = resolveArchiveRelativePath(manifestEntry.name, item.path);
      const targetEntry = byName.get(targetPath);
      if (!targetEntry) continue;
      const themeText = decoder.decode(await readZipEntryBytes(bytes, targetEntry));
      const label = typeof item.label === "string" ? item.label : manifestDisplayName;
      const result = importVsCodeThemeText(themeText, {
        fallbackName: label,
        appearanceHint: uiThemeToAppearance(item.uiTheme),
      });
      if (result.ok) return result;
    }

    return { ok: false, reason: "No usable VS Code theme JSON was found inside the .vsix archive.", errors: [] };
  } catch (error) {
    return {
      ok: false,
      reason: error instanceof Error ? error.message : "Couldn't read the .vsix archive.",
      errors: [],
    };
  }
}
