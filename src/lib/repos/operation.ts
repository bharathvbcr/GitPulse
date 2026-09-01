/**
 * The parked-git-operation model: wire shapes plus the plain-language phrasing
 * the UI renders.
 *
 * A repository stopped mid-merge or mid-rebase is the single state where a Git
 * client is most likely to lose a user — the working tree is full of markers,
 * the branch name looks wrong, and every next action fails for reasons that
 * name internals. So the phrasing here is a first-class part of the feature,
 * not decoration: it names the operation, says how far through it is, says what
 * is blocking, and says exactly what each escape hatch would throw away.
 *
 * All rendering is pure and lives here rather than in the component, so the
 * wording is unit-tested and one sentence cannot drift between the banner, the
 * tab tooltip and the status bar.
 */

/** Mirrors the Rust `OperationKind`; serde emits unit variants as their names. */
export type OperationKind =
  | "Merge"
  | "Rebase"
  | "RebaseApply"
  | "ApplyMailbox"
  | "CherryPick"
  | "Revert"
  | "Bisect";

/** Mirrors the Rust `OperationAction` under `rename_all = "lowercase"`. */
export type OperationAction = "abort" | "continue" | "skip";

/** Wire shape of `cmd_repo_operation`; snake_case like every other command. */
export interface RepoOperation {
  kind: OperationKind;
  current_step: number | null;
  total_steps: number | null;
  head_ref: string | null;
  incoming_ref: string | null;
  conflicted_paths: string[];
  conflicted_total: number;
  available: OperationAction[];
  warnings?: string[];
}

/**
 * The operation facet of a session.
 *
 * `probeFailed` exists so a probe that could not run is never rendered as an
 * idle repository. Showing "no operation in progress" because the check itself
 * broke is the failure mode that strands a user mid-merge with a UI insisting
 * everything is fine.
 */
export interface OperationState {
  operation: RepoOperation | null;
  probeFailed: boolean;
}

export const IDLE_OPERATION: OperationState = {
  operation: null,
  probeFailed: false,
};

/** How the operation is named in prose. */
export function kindLabel(kind: OperationKind): string {
  switch (kind) {
    case "Merge":
      return "merge";
    case "Rebase":
    case "RebaseApply":
      return "rebase";
    case "ApplyMailbox":
      return "patch application";
    case "CherryPick":
      return "cherry-pick";
    case "Revert":
      return "revert";
    case "Bisect":
      return "bisect";
  }
}

/** Title-case form for the banner heading. */
export function kindTitle(kind: OperationKind): string {
  const label = kindLabel(kind);
  return label.charAt(0).toUpperCase() + label.slice(1);
}

/**
 * "step 2 of 7", or null when the operation is single-step or its counters
 * were unreadable.
 *
 * A partial pair is deliberately not rendered: "step 2 of ?" reads as a bug,
 * and inventing a total is worse than omitting the phrase.
 */
export function progressLabel(op: RepoOperation): string | null {
  const { current_step: current, total_steps: total } = op;
  if (current === null || total === null) return null;
  if (!Number.isFinite(current) || !Number.isFinite(total)) return null;
  if (total <= 0 || current <= 0 || current > total) return null;
  return `step ${current} of ${total}`;
}

/**
 * The one-line headline: what is happening, and to what.
 *
 * Examples:
 *   "Merge in progress"
 *   "Rebase in progress — step 2 of 7, rebasing side"
 */
export function headline(op: RepoOperation): string {
  const parts: string[] = [];
  const progress = progressLabel(op);
  if (progress) parts.push(progress);
  if (op.head_ref) {
    parts.push(`${branchPreposition(op.kind)} ${op.head_ref}`);
  }
  const suffix = parts.length > 0 ? ` — ${parts.join(", ")}` : "";
  return `${kindTitle(op.kind)} in progress${suffix}`;
}

/**
 * How the operation relates to the branch named after it.
 *
 * Only a rebase rebases. Every other kind acts *on* the branch that is checked
 * out, and saying "Bisect in progress — rebasing main" is both wrong and
 * alarming: it tells a user history is being rewritten when it is not.
 */
function branchPreposition(kind: OperationKind): string {
  return kind === "Rebase" || kind === "RebaseApply" ? "rebasing" : "on";
}

/**
 * What the user has to do next, in the imperative.
 *
 * This is the sentence a first-time user acts on, so it never names a control
 * file or a git flag — it names files and buttons.
 */
export function nextStep(op: RepoOperation): string {
  if (op.kind === "Bisect") {
    return "Mark commits good or bad in a terminal, or end the bisect to return to where you started.";
  }
  const n = op.conflicted_total;
  if (n > 0) {
    const files = n === 1 ? "1 file" : `${n} files`;
    return `Resolve ${files} in the Resolve view, stage the result, then continue.`;
  }
  if (op.available.includes("continue")) {
    return "All conflicts are resolved — continue to finish the operation.";
  }
  // No conflicts and no continue offered: git is waiting on something this
  // surface does not model. Say so rather than inventing an instruction.
  return "Nothing is left to resolve here. Continue from a terminal, or abort to start over.";
}

/** Button text for an action, naming the operation so the button stands alone. */
export function actionLabel(kind: OperationKind, action: OperationAction): string {
  const label = kindLabel(kind);
  switch (action) {
    case "abort":
      // "Abort bisect" is not what git calls it, and the mismatch matters when
      // a user goes looking for the equivalent command.
      return kind === "Bisect" ? "End bisect" : `Abort ${label}`;
    case "continue":
      return kind === "Merge" ? "Commit the merge" : `Continue ${label}`;
    case "skip":
      return "Skip this commit";
  }
}

/**
 * What the action will do, in the words of consequence rather than mechanism.
 * Rendered as the button's tooltip and as the confirmation body.
 */
export function actionConsequence(
  kind: OperationKind,
  action: OperationAction,
): string {
  const label = kindLabel(kind);
  switch (action) {
    case "abort":
      return kind === "Bisect"
        ? "Returns you to the commit you were on before the bisect started."
        : `Undoes the whole ${label} and restores the branch to how it was before it started. Conflict edits made in the working tree are discarded.`;
    case "continue":
      return kind === "Merge"
        ? "Records the resolved files as a merge commit."
        : `Records the current step and moves the ${label} to the next commit.`;
    case "skip":
      return `Drops the commit currently being applied and moves on. That commit's changes will not appear in the result.`;
  }
}

/**
 * True for actions that discard work and therefore need confirming.
 *
 * Abort throws away every conflict resolution the user has typed; skip throws
 * away a whole commit's changes. Neither is undoable from this surface, so
 * both are confirmed. Continue only ever moves forward.
 */
export function isDestructive(action: OperationAction): boolean {
  return action === "abort" || action === "skip";
}

/**
 * Orders the buttons so the safe, forward action leads.
 *
 * Continue is the action a user in a resolved state wants; abort is the one a
 * stuck user wants; skip is the rare expert move. Rendering them in git's
 * alphabetical order would put the destructive abort first.
 */
const ACTION_ORDER: readonly OperationAction[] = ["continue", "abort", "skip"];

export function orderedActions(op: RepoOperation): OperationAction[] {
  return ACTION_ORDER.filter((action) => op.available.includes(action));
}

/**
 * Short marker for the repo tab and status bar, where one word is all there is
 * room for. `null` for an idle repository, so callers can render nothing.
 */
export function tabMarker(state: OperationState): string | null {
  if (state.probeFailed) return "?";
  if (!state.operation) return null;
  const op = state.operation;
  const progress = progressLabel(op);
  return progress
    ? `${kindTitle(op.kind)} ${op.current_step}/${op.total_steps}`
    : kindTitle(op.kind);
}

/**
 * The full sentence behind that marker, for the tooltip.
 *
 * A failed probe gets its own honest wording: the app does not know, and
 * saying so is the only safe answer.
 */
export function tabTooltip(state: OperationState): string | null {
  if (state.probeFailed) {
    return "GitPulse could not determine whether an operation is in progress in this repository.";
  }
  if (!state.operation) return null;
  return `${headline(state.operation)}. ${nextStep(state.operation)}`;
}

/**
 * Whether a mutating action other than the recovery verbs should be held back.
 *
 * Committing, merging, rebasing or checking out on top of a parked operation
 * either fails with an internals-flavored error or corrupts the operation. A
 * bisect is the exception: it parks HEAD but leaves the index clean, and
 * building/committing during one is the normal way to use it.
 */
export function blocksOtherMutations(state: OperationState): boolean {
  if (!state.operation) return false;
  return state.operation.kind !== "Bisect";
}

/**
 * Why an action was held back, for the disabled control's tooltip. Returns
 * null when nothing is blocking.
 */
export function blockedReason(state: OperationState, verb: string): string | null {
  if (!blocksOtherMutations(state)) return null;
  const op = state.operation;
  if (!op) return null;
  return `Cannot ${verb} while a ${kindLabel(op.kind)} is in progress. Finish or abort it first.`;
}

/**
 * Structural equality over the operation facet, for the store's publish gate.
 *
 * The snapshot builds a fresh `OperationState` object on every status poll, so
 * reference equality reports "changed" every six seconds and republishes the
 * whole store to every subscriber — which re-runs the Resolve view's load
 * effect and repaints the graph on a repository where nothing happened. The
 * gate needs to know whether the *content* moved.
 *
 * Field-by-field rather than JSON round-tripping: this runs on every poll for
 * every open repository, and `conflicted_paths` can hold a thousand entries.
 */
export function operationStatesEqual(a: OperationState, b: OperationState): boolean {
  if (a === b) return true;
  if (a.probeFailed !== b.probeFailed) return false;
  const left = a.operation;
  const right = b.operation;
  if (left === right) return true;
  if (!left || !right) return false;
  return (
    left.kind === right.kind &&
    left.current_step === right.current_step &&
    left.total_steps === right.total_steps &&
    left.head_ref === right.head_ref &&
    left.incoming_ref === right.incoming_ref &&
    left.conflicted_total === right.conflicted_total &&
    stringListsEqual(left.conflicted_paths, right.conflicted_paths) &&
    stringListsEqual(left.available, right.available) &&
    stringListsEqual(left.warnings ?? [], right.warnings ?? [])
  );
}

function stringListsEqual(a: readonly string[], b: readonly string[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}
