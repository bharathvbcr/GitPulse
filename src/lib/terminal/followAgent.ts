/**
 * Which pane "Follow agent" may drive, and when following has to end.
 *
 * Follow used to retarget whichever file pane happened to be visible and,
 * when none was, open a new split for every file the agent touched. So a file
 * the user opened beside the agent was replaced by the agent's next edit, and
 * once the user closed that pane, each later edit opened another one: files
 * appearing with no action from the user.
 *
 * Follow now owns at most one pane. It records the pane and the file it last
 * put there, and gives the pane up — ending follow — the moment the user
 * closes it or points it at a file of their own. It never opens a second pane
 * behind the user's back, and never navigates a pane it did not open or adopt.
 */

/** The fields of a terminal tab this decision reads. */
export interface FollowPaneCandidate {
  readonly id: string;
  readonly launcher: string;
  readonly filePath?: string;
  readonly snippet?: unknown;
}

/** What follow owns: a pane id and the file follow last showed in it. */
export interface FollowTarget {
  /** The pane follow drives; null until follow has opened or adopted one. */
  readonly paneId: string | null;
  /** The file follow last put in that pane; null for a pane follow has not pointed yet. */
  readonly filePath: string | null;
}

export const NO_FOLLOW_TARGET: FollowTarget = { paneId: null, filePath: null };

export type FollowDecision =
  /** No pane yet: follow may open one, and must adopt it. */
  | { readonly kind: "open" }
  /** Point the owned pane at the touched file. */
  | { readonly kind: "retarget"; readonly paneId: string }
  /** The user closed or took over the owned pane: follow ends and opens nothing. */
  | { readonly kind: "stop"; readonly reason: "closed" | "taken" };

/** A file pane: one showing a repository file, or a snippet (a plan outside the repository). */
function isFilePane(tab: FollowPaneCandidate | undefined): tab is FollowPaneCandidate {
  return tab !== undefined && tab.launcher === "file";
}

/**
 * What follow may do with the next file the agent touches.
 *
 * `paneFile` is how the panel normalizes a pane's path, so a pane whose path
 * differs only in spelling from what follow recorded is still follow's.
 */
export function decideFollow(
  target: FollowTarget,
  tabs: readonly FollowPaneCandidate[],
  paneFile: (tab: FollowPaneCandidate) => string | null = (tab) => tab.filePath ?? null,
): FollowDecision {
  if (target.paneId === null) return { kind: "open" };
  const pane = tabs.find((tab) => tab.id === target.paneId);
  if (!pane) return { kind: "stop", reason: "closed" };
  if (!isFilePane(pane)) return { kind: "stop", reason: "taken" };
  if (paneFile(pane) !== target.filePath) return { kind: "stop", reason: "taken" };
  return { kind: "retarget", paneId: pane.id };
}

/**
 * Whether the pane follow owns is still open, so a panel can turn the toggle
 * off as soon as the user closes it rather than at the agent's next edit.
 *
 * Only closing is judged here. A takeover (another file in the pane) is
 * judged by `decideFollow` at the next edit, because follow itself changes
 * the pane's file between edits (a plan artifact), and a watcher comparing
 * paths would race that write and end follow on its own action.
 */
export function followPaneOpen(target: FollowTarget, tabs: readonly FollowPaneCandidate[]): boolean {
  if (target.paneId === null) return true;
  return isFilePane(tabs.find((tab) => tab.id === target.paneId));
}

/**
 * The target for a pane follow has just opened, pointed, or been handed by a
 * preset. Turning follow on with the toggle hands it nothing: a file pane the
 * user already has open stays theirs.
 */
export function adoptFollowPane(
  tab: FollowPaneCandidate | undefined,
  paneFile: (tab: FollowPaneCandidate) => string | null = (candidate) => candidate.filePath ?? null,
): FollowTarget {
  if (!isFilePane(tab)) return NO_FOLLOW_TARGET;
  return { paneId: tab.id, filePath: paneFile(tab) };
}
