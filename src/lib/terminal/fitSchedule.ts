import { createFrameScheduler, type Caf, type Raf } from "../motion/frameScheduler";

/**
 * The fewest milliseconds between two fits that were asked for by layout.
 *
 * A column change reflows the whole scrollback, and every fit that changes the
 * grid also tells the PTY, whose program repaints in answer. A drag that moves
 * the box a little every frame would otherwise do both at the display's frame
 * rate. 32 ms is what other terminal front ends use for the same job: half a
 * 60 Hz frame budget twice over, short enough that the grid still follows the
 * pointer.
 */
export const FIT_MIN_INTERVAL_MS = 32;

export interface FitSchedulerOptions {
  minIntervalMs?: number;
  now?: () => number;
  setTimer?: (run: () => void, ms: number) => unknown;
  clearTimer?: (handle: unknown) => void;
  raf?: Raf;
  caf?: Caf;
}

export interface FitScheduler {
  /**
   * Layout moved: fit now if the last fit was long enough ago, otherwise once
   * when it will have been. Any number of requests in the gap are one run, and
   * that run measures the box as it is then, so the last size of a drag is
   * always fitted.
   */
  request(): void;
  /**
   * The terminal is being shown, or the metrics under its grid changed: fit
   * once on the next animation frame, which is after the layout that made it
   * visible, and drop the pending throttled run (this one measures the same
   * box). `after` runs once that fit is done.
   */
  settle(after?: () => void): void;
  /** Teardown: nothing scheduled runs afterwards. */
  cancel(): void;
}

/**
 * Decides when `run` (one measurement and, if the grid changed, one resize)
 * happens. It never decides whether the terminal may be resized: that is
 * `run`'s own check, so every path here is guarded the same way.
 */
export function createFitScheduler(run: () => void, options: FitSchedulerOptions = {}): FitScheduler {
  const minInterval = options.minIntervalMs ?? FIT_MIN_INTERVAL_MS;
  const now = options.now ?? (() => performance.now());
  const setTimer = options.setTimer ?? ((fn: () => void, ms: number) => setTimeout(fn, ms));
  const clearTimer = options.clearTimer ?? ((handle: unknown) => clearTimeout(handle as ReturnType<typeof setTimeout>));
  const frames = createFrameScheduler(options.raf, options.caf);
  const afterFit = new Set<() => void>();
  let lastRun = Number.NEGATIVE_INFINITY;
  let timer: unknown = null;

  const runNow = () => {
    lastRun = now();
    run();
  };
  const dropTimer = () => {
    if (timer === null) return;
    clearTimer(timer);
    timer = null;
  };

  return {
    request() {
      if (timer !== null) return;
      const wait = lastRun + minInterval - now();
      if (wait <= 0) {
        runNow();
        return;
      }
      timer = setTimer(() => {
        timer = null;
        runNow();
      }, wait);
    },
    settle(after) {
      if (after) afterFit.add(after);
      dropTimer();
      frames.schedule(() => {
        const done = [...afterFit];
        afterFit.clear();
        dropTimer();
        runNow();
        for (const callback of done) callback();
      });
    },
    cancel() {
      dropTimer();
      frames.cancel();
      afterFit.clear();
    },
  };
}
