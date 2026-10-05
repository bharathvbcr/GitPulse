/**
 * Showing a prepared task attempt's terminal. One implementation, used by the
 * handoff form after a launch and by the run history's "Open terminal".
 *
 * The request is queued before the checkout is opened (see `taskLaunches.ts`
 * for why), so an open that is superseded by later navigation no longer loses
 * the terminal: the outcome is `queued`, and the tab appears when that
 * repository's dock is next shown. Both callers used to enqueue from the
 * open's ready callback and throw when it never ran, leaving a prepared
 * attempt holding its checkout with no terminal coming.
 */

import { interfaceStore } from "../stores/interfaceStore";
import { repoStore } from "../stores/repoStore";
import { enqueueTaskTerminal } from "../terminal/taskLaunches";
import { findConversation, type TaskRun } from "./client";

export type TaskTerminalOutcome = "opened" | "queued";

export async function openTaskTerminal(
  run: Pick<TaskRun, "id" | "cwd" | "provider" | "task_title">,
): Promise<TaskTerminalOutcome> {
  enqueueTaskTerminal({ runId: run.id, repoPath: run.cwd, provider: run.provider, title: run.task_title });
  return showCheckout(run.cwd);
}

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
): Promise<{ outcome: TaskTerminalOutcome } | { outcome: "unavailable"; reason: string }> {
  const conversation = await findConversation(run.id);
  if (!conversation.resumable) return { outcome: "unavailable", reason: conversation.reason };
  enqueueTaskTerminal({
    runId: run.id,
    repoPath: conversation.cwd,
    provider: "claude",
    title: run.task_title,
    resume: { sessionId: conversation.sessionId, mode: conversation.mode },
  });
  return { outcome: await showCheckout(conversation.cwd) };
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

/** What to tell the reader when the terminal is waiting rather than shown. */
export function queuedTerminalNote(cwd: string): string {
  const name = cwd.split(/[\\/]/).filter(Boolean).pop() ?? cwd;
  return `The agent's terminal is queued and opens in ${name}'s terminal dock when you switch to it.`;
}
