/**
 * Pure path normalization, containment, and pluralization utilities.
 * Zero DOM, Svelte, or Tauri dependencies.
 */

export interface PathIdentityOptions {
  caseInsensitive: boolean;
}

const CONTROL = /[\u0000-\u001F\u007F]/;
const DRIVE = /^[A-Za-z]:$/;

function collapseDotSegments(value: string): string {
  const unc = value.startsWith("//");
  const absolute = !unc && value.startsWith("/");
  const lead = unc ? "//" : absolute ? "/" : "";
  const segments = value.slice(lead.length).split("/");
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
  return collapseDotSegments(value);
}

export function identityKey(path: string, options: PathIdentityOptions): string {
  const normalized = normalizeRepoPath(path);
  if (!normalized) return "";
  return options.caseInsensitive ? normalized.toLowerCase() : normalized;
}

export function sameRepo(
  a: string,
  b: string,
  options: PathIdentityOptions,
): boolean {
  const left = identityKey(a, options);
  const right = identityKey(b, options);
  return Boolean(left) && left === right;
}

/**
 * The value stored for `path` or its nearest ancestor in a map keyed by `identityKey`.
 */
export function nearestContaining<V>(
  index: ReadonlyMap<string, V>,
  path: string,
  paths: PathIdentityOptions,
): V | undefined {
  let key = identityKey(path, paths);
  while (key) {
    const found = index.get(key);
    if (found !== undefined) return found;
    const cut = key.lastIndexOf("/");
    if (cut <= 0) return undefined;
    key = key.slice(0, cut);
  }
  return undefined;
}

export function plural(
  count: number,
  singular: string,
  pluralForm = `${singular}s`,
): string {
  return `${count} ${count === 1 ? singular : pluralForm}`;
}
