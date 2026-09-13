import { writable } from "svelte/store";

// Product learning is independent of optional CLI installation and its config.
export const TOUR_STEPS = ["welcome", "repository", "views", "tasks", "permissions", "ready"] as const;
const STORAGE_KEY = "gitpulse.product-tour.v1";
type TourStatus = "active" | "deferred" | "completed";
interface Progress { version: 1; step: number; status: TourStatus }
interface TourState extends Progress { open: boolean; error: string | null }
type TourStorage = Pick<Storage, "getItem" | "setItem">;

function parseProgress(raw: string | null): Progress {
  const initial: Progress = { version: 1, step: 0, status: "active" };
  if (!raw) return initial;
  const value: unknown = JSON.parse(raw);
  if (typeof value !== "object" || !value || !("version" in value) || value.version !== 1 ||
      !("step" in value) || typeof value.step !== "number" || !Number.isInteger(value.step) ||
      value.step < 0 || value.step >= TOUR_STEPS.length || !("status" in value) ||
      (value.status !== "active" && value.status !== "deferred" && value.status !== "completed")) return initial;
  return { version: 1, step: value.step, status: value.status };
}

export function createProductTour(storage: () => TourStorage) {
  let state: TourState = { version: 1, step: 0, status: "active", open: false, error: null };
  let initialized = false;
  const { subscribe, set } = writable(state);
  const publish = (patch: Partial<TourState>) => { state = { ...state, ...patch }; set(state); };
  function save(progress: Progress): boolean {
    try {
      storage().setItem(STORAGE_KEY, JSON.stringify(progress));
      publish({ ...progress, error: null });
      return true;
    } catch {
      publish({ error: "Walkthrough progress could not be saved. Try again, or close without saving; it may appear next launch." });
      return false;
    }
  }
  function move(delta: number) {
    if (!state.open) return;
    const step = Math.max(0, Math.min(TOUR_STEPS.length - 1, state.step + delta));
    publish({ step });
    save({ version: 1, step, status: "active" });
  }
  return {
    subscribe,
    initialize() {
      if (initialized) return;
      initialized = true;
      try {
        const progress = parseProgress(storage().getItem(STORAGE_KEY));
        publish({ ...progress, open: progress.status === "active" });
      } catch {
        publish({ open: true, error: "Walkthrough progress could not be loaded. You can continue from the beginning." });
      }
    },
    open() {
      publish({ open: true, step: state.status === "completed" ? 0 : state.step, status: "active" });
      save({ version: 1, step: state.step, status: "active" });
    },
    next() { move(1); },
    back() { move(-1); },
    dismiss() {
      if (!save({ version: 1, step: state.step, status: "deferred" })) return false;
      publish({ open: false });
      return true;
    },
    finish() {
      if (!state.open || state.step !== TOUR_STEPS.length - 1) return false;
      if (!save({ version: 1, step: state.step, status: "completed" })) return false;
      publish({ open: false });
      return true;
    },
    closeForSession() { publish({ open: false }); },
  };
}

export type ProductTourStore = ReturnType<typeof createProductTour>;
export const productTour = createProductTour(() => window.localStorage);
