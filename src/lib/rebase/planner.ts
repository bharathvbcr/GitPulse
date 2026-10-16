import type { VisualCommitRow } from "../canvas/GraphRenderer";

export type PlannerAction = "Pick" | "Squash" | "Fixup" | "Drop" | "Reword";

export interface RebaseStep {
  commit_id: string;
  action: "Pick" | "Squash" | "Fixup" | "Drop" | { Reword: string };
}

export interface PlannerItem {
  id: string;
  action: PlannerAction;
  summary: string;
}

/**
 * Seeds the interactive rebase plan from the loaded history: newest first in
 * the graph, oldest first in the plan, capped at the modal's window.
 */
export function seedRebasePlan(commits: VisualCommitRow[], window = 12): PlannerItem[] {
  const newestFirst = commits.slice(0, window);
  const oldestFirst = [...newestFirst].reverse();
  return oldestFirst.map((c) => ({ id: c.id, action: "Pick", summary: c.summary }));
}

/**
 * Why a plan cannot run, or null when it can. A squash or fixup folds into the
 * nearest kept commit before it; a dropped commit is not one. With only drops
 * ahead of it there is nothing to fold into but the onto commit itself, which
 * git refuses ("cannot 'squash' without a previous commit") and which the
 * backend rejects with the same rule, so the dialog stops it first. Pick and
 * reword keep a commit.
 */
export function rebasePlanProblem(items: ReadonlyArray<Pick<PlannerItem, "id" | "action">>): string | null {
  let hasKeptCommit = false;
  for (const item of items) {
    if (item.action === "Pick" || item.action === "Reword") {
      hasKeptCommit = true;
    } else if ((item.action === "Squash" || item.action === "Fixup") && !hasKeptCommit) {
      return `Cannot ${item.action.toLowerCase()} ${item.id.slice(0, 8)}: no kept commit before it to combine into.`;
    }
  }
  return null;
}

/**
 * True only on the closed→open transition. The dialog must seed its plan
 * once per opening; a background refresh (watcher event, filter keystroke)
 * while the dialog is open updates the commit list but must NOT rebuild the
 * user's half-edited plan out from under them.
 */
export function shouldSeed(isOpen: boolean, wasOpen: boolean): boolean {
  return isOpen && !wasOpen;
}

/**
 * Whether the dialog should (re)build its plan right now. Three cases:
 * - opening: always seed;
 * - still pristine (user touched nothing) and the underlying history
 *   changed: follow it, so a dialog opened before the graph loaded or
 *   refreshed by a watcher event shows real commits;
 * - user-edited: never reseed — their edits win over any refresh.
 */
export function shouldReseed(args: {
  isOpen: boolean;
  wasOpen: boolean;
  dirty: boolean;
  currentSignature: string;
  seededSignature: string;
}): boolean {
  if (!args.isOpen) return false;
  if (shouldSeed(args.isOpen, args.wasOpen)) return true;
  return !args.dirty && args.currentSignature !== args.seededSignature;
}
