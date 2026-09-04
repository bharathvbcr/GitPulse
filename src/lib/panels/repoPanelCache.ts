/**
 * Tiny LRU cache keyed by repository path, for panel payloads that survive
 * view-tab remounts. Panels are destroyed and recreated on every
 * `{#key $repoStore.activeTab}` switch in App.svelte; hydrating from this
 * cache synchronously before a refetch renders last-known data instantly
 * instead of blanking behind a loading placeholder.
 *
 * Exported as a factory (not a singleton) so each panel owns its instance and
 * tests stay isolated. Mirrors graphStore's plain-Map per-repo-path spirit.
 */
export interface RepoPanelCache<T> {
  /** Most-recent value stored for `path`, refreshing its recency. */
  get(path: string): T | undefined;
  set(path: string, value: T): void;
  clear(): void;
  readonly size: number;
}

export interface RepoPanelCacheOptions<T> {
  maxRepos?: number;
  /** Protected values may exceed the soft bound rather than be destroyed. */
  canEvict?: (value: T, path: string) => boolean;
}

export function createRepoPanelCache<T>(options?: RepoPanelCacheOptions<T>): RepoPanelCache<T> {
  const maxRepos = Math.max(1, options?.maxRepos ?? 8);
  // Map iterates in insertion order: re-inserting on get/set moves an entry to
  // the tail, so the head is always the least-recently-used victim.
  const entries = new Map<string, T>();

  return {
    get(path: string): T | undefined {
      if (!entries.has(path)) return undefined;
      const value = entries.get(path) as T;
      entries.delete(path);
      entries.set(path, value);
      return value;
    },
    set(path: string, value: T): void {
      entries.delete(path);
      entries.set(path, value);
      while (entries.size > maxRepos) {
        let evicted = false;
        for (const [candidatePath, candidateValue] of entries) {
          if (options?.canEvict && !options.canEvict(candidateValue, candidatePath)) continue;
          entries.delete(candidatePath);
          evicted = true;
          break;
        }
        if (!evicted) break;
      }
    },
    clear(): void {
      entries.clear();
    },
    get size(): number {
      return entries.size;
    },
  };
}
