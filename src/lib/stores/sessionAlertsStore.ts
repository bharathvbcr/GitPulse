import { writable, type Readable } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import { isTauri } from "../platform";
import { createAttendance } from "../terminal/attendance";

/**
 * Whether an agent session may raise an OS notification, and what happened.
 *
 * The settings and the counters travel together on purpose. "Enabled, and
 * nothing delivered" and "enabled, and nine suppressed because you were
 * watching" look identical from the outside and mean opposite things, so the
 * panel that shows one always shows the other.
 */
export interface SessionAlertSettings {
  enabled: boolean;
  sound: boolean;
  shell_bell: boolean;
  configure_agents: boolean;
  hook_bridge: boolean;
  quiet_start: number | null;
  quiet_end: number | null;
}

export interface SessionAlertStatus {
  running: boolean;
  delivered: number;
  suppressed_disabled: number;
  suppressed_not_agent: number;
  suppressed_attended: number;
  suppressed_quiet: number;
  coalesced: number;
  displaced: number;
  rate_limited: number;
  dropped_queue: number;
  dropped_scan: number;
  failed: number;
  bridge_rejected: number;
  bridge_listening: boolean;
  bridge_path: string | null;
  last_error: string | null;
}

export interface SessionAlertsView {
  settings: SessionAlertSettings;
  status: SessionAlertStatus;
  bridge_supported: boolean;
}

/**
 * What the panel shows before the backend has answered, and what a browser
 * preview shows for good.
 *
 * Mirrors `SessionNotifyConfig::default` in Rust — a contract test binds the
 * two, because a default that disagrees would make the first render of the
 * settings panel show checkboxes the backend does not have.
 */
export const DEFAULT_SESSION_ALERT_SETTINGS: SessionAlertSettings = {
  enabled: true,
  sound: false,
  shell_bell: false,
  configure_agents: true,
  hook_bridge: true,
  quiet_start: null,
  quiet_end: null,
};

const EMPTY_STATUS: SessionAlertStatus = {
  running: false,
  delivered: 0,
  suppressed_disabled: 0,
  suppressed_not_agent: 0,
  suppressed_attended: 0,
  suppressed_quiet: 0,
  coalesced: 0,
  displaced: 0,
  rate_limited: 0,
  dropped_queue: 0,
  dropped_scan: 0,
  failed: 0,
  bridge_rejected: 0,
  bridge_listening: false,
  bridge_path: null,
  last_error: null,
};

const store = writable<SessionAlertsView>({
  settings: DEFAULT_SESSION_ALERT_SETTINGS,
  status: EMPTY_STATUS,
  bridge_supported: false,
});

let loaded = false;
let inFlight: Promise<SessionAlertsView> | null = null;

/**
 * Loads once and caches. Every terminal session reads `configure_agents`
 * before it spawns, so this is on the hot path of opening a tab; a per-session
 * round trip would delay every launch behind an answer that cannot change
 * between them.
 */
export async function loadSessionAlerts(): Promise<SessionAlertsView> {
  if (loaded) return current();
  if (inFlight) return inFlight;
  if (!isTauri()) {
    loaded = true;
    return current();
  }
  inFlight = invoke<SessionAlertsView>("cmd_session_alerts")
    .then((view) => {
      store.set(view);
      loaded = true;
      return view;
    })
    .catch(() => current())
    .finally(() => {
      inFlight = null;
    });
  return inFlight;
}

/** Re-reads the counters. Cheap, and the only way the panel stays truthful. */
export async function refreshSessionAlerts(): Promise<SessionAlertsView> {
  if (!isTauri()) return current();
  const view = await invoke<SessionAlertsView>("cmd_session_alerts");
  store.set(view);
  loaded = true;
  return view;
}

export async function saveSessionAlerts(
  settings: SessionAlertSettings,
): Promise<SessionAlertsView> {
  if (!isTauri()) {
    store.update((view) => ({ ...view, settings }));
    return current();
  }
  const view = await invoke<SessionAlertsView>("cmd_session_alerts_save", { settings });
  store.set(view);
  loaded = true;
  return view;
}

let snapshot: SessionAlertsView = {
  settings: DEFAULT_SESSION_ALERT_SETTINGS,
  status: EMPTY_STATUS,
  bridge_supported: false,
};
store.subscribe((view) => {
  snapshot = view;
});

function current(): SessionAlertsView {
  return snapshot;
}

/** Synchronous read for a code path that cannot await, such as a spawn. */
export function sessionAlertSettings(): SessionAlertSettings {
  return snapshot.settings;
}

export const sessionAlerts: Readable<SessionAlertsView> = { subscribe: store.subscribe };

/**
 * The one attendance reporter for the process.
 *
 * Outside Tauri it still records, so the panel's own tests can observe it
 * without a backend; the push is simply a no-op.
 */
export const terminalAttendance = createAttendance(async (sessionIds) => {
  if (!isTauri()) return;
  await invoke("cmd_session_alerts_visible", { sessionIds });
});
