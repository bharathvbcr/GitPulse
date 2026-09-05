/**
 * Narrowing the Work overview, and describing the repository the reader is
 * standing in.
 *
 * Two gaps this closes, both about the screen being a workspace rather than a
 * report:
 *
 * - **The counts were not doors.** The strip said "3 blocked" above a list
 *   sorted by weight, and finding those three meant scrolling. A count that
 *   names a subset should select it.
 * - **The screen never said where you are.** Work described every worktree in
 *   flight and never mentioned the branch actually checked out in front of
 *   the reader, whether it was behind its remote, or what was uncommitted in
 *   it — the questions asked most often, answered nowhere on the page.
 *
 * Everything here is a pure function over data the view already holds, so the
 * join stays testable without a repository.
 */

import type { BranchInfo } from "../branches/types";
import type { FileStatus } from "../stores/repoStore";
import { agentKindsOn } from "./agentWorktree";
import { dirtyCount, type WorkRow } from "./projection";

/** Which subset of the rows is on screen. */
export type WorkFacet = "all" | "blocked" | "dirty" | "agents" | "pullRequests";

/** The facets a tile can select, in strip order. */
export const WORK_FACETS: readonly WorkFacet[] = [
  "all",
  "agents",
  "blocked",
  "pullRequests",
] as const;

/** True when `row` belongs to `facet`. */
export function rowInFacet(row: WorkRow, facet: WorkFacet): boolean {
  switch (facet) {
    case "all":
      return true;
    case "blocked":
      return row.operation !== null;
    case "dirty":
      return dirtyCount(row) > 0;
    case "agents":
      return agentKindsOn(row.worktrees.map((b) => b.worktree.path)).length > 0;
    case "pullRequests":
      return row.pullRequests.length > 0;
  }
}

/**
 * Every field of a row a text query is matched against.
 *
 * Paths and branch names are included because that is what a reader actually
 * types — the title of an agent row is a directory name they have never seen,
 * while the branch is the thing they were just looking at in the graph.
 */
export function rowSearchText(row: WorkRow): string {
  const parts = [row.title, row.taskId, row.status];
  for (const binding of row.worktrees) {
    parts.push(binding.worktree.path, binding.worktree.branch ?? "", binding.worktree.name);
  }
  for (const pr of row.pullRequests) parts.push(`#${pr.number}`, pr.title, pr.head_ref);
  return parts.filter(Boolean).join(" ").toLowerCase();
}

/**
 * Rows narrowed by facet and query, in the order the projection sorted them.
 *
 * Sort order is never re-derived here: the projection puts blocked rows first
 * for a reason, and a filter that also reordered would make the same row move
 * when the reader typed a character.
 */
export function filterWorkRows(
  rows: readonly WorkRow[],
  facet: WorkFacet,
  query: string,
): WorkRow[] {
  const needle = query.trim().toLowerCase();
  return rows.filter(
    (row) =>
      rowInFacet(row, facet) && (needle === "" || rowSearchText(row).includes(needle)),
  );
}

/**
 * The branch a row's first worktree has checked out, joined to the branch
 * list for its last-commit time.
 *
 * Null when the row has no branch, or when the branch list has no entry for
 * it — a worktree whose branch has not been measured must read as unknown
 * rather than as "last touched at the epoch".
 */
export function rowLastActivity(
  row: WorkRow,
  branches: readonly BranchInfo[],
): { branch: string; timestamp: number; author: string } | null {
  let best: { branch: string; timestamp: number; author: string } | null = null;
  for (const binding of row.worktrees) {
    const name = binding.worktree.branch;
    if (!name) continue;
    const info = branches.find((b) => !b.is_remote && b.name === name);
    if (!info || info.last_commit_timestamp <= 0) continue;
    if (!best || info.last_commit_timestamp > best.timestamp) {
      best = {
        branch: name,
        timestamp: info.last_commit_timestamp,
        author: info.last_author,
      };
    }
  }
  return best;
}

/** What the checked-out repository looks like right now. */
export interface HereSummary {
  branch: string;
  /** Tracking state, or null when the branch has no upstream configured. */
  upstream: { name: string; ahead: number; behind: number; gone: boolean } | null;
  /** Commits behind the branch the backend compared this one against. */
  behindBase: number;
  comparedTo: string | null;
  staged: number;
  unstaged: number;
  conflicted: number;
  /** True when the branch list carries no entry for the checked-out branch. */
  unmeasured: boolean;
}

/**
 * The one-line answer to "where am I and what is uncommitted".
 *
 * Null without a checked-out branch (a detached HEAD or a bare repository),
 * which is a state the caller renders differently rather than as a branch
 * named "".
 *
 * `unmeasured` exists because the branch list arrives progressively: ahead,
 * behind and base counts are zero until the stats pass lands, and a strip
 * that renders those zeroes says "up to date with your remote" when what it
 * knows is nothing at all.
 */
export function hereSummary(
  currentBranch: string | null,
  branches: readonly BranchInfo[],
  statuses: readonly FileStatus[],
): HereSummary | null {
  if (!currentBranch) return null;
  const info = branches.find((b) => !b.is_remote && b.name === currentBranch);
  let staged = 0;
  let unstaged = 0;
  let conflicted = 0;
  for (const status of statuses) {
    if (status.is_conflicted) conflicted += 1;
    else if (status.is_staged) staged += 1;
    else unstaged += 1;
  }
  return {
    branch: currentBranch,
    upstream: info?.upstream
      ? {
          name: info.upstream,
          ahead: info.ahead_count,
          behind: info.behind_count,
          gone: info.is_gone,
        }
      : null,
    behindBase: info?.commits_behind_base ?? 0,
    comparedTo: info?.compared_to ?? null,
    staged,
    unstaged,
    conflicted,
    unmeasured: info === undefined,
  };
}
