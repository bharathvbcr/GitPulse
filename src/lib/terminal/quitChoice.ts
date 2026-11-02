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
 *
 * Managed agent runs (Codex and Claude driven by the Manvi sidecar) are a
 * different kind of work: no terminal host keeps them. Quitting closes the
 * sidecar, and the sidecar cancels every run it holds, with either terminal
 * backend. `decideQuitManagedRuns` asks about them on every quit.
 */

import { invoke as tauriInvoke } from "../ipc/invoke";
import type { TaskRun } from "../workbench/client";
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

/** A managed run the sidecar's shutdown cancels: it has started, or may have, and has not ended. */
const WORKING_MANAGED_STATES: readonly string[] = ["starting", "running", "unresolved"];

export function isWorkingManagedRun(run: Pick<TaskRun, "kind" | "state">): boolean {
  return run.kind === "managed" && WORKING_MANAGED_STATES.includes(run.state);
}

/** The two-way question: `true` stops them and quits, `false` keeps working. */
export interface QuitStopQuestion {
  title: string;
  message: string;
  confirmLabel: string;
  cancelLabel: string;
  destructive: true;
}

export type AskStop = (question: QuitStopQuestion) => Promise<boolean>;

/**
 * The question for quitting over managed runs. `runs` are the working ones
 * read, or null when the list could not be read at all; `complete` false says
 * the list was cut short, so the count is a floor.
 */
export function quitManagedQuestion(
  runs: readonly Pick<TaskRun, "provider" | "task_title" | "cwd">[] | null,
  complete: boolean,
  displayName: (path: string) => string = (path) => path,
): QuitStopQuestion {
  const count = runs?.length ?? null;
  const listed = (runs ?? [])
    .slice(0, LISTED)
    .map((run) => `• ${run.provider} — ${run.task_title} — ${displayName(run.cwd)}`)
    .join("\n");
  const omitted = count !== null && count > LISTED ? `\n…and ${count - LISTED} more` : "";
  const noun = (n: number) => (n === 1 ? "managed run" : "managed runs");
  let what: string;
  let title: string;
  if (count === null) {
    what = "Managed agent runs may still be working (GitPulse could not list them).";
    title = "Quit While Managed Runs May Be Working?";
  } else if (!complete) {
    what = count === 0
      ? "The list of managed agent runs could not be read in full, so some may still be working."
      : `The list of managed agent runs could not be read in full, so at least ${count} ${noun(count)} will be stopped:`;
    title = "Quit While Managed Runs May Be Working?";
  } else {
    what = `${count} ${noun(count)} will be stopped:`;
    title = count === 1 ? "Quit While a Managed Run Is Working?" : `Quit While ${count} Managed Runs Are Working?`;
  }
  return {
    title,
    message: `${what}${listed ? `\n${listed}${omitted}` : ""}\nQuitting GitPulse closes the agent runtime, which stops every managed run and whatever it is in the middle of. A terminal host does not keep them.`,
    confirmLabel: "Stop and Quit",
    cancelLabel: "Keep Working",
    destructive: true,
  };
}

/** What the quit does about managed runs: `stopping` is how many were counted, null when they could not be listed. */
export type QuitManagedDecision = { cancelled: true } | { cancelled: false; stopping: number | null };

/**
 * Decides whether a quit may stop the managed runs. Asks when one is working,
 * when the list was cut short, or when it could not be read: none of those is
 * "nothing is running". It does not depend on the terminal backend, because a
 * managed run dies on quit with the terminal host on or off.
 */
export async function decideQuitManagedRuns(deps: {
  ask: AskStop;
  listRuns: () => Promise<{ runs: readonly TaskRun[]; complete: boolean }>;
  displayName?: (path: string) => string;
  onError?: (what: string, error: unknown) => void;
}): Promise<QuitManagedDecision> {
  let working: readonly TaskRun[] | null;
  let complete = true;
  try {
    const live = await deps.listRuns();
    working = live.runs.filter(isWorkingManagedRun);
    complete = live.complete;
  } catch (error) {
    (deps.onError ?? (() => {}))("the managed runs", error);
    working = null;
  }
  if (working !== null && working.length === 0 && complete) return { cancelled: false, stopping: 0 };
  const stop = await deps.ask(quitManagedQuestion(working, complete, deps.displayName));
  return stop ? { cancelled: false, stopping: working?.length ?? null } : { cancelled: true };
}
