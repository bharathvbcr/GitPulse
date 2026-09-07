/**
 * Debounced doc-vault refresh on the same watcher path as the live index.
 */

import { docsRefresh } from "./client";

const DEBOUNCE_MS = 200;
const timers = new Map<string, ReturnType<typeof setTimeout>>();
const inflight = new Set<string>();

export function onDocsRepoChanged(repoPath: string): void {
  const existing = timers.get(repoPath);
  if (existing) clearTimeout(existing);
  timers.set(
    repoPath,
    setTimeout(() => {
      timers.delete(repoPath);
      void run(repoPath);
    }, DEBOUNCE_MS),
  );
}

async function run(repoPath: string): Promise<void> {
  if (inflight.has(repoPath)) return;
  inflight.add(repoPath);
  try {
    await docsRefresh(repoPath);
  } catch {
    // Soft: a vault rebuild failure must not break the watcher path.
  } finally {
    inflight.delete(repoPath);
  }
}

export function resetDocsVaultRefresh(): void {
  for (const timer of timers.values()) clearTimeout(timer);
  timers.clear();
  inflight.clear();
}
