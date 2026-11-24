/**
 * Three-stage idle memory manager for the main WebView.
 *
 * Distinguishes unfocused-but-visible (e.g. GitPulse open on a second display)
 * from truly hidden (minimized, occluded, or hidden to the menu bar) so visible
 * panes never lose WebGL or flicker during rapid Cmd+Tab switches:
 *
 *  - **Stage 1A (`soft_shed`)**: Unfocused ≥ `softShedMs` (still visible).
 *    Drops only *off-screen* retainers — hidden terminal WebGL contexts,
 *    inactive repository commit graphs, inactive file listings, inactive
 *    harness ledger buckets, and hidden global surfaces (`tasks`, `fleet`,
 *    `agents`).
 *  - **Stage 1B (`deep_shed`)**: Truly hidden ≥ `deepShedMs` (or hidden under
 *    `critical` host memory pressure). Suspends ALL WebGL contexts, sheds all
 *    commit-graph rows and file listings, parks hidden terminals, and signals
 *    native WebView low-memory hints (`cmd_webview_idle_hint`).
 *  - **Stage 2 (`hibernating`)**: Hidden ≥ `hibernateMs` (or hidden under
 *    `critical` pressure in `aggressive` mode). Checks for unsaved drafts or
 *    in-flight mutations, flushes pending storage + wake-skeleton layout hints,
 *    and asks Rust to destroy the `main` WebView (`cmd_main_hibernate`) while
 *    keeping shells/agents alive in `gitpulse-ptyd` and the app resident in the
 *    tray.
 */
import { writable, type Readable } from "svelte/store";
import {
  addForegroundListener,
  isBackgroundDocument,
  isHiddenDocument,
  noteForegroundFocus,
  removeForegroundListener,
  type ForegroundDocument,
} from "./foreground";
import type { PressureInput } from "../stores/hostLoadStore";

export type IdleMemoryMode = "off" | "balanced" | "aggressive";
export type IdleStage = "active" | "soft_shed" | "deep_shed" | "hibernating";

export interface IdleModeSchedule {
  softShedMs: number;
  deepShedMs: number;
  hibernateMs: number;
  shedOnCriticalPressure: boolean;
  hibernateOnCriticalWhenHidden: boolean;
}

export const IDLE_MEMORY_SCHEDULES: Record<Exclude<IdleMemoryMode, "off">, IdleModeSchedule> = {
  balanced: {
    softShedMs: 45_000,
    deepShedMs: 30_000,
    hibernateMs: 10 * 60_000,
    shedOnCriticalPressure: true,
    hibernateOnCriticalWhenHidden: false,
  },
  aggressive: {
    softShedMs: 20_000,
    deepShedMs: 5_000,
    hibernateMs: 3 * 60_000,
    shedOnCriticalPressure: true,
    hibernateOnCriticalWhenHidden: true,
  },
};

export const WAKE_HINT_STORAGE_KEY = "gitpulse.wake-skeleton.v1";

export interface WakeSkeletonHint {
  surface: string;
  sidebarWidth: number;
  hasTerminalDock: boolean;
  savedAt: number;
}

export function writeWakeSkeletonHint(
  hint: WakeSkeletonHint,
  storage: Pick<Storage, "setItem"> | null = typeof localStorage === "undefined" ? null : localStorage,
): void {
  if (!storage) return;
  try {
    storage.setItem(WAKE_HINT_STORAGE_KEY, JSON.stringify(hint));
  } catch {
    // Best-effort visual hint only.
  }
}

export function clearWakeSkeletonHint(
  storage: Pick<Storage, "removeItem"> | null = typeof localStorage === "undefined" ? null : localStorage,
): void {
  if (!storage) return;
  try {
    storage.removeItem(WAKE_HINT_STORAGE_KEY);
  } catch {
    // Ignore restricted storage.
  }
}

export interface IdleShedderMetrics {
  stage: IdleStage;
  mode: IdleMemoryMode;
  softShedCount: number;
  deepShedCount: number;
  hibernateAttempts: number;
  hibernateDeferrals: number;
  lastTransitionAt: number | null;
  lastDeferReason: string | null;
}

export interface IdleShedderTimers {
  setTimeout(cb: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
  now(): number;
}

const realTimers: IdleShedderTimers = {
  setTimeout: (cb, ms) => globalThis.setTimeout(cb, ms),
  clearTimeout: (handle) => globalThis.clearTimeout(handle as ReturnType<typeof setTimeout>),
  now: () => Date.now(),
};

export interface IdleShedderActions {
  /** Stage 1A: shed off-screen retainers while keeping the visible pane crisp. */
  onSoftShed(): void;
  /** Stage 1B: release all WebGL contexts, graphs, listings, and send native low-memory hint. */
  onDeepShed(): void;
  /** Restore normal budgets and WebView hints when the window returns to foreground. */
  onResume(previous: IdleStage): void;
  /**
   * Returns a human-readable blocker reason if hibernation must be deferred
   * (e.g. unsaved editor drafts or active git mutations), or `null` if safe.
   */
  hibernateBlocker(): string | null;
  /** Stage 2: flush drafts/state and request Rust to destroy the main WebView. */
  onHibernate(): Promise<boolean> | boolean;
}

export interface IdleShedder {
  stage: Readable<IdleStage>;
  metrics: Readable<IdleShedderMetrics>;
  setMode(mode: IdleMemoryMode): void;
  setPressure(pressure: PressureInput): void;
  /** Notify explicitly (e.g. when Rust hides the main window to the tray). */
  noteWindowHidden(hidden: boolean): void;
  /** Manually re-evaluate timers against current visibility & focus. */
  evaluate(): void;
  /** Manually trigger hibernation attempt if safe (for testing or Diagnostics). */
  tryHibernateNow(): Promise<boolean>;
  stop(): void;
}

export function createIdleShedder(
  actions: IdleShedderActions,
  deps: {
    initialMode?: IdleMemoryMode;
    doc?: (EventTarget & ForegroundDocument) | null;
    win?: EventTarget | null;
    timers?: IdleShedderTimers;
  } = {},
): IdleShedder {
  const timers = deps.timers ?? realTimers;
  const doc = deps.doc === undefined ? (typeof document === "undefined" ? null : document) : deps.doc;
  const win = deps.win === undefined ? (typeof window === "undefined" ? null : window) : deps.win;

  let mode: IdleMemoryMode = deps.initialMode ?? "balanced";
  let pressure: PressureInput = "unavailable";
  let forcedHidden = false;
  let stopped = false;

  let stage: IdleStage = "active";
  let softTimer: unknown = null;
  let deepTimer: unknown = null;
  let hibernateTimer: unknown = null;
  let unfocusedSinceMs: number | null = null;
  let hiddenSinceMs: number | null = null;

  let metricsState: IdleShedderMetrics = {
    stage: "active",
    mode,
    softShedCount: 0,
    deepShedCount: 0,
    hibernateAttempts: 0,
    hibernateDeferrals: 0,
    lastTransitionAt: null,
    lastDeferReason: null,
  };

  const stageStore = writable<IdleStage>(stage);
  const metricsStore = writable<IdleShedderMetrics>(metricsState);

  const publish = () => {
    stageStore.set(stage);
    metricsState = { ...metricsState, stage, mode };
    metricsStore.set(metricsState);
  };

  const clearTimers = () => {
    if (softTimer !== null) {
      timers.clearTimeout(softTimer);
      softTimer = null;
    }
    if (deepTimer !== null) {
      timers.clearTimeout(deepTimer);
      deepTimer = null;
    }
    if (hibernateTimer !== null) {
      timers.clearTimeout(hibernateTimer);
      hibernateTimer = null;
    }
  };

  const isHidden = () => forcedHidden || isHiddenDocument(doc);
  const isUnfocused = () => isHidden() || isBackgroundDocument(doc);

  const enterActive = () => {
    clearTimers();
    if (stage === "active") return;
    const prev = stage;
    stage = "active";
    metricsState = { ...metricsState, lastTransitionAt: timers.now(), lastDeferReason: null };
    publish();
    actions.onResume(prev);
  };

  const enterSoftShed = () => {
    if (stopped || mode === "off") return;
    if (stage === "active") {
      stage = "soft_shed";
      metricsState = {
        ...metricsState,
        softShedCount: metricsState.softShedCount + 1,
        lastTransitionAt: timers.now(),
      };
      publish();
      actions.onSoftShed();
    }
  };

  const enterDeepShed = () => {
    if (stopped || mode === "off") return;
    if (stage === "active") enterSoftShed();
    if (stage === "soft_shed") {
      stage = "deep_shed";
      metricsState = {
        ...metricsState,
        deepShedCount: metricsState.deepShedCount + 1,
        lastTransitionAt: timers.now(),
      };
      publish();
      actions.onDeepShed();
    }
  };

  const attemptHibernate = async (): Promise<boolean> => {
    if (stopped || mode === "off") return false;
    if (!isHidden()) return false;
    const blocker = actions.hibernateBlocker();
    if (blocker !== null) {
      metricsState = {
        ...metricsState,
        hibernateDeferrals: metricsState.hibernateDeferrals + 1,
        lastDeferReason: blocker,
      };
      publish();
      // Retry on the next idle cycle while still hidden.
      const schedule = IDLE_MEMORY_SCHEDULES[mode];
      if (hibernateTimer === null && isHidden() && !stopped) {
        hibernateTimer = timers.setTimeout(() => {
          hibernateTimer = null;
          void attemptHibernate();
        }, schedule.hibernateMs);
      }
      return false;
    }
    enterDeepShed();
    stage = "hibernating";
    metricsState = {
      ...metricsState,
      hibernateAttempts: metricsState.hibernateAttempts + 1,
      lastTransitionAt: timers.now(),
      lastDeferReason: null,
    };
    publish();
    try {
      const ok = await actions.onHibernate();
      if (!ok && !stopped && stage === "hibernating") {
        stage = "deep_shed";
        publish();
      }
      return ok;
    } catch {
      if (!stopped && stage === "hibernating") {
        stage = "deep_shed";
        publish();
      }
      return false;
    }
  };

  const evaluate = () => {
    if (stopped) return;
    if (mode === "off") {
      unfocusedSinceMs = null;
      hiddenSinceMs = null;
      enterActive();
      return;
    }
    const schedule = IDLE_MEMORY_SCHEDULES[mode];
    const hidden = isHidden();
    const unfocused = isUnfocused();
    const critical = pressure === "critical";
    const nowMs = timers.now();

    if (!unfocused && !hidden) {
      unfocusedSinceMs = null;
      hiddenSinceMs = null;
      enterActive();
      return;
    }

    if (unfocusedSinceMs === null) unfocusedSinceMs = nowMs;
    if (hidden) {
      if (hiddenSinceMs === null) hiddenSinceMs = nowMs;
    } else {
      hiddenSinceMs = null;
    }

    // Window is unfocused (or hidden).
    if (stage === "active") {
      const elapsed = nowMs - unfocusedSinceMs;
      if ((critical && schedule.shedOnCriticalPressure) || elapsed >= schedule.softShedMs) {
        if (softTimer !== null) {
          timers.clearTimeout(softTimer);
          softTimer = null;
        }
        enterSoftShed();
      } else if (softTimer === null) {
        softTimer = timers.setTimeout(() => {
          softTimer = null;
          if (!stopped && isUnfocused()) enterSoftShed();
        }, Math.max(0, schedule.softShedMs - elapsed));
      }
    }

    if (!hidden) {
      // Visible on a second monitor: never escalate to deep_shed or hibernate.
      if (deepTimer !== null) {
        timers.clearTimeout(deepTimer);
        deepTimer = null;
      }
      if (hibernateTimer !== null) {
        timers.clearTimeout(hibernateTimer);
        hibernateTimer = null;
      }
      return;
    }

    // Truly hidden (minimized / tray-hidden / occluded).
    const hiddenElapsed = nowMs - (hiddenSinceMs ?? nowMs);
    if (stage !== "deep_shed" && stage !== "hibernating") {
      if ((critical && schedule.shedOnCriticalPressure) || hiddenElapsed >= schedule.deepShedMs) {
        if (deepTimer !== null) {
          timers.clearTimeout(deepTimer);
          deepTimer = null;
        }
        enterDeepShed();
      } else if (deepTimer === null) {
        deepTimer = timers.setTimeout(() => {
          deepTimer = null;
          if (!stopped && isHidden()) enterDeepShed();
        }, Math.max(0, schedule.deepShedMs - hiddenElapsed));
      }
    }

    if (stage !== "hibernating") {
      if (
        (critical && schedule.hibernateOnCriticalWhenHidden && (hibernateTimer === null || metricsState.lastDeferReason === null)) ||
        (hiddenElapsed >= schedule.hibernateMs && hibernateTimer === null)
      ) {
        if (hibernateTimer !== null) {
          timers.clearTimeout(hibernateTimer);
          hibernateTimer = null;
        }
        void attemptHibernate();
      } else if (hibernateTimer === null) {
        hibernateTimer = timers.setTimeout(() => {
          hibernateTimer = null;
          void attemptHibernate();
        }, Math.max(0, schedule.hibernateMs - hiddenElapsed));
      }
    }
  };

  const onFocus = () => {
    noteForegroundFocus(true);
    forcedHidden = false;
    evaluate();
  };
  const onBlur = () => {
    noteForegroundFocus(false);
    evaluate();
  };
  const onVisibility = () => {
    if (!isHiddenDocument(doc)) forcedHidden = false;
    evaluate();
  };

  if (doc) {
    addForegroundListener(doc, win, "focus", onFocus);
    addForegroundListener(doc, win, "blur", onBlur);
    addForegroundListener(doc, win, "visibilitychange", onVisibility);
  }

  evaluate();

  return {
    stage: { subscribe: stageStore.subscribe },
    metrics: { subscribe: metricsStore.subscribe },
    setMode(next) {
      if (next === mode) return;
      mode = next;
      clearTimers();
      publish();
      evaluate();
    },
    setPressure(next) {
      if (next === pressure) return;
      pressure = next;
      evaluate();
    },
    noteWindowHidden(hidden) {
      if (forcedHidden === hidden) return;
      forcedHidden = hidden;
      evaluate();
    },
    evaluate,
    tryHibernateNow: attemptHibernate,
    stop() {
      if (stopped) return;
      stopped = true;
      clearTimers();
      if (doc) {
        removeForegroundListener("focus", onFocus);
        removeForegroundListener("blur", onBlur);
        removeForegroundListener("visibilitychange", onVisibility);
      }
    },
  };
}

/** Shared singleton store reflecting the live page's idle stage for UI surfaces. */
const globalIdleStage = writable<IdleStage>("active");
export const idleStage: Readable<IdleStage> = { subscribe: globalIdleStage.subscribe };

export function publishIdleStage(stage: IdleStage): void {
  globalIdleStage.set(stage);
}
