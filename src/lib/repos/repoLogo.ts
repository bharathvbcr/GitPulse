/**
 * Which file in a repository is its logo, and how a tab draws one.
 *
 * Pure on purpose: the ranking is a rule over a file listing, and the
 * monogram is a rule over a name, so both are tested without a backend.
 * `repoLogoStore.ts` owns asking for the listing and the bytes.
 *
 * A repository's own files are untrusted input. They are only ever drawn as
 * an `<img>` from a `data:` URL built here, never as inline SVG or HTML: an
 * SVG inside an image element runs no script and loads nothing.
 */

import { TAB_COLORS, type TabColor } from "./tabColors";

/** How many ranked candidates are read before a repository is called logo-less. */
export const MAX_LOGO_ATTEMPTS = 4;

/** How many images the "choose a logo" row offers. */
export const MAX_LOGO_CHOICES = 8;

const IMAGE_MIME: Readonly<Record<string, string>> = {
  svg: "image/svg+xml",
  png: "image/png",
  webp: "image/webp",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  ico: "image/x-icon",
};

/** Ranking bonus by format: vectors and transparency read best at 16px. */
const FORMAT_SCORE: Readonly<Record<string, number>> = {
  svg: 6,
  png: 4,
  webp: 3,
  ico: 0,
  jpg: -2,
  jpeg: -2,
  gif: -4,
};

/**
 * Directories whose images are never a repository's own logo: dependencies,
 * build output, and test material. Matched against every path segment.
 */
const EXCLUDED_DIRS: ReadonlySet<string> = new Set([
  "node_modules",
  "vendor",
  "vendored",
  "third_party",
  "third-party",
  "external",
  "target",
  "dist",
  "out",
  ".next",
  ".svelte-kit",
  "coverage",
  "pods",
  "deriveddata",
  "test",
  "tests",
  "__tests__",
  "__snapshots__",
  "fixtures",
  "fixture",
  "testdata",
  "spec",
  "examples",
  "example",
  "samples",
  "screenshots",
  "harness",
]);

/** Directories conventionally holding a project's artwork. */
const ART_DIRS: ReadonlySet<string> = new Set([
  ".github",
  "assets",
  "asset",
  "public",
  "static",
  "docs",
  "doc",
  "images",
  "image",
  "img",
  "media",
  "branding",
  "brand",
  "art",
  "artwork",
  "resources",
  "res",
  "icons",
  "logo",
  "logos",
  "web",
  "app",
]);

/**
 * Directories that hold sub-projects with logos of their own (a plugin's, a
 * crate's). Their art is a candidate only when the repository has nothing
 * nearer the root.
 */
const SUBPROJECT_DIRS: ReadonlySet<string> = new Set([
  "plugins",
  "packages",
  "crates",
  "extensions",
  "addons",
  "libs",
  "modules",
  "integrations",
  "tools",
]);

/** Words in a file name that mean "not the logo" (marketing art, variants for one theme). */
const NOT_A_LOGO = /(banner|screenshot|screen-shot|social|og[-_]image|opengraph|preview|background|backdrop|splash|wallpaper|hero|cover|demo|header|footer|badge|sponsor|placeholder|example)/;

export function logoExtension(path: string): string | null {
  const name = path.slice(path.lastIndexOf("/") + 1);
  const dot = name.lastIndexOf(".");
  if (dot <= 0) return null;
  const ext = name.slice(dot + 1).toLowerCase();
  return ext in IMAGE_MIME ? ext : null;
}

export function logoMime(path: string): string | null {
  const ext = logoExtension(path);
  return ext ? IMAGE_MIME[ext] : null;
}

/** The pixel size a file name advertises (`128x128.png`, `icon-192.png`), or null. */
function advertisedSize(stem: string): number | null {
  const pair = /(\d{2,4})x(\d{2,4})/.exec(stem);
  if (pair) return Number(pair[1]);
  const single = /(?:^|[-_@])(\d{2,4})(?:$|[-_@x])/.exec(stem);
  return single ? Number(single[1]) : null;
}

/**
 * How much a size suits a 16–20px slot on a 2x display: 64–256 is ideal,
 * tiny ones blur and huge ones only cost bytes.
 */
function sizeScore(size: number | null): number {
  if (size === null) return 0;
  if (size >= 64 && size <= 256) return 6;
  if (size >= 32 && size < 64) return 1;
  if (size > 256 && size <= 512) return 0;
  if (size > 512) return -3;
  return -4;
}

/**
 * How likely a path is to be the repository's logo, or null when it is not
 * a candidate at all. Higher is better; only the order matters.
 *
 * The name carries most of the weight (a `logo` beats an `icon` beats a
 * `favicon`), then where it sits (the root and art folders beat a deep
 * source tree), then the format and the size its name advertises.
 */
export function scoreLogoPath(path: string, options: { anyImage?: boolean } = {}): number | null {
  const ext = logoExtension(path);
  if (!ext) return null;
  const segments = path.split("/").filter(Boolean);
  if (segments.length === 0) return null;
  const dirs = segments.slice(0, -1).map((segment) => segment.toLowerCase());
  if (dirs.some((dir) => EXCLUDED_DIRS.has(dir))) return null;
  const file = segments[segments.length - 1].toLowerCase();
  const stem = file.slice(0, file.length - ext.length - 1);
  const dirPath = dirs.join("/");

  let base: number | null = null;
  if (dirPath === ".idea" && stem === "icon") base = 140; // JetBrains' own project-icon convention
  else if (/^square\d+x\d+logo$/.test(stem) || stem === "storelogo") base = 70; // Windows tile renditions of the app icon
  else if (/(^|[-_.])logo([-_.]?(icon|mark|square|small|192|512|\d+))?$/.test(stem) || stem === "logomark") base = 100;
  else if (/^logo/.test(stem) || /logo$/.test(stem)) base = 88;
  else if (/^(app[-_]?)?icon([-_.]?\d+(x\d+)?)?(@\dx)?$/.test(stem)) base = 80;
  else if (/^\d{2,4}x\d{2,4}(@\dx)?$/.test(stem) && dirs[dirs.length - 1] === "icons") base = 78; // Tauri's icons/128x128.png
  else if (stem === "apple-touch-icon" || /^android-chrome-\d+/.test(stem)) base = 72;
  else if (dirPath.endsWith(".appiconset")) base = 70; // an Xcode AppIcon set
  else if (stem === "ic_launcher") base = 66;
  else if (stem === "ic_launcher_round") base = 62;
  else if (/^favicon/.test(stem)) base = 60;
  else if (stem === "ic_launcher_foreground") base = 30;
  else if (options.anyImage) base = 10;
  if (base === null) return null;
  if (NOT_A_LOGO.test(stem)) {
    if (!options.anyImage) return null;
    base = Math.min(base, 10);
  }

  let score = base;
  if (dirs.length === 0) score += 20;
  else if (dirs.length === 1 && ART_DIRS.has(dirs[0])) score += 14;
  else if (dirs.some((dir) => ART_DIRS.has(dir))) score += 6;
  if (dirs.some((dir) => dir === "src-tauri" || dir.endsWith(".xcassets") || dir === "mipmap-xxhdpi" || dir === "mipmap-xhdpi")) score += 4;
  // Each level deeper is a little less likely to be the project's own mark.
  score -= Math.max(0, dirs.length - 1) * 4;
  if (dirs.some((dir) => SUBPROJECT_DIRS.has(dir))) score -= 14;
  // Theme variants are second choice: the plain one fits both themes.
  if (/[-_.](dark|light|white|black|inverted|mono|monochrome|outline)\b/.test(stem)) score -= 6;
  score += FORMAT_SCORE[ext] ?? 0;
  score += sizeScore(advertisedSize(stem));
  return score;
}

/**
 * The listing's logo candidates, best first, at most `limit`. Ties keep the
 * shorter path, then the listing's own order, so the answer is stable.
 */
export function rankLogoCandidates(
  paths: readonly string[],
  limit = MAX_LOGO_ATTEMPTS,
  options: { anyImage?: boolean } = {},
): string[] {
  const scored: { path: string; score: number; index: number }[] = [];
  paths.forEach((raw, index) => {
    if (typeof raw !== "string" || raw.length === 0 || raw.length > 1024) return;
    const path = raw.replace(/\\/g, "/");
    const score = scoreLogoPath(path, options);
    if (score !== null) scored.push({ path, score, index });
  });
  scored.sort((a, b) => b.score - a.score || a.path.length - b.path.length || a.index - b.index);
  return scored.slice(0, Math.max(0, limit)).map((entry) => entry.path);
}

/** Images the logo browser holds from one listing. */
export const MAX_BROWSE_IMAGES = 5000;

/** Entries one folder, or one search, shows at once. */
export const MAX_BROWSE_SHOWN = 120;

/** Single-folder chains are collapsed into one entry at most this deep (`src-tauri/icons`). */
const MAX_COLLAPSED_DEPTH = 8;

const pathCollator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

export interface LogoImageList {
  /** Image paths in path order, at most {@link MAX_BROWSE_IMAGES}. */
  paths: string[];
  /** Every image the listing holds, kept or not. */
  total: number;
}

/**
 * Every file in a listing that a logo can be drawn from, in path order,
 * for a person to browse. Unlike {@link rankLogoCandidates} nothing is
 * excluded by where it sits: a person who goes looking in `tests/` means it.
 */
export function logoImagePaths(listing: readonly string[], limit = MAX_BROWSE_IMAGES): LogoImageList {
  const seen = new Set<string>();
  for (const raw of listing) {
    if (typeof raw !== "string" || raw.length === 0 || raw.length > 1024 || raw.includes("\0")) continue;
    const path = raw.replace(/\\/g, "/").replace(/^\/+/, "");
    if (path.length === 0 || path.endsWith("/") || !logoExtension(path)) continue;
    seen.add(path);
  }
  const paths = [...seen].sort(pathCollator.compare);
  return { paths: paths.slice(0, Math.max(0, limit)), total: paths.length };
}

export interface LogoFolderEntry {
  /** What the entry reads as; a collapsed chain reads `src-tauri/icons`. */
  name: string;
  /** The folder's repository-relative path. */
  path: string;
  /** Images at any depth below it. */
  images: number;
}

export interface LogoFolderView {
  folders: LogoFolderEntry[];
  files: string[];
  /** Folders and files before the cap; `folders.length + files.length` were kept. */
  total: number;
}

function underDir(dir: string): string {
  return dir === "" ? "" : `${dir.replace(/\/+$/, "")}/`;
}

/** The direct children of one folder: subfolder names with their image counts, and its own files. */
function children(images: readonly string[], dir: string): { folders: Map<string, number>; files: string[] } {
  const prefix = underDir(dir);
  const folders = new Map<string, number>();
  const files: string[] = [];
  for (const path of images) {
    if (!path.startsWith(prefix)) continue;
    const rest = path.slice(prefix.length);
    const slash = rest.indexOf("/");
    if (slash < 0) files.push(path);
    else {
      const name = rest.slice(0, slash);
      folders.set(name, (folders.get(name) ?? 0) + 1);
    }
  }
  return { folders, files };
}

/**
 * What the logo browser shows: the folders and images directly in `dir`
 * (`""` is the repository root), or, with a `query`, every image under
 * `dir` whose path holds each of its words, likeliest logos first. A folder
 * that only leads to one other folder is collapsed into it, so
 * `src-tauri/icons` is one step. At most `limit` entries, folders first.
 */
export function logoFolderView(
  images: readonly string[],
  dir: string,
  query = "",
  limit = MAX_BROWSE_SHOWN,
): LogoFolderView {
  const cap = Math.max(0, limit);
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length > 0) {
    const prefix = underDir(dir);
    const matches = images.filter((path) => {
      if (!path.startsWith(prefix)) return false;
      const rest = path.slice(prefix.length).toLowerCase();
      return words.every((word) => rest.includes(word));
    });
    const rank = (path: string) => {
      const name = path.slice(path.lastIndexOf("/") + 1).toLowerCase();
      const inName = words.every((word) => name.includes(word)) ? 1000 : 0;
      return inName + (scoreLogoPath(path, { anyImage: true }) ?? -500);
    };
    const ranked = matches
      .map((path) => ({ path, rank: rank(path) }))
      .sort((a, b) => b.rank - a.rank || pathCollator.compare(a.path, b.path))
      .map((entry) => entry.path);
    return { folders: [], files: ranked.slice(0, cap), total: ranked.length };
  }

  const { folders, files } = children(images, dir);
  const prefix = underDir(dir);
  const entries: LogoFolderEntry[] = [...folders.entries()].map(([name, count]) => {
    let path = `${prefix}${name}`;
    let label = name;
    for (let depth = 0; depth < MAX_COLLAPSED_DEPTH; depth++) {
      const inner = children(images, path);
      if (inner.files.length > 0 || inner.folders.size !== 1) break;
      const [only] = inner.folders.keys();
      path = `${path}/${only}`;
      label = `${label}/${only}`;
    }
    return { name: label, path, images: count };
  });
  entries.sort((a, b) => pathCollator.compare(a.name, b.name));
  const keptFolders = entries.slice(0, cap);
  const keptFiles = files.slice(0, Math.max(0, cap - keptFolders.length));
  return { folders: keptFolders, files: keptFiles, total: entries.length + files.length };
}

/** The folder a path sits in, `""` for the root. */
export function logoDirOf(path: string): string {
  const slash = path.lastIndexOf("/");
  return slash < 0 ? "" : path.slice(0, slash);
}

/**
 * The folder one step up from `dir` as the browser shows folders: a folder
 * that only leads on to one other folder is passed over, since the view
 * collapses it, so going up from `src-tauri/icons` lands at the root.
 */
export function logoParentDir(images: readonly string[], dir: string): string {
  let parent = logoDirOf(dir);
  while (parent !== "") {
    const inner = children(images, parent);
    if (inner.files.length > 0 || inner.folders.size !== 1) break;
    parent = logoDirOf(parent);
  }
  return parent;
}

/** The blob fields a logo is built from (a subset of `FileBlob`). */
export interface LogoBlob {
  base64?: string | null;
  text?: string | null;
}

const BASE64 = /^[A-Za-z0-9+/]*={0,2}$/;

function leadingBytes(base64: string, count: number): number[] | null {
  try {
    const chunk = base64.slice(0, Math.ceil(count / 3) * 4);
    const decoded = atob(chunk);
    return Array.from(decoded.slice(0, count), (ch) => ch.charCodeAt(0));
  } catch {
    return null;
  }
}

/** Whether the first bytes are an image the format claims to be. */
function signatureMatches(ext: string, bytes: readonly number[]): boolean {
  const at = (i: number) => bytes[i] ?? -1;
  const ascii = (from: number, text: string) => [...text].every((ch, i) => at(from + i) === ch.charCodeAt(0));
  switch (ext) {
    case "png":
      return at(0) === 0x89 && ascii(1, "PNG");
    case "jpg":
    case "jpeg":
      return at(0) === 0xff && at(1) === 0xd8 && at(2) === 0xff;
    case "gif":
      return ascii(0, "GIF8");
    case "webp":
      return ascii(0, "RIFF") && ascii(8, "WEBP");
    case "ico":
      return at(0) === 0 && at(1) === 0 && at(2) === 1 && at(3) === 0;
    default:
      return false;
  }
}

function utf8Base64(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(binary);
}

/**
 * A `data:` URL for a logo file, or null when the bytes are not a drawable
 * image of the format the name claims.
 */
export function logoDataUrl(path: string, blob: LogoBlob): string | null {
  const ext = logoExtension(path);
  const mime = logoMime(path);
  if (!ext || !mime) return null;
  if (ext === "svg") {
    // The backend reads SVG as text; a binary read of one is not an SVG.
    const text = typeof blob.text === "string" ? blob.text : null;
    if (text === null) return null;
    if (!/<svg[\s>]/i.test(text.slice(0, 4096))) return null;
    return `data:${mime};base64,${utf8Base64(text)}`;
  }
  const base64 = typeof blob.base64 === "string" ? blob.base64 : null;
  if (!base64) return null;
  if (!BASE64.test(base64)) return null;
  const head = leadingBytes(base64, 12);
  if (!head || !signatureMatches(ext, head)) return null;
  return `data:${mime};base64,${base64}`;
}

/**
 * One or two letters for a repository with no logo: the first letters of its
 * first two words, where a word starts after `-`, `_`, `.`, a space, or at a
 * capital in camelCase (`GitPulse` → `GP`, `dev-council` → `DC`), else the
 * first letter alone (`gusset` → `G`).
 */
export function logoInitials(name: string): string {
  const cleaned = name.normalize("NFC").replace(/\.git$/i, "").trim();
  const words = cleaned
    .replace(/([\p{Ll}\d])(\p{Lu})/gu, "$1 $2")
    .split(/[\s\-_.]+/u)
    .map((word) => word.replace(/[^\p{L}\p{N}]/gu, ""))
    .filter(Boolean);
  if (words.length === 0) return "?";
  const first = [...words[0]][0] ?? "?";
  if (words.length === 1) return first.toLocaleUpperCase();
  const second = [...words[1]][0] ?? "";
  return `${first}${second}`.toLocaleUpperCase();
}

/** FNV-1a: a stable, well-spread hash of a short key. */
function stableHash(input: string): number {
  let hash = 0x811c9dc5;
  for (let i = 0; i < input.length; i++) {
    hash ^= input.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  return hash >>> 0;
}

/**
 * The monogram's color: the tab's own color when it has one, so the tile
 * agrees with the tab's color bar, else one picked from the same closed
 * palette by the repository's name, so two repositories rarely share a tile.
 */
export function monogramColor(name: string, own: TabColor | null | undefined): TabColor {
  if (own) return own;
  return TAB_COLORS[stableHash(name.normalize("NFC").toLowerCase()) % TAB_COLORS.length];
}
