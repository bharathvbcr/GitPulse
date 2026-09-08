/** Optional work follows the visible repository; closed keys are forgotten. */
export interface BackgroundScope {
  activeKey: string | null;
  retainedKeys: readonly string[];
  visible: boolean;
}

/** Bounded, fair background scans. A reset cannot cancel an issued IPC. */
export function createPacedQueue(options: {
  debounceMs: number;
  maxWaitMs: number;
  restMs: number;
  capacity: number;
  run: (key: string, isCurrent: () => boolean) => Promise<void>;
  onError: (key: string, error: unknown) => void;
  onOverflow: () => void;
  scope?: BackgroundScope;
}) {
  const { debounceMs, maxWaitMs, restMs, capacity } = options;
  if (![debounceMs, maxWaitMs, restMs, capacity].every(Number.isFinite) ||
    debounceMs < 0 || maxWaitMs < debounceMs || restMs < 0 ||
    !Number.isInteger(capacity) || capacity < 1) {
    throw new RangeError("Invalid background queue limits");
  }
  const pending = new Map<string, { first: number; due: number }>();
  let timer: ReturnType<typeof setTimeout> | null = null;
  let running: string | null = null;
  let generation = 0;
  let nextStart = 0;
  let overflowReported = false;
  let scope = options.scope ? copyScope(options.scope) : null;

  function copyScope(next: BackgroundScope) {
    return { activeKey: next.activeKey, visible: next.visible, retainedKeys: new Set(next.retainedKeys) };
  }

  function eligible(key: string): boolean {
    return scope === null || (scope.visible && scope.activeKey === key && scope.retainedKeys.has(key));
  }

  function schedule(): void {
    if (timer !== null) clearTimeout(timer);
    timer = null;
    if (running !== null || pending.size === 0) return;
    let earliest = Infinity;
    for (const [key, { due }] of pending) {
      if (eligible(key)) earliest = Math.min(earliest, due);
    }
    // Hidden/inactive dirty keys need no polling timer. Scope changes wake us.
    if (earliest === Infinity) return;
    timer = setTimeout(() => {
      timer = null;
      const now = performance.now();
      for (const [key, { due }] of pending) {
        if (due <= now && eligible(key)) {
          pending.delete(key);
          void run(key);
          return;
        }
      }
      schedule();
    }, Math.max(0, Math.ceil(Math.max(earliest, nextStart) - performance.now())));
  }

  async function run(key: string): Promise<void> {
    running = key;
    const runGeneration = generation;
    try {
      await options.run(key, () => runGeneration === generation);
    } catch (error) {
      if (runGeneration === generation) options.onError(key, error);
    } finally {
      running = null;
      nextStart = performance.now() + restMs;
      if (pending.size === 0) overflowReported = false;
      schedule();
    }
  }

  return {
    enqueue(key: string): boolean {
      if (!key || (scope !== null && !scope.retainedKeys.has(key))) return false;
      const existing = pending.get(key);
      if (!existing && pending.size >= capacity) {
        if (!overflowReported) {
          overflowReported = true;
          options.onOverflow();
        }
        return false;
      }
      const now = performance.now();
      const first = existing?.first ?? now;
      pending.set(key, { first, due: Math.min(now + debounceMs, first + maxWaitMs) });
      schedule();
      return true;
    },
    has(key: string): boolean { return running === key || pending.has(key); },
    isPending(key: string): boolean { return pending.has(key); },
    setScope(next: BackgroundScope | null): void {
      const previousKey = scope?.visible ? scope.activeKey : null;
      scope = next ? copyScope(next) : null;
      if (scope) {
        for (const key of pending.keys()) {
          if (!scope.retainedKeys.has(key)) pending.delete(key);
        }
        // Closing and reopening the same path cannot revive an old result.
        if (running !== null && !scope.retainedKeys.has(running)) generation += 1;
      }
      const nextKey = scope?.visible ? scope.activeKey : null;
      if (nextKey && nextKey !== previousKey) {
        nextStart = Math.max(nextStart, performance.now() + debounceMs);
      }
      if (pending.size < capacity) overflowReported = false;
      schedule();
    },
    reset(): void {
      if (timer !== null) clearTimeout(timer);
      timer = null;
      pending.clear();
      nextStart = 0;
      generation += 1;
      overflowReported = false;
      // Keep the running slot until the native operation actually settles.
    },
  };
}
