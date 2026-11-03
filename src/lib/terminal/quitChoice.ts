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
 * different kind of work: the terminal host does not keep them. By default
 * quitting closes the sidecar, and the sidecar cancels every run it holds, with
 * either terminal backend, so `decideQuitManagedRuns` asks about them on every
 * quit. When the person has chosen to keep agents running and the installed
 * Manvi can host them (`ManagedHosting.hosted`), a hosted Manvi holds the runs
 * instead and the question becomes the terminals' three-way one: leave them,
 * stop them, or keep working.
 */

import { invoke as tauriInvoke } from "../ipc/invoke";
import type { TaskRun } from "../workbench/client";
import { parseListing } from "./detachedSessions";

type Invoke = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

/** `ManagedHosting` in `harness::hosted`: what becomes of managed runs on a quit, and why. */
export interface ManagedHosting {
  /** The person chose to keep agents running. */
  requested: boolean;
  /** A hosted Manvi holds the managed runs right now, so they survive a quit. */
  hosted: boolean;
  /** One plain sentence, saying why when the choice is not in effect. */
  detail: string;
}

/** `cmd_terminal_backend`. Mirrors `terminal::TerminalBackendStatus`. */
export interface TerminalBackendStatus {
  backend: "host" | "in-process";
  /** What is wrong with the host, plainly. Empty when nothing is known to be. */
  trouble: string[];
  /** Absent when the backend did not say, which is not "ended with the app". */
  managed?: ManagedHosting;
}

export function parseManagedHosting(raw: unknown): ManagedHosting | null {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const row = raw as Record<string, unknown>;
  if (typeof row.requested !== "boolean" || typeof row.hosted !== "boolean" || typeof row.detail !== "string") return null;
  return { requested: row.requested, hosted: row.hosted, detail: row.detail.slice(0, 600) };
}

export function parseBackendStatus(raw: unknown): TerminalBackendStatus | null {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const row = raw as Record<string, unknown>;
  if (row.backend !== "host" && row.backend !== "in-process") return null;
  if (!Array.isArray(row.trouble) || !row.trouble.every((t) => typeof t === "string")) return null;
  const status: TerminalBackendStatus = { backend: row.backend, trouble: row.trouble.slice(0, 16) };
  if (row.managed !== undefined) {
    // A present but unreadable account is a malformed answer, not an absent one.
    const managed = parseManagedHosting(row.managed);
    if (!managed) return null;
    status.managed = managed;
  }
  return status;
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

/** `cmd_exit_app`'s `managed`: what a hosted Manvi does with its runs on the way out. */
export type QuitManaged = "stop" | "leave";

/**
 * What the quit does about managed runs: `stopping` is how many were counted
 * as ending, null when they could not be listed. `managed` is the choice for a
 * hosted Manvi's runs, present only when one holds them and the person was
 * asked.
 */
export type QuitManagedDecision =
  | { cancelled: true }
  | { cancelled: false; stopping: number | null; managed?: QuitManaged };

/**
 * The three-way question for runs a hosted Manvi keeps: `confirm` leaves
 * them, `alternate` stops them. `runs` and `complete` are as for
 * [`quitManagedQuestion`].
 */
export function quitManagedHostedQuestion(
  runs: readonly Pick<TaskRun, "provider" | "task_title" | "cwd">[] | null,
  complete: boolean,
  displayName: (path: string) => string = (path) => path,
): QuitQuestion {
  const count = runs?.length ?? null;
  const listed = (runs ?? [])
    .slice(0, LISTED)
    .map((run) => `• ${run.provider} — ${run.task_title} — ${displayName(run.cwd)}`)
    .join("\n");
  const omitted = count !== null && count > LISTED ? `\n…and ${count - LISTED} more` : "";
  const noun = (n: number) => (n === 1 ? "managed run is" : "managed runs are");
  let what: string;
  if (count === null) what = "Managed agent runs may still be working (GitPulse could not list them).";
  else if (!complete) {
    what = count === 0
      ? "The list of managed agent runs could not be read in full, so some may still be working."
      : `The list of managed agent runs could not be read in full, so at least ${count} ${noun(count)} still working:`;
  } else what = `${count} ${noun(count)} still working:`;
  const plural = count !== 1;
  return {
    title: count === 1 ? "Leave the Managed Run Working?" : "Leave the Managed Runs Working?",
    message: `${what}${listed ? `\n${listed}${omitted}` : ""}\nLeft running, ${plural ? "they keep" : "it keeps"} working while GitPulse is closed, and the next launch picks ${plural ? "them" : "it"} up again. Stopped, ${plural ? "they end" : "it ends"} now, with whatever ${plural ? "they are" : "it is"} in the middle of.`,
    confirmLabel: "Quit and Leave Running",
    alternateLabel: count === 1 ? "Stop It and Quit" : "Stop Them and Quit",
    cancelLabel: "Keep Working",
  };
}

/**
 * Decides what a quit does with the managed runs. Asks when one is working,
 * when the list was cut short, or when it could not be read: none of those is
 * "nothing is running". It does not depend on the terminal backend.
 *
 * Without a hosted Manvi (`hosted` absent or false) a managed run dies on quit,
 * so the question is the two-way one with `ask`. With one the runs can outlive
 * the app, and the question is the three-way `askHosted`; its answer is
 * `managed`. A `hosted` that cannot be read is read as "not hosted", whose
 * question says the runs end — the warning that cannot do harm if it is wrong.
 *
 * "Read in full" is judged over the runs that matter to a quit — starting,
 * running and unresolved ones — not over `prepared` rows, which are not
 * working and can be a long backlog that must not make every quit say the
 * list was cut short.
 */
export async function decideQuitManagedRuns(deps: {
  ask: AskStop;
  askHosted?: Ask;
  hosted?: () => Promise<boolean>;
  listRuns: () => Promise<{ runs: readonly TaskRun[]; complete: boolean; workingComplete?: boolean }>;
  displayName?: (path: string) => string;
  onError?: (what: string, error: unknown) => void;
}): Promise<QuitManagedDecision> {
  const report = deps.onError ?? (() => {});
  let working: readonly TaskRun[] | null;
  let complete = true;
  try {
    const live = await deps.listRuns();
    working = live.runs.filter(isWorkingManagedRun);
    complete = live.workingComplete ?? live.complete;
  } catch (error) {
    report("the managed runs", error);
    working = null;
  }
  if (working !== null && working.length === 0 && complete) return { cancelled: false, stopping: 0 };
  let hosted = false;
  if (deps.hosted && deps.askHosted) {
    try {
      hosted = await deps.hosted();
    } catch (error) {
      report("whether a hosted Manvi keeps the managed runs", error);
    }
  }
  if (hosted && deps.askHosted) {
    const answer = await deps.askHosted(quitManagedHostedQuestion(working, complete, deps.displayName));
    if (answer === null) return { cancelled: true };
    return answer === "confirm"
      ? { cancelled: false, stopping: 0, managed: "leave" }
      : { cancelled: false, stopping: working?.length ?? null, managed: "stop" };
  }
  const stop = await deps.ask(quitManagedQuestion(working, complete, deps.displayName));
  return stop ? { cancelled: false, stopping: working?.length ?? null } : { cancelled: true };
}
