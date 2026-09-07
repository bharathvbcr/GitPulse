/**
 * Debounced sync of open repo tabs into the active repo's workspace.json.
 *
 * Failures are swallowed after logging — a registry miss must never block
 * opening or closing a tab. Search surfaces read whatever was last written.
 */

import { syncWorkspaceTabs } from "./client";

const DEBOUNCE_MS = 400;

let timer: ReturnType<typeof setTimeout> | null = null;
let pending: { registryRoot: string; paths: string[] } | null = null;
let generation = 0;

export function scheduleWorkspaceSync(
  registryRoot: string | null | undefined,
  openPaths: readonly string[],
): void {
  if (!registryRoot) {
    pending = null;
    if (timer) {
      clearTimeout(timer);
      timer = null;
    }
    return;
  }
  pending = { registryRoot, paths: [...openPaths] };
  if (timer) clearTimeout(timer);
  const ticket = ++generation;
  timer = setTimeout(() => {
    timer = null;
    const job = pending;
    pending = null;
    if (!job || ticket !== generation) return;
    void syncWorkspaceTabs(job.registryRoot, job.paths).catch((err) => {
      console.warn("[workspace] sync failed:", err);
    });
  }, DEBOUNCE_MS);
}

/** Test seam: flush without waiting for the debounce. */
export async function flushWorkspaceSyncForTests(): Promise<void> {
  if (timer) {
    clearTimeout(timer);
    timer = null;
  }
  const job = pending;
  pending = null;
  if (!job) return;
  await syncWorkspaceTabs(job.registryRoot, job.paths);
}

export function resetWorkspaceSyncForTests(): void {
  if (timer) clearTimeout(timer);
  timer = null;
  pending = null;
  generation += 1;
}
