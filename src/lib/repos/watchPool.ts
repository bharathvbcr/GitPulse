/**
 * Which open repositories hold a live filesystem watch.
 *
 * A watch is the scarce resource, not a tab. Each one is a native session
 * with its own threads and FSEvents streams (inotify instances on Linux), and
 * macOS registers streams one at a time system-wide, so every watch costs
 * startup latency and a slot in `MAX_WATCHES` (src-tauri/src/watcher/mod.rs).
 * Tabs used to be capped at that same number, one watch each, which is why a
 * reader could not open a twenty-fifth repository.
 *
 * So tabs are now bounded only by what the workspace can persist
 * (`MAX_OPEN_TABS`), and watches by this pool. The repositories used most
 * recently hold the watches; the rest are parked (`watchState.WATCH_PARKED`):
 * shown on the strip, not watched, and brought live — with a full read — when
 * someone opens them. That loses nothing a reader can see, because a
 * background tab is never polled and the activation hydrate reads everything
 * a watcher would have refreshed.
 *
 * Pure: the store owns the native calls and their ordering.
 */

/**
 * Live watches the frontend will hold at once.
 *
 * Kept strictly below the backend's `MAX_WATCHES`, so an eviction can admit
 * the newcomer before the evicted watch has finished tearing down (an unwatch
 * waits for its callback to release), and a leaked slot cannot refuse the tab
 * a person just opened. scripts/documented-counts-contract.test.ts holds the
 * two apart.
 */
export const WATCH_POOL_SIZE = 24;

export interface WatchAdmission {
  /** The key holds a slot now (it may already have held one). */
  admitted: boolean;
  /** A key that lost its slot to make room, whose watch must be released. */
  evicted: string | null;
}

export interface WatchPool {
  /** Whether `key` holds a slot. */
  has(key: string): boolean;
  /**
   * Gives `key` a slot and marks it most recently used.
   *
   * A foreground admission (the repository a person is looking at) always
   * succeeds, evicting the least recently used other key when the pool is
   * full. A background one takes only a free slot, so a background open can
   * never take a watch away from a repository someone used.
   */
  admit(key: string, foreground: boolean): WatchAdmission;
  /** Frees `key`'s slot; false when it held none. */
  release(key: string): boolean;
  /** Held keys, least recently used first. */
  keys(): string[];
  readonly size: number;
  readonly capacity: number;
}

export function createWatchPool(capacity: number = WATCH_POOL_SIZE): WatchPool {
  if (!Number.isInteger(capacity) || capacity < 1) {
    throw new RangeError(`A watch pool needs at least one slot, not ${capacity}`);
  }
  // Insertion order is recency order: a re-admitted key is deleted and set
  // again, so the first key is always the least recently used.
  const held = new Set<string>();
  return {
    has: (key) => held.has(key),
    admit(key, foreground) {
      if (!key) return { admitted: false, evicted: null };
      if (held.has(key)) {
        held.delete(key);
        held.add(key);
        return { admitted: true, evicted: null };
      }
      let evicted: string | null = null;
      if (held.size >= capacity) {
        if (!foreground) return { admitted: false, evicted: null };
        evicted = held.values().next().value ?? null;
        if (evicted !== null) held.delete(evicted);
      }
      held.add(key);
      return { admitted: true, evicted };
    },
    release: (key) => held.delete(key),
    keys: () => [...held],
    get size() {
      return held.size;
    },
    capacity,
  };
}
