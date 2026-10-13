import type { createHostedClaudeStore, createResumers } from "./hostedClaude";

/**
 * A process the unified log names as a signal's sender
 * (`terminal/incident.rs::Sender`). Named as Rust names it, which is how
 * `check:types` pairs the two sides.
 */
export interface Sender {
  pid: number;
  name: string;
}

/**
 * A burst of hosted sessions ended by the same signal, as the backend reports
 * it on the `terminal-incident` event and in `cmd_terminal_incidents`. The one
 * place this wire shape is written down: the backend emits exactly this and
 * the frontend reads exactly this. Held to Rust's `TerminalIncidentPayload`
 * field for field by `check:types` (the `terminal-incident` contract).
 */
export interface TerminalIncidentPayload {
  /** Stable for one incident; a repeated event carries the same id. */
  id: string;
  /** The signal that ended them, as the exit event names it. */
  signal: string;
  /** When it happened, in milliseconds since the epoch. */
  at_ms: number;
  /** The backend's terminal session ids (`terminal-exit` `id`), not Claude's. */
  session_ids: string[];
  /** Who sent it, when the platform said; null when it did not. */
  sender: Sender | null;
  /** What is known about the sender, in words, including "sender unknown". */
  sender_note: string;
}

/** The name the panels use for the payload above. */
export type TerminalIncident = TerminalIncidentPayload;

export const INCIDENT_EVENT = "terminal-incident";

/**
 * Most session ids one incident may list: the backend's own cap
 * (`MAX_MEMBERS` in src-tauri/src/terminal/incident.rs), so every id it sends
 * is kept. A longer list is truncated, not trusted.
 */
export const MAX_INCIDENT_SESSIONS = 512;

/** Incident ids remembered so a repeated event is not offered twice. */
const MAX_SEEN_INCIDENTS = 64;

/** An incident from an event payload, or null for anything that is not one. */
export function parseIncident(payload: unknown): TerminalIncident | null {
  if (typeof payload !== "object" || payload === null) return null;
  const p = payload as Record<string, unknown>;
  if (typeof p.id !== "string" || !p.id || p.id.length > 128) return null;
  if (typeof p.signal !== "string") return null;
  if (typeof p.at_ms !== "number" || !Number.isFinite(p.at_ms)) return null;
  if (!Array.isArray(p.session_ids)) return null;
  const ids = p.session_ids.filter((id): id is string => typeof id === "string" && id.length > 0 && id.length <= 128);
  const sender =
    typeof p.sender === "object" && p.sender !== null &&
    typeof (p.sender as Record<string, unknown>).pid === "number" &&
    typeof (p.sender as Record<string, unknown>).name === "string"
      ? { pid: (p.sender as { pid: number }).pid, name: (p.sender as { name: string }).name.slice(0, 128) }
      : null;
  return {
    id: p.id,
    signal: p.signal.slice(0, 128),
    at_ms: p.at_ms,
    session_ids: [...new Set(ids)].slice(0, MAX_INCIDENT_SESSIONS),
    sender,
    sender_note: typeof p.sender_note === "string" ? p.sender_note.slice(0, 512) : "",
  };
}

export interface IncidentPlan {
  /** Sessions with a recorded conversation: the backend id of the dead PTY and Claude's session id. */
  resumable: { terminalId: string; sessionId: string }[];
  /** Listed sessions with no recorded conversation: shells, `--continue` launches, sessions this page never hosted. */
  skipped: number;
}

/**
 * Which of an incident's sessions have a Claude conversation on record.
 * Whether a tab is still there to relaunch it is asked when the offer is
 * taken, not here: the incident can arrive a moment before a tab has seen its
 * own exit. A session without a record is counted, not guessed at.
 */
export function planIncident(
  incident: TerminalIncident,
  store: Pick<ReturnType<typeof createHostedClaudeStore>, "byTerminal">,
): IncidentPlan {
  const resumable: IncidentPlan["resumable"] = [];
  const claimed = new Set<string>();
  let skipped = 0;
  for (const terminalId of incident.session_ids) {
    const record = store.byTerminal(terminalId);
    if (!record || claimed.has(record.sessionId)) {
      skipped += 1;
      continue;
    }
    claimed.add(record.sessionId);
    resumable.push({ terminalId, sessionId: record.sessionId });
  }
  return { resumable, skipped };
}

/** How an incident reaches the person. */
export interface IncidentNotice {
  message: string;
  action: { label: string; onClick: () => Promise<void> } | null;
}

/** One line naming the incident: how many, what signal, and who sent it if known. */
export function describeIncident(incident: TerminalIncident, plan: IncidentPlan): string {
  const sender = incident.sender
    ? `sent by ${incident.sender.name} (pid ${incident.sender.pid})`
    : incident.sender_note || "sender unknown";
  const hosted = incident.session_ids.length;
  const resumable = plan.resumable.length;
  return `${hosted} terminal session${hosted === 1 ? "" : "s"} ended by ${incident.signal || "a signal"} — ${sender}. ${
    resumable === 0
      ? "None has a Claude conversation to resume."
      : `${resumable} Claude conversation${resumable === 1 ? "" : "s"} can be resumed.`
  }`;
}

/**
 * Turns `terminal-incident` events into one offer each.
 *
 * An event is handled once per incident id however often it arrives. The offer
 * is "Resume N" for the N listed tabs that have a recorded conversation and a
 * tab to relaunch it; taking it relaunches them one at a time, because each
 * one claims a session slot and a refused slot must not stop the others.
 */
export function createIncidentController(options: {
  store: Pick<ReturnType<typeof createHostedClaudeStore>, "byTerminal">;
  resumers: Pick<ReturnType<typeof createResumers>, "get" | "unregister">;
  notify: (notice: IncidentNotice) => void;
  /** Reports a relaunch that failed, once per tab. */
  fail?: (terminalId: string, cause: unknown) => void;
  /** Reports what taking the offer did: tabs relaunched out of tabs offered. */
  done?: (started: number, offered: number) => void;
}) {
  const seen: string[] = [];
  const { store, resumers, notify } = options;

  async function resumeAll(plan: IncidentPlan) {
    let started = 0;
    for (const { terminalId } of plan.resumable) {
      // Taken first: a second press, or the tab's own button, must not relaunch it twice.
      const resume = resumers.get(terminalId);
      resumers.unregister(terminalId);
      // A tab the user closed is gone with its resumer; nothing to relaunch.
      if (!resume) continue;
      try {
        await resume();
        started += 1;
      } catch (cause) {
        options.fail?.(terminalId, cause);
      }
    }
    options.done?.(started, plan.resumable.length);
    return started;
  }

  return {
    handle(payload: unknown): IncidentPlan | null {
      const incident = parseIncident(payload);
      if (!incident || seen.includes(incident.id)) return null;
      seen.push(incident.id);
      if (seen.length > MAX_SEEN_INCIDENTS) seen.shift();
      const plan = planIncident(incident, store);
      const resumable = plan.resumable.length;
      notify({
        message: describeIncident(incident, plan),
        action: resumable === 0 ? null : { label: `Resume ${resumable}`, onClick: () => resumeAll(plan).then(() => undefined) },
      });
      return plan;
    },
  };
}
