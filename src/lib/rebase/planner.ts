import type { VisualCommitRow } from "../canvas/GraphRenderer";

export type PlannerAction = "Pick" | "Squash" | "Fixup" | "Drop" | "Reword";

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
