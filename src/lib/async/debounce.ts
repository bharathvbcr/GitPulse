export interface Debounced<A extends unknown[]> {
  (...args: A): void;
  /** Drops the pending trailing call; later invocations schedule anew. */
  cancel(): void;
}

/** Trailing-edge debounce: `fn` runs once, after `waitMs` of quiet. */
export function debounce<A extends unknown[]>(fn: (...args: A) => void, waitMs: number): Debounced<A> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const debounced = (...args: A): void => {
    if (timer !== undefined) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = undefined;
      fn(...args);
    }, waitMs);
  };
  debounced.cancel = (): void => {
    if (timer !== undefined) {
      clearTimeout(timer);
      timer = undefined;
    }
  };
  return debounced;
}
