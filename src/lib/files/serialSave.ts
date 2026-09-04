export interface KeyedSerialQueue {
  run<T>(key: string, operation: () => Promise<T>): Promise<T>;
  /** Waits for the accepted operations on one key, a key set, or every key. */
  whenIdle(key?: string | readonly string[]): Promise<void>;
  readonly pending: number;
}

export interface LatestOwnerLease {
  readonly key: string;
  isCurrent(): boolean;
  release(): void;
}

export interface LatestOwnerRegistry<T> {
  claim(key: string, owner: T): LatestOwnerLease;
  current(key: string): T | undefined;
}

/**
 * Build a collision-free queue key from the canonical repository and its
 * repository-relative file path. Length prefixes avoid relying on a separator
 * that either input might itself contain.
 */
export function fileSaveKey(repoPath: string, filePath: string): string {
  return `${repoPath.length}:${repoPath}${filePath.length}:${filePath}`;
}

/**
 * Serializes operations per key while allowing unrelated keys to proceed.
 * A rejected operation is observed by the internal tail but still rejects its
 * caller; the next operation must not be poisoned by an earlier disk error.
 */
export function createKeyedSerialQueue(): KeyedSerialQueue {
  const tails = new Map<string, Promise<void>>();

  function currentTails(keys?: readonly string[]): Promise<void>[] {
    if (keys === undefined) return [...tails.values()];

    const observed: Promise<void>[] = [];
    for (const key of new Set(keys)) {
      const tail = tails.get(key);
      if (tail) observed.push(tail);
    }
    return observed;
  }

  async function waitForKeys(keys?: readonly string[]): Promise<void> {
    while (true) {
      const observed = new Set(currentTails(keys));
      if (observed.size === 0) return;
      await Promise.all(observed);
      // Include work accepted on any target while the snapshot was settling,
      // but do not spin on resolved tails awaiting caller cleanup microtasks.
      if (currentTails(keys).every((tail) => observed.has(tail))) return;
    }
  }

  return {
    run<T>(key: string, operation: () => Promise<T>): Promise<T> {
      const previous = tails.get(key) ?? Promise.resolve();
      const task = previous.catch(() => undefined).then(operation);
      const tail = task.then(
        () => undefined,
        () => undefined,
      );
      tails.set(key, tail);
      return task.finally(() => {
        if (tails.get(key) === tail) tails.delete(key);
      });
    },
    whenIdle(key?: string | readonly string[]): Promise<void> {
      if (typeof key === "string") return waitForKeys([key]);
      return waitForKeys(key);
    },
    get pending(): number {
      return tails.size;
    },
  };
}

/** Process-lifetime coordinator shared by remounted editors and app exit. */
export const editorFileSaveQueue = createKeyedSerialQueue();

/**
 * Tracks the newest live UI owner for each repository. A stale lease can be
 * released safely after a replacement has claimed the same key; it can never
 * remove or impersonate that replacement.
 */
export function createLatestOwnerRegistry<T>(): LatestOwnerRegistry<T> {
  const owners = new Map<string, { token: symbol; owner: T }>();

  return {
    claim(key: string, owner: T): LatestOwnerLease {
      const token = Symbol(key);
      owners.set(key, { token, owner });
      return {
        key,
        isCurrent: () => owners.get(key)?.token === token,
        release: () => {
          if (owners.get(key)?.token === token) owners.delete(key);
        },
      };
    },
    current(key: string): T | undefined {
      return owners.get(key)?.owner;
    },
  };
}
