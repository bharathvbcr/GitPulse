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
  const collapsed = collapseDotSegments(value);
  return collapsed === "/" || collapsed === "//" || collapsed === "." ? null : collapsed;
}

/**
 * Drops `.` segments and folds `name/..` pairs, so two spellings of one path
 * are one identity. The collapse is lexical (a symlinked directory before a
 * `..` is not followed). A `..` that has nothing to fold stays in a relative
 * path and is dropped at the root of an absolute one, as the OS does.
 */
function collapseDotSegments(value: string): string {
  const unc = value.startsWith("//");
  const absolute = !unc && value.startsWith("/");
  const lead = unc ? "//" : absolute ? "/" : "";
  const kept: string[] = [];
  for (const segment of value.slice(lead.length).split("/")) {
    if (segment === "." || segment === "") continue;
    if (segment === "..") {
      const last = kept[kept.length - 1];
      if (last !== undefined && last !== ".." && !/^[A-Za-z]:$/.test(last)) kept.pop();
      else if (!lead && !/^[A-Za-z]:$/.test(last ?? "")) kept.push("..");
      continue;
    }
    kept.push(segment);
  }
  const joined = kept.join("/");
  return joined ? `${lead}${joined}` : lead ? lead : ".";
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
 * Last `depth` segments joined with "/", capped at the whole path. Depth 1 is
 * the bare display name; every deepening step prepends one more segment.
 */
function suffixLabel(path: string, depth: number): string {
  const parts = pathSegments(path);
  const width = Math.min(Math.max(depth, 1), parts.length);
  return parts.slice(parts.length - width).join("/");
}

export function disambiguateLabels(paths: string[]): Map<string, string> {
  const labels = new Map<string, string>();
  const groups = new Map<string, string[]>();
  for (const path of paths) {
    const name = displayName(path);
    const bucket = groups.get(name) ?? [];
    bucket.push(path);
    groups.set(name, bucket);
  }
  for (const [name, group] of groups) {
    if (group.length === 1) {
      labels.set(group[0], name);
      continue;
    }
    // Widening collision resolution: `parent/name` still collides when two
    // repos share both leaf and parent (`/a/x/y` vs `/b/x/y`), so keep
    // prepending segments — grandparent, then great-grandparent — until every
    // member's label is unique within the group. Input order decides who
    // claims a label first; identical normalized paths share a label by
    // construction and fall back to the caller's displayName.
    const claimed = new Set<string>();
    let pending = [...group];
    for (let depth = 2; pending.length > 0; depth += 1) {
      const byLabel = new Map<string, string[]>();
      for (const path of pending) {
        const label = suffixLabel(path, depth);
        const bucket = byLabel.get(label) ?? [];
        bucket.push(path);
        byLabel.set(label, bucket);
      }
      const nextPending: string[] = [];
      for (const [label, members] of byLabel) {
        if (members.length === 1 && !claimed.has(label)) {
          labels.set(members[0], label);
          claimed.add(label);
        } else {
          nextPending.push(...members);
        }
      }
      pending = nextPending;
      if (pending.length > 0 && pending.every((path) => pathSegments(path).length <= depth)) {
        // Every remaining path has run out of segments: they normalize to the
        // same string, so no deepening can separate them.
        break;
      }
    }
  }
  return labels;
}
