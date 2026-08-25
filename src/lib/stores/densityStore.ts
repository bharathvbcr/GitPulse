import { writable } from "svelte/store";
import type { DensityMode } from "../canvas/GraphRenderer";

export type { DensityMode } from "../canvas/GraphRenderer";

const STORAGE_KEY = "gitpulse_density_mode";

function readInitial(): DensityMode {
  if (typeof window === "undefined") return "spacious";
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (raw === "spacious" || raw === "compact") return raw;
  } catch {
    /* ignore storage errors */
  }
  return "spacious";
}

function persist(mode: DensityMode) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, mode);
  } catch {
    /* ignore storage errors */
  }
}

function createDensityStore() {
  const initial = readInitial();
  const { subscribe, set, update } = writable<DensityMode>(initial);

  return {
    subscribe,
    setDensity: (mode: DensityMode) => {
      persist(mode);
      set(mode);
    },
    toggle: () => {
      update((current) => {
        const next: DensityMode = current === "spacious" ? "compact" : "spacious";
        persist(next);
        return next;
      });
    },
  };
}

export const densityStore = createDensityStore();
