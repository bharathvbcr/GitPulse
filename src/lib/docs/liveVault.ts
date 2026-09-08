/** Paced document rebuilds on the watcher path. */
import { docsRefresh } from "./client";
import { diagnostics } from "../diagnostics/diagnostics";
import { createPacedQueue, type BackgroundScope } from "../async/pacedQueue";

const queue = createPacedQueue({
  debounceMs: 200,
  maxWaitMs: 1_000,
  restMs: 1_000,
  capacity: 64,
  run: async (repoPath) => { await docsRefresh(repoPath, { background: true }); },
  onError: (repoPath, error) => {
    const detail = error instanceof Error ? error.message : String(error);
    diagnostics.warn("docs-refresh", `${repoPath}: ${detail}`);
  },
  onOverflow: () => diagnostics.warn("docs-refresh", "Background document refresh queue is full (64 repositories); additional repositories were not refreshed."),
});

export function onDocsRepoChanged(repoPath: string): void {
  queue.enqueue(repoPath);
}

export function resetDocsVaultRefresh(): void {
  queue.reset();
}

export function setDocsVaultRefreshScope(scope: BackgroundScope | null): void {
  queue.setScope(scope);
}
