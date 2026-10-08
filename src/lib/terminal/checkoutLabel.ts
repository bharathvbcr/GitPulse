/**
 * What to call the checkout a terminal or a task attempt runs in: the
 * repository, and which of its checkouts.
 *
 * A path's last segment says neither. Every agent worktree of `app` ends in a
 * session slug, and the primary checkout ends in `app`, so a list naming each
 * row by its basename put `fix-importer-a1b2c3d4` beside `app` with nothing
 * saying they are one repository — and two repositories that are both called
 * `api` read the same.
 *
 * The naming itself is `repos/repoFamily.ts::checkoutName`, the rule the tab
 * strip's stacks use; this only supplies its inputs from the open tab on the
 * checkout (its label and its repository family), so a row and the tab it
 * would switch to name the checkout the same way. A checkout with no open tab
 * still gets a repository name when its path is an agent worktree, whose
 * layout puts the repository in front of `.<agent>/worktrees`; otherwise the
 * repository is not known and is not guessed.
 */

import { agentLayout } from "../work/agentWorktree";
import { checkoutName, familyName } from "../repos/repoFamily";
import { displayName, identityKey, normalizeRepoPath, sameRepo, type PathIdentityOptions } from "../repos/paths";
import { launcherLabel, type LauncherKind } from "./tabs";

/** The fields of an open repository tab this reads. */
export interface CheckoutTab {
  path: string;
  label: string;
  familyRoot?: string | null;
}

export interface CheckoutLabel {
  /** The repository's name, or the checkout's own when no repository is known. */
  repository: string;
  /** The checkout among its repository's, or null when that is the repository itself. */
  checkout: string | null;
  /** The checkout holds the repository's `.git`. False when unknown. */
  primary: boolean;
  /** Agent whose worktree this is (`claude`, `gitpulse`, …), or empty. */
  worktreeAgent: string;
}

/** The repository directory in front of an agent layout, or null. */
function layoutRoot(path: string): string | null {
  const layout = agentLayout(path);
  const normalized = normalizeRepoPath(path);
  if (!layout || !normalized) return null;
  const marker = `/.${layout.kind}/worktrees`;
  const at = normalized.indexOf(marker);
  return at > 0 ? normalizeRepoPath(normalized.slice(0, at)) : null;
}

export function describeCheckout(
  path: string,
  tabs: readonly CheckoutTab[],
  options: PathIdentityOptions,
): CheckoutLabel {
  const identity = (candidate: string) => identityKey(candidate, options);
  const key = identity(path);
  const tab = key ? tabs.find((candidate) => identity(candidate.path) === key) : undefined;
  const root = (tab?.familyRoot ? normalizeRepoPath(tab.familyRoot) : null) ?? layoutRoot(path);
  const label = tab?.label || displayName(path);
  const named = checkoutName(path, label, root ?? "", identity);
  const repository = root ? familyName(root) : label;
  // The primary checkout is the repository itself; naming it twice is noise.
  const checkout = named.primary || named.name === repository ? null : named.name;
  return { repository, checkout, primary: named.primary, worktreeAgent: named.agent };
}

/** The pieces of one row in the terminal's Sessions list. */
export interface SessionRow extends CheckoutLabel {
  /** The launcher's name when it is an agent CLI, for a chip; null for a shell. */
  agent: string | null;
  /** The session runs in the checkout this panel is bound to. */
  here: boolean;
  /** The task attempt this session is, or continues; null when neither. */
  taskRunId: string | null;
}

export function sessionRow(
  session: { repoPath: string; checkout?: string; launcher?: LauncherKind; taskRunId?: string; continuesRunId?: string },
  panelPath: string | null,
  tabs: readonly CheckoutTab[],
  options: PathIdentityOptions,
): SessionRow {
  const here = !!panelPath && sameRepo(panelPath, session.repoPath, options);
  return {
    // Named by where it runs; `here` stays the panel that holds it.
    ...describeCheckout(session.checkout ?? session.repoPath, tabs, options),
    agent: session.launcher && session.launcher !== "shell" ? launcherLabel(session.launcher) : null,
    here,
    taskRunId: session.taskRunId ?? session.continuesRunId ?? null,
  };
}
