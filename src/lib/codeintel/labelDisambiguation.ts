/**
 * Shortest-unique-path-suffix labels (the VS Code tab rule).
 *
 * Pure: no Svelte, store, DOM or app imports. The one owner of the widening
 * algorithm; `repos/paths.ts` (repository tabs, diff rail) and the code-graph
 * model both delegate here with their own notion of a path's segments.
 */

/** Last `depth` segments joined with "/", capped at the whole path. */
function suffixOf(parts: readonly string[], depth: number): string {
  const width = Math.min(Math.max(depth, 1), parts.length);
  return parts.slice(parts.length - width).join("/");
}

/**
 * Label every item with the shortest trailing run of its segments that no
 * other item shares. Items whose last segment is already unique keep it alone.
 *
 * Widening collision resolution: `parent/name` still collides when two items
 * share both leaf and parent, so segments keep being prepended until every
 * member of the group is unique. Input order decides who claims a label first.
 * Items that run out of segments while still colliding (identical paths) are
 * left out of the map; callers fall back to `nameOf`.
 *
 * @param items    The strings to label (a path, or any stable key).
 * @param segmentsOf Splits an item into path segments, leaf last.
 * @param nameOf   The bare label (depth 1) of an item; groups form around it.
 */
export function disambiguateSuffixes(
  items: readonly string[],
  segmentsOf: (item: string) => readonly string[],
  nameOf: (item: string) => string,
): Map<string, string> {
  const labels = new Map<string, string>();
  const groups = new Map<string, string[]>();
  for (const item of items) {
    const name = nameOf(item);
    const bucket = groups.get(name) ?? [];
    bucket.push(item);
    groups.set(name, bucket);
  }
  for (const [name, group] of groups) {
    if (group.length === 1) {
      labels.set(group[0], name);
      continue;
    }
    const claimed = new Set<string>();
    let pending = [...group];
    for (let depth = 2; pending.length > 0; depth += 1) {
      const byLabel = new Map<string, string[]>();
      for (const item of pending) {
        const label = suffixOf(segmentsOf(item), depth);
        const bucket = byLabel.get(label) ?? [];
        bucket.push(item);
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
      if (pending.length > 0 && pending.every((item) => segmentsOf(item).length <= depth)) {
        // Every remaining item has run out of segments: they normalize to the
        // same string, so no deepening can separate them.
        break;
      }
    }
  }
  return labels;
}

/** Plain "/"-separated segments, for payload paths that are already repo-relative. */
export function splitSegments(path: string): string[] {
  return path.replace(/\\/g, "/").split("/").filter((part) => part.length > 0 && part !== ".");
}
