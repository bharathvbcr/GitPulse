import type { createHostedClaudeStore, createResumers } from "./hostedClaude";
import type { TranscriptAnswer } from "./transcripts";

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
  /**
   * Sessions whose conversation Claude Code saved: the backend id of the dead
   * PTY and Claude's session id. The only ones Resume is offered for.
   */
  resumable: { terminalId: string; sessionId: string }[];
  /** Conversations on record whose transcript could not be checked: not offered, and said apart. */
  unknown: number;
  /**
   * Listed sessions with nothing to resume: shells, `--continue` launches,
   * sessions this page never hosted, and conversations Claude Code never saved.
   */
  skipped: number;
}

/** Asks whether Claude Code saved each conversation (`transcripts.checkTranscripts`). */
export type TranscriptCheck = (sessionIds: string[]) => Promise<Map<string, TranscriptAnswer>>;

/**
 * Which of an incident's sessions have a conversation to resume: one on
 * record whose transcript was found. Whether a tab is still there to relaunch
 * it is asked when the offer is taken, not here: the incident can arrive a
 * moment before a tab has seen its own exit. A session without a record is
 * counted, not guessed at, and one whose transcript could not be checked is
 * counted apart from one that has none.
 */
export async function planIncident(
  incident: TerminalIncident,
  store: Pick<ReturnType<typeof createHostedClaudeStore>, "byTerminal">,
  check: TranscriptCheck,
): Promise<IncidentPlan> {
  const recorded: IncidentPlan["resumable"] = [];
  const claimed = new Set<string>();
  let skipped = 0;
  for (const terminalId of incident.session_ids) {
    const record = store.byTerminal(terminalId);
    if (!record || claimed.has(record.sessionId)) {
      skipped += 1;
      continue;
    }
    claimed.add(record.sessionId);
    recorded.push({ terminalId, sessionId: record.sessionId });
  }
  const answers = recorded.length === 0 ? new Map<string, TranscriptAnswer>() : await check(recorded.map((r) => r.sessionId));
  const plan: IncidentPlan = { resumable: [], unknown: 0, skipped };
  for (const entry of recorded) {
    const state = answers.get(entry.sessionId)?.state ?? "unknown";
    if (state === "found") plan.resumable.push(entry);
    else if (state === "unknown") plan.unknown += 1;
    else plan.skipped += 1;
  }
  return plan;
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
  const unknown =
    plan.unknown === 0
      ? ""
      : ` ${plan.unknown} more could not be checked for a saved conversation, so ${plan.unknown === 1 ? "it is" : "they are"} not offered.`;
  return `${hosted} terminal session${hosted === 1 ? "" : "s"} ended by ${incident.signal || "a signal"} — ${sender}. ${
    resumable === 0
      ? "None has a saved Claude conversation to resume."
      : `${resumable} Claude conversation${resumable === 1 ? "" : "s"} can be resumed.`
  }${unknown}`;
}

/** What taking an incident's offer did, tab by tab. */
export interface ResumeReport {
  offered: number;
  /** Relaunched: a process started. */
  started: number;
  /** Waiting for a free terminal slot; each starts when one frees. Not started yet. */
  deferred: number;
  /** Tried and failed; each tab says why. */
  failed: number;
  /** No tab left to relaunch in: the user closed it, or another press took it. */
  missing: number;
}

/**
 * Turns `terminal-incident` events into one offer each.
 *
 * An event is handled once per incident id however often it arrives. The offer
 * is "Resume N" for the N listed tabs whose conversation Claude Code saved;
 * taking it relaunches them one at a time, because each one claims a session
 * slot and a refused slot must not stop the others. Each relaunch says what it
 * did, and only a process that started is counted as resumed.
 */
export function createIncidentController(options: {
  store: Pick<ReturnType<typeof createHostedClaudeStore>, "byTerminal">;
  resumers: Pick<ReturnType<typeof createResumers>, "get" | "unregister">;
  check: TranscriptCheck;
  notify: (notice: IncidentNotice) => void;
  /** Reports a relaunch that threw, once per tab. A resumer reports its own failures; this is for one that broke. */
  fail?: (terminalId: string, cause: unknown) => void;
  /** Reports what taking the offer did. */
  done?: (report: ResumeReport) => void;
}) {
  const seen: string[] = [];
  const { store, resumers, notify } = options;

  async function resumeAll(plan: IncidentPlan): Promise<ResumeReport> {
    const report: ResumeReport = { offered: plan.resumable.length, started: 0, deferred: 0, failed: 0, missing: 0 };
    for (const { terminalId } of plan.resumable) {
      // Taken first: a second press, or the tab's own button, must not relaunch it twice.
      const resume = resumers.get(terminalId);
      resumers.unregister(terminalId);
      if (!resume) {
        report.missing += 1;
        continue;
      }
      try {
        report[await resume()] += 1;
      } catch (cause) {
        report.failed += 1;
        options.fail?.(terminalId, cause);
      }
    }
    options.done?.(report);
    return report;
  }

  return {
    /**
     * Offers the incident once. Resolves with its plan, or null for a payload
     * that is not an incident or one already offered.
     */
    async handle(payload: unknown): Promise<IncidentPlan | null> {
      const incident = parseIncident(payload);
      if (!incident || seen.includes(incident.id)) return null;
      // Marked before the transcript check, so a repeat arriving meanwhile is not offered twice.
      seen.push(incident.id);
      if (seen.length > MAX_SEEN_INCIDENTS) seen.shift();
      const plan = await planIncident(incident, store, options.check);
      const resumable = plan.resumable.length;
      notify({
        message: describeIncident(incident, plan),
        action: resumable === 0 ? null : { label: `Resume ${resumable}`, onClick: () => resumeAll(plan).then(() => undefined) },
      });
      return plan;
    },
  };
}
