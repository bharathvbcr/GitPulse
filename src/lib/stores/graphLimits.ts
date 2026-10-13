/**
 * History page sizing for the commit graph.
 *
 * The first page loads fast on a monorepo-scale history; deeper pages are
 * opt-in via "load more" rather than one giant walk that an agent's 100k-commit
 * repository would turn into a freeze.
 */

export const DEFAULT_MAX_COMMITS = 5_000;

/**
 * Repositories whose walked history stays in memory at once, the visible one
 * included. A full first page is about 3 MB of row objects (measured with
 * `scripts/memory-footprint.mjs`: 600 to 760 bytes a row across runs on the two
 * committed real-repository fixtures, 5,000 rows), and "load more" raises one tab to as many as MAX_LOAD_COMMITS, so
 * the cost grew with every tab visited for as long as it stayed open.
 * Switching back to a repository past this recency window shows the loading
 * state and reloads, instead of rendering the rows held since it was last seen.
 */
export const GRAPH_CACHE_LIMIT = 6;
export const MAX_LOAD_COMMITS = 100_000;
export const LOAD_MORE_STEP = 10_000;

/** Next limit after `current`, or null when the ceiling is reached. */
export function nextLoadLimit(current: number): number | null {
  if (current >= MAX_LOAD_COMMITS) return null;
  return Math.min(current + LOAD_MORE_STEP, MAX_LOAD_COMMITS);
}
