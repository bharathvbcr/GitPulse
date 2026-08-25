export type Raf = (cb: FrameRequestCallback) => number;
export type Caf = (id: number) => void;

function defaultRaf(): Raf {
  if (typeof requestAnimationFrame === "function") {
    return requestAnimationFrame.bind(globalThis);
  }
  return (cb) => setTimeout(() => cb(performance.now()), 16) as unknown as number;
}

function defaultCaf(): Caf {
  if (typeof cancelAnimationFrame === "function") {
    return cancelAnimationFrame.bind(globalThis);
  }
  return (id) => clearTimeout(id);
}

/**
 * Coalesces work onto the next animation frame so scroll, hover, and resize
 * cannot queue more than one paint per vsync.
 */
export function createFrameScheduler(raf: Raf = defaultRaf(), caf: Caf = defaultCaf()) {
  let handle = 0;
  let scheduled = false;
  let pending: FrameRequestCallback | null = null;

  function flush(now: number) {
    scheduled = false;
    handle = 0;
    const job = pending;
    pending = null;
    job?.(now);
  }

  function schedule(job: FrameRequestCallback) {
    pending = job;
    if (scheduled) return;
    scheduled = true;
    handle = raf(flush);
  }

  function cancel() {
    if (scheduled) {
      caf(handle);
    }
    scheduled = false;
    handle = 0;
    pending = null;
  }

  function isScheduled() {
    return scheduled;
  }

  return { schedule, cancel, isScheduled };
}
