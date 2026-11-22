/**
 * Which live session a reference or a block of code goes to, and what the
 * sender is told about it.
 *
 * One rule, used twice. The terminal panel applies it at the moment of sending
 * (over its own tabs, asking each session whether it is live) and a file viewer
 * applies it to draw the Send button's label (over the candidates the panel
 * last published). Two copies of a rule drift, and the label would then name a
 * session the text did not go to.
 *
 * The rule, in order: the session the person picked for this repository, if it
 * is still live; else a live session in the file's own checkout, an agent
 * before a shell and a visible one before a hidden one; else the same ranking
 * over every checkout. A reference sent to an agent working in another
 * worktree names a file that may not exist there, so the checkout leads.
 *
 * In memory only: nothing here is persisted, and a restart forgets every
 * choice, which is right because the sessions it named are gone too.
 */

import { get, writable } from "svelte/store";
import { displayName, isCaseInsensitiveFs, sameRepo } from "../repos/paths";
import { isPromptLauncher } from "./launchRequests";
import type { TabKind } from "./tabs";

export interface RecipientCandidate {
  /** The terminal tab id. */
  id: string;
  launcher: TabKind;
  /** The checkout (worktree path) the session runs in. */
  checkout: string;
  /** The session is running: not exited, not errored. */
  live: boolean;
  /** The session's pane is on screen. */
  visible: boolean;
}

/** Sessions text can be typed into: not a browser, not a file pane. */
export function eligibleRecipients(candidates: readonly RecipientCandidate[]): RecipientCandidate[] {
  return candidates.filter((c) => c.live && c.launcher !== "browser" && c.launcher !== "file");
}

/**
 * The session to send to, or null when none is eligible. `stickyId` is the
 * person's earlier pick; it is honoured only while that session is still
 * eligible, so a session that ended hands the choice back to the rule.
 */
export function pickRecipient(
  candidates: readonly RecipientCandidate[],
  options: { checkout: string | null; stickyId?: string | null },
): RecipientCandidate | null {
  const eligible = eligibleRecipients(candidates);
  const sticky = options.stickyId ? eligible.find((c) => c.id === options.stickyId) : undefined;
  if (sticky) return sticky;
  const here = (c: RecipientCandidate) =>
    options.checkout !== null && sameRepo(c.checkout, options.checkout, { caseInsensitive: isCaseInsensitiveFs() });
  const rank = (c: RecipientCandidate) =>
    (here(c) ? 0 : 4) + (isPromptLauncher(c.launcher) ? 0 : 2) + (c.visible ? 0 : 1);
  // Array.prototype.sort is stable: equal ranks keep tab order.
  return [...eligible].sort((a, b) => rank(a) - rank(b))[0] ?? null;
}

/** "claude · feature-x": the session and the checkout it works in. */
export function recipientLabel(candidate: RecipientCandidate): string {
  return `${candidate.launcher} · ${displayName(candidate.checkout)}`;
}

/** What a viewer shows for a repository: the session text will go to, and every one it could go to. */
export interface RecipientView {
  chosen: RecipientCandidate | null;
  /** Every eligible session, in tab order; the picker lists these when there is more than one. */
  options: RecipientCandidate[];
}

export function recipientView(
  candidates: readonly RecipientCandidate[],
  options: { checkout: string | null; stickyId?: string | null },
): RecipientView {
  return { chosen: pickRecipient(candidates, options), options: eligibleRecipients(candidates) };
}

/** The candidates each open terminal panel last published, by repository path. */
export const recipientDirectory = writable<Readonly<Record<string, readonly RecipientCandidate[]>>>({});
/** The session the person picked, by repository path. */
export const recipientChoice = writable<Readonly<Record<string, string>>>({});

/** Published by the panel whenever its tabs or their liveness change. */
export function publishRecipients(repoPath: string, candidates: readonly RecipientCandidate[]): void {
  recipientDirectory.update((all) => ({ ...all, [repoPath]: candidates }));
  // A pick that is no longer eligible is dropped, so it cannot revive if the id is ever reused.
  const sticky = get(recipientChoice)[repoPath];
  if (sticky && !eligibleRecipients(candidates).some((c) => c.id === sticky)) chooseRecipient(repoPath, null);
}

/** The panel for `repoPath` is gone: its sessions can no longer be sent to. */
export function forgetRecipients(repoPath: string): void {
  recipientDirectory.update(({ [repoPath]: _gone, ...rest }) => rest);
  chooseRecipient(repoPath, null);
}

/** Remember the person's pick for `repoPath`, or forget it with null. */
export function chooseRecipient(repoPath: string, id: string | null): void {
  recipientChoice.update(({ [repoPath]: _old, ...rest }) => (id ? { ...rest, [repoPath]: id } : rest));
}
