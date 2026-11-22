/**
 * Sending text to an agent from a page that is not the terminal (the Code
 * page's file viewer): open the dock, hand the text to the panel over the
 * launch-request channel, and say so in a toast when it does not arrive.
 *
 * The panel owns the sessions and the rule for choosing one, so this holds
 * neither. It owns only the outcome: a send that nobody received (no live
 * session, a request still waiting, a timeout, a cancel) must never look like
 * one that did.
 */

import { get } from "svelte/store";
import { repoStore } from "../stores/repoStore";
import { toastStore } from "../stores/toastStore";
import { formatError } from "../ui/formatError";
import { terminalLaunchRequests } from "./launchRequests";

export interface PageSendDeps {
  currentPath(): string | null;
  openDock(): void;
  request(repoPath: string, text: string, checkout: string): Promise<void>;
  report(message: string): void;
}

const liveDeps: PageSendDeps = {
  currentPath: () => get(repoStore).currentPath,
  openDock: () => repoStore.setTerminalOpen(true),
  request: (repoPath, text, checkout) => terminalLaunchRequests.requestReference(repoPath, text, checkout),
  report: (message) => toastStore.error(message),
};

/** Resolves true when the panel inserted `text`; false after reporting why it did not. */
export async function sendToAgentFromPage(text: string, checkout?: string, deps: PageSendDeps = liveDeps): Promise<boolean> {
  const repo = deps.currentPath();
  if (!repo) {
    deps.report("Open a repository first to send to an agent.");
    return false;
  }
  try {
    // Request first, then show the dock: the request waits for a panel that may still be loading.
    const sent = deps.request(repo, text, checkout || repo);
    deps.openDock();
    await sent;
    return true;
  } catch (cause) {
    deps.report(`Not sent to an agent: ${formatError(cause)}`);
    return false;
  }
}
