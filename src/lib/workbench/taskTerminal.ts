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
  queueAttempt(run);
  return (await repoStore.openRepo(run.cwd, backgroundOpen())) ? "started" : "queued";
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
  queueAttempt(run);
  return showCheckout(run.cwd);
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
  const request: TaskTerminalRequest = {
    runId: run.id,
    repoPath: conversation.cwd,
    provider: "claude",
    title: run.task_title,
    resume: { sessionId: conversation.sessionId, mode: conversation.mode, runId: run.id },
  };
  if (disposition === "show") {
    const live = get(terminalSessions).find((record) => record.continuesRunId === run.id && record.reveal);
    if (live && (await focusTerminalSession(live)).ok) return { outcome: "opened" };
    enqueueTaskTerminal(request);
    return { outcome: await showCheckout(conversation.cwd) };
  }
  enqueueTaskTerminal(request);
  return { outcome: (await repoStore.openRepo(conversation.cwd, backgroundOpen())) ? "started" : "queued" };
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

function queueAttempt(run: RunRef): void {
  enqueueTaskTerminal({ runId: run.id, repoPath: run.cwd, provider: run.provider, title: run.task_title });
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
