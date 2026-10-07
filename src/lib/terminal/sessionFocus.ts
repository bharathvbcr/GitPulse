/**
 * Jumping to a terminal session that is running in another repository.
 *
 * The Sessions list has always been able to *name* every shell across every
 * open repository — it is how the global `MAX_PTY_SESSIONS` budget is spent —
 * but the only thing it could do with one was kill it. Getting back to a shell
 * in another repository meant finding that repository's tab by eye, opening
 * its dock, and hunting the right terminal tab.
 *
 * Three things have to happen, in this order, and each is a different owner's
 * job: the repository tab has to come forward (repoStore), its dock has to be
 * showing (repoStore, per tab since the decoupling), and the panel has to
 * select that session's tab and focus it (the panel, via `record.reveal`).
 * Doing them out of order reveals into a hidden panel and shows nothing.
 *
 * Kept as one function rather than inlined at the call site because the order
 * is the whole contract, and a second caller getting it subtly wrong is how a
 * "jump" silently becomes a no-op.
 */

import { tick } from "svelte";
import { get } from "svelte/store";
import { interfaceStore } from "../stores/interfaceStore";
import { repoStore } from "../stores/repoStore";
import type { TerminalSessionRecord } from "./sessionRegistry";
import { isCaseInsensitiveFs, sameRepo, type PathIdentityOptions } from "../repos/paths";

/** What `focusTerminalSession` needs from the repository store. */
export interface RepoFocusTarget {
  openTabs: readonly { id: string; path: string }[];
  activeTabId: string | null;
}

export interface RepoFocusActions {
  /**
   * Brings the repository surface forward. The dock lives there, so a jump
   * made from Tasks or Fleet — an alert clicked while reading the board —
   * otherwise revealed into a surface nobody could see.
   */
  showRepositorySurface(): void;
  snapshot(): RepoFocusTarget;
  activateTab(id: string): Promise<void> | void;
  setTerminalOpen(open: boolean): boolean;
  /** Resolves a path that is not currently an open tab. */
  openRepo(path: string): Promise<boolean> | boolean;
  /** Defers to the next render so the revealed panel is on screen first. */
  afterRender(): Promise<void>;
  /**
   * How checkouts are compared — the repository store's rule. Absent means
   * the host's own volume rule, which is what the store defaults to.
   */
  identity?: PathIdentityOptions;
}

export type FocusOutcome =
  | { ok: true; switchedRepo: boolean; openedDock: boolean }
  | { ok: false; reason: "no-session" | "unavailable" };

/**
 * Brings `record`'s shell on screen, wherever it is running.
 *
 * Returns what it had to do rather than void: a caller that reports "switched
 * to <repo>" must not say so when the session was already in front, and a
 * failure has to be distinguishable from a no-op.
 */
export async function focusTerminalSession(
  record: Pick<TerminalSessionRecord, "repoPath" | "reveal"> | null | undefined,
  repo: RepoFocusActions = repoStoreFocus,
): Promise<FocusOutcome> {
  if (!record) return { ok: false, reason: "no-session" };
  // Checked BEFORE anything moves. A record with no reveal can never be shown,
  // so switching repository tabs and opening a dock on the way to discovering
  // that leaves the user somewhere they did not ask to be — and opening a dock
  // is what starts a shell.
  if (!record.reveal) return { ok: false, reason: "unavailable" };

  repo.showRepositorySurface();
  const before = repo.snapshot();
  // By checkout identity, as the tab strip matches: the same checkout spelled
  // with another case or a trailing separator is this tab, not a reason to
  // open a second one.
  const identity = repo.identity ?? { caseInsensitive: isCaseInsensitiveFs() };
  const target = before.openTabs.find((tab) => sameRepo(tab.path, record.repoPath, identity));
  let switchedRepo = false;

  if (target) {
    if (target.id !== before.activeTabId) {
      await repo.activateTab(target.id);
      switchedRepo = true;
    }
  } else {
    // A live session whose repository tab is gone should be impossible —
    // closing the tab disposes the panel and kills its shells. Reopening is
    // the honest fallback rather than pretending the jump worked. The path is
    // a checkout root by construction, so it needs no root resolution: a
    // panel's record carries the tab path it was mounted on, and an adopted
    // one carries the host's session repo, which it only spawned after
    // `validate_repo` accepted it as a checkout.
    const opened = await repo.openRepo(record.repoPath);
    if (!opened) return { ok: false, reason: "unavailable" };
    switchedRepo = true;
  }

  // Per repository tab, so this has to run AFTER the switch: before it, it
  // would open the dock on whichever repository the user was leaving.
  const openedDock = repo.setTerminalOpen(true);

  // The panel may have been hidden (or not yet hosted) a moment ago; let it
  // render before asking it to select and focus a tab.
  await repo.afterRender();
  record.reveal();
  return { ok: true, switchedRepo, openedDock };
}

/**
 * The app's own stores, as every caller in the app uses them: the dock's
 * Sessions list and a clicked session alert. One definition, so the two
 * cannot drift into doing the steps differently.
 */
export const repoStoreFocus: RepoFocusActions = {
  showRepositorySurface: () => interfaceStore.setGlobalSurface("repository"),
  snapshot: () => {
    const state = get(repoStore);
    return { openTabs: state.openTabs, activeTabId: state.activeTabId };
  },
  activateTab: (id) => repoStore.activateTab(id),
  setTerminalOpen: (open) => repoStore.setTerminalOpen(open),
  openRepo: (path) => repoStore.openRepo(path),
  afterRender: () => tick(),
};
