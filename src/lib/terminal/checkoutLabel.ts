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

interface ResolvedCheckout {
  label: CheckoutLabel;
  /**
   * Identity of the repository this checkout belongs to. When no repository
   * is known, the checkout's own identity, so two directories that happen to
   * share a name stay apart.
   */
  groupKey: string;
}

function resolveCheckout(
  path: string,
  tabs: readonly CheckoutTab[],
  options: PathIdentityOptions,
): ResolvedCheckout {
  const identity = (candidate: string) => identityKey(candidate, options);
  const key = identity(path);
  const tab = key ? tabs.find((candidate) => identity(candidate.path) === key) : undefined;
  const root = (tab?.familyRoot ? normalizeRepoPath(tab.familyRoot) : null) ?? layoutRoot(path);
  const label = tab?.label || displayName(path);
  const named = checkoutName(path, label, root ?? "", identity);
  const repository = root ? familyName(root) : label;
  // The primary checkout is the repository itself; naming it twice is noise.
  const checkout = named.primary || named.name === repository ? null : named.name;
  return {
    label: { repository, checkout, primary: named.primary, worktreeAgent: named.agent },
    groupKey: (root ? identity(root) : key) || repository,
  };
}

export function describeCheckout(
  path: string,
  tabs: readonly CheckoutTab[],
  options: PathIdentityOptions,
): CheckoutLabel {
  return resolveCheckout(path, tabs, options).label;
}

/** One repository's sessions in the cross-repository Sessions list. */
export interface SessionListGroup<T> {
  /** Repository identity. Stable across spelling and across its checkouts. */
  key: string;
  repository: string;
  /** This panel's checkout belongs to the repository. */
  here: boolean;
  sessions: T[];
}

/**
 * Group a session list by repository.
 *
 * The flat list repeated the repository on every row and left the checkout
 * path to a tooltip. Rows keep the order they arrived in. The panel's own
 * repository comes first; the rest follow by name, and a shared name does
 * not merge two repositories.
 */
export function groupSessionsByRepository<T extends { repoPath: string; checkout?: string }>(
  sessions: readonly T[],
  panelPath: string | null,
  tabs: readonly CheckoutTab[],
  options: PathIdentityOptions,
): SessionListGroup<T>[] {
  if (!Array.isArray(sessions)) return [];
  const panelKey = panelPath ? resolveCheckout(panelPath, tabs, options).groupKey : "";
  const order: string[] = [];
  const groups = new Map<string, SessionListGroup<T>>();
  for (const session of sessions) {
    if (!session || typeof session.repoPath !== "string" || session.repoPath.length === 0) continue;
    const resolved = resolveCheckout(session.checkout || session.repoPath, tabs, options);
    let group = groups.get(resolved.groupKey);
    if (!group) {
      group = {
        key: resolved.groupKey,
        repository: resolved.label.repository,
        here: panelKey !== "" && panelKey === resolved.groupKey,
        sessions: [],
      };
      groups.set(resolved.groupKey, group);
      order.push(resolved.groupKey);
    }
    group.sessions.push(session);
  }
  return order
    .map((key) => groups.get(key)!)
    .sort((a, b) => {
      if (a.here !== b.here) return a.here ? -1 : 1;
      const name = a.repository.localeCompare(b.repository, undefined, { sensitivity: "base" });
      if (name !== 0) return name;
      return a.key < b.key ? -1 : a.key > b.key ? 1 : 0;
    });
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
