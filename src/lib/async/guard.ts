export interface AsyncGuard {
  isLive(): boolean;
  cancel(): void;
}

export function createAsyncGuard(): AsyncGuard {
  let cancelled = false;
  return {
    isLive: () => !cancelled,
    cancel: () => {
      cancelled = true;
    },
  };
}

export function beginGeneration(): { next(): number; isCurrent(token: number): boolean } {
  let current = 0;
  return {
    next: () => ++current,
    isCurrent: (token: number) => token === current,
  };
}
