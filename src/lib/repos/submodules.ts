/**
 * Submodules, as the UI sees them.
 *
 * The state a user actually hits is the uninitialized one: a fresh clone
 * presents empty directories where the submodules should be, the build fails
 * with missing files, and nothing in a Git client explains why. So the model's
 * job is to say which submodules are unusable, why, and what single action
 * fixes them.
 */

/** Mirrors the Rust `SubmoduleState`. */
export type SubmoduleState =
  | "Uninitialized"
  | "UpToDate"
  | "CommitDiffers"
  | "Conflicted";

/** Mirrors the Rust `SubmoduleInfo`. */
export interface SubmoduleInfo {
  name: string;
  path: string;
  url: string | null;
  oid: string | null;
  described: string | null;
  state: SubmoduleState;
  orphaned: boolean;
}

/** Mirrors the Rust `SubmoduleChange`, an internally tagged enum. */
export type SubmoduleChange =
  | { kind: "update"; path: string | null; recursive: boolean }
  | { kind: "sync"; path: string | null; recursive: boolean }
  | { kind: "deinit"; path: string; force: boolean };

/** Short state word for a badge. */
export function submoduleStateLabel(state: SubmoduleState): string {
  switch (state) {
    case "Uninitialized":
      return "not initialized";
    case "UpToDate":
      return "up to date";
    case "CommitDiffers":
      return "moved";
    case "Conflicted":
      return "conflicted";
  }
}

/**
 * What the state means, in terms of what the user will observe.
 *
 * Each sentence names the symptom, not the git internal — "the folder is
 * empty" is what the user is actually looking at.
 */
export function submoduleStateExplanation(state: SubmoduleState): string {
  switch (state) {
    case "Uninitialized":
      return "Its folder is empty — the submodule has never been checked out here. Builds and imports that rely on it will fail.";
    case "UpToDate":
      return "Checked out at the commit this repository records.";
    case "CommitDiffers":
      return "Checked out at a different commit than this repository records. Committing here would move the recorded pointer.";
    case "Conflicted":
      return "A merge left this submodule's recorded commit in conflict. Resolve it like any other conflicted file.";
  }
}

/** True when the submodule is not usable as it stands. */
export function needsAttention(sub: SubmoduleInfo): boolean {
  return sub.state !== "UpToDate";
}

/**
 * Whether `update --init` would help this submodule.
 *
 * An orphaned entry — present in the index, absent from `.gitmodules` — has no
 * URL to clone from, so offering "Initialize" would present a button that
 * cannot work. Saying so is the only way the user learns the real problem is a
 * missing `.gitmodules` entry.
 */
export function canInitialize(sub: SubmoduleInfo): boolean {
  return sub.state === "Uninitialized" && !sub.orphaned;
}

/** Why an action is unavailable, or null when it is available. */
export function blockedInitializeReason(sub: SubmoduleInfo): string | null {
  if (sub.state !== "Uninitialized") return null;
  if (sub.orphaned) {
    return "This submodule is recorded in the repository but missing from .gitmodules, so there is no URL to fetch it from. It must be re-added.";
  }
  return null;
}

/** True for changes that discard a checked-out submodule's contents. */
export function isDestructiveSubmoduleChange(change: SubmoduleChange): boolean {
  return change.kind === "deinit";
}

export function submoduleChangeConsequence(change: SubmoduleChange): string {
  switch (change.kind) {
    case "update":
      return change.path
        ? "Clones and checks out this submodule at the commit this repository records."
        : "Clones and checks out every submodule at the commits this repository records.";
    case "sync":
      return "Rewrites each submodule's configured URL from .gitmodules — the fix after an upstream moves.";
    case "deinit":
      return `Removes the working copy at '${change.path}'. Uncommitted changes inside it are lost; the recorded commit in this repository is untouched.`;
  }
}

/**
 * One line for the whole set.
 *
 * Leads with what is broken. A repository with no submodules says so plainly
 * rather than rendering an empty list with no explanation.
 */
export function describeSubmodules(subs: readonly SubmoduleInfo[]): string {
  if (subs.length === 0) return "This repository has no submodules.";
  const needing = subs.filter(needsAttention);
  if (needing.length === 0) {
    return subs.length === 1
      ? "1 submodule, up to date."
      : `${subs.length} submodules, all up to date.`;
  }
  const uninitialized = needing.filter((s) => s.state === "Uninitialized").length;
  if (uninitialized === needing.length) {
    return `${uninitialized} of ${subs.length} submodule${subs.length === 1 ? "" : "s"} not initialized.`;
  }
  return `${needing.length} of ${subs.length} submodule${subs.length === 1 ? "" : "s"} need attention.`;
}

/**
 * Submodules that a single "Initialize all" would fix.
 *
 * Orphaned entries are excluded: including them would make the bulk action
 * report a partial failure every time, for a cause the action cannot address.
 */
export function initializableSubmodules(
  subs: readonly SubmoduleInfo[],
): SubmoduleInfo[] {
  return subs.filter(canInitialize);
}

/** Worst state first, so the broken ones lead the list. */
const STATE_ORDER: readonly SubmoduleState[] = [
  "Conflicted",
  "Uninitialized",
  "CommitDiffers",
  "UpToDate",
];

export function sortSubmodules(subs: readonly SubmoduleInfo[]): SubmoduleInfo[] {
  return [...subs].sort((a, b) => {
    const byState = STATE_ORDER.indexOf(a.state) - STATE_ORDER.indexOf(b.state);
    return byState !== 0 ? byState : a.path.localeCompare(b.path);
  });
}
