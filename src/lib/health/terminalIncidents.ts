import { listen } from "@tauri-apps/api/event";
import { invoke } from "../ipc/invoke";
import { isTauri } from "../platform";
import { INCIDENT_EVENT, parseIncident, type TerminalIncident } from "../terminal/incident";

/** The backend keeps this many; the view never holds more. */
export const MAX_INCIDENTS_SHOWN = 32;

/**
 * Folds one incident into the list: newest first, one row per id (a re-read
 * and the live event can both carry the same incident), bounded.
 */
export function mergeIncident(
  list: readonly TerminalIncident[],
  incoming: TerminalIncident,
): TerminalIncident[] {
  const rest = list.filter((incident) => incident.id !== incoming.id);
  return [incoming, ...rest]
    .sort((a, b) => b.at_ms - a.at_ms)
    .slice(0, MAX_INCIDENTS_SHOWN);
}

/** "pkill[89829]", or the stated reason there is no sender. */
export function describeSender(incident: TerminalIncident): string {
  return incident.sender
    ? `${incident.sender.name}[${incident.sender.pid}]`
    : `unknown (${incident.sender_note})`;
}

/** Recorded incidents; a row that is not one is dropped, not trusted. */
export async function loadTerminalIncidents(): Promise<TerminalIncident[]> {
  const rows = await invoke<unknown[]>("cmd_terminal_incidents");
  return rows.map(parseIncident).filter((row): row is TerminalIncident => row !== null);
}

/** Subscribes to live incidents; resolves to the unlisten function. */
export async function watchTerminalIncidents(
  onIncident: (incident: TerminalIncident) => void,
): Promise<() => void> {
  if (!isTauri()) return () => {};
  return listen<unknown>(INCIDENT_EVENT, (event) => {
    const incident = parseIncident(event.payload);
    if (incident) onIncident(incident);
  });
}
