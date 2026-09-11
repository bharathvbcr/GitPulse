import { createFrameScheduler, type Caf, type Raf } from "../motion/frameScheduler";

export type ResizeObserverCtor = new (callback: ResizeObserverCallback) => ResizeObserver;

export interface ObserveResizeOptions {
  raf?: Raf;
  caf?: Caf;
  ResizeObserver?: ResizeObserverCtor;
}

export interface ResizeObservation {
  observe(target: Element): void;
  unobserve(target: Element): void;
  disconnect(): void;
}

/**
 * ResizeObserver that coalesces delivery onto the next animation frame.
 * Disconnect cancels any pending frame so a teardown cannot flush into a
 * disposed owner.
 */
export function createResizeObservation(
  onResize: (entries: ResizeObserverEntry[]) => void,
  options: ObserveResizeOptions = {},
): ResizeObservation {
  const scheduler = createFrameScheduler(options.raf, options.caf);
  const pending = new Map<Element, ResizeObserverEntry>();
  const RO = options.ResizeObserver ?? globalThis.ResizeObserver;
  if (typeof RO !== "function") {
    return {
      observe() {},
      unobserve() {},
      disconnect() {
        scheduler.cancel();
        pending.clear();
      },
    };
  }
  const observer = new RO((entries) => {
    for (const entry of entries) {
      pending.set(entry.target as Element, entry);
    }
    scheduler.schedule(() => {
      const batch = [...pending.values()];
      pending.clear();
      if (batch.length > 0) onResize(batch);
    });
  });
  return {
    observe(target) {
      observer.observe(target);
    },
    unobserve(target) {
      observer.unobserve(target);
    },
    disconnect() {
      scheduler.cancel();
      pending.clear();
      observer.disconnect();
    },
  };
}

/**
 * Observe one or more elements; returns a disconnect that also cancels the
 * pending frame.
 */
export function observeResize(
  target: Element | readonly Element[],
  onResize: (entries: ResizeObserverEntry[]) => void,
  options?: ObserveResizeOptions,
): () => void {
  const observation = createResizeObservation(onResize, options);
  const list = Array.isArray(target) ? target : [target];
  for (const el of list) {
    if (el) observation.observe(el);
  }
  return () => observation.disconnect();
}
