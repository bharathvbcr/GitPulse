import { writable, type Readable } from "svelte/store";
import { identityKey, isCaseInsensitiveFs, type PathIdentityOptions } from "../repos/paths";

/**
 * Open terminal-dock tab counts per repository checkout (including non-PTY
 * Browser, File, and Snippet tabs).
 *
 * `terminalSessions` tracks native PTY slots, whereas `panelTabCounts` lets
 * surfaces like `TerminalPill` know when a repository's dock has open tabs
 * without live PTYs so an open Browser/File dock is never described as empty.
 */
export function createPanelTabCounts(
  defaultOptions: PathIdentityOptions = { caseInsensitive: isCaseInsensitiveFs() },
) {
  const counts = new Map<string, number>();
  const store = writable<ReadonlyMap<string, number>>(new Map());
  const publish = () => store.set(new Map(counts));

  return {
    subscribe: store.subscribe as Readable<ReadonlyMap<string, number>>["subscribe"],
    report(repoPath: string, count: number, options: PathIdentityOptions = defaultOptions): void {
      const key = identityKey(repoPath, options);
      if (!key) return;
      const normalized = Number.isFinite(count) ? Math.max(0, Math.trunc(count)) : 0;
      if (normalized === 0) {
        if (!counts.delete(key)) return;
        publish();
        return;
      }
      if (counts.get(key) === normalized) return;
      counts.set(key, normalized);
      publish();
    },
    forget(repoPath: string, options: PathIdentityOptions = defaultOptions): void {
      const key = identityKey(repoPath, options);
      if (!key || !counts.delete(key)) return;
      publish();
    },
    countFor(
      snapshot: ReadonlyMap<string, number>,
      repoPath: string | null | undefined,
      options: PathIdentityOptions = defaultOptions,
    ): number {
      if (!repoPath) return 0;
      const key = identityKey(repoPath, options);
      return key ? (snapshot.get(key) ?? 0) : 0;
    },
    reset(): void {
      if (counts.size === 0) return;
      counts.clear();
      publish();
    },
  };
}

export const panelTabCounts = createPanelTabCounts();
