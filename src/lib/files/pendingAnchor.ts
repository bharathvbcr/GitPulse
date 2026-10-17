/**
 * The `#section` of a cross-file Markdown link, carried across the file
 * switch it causes.
 *
 * `repoStore.selectFilePath` records which file is open and nothing else, and
 * the viewer that renders it mounts later, so the fragment has nowhere to
 * ride. A single pending slot, keyed by repository and path and expiring on
 * its own, lets the viewer that renders exactly that file scroll to it once:
 * a request for a file that never shows a document (an image, a binary) must
 * not scroll the next note the user happens to open.
 */

/** Long enough for the file read and render; short enough to be gone after. */
export const PENDING_ANCHOR_TTL_MS = 5000;

interface Pending {
  repoPath: string;
  path: string;
  fragment: string;
  expiresAt: number;
}

let pending: Pending | null = null;

export function requestAnchor(
  repoPath: string,
  path: string,
  fragment: string | null,
  now: number = Date.now(),
): void {
  pending = fragment ? { repoPath, path, fragment, expiresAt: now + PENDING_ANCHOR_TTL_MS } : null;
}

/** The fragment waiting for this file, consumed; otherwise `null`. */
export function takeAnchor(repoPath: string, path: string, now: number = Date.now()): string | null {
  const wanted = pending;
  if (!wanted || wanted.repoPath !== repoPath || wanted.path !== path) return null;
  pending = null;
  return now <= wanted.expiresAt ? wanted.fragment : null;
}

/**
 * Opens `path` and remembers where in it to land. The anchor is recorded
 * first, so a viewer that renders synchronously on selection still finds it.
 */
export function openFileAtAnchor(
  select: (path: string) => void,
  repoPath: string,
  path: string,
  fragment: string | null,
): void {
  requestAnchor(repoPath, path, fragment);
  select(path);
}
