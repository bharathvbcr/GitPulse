import { writable } from "svelte/store";

export type ToastKind = "success" | "info" | "warning" | "error";

export interface ToastAction {
  label: string;
  onClick: () => void | Promise<void>;
}

export interface ToastItem {
  id: string;
  kind: ToastKind;
  message: string;
  action?: ToastAction;
  duration?: number;
  createdAt: number;
}

export type ToastInput = {
  kind?: ToastKind;
  message: string;
  action?: ToastAction;
  duration?: number;
};

/**
 * How long each kind stays up. Zero means "until dismissed".
 *
 * Errors used to expire after eight seconds. `repoStore.error` is routed to a
 * toast and nowhere else, so a failed git operation flashed and was then gone
 * for good — no history, no record, nothing to copy into a bug report. An
 * error is the one kind whose whole content is something the user may need to
 * act on after reading it, so it now waits to be dismissed.
 */
const DEFAULT_DURATIONS: Record<ToastKind, number> = {
  success: 3500,
  info: 4000,
  warning: 6000,
  error: 0,
};

/**
 * Extra time granted to a toast carrying an action.
 *
 * "Undo" after a branch delete and "Pop" after a stash both rode the 4 s info
 * default, which can expire while the pointer is still travelling to the
 * button. An offer the user cannot reach is not an offer.
 */
const ACTION_DURATION_MS = 12_000;

const MAX_TOASTS = 5;

function createToastStore() {
  const { subscribe, update, set } = writable<ToastItem[]>([]);
  const timers = new Map<string, ReturnType<typeof setTimeout>>();
  /** Ids whose countdown is frozen while the user is on the stack. */
  const paused = new Set<string>();

  function dismiss(id: string) {
    const timer = timers.get(id);
    if (timer) {
      clearTimeout(timer);
      timers.delete(id);
    }
    paused.delete(id);
    update((toasts) => toasts.filter((t) => t.id !== id));
  }

  function add(input: ToastInput | string): string {
    const opts: ToastInput = typeof input === "string" ? { message: input, kind: "info" } : input;
    const kind = opts.kind ?? "info";
    const id = `toast-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    const duration =
      opts.duration ?? (opts.action ? ACTION_DURATION_MS : DEFAULT_DURATIONS[kind]);

    const item: ToastItem = {
      id,
      kind,
      message: opts.message,
      action: opts.action,
      duration,
      createdAt: Date.now(),
    };

    update((toasts) => {
      const next = [...toasts, item];
      if (next.length > MAX_TOASTS) {
        const oldest = next[0];
        if (oldest) {
          const t = timers.get(oldest.id);
          if (t) {
            clearTimeout(t);
            timers.delete(oldest.id);
          }
        }
        return next.slice(next.length - MAX_TOASTS);
      }
      return next;
    });

    if (duration > 0) {
      const timer = setTimeout(() => {
        dismiss(id);
      }, duration);
      timers.set(id, timer);
    }

    return id;
  }

  function clear() {
    timers.forEach((t) => clearTimeout(t));
    timers.clear();
    paused.clear();
    set([]);
  }

  /**
   * Freezes every countdown, and restarts them from their full duration.
   *
   * Called when the pointer enters the stack or focus lands inside it. The
   * remaining time is deliberately NOT preserved: someone who moved to the
   * toast is reading it, and restarting the clock is the behaviour that makes
   * a reachable action reachable. A toast with `duration === 0` has no timer
   * to pause and is unaffected.
   */
  function pauseAll() {
    for (const [id, timer] of timers) {
      clearTimeout(timer);
      timers.delete(id);
      paused.add(id);
    }
  }

  /** Restarts the countdown for every toast paused above that still exists. */
  function resumeAll() {
    const live = new Map<string, number>();
    update((toasts) => {
      for (const toast of toasts) live.set(toast.id, toast.duration ?? 0);
      return toasts;
    });
    for (const id of paused) {
      const duration = live.get(id);
      if (!duration || duration <= 0) continue;
      timers.set(
        id,
        setTimeout(() => dismiss(id), duration),
      );
    }
    paused.clear();
  }

  return {
    subscribe,
    add,
    dismiss,
    clear,
    pauseAll,
    resumeAll,
    action: (message: string, actionLabel: string, onClick: () => void | Promise<void>, duration?: number) =>
      add({ kind: "info", message, action: { label: actionLabel, onClick }, duration }),
    success: (message: string, action?: ToastAction, duration?: number) =>
      add({ kind: "success", message, action, duration }),
    info: (message: string, action?: ToastAction, duration?: number) =>
      add({ kind: "info", message, action, duration }),
    warning: (message: string, action?: ToastAction, duration?: number) =>
      add({ kind: "warning", message, action, duration }),
    error: (message: string, action?: ToastAction, duration?: number) =>
      add({ kind: "error", message, action, duration }),
  };
}

export const toastStore = createToastStore();
