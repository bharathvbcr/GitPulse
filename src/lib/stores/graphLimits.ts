/**
 * History page sizing for the commit graph.
 *
 * The first page loads fast on a monorepo-scale history; deeper pages are
 * opt-in via "load more" rather than one giant walk that an agent's 100k-commit
 * repository would turn into a freeze.
 */

export const DEFAULT_MAX_COMMITS = 5_000;
export const MAX_LOAD_COMMITS = 100_000;
export const LOAD_MORE_STEP = 10_000;

/** Next limit after `current`, or null when the ceiling is reached. */
export function nextLoadLimit(current: number): number | null {
  if (current >= MAX_LOAD_COMMITS) return null;
  return Math.min(current + LOAD_MORE_STEP, MAX_LOAD_COMMITS);
}
