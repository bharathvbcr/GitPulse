import { writable, type Readable } from "svelte/store";

/**
 * Whether the Explorer page shows the Live pulse column beside the editor.
 *
 * One app-wide preference, remembered across launches. It used to be per
 * repository and in memory only, defaulting to shown, so a hidden Live pulse
 * came back on every repository switch and every restart, and hiding it never
 * stuck. Explorer's own visibility stays per repository in `FileViewer`.
 */

export const LIVE_PULSE_STORAGE_KEY = "gitpulse.explorer.livePulse";

export interface LivePulsePrefs {
  readonly visible: boolean;
}

type StorageLike = Pick<Storage, "getItem" | "setItem">;

const DEFAULTS: LivePulsePrefs = { visible: true };

function defaultStorage(): StorageLike | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    /* storage disabled — run in memory */
    return null;
  }
}

/** Field-wise defensive parse: anything unusable reads as the default. */
export function readLivePulsePrefs(
  storage: Pick<StorageLike, "getItem"> | null = defaultStorage(),
): LivePulsePrefs {
  if (!storage) return { ...DEFAULTS };
  try {
    const raw = storage.getItem(LIVE_PULSE_STORAGE_KEY);
    if (!raw) return { ...DEFAULTS };
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return { ...DEFAULTS };
    const visible: unknown = Reflect.get(parsed, "visible");
    return { visible: typeof visible === "boolean" ? visible : DEFAULTS.visible };
  } catch {
    return { ...DEFAULTS };
  }
}

export function writeLivePulsePrefs(
  prefs: LivePulsePrefs,
  storage: Pick<StorageLike, "setItem"> | null = defaultStorage(),
): void {
  if (!storage) return;
  try {
    storage.setItem(LIVE_PULSE_STORAGE_KEY, JSON.stringify({ visible: Boolean(prefs.visible) }));
  } catch {
    /* quota or private mode — keep the in-memory value */
  }
}

export interface LivePulsePrefStore extends Readable<LivePulsePrefs> {
  setVisible(visible: boolean): void;
}

export function createLivePulsePrefStore(
  storage: StorageLike | null = defaultStorage(),
): LivePulsePrefStore {
  const { subscribe, update } = writable<LivePulsePrefs>(readLivePulsePrefs(storage));
  return {
    subscribe,
    setVisible: (visible) =>
      update((current) => {
        const next = Boolean(visible);
        if (current.visible === next) return current;
        const prefs = { visible: next };
        writeLivePulsePrefs(prefs, storage);
        return prefs;
      }),
  };
}

export const livePulsePref = createLivePulsePrefStore();
