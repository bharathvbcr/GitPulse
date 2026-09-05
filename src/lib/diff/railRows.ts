/**
 * Rows for the diff's file rail: what it lists, what each row is called, and
 * how a two-hundred-file commit stays navigable.
 *
 * The rail used to print `displayName` — the basename — for every entry. On
 * this repository's own head commit that produced `marketplace.json` twice,
 * `plugin.json` three times, `SKILL.md` twice and `mod.rs` eight times, in a
 * 224px column with no filter, no grouping and every one of two hundred
 * buttons in the DOM as its own tab stop. A list whose rows are not
 * distinguishable is not a navigation aid; it is two hundred coin flips.
 *
 * Two answers, both here so the component stays a renderer:
 *
 * - **List** keeps git's own order and gives each row the shortest path
 *   suffix that is unique in the list, so `mod.rs` becomes `analyzer/mod.rs`
 *   and `codeintel/mod.rs` only where it has to.
 * - **Tree** groups by directory, reusing `files/fileTree` — the same builder
 *   the Code explorer uses, so the two lists shape paths identically instead
 *   of drifting apart.
 *
 * Both emit one flat row list, because the rail is virtualized and a virtual
 * window needs a flat list with a fixed row height.
 */

import { buildFileTree, flattenFileTree, isValidRelativePath } from "../files/fileTree";
import { disambiguateLabels } from "../repos/paths";
import { entryKey, type RailEntry } from "./fileRail";

export type RailMode = "list" | "tree";

export interface RailFileRow {
  kind: "file";
  /** Stable identity, distinct for the staged and unstaged sides of one path. */
  key: string;
  entry: RailEntry;
  /**
   * Directory prefix shown dimmed before the name. In list mode this is the
   * minimal disambiguating suffix; in tree mode it is empty, because the
   * indentation already says where the file is.
   */
  dir: string;
  name: string;
  /** Full path (with the rename arrow when there is one), for the tooltip. */
  title: string;
  depth: number;
}

export interface RailDirRow {
  kind: "dir";
  key: string;
  path: string;
  name: string;
  depth: number;
  fileCount: number;
  additions: number;
  deletions: number;
}

export type RailRow = RailFileRow | RailDirRow;

export interface RailRowsInput {
  entries: readonly RailEntry[];
  mode: RailMode;
  /** Case-insensitive substring over the full path; empty keeps everything. */
  query: string;
  /** Tree mode only: directories the user has folded away. */
  isCollapsed?: (dirPath: string) => boolean;
}

export interface RailRowsResult {
  rows: RailRow[];
  /** Entries surviving the filter. */
  matched: number;
  /** Entries before filtering, so "3 of 200" can be stated honestly. */
  total: number;
}

export const EMPTY_RAIL_ROWS: RailRowsResult = { rows: [], matched: 0, total: 0 };

/**
 * Final path segment, ignoring a trailing slash.
 *
 * `lastIndexOf("/")` alone answers `""` for `src/dir/`, which renders as a
 * nameless row; git does emit directory-shaped paths for submodule and mode
 * entries.
 */
function basename(path: string): string {
  const parts = path.split("/").filter(Boolean);
  return parts.length > 0 ? parts[parts.length - 1] : path;
}

/**
 * The shortest trailing run of path segments that tells each path apart,
 * as the DIRECTORY part of that suffix.
 *
 * The widening-suffix algorithm already exists — `repos/paths` uses it to
 * label repository tabs, which is the same question asked of a different kind
 * of path — so this delegates rather than growing a second copy that would
 * drift. What is diff-specific is the shape of the answer: the rail always
 * renders the basename, so it wants only what precedes it, and a unique
 * basename maps to `""` and adds no chrome to the common case.
 *
 * Paths that are genuinely identical (the staged and unstaged sides of one
 * file) are collapsed before labelling: they share a row label and are told
 * apart by the staged badge, not by a fabricated difference.
 */
export function disambiguatePaths(paths: readonly string[]): Map<string, string> {
  const unique = [...new Set(paths)];
  const labels = disambiguateLabels(unique);
  const result = new Map<string, string>();
  for (const path of unique) {
    const label = labels.get(path);
    // No label means the shared algorithm ran out of segments — two inputs
    // that normalize to the same string, or a path it could not normalize at
    // all. Falling back to the path's own directory keeps the row honest
    // rather than borrowing the repo-tab "repo" placeholder.
    const source = label ?? path;
    const cut = source.lastIndexOf("/");
    result.set(path, cut < 0 ? "" : source.slice(0, cut));
  }
  return result;
}

/** Case-insensitive match on the full path, its basename, or a rename source. */
export function entryMatchesQuery(entry: RailEntry, needle: string): boolean {
  if (!needle) return true;
  if (entry.path.toLowerCase().includes(needle)) return true;
  return !!entry.oldPath && entry.oldPath.toLowerCase().includes(needle);
}

function rowTitle(entry: RailEntry): string {
  return entry.oldPath && entry.oldPath !== entry.path
    ? `${entry.oldPath} → ${entry.path}`
    : entry.path;
}

/** Display name, keeping the rename arrow the old rail showed. */
function rowName(entry: RailEntry): string {
  if (entry.oldPath && entry.oldPath !== entry.path) {
    return `${basename(entry.oldPath)} → ${basename(entry.path)}`;
  }
  return basename(entry.path);
}

/**
 * Row keys, guaranteed distinct.
 *
 * `entryKey` is path plus staged side, which is unique for every list git can
 * produce — but the rail renders a keyed each-block, and Svelte throws on a
 * duplicate key rather than drawing one row fewer. A malformed status list
 * would take the whole pane down, so the second claimant on a key gets a
 * suffix instead.
 */
function uniqueKeys(): (base: string) => string {
  const seen = new Map<string, number>();
  return (base) => {
    const count = seen.get(base) ?? 0;
    seen.set(base, count + 1);
    return count === 0 ? base : `${base}#${count}`;
  };
}

function buildListRows(entries: readonly RailEntry[]): RailRow[] {
  const dirs = disambiguatePaths(entries.map((entry) => entry.path));
  const key = uniqueKeys();
  return entries.map((entry) => ({
    kind: "file" as const,
    key: key(entryKey(entry)),
    entry,
    dir: dirs.get(entry.path) ?? "",
    name: rowName(entry),
    title: rowTitle(entry),
    depth: 0,
  }));
}

function buildTreeRows(
  entries: readonly RailEntry[],
  isCollapsed: (dirPath: string) => boolean,
): RailRow[] {
  // Entries whose path the shared tree builder rejects (absolute, `..`, a
  // Windows drive) would vanish from a tree silently. They keep their own
  // flat rows at the root so the tree can never show fewer files than the
  // list does.
  const byPath = new Map<string, RailEntry[]>();
  const rejected: RailEntry[] = [];
  for (const entry of entries) {
    if (!isValidRelativePath(entry.path)) {
      rejected.push(entry);
      continue;
    }
    const bucket = byPath.get(entry.path);
    if (bucket) bucket.push(entry);
    else byPath.set(entry.path, [entry]);
  }

  const stats = new Map<string, { files: number; additions: number; deletions: number }>();
  for (const [path, group] of byPath) {
    const parts = path.split("/");
    for (let i = 0; i < parts.length - 1; i += 1) {
      const dirPath = parts.slice(0, i + 1).join("/");
      let stat = stats.get(dirPath);
      if (!stat) {
        stat = { files: 0, additions: 0, deletions: 0 };
        stats.set(dirPath, stat);
      }
      for (const entry of group) {
        stat.files += 1;
        stat.additions += entry.additions;
        stat.deletions += entry.deletions;
      }
    }
  }

  const tree = buildFileTree([...byPath.keys()]);
  const flat = flattenFileTree(tree, isCollapsed);
  const rows: RailRow[] = [];
  const key = uniqueKeys();
  for (const row of flat) {
    if (row.kind === "dir") {
      const stat = stats.get(row.path);
      rows.push({
        kind: "dir",
        key: row.key,
        path: row.path,
        name: row.name,
        depth: row.depth,
        fileCount: stat?.files ?? 0,
        additions: stat?.additions ?? 0,
        deletions: stat?.deletions ?? 0,
      });
      continue;
    }
    for (const entry of byPath.get(row.path) ?? []) {
      rows.push({
        kind: "file",
        key: key(entryKey(entry)),
        entry,
        dir: "",
        name: rowName(entry),
        title: rowTitle(entry),
        depth: row.depth,
      });
    }
  }
  for (const entry of rejected) {
    rows.push({
      kind: "file",
      key: key(entryKey(entry)),
      entry,
      dir: "",
      name: rowName(entry),
      title: rowTitle(entry),
      depth: 0,
    });
  }
  return rows;
}

/**
 * Builds the rail's rows for the current mode and filter.
 *
 * The filter runs before the mode does, so tree mode folds only what survived
 * — a filtered tree that still shows every empty directory would answer a
 * search with the shape of the repository rather than with its matches.
 */
export function buildRailRows(input: RailRowsInput): RailRowsResult {
  const total = input.entries.length;
  const needle = input.query.trim().toLowerCase();
  const entries = needle
    ? input.entries.filter((entry) => entryMatchesQuery(entry, needle))
    : input.entries;
  if (entries.length === 0) return { rows: [], matched: 0, total };
  const rows =
    input.mode === "tree"
      ? buildTreeRows(entries, input.isCollapsed ?? (() => false))
      : buildListRows(entries);
  return { rows, matched: entries.length, total };
}

/** Index of the row for the file on screen, or -1 when it is filtered out. */
export function activeRowIndex(
  rows: readonly RailRow[],
  currentPath: string | null,
  currentIsStaged: boolean,
  matchStaged: boolean,
): number {
  if (!currentPath) return -1;
  for (let i = 0; i < rows.length; i += 1) {
    const row = rows[i];
    if (row.kind !== "file") continue;
    if (row.entry.path !== currentPath) continue;
    if (matchStaged && row.entry.isStaged !== currentIsStaged) continue;
    return i;
  }
  return -1;
}

/** `showing 12 of 200` while a filter is on; empty when it is not. */
export function filterNote(result: RailRowsResult, query: string): string {
  if (!query.trim()) return "";
  if (result.matched === 0) return `no files match “${query.trim()}”`;
  return `${result.matched} of ${result.total} files`;
}
