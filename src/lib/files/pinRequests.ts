import { get, writable } from "svelte/store";

/**
 * A one-shot "keep this file's tab" request, handed from an explorer that does
 * not own the editor tabs to the Explorer page that does.
 *
 * The main sidebar's File Explorer opens a file in Code by selecting it, and a
 * selection opens a preview tab, which the next single click replaces. Its
 * double-click used to do exactly what its single click did, so a file
 * double-clicked there never kept its tab, although the tab itself says
 * "double-click to pin". Pinning is a navigation intent like a reveal, not
 * session state, so it travels the way `revealRequests.ts` does: one pending
 * slot, replaced by a newer request, collected by the viewer it names, and
 * expired if nobody collects it.
 */
export interface PinRequest {
  /** The repository the file belongs to; a viewer of another one ignores it. */
  repo: string;
  /** Repository-relative POSIX path, exactly as the viewer knows the file. */
  path: string;
  /** When the request was made, for expiry. */
  at: number;
}

/** Longest path this will carry, the same bound reveal requests use. */
export const MAX_PIN_PATH = 1024;

/** How long a request stays collectable: the click, a view switch, a mount. */
export const PIN_TTL_MS = 30_000;

const pending = writable<PinRequest | null>(null);

/** Subscribe-only view, so a mounted viewer reacts to a pin of the file it already shows. */
export const pinRequest = { subscribe: pending.subscribe };

/** Records a request to keep `path`'s tab. False when the request is unusable. */
export function requestPin(repo: string, path: string, now: number = Date.now()): boolean {
  if (!repo || !path || path.length > MAX_PIN_PATH) return false;
  pending.set({ repo, path, at: now });
  return true;
}

/**
 * Takes the pending request if it names `repo` and `path` and has not
 * expired. An expired request is dropped as well, so it can never pin the
 * file when it is next opened for an unrelated reason.
 */
export function consumePin(repo: string, path: string, now: number = Date.now()): PinRequest | null {
  const current = get(pending);
  if (!current) return null;
  if (now - current.at > PIN_TTL_MS) {
    pending.set(null);
    return null;
  }
  if (current.repo !== repo || current.path !== path) return null;
  pending.set(null);
  return current;
}

/** Drops any pending request. For teardown and tests. */
export function clearPin(): void {
  pending.set(null);
}
