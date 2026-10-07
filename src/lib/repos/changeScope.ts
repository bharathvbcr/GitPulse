/**
 * Which views a `repo-changed` event may cost a refresh.
 *
 * Every event used to reach every view: a `git fetch` (remote refs and
 * objects, nothing in the checkout) still re-measured line counts and
 * coverage, rebuilt the code index and re-read the document vault. The
 * watcher now says what moved ({@link RepoChange}); each view below declares
 * what it is derived from, and an event that touches none of it is not its
 * refresh.
 *
 * Two rules keep this from hiding a real change:
 *
 * - **`index` counts as content.** Every git command that rewrites tracked
 *   files — checkout, reset, merge, stash, rebase — writes the index, and the
 *   worktree watch is non-recursive, so a deep file rewritten by git often
 *   surfaces as nothing else. A commit writes it too; that costs a refresh,
 *   not a missed one.
 * - **No change, or `unknown`, is everything.** A payload this build cannot
 *   read, or a backend error the watcher could not classify, refreshes every
 *   view, as every event did before.
 *
 * Repository state (status, branches, graph) depends on all of it and is not
 * routed here: `repoStore` refreshes on every event, under the scope rules in
 * `watcherRefresh.ts`. Scope deferral for the other views stays where it is —
 * the metric cells, the code-index queue and the docs queue each already wait
 * for their repository to be visible and active.
 */

import type { ChangeKind, RepoChange } from "./events";

export type ChangeDependency = (change: RepoChange) => boolean;

/** Every kind this build understands; checked against `ChangeKind` in Rust. */
export const CHANGE_KINDS: readonly ChangeKind[] = [
  "refs", "index", "config", "ignore", "objects", "git_state", "worktree", "documents", "unknown",
];

const KINDS: ReadonlySet<string> = new Set(CHANGE_KINDS);

function isChangeKind(value: unknown): value is ChangeKind {
  return typeof value === "string" && KINDS.has(value);
}

function isString(value: unknown): value is string {
  return typeof value === "string";
}

function dependsOn(...kinds: ChangeKind[]): ChangeDependency {
  return (change) => change.kinds.some((kind) => kind === "unknown" || kinds.includes(kind));
}

/** Derived from the checkout's files: line counts, coverage, the code index. */
export const dependsOnWorktreeContent = dependsOn("worktree", "index", "ignore");

/**
 * The document vault: tracked Markdown. The watcher marks an entry that is,
 * or may contain, a document (`documents`); the index decides what is tracked.
 */
export const dependsOnDocuments = dependsOn("documents", "index", "ignore");

/** Disk usage covers the checkout and the git directory alike. */
export const dependsOnStorage = dependsOn("worktree", "index", "ignore", "objects", "git_state");

/**
 * The payload's change, or null when it carries none this build can trust.
 * Null is not "nothing changed": the router refreshes every view for it.
 */
export function readRepoChange(value: unknown): RepoChange | null {
  if (typeof value !== "object" || value === null) return null;
  const { kinds, paths, paths_truncated } = value as Record<string, unknown>;
  if (!Array.isArray(kinds) || kinds.length === 0) return null;
  if (!kinds.every(isChangeKind)) return null;
  if (!Array.isArray(paths) || !paths.every(isString)) return null;
  if (typeof paths_truncated !== "boolean") return null;
  return { kinds, paths, paths_truncated };
}

export interface RepoChangeTargets {
  /** Repository state: every event. No path means the current repository. */
  repoState(path: string | undefined): void;
  /** Each metric applies its own dependency to `change` (null: all). */
  metrics(path: string, change: RepoChange | null): void;
  codeIndex(path: string): void;
  docs(path: string): void;
}

/**
 * Routes one event. `blocked` is a repository awaiting trust: its state
 * refresh runs (it reports the block), nothing derived from its files does.
 */
export function routeRepoChange(
  path: string | undefined,
  change: RepoChange | null,
  blocked: boolean,
  targets: RepoChangeTargets,
): void {
  targets.repoState(path);
  if (!path || blocked) return;
  targets.metrics(path, change);
  if (!change || dependsOnWorktreeContent(change)) targets.codeIndex(path);
  if (!change || dependsOnDocuments(change)) targets.docs(path);
}
