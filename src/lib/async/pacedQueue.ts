/** Optional work follows the visible repository; closed keys are forgotten. */
export interface BackgroundScope {
  activeKey: string | null;
  retainedKeys: readonly string[];
  visible: boolean;
}

/**
 * Which open keys may run while the window is visible.
 * `active` (default) is the focused repository only — metrics and docs.
 * `retained` is every open tab, so a background map can heal without focus.
 */
export type PacedQueueRunWhen = "active" | "retained";

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
  runWhen?: PacedQueueRunWhen;
}) {
  const { debounceMs, maxWaitMs, restMs, capacity } = options;
  const runWhen: PacedQueueRunWhen = options.runWhen ?? "active";
  if (![debounceMs, maxWaitMs, restMs, capacity].every(Number.isFinite) ||
    debounceMs < 0 || maxWaitMs < debounceMs || restMs < 0 ||
    !Number.isInteger(capacity) || capacity < 1) {
    throw new RangeError("Invalid background queue limits");
  }
  /** `hold` is a retry's earliest start; later enqueues may not pull it in. */
  const pending = new Map<string, { first: number; due: number; hold: number }>();
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
    if (scope === null) return true;
    if (!scope.visible || !scope.retainedKeys.has(key)) return false;
    return runWhen === "retained" || scope.activeKey === key;
  }

  function schedule(): void {
    if (timer !== null) clearTimeout(timer);
    timer = null;
    if (running !== null || pending.size === 0) return;
    let earliest = Infinity;
    for (const [key, { due }] of pending) {
      if (eligible(key)) earliest = Math.min(earliest, due);
    }
    // Hidden, closed, or (in active mode) unfocused keys need no wakeup.
    if (earliest === Infinity) return;
    timer = setTimeout(() => {
      timer = null;
      const now = performance.now();
      const ready = (key: string) => {
        const entry = pending.get(key);
        return entry !== undefined && entry.due <= now && eligible(key);
      };
      // The focused repository goes first. Insertion order alone put it
      // behind every tab restored before it, so at launch the one map the user
      // is looking at waited on all the others.
      const activeKey = scope?.visible ? scope.activeKey : null;
      let next = activeKey !== null && ready(activeKey) ? activeKey : null;
      if (next === null) {
        for (const key of pending.keys()) {
          if (ready(key)) {
            next = key;
            break;
          }
        }
      }
      if (next !== null) {
        pending.delete(next);
        void run(next);
        return;
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
    /**
     * `delayMs` holds the key back at least that long, past the debounce and
     * `maxWaitMs`: a retry after the backend declined under load. An enqueue
     * while that retry is pending joins it rather than pulling it forward.
     */
    enqueue(key: string, retry?: { delayMs?: number }): boolean {
      if (!key || (scope !== null && !scope.retainedKeys.has(key))) return false;
      const delayMs = retry?.delayMs ?? 0;
      if (!Number.isFinite(delayMs) || delayMs < 0) throw new RangeError("Invalid background queue delay");
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
      const hold = Math.max(existing?.hold ?? 0, now + delayMs);
      pending.set(key, { first, hold, due: Math.max(Math.min(now + debounceMs, first + maxWaitMs), hold) });
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
