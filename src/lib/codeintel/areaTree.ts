/**
 * Fold the flat `subsystems` list into a navigable folder tree.
 *
 * Pure: no Svelte, store or DOM. The Map navigator used to render areas flat,
 * so nested areas looked like duplicates, basenames collided and a producer that
 * emitted one area twice collapsed under selection-by-string. The tree fixes
 * all three: labels are relative to the nearest enclosing row, shared prefixes
 * that are not areas become non-selectable group rows, and exact duplicates
 * merge into the first occurrence.
 */

import type { RepoMapSubsystem } from "./types";

export interface AreaTreeRow {
  /** Unique, stable key for `{#each}`. */
  key: string;
  /** Selection value: the producer's own `area` string, or the group's folder path. */
  area: string;
  /** Path relative to the parent row; a top-level row shows its full path. */
  label: string;
  depth: number;
  /** Index of the first subsystem with this area; null for a group row. */
  subsystemIndex: number | null;
  /** How many subsystems carried this exact area (0 for a group row). */
  duplicateCount: number;
  parentKey: string | null;
  hasChildren: boolean;
}

interface TrieNode {
  segment: string;
  path: string;
  children: Map<string, TrieNode>;
  /** First subsystem index when this folder is itself an area. */
  index: number | null;
  count: number;
}

/** Repo-relative, forward-slash, no "./" or trailing "/"; "" is the repo root. */
export function normalizeArea(area: string): string {
  const parts = area.trim().replace(/\\/g, "/").split("/").filter((p) => p !== "" && p !== ".");
  return parts.join("/");
}

function compare(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

function newNode(segment: string, path: string): TrieNode {
  return { segment, path, children: new Map(), index: null, count: 0 };
}

export function buildAreaTree(subsystems: readonly RepoMapSubsystem[]): AreaTreeRow[] {
  const root = newNode("", "");
  subsystems.forEach((sub, index) => {
    let node = root;
    for (const segment of normalizeArea(sub.area).split("/").filter(Boolean)) {
      let next = node.children.get(segment);
      if (!next) {
        next = newNode(segment, node.path ? `${node.path}/${segment}` : segment);
        node.children.set(segment, next);
      }
      node = next;
    }
    node.index ??= index;
    node.count += 1;
  });

  const rows: AreaTreeRow[] = [];
  const parentsWithChildren = new Set<string>();

  const emitChildren = (node: TrieNode, parentKey: string | null, depth: number): void => {
    const entries = [...node.children.values()].map((child) => {
      // VS Code compact folders: a non-area folder with one child joins it.
      let tail = child;
      const segments = [child.segment];
      while (tail.index === null && tail.children.size === 1) {
        const [only] = tail.children.values();
        tail = only;
        segments.push(tail.segment);
      }
      return { tail, label: segments.join("/") };
    });
    entries.sort((a, b) => compare(a.label, b.label) || compare(a.tail.path, b.tail.path));
    for (const { tail, label } of entries) {
      const isArea = tail.index !== null;
      const key = isArea ? `area:${tail.path}` : `group:${tail.path}`;
      if (parentKey !== null) parentsWithChildren.add(parentKey);
      rows.push({
        key,
        area: isArea ? subsystems[tail.index as number].area : tail.path,
        label,
        depth,
        subsystemIndex: tail.index,
        duplicateCount: tail.count,
        parentKey,
        hasChildren: false,
      });
      emitChildren(tail, key, depth + 1);
    }
  };

  if (root.index !== null) {
    const key = "area:";
    rows.push({
      key,
      area: subsystems[root.index].area,
      label: ".",
      depth: 0,
      subsystemIndex: root.index,
      duplicateCount: root.count,
      parentKey: null,
      hasChildren: false,
    });
    emitChildren(root, key, 1);
  } else {
    emitChildren(root, null, 0);
  }
  for (const row of rows) row.hasChildren = parentsWithChildren.has(row.key);
  return rows;
}

/**
 * Producer summaries begin with their own area ("src-tauri/src: 479 files…").
 * The navigator already shows the area, so drop that lead-in; unchanged when
 * the summary does not start with it.
 */
export function summaryWithoutArea(sub: Pick<RepoMapSubsystem, "area" | "summary">): string {
  const summary = sub.summary;
  for (const area of new Set([sub.area, normalizeArea(sub.area)])) {
    if (area === "" || !summary.startsWith(area)) continue;
    const rest = summary.slice(area.length).match(/^\s*(?::|—)\s+/);
    if (rest) return summary.slice(area.length + rest[0].length);
  }
  return summary;
}
