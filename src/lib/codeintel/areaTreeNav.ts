/**
 * Behaviour of the Map navigator's area tree that does not need a DOM:
 * which rows are visible under a filter and a collapsed set, what a key does
 * to focus / expansion / selection, and how a neighbor name or a subsystem
 * resolves to a row or a graph node. Pure so the panel stays a thin renderer
 * and the keyboard contract is testable without mounting anything.
 */

import { normalizeArea, type AreaTreeRow } from "./areaTree";

/** The collapsed set while a filter is active: nothing is folded away. */
export const NO_KEYS: ReadonlySet<string> = new Set();

function keyOf(area: string): string {
  return `area:${normalizeArea(area)}`;
}

/** Keys of every ancestor of `key`, nearest first. */
export function ancestorKeys(rows: readonly AreaTreeRow[], key: string): string[] {
  const byKey = new Map(rows.map((row) => [row.key, row]));
  const out: string[] = [];
  let parent = byKey.get(key)?.parentKey ?? null;
  while (parent !== null) {
    out.push(parent);
    parent = byKey.get(parent)?.parentKey ?? null;
  }
  return out;
}

/**
 * Rows to draw. A non-empty filter keeps rows whose area contains it
 * (case-insensitive) plus their ancestors, and ignores `collapsed`: a match
 * folded away would read as "no match". Otherwise a row hides under any
 * collapsed ancestor.
 */
export function visibleAreaRows(
  rows: readonly AreaTreeRow[],
  collapsed: ReadonlySet<string>,
  filter: string,
): AreaTreeRow[] {
  const needle = filter.trim().toLowerCase();
  if (needle === "") {
    const hidden = new Set<string>();
    const out: AreaTreeRow[] = [];
    for (const row of rows) {
      // Rows arrive parent-first, so a parent's verdict is already known.
      if (row.parentKey !== null && (collapsed.has(row.parentKey) || hidden.has(row.parentKey))) {
        hidden.add(row.key);
        continue;
      }
      out.push(row);
    }
    return out;
  }
  const keep = new Set<string>();
  for (const row of rows) {
    if (!row.area.toLowerCase().includes(needle)) continue;
    keep.add(row.key);
    for (const ancestor of ancestorKeys(rows, row.key)) keep.add(ancestor);
  }
  return rows.filter((row) => keep.has(row.key));
}

export interface TreeKeyOutcome {
  /** Row that should hold the roving tabindex (and DOM focus). */
  focusKey: string;
  /** The new collapsed set, when the key expanded or collapsed a row. */
  collapsed?: ReadonlySet<string>;
  /** Row to select, when the key activated an area row. */
  select?: string;
}

/**
 * WAI-ARIA tree keys over the visible rows: Up/Down, Home/End, Left (collapse,
 * else parent), Right (expand, else first child), Enter/Space (select an area
 * row; toggle a group row). `null` means the key is not the tree's.
 */
export function treeKeyOutcome(
  key: string,
  visible: readonly AreaTreeRow[],
  focusKey: string,
  collapsed: ReadonlySet<string>,
): TreeKeyOutcome | null {
  const at = visible.findIndex((row) => row.key === focusKey);
  if (at < 0) return null;
  const row = visible[at];
  const open = row.hasChildren && !collapsed.has(row.key);
  const toggled = (): ReadonlySet<string> => {
    const next = new Set(collapsed);
    if (!next.delete(row.key)) next.add(row.key);
    return next;
  };
  switch (key) {
    case "ArrowDown":
      return { focusKey: visible[Math.min(at + 1, visible.length - 1)].key };
    case "ArrowUp":
      return { focusKey: visible[Math.max(at - 1, 0)].key };
    case "Home":
      return { focusKey: visible[0].key };
    case "End":
      return { focusKey: visible[visible.length - 1].key };
    case "ArrowLeft": {
      if (open) return { focusKey: row.key, collapsed: toggled() };
      const parent = visible.find((candidate) => candidate.key === row.parentKey);
      return { focusKey: parent ? parent.key : row.key };
    }
    case "ArrowRight": {
      if (row.hasChildren && collapsed.has(row.key)) return { focusKey: row.key, collapsed: toggled() };
      const next = visible[at + 1];
      return { focusKey: next && next.parentKey === row.key ? next.key : row.key };
    }
    case "Enter":
    case " ":
      if (row.subsystemIndex !== null) return { focusKey: row.key, select: row.key };
      return row.hasChildren ? { focusKey: row.key, collapsed: toggled() } : { focusKey: row.key };
    default:
      return null;
  }
}

/** Selectable row for a neighbor name, matched by normalized area; null when the map has no such area. */
export function neighborRowKey(rows: readonly AreaTreeRow[], neighbor: string): string | null {
  const key = keyOf(neighbor);
  return rows.some((row) => row.subsystemIndex !== null && row.key === key) ? key : null;
}

/**
 * The id the subsystem map payload gives a subsystem's graph node: the
 * producer's area with backslashes turned into slashes (`build_preview_payload`
 * in devmap-query `map_preview.rs`), not the fully normalized area.
 */
export function subsystemNodeId(area: string): string {
  return area.replace(/\\/g, "/");
}
