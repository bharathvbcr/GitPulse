/**
 * Language logo and file type identifiers for GitPulse.
 * Maps programming languages, file extensions, and special filenames
 * to canonical icon keys and official brand colors.
 */

/**
 * Every mark the set can draw. Declared as data rather than as a bare union
 * so the geometry sweep in `LanguageLogo.test.ts` can iterate it: a key that
 * exists only in the type is a key no test can reach.
 */
export const ICON_KEYS = [
  "rust",
  "typescript",
  "javascript",
  "python",
  "go",
  "svelte",
  "html",
  "css",
  "c",
  "cpp",
  "csharp",
  "java",
  "ruby",
  "php",
  "swift",
  "kotlin",
  "dart",
  "shell",
  "sql",
  "lua",
  "zig",
  "json",
  "yaml",
  "toml",
  "markdown",
  "xml",
  "svg",
  "image",
  "docker",
  "git",
  "lock",
  "archive",
  "config",
  "file",
] as const;

export type LanguageIconKey = (typeof ICON_KEYS)[number];

const LANGUAGE_NAME_MAP: Record<string, LanguageIconKey> = {
  rust: "rust",
  typescript: "typescript",
  javascript: "javascript",
  python: "python",
  go: "go",
  golang: "go",
  svelte: "svelte",
  html: "html",
  css: "css",
  c: "c",
  "c++": "cpp",
  cpp: "cpp",
  "c#": "csharp",
  csharp: "csharp",
  java: "java",
  ruby: "ruby",
  php: "php",
  swift: "swift",
  kotlin: "kotlin",
  dart: "dart",
  shell: "shell",
  bash: "shell",
  zsh: "shell",
  sh: "shell",
  sql: "sql",
  lua: "lua",
  zig: "zig",
  json: "json",
  yaml: "yaml",
  toml: "toml",
  markdown: "markdown",
  xml: "xml",
  docker: "docker",
  dockerfile: "docker",
  git: "git",
  image: "image",
};

const EXTENSION_MAP: Record<string, LanguageIconKey> = {
  rs: "rust",
  ts: "typescript",
  mts: "typescript",
  cts: "typescript",
  tsx: "typescript",
  js: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  jsx: "javascript",
  py: "python",
  pyw: "python",
  ipynb: "python",
  go: "go",
  svelte: "svelte",
  html: "html",
  htm: "html",
  css: "css",
  scss: "css",
  sass: "css",
  less: "css",
  c: "c",
  h: "c",
  cpp: "cpp",
  hpp: "cpp",
  cc: "cpp",
  cxx: "cpp",
  hh: "cpp",
  cs: "csharp",
  java: "java",
  jar: "java",
  rb: "ruby",
  php: "php",
  swift: "swift",
  kt: "kotlin",
  kts: "kotlin",
  dart: "dart",
  sh: "shell",
  bash: "shell",
  zsh: "shell",
  sql: "sql",
  lua: "lua",
  zig: "zig",
  json: "json",
  jsonc: "json",
  json5: "json",
  yaml: "yaml",
  yml: "yaml",
  toml: "toml",
  md: "markdown",
  markdown: "markdown",
  mdx: "markdown",
  xml: "xml",
  svg: "svg",
  png: "image",
  jpg: "image",
  jpeg: "image",
  gif: "image",
  webp: "image",
  ico: "image",
  avif: "image",
  zip: "archive",
  tar: "archive",
  gz: "archive",
  "7z": "archive",
  rar: "archive",
  ini: "config",
  env: "config",
  cfg: "config",
  conf: "config",
};

const BRAND_COLORS: Record<LanguageIconKey, string> = {
  rust: "#dea584",
  typescript: "#3178c6",
  javascript: "#f7df1e",
  python: "#3776ab",
  go: "#00add8",
  svelte: "#ff3e00",
  html: "#e34f26",
  css: "#1572b6",
  c: "#555555",
  cpp: "#f34b7d",
  csharp: "#178600",
  java: "#b07219",
  ruby: "#701516",
  php: "#4f5d95",
  swift: "#f05138",
  kotlin: "#a97bff",
  dart: "#00b4ab",
  shell: "#89e051",
  sql: "#e38c00",
  lua: "#000080",
  zig: "#ec915c",
  json: "#cbcb41",
  yaml: "#cb171e",
  toml: "#9c4221",
  markdown: "#083fa1",
  xml: "#0060ac",
  svg: "#ff9900",
  image: "#a2d9ff",
  docker: "#384d54",
  git: "#f05032",
  lock: "#6b7280",
  archive: "#d48434",
  config: "#6e7681",
  file: "#6b7280",
};

const DISPLAY_NAMES: Record<LanguageIconKey, string> = {
  rust: "Rust",
  typescript: "TypeScript",
  javascript: "JavaScript",
  python: "Python",
  go: "Go",
  svelte: "Svelte",
  html: "HTML",
  css: "CSS",
  c: "C",
  cpp: "C++",
  csharp: "C#",
  java: "Java",
  ruby: "Ruby",
  php: "PHP",
  swift: "Swift",
  kotlin: "Kotlin",
  dart: "Dart",
  shell: "Shell",
  sql: "SQL",
  lua: "Lua",
  zig: "Zig",
  json: "JSON",
  yaml: "YAML",
  toml: "TOML",
  markdown: "Markdown",
  xml: "XML",
  svg: "SVG",
  image: "Image",
  docker: "Docker",
  git: "Git",
  lock: "Lockfile",
  archive: "Archive",
  config: "Config",
  file: "File",
};

/**
 * Resolves an icon key from a language name, full path, or file extension.
 */
export function resolveLanguageIconKey(input: string): LanguageIconKey {
  if (!input) return "file";

  const clean = input.trim().toLowerCase();

  // 1. Direct language name match (e.g. from Rust analyzer or LanguageBar)
  if (LANGUAGE_NAME_MAP[clean]) {
    return LANGUAGE_NAME_MAP[clean];
  }

  // 2. Extract filename and extension
  const normalized = clean.replace(/\\/g, "/");
  const filename = normalized.slice(normalized.lastIndexOf("/") + 1);

  // Special full filenames
  if (
    filename === "dockerfile" ||
    filename.startsWith("dockerfile.") ||
    filename.startsWith("docker-compose.") ||
    filename === "compose.yaml" ||
    filename === "compose.yml"
  ) {
    return "docker";
  }
  if (filename.startsWith(".git")) {
    return "git";
  }
  if (
    filename === "cargo.lock" ||
    filename === "package-lock.json" ||
    filename === "yarn.lock" ||
    filename === "pnpm-lock.yaml" ||
    filename === "bun.lock" ||
    filename === "bun.lockb" ||
    filename.endsWith(".lock")
  ) {
    return "lock";
  }

  const dotIdx = filename.lastIndexOf(".");
  if (dotIdx > 0 && dotIdx < filename.length - 1) {
    const ext = filename.slice(dotIdx + 1);
    if (EXTENSION_MAP[ext]) {
      return EXTENSION_MAP[ext];
    }
  }

  return "file";
}

export function getLanguageBrandColor(key: LanguageIconKey): string {
  return BRAND_COLORS[key] ?? BRAND_COLORS.file;
}

export function getLanguageDisplayName(key: LanguageIconKey): string {
  return DISPLAY_NAMES[key] ?? DISPLAY_NAMES.file;
}

/* ------------------------------------------------------------------ *
 * Display colours
 *
 * `BRAND_COLORS` are the official brand hexes and stay canonical, but a brand
 * hex is chosen against a white page, not against this app's two surfaces.
 * Painted raw they fail in both directions: Lua's `#000080` and Docker's
 * `#384d54` disappear into the dark surface, JavaScript's `#f7df1e` and the
 * image tint `#a2d9ff` disappear into the light one. Rather than hand-curate
 * a second table that can drift from the first, the display colour is derived
 * — walk the brand hue's lightness until it clears 3:1 against the worst-case
 * surface for that theme, which is exactly what `icon-contrast` asserts.
 * ------------------------------------------------------------------ */

export type IconTheme = "dark" | "light";

/**
 * The surface an icon is least legible against, per theme. Dark rows range
 * from `--c-bg` to `--c-surface-hover`, so a too-dark icon fails against the
 * lightest of them; light rows run the other way, so a too-light icon fails
 * against plain white `--c-surface`.
 */
const WORST_SURFACE: Record<IconTheme, readonly [number, number, number]> = {
  dark: [31, 39, 58],
  light: [255, 255, 255],
};

/** Non-text graphics only have to clear WCAG 1.4.11's 3:1. */
const MIN_ICON_CONTRAST = 3;
/** Ink sits on the icon colour itself, where it carries the glyph. */
const MIN_INK_CONTRAST = 4.5;

const INK_CANDIDATES = ["#ffffff", "#10141f"] as const;

function parseHex(hex: string): [number, number, number] {
  const value = hex.replace("#", "");
  const full =
    value.length === 3
      ? value
          .split("")
          .map((c) => c + c)
          .join("")
      : value;
  return [
    Number.parseInt(full.slice(0, 2), 16),
    Number.parseInt(full.slice(2, 4), 16),
    Number.parseInt(full.slice(4, 6), 16),
  ];
}

function toHex(rgb: readonly number[]): string {
  return `#${rgb
    .map((channel) => Math.round(Math.min(255, Math.max(0, channel))).toString(16).padStart(2, "0"))
    .join("")}`;
}

function relativeLuminance(rgb: readonly number[]): number {
  const [r, g, b] = rgb.map((channel) => {
    const value = channel / 255;
    return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

export function contrastRatio(a: string, b: string): number {
  const first = relativeLuminance(parseHex(a));
  const second = relativeLuminance(parseHex(b));
  return (Math.max(first, second) + 0.05) / (Math.min(first, second) + 0.05);
}

function rgbToHsl(rgb: readonly number[]): [number, number, number] {
  const [r, g, b] = rgb.map((channel) => channel / 255);
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const lightness = (max + min) / 2;
  if (max === min) return [0, 0, lightness];
  const delta = max - min;
  const saturation = delta / (1 - Math.abs(2 * lightness - 1));
  let hue: number;
  if (max === r) hue = ((g - b) / delta) % 6;
  else if (max === g) hue = (b - r) / delta + 2;
  else hue = (r - g) / delta + 4;
  hue *= 60;
  return [hue < 0 ? hue + 360 : hue, saturation, lightness];
}

function hslToHex(hue: number, saturation: number, lightness: number): string {
  const chroma = (1 - Math.abs(2 * lightness - 1)) * saturation;
  const sector = ((hue % 360) + 360) % 360 / 60;
  const second = chroma * (1 - Math.abs((sector % 2) - 1));
  const base = lightness - chroma / 2;
  const table: readonly (readonly number[])[] = [
    [chroma, second, 0],
    [second, chroma, 0],
    [0, chroma, second],
    [0, second, chroma],
    [second, 0, chroma],
    [chroma, 0, second],
  ];
  const [r, g, b] = table[Math.floor(sector) % 6];
  return toHex([(r + base) * 255, (g + base) * 255, (b + base) * 255]);
}

/**
 * Walk the brand hue toward the theme's foreground until it clears
 * `MIN_ICON_CONTRAST`. Hue and saturation are preserved so the colour still
 * reads as the brand; only lightness moves, and only as far as it must.
 */
function contrastSafeColor(brand: string, theme: IconTheme): string {
  const background = toHex(WORST_SURFACE[theme]);
  const [hue, saturation, lightness] = rgbToHsl(parseHex(brand));
  const step = theme === "dark" ? 0.02 : -0.02;
  let candidate = brand;
  for (let level = lightness; level >= 0 && level <= 1; level += step) {
    candidate = hslToHex(hue, saturation, level);
    if (contrastRatio(candidate, background) >= MIN_ICON_CONTRAST) return candidate;
  }
  return theme === "dark" ? "#ffffff" : "#000000";
}

const iconColorCache = new Map<string, string>();

/**
 * The colour an icon's body is painted in for the active theme. Falls back to
 * the brand hex whenever it already clears the bar, which is most of them.
 */
export function getLanguageIconColor(key: LanguageIconKey, theme: IconTheme): string {
  const cacheKey = `${key}:${theme}`;
  const cached = iconColorCache.get(cacheKey);
  if (cached) return cached;
  const resolved = contrastSafeColor(getLanguageBrandColor(key), theme);
  iconColorCache.set(cacheKey, resolved);
  return resolved;
}

/**
 * The colour of detail drawn *on top of* an icon body — a badge monogram, a
 * knocked-through facet. Chosen against the body, never against the page, so
 * it stays legible whichever surface the row happens to be.
 */
export function getLanguageInkColor(key: LanguageIconKey, theme: IconTheme): string {
  const body = getLanguageIconColor(key, theme);
  let best: string = INK_CANDIDATES[0];
  let bestRatio = 0;
  for (const candidate of INK_CANDIDATES) {
    const ratio = contrastRatio(candidate, body);
    if (ratio >= MIN_INK_CONTRAST) return candidate;
    if (ratio > bestRatio) {
      bestRatio = ratio;
      best = candidate;
    }
  }
  return best;
}
