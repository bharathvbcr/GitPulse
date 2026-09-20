import { get, writable } from "svelte/store";

/**
 * A one-shot "open this file *here*" request, handed from whoever names a
 * location to whichever viewer ends up showing that file.
 *
 * Terminal output is full of `src/lib/a.ts:12:5`, and selecting the file is
 * only half of following that reference. The line cannot travel on the
 * repository session, though: that is persisted state describing which file is
 * open, and "scroll to line 12" is a navigation intent that must not survive a
 * restart and must not replay when the file is reopened for another reason.
 *
 * So it travels here instead, following the same shape as
 * `terminal/consoleLaunches.ts` — a bounded pending slot with an explicit
 * `consume`, rather than a flag someone has to remember to clear.
 *
 * Three rules keep a stale request from acting on the wrong thing:
 *  - `consume` only yields a request that names the file asking for it, so a
 *    viewer showing something else cannot swallow it.
 *  - A newer request replaces an older one; there is never a queue to drain.
 *  - A request nobody collected expires, because the alternative is one
 *    firing much later when the same file is opened for an unrelated reason.
 */
export interface RevealRequest {
  /** Repository-relative POSIX path, exactly as the viewer knows the file. */
  path: string;
  /** 1-based line to reveal. */
  line: number;
  /** 1-based column, carried for display; viewers may ignore it. */
  column: number | null;
  /** When the request was made, for expiry. */
  at: number;
}

/** Longest path this will carry, matching the linkifier's own path bound. */
export const MAX_REVEAL_PATH = 1024;

/**
 * Largest line number worth honouring. Well past any file a viewer will
 * render, and small enough that a parsed run of digits cannot arrive as
 * something absurd; viewers clamp to the real line count regardless.
 */
export const MAX_REVEAL_LINE = 10_000_000;

/**
 * How long a request stays collectable. Long enough to survive the file read
 * and the view switch that follow the click, short enough that it cannot be
 * mistaken for a request made by the next thing the user does.
 */
export const REVEAL_TTL_MS = 30_000;

const pending = writable<RevealRequest | null>(null);

/** Subscribe-only view, for anything that wants to observe without consuming. */
export const revealRequest = { subscribe: pending.subscribe };

/**
 * Records where to land in `path`. Returns false when the request is not
 * usable, rather than throwing: the caller is a click handler on untrusted
 * terminal text, and an unusable reference should open the file plainly, not
 * raise.
 */
export function requestReveal(
  path: string,
  line: number | null,
  column: number | null = null,
  now: number = Date.now(),
): boolean {
  if (!path || path.length > MAX_REVEAL_PATH) return false;
  if (line === null) return false;
  if (!Number.isInteger(line) || line < 1 || line > MAX_REVEAL_LINE) return false;
  const usableColumn =
    column !== null && Number.isInteger(column) && column >= 1 ? column : null;
  pending.set({ path, line, column: usableColumn, at: now });
  return true;
}

/**
 * Takes the pending request if it names `path` and has not expired.
 *
 * Clearing on expiry as well as on collection matters: a request left in the
 * slot would otherwise be handed to the next viewer of that file, whenever
 * that happened to be.
 */
export function consumeReveal(path: string, now: number = Date.now()): RevealRequest | null {
  const current = get(pending);
  if (!current) return null;
  if (now - current.at > REVEAL_TTL_MS) {
    pending.set(null);
    return null;
  }
  if (current.path !== path) return null;
  pending.set(null);
  return current;
}

/** Drops any pending request. For teardown and tests. */
export function clearReveal(): void {
  pending.set(null);
}
