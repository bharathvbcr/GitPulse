/**
 * Branch health verdicts.
 *
 * Backlog C1. `cmd_branch_stats` already returns divergence and tip timing, so
 * a verdict is derivable from data the sidebar has fetched — no extra IPC.
 *
 * The entry's design note is the important part: an arbitrary "30 days = stale"
 * baked into the logic is the piece most likely to be wrong for a given team.
 * The thresholds are therefore parameters with documented defaults, not
 * constants buried in a comparison, so changing them is a call-site decision
 * and the tests can pin behaviour either side of a boundary.
 */

import type { BranchInfo } from "./types";

export type BranchHealthLevel = "healthy" | "info" | "warn" | "attention";

export type BranchHealthCode =
  | "healthy"
  | "current"
  | "unpublished"
  | "merged"
  | "behind"
  | "diverged"
  | "stale"
  | "gone";

export interface BranchHealth {
  readonly code: BranchHealthCode;
  readonly level: BranchHealthLevel;
  /** Two or three words for a badge. */
  readonly title: string;
  /** One sentence explaining the verdict, for the indicator's tooltip. */
  readonly detail: string;
}

export interface BranchHealthThresholds {
  /**
   * Days without a commit before a branch reads as stale.
   *
   * 30 is a default, not a truth: it is roughly a month, long enough that a
   * branch someone is actively working on will not trip it, short enough that
   * abandoned work surfaces before it rots. Teams working in shorter cycles
   * should lower it.
   */
  readonly staleDays: number;
}

export const DEFAULT_BRANCH_HEALTH_THRESHOLDS: BranchHealthThresholds = Object.freeze({
  staleDays: 30,
});

const MS_PER_DAY = 86_400_000;

/**
 * Whole days between a branch tip and now. Negative ages (a tip dated in the
 * future, which clock skew and rewritten history both produce) clamp to 0 so a
 * skewed branch is never reported as stale.
 */
export function branchAgeInDays(branch: BranchInfo, nowMs: number): number | null {
  const { last_commit_timestamp: seconds } = branch;
  if (!Number.isFinite(seconds) || seconds <= 0 || !Number.isFinite(nowMs)) return null;
  return Math.max(0, Math.floor((nowMs - seconds * 1000) / MS_PER_DAY));
}

function plural(count: number, noun: string): string {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

/**
 * Classify one branch.
 *
 * Exactly one verdict is returned, so the order below is the whole design.
 * It runs most-actionable first: a branch can be stale *and* merged *and*
 * behind, and telling the reader it is merged (delete it) is more useful than
 * telling them it is old.
 */
export function branchHealth(
  branch: BranchInfo,
  nowMs: number,
  thresholds: BranchHealthThresholds = DEFAULT_BRANCH_HEALTH_THRESHOLDS,
): BranchHealth {
  const ageDays = branchAgeInDays(branch, nowMs);
  const staleDays = Math.max(1, Math.floor(thresholds.staleDays));

  // 1. The upstream this branch tracked no longer exists. Nothing else it
  //    reports about divergence means anything until that is resolved.
  if (branch.is_gone) {
    return {
      code: "gone",
      level: "attention",
      title: "Upstream gone",
      detail: `The upstream ${branch.upstream ?? "branch"} no longer exists on the remote. It was probably deleted after merging.`,
    };
  }

  // 2. The default branch is the base everything else is measured against, so
  //    "merged" and "behind base" are meaningless for it.
  if (branch.is_default) {
    return {
      code: "healthy",
      level: "healthy",
      title: "Default branch",
      detail: "The branch other branches are compared against.",
    };
  }

  // 3. Fully merged: no unique commits left, so it is safe to delete. Checked
  //    before staleness because it is the actionable half of "old branch".
  if (branch.commits_ahead_of_base === 0 && branch.compared_to) {
    return {
      code: "merged",
      level: "info",
      title: "Merged",
      detail: `Every commit is already in ${branch.compared_to}. Safe to delete.`,
    };
  }

  // 4. Diverged from the base in both directions — a rebase or merge is needed
  //    before this can land cleanly.
  if (branch.commits_ahead_of_base > 0 && branch.commits_behind_base > 0) {
    return {
      code: "diverged",
      level: "warn",
      title: "Diverged",
      detail:
        `${plural(branch.commits_ahead_of_base, "commit")} ahead and ` +
        `${plural(branch.commits_behind_base, "commit")} behind ${branch.compared_to ?? "the base"}. ` +
        "Rebase or merge before it can land cleanly.",
    };
  }

  // 5. Old enough to be worth a look. Deliberately below divergence: a branch
  //    someone must act on beats one they must merely remember.
  if (ageDays !== null && ageDays >= staleDays) {
    return {
      code: "stale",
      level: "warn",
      title: "Stale",
      detail: `No commits for ${plural(ageDays, "day")}. The last was by ${branch.last_author || "an unknown author"}.`,
    };
  }

  if (branch.commits_behind_base > 0) {
    return {
      code: "behind",
      level: "info",
      title: "Behind",
      detail: `${plural(branch.commits_behind_base, "commit")} behind ${branch.compared_to ?? "the base"}.`,
    };
  }

  // 6. Local work that has never been pushed. Normal for a new branch, so this
  //    is information rather than a warning — but it is the reason a branch
  //    shows no upstream divergence at all.
  if (!branch.upstream && !branch.is_remote) {
    return {
      code: "unpublished",
      level: "info",
      title: "Not published",
      detail: "This branch has no upstream. Push it to share the work.",
    };
  }

  if (branch.is_current) {
    return {
      code: "current",
      level: "healthy",
      title: "Up to date",
      detail: "The checked-out branch is current with its base.",
    };
  }

  return {
    code: "healthy",
    level: "healthy",
    title: "Healthy",
    detail: "Up to date with its base, with recent activity.",
  };
}

/** Whether a verdict is worth drawing an indicator for at all. */
export function needsAttention(health: BranchHealth): boolean {
  return health.level === "warn" || health.level === "attention";
}
