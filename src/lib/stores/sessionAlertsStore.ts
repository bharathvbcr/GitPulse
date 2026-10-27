import { writable, type Readable } from "svelte/store";
import { invoke } from "../ipc/invoke";
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
  /** Answered before its banner was due, so never shown. */
  resolved: number;
  displaced: number;
  rate_limited: number;
  /** Banners the rate limit delayed. Not an outcome: each also lands in another counter. */
  deferred: number;
  dropped_queue: number;
  dropped_scan: number;
  failed: number;
  bridge_rejected: number;
  /** Well-formed hook reports that arrived behind a later one from the same session and were dropped. */
  stale: number;
  /**
   * Hook reports in the last 7 days that got no reply from GitPulse before the
   * hook's own deadline, from the record the hooks write beside the socket. The
   * hook's count: not part of `bridge_rejected`, and not "lost", because the
   * app may have applied a report the hook had stopped waiting for. A floor
   * when `hook_drop_record_error` is set.
   */
  hook_replies_missed: number;
  /** The newest of them, as `<event> at <UTC time>`. */
  hook_reply_missed_last: string | null;
  /** Why that record could not be read in full. Unknown is never shown as 0. */
  hook_drop_record_error: string | null;
  bridge_listening: boolean;
  bridge_path: string | null;
  last_error: string | null;
}

/** What the panel says about replies the hooks stopped waiting for. */
export interface HookReplyMisses {
  /** `null` when there is nothing to say: no misses on record. */
  line: string | null;
  /** The record could not be read in full; shown as its own alert. */
  error: string | null;
}

/**
 * The sentence for `hook_replies_missed`, and the record's read error apart
 * from it.
 *
 * Its own number, in its own words: the hook's wait, never added to
 * `bridge_rejected` (which counts what the socket refused) and never called
 * "lost", because a report the hook stopped waiting for may well have been
 * applied. With nothing counted and an error, only the error is said: an
 * unreadable record is not a clean zero.
 */
export function hookReplyMisses(status: SessionAlertStatus): HookReplyMisses {
  const count = status.hook_replies_missed;
  const last = status.hook_reply_missed_last ? ` (last: ${status.hook_reply_missed_last})` : "";
  const reports = count === 1 ? "1 hook report" : `${count} hook reports`;
  return {
    line:
      count > 0
        ? `${reports} in the last 7 days got no reply from GitPulse in time${last}. ` +
          "A missed SessionEnd can leave that agent's banner standing; hooks do not retry."
        : null,
    error: status.hook_drop_record_error,
  };
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
  resolved: 0,
  deferred: 0,
  displaced: 0,
  rate_limited: 0,
  dropped_queue: 0,
  dropped_scan: 0,
  failed: 0,
  bridge_rejected: 0,
  stale: 0,
  hook_replies_missed: 0,
  hook_reply_missed_last: null,
  hook_drop_record_error: null,
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
export const terminalAttendance = createAttendance(async (sessionIds, backgroundIds) => {
  if (!isTauri()) return;
  await invoke("cmd_session_alerts_visible", { sessionIds, backgroundIds });
});

/**
 * Tells the notifier the reader answered what `sessionId` was asking, so it
 * is not replayed to the next page and its banner comes down. Best effort:
 * the pane has already cleared, and a failure is logged rather than shown,
 * because the reader is mid-keystroke and cannot act on it.
 */
export function reportAttentionAnswered(sessionId: string): void {
  if (!isTauri()) return;
  invoke("cmd_session_attention_answered", { sessionId }).catch((error: unknown) => {
    console.warn("[gitpulse-alerts] answered attention not reported:", error);
  });
}

/** What every agent session stands asking for, for a page that just started listening. */
export async function standingAttention(): Promise<unknown> {
  if (!isTauri()) return [];
  return invoke<unknown>("cmd_session_attention_standing");
}
