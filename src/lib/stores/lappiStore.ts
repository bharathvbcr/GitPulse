import { writable, type Readable } from "svelte/store";
import { invoke } from "../ipc/invoke";
import { isTauri } from "../platform";

/**
 * GitPulse's two Lappi switches (`src-tauri/src/lappi`), both off by default.
 *
 * Asking and recording are independent: recording never sends anything, and
 * asking never records. `LAPPI_COLLECT=0` in GitPulse's environment forces
 * recording off whatever the switch says, which `collect_forced_off` reports.
 */
export interface LappiSettings {
  ask_on_ambiguous_commit_type: boolean;
  record_caller_data: boolean;
}

export interface StoreStatus {
  dir: string;
  written: number;
  dropped: number;
  stopped: string | null;
  last_error: string | null;
}

export interface LappiView {
  settings: LappiSettings;
  collect_forced_off: boolean;
  socket: string | null;
  store: StoreStatus | null;
  transport_supported: boolean;
}

/** Mirrors `LappiSettings::default` in Rust: both off. */
export const DEFAULT_LAPPI_SETTINGS: LappiSettings = {
  ask_on_ambiguous_commit_type: false,
  record_caller_data: false,
};

const EMPTY: LappiView = {
  settings: DEFAULT_LAPPI_SETTINGS,
  collect_forced_off: false,
  socket: null,
  store: null,
  transport_supported: false,
};

const store = writable<LappiView>(EMPTY);

/** Reads the switches and what recording has done. */
export async function refreshLappi(): Promise<LappiView> {
  if (!isTauri()) return EMPTY;
  const view = await invoke<LappiView>("cmd_lappi_settings");
  store.set(view);
  return view;
}

export async function saveLappi(settings: LappiSettings): Promise<LappiView> {
  if (!isTauri()) {
    store.update((view) => ({ ...view, settings }));
    return { ...EMPTY, settings };
  }
  const view = await invoke<LappiView>("cmd_lappi_settings_save", { settings });
  store.set(view);
  return view;
}

export const lappi: Readable<LappiView> = { subscribe: store.subscribe };
