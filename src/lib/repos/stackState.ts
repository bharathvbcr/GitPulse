/**
 * Session state of the strip's worktree stacks.
 *
 * Two facts live here because both the strip and the store's tab cycling need
 * them, and they must agree: which stacks the reader has unfolded, and which
 * checkout of each repository was last on screen (what a folded stack shows
 * while its repository is not the active one). Neither is persisted — a family
 * is derived from the backend on every resolve, so a remembered key could name
 * a repository that no longer resolves the same way; a fresh launch starts with
 * every stack folded.
 *
 * Both are bounded so a long session that opens and closes many worktrees
 * cannot grow them without limit; the oldest entry goes first.
 */
import { writable, type Readable } from "svelte/store";

export const MAX_REMEMBERED_STACKS = 64;

const expanded = writable<ReadonlySet<string>>(new Set());

/** Stack keys (see `stackKeyFor`) the reader has unfolded. */
export const expandedStacks: Readable<ReadonlySet<string>> = { subscribe: expanded.subscribe };

export function setStackExpanded(key: string, open: boolean): void {
  if (typeof key !== "string" || key.length === 0) return;
  expanded.update((current) => {
    if (current.has(key) === open) return current;
    const next = new Set(current);
    if (open) {
      next.add(key);
      while (next.size > MAX_REMEMBERED_STACKS) {
        const oldest = next.values().next().value;
        if (oldest === undefined) break;
        next.delete(oldest);
      }
    } else {
      next.delete(key);
    }
    return next;
  });
}

export function toggleStackExpanded(key: string, current: ReadonlySet<string>): void {
  setStackExpanded(key, !current.has(key));
}

const lastUsed = new Map<string, string>();

/**
 * Records which checkout of a family is on screen. A plain map, not a store:
 * it only changes when the active tab does, and that already republishes the
 * tab list, so every reader recomputes after it is written.
 */
export function noteActiveCheckout(family: string | null | undefined, tabId: string | null | undefined): void {
  if (!family || !tabId) return;
  if (lastUsed.get(family) === tabId) return;
  lastUsed.delete(family);
  lastUsed.set(family, tabId);
  while (lastUsed.size > MAX_REMEMBERED_STACKS) {
    const oldest = lastUsed.keys().next().value;
    if (oldest === undefined) break;
    lastUsed.delete(oldest);
  }
}

/** Family key → the checkout last on screen. */
export function lastUsedCheckouts(): ReadonlyMap<string, string> {
  return lastUsed;
}

/** Test seam: forget everything. */
export function resetStackState(): void {
  expanded.set(new Set());
  lastUsed.clear();
}
