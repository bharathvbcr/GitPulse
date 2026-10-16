/**
 * What a quit does with the terminals, when they can outlive GitPulse.
 *
 * With the terminal host (`gitpulse-ptyd`, Settings → Agents → "Keep
 * terminals running when GitPulse quits") a session no longer has to end with
 * the app, so a quit with sessions open asks: leave them running for the next
 * launch, or stop them. In-process, nothing changes: quitting ends every
 * session, and `quitGuard.ts` asks about the agents among them.
 *
 * A session list that cannot be read is not "no sessions": the question is
 * asked anyway, saying the count is unknown.
 */

import { invoke as tauriInvoke } from "../ipc/invoke";
import { parseListing } from "./detachedSessions";

type Invoke = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

/** `cmd_terminal_backend`. Mirrors `terminal::TerminalBackendStatus`. */
export interface TerminalBackendStatus {
  backend: "host" | "in-process";
  /** What is wrong with the host, plainly. Empty when nothing is known to be. */
  trouble: string[];
}

export function parseBackendStatus(raw: unknown): TerminalBackendStatus | null {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const row = raw as Record<string, unknown>;
  if (row.backend !== "host" && row.backend !== "in-process") return null;
  if (!Array.isArray(row.trouble) || !row.trouble.every((t) => typeof t === "string")) return null;
  return { backend: row.backend, trouble: row.trouble.slice(0, 16) };
}

export async function readTerminalBackend(invoke: Invoke = tauriInvoke as Invoke): Promise<TerminalBackendStatus> {
  const status = parseBackendStatus(await invoke<unknown>("cmd_terminal_backend"));
  if (!status) throw new Error("The terminal backend answered in a form this page cannot read.");
  return status;
}

/** `cmd_exit_app`'s `terminals`. */
export type QuitTerminals = "stop" | "leave";

export type QuitTerminalsDecision =
  /** Terminals end with the app; the agent check in `quitGuard.ts` applies. */
  | { backend: "in-process" }
  /** The host keeps them, or stops them, as chosen. */
  | { backend: "host"; terminals: QuitTerminals }
  /** The person chose to keep working. */
  | { backend: "host"; cancelled: true };

/** The three-way question: `confirm` leaves them, `alternate` stops them. */
export interface QuitQuestion {
  title: string;
  message: string;
  confirmLabel: string;
  alternateLabel: string;
  cancelLabel: string;
}

export type Ask = (question: QuitQuestion) => Promise<"confirm" | "alternate" | null>;

const LISTED = 5;

export function quitQuestion(
  sessions: readonly { label: string; repo: string }[] | null,
  displayName: (repo: string) => string = (repo) => repo,
): QuitQuestion {
  const count = sessions?.length ?? null;
  const listed = (sessions ?? [])
    .slice(0, LISTED)
    .map((session) => `• ${session.label} — ${displayName(session.repo)}`)
    .join("\n");
  const omitted = count !== null && count > LISTED ? `\n…and ${count - LISTED} more` : "";
  const what =
    count === null ? "Terminal sessions may still be running (GitPulse could not list them)."
      : count === 1 ? "A terminal session is still running:"
      : `${count} terminal sessions are still running:`;
  return {
    title: count === 1 ? "Leave the Terminal Running?" : "Leave the Terminals Running?",
    message: `${what}${listed ? `\n${listed}${omitted}` : ""}\nLeft running, they keep working while GitPulse is closed, and the next launch shows them again with what they printed. Stopped, they end now, with whatever they are in the middle of.`,
    confirmLabel: "Quit and Leave Running",
    alternateLabel: count === 1 ? "Stop It and Quit" : "Stop Them and Quit",
    cancelLabel: "Keep Working",
  };
}

/**
 * Decides what a quit does with the terminals. Asks only when the host holds
 * them and at least one is running (or the list could not be read).
 */
export async function decideQuitTerminals(deps: {
  ask: Ask;
  invoke?: Invoke;
  displayName?: (repo: string) => string;
  onError?: (what: string, error: unknown) => void;
}): Promise<QuitTerminalsDecision> {
  const invoke = deps.invoke ?? (tauriInvoke as Invoke);
  const report = deps.onError ?? (() => {});
  let status: TerminalBackendStatus;
  try {
    status = await readTerminalBackend(invoke);
  } catch (error) {
    // Unknown is read as in-process: that question says the agents end,
    // which is the warning that cannot do harm if it is wrong.
    report("which process holds the terminals", error);
    return { backend: "in-process" };
  }
  if (status.backend === "in-process") return { backend: "in-process" };
  let sessions: { label: string; repo: string }[] | null;
  try {
    const raw = await invoke<unknown>("cmd_terminal_sessions");
    if (!Array.isArray(raw)) throw new Error("The terminal host returned an invalid session list.");
    sessions = raw
      .map(parseListing)
      .filter((listing) => listing !== null && listing.ending !== true)
      .map((listing) => ({ label: listing!.launcher ?? "Terminal", repo: listing!.repo }));
  } catch (error) {
    report("the terminal host's sessions", error);
    sessions = null;
  }
  if (sessions !== null && sessions.length === 0) return { backend: "host", terminals: "leave" };
  const answer = await deps.ask(quitQuestion(sessions, deps.displayName));
  if (answer === null) return { backend: "host", cancelled: true };
  return { backend: "host", terminals: answer === "confirm" ? "leave" : "stop" };
}
