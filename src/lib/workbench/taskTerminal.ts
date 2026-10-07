/**
 * A prepared task attempt's terminal: starting it, and showing it. One
 * implementation, used by the handoff form after a launch and by the task's
 * Agents pane.
 *
 * Starting and showing are two decisions, and they used to be one. Every
 * launch opened the checkout as the active repository, brought the repository
 * surface forward and opened its dock — because only a shown dock could
 * start the process. So pressing Launch in a task sheet took the reader off
 * the sheet they had just used, and every further attempt from it meant
 * finding the task again. Now a launch starts the agent where it stands: the
 * checkout opens as a background tab, the dock hosts its panel unseen (see
 * `repoHosts.ts`), and the reader stays on the task, which lists the session
 * with a "Show terminal" for when they want it.
 *
 * The request is queued before the checkout is opened (see `taskLaunches.ts`
 * for why), so an open that is superseded by later navigation, or refused,
 * does not lose the terminal: it waits, and starts when that checkout opens.
 */

import { get } from "svelte/store";
import { interfaceStore } from "../stores/interfaceStore";
import { repoStore } from "../stores/repoStore";
import { enqueueTaskTerminal, type TaskTerminalRequest } from "../terminal/taskLaunches";
import { handOverDetachedRun, isAdoptedSession } from "../terminal/detachedSessions";
import { focusTerminalSession } from "../terminal/sessionFocus";
import { terminalSessions, type TerminalSessionRecord } from "../terminal/sessionRegistry";
import { findConversation, type TaskRun } from "./client";
import { resolveGitRoot } from "../desktop/nativeShell";
import { identityKey, isCaseInsensitiveFs, normalizeRepoPath } from "../repos/paths";
import { relativeStartDir } from "../terminal/tabs";
import { formatError } from "../ui/formatError";

/**
 * `opened`: on screen now. `started`: its checkout is open and the terminal
 * starts out of sight. `queued`: the checkout could not be opened, so the
 * terminal waits until it is.
 */
export type TaskTerminalOutcome = "opened" | "started" | "queued";

type RunRef = Pick<TaskRun, "id" | "cwd" | "provider" | "task_title">;

/**
 * Starts an attempt's terminal without changing what is on screen.
 */
export async function startTaskTerminal(run: RunRef): Promise<TaskTerminalOutcome> {
  const place = await checkoutFor(run.cwd);
  queueAttempt(run, place);
  return (await repoStore.openRepo(place.root, backgroundOpen())) ? "started" : "queued";
}

/** The checkout a directory belongs to, and where below its root the directory is. */
interface Placement {
  root: string;
  startDir?: string;
}

/**
 * Finds the checkout that holds `cwd`, through the same native walk a dropped
 * folder uses (`find_git_root`).
 *
 * An attempt's directory need not be a checkout root, and the repository
 * store opens only roots: opening a subdirectory was refused every time, so
 * the launch reported "queued" and waited for a tab that could never open.
 * When no checkout holds the directory at all, that is said now, and nothing
 * is queued — a request nothing can ever take is not "waiting".
 *
 * `exact` asks for the subdirectory to be known exactly (a resumed
 * conversation must start where it ran). When the host's canonical root and
 * the recorded directory are spelled through different links, the relative
 * part cannot be read off the two strings, and that is refused rather than
 * guessed.
 */
async function checkoutFor(cwd: string, exact = false): Promise<Placement> {
  let root: string;
  try {
    root = await resolveGitRoot(cwd);
  } catch (cause) {
    throw new Error(`No Git checkout contains ${cwd}, so its terminal cannot start. ${formatError(cause)}`);
  }
  const options = { caseInsensitive: isCaseInsensitiveFs() };
  const rootKey = identityKey(root, options);
  const cwdKey = identityKey(cwd, options);
  if (!rootKey) throw new Error(`No Git checkout contains ${cwd}, so its terminal cannot start.`);
  if (cwdKey === rootKey) return { root };
  const normalizedRoot = normalizeRepoPath(root) ?? root;
  const normalizedCwd = normalizeRepoPath(cwd) ?? cwd;
  const startDir = cwdKey.startsWith(`${rootKey}/`) ? relativeStartDir(normalizedCwd.slice(normalizedRoot.length + 1)) : null;
  if (startDir) return { root, startDir };
  if (exact) {
    throw new Error(`${cwd} is inside the checkout ${root}, but under a different spelling of its path, so the conversation cannot be started where it ran.`);
  }
  return { root };
}

/**
 * How to open a checkout without showing it.
 *
 * As a background tab — except when no repository is active. The dock lives
 * in the repository view, and App renders that view only while some
 * repository is current, so with none a background tab waits for a dock that
 * is never mounted and the agent never starts. Then the checkout becomes the
 * active repository instead: on the repository surface, which the reader is
 * not looking at, so they still stay on the task.
 */
function backgroundOpen(): { activate: boolean } {
  return { activate: get(repoStore).currentPath === null };
}

/**
 * Brings an attempt's terminal on screen — the one thing here that moves the
 * reader, and only ever on their request.
 *
 * A session already running is focused through `focusTerminalSession`, the
 * owner of the repository → dock → tab order. Anything else (not started
 * yet, or a session a reload left behind) is queued and its checkout opened
 * in front, where the panel finds the request and focuses or starts it.
 */
export async function showTaskTerminal(run: RunRef): Promise<TaskTerminalOutcome> {
  const live = liveAttemptSession(run.id);
  if (live) {
    const focused = await focusTerminalSession(live);
    if (focused.ok) return "opened";
  }
  const place = await checkoutFor(run.cwd);
  queueAttempt(run, place);
  return showCheckout(place.root);
}

/** Which way a resumed conversation should arrive. */
export type ResumeDisposition = "background" | "show";

/**
 * Picks an ended attempt's Claude Code conversation back up in a new tab in
 * its checkout, under the attempt's own permission mode.
 *
 * The host decides whether there is a conversation (from the transcript
 * Claude Code saved), so an attempt with nothing to resume is answered with
 * the reason rather than a tab that opens only to say "No conversation found".
 */
export async function resumeTaskConversation(
  run: Pick<TaskRun, "id" | "task_title">,
  disposition: ResumeDisposition = "background",
): Promise<{ outcome: TaskTerminalOutcome } | { outcome: "unavailable"; reason: string }> {
  const conversation = await findConversation(run.id);
  if (!conversation.resumable) return { outcome: "unavailable", reason: conversation.reason };
  if (disposition === "show") {
    const live = get(terminalSessions).find((record) => record.continuesRunId === run.id && record.reveal);
    if (live && (await focusTerminalSession(live)).ok) return { outcome: "opened" };
  }
  // Exact: Claude Code keeps the conversation under the directory it ran in.
  const place = await checkoutFor(conversation.cwd, true);
  const request: TaskTerminalRequest = {
    runId: run.id,
    repoPath: place.root,
    provider: "claude",
    title: run.task_title,
    resume: { sessionId: conversation.sessionId, mode: conversation.mode, runId: run.id },
    ...(place.startDir ? { startDir: place.startDir } : {}),
  };
  enqueueTaskTerminal(request);
  if (disposition === "show") return { outcome: await showCheckout(place.root) };
  return { outcome: (await repoStore.openRepo(place.root, backgroundOpen())) ? "started" : "queued" };
}

/**
 * The renderer session running this attempt's own process, if one is.
 * An adopted record from before a reload is not one: its "reveal" hands the
 * process to a new tab, which is `showTaskTerminal`'s queued path.
 */
function liveAttemptSession(runId: string): TerminalSessionRecord | undefined {
  return get(terminalSessions).find(
    (record) => record.taskRunId === runId && !isAdoptedSession(record) && record.reveal,
  );
}

function queueAttempt(run: RunRef, place: Placement): void {
  enqueueTaskTerminal({
    runId: run.id, repoPath: place.root, provider: run.provider, title: run.task_title,
    ...(place.startDir ? { startDir: place.startDir } : {}),
  });
  // A session a reloaded page left running for this attempt gives up its
  // adopted record now, before the tab that takes the same process over
  // reserves a slot. Queued first, so the request that stands is this one.
  handOverDetachedRun(run.id);
}

async function showCheckout(cwd: string): Promise<TaskTerminalOutcome> {
  let ready = false;
  const opened = await repoStore.openRepo(cwd, {
    onReady: () => {
      ready = true;
      interfaceStore.setGlobalSurface("repository");
      repoStore.setTerminalOpen(true);
    },
  });
  return opened && ready ? "opened" : "queued";
}

/** What to tell the reader when the terminal is waiting rather than started. */
export function queuedTerminalNote(cwd: string): string {
  const name = cwd.split(/[\\/]/).filter(Boolean).pop() ?? cwd;
  return `The agent's terminal is waiting for ${name} to open in GitPulse, and starts as soon as it does. Show terminal opens it now — and says why, if it cannot be opened.`;
}
