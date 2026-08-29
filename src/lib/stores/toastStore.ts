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

const DEFAULT_DURATIONS: Record<ToastKind, number> = {
  success: 3500,
  info: 4000,
  warning: 6000,
  error: 8000,
};

const MAX_TOASTS = 5;

function createToastStore() {
  const { subscribe, update, set } = writable<ToastItem[]>([]);
  const timers = new Map<string, ReturnType<typeof setTimeout>>();

  function dismiss(id: string) {
    const timer = timers.get(id);
    if (timer) {
      clearTimeout(timer);
      timers.delete(id);
    }
    update((toasts) => toasts.filter((t) => t.id !== id));
  }

  function add(input: ToastInput | string): string {
    const opts: ToastInput = typeof input === "string" ? { message: input, kind: "info" } : input;
    const kind = opts.kind ?? "info";
    const id = `toast-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    const duration = opts.duration ?? DEFAULT_DURATIONS[kind];

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
    set([]);
  }

  return {
    subscribe,
    add,
    dismiss,
    clear,
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
