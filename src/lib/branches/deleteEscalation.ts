import type { BranchInfo } from "./types";

/**
 * Classifies why a branch delete was refused, and whether escalating to a
 * force delete could plausibly succeed.
 *
 * The backend (`GitWriter::delete_branch`) refuses force-deletes of the
 * default branch and of branches checked out in linked worktrees, and git's
 * own `-d` refuses unmerged branches. Only the unmerged case is recoverable
 * by retrying with `-D`, so only that case earns an escalation prompt —
 * everything else would just bounce off the same server-side guard.
 */
export type DeleteFailureKind =
  | "unmerged"
  | "default-branch"
  | "worktree-checked-out"
  | "other";

export interface DeleteFailureDecision {
  kind: DeleteFailureKind;
  /** True only when retrying with force could succeed. */
  canRetryForce: boolean;
  /**
   * Copy for the escalation confirm. Null whenever no confirm should be
   * shown (protected refusals and unknown failures surface their error
   * through the shared session banner instead).
   */
  message: string | null;
}

type EscalationBranch = Pick<
  BranchInfo,
  "name" | "commits_ahead_of_base" | "is_gone" | "compared_to"
>;

/** Wording git emits when `branch -d` hits an unmerged branch (case drifted across versions). */
const UNMERGED_MARKER = "not fully merged";
/** Wording of the Rust guards around `branch -D`. */
const DEFAULT_BRANCH_GUARD = "default branch";
const WORKTREE_GUARD = "checked out in a linked worktree";
/** Git's own refusal for either flag when a worktree holds the branch. */
const GIT_WORKTREE_REFUSAL = "used by worktree";

export function classifyDeleteFailure(errorText: string): DeleteFailureKind {
  const text = errorText.toLowerCase();
  if (text.includes(WORKTREE_GUARD) || text.includes(GIT_WORKTREE_REFUSAL)) {
    return "worktree-checked-out";
  }
  if (text.includes(DEFAULT_BRANCH_GUARD)) {
    return "default-branch";
  }
  if (text.includes(UNMERGED_MARKER)) {
    return "unmerged";
  }
  return "other";
}

/**
 * Builds the escalation confirm body for an unmerged branch: what would be
 * lost (ahead-count), whether the upstream is already gone, and how fresh
 * those numbers are — the menu snapshot can lag the repo (B6), so the copy
 * carries its own caveat rather than implying live data.
 */
function unmergedMessage(branch: EscalationBranch): string {
  const base = branch.compared_to || "the base branch";
  const n = branch.commits_ahead_of_base;
  const commits = `${n} commit${n === 1 ? "" : "s"} not on ${base}`;
  const gone = branch.is_gone ? "\nIts upstream is gone, so these commits exist nowhere else." : "";
  return (
    `${branch.name} has ${commits}.${gone}\n` +
    "Force-deleting discards them permanently.\n" +
    "(Counts are from the last refresh.)"
  );
}

export function escalateDeleteDecision(
  errorText: string,
  branch: EscalationBranch,
): DeleteFailureDecision {
  const kind = classifyDeleteFailure(errorText);
  switch (kind) {
    case "unmerged":
      return { kind, canRetryForce: true, message: unmergedMessage(branch) };
    case "default-branch":
    case "worktree-checked-out":
      return { kind, canRetryForce: false, message: null };
    case "other":
      return { kind, canRetryForce: false, message: null };
  }
}
