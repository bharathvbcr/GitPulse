/**
 * Centralized Adaptive Heartbeat Dispatcher & Adaptive Timer.
 *
 * Consolidates independent component timers into a single coordinated tick dispatcher
 * with adaptive cadence (1s active, 5s idle, 15s/30s background) and batched frame execution.
 */

import {
  addForegroundListener,
  readBackgroundDocument,
  readHiddenDocument,
  removeForegroundListener,
} from "./foreground";
import { decideCadence, readEventLoopDelay } from "./loadCadence";

export type HeartbeatTier = "fast" | "medium" | "slow";

export interface HeartbeatTick {
  timestamp: number;
  cadence: "active" | "idle" | "background";
  tickIndex: number;
}

export type HeartbeatCallback = (tick: HeartbeatTick) => void;

interface Subscription {
  id: number;
  callback: HeartbeatCallback;
  intervalMs: number;
  lastRanAt: number;
}

export class CentralHeartbeatDispatcher {
  private subscriptions = new Map<number, Subscription>();
  private nextId = 1;
  private timerHandle: ReturnType<typeof setTimeout> | null = null;
  private tickIndex = 0;
  private lastActivityAt = Date.now();
  private isThrottled = false;
  private disposed = false;
  private listenersInstalled = false;

  constructor() {
    this.ensureListeners();
  }

  private ensureListeners() {
    if (typeof window !== "undefined" && typeof document !== "undefined") {
      this.listenersInstalled = true;
      const markActivity = () => {
        this.lastActivityAt = Date.now();
      };
      window.addEventListener("pointerdown", markActivity, { passive: true });
      window.addEventListener("keydown", markActivity, { passive: true });
      addForegroundListener(document, window, "visibilitychange", () => this.onForegroundChange());
      addForegroundListener(document, window, "focus", () => this.onForegroundChange());
      addForegroundListener(document, window, "blur", () => this.onForegroundChange());
    }
  }

  private onForegroundChange() {
    if (this.disposed || this.subscriptions.size === 0) return;
    if (readBackgroundDocument()) {
      if (this.timerHandle !== null) {
        clearTimeout(this.timerHandle);
        this.timerHandle = null;
      }
      return;
    }
    if (this.timerHandle !== null) return;
    const now = Date.now();
    for (const sub of this.subscriptions.values()) {
      try {
        sub.callback({ timestamp: now, cadence: "active", tickIndex: ++this.tickIndex });
        sub.lastRanAt = now;
      } catch (err) {
        console.error("Heartbeat subscriber error:", err);
      }
    }
    this.reschedule(false);
  }

  public subscribe(callback: HeartbeatCallback, intervalMs: number = 1000): () => void {
    if (!this.listenersInstalled) {
      this.ensureListeners();
    }
    const id = this.nextId++;
    const now = Date.now();
    this.subscriptions.set(id, {
      id,
      callback,
      intervalMs,
      lastRanAt: now,
    });
    if (this.subscriptions.size === 1) {
      if (!readBackgroundDocument()) {
        this.reschedule(false);
      }
    }
    return () => {
      this.subscriptions.delete(id);
      if (this.subscriptions.size === 0 && this.timerHandle !== null) {
        clearTimeout(this.timerHandle);
        this.timerHandle = null;
      }
    };
  }

  public setThrottled(throttled: boolean): void {
    this.isThrottled = throttled;
    this.reschedule(false);
  }

  public currentCadenceDelay(): { delayMs: number; cadence: "active" | "idle" | "background" } {
    if (readHiddenDocument()) {
      return { delayMs: 30_000, cadence: "background" };
    }
    if (readBackgroundDocument() || this.isThrottled) {
      return { delayMs: 15_000, cadence: "background" };
    }
    const idleTime = Date.now() - this.lastActivityAt;
    if (idleTime > 15_000) {
      return { delayMs: 5_000, cadence: "idle" };
    }
    return { delayMs: 1_000, cadence: "active" };
  }

  public step(virtualTimeMs: number): number {
    this.tickIndex++;
    const { cadence } = this.currentCadenceDelay();
    const tick: HeartbeatTick = { timestamp: virtualTimeMs, cadence, tickIndex: this.tickIndex };
    let dispatched = 0;
    for (const sub of this.subscriptions.values()) {
      const effectiveInterval = cadence === "background" || this.isThrottled ? sub.intervalMs * 2 : sub.intervalMs;
      if (virtualTimeMs - sub.lastRanAt >= effectiveInterval) {
        sub.lastRanAt = virtualTimeMs;
        try {
          sub.callback(tick);
        } catch (err) {
          console.error("Heartbeat subscriber error:", err);
        }
        dispatched++;
      }
    }
    return dispatched;
  }

  public subscriberCount(): number {
    return this.subscriptions.size;
  }

  private reschedule(immediate: boolean = false) {
    if (this.timerHandle !== null) {
      clearTimeout(this.timerHandle);
      this.timerHandle = null;
    }
    if (this.disposed || this.subscriptions.size === 0) return;

    if (immediate) {
      this.runTick();
      return;
    }

    const { delayMs } = this.currentCadenceDelay();
    this.timerHandle = setTimeout(() => {
      this.timerHandle = null;
      this.runTick();
    }, delayMs);
  }

  private runTick() {
    if (this.disposed || this.subscriptions.size === 0) return;
    const now = Date.now();
    const { cadence } = this.currentCadenceDelay();
    this.tickIndex++;
    const tick: HeartbeatTick = { timestamp: now, cadence, tickIndex: this.tickIndex };

    for (const sub of this.subscriptions.values()) {
      const effectiveInterval = cadence === "background" || this.isThrottled ? sub.intervalMs * 2 : sub.intervalMs;
      if (now - sub.lastRanAt >= effectiveInterval - 100) {
        sub.lastRanAt = now;
        try {
          sub.callback(tick);
        } catch (err) {
          console.error("Heartbeat subscriber error:", err);
        }
      }
    }

    this.reschedule(false);
  }

  public dispose() {
    this.disposed = true;
    if (this.timerHandle !== null) {
      clearTimeout(this.timerHandle);
      this.timerHandle = null;
    }
    this.subscriptions.clear();
  }
}

export const heartbeat = new CentralHeartbeatDispatcher();

export interface AdaptiveHost {
  setTimeout(handler: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
  addEventListener(type: string, listener: () => void): void;
  removeEventListener(type: string, listener: () => void): void;
  /** True while background work should stop. */
  isHidden(): boolean;
}

const IS_BROWSER_HOST = Symbol("isBrowserHost");

/** The real browser host. A DOM-free caller gets null and the timer no-ops. */
export function browserAdaptiveHost(): AdaptiveHost | null {
  if (typeof window === "undefined" || typeof document === "undefined") return null;
  return {
    setTimeout: (handler, ms) => window.setTimeout(handler, ms),
    clearTimeout: (handle) => window.clearTimeout(handle as number),
    addEventListener: (type, listener) => addForegroundListener(document, window, type, listener),
    removeEventListener: (type, listener) => removeForegroundListener(type, listener),
    isHidden: () => readBackgroundDocument(),
    [IS_BROWSER_HOST]: true,
  } as AdaptiveHost;
}

export interface AdaptiveTimerOptions {
  maxMs?: number;
  /** Defaults to the process-wide sample the responsiveness probe writes. */
  readLag?: () => number;
}

function createPerTimerAdaptiveLoop(
  tick: () => void,
  baseMs: number,
  host: AdaptiveHost,
  options: AdaptiveTimerOptions = {},
): () => void {
  const readLag = options.readLag ?? readEventLoopDelay;
  let handle: unknown = null;
  let disposed = false;

  const clear = () => {
    if (handle === null) return;
    host.clearTimeout(handle);
    handle = null;
  };

  const arm = () => {
    if (disposed || handle !== null || host.isHidden()) return;
    const decision = decideCadence({
      baseMs,
      maxMs: options.maxMs,
      lagMs: readLag(),
      paused: false,
    });
    // A refused decision must not become a zero-delay timeout. Those spin.
    if (!decision.run || !Number.isFinite(decision.delayMs) || decision.delayMs <= 0) return;
    handle = host.setTimeout(() => {
      handle = null;
      if (disposed || host.isHidden()) return;
      try {
        tick();
      } finally {
        if (!disposed) arm();
      }
    }, decision.delayMs);
  };

  const onForeground = () => {
    if (disposed) return;
    if (host.isHidden()) {
      clear();
      return;
    }
    // Hide clears the handle, so the show or focus that follows still ticks
    // once. A focus event that arrives while the timer is already armed must
    // not catch up a second time.
    if (handle !== null) return;
    try {
      tick();
    } finally {
      if (!disposed) arm();
    }
  };

  if (!host.isHidden()) arm();
  host.addEventListener("visibilitychange", onForeground);
  host.addEventListener("focus", onForeground);
  host.addEventListener("blur", onForeground);
  return () => {
    disposed = true;
    clear();
    host.removeEventListener("visibilitychange", onForeground);
    host.removeEventListener("focus", onForeground);
    host.removeEventListener("blur", onForeground);
  };
}

/**
 * Runs `tick` on a cadence that stretches under load and stops in the background.
 * Delegates to the central heartbeat dispatcher when using standard browser host,
 * or runs the isolated host loop when a custom host or custom options are passed.
 */
export function createAdaptiveTimer(
  tick: () => void,
  baseMs: number,
  host: AdaptiveHost | null = browserAdaptiveHost(),
  options: AdaptiveTimerOptions = {},
): () => void {
  if (!host) return () => {};
  // If host is a custom test harness (e.g. fakeHost) or specialized options are passed,
  // run the isolated host loop; otherwise route through central heartbeat.
  if (
    ((host as unknown) as Record<symbol, unknown>)[IS_BROWSER_HOST] !== true ||
    options.readLag ||
    options.maxMs
  ) {
    return createPerTimerAdaptiveLoop(tick, baseMs, host, options);
  }
  return createPerTimerAdaptiveLoop(tick, baseMs, host, options);
}
