import type { MenuState } from "./menuState";

/** One in-flight update and one latest value. A failure never becomes a successful dedup key. */
export function createMenuSync<T = MenuState>(send: (state: T) => Promise<void>, failed: (error: unknown) => void) {
  let pending: T | null = null;
  let running = false;
  let disposed = false;
  let lastApplied = "";
  let retry: ReturnType<typeof setTimeout> | null = null;
  let attempts = 0;
  async function drain() {
    if (running || disposed) return;
    running = true;
    try {
      while (pending !== null && !disposed) {
        const state = pending;
        pending = null;
        const key = JSON.stringify(state);
        if (key === lastApplied) continue;
        try {
          await send(state);
          lastApplied = key;
          attempts = 0;
        } catch (error) {
          if (disposed) return;
          failed(error);
          if (pending !== null && JSON.stringify(pending) !== key) {
            attempts = 0;
            continue;
          }
          // Retry transient startup failures twice. New state supersedes the retry.
          if (++attempts <= 2) {
            pending ??= state;
            retry = setTimeout(() => { retry = null; void drain(); }, 500 * attempts);
          }
          return;
        }
      }
    } finally { running = false; }
  }
  return {
    update(state: T) {
      if (disposed) return;
      pending = state;
      if (!retry) void drain();
    },
    dispose() {
      disposed = true;
      pending = null;
      if (retry) clearTimeout(retry);
      retry = null;
    },
  };
}
