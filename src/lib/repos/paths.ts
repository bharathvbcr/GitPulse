import { disambiguateSuffixes } from "../codeintel/labelDisambiguation";

export interface PathIdentityOptions {
  caseInsensitive: boolean;
}

const CONTROL = /[\u0000-\u001F\u007F]/;

export function isCaseInsensitiveFs(): boolean {
  if (typeof navigator !== "undefined") {
    const platform = navigator.platform || "";
    const ua = navigator.userAgent || "";
    if (/Mac|Macintosh|Win/i.test(platform) || /Mac OS X|Windows/i.test(ua)) {
      return true;
    }
    if (/Linux/i.test(platform) || /Linux/i.test(ua)) {
      return false;
    }
  }
  if (typeof process !== "undefined" && typeof process.platform === "string") {
    return process.platform === "darwin" || process.platform === "win32";
  }
  return false;
}

export function normalizeRepoPath(raw: string): string | null {
  if (typeof raw !== "string") return null;
  let value = raw.normalize("NFC").trim();
  if (!value || CONTROL.test(value)) return null;
  value = value.replace(/\\/g, "/");
  if (value.startsWith("//")) {
    const rest = value.slice(2).replace(/\/{2,}/g, "/");
    value = `//${rest}`;
  } else {
    value = value.replace(/\/{2,}/g, "/");
  }
  if (value.length > 1 && value.endsWith("/")) {
    value = value.replace(/\/+$/, "");
  }
  if (!value || value === "/" || value === "//") return null;
  // The null set is the input's alone (empty, control characters, a bare root); a path that folds to nothing is "." or its root, as before.
  return collapseDotSegments(value);
}

const DRIVE = /^[A-Za-z]:$/;

/**
 * Drops `.` segments and folds `name/..` pairs, so two spellings of one path
 * are one identity. The collapse is lexical (a symlinked directory before a
 * `..` is not followed). A `..` never climbs above a root: a drive letter, or
 * the server and share of a UNC path (its first two names), or `/`. A `..`
 * with nothing to fold stays in a relative path and is dropped at a root.
 */
function collapseDotSegments(value: string): string {
  const unc = value.startsWith("//");
  const absolute = !unc && value.startsWith("/");
  const lead = unc ? "//" : absolute ? "/" : "";
  const segments = value.slice(lead.length).split("/");
  // How many leading names are the root itself.
  const rootNames = unc ? 2 : !lead && DRIVE.test(segments[0] ?? "") ? 1 : 0;
  const kept: string[] = [];
  for (const segment of segments) {
    if (segment === "." || segment === "") continue;
    if (segment === "..") {
      if (kept.length > rootNames && kept[kept.length - 1] !== "..") kept.pop();
      else if (!lead && rootNames === 0) kept.push("..");
      continue;
    }
    kept.push(segment);
  }
  const joined = kept.join("/");
  return joined ? `${lead}${joined}` : lead || ".";
}

export function identityKey(path: string, options: PathIdentityOptions): string {
  const normalized = normalizeRepoPath(path);
  if (!normalized) return "";
  return options.caseInsensitive ? normalized.toLowerCase() : normalized;
}

export function sameRepo(a: string, b: string, options: PathIdentityOptions): boolean {
  const left = identityKey(a, options);
  const right = identityKey(b, options);
  return Boolean(left) && left === right;
}

export function isPathAmong(path: string, list: readonly string[], options: PathIdentityOptions): boolean {
  return list.some((item) => sameRepo(item, path, options));
}

export function pathSegments(path: string): string[] {
  const normalized = normalizeRepoPath(path);
  if (!normalized) return [];
  return normalized.split("/").filter((part) => part.length > 0 && part !== ".");
}

export function displayName(path: string): string {
  const parts = pathSegments(path);
  if (parts.length === 0) {
    const normalized = normalizeRepoPath(path);
    return normalized || "repo";
  }
  return parts[parts.length - 1];
}

/**
 * Repository labels: the leaf, widened with parent segments only where two
 * paths share a leaf. The algorithm lives in codeintel (one owner, shared with
 * the code-graph model); this supplies the path-identity rules.
 */
export function disambiguateLabels(paths: string[]): Map<string, string> {
  return disambiguateSuffixes(paths, pathSegments, displayName);
}
